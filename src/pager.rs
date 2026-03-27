use crate::render::PaneLayout;
use crate::render::TintPalette;
use crate::terminal_palette::AnsiColor;
use crate::terminal_palette::search_highlight_bg;
use anyhow::Context;
use anyhow::Result;
use crossterm::cursor;
use crossterm::event;
use crossterm::event::DisableFocusChange;
use crossterm::event::DisableMouseCapture;
use crossterm::event::EnableFocusChange;
use crossterm::event::EnableMouseCapture;
use crossterm::event::Event;
use crossterm::event::KeyCode;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseButton;
use crossterm::event::MouseEventKind;
use crossterm::execute;
use crossterm::queue;
use crossterm::style::Print;
use crossterm::terminal;
use crossterm::terminal::Clear;
use crossterm::terminal::ClearType;
use crossterm::terminal::EnterAlternateScreen;
use crossterm::terminal::LeaveAlternateScreen;
use std::io;
use std::io::IsTerminal;
use std::io::Write;
use unicode_width::UnicodeWidthChar;

const MOUSE_SCROLL_LINES: usize = 3;
const HORIZONTAL_SCROLL_COLUMNS: usize = 8;
const FILE_FILTER_PROMPT: &str = "› ";
const HELP_LINES: &[&str] = &[
    "mdiff help",
    "",
    "Navigation",
    "  q           quit pager",
    "  ?           toggle help",
    "  Up/PageUp   page up",
    "  Down/PageDown/Space",
    "              page down",
    "  g/Home      jump to top",
    "  G/End       jump to bottom",
    "  Left/Right  scroll horizontally",
    "  Mouse wheel scroll",
    "",
    "Search",
    "  /           open search",
    "  Enter       confirm search",
    "  n           next match",
    "  N           previous match",
    "  Esc         leave search",
    "",
    "File filter",
    "  Ctrl-F      open file filter",
    "  Type        narrow files",
    "  Up/Down     jump between files",
    "  Backspace   delete filter text",
    "  Enter/Esc   close file filter",
    "",
    "Press ? or Esc to close this help.",
];

#[derive(Clone, Debug, Eq, PartialEq)]
struct SearchMatch {
    line: usize,
    start: usize,
    end: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
enum SearchMode {
    #[default]
    Inactive,
    Prompt {
        query: String,
    },
    Active {
        query: String,
        matches: Vec<SearchMatch>,
        current: usize,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct TextStyle {
    bold: bool,
    dim: bool,
    italic: bool,
    background: Option<AnsiColor>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StyledCell {
    ch: char,
    style: TextStyle,
    start: usize,
    end: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct PlainCell {
    ch: char,
    start: usize,
    end: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileHeaderLine {
    name: String,
    line: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SelectionPane {
    Left,
    Right,
    Full,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Selection {
    pane: SelectionPane,
    anchor_line: usize,
    extent_line: usize,
}

pub fn page_or_render<F>(files: Vec<String>, force_pager: bool, render: F) -> Result<Option<String>>
where
    F: Fn(usize, &str, &TintPalette) -> (String, PaneLayout),
{
    let palette = TintPalette::detect();

    let stdout_is_tty = std::io::stdout().is_terminal();
    if !stdout_is_tty {
        let (output, _) = render(0, "", &palette);
        return Ok(Some(output));
    }

    let (width, rows) = terminal::size().context("failed to read terminal size")?;
    let width = width as usize;
    let rows = rows as usize;
    let (initial_output, initial_layout) = render(width, "", &palette);

    if !should_page_output(
        stdout_is_tty,
        force_pager,
        line_count(&initial_output),
        rows,
    ) {
        return Ok(Some(initial_output));
    }

    page(
        files,
        render,
        width,
        rows,
        initial_output,
        initial_layout,
        palette,
    )?;
    Ok(None)
}

fn page<F>(
    files: Vec<String>,
    render: F,
    width: usize,
    height: usize,
    initial_output: String,
    initial_layout: PaneLayout,
    initial_palette: TintPalette,
) -> Result<()>
where
    F: Fn(usize, &str, &TintPalette) -> (String, PaneLayout),
{
    let mut stdout = io::stdout();
    let mut state = PagerState::new(
        render,
        width,
        height,
        initial_output,
        initial_layout,
        files,
        initial_palette,
    );

    terminal::enable_raw_mode().context("failed to enable raw mode")?;
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableFocusChange,
        cursor::Hide
    )
    .context("failed to initialize pager screen")?;

    let result = run_pager(&mut stdout, &mut state);

    execute!(
        stdout,
        cursor::Show,
        DisableFocusChange,
        DisableMouseCapture,
        LeaveAlternateScreen
    )
    .context("failed to restore terminal after pager")?;
    terminal::disable_raw_mode().context("failed to disable raw mode")?;

    result
}

fn run_pager<F>(stdout: &mut io::Stdout, state: &mut PagerState<F>) -> Result<()>
where
    F: Fn(usize, &str, &TintPalette) -> (String, PaneLayout),
{
    loop {
        state.refresh_dimensions()?;
        draw(stdout, state)?;

        match event::read().context("failed to read pager input")? {
            Event::Key(key) => {
                state.selection = None;

                if state.handle_hud_key(key.code, key.modifiers) {
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Esc => return Ok(()),
                    KeyCode::Up | KeyCode::PageUp => state.page_up(),
                    KeyCode::Down | KeyCode::PageDown | KeyCode::Char(' ') => state.page_down(),
                    KeyCode::Left => state.scroll_left(),
                    KeyCode::Right => state.scroll_right(),
                    KeyCode::Home | KeyCode::Char('g') => state.to_top(),
                    KeyCode::End | KeyCode::Char('G') => state.to_bottom(),
                    _ => {}
                }
            }
            Event::Mouse(mouse) => {
                if state.help_open {
                    continue;
                }

                match mouse.kind {
                    MouseEventKind::ScrollUp => state.scroll_up(MOUSE_SCROLL_LINES),
                    MouseEventKind::ScrollDown => state.scroll_down(MOUSE_SCROLL_LINES),
                    MouseEventKind::Down(MouseButton::Left) => {
                        let row = mouse.row as usize;
                        if row < state.viewport_height() {
                            let line = state.offset + row;
                            let content_col = state.horizontal_offset + mouse.column as usize;
                            let pane = state.pane_at_column(content_col);
                            state.selection = Some(Selection {
                                pane,
                                anchor_line: line,
                                extent_line: line,
                            });
                        }
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        let row = mouse.row as usize;
                        let max_row = state.viewport_height().saturating_sub(1);
                        let line = state.offset + row.min(max_row);
                        if let Some(ref mut sel) = state.selection {
                            sel.extent_line = line;
                        }
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        let row = mouse.row as usize;
                        let max_row = state.viewport_height().saturating_sub(1);
                        let line = state.offset + row.min(max_row);
                        if let Some(ref mut sel) = state.selection {
                            sel.extent_line = line;
                        }
                        let text = state.extract_selection_text();
                        state.selection = None;
                        if !text.is_empty() {
                            let _ = write_osc52(stdout, &text);
                        }
                    }
                    _ => continue,
                }
            }
            Event::FocusGained => state.refresh_palette_from_terminal(),
            Event::Resize(_, _) => {}
            _ => continue,
        }
    }
}

fn draw<F>(stdout: &mut io::Stdout, state: &PagerState<F>) -> Result<()>
where
    F: Fn(usize, &str, &TintPalette) -> (String, PaneLayout),
{
    let hud_lines = state.hud_lines();
    let viewport_height = state.height.saturating_sub(hud_lines.len());

    queue!(stdout, cursor::MoveTo(0, 0), Clear(ClearType::All))
        .context("failed to clear pager screen")?;

    for row in 0..viewport_height {
        queue!(stdout, cursor::MoveTo(0, row as u16)).context("failed to move cursor")?;
        if let Some(line) = state.rendered_line_at(row) {
            queue!(stdout, Print(line)).context("failed to draw line")?;
        }
    }

    let hud_start = state.height.saturating_sub(hud_lines.len());
    for (index, line) in hud_lines.iter().enumerate() {
        queue!(
            stdout,
            cursor::MoveTo(0, (hud_start + index) as u16),
            Print(line)
        )
        .context("failed to draw hud line")?;
    }

    if state.help_open {
        for (x, y, line) in
            render_centered_overlay_lines(state.width, state.height, state.search_bg, HELP_LINES)
        {
            queue!(stdout, cursor::MoveTo(x, y), Print(line))
                .context("failed to draw help overlay")?;
        }
    }

    if let Some((column, row)) = state.hud_cursor_position() {
        queue!(stdout, cursor::MoveTo(column, row), cursor::Show)
            .context("failed to place hud cursor")?;
    } else {
        queue!(stdout, cursor::Hide).context("failed to hide cursor")?;
    }

    stdout.flush().context("failed to flush pager screen")
}

fn line_count(output: &str) -> usize {
    output.lines().count()
}

fn should_page_output(
    stdout_is_tty: bool,
    force_pager: bool,
    output_line_count: usize,
    terminal_rows: usize,
) -> bool {
    stdout_is_tty && (force_pager || output_line_count > terminal_rows)
}

fn clip_ansi_text(text: &str, width: usize) -> String {
    clip_ansi_text_from(text, 0, width)
}

fn clip_ansi_text_from(text: &str, start_col: usize, width: usize) -> String {
    render_visible_cells(
        &parse_styled_cells(text),
        start_col,
        width,
        &[],
        None,
        None,
        0,
    )
}

fn strip_ansi_text(text: &str) -> String {
    let mut rendered = String::new();
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(next) = chars.next() {
                    if next == 'm' {
                        break;
                    }
                }
            }
            continue;
        }

        rendered.push(ch);
    }

    rendered
}

fn parse_plain_cells(text: &str) -> Vec<PlainCell> {
    let mut cells = Vec::new();
    let mut plain_offset = 0usize;

    for ch in text.chars() {
        let start = plain_offset;
        plain_offset += ch.len_utf8();
        cells.push(PlainCell {
            ch,
            start,
            end: plain_offset,
        });
    }

    cells
}

fn parse_styled_cells(text: &str) -> Vec<StyledCell> {
    let mut cells = Vec::new();
    let mut chars = text.chars().peekable();
    let mut style = TextStyle::default();
    let mut plain_offset = 0usize;

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            let mut sequence = String::new();
            while let Some(next) = chars.next() {
                if next == 'm' {
                    break;
                }
                sequence.push(next);
            }
            apply_sgr(&mut style, &sequence);
            continue;
        }

        let start = plain_offset;
        plain_offset += ch.len_utf8();
        cells.push(StyledCell {
            ch,
            style,
            start,
            end: plain_offset,
        });
    }

    cells
}

fn apply_sgr(style: &mut TextStyle, sequence: &str) {
    if sequence.is_empty() {
        *style = TextStyle::default();
        return;
    }

    let parts: Vec<&str> = sequence.split(';').collect();
    let mut index = 0usize;

    while index < parts.len() {
        let Ok(code) = parts[index].parse::<u16>() else {
            index += 1;
            continue;
        };

        match code {
            0 => *style = TextStyle::default(),
            1 => style.bold = true,
            2 => style.dim = true,
            3 => style.italic = true,
            22 => {
                style.bold = false;
                style.dim = false;
            }
            23 => style.italic = false,
            49 => style.background = None,
            48 => {
                if index + 2 < parts.len() && parts[index + 1] == "5" {
                    if let Ok(value) = parts[index + 2].parse::<u8>() {
                        style.background = Some(AnsiColor::Indexed(value));
                    }
                    index += 2;
                } else if index + 4 < parts.len() && parts[index + 1] == "2" {
                    let rgb = (
                        parts[index + 2].parse::<u8>(),
                        parts[index + 3].parse::<u8>(),
                        parts[index + 4].parse::<u8>(),
                    );
                    if let (Ok(r), Ok(g), Ok(b)) = rgb {
                        style.background = Some(AnsiColor::Rgb(r, g, b));
                    }
                    index += 4;
                }
            }
            _ => {}
        }

        index += 1;
    }
}

fn render_highlighted_line(
    text: &str,
    start_col: usize,
    width: usize,
    matches: &[SearchMatch],
    search_bg: Option<AnsiColor>,
    selection: Option<(usize, usize)>,
    content_start: usize,
) -> String {
    render_visible_cells(
        &parse_styled_cells(text),
        start_col,
        width,
        matches,
        search_bg,
        selection,
        content_start,
    )
}

fn render_visible_cells(
    cells: &[StyledCell],
    start_col: usize,
    width: usize,
    matches: &[SearchMatch],
    search_bg: Option<AnsiColor>,
    selection: Option<(usize, usize)>,
    content_start: usize,
) -> String {
    if width == 0 {
        return String::new();
    }

    let total_width: usize = cells.iter().map(|cell| char_width(cell.ch)).sum();
    let trailing_background = trailing_background(cells, total_width);
    let right_overflow = total_width > start_col.saturating_add(width);
    let visible_width = if right_overflow {
        width.saturating_sub(1)
    } else {
        width
    };
    let mut rendered = String::new();
    let mut current_style = TextStyle::default();
    let mut used = 0usize;
    let mut skipped = 0usize;

    for cell in cells {
        let ch_width = char_width(cell.ch);

        if skipped + ch_width <= start_col {
            skipped += ch_width;
            continue;
        }

        if used + ch_width > visible_width {
            break;
        }

        let mut style = cell.style;
        if search_bg.is_some() && cell_is_highlighted(&cell, matches) {
            style.background = search_bg;
        }
        if let (Some((sel_start, sel_end)), Some(bg)) = (selection, search_bg) {
            if skipped >= sel_start && skipped < sel_end {
                style.background = Some(bg);
            }
        }

        if style != current_style {
            if current_style != TextStyle::default() || style != TextStyle::default() {
                rendered.push_str("\u{1b}[0m");
            }
            if let Some(prefix) = style_prefix(style) {
                rendered.push_str(&prefix);
            }
            current_style = style;
        }

        rendered.push(cell.ch);
        used += ch_width;
        skipped += ch_width;
    }

    if current_style != TextStyle::default() {
        rendered.push_str("\u{1b}[0m");
    }

    if let Some((background, background_start)) = trailing_background
        && background_start >= content_start
        && used < visible_width
    {
        if let Some(prefix) = style_prefix(TextStyle {
            background: Some(background),
            ..TextStyle::default()
        }) {
            rendered.push_str(&prefix);
            rendered.push_str(&" ".repeat(visible_width - used));
            rendered.push_str("\u{1b}[0m");
        }
    }

    if right_overflow {
        rendered.push('»');
    }

    rendered
}

fn trailing_background(cells: &[StyledCell], total_width: usize) -> Option<(AnsiColor, usize)> {
    let background = cells.last()?.style.background?;
    let mut run_width = 0usize;

    for cell in cells.iter().rev() {
        if cell.style.background != Some(background) {
            break;
        }
        run_width += char_width(cell.ch);
    }

    Some((background, total_width.saturating_sub(run_width)))
}

fn plain_offset_to_column(text: &str, offset: usize) -> usize {
    parse_plain_cells(text)
        .into_iter()
        .take_while(|cell| cell.end <= offset)
        .map(|cell| char_width(cell.ch))
        .sum()
}

fn cell_is_highlighted(cell: &StyledCell, matches: &[SearchMatch]) -> bool {
    matches
        .iter()
        .any(|matched| cell.start < matched.end && cell.end > matched.start)
}

fn style_prefix(style: TextStyle) -> Option<String> {
    let mut codes = Vec::new();

    if style.bold {
        codes.push("1".to_owned());
    }
    if style.dim {
        codes.push("2".to_owned());
    }
    if style.italic {
        codes.push("3".to_owned());
    }

    if let Some(color) = style.background {
        match color {
            AnsiColor::Indexed(index) => codes.push(format!("48;5;{index}")),
            AnsiColor::Rgb(r, g, b) => codes.push(format!("48;2;{r};{g};{b}")),
        }
    }

    if codes.is_empty() {
        None
    } else {
        Some(format!("\u{1b}[{}m", codes.join(";")))
    }
}

fn render_hud_row(text: &str, width: usize, background: Option<AnsiColor>, bold: bool) -> String {
    let mut clipped = clip_plain_text(text, width);
    let clipped_width = display_width(&clipped);
    if clipped_width < width {
        clipped.push_str(&" ".repeat(width - clipped_width));
    }

    let style = style_prefix(TextStyle {
        bold,
        background,
        ..TextStyle::default()
    })
    .unwrap_or_default();

    if style.is_empty() {
        clipped
    } else {
        format!("{style}{clipped}\u{1b}[0m")
    }
}

fn render_search_hud(
    query: &str,
    width: usize,
    background: Option<AnsiColor>,
    status: Option<&str>,
) -> String {
    let status_width = status.map(display_width).unwrap_or(0);
    let gap_width = usize::from(status.is_some() && width > status_width);
    let query_width = width.saturating_sub(status_width + gap_width);

    let mut text = clip_plain_text(&format!("/{query}"), query_width);
    let text_width = display_width(&text);

    if let Some(status) = status {
        let padding = width.saturating_sub(text_width + status_width);
        text.push_str(&" ".repeat(padding));
        text.push_str(status);
    }

    render_hud_row(&text, width, background, false)
}

fn filter_file_names(files: &[String], query: &str) -> Vec<String> {
    if query.is_empty() {
        return files.to_vec();
    }

    files
        .iter()
        .filter(|path| path.contains(query))
        .cloned()
        .collect()
}

fn build_file_header_lines(
    plain_lines: &[String],
    visible_files: &[String],
) -> Vec<FileHeaderLine> {
    let mut headers = Vec::new();
    let mut next_file = 0usize;

    for (line_index, line) in plain_lines.iter().enumerate() {
        if next_file >= visible_files.len() {
            break;
        }

        if line == &visible_files[next_file] {
            headers.push(FileHeaderLine {
                name: visible_files[next_file].clone(),
                line: line_index,
            });
            next_file += 1;
        }
    }

    headers
}

fn find_matches(lines: &[String], query: &str) -> Vec<SearchMatch> {
    if query.is_empty() {
        return Vec::new();
    }

    let mut matches = Vec::new();
    for (line_index, line) in lines.iter().enumerate() {
        let mut search_start = 0usize;
        while let Some(found) = line[search_start..].find(query) {
            let start = search_start + found;
            let end = start + query.len();
            matches.push(SearchMatch {
                line: line_index,
                start,
                end,
            });
            search_start = start.saturating_add(1);
        }
    }

    matches
}

fn rebuild_plain_lines(lines: &[String]) -> Vec<String> {
    lines.iter().map(|line| strip_ansi_text(line)).collect()
}

fn clip_plain_text(text: &str, width: usize) -> String {
    let mut rendered = String::new();
    let mut used = 0usize;

    for ch in text.chars() {
        let ch_width = char_width(ch);
        if used + ch_width > width {
            break;
        }
        rendered.push(ch);
        used += ch_width;
    }

    rendered
}

fn display_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

fn char_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

#[derive(Debug)]
struct PagerState<F>
where
    F: Fn(usize, &str, &TintPalette) -> (String, PaneLayout),
{
    render: F,
    palette: TintPalette,
    lines: Vec<String>,
    plain_lines: Vec<String>,
    offset: usize,
    horizontal_offset: usize,
    width: usize,
    height: usize,
    search: SearchMode,
    search_bg: Option<AnsiColor>,
    search_message: Option<&'static str>,
    all_files: Vec<String>,
    visible_files: Vec<String>,
    file_headers: Vec<FileHeaderLine>,
    file_filter_query: String,
    file_filter_open: bool,
    help_open: bool,
    selection: Option<Selection>,
    pane_layout: PaneLayout,
}

impl<F> PagerState<F>
where
    F: Fn(usize, &str, &TintPalette) -> (String, PaneLayout),
{
    fn new(
        render: F,
        width: usize,
        height: usize,
        initial_output: String,
        initial_layout: PaneLayout,
        all_files: Vec<String>,
        palette: TintPalette,
    ) -> Self {
        let lines: Vec<String> = initial_output.lines().map(str::to_owned).collect();
        let plain_lines = rebuild_plain_lines(&lines);
        let visible_files = all_files.clone();
        let file_headers = build_file_header_lines(&plain_lines, &visible_files);

        Self {
            render,
            palette,
            lines,
            plain_lines,
            offset: 0,
            horizontal_offset: 0,
            width,
            height,
            search: SearchMode::Inactive,
            search_bg: search_highlight_bg(),
            search_message: None,
            all_files,
            visible_files,
            file_headers,
            file_filter_query: String::new(),
            file_filter_open: false,
            help_open: false,
            selection: None,
            pane_layout: initial_layout,
        }
    }

    fn refresh_dimensions(&mut self) -> Result<()> {
        let (width, height) = terminal::size().context("failed to read terminal size")?;
        self.set_dimensions(width as usize, height as usize);
        Ok(())
    }

    fn set_dimensions(&mut self, width: usize, height: usize) {
        let width_changed = self.width != width || self.lines.is_empty();
        self.width = width;
        self.height = height;

        if width_changed {
            let preferred = self.current_top_file_name();
            self.rerender(preferred);
        } else {
            self.clamp_offset();
        }
    }

    fn rerender(&mut self, preferred_file: Option<String>) {
        let (output, layout) = (self.render)(self.width, &self.file_filter_query, &self.palette);
        self.pane_layout = layout;
        self.lines = output.lines().map(str::to_owned).collect();
        self.plain_lines = rebuild_plain_lines(&self.lines);
        self.visible_files = filter_file_names(&self.all_files, &self.file_filter_query);
        self.file_headers = build_file_header_lines(&self.plain_lines, &self.visible_files);

        if matches!(self.search, SearchMode::Active { .. }) {
            self.refresh_search_matches();
        } else {
            self.restore_offset_for_file(preferred_file);
        }

        self.clamp_offset();
    }

    fn viewport_height(&self) -> usize {
        self.height.saturating_sub(self.hud_height())
    }

    fn hud_height(&self) -> usize {
        if self.file_filter_open {
            let max_hud_rows = self.height.saturating_sub(1).max(1);
            let requested_rows = self.visible_files.len().saturating_add(1);
            requested_rows.min(max_hud_rows)
        } else if self.search_visible() {
            1
        } else {
            0
        }
    }

    fn line_at(&self, row: usize) -> Option<&str> {
        self.lines.get(self.offset + row).map(String::as_str)
    }

    fn rendered_line_at(&self, row: usize) -> Option<String> {
        let line_index = self.offset + row;
        let line = self.lines.get(line_index)?;
        let matches = self.matches_for_line(line_index);
        let selection = self.selection_columns_for_line(line_index);
        Some(render_highlighted_line(
            line,
            self.horizontal_offset,
            self.width,
            &matches,
            self.search_bg,
            selection,
            self.pane_layout
                .content_start
                .max(self.pane_layout.right_start),
        ))
    }

    fn page_up(&mut self) {
        self.offset = self.offset.saturating_sub(self.page_size());
    }

    fn page_down(&mut self) {
        self.offset = (self.offset + self.page_size()).min(self.max_offset());
    }

    fn scroll_up(&mut self, lines: usize) {
        self.offset = self.offset.saturating_sub(lines);
    }

    fn scroll_down(&mut self, lines: usize) {
        self.offset = (self.offset + lines).min(self.max_offset());
    }

    fn to_top(&mut self) {
        self.offset = 0;
    }

    fn to_bottom(&mut self) {
        self.offset = self.max_offset();
    }

    fn max_offset(&self) -> usize {
        self.lines.len().saturating_sub(self.viewport_height())
    }

    fn max_horizontal_offset(&self) -> usize {
        if self.width == 0 {
            return 0;
        }

        self.plain_lines
            .iter()
            .map(|line| display_width(line))
            .max()
            .unwrap_or(0)
            .saturating_sub(self.width)
    }

    fn page_size(&self) -> usize {
        self.viewport_height().max(1)
    }

    fn clamp_offset(&mut self) {
        self.offset = self.offset.min(self.max_offset());
        self.horizontal_offset = self.horizontal_offset.min(self.max_horizontal_offset());
    }

    fn refresh_palette_from_terminal(&mut self) {
        self.apply_palette(TintPalette::detect(), search_highlight_bg());
    }

    fn apply_palette(&mut self, palette: TintPalette, search_bg: Option<AnsiColor>) {
        if self.palette == palette && self.search_bg == search_bg {
            return;
        }

        let preferred = self.current_top_file_name();
        self.palette = palette;
        self.search_bg = search_bg;
        self.rerender(preferred);
    }

    fn scroll_left(&mut self) {
        self.horizontal_offset = self
            .horizontal_offset
            .saturating_sub(HORIZONTAL_SCROLL_COLUMNS);
    }

    fn scroll_right(&mut self) {
        self.horizontal_offset =
            (self.horizontal_offset + HORIZONTAL_SCROLL_COLUMNS).min(self.max_horizontal_offset());
    }

    fn handle_hud_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if self.help_open {
            return match code {
                KeyCode::Char('?') | KeyCode::Esc => {
                    self.help_open = false;
                    true
                }
                _ => true,
            };
        }

        if code == KeyCode::Char('?') {
            self.help_open = true;
            return true;
        }

        if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('f') {
            self.open_file_filter();
            return true;
        }

        if self.file_filter_open {
            return self.handle_file_filter_key(code, modifiers);
        }

        self.handle_search_key(code)
    }

    fn handle_search_key(&mut self, code: KeyCode) -> bool {
        match (&mut self.search, code) {
            (SearchMode::Inactive, KeyCode::Char('/')) => {
                self.file_filter_open = false;
                self.search = SearchMode::Prompt {
                    query: String::new(),
                };
                self.search_message = None;
                true
            }
            (SearchMode::Prompt { .. }, KeyCode::Esc) => {
                self.search = SearchMode::Inactive;
                self.search_message = None;
                self.clamp_offset();
                true
            }
            (SearchMode::Prompt { query }, KeyCode::Backspace) => {
                query.pop();
                self.search_message = None;
                true
            }
            (SearchMode::Prompt { .. }, KeyCode::Enter) => {
                self.commit_search();
                true
            }
            (SearchMode::Prompt { query }, KeyCode::Char(ch)) => {
                query.push(ch);
                self.search_message = None;
                true
            }
            (SearchMode::Active { .. }, KeyCode::Esc) => {
                self.search = SearchMode::Inactive;
                self.search_message = None;
                self.clamp_offset();
                true
            }
            (SearchMode::Active { .. }, KeyCode::Char('/')) => {
                let query = self.search_query().unwrap_or_default();
                self.search = SearchMode::Prompt { query };
                self.search_message = None;
                true
            }
            (SearchMode::Active { .. }, KeyCode::Char('n')) => {
                self.next_match();
                true
            }
            (SearchMode::Active { .. }, KeyCode::Char('N')) => {
                self.previous_match();
                true
            }
            _ => false,
        }
    }

    fn handle_file_filter_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        match code {
            KeyCode::Esc | KeyCode::Enter => {
                self.file_filter_open = false;
                self.clamp_offset();
                true
            }
            KeyCode::Backspace => {
                let mut query = self.file_filter_query.clone();
                query.pop();
                self.apply_file_filter_query(query);
                true
            }
            KeyCode::Up => {
                self.navigate_previous_file();
                true
            }
            KeyCode::Down => {
                self.navigate_next_file();
                true
            }
            KeyCode::Home => {
                self.jump_to_file_index(0);
                true
            }
            KeyCode::End => {
                if !self.file_headers.is_empty() {
                    self.jump_to_file_index(self.file_headers.len() - 1);
                }
                true
            }
            KeyCode::Char(ch) if !modifiers.contains(KeyModifiers::CONTROL) => {
                let mut query = self.file_filter_query.clone();
                query.push(ch);
                self.apply_file_filter_query(query);
                true
            }
            _ => false,
        }
    }

    fn open_file_filter(&mut self) {
        self.search = SearchMode::Inactive;
        self.search_message = None;
        self.file_filter_open = true;
        if self.visible_files.is_empty() {
            self.offset = 0;
        }
    }

    fn apply_file_filter_query(&mut self, query: String) {
        let preferred = self.current_top_file_name();
        self.file_filter_query = query;
        self.search = SearchMode::Inactive;
        self.search_message = None;
        self.rerender(preferred);
    }

    fn navigate_previous_file(&mut self) {
        let Some(current) = self.current_top_file_index() else {
            self.jump_to_file_index(0);
            return;
        };

        if current > 0 {
            self.jump_to_file_index(current - 1);
        }
    }

    fn navigate_next_file(&mut self) {
        let Some(current) = self.current_top_file_index() else {
            self.jump_to_file_index(0);
            return;
        };

        if current + 1 < self.file_headers.len() {
            self.jump_to_file_index(current + 1);
        }
    }

    fn jump_to_file_index(&mut self, index: usize) {
        if let Some(header) = self.file_headers.get(index) {
            self.offset = if self.file_filter_open {
                header.line
            } else {
                header.line.min(self.max_offset())
            };
        }
    }

    fn restore_offset_for_file(&mut self, preferred_file: Option<String>) {
        let target = preferred_file
            .and_then(|name| {
                self.file_headers
                    .iter()
                    .position(|header| header.name == name)
            })
            .or_else(|| {
                usize::from(!self.file_headers.is_empty())
                    .checked_sub(1)
                    .map(|_| 0)
            });

        if let Some(index) = target {
            self.jump_to_file_index(index);
        } else {
            self.offset = 0;
        }
    }

    fn current_top_file_name(&self) -> Option<String> {
        self.current_top_file_index()
            .and_then(|index| self.file_headers.get(index))
            .map(|header| header.name.clone())
    }

    fn current_top_file_index(&self) -> Option<usize> {
        self.file_headers
            .iter()
            .rposition(|header| header.line <= self.offset)
            .or_else(|| (!self.file_headers.is_empty()).then_some(0))
    }

    fn commit_search(&mut self) {
        let SearchMode::Prompt { query } = &self.search else {
            return;
        };

        if query.is_empty() {
            self.search = SearchMode::Inactive;
            self.search_message = None;
            self.clamp_offset();
            return;
        }

        let query = query.clone();
        let matches = find_matches(&self.plain_lines, &query);
        self.search = SearchMode::Active {
            query,
            matches,
            current: 0,
        };
        self.search_message = None;
        self.jump_to_current_match();
    }

    fn refresh_search_matches(&mut self) {
        let SearchMode::Active { query, current, .. } = &self.search else {
            return;
        };

        let query = query.clone();
        let current = *current;
        let matches = find_matches(&self.plain_lines, &query);
        let current = if matches.is_empty() {
            0
        } else {
            current.min(matches.len() - 1)
        };
        self.search = SearchMode::Active {
            query,
            matches,
            current,
        };
        self.search_message = None;
        self.jump_to_current_match();
    }

    fn next_match(&mut self) {
        if let SearchMode::Active {
            matches, current, ..
        } = &mut self.search
        {
            if matches.is_empty() {
                return;
            }
            if *current + 1 >= matches.len() {
                self.search_message = Some("end of file");
                return;
            }
            *current += 1;
            self.search_message = None;
            self.jump_to_current_match();
        }
    }

    fn previous_match(&mut self) {
        if let SearchMode::Active {
            matches, current, ..
        } = &mut self.search
        {
            if matches.is_empty() {
                return;
            }
            if *current == 0 {
                self.search_message = Some("beginning of file");
                return;
            }
            *current -= 1;
            self.search_message = None;
            self.jump_to_current_match();
        }
    }

    fn jump_to_current_match(&mut self) {
        let Some((line, start)) = self.current_match_position() else {
            return;
        };
        let anchor = self.viewport_height() / 2;
        self.offset = line.saturating_sub(anchor).min(self.max_offset());
        self.reveal_line_column(line, start);
    }

    fn current_match_position(&self) -> Option<(usize, usize)> {
        match &self.search {
            SearchMode::Active {
                matches, current, ..
            } => matches
                .get(*current)
                .map(|matched| (matched.line, matched.start)),
            SearchMode::Inactive | SearchMode::Prompt { .. } => None,
        }
    }

    fn reveal_line_column(&mut self, line_index: usize, plain_offset: usize) {
        if self.width == 0 {
            return;
        }

        let Some(line) = self.plain_lines.get(line_index) else {
            return;
        };

        let column = plain_offset_to_column(line, plain_offset);
        if column < self.horizontal_offset {
            self.horizontal_offset = column;
        } else if column >= self.horizontal_offset + self.width {
            self.horizontal_offset = column + 1 - self.width;
        }
        self.horizontal_offset = self.horizontal_offset.min(self.max_horizontal_offset());
    }

    fn matches_for_line(&self, line: usize) -> Vec<SearchMatch> {
        match &self.search {
            SearchMode::Active { matches, .. } => matches
                .iter()
                .filter(|matched| matched.line == line)
                .cloned()
                .collect(),
            SearchMode::Inactive | SearchMode::Prompt { .. } => Vec::new(),
        }
    }

    fn search_visible(&self) -> bool {
        !matches!(self.search, SearchMode::Inactive)
    }

    fn search_query(&self) -> Option<String> {
        match &self.search {
            SearchMode::Prompt { query } => Some(query.clone()),
            SearchMode::Active { query, .. } => Some(query.clone()),
            SearchMode::Inactive => None,
        }
    }

    fn hud_lines(&self) -> Vec<String> {
        if self.file_filter_open {
            return self.file_filter_hud_lines();
        }

        match &self.search {
            SearchMode::Inactive => Vec::new(),
            SearchMode::Prompt { query } => {
                vec![render_search_hud(query, self.width, self.search_bg, None)]
            }
            SearchMode::Active {
                query,
                matches,
                current,
            } => {
                let mut status = if matches.is_empty() {
                    "0/0".to_owned()
                } else {
                    format!("{}/{}", current + 1, matches.len())
                };
                if let Some(message) = self.search_message {
                    status.push(' ');
                    status.push_str(message);
                }
                vec![render_search_hud(
                    query,
                    self.width,
                    self.search_bg,
                    Some(&status),
                )]
            }
        }
    }

    fn file_filter_hud_lines(&self) -> Vec<String> {
        let hud_height = self.hud_height();
        if hud_height == 0 {
            return Vec::new();
        }

        let list_rows = hud_height.saturating_sub(1);
        let current_index = self.current_top_file_index().unwrap_or(0);
        let start = if self.visible_files.len() <= list_rows {
            0
        } else {
            current_index.min(self.visible_files.len().saturating_sub(list_rows))
        };
        let end = (start + list_rows).min(self.visible_files.len());
        let current_file = self.current_top_file_name();

        let mut lines = Vec::new();
        for file in &self.visible_files[start..end] {
            lines.push(render_hud_row(
                &format!("  {file}"),
                self.width,
                self.search_bg,
                current_file.as_deref() == Some(file.as_str()),
            ));
        }

        while lines.len() < list_rows {
            lines.push(render_hud_row("", self.width, self.search_bg, false));
        }

        lines.push(render_hud_row(
            &format!("{FILE_FILTER_PROMPT}{}", self.file_filter_query),
            self.width,
            self.search_bg,
            false,
        ));
        lines
    }

    fn hud_cursor_position(&self) -> Option<(u16, u16)> {
        if self.help_open {
            return None;
        }

        if self.file_filter_open {
            let prompt = clip_plain_text(
                &format!("{FILE_FILTER_PROMPT}{}", self.file_filter_query),
                self.width,
            );
            return Some((
                display_width(&prompt) as u16,
                self.height.saturating_sub(1) as u16,
            ));
        }

        let SearchMode::Prompt { query } = &self.search else {
            return None;
        };

        let prompt = clip_plain_text(&format!("/{query}"), self.width);
        Some((
            display_width(&prompt) as u16,
            self.height.saturating_sub(1) as u16,
        ))
    }

    fn pane_at_column(&self, content_col: usize) -> SelectionPane {
        let layout = &self.pane_layout;
        if layout.left_end == 0 {
            SelectionPane::Full
        } else if content_col < layout.left_end {
            SelectionPane::Left
        } else if content_col >= layout.right_start {
            SelectionPane::Right
        } else {
            SelectionPane::Left
        }
    }

    fn selection_columns_for_line(&self, line_index: usize) -> Option<(usize, usize)> {
        let sel = self.selection.as_ref()?;
        let start = sel.anchor_line.min(sel.extent_line);
        let end = sel.anchor_line.max(sel.extent_line);
        if line_index < start || line_index > end {
            return None;
        }
        let layout = &self.pane_layout;
        match sel.pane {
            SelectionPane::Full => Some((layout.content_start, usize::MAX)),
            SelectionPane::Left => Some((0, layout.left_end)),
            SelectionPane::Right => Some((layout.right_start, usize::MAX)),
        }
    }

    fn extract_selection_text(&self) -> String {
        let Some(sel) = &self.selection else {
            return String::new();
        };
        let start = sel.anchor_line.min(sel.extent_line);
        let end = sel.anchor_line.max(sel.extent_line);
        let layout = &self.pane_layout;
        let (col_start, col_end) = match sel.pane {
            SelectionPane::Full => (layout.content_start, usize::MAX),
            SelectionPane::Left => (0, layout.left_end),
            SelectionPane::Right => (layout.right_start, usize::MAX),
        };

        let mut lines = Vec::new();
        for i in start..=end {
            if let Some(line) = self.plain_lines.get(i) {
                let extracted = extract_column_range(line, col_start, col_end);
                lines.push(extracted.trim_end().to_owned());
            }
        }
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }
}

fn extract_column_range(text: &str, start_col: usize, end_col: usize) -> String {
    let mut result = String::new();
    let mut col = 0usize;
    for ch in text.chars() {
        let w = char_width(ch);
        if col >= start_col && col + w <= end_col {
            result.push(ch);
        }
        col += w;
        if col >= end_col {
            break;
        }
    }
    result
}

fn encode_base64(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    let mut i = 0;
    while i < data.len() {
        let b0 = data[i] as u32;
        let b1 = if i + 1 < data.len() {
            data[i + 1] as u32
        } else {
            0
        };
        let b2 = if i + 2 < data.len() {
            data[i + 2] as u32
        } else {
            0
        };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3f) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3f) as usize] as char);
        if i + 1 < data.len() {
            result.push(CHARS[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            result.push('=');
        }
        if i + 2 < data.len() {
            result.push(CHARS[(triple & 0x3f) as usize] as char);
        } else {
            result.push('=');
        }
        i += 3;
    }
    result
}

fn write_osc52(stdout: &mut io::Stdout, text: &str) -> Result<()> {
    let encoded = encode_base64(text.as_bytes());
    write!(stdout, "\x1b]52;c;{encoded}\x07").context("failed to write OSC 52")?;
    stdout.flush().context("failed to flush OSC 52")
}

fn render_centered_overlay_lines(
    width: usize,
    height: usize,
    background: Option<AnsiColor>,
    lines: &[&str],
) -> Vec<(u16, u16, String)> {
    if width == 0 || height == 0 || lines.is_empty() {
        return Vec::new();
    }

    let content_width = lines
        .iter()
        .map(|line| display_width(line))
        .max()
        .unwrap_or(0);
    let box_width = (content_width + 4).min(width);
    let box_height = lines.len().min(height);
    let start_x = width.saturating_sub(box_width) / 2;
    let start_y = height.saturating_sub(box_height) / 2;
    let blank = " ".repeat(box_width);
    let mut rendered = Vec::new();

    for (index, line) in lines.iter().take(box_height).enumerate() {
        let clipped = clip_plain_text(line, box_width.saturating_sub(4));
        let row = format!("  {clipped}");
        let padded = if display_width(&row) < box_width {
            format!("{row}{}", " ".repeat(box_width - display_width(&row)))
        } else {
            row
        };
        let bold = index == 0;
        rendered.push((
            start_x as u16,
            (start_y + index) as u16,
            render_hud_row(&padded, box_width, background, bold),
        ));
    }

    if box_height < lines.len() {
        return rendered;
    }

    if rendered.is_empty() {
        rendered.push((
            start_x as u16,
            start_y as u16,
            render_hud_row(&blank, box_width, background, false),
        ));
    }

    rendered
}

#[cfg(test)]
mod tests {
    use super::PagerState;
    use super::SearchMode;
    use super::build_file_header_lines;
    use super::clip_ansi_text;
    use super::clip_ansi_text_from;
    use super::encode_base64;
    use super::extract_column_range;
    use super::filter_file_names;
    use super::find_matches;
    use super::render_centered_overlay_lines;
    use super::render_highlighted_line;
    use super::render_search_hud;
    use super::should_page_output;
    use super::strip_ansi_text;
    use crate::render::PaneLayout;
    use crate::render::TintPalette;
    use crate::terminal_palette::AnsiColor;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyModifiers;

    fn test_render(s: String) -> (String, PaneLayout) {
        (s, PaneLayout::default())
    }

    #[test]
    fn clip_ansi_text_preserves_escape_sequences() {
        let text = "\u{1b}[1mheader\u{1b}[0m";
        let clipped = clip_ansi_text(text, 3);
        assert!(clipped.contains("\u{1b}[1m"));
        assert!(clipped.contains("\u{1b}[0m"));
    }

    #[test]
    fn clipped_overflow_uses_right_marker() {
        assert_eq!(clip_ansi_text_from("abcdefgh", 0, 4), "abc»");
        assert_eq!(clip_ansi_text_from("abcdefgh", 4, 4), "efgh");
    }

    #[test]
    fn force_pager_overrides_screen_fit() {
        assert!(should_page_output(true, true, 5, 20));
    }

    #[test]
    fn fitting_output_skips_pager_without_override() {
        assert!(!should_page_output(true, false, 5, 20));
    }

    #[test]
    fn non_tty_output_never_pages() {
        assert!(!should_page_output(false, true, 50, 20));
    }

    #[test]
    fn strip_ansi_text_removes_escape_sequences() {
        assert_eq!(strip_ansi_text("\u{1b}[1mheader\u{1b}[0m"), "header");
    }

    #[test]
    fn pager_page_movement_is_page_sized() {
        let mut state = PagerState::new(
            |_, _, _| test_render("1\n2\n3\n4\n5\n6\n7\n8\n9\n10".into()),
            80,
            3,
            "1\n2\n3\n4\n5\n6\n7\n8\n9\n10".into(),
            PaneLayout::default(),
            Vec::new(),
            TintPalette::default(),
        );
        state.page_down();
        assert_eq!(state.offset, 3);
        state.page_up();
        assert_eq!(state.offset, 0);
    }

    #[test]
    fn pager_scroll_is_line_sized() {
        let mut state = PagerState::new(
            |_, _, _| test_render("1\n2\n3\n4\n5".into()),
            80,
            2,
            "1\n2\n3\n4\n5".into(),
            PaneLayout::default(),
            Vec::new(),
            TintPalette::default(),
        );
        state.scroll_down(3);
        assert_eq!(state.offset, 3);
        state.scroll_up(2);
        assert_eq!(state.offset, 1);
    }

    #[test]
    fn rerenders_when_width_changes() {
        let mut state = PagerState::new(
            |width, _, _| test_render(format!("width={width}")),
            80,
            10,
            "width=80".into(),
            PaneLayout::default(),
            Vec::new(),
            TintPalette::default(),
        );
        assert_eq!(state.line_at(0), Some("width=80"));

        state.set_dimensions(100, 10);

        assert_eq!(state.line_at(0), Some("width=100"));
    }

    #[test]
    fn horizontal_scroll_is_clamped_to_widest_displayed_line() {
        let line = "this is a very long line";
        let mut state = PagerState::new(
            |_, _, _| test_render(format!("short\n{line}")),
            10,
            4,
            format!("short\n{line}"),
            PaneLayout::default(),
            Vec::new(),
            TintPalette::default(),
        );

        state.scroll_right();
        state.scroll_right();
        state.scroll_right();

        assert_eq!(state.horizontal_offset, line.len() - 10);

        state.set_dimensions(20, 4);

        assert_eq!(state.horizontal_offset, line.len() - 20);
    }

    #[test]
    fn finds_matches_in_all_plain_text() {
        let matches = find_matches(
            &[
                "alpha beta".to_owned(),
                "gamma alpha".to_owned(),
                "delta".to_owned(),
            ],
            "alpha",
        );
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].line, 0);
        assert_eq!(matches[1].line, 1);
    }

    #[test]
    fn search_matches_rendered_text_without_ansi() {
        let mut state = PagerState::new(
            |_, _, _| test_render("\u{1b}[1malpha\u{1b}[0m\nbeta".into()),
            80,
            4,
            "\u{1b}[1malpha\u{1b}[0m\nbeta".into(),
            PaneLayout::default(),
            Vec::new(),
            TintPalette::default(),
        );

        assert!(state.handle_hud_key(KeyCode::Char('/'), KeyModifiers::NONE));
        assert!(state.handle_hud_key(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(state.handle_hud_key(KeyCode::Char('l'), KeyModifiers::NONE));
        assert!(state.handle_hud_key(KeyCode::Enter, KeyModifiers::NONE));

        match &state.search {
            SearchMode::Active { matches, .. } => assert_eq!(matches.len(), 1),
            SearchMode::Inactive | SearchMode::Prompt { .. } => panic!("search not active"),
        }
    }

    #[test]
    fn highlight_reuses_search_background() {
        let rendered = render_highlighted_line(
            "\u{1b}[1malpha\u{1b}[0m",
            0,
            80,
            &[super::SearchMatch {
                line: 0,
                start: 0,
                end: 5,
            }],
            Some(AnsiColor::Indexed(240)),
            None,
            0,
        );
        assert!(rendered.contains("\u{1b}[1;48;5;240m"));
    }

    #[test]
    fn tinted_lines_fill_to_edge_without_forcing_overflow_marker() {
        let rendered =
            render_highlighted_line("\u{1b}[48;5;240mabc\u{1b}[0m", 0, 6, &[], None, None, 0);
        assert!(rendered.contains("\u{1b}[48;5;240mabc"));
        assert!(rendered.contains("   \u{1b}[0m"));
        assert!(!rendered.contains('»'));
    }

    #[test]
    fn gutter_only_background_does_not_fill_to_edge() {
        let rendered =
            render_highlighted_line("  12 \u{1b}[48;5;238m \u{1b}[0m", 0, 10, &[], None, None, 6);
        assert_eq!(rendered.matches("\u{1b}[48;5;238m").count(), 1);
        assert!(!rendered.contains("\u{1b}[48;5;238m    "));
    }

    #[test]
    fn active_search_reserves_bottom_row_for_hud() {
        let mut state = PagerState::new(
            |_, _, _| test_render("a\nb\nc\nd".into()),
            80,
            4,
            "a\nb\nc\nd".into(),
            PaneLayout::default(),
            Vec::new(),
            TintPalette::default(),
        );
        assert!(state.handle_hud_key(KeyCode::Char('/'), KeyModifiers::NONE));
        assert!(state.handle_hud_key(KeyCode::Char('b'), KeyModifiers::NONE));
        assert!(state.handle_hud_key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(state.viewport_height(), 3);
        assert_eq!(state.hud_lines().len(), 1);
    }

    #[test]
    fn search_navigation_stops_at_edges_and_reports_boundary() {
        let mut state = PagerState::new(
            |_, _, _| test_render("alpha\nbeta alpha\ngamma alpha".into()),
            80,
            3,
            "alpha\nbeta alpha\ngamma alpha".into(),
            PaneLayout::default(),
            Vec::new(),
            TintPalette::default(),
        );
        assert!(state.handle_hud_key(KeyCode::Char('/'), KeyModifiers::NONE));
        assert!(state.handle_hud_key(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(state.handle_hud_key(KeyCode::Char('l'), KeyModifiers::NONE));
        assert!(state.handle_hud_key(KeyCode::Char('p'), KeyModifiers::NONE));
        assert!(state.handle_hud_key(KeyCode::Char('h'), KeyModifiers::NONE));
        assert!(state.handle_hud_key(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(state.handle_hud_key(KeyCode::Enter, KeyModifiers::NONE));

        state.next_match();
        state.next_match();
        assert!(state.handle_hud_key(KeyCode::Char('n'), KeyModifiers::NONE));

        match &state.search {
            SearchMode::Active { current, .. } => assert_eq!(*current, 2),
            SearchMode::Inactive | SearchMode::Prompt { .. } => panic!("search not active"),
        }
        assert!(state.hud_lines().join("\n").contains("end of file"));

        assert!(state.handle_hud_key(KeyCode::Char('N'), KeyModifiers::NONE));
        assert!(state.handle_hud_key(KeyCode::Char('N'), KeyModifiers::NONE));
        assert!(state.handle_hud_key(KeyCode::Char('N'), KeyModifiers::NONE));

        match &state.search {
            SearchMode::Active { current, .. } => assert_eq!(*current, 0),
            SearchMode::Inactive | SearchMode::Prompt { .. } => panic!("search not active"),
        }
        assert!(state.hud_lines().join("\n").contains("beginning of file"));
    }

    #[test]
    fn hud_is_full_width_and_uses_background() {
        let hud = render_search_hud("alpha", 12, Some(AnsiColor::Indexed(240)), Some("1/3"));
        assert!(hud.contains("\u{1b}[48;5;240m"));
        assert!(hud.contains("/alpha"));
        assert!(hud.contains("1/3"));
    }

    #[test]
    fn file_filter_narrows_files_and_rerenders_output() {
        let files = vec!["a.go".into(), "b.rs".into(), "c.go".into()];
        let mut state = PagerState::new(
            |_, filter, _| {
                if filter.is_empty() {
                    test_render("a.go\nbody\nb.rs\nbody\nc.go\nbody".into())
                } else {
                    test_render(format!("{filter}\nfiltered"))
                }
            },
            80,
            10,
            "a.go\nbody\nb.rs\nbody\nc.go\nbody".into(),
            PaneLayout::default(),
            files,
            TintPalette::default(),
        );

        assert!(state.handle_hud_key(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert!(state.handle_hud_key(KeyCode::Char('.'), KeyModifiers::NONE));
        assert!(state.handle_hud_key(KeyCode::Char('g'), KeyModifiers::NONE));
        assert!(state.handle_hud_key(KeyCode::Char('o'), KeyModifiers::NONE));

        assert_eq!(state.file_filter_query, ".go");
        assert_eq!(
            state.visible_files,
            vec!["a.go".to_owned(), "c.go".to_owned()]
        );
        assert_eq!(state.line_at(0), Some(".go"));
    }

    #[test]
    fn file_filter_hud_bolds_current_top_file() {
        let files = vec!["file1".into(), "file2".into()];
        let initial = "\u{1b}[1mfile1\u{1b}[0m\nx\n\u{1b}[1mfile2\u{1b}[0m\ny".to_owned();
        let rendered = initial.clone();
        let mut state = PagerState::new(
            |_, _, _| test_render(rendered.clone()),
            80,
            6,
            initial,
            PaneLayout::default(),
            files,
            TintPalette::default(),
        );
        assert!(state.handle_hud_key(KeyCode::Char('f'), KeyModifiers::CONTROL));
        state.navigate_next_file();

        let hud = state.hud_lines().join("\n");
        assert!(hud.contains("\u{1b}[1m") || hud.contains("\u{1b}[1;48"));
        assert!(hud.contains("file2"));
    }

    #[test]
    fn file_filter_navigation_uses_up_and_down() {
        let files = vec!["file1".into(), "file2".into()];
        let initial = "file1\nx\nfile2\ny".to_owned();
        let rendered = initial.clone();
        let mut state = PagerState::new(
            |_, _, _| test_render(rendered.clone()),
            80,
            6,
            initial,
            PaneLayout::default(),
            files,
            TintPalette::default(),
        );
        assert!(state.handle_hud_key(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert!(state.handle_hud_key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(state.current_top_file_name().as_deref(), Some("file2"));
        assert!(state.handle_hud_key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(state.current_top_file_name().as_deref(), Some("file1"));
    }

    #[test]
    fn applying_new_palette_rerenders_output() {
        let mut state = PagerState::new(
            |_, _, palette| test_render(format!("{:?}", palette.changed_line_bg)),
            80,
            4,
            "None".into(),
            PaneLayout::default(),
            Vec::new(),
            TintPalette::default(),
        );

        state.apply_palette(
            TintPalette {
                changed_line_bg: Some(AnsiColor::Indexed(240)),
                gutter_fg: Some(AnsiColor::Indexed(238)),
            },
            None,
        );

        assert_eq!(state.line_at(0), Some("Some(Indexed(240))"));
    }

    #[test]
    fn file_name_filter_matches_by_substring() {
        let filtered =
            filter_file_names(&["foo.go".into(), "bar.rs".into(), "baz.go".into()], ".go");
        assert_eq!(filtered, vec!["foo.go".to_owned(), "baz.go".to_owned()]);
    }

    #[test]
    fn file_headers_are_discovered_in_rendered_output() {
        let headers = build_file_header_lines(
            &[
                "file1".to_owned(),
                "  body".to_owned(),
                "file2".to_owned(),
                "  body".to_owned(),
            ],
            &["file1".into(), "file2".into()],
        );
        assert_eq!(headers[0].line, 0);
        assert_eq!(headers[1].line, 2);
    }

    #[test]
    fn question_mark_toggles_help_overlay() {
        let mut state = PagerState::new(
            |_, _, _| test_render("file1\nbody".into()),
            80,
            10,
            "file1\nbody".into(),
            PaneLayout::default(),
            Vec::new(),
            TintPalette::default(),
        );
        assert!(state.handle_hud_key(KeyCode::Char('?'), KeyModifiers::NONE));
        assert!(state.help_open);
        assert!(state.hud_cursor_position().is_none());
        assert!(state.handle_hud_key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!state.help_open);
    }

    #[test]
    fn help_overlay_uses_hud_tint_and_centers_content() {
        let overlay =
            render_centered_overlay_lines(40, 20, Some(AnsiColor::Indexed(240)), &["mdiff help"]);
        assert_eq!(overlay.len(), 1);
        assert!(overlay[0].0 > 0);
        assert!(overlay[0].1 > 0);
        assert!(
            overlay[0].2.contains("\u{1b}[48;5;240m")
                || overlay[0].2.contains("\u{1b}[1;48;5;240m")
        );
    }

    #[test]
    fn extract_column_range_selects_by_display_width() {
        assert_eq!(extract_column_range("abcdefgh", 2, 5), "cde");
        assert_eq!(extract_column_range("abcdefgh", 0, 3), "abc");
        assert_eq!(extract_column_range("abcdefgh", 6, 100), "gh");
        assert_eq!(extract_column_range("abcdefgh", 0, 100), "abcdefgh");
    }

    #[test]
    fn encode_base64_matches_known_values() {
        assert_eq!(encode_base64(b"hello"), "aGVsbG8=");
        assert_eq!(encode_base64(b"ab"), "YWI=");
        assert_eq!(encode_base64(b"abc"), "YWJj");
        assert_eq!(encode_base64(b""), "");
    }
}
