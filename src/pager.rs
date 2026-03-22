use crate::terminal_palette::AnsiColor;
use crate::terminal_palette::search_highlight_bg;
use anyhow::Context;
use anyhow::Result;
use crossterm::cursor;
use crossterm::event;
use crossterm::event::DisableMouseCapture;
use crossterm::event::EnableMouseCapture;
use crossterm::event::Event;
use crossterm::event::KeyCode;
use crossterm::event::KeyModifiers;
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
const FILE_FILTER_PROMPT: &str = "› ";

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

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileHeaderLine {
    name: String,
    line: usize,
}

pub fn page_or_render<F>(files: Vec<String>, render: F) -> Result<Option<String>>
where
    F: Fn(usize, &str) -> String,
{
    if !std::io::stdout().is_terminal() {
        return Ok(Some(render(0, "")));
    }

    let (width, rows) = terminal::size().context("failed to read terminal size")?;
    let width = width as usize;
    let rows = rows as usize;
    let initial_output = render(width, "");

    if line_count(&initial_output) <= rows {
        return Ok(Some(initial_output));
    }

    page(files, render, width, rows, initial_output)?;
    Ok(None)
}

fn page<F>(
    files: Vec<String>,
    render: F,
    width: usize,
    height: usize,
    initial_output: String,
) -> Result<()>
where
    F: Fn(usize, &str) -> String,
{
    let mut stdout = io::stdout();
    let mut state = PagerState::new(render, width, height, initial_output, files);

    terminal::enable_raw_mode().context("failed to enable raw mode")?;
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        cursor::Hide
    )
    .context("failed to initialize pager screen")?;

    let result = run_pager(&mut stdout, &mut state);

    execute!(
        stdout,
        cursor::Show,
        DisableMouseCapture,
        LeaveAlternateScreen
    )
    .context("failed to restore terminal after pager")?;
    terminal::disable_raw_mode().context("failed to disable raw mode")?;

    result
}

fn run_pager<F>(stdout: &mut io::Stdout, state: &mut PagerState<F>) -> Result<()>
where
    F: Fn(usize, &str) -> String,
{
    loop {
        state.refresh_dimensions()?;
        draw(stdout, state)?;

        match event::read().context("failed to read pager input")? {
            Event::Key(key) => {
                if state.handle_hud_key(key.code, key.modifiers) {
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Esc => return Ok(()),
                    KeyCode::Up | KeyCode::PageUp => state.page_up(),
                    KeyCode::Down | KeyCode::PageDown | KeyCode::Char(' ') => state.page_down(),
                    KeyCode::Home | KeyCode::Char('g') => state.to_top(),
                    KeyCode::End | KeyCode::Char('G') => state.to_bottom(),
                    _ => {}
                }
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => state.scroll_up(MOUSE_SCROLL_LINES),
                MouseEventKind::ScrollDown => state.scroll_down(MOUSE_SCROLL_LINES),
                _ => {}
            },
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
}

fn draw<F>(stdout: &mut io::Stdout, state: &PagerState<F>) -> Result<()>
where
    F: Fn(usize, &str) -> String,
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

fn clip_ansi_text(text: &str, width: usize) -> String {
    let mut rendered = String::new();
    let mut used = 0usize;
    let mut chars = text.chars().peekable();
    let mut saw_ansi = false;

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            saw_ansi = true;
            rendered.push(ch);
            while let Some(next) = chars.next() {
                rendered.push(next);
                if next == 'm' {
                    break;
                }
            }
            continue;
        }

        let ch_width = char_width(ch);
        if used + ch_width > width {
            break;
        }

        rendered.push(ch);
        used += ch_width;
    }

    if saw_ansi {
        rendered.push_str("\u{1b}[0m");
    }

    rendered
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
    width: usize,
    matches: &[SearchMatch],
    search_bg: Option<AnsiColor>,
) -> String {
    if matches.is_empty() {
        return clip_ansi_text(text, width);
    }

    let mut rendered = String::new();
    let mut current_style = TextStyle::default();
    let mut used = 0usize;

    for cell in parse_styled_cells(text) {
        let ch_width = char_width(cell.ch);
        if used + ch_width > width {
            break;
        }

        let mut style = cell.style;
        if search_bg.is_some() && cell_is_highlighted(&cell, matches) {
            style.background = search_bg;
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
    }

    if current_style != TextStyle::default() {
        rendered.push_str("\u{1b}[0m");
    }

    rendered
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
    F: Fn(usize, &str) -> String,
{
    render: F,
    lines: Vec<String>,
    plain_lines: Vec<String>,
    offset: usize,
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
}

impl<F> PagerState<F>
where
    F: Fn(usize, &str) -> String,
{
    fn new(
        render: F,
        width: usize,
        height: usize,
        initial_output: String,
        all_files: Vec<String>,
    ) -> Self {
        let lines: Vec<String> = initial_output.lines().map(str::to_owned).collect();
        let plain_lines = rebuild_plain_lines(&lines);
        let visible_files = all_files.clone();
        let file_headers = build_file_header_lines(&plain_lines, &visible_files);

        Self {
            render,
            lines,
            plain_lines,
            offset: 0,
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
        let output = (self.render)(self.width, &self.file_filter_query);
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
        Some(render_highlighted_line(
            line,
            self.width,
            &matches,
            self.search_bg,
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

    fn page_size(&self) -> usize {
        self.viewport_height().max(1)
    }

    fn clamp_offset(&mut self) {
        self.offset = self.offset.min(self.max_offset());
    }

    fn handle_hud_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
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
        let Some(line) = self.current_match_line() else {
            return;
        };
        let anchor = self.viewport_height() / 2;
        self.offset = line.saturating_sub(anchor).min(self.max_offset());
    }

    fn current_match_line(&self) -> Option<usize> {
        match &self.search {
            SearchMode::Active {
                matches, current, ..
            } => matches.get(*current).map(|matched| matched.line),
            SearchMode::Inactive | SearchMode::Prompt { .. } => None,
        }
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
}

#[cfg(test)]
mod tests {
    use super::PagerState;
    use super::SearchMode;
    use super::build_file_header_lines;
    use super::clip_ansi_text;
    use super::filter_file_names;
    use super::find_matches;
    use super::render_highlighted_line;
    use super::render_search_hud;
    use super::strip_ansi_text;
    use crate::terminal_palette::AnsiColor;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyModifiers;

    #[test]
    fn clip_ansi_text_preserves_escape_sequences() {
        let text = "\u{1b}[1mheader\u{1b}[0m";
        let clipped = clip_ansi_text(text, 3);
        assert!(clipped.contains("\u{1b}[1m"));
        assert!(clipped.contains("\u{1b}[0m"));
    }

    #[test]
    fn strip_ansi_text_removes_escape_sequences() {
        assert_eq!(strip_ansi_text("\u{1b}[1mheader\u{1b}[0m"), "header");
    }

    #[test]
    fn pager_page_movement_is_page_sized() {
        let mut state = PagerState::new(
            |_, _| "1\n2\n3\n4\n5\n6\n7\n8\n9\n10".into(),
            80,
            3,
            "1\n2\n3\n4\n5\n6\n7\n8\n9\n10".into(),
            Vec::new(),
        );
        state.page_down();
        assert_eq!(state.offset, 3);
        state.page_up();
        assert_eq!(state.offset, 0);
    }

    #[test]
    fn pager_scroll_is_line_sized() {
        let mut state = PagerState::new(
            |_, _| "1\n2\n3\n4\n5".into(),
            80,
            2,
            "1\n2\n3\n4\n5".into(),
            Vec::new(),
        );
        state.scroll_down(3);
        assert_eq!(state.offset, 3);
        state.scroll_up(2);
        assert_eq!(state.offset, 1);
    }

    #[test]
    fn rerenders_when_width_changes() {
        let mut state = PagerState::new(
            |width, _| format!("width={width}"),
            80,
            10,
            "width=80".into(),
            Vec::new(),
        );
        assert_eq!(state.line_at(0), Some("width=80"));

        state.set_dimensions(100, 10);

        assert_eq!(state.line_at(0), Some("width=100"));
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
            |_, _| "\u{1b}[1malpha\u{1b}[0m\nbeta".into(),
            80,
            4,
            "\u{1b}[1malpha\u{1b}[0m\nbeta".into(),
            Vec::new(),
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
            80,
            &[super::SearchMatch {
                line: 0,
                start: 0,
                end: 5,
            }],
            Some(AnsiColor::Indexed(240)),
        );
        assert!(rendered.contains("\u{1b}[1;48;5;240m"));
    }

    #[test]
    fn active_search_reserves_bottom_row_for_hud() {
        let mut state = PagerState::new(
            |_, _| "a\nb\nc\nd".into(),
            80,
            4,
            "a\nb\nc\nd".into(),
            Vec::new(),
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
            |_, _| "alpha\nbeta alpha\ngamma alpha".into(),
            80,
            3,
            "alpha\nbeta alpha\ngamma alpha".into(),
            Vec::new(),
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
            |_, filter| {
                if filter.is_empty() {
                    "a.go\nbody\nb.rs\nbody\nc.go\nbody".into()
                } else {
                    format!("{filter}\nfiltered")
                }
            },
            80,
            10,
            "a.go\nbody\nb.rs\nbody\nc.go\nbody".into(),
            files,
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
        let mut state = PagerState::new(|_, _| rendered.clone(), 80, 6, initial, files);
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
        let mut state = PagerState::new(|_, _| rendered.clone(), 80, 6, initial, files);
        assert!(state.handle_hud_key(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert!(state.handle_hud_key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(state.current_top_file_name().as_deref(), Some("file2"));
        assert!(state.handle_hud_key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(state.current_top_file_name().as_deref(), Some("file1"));
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
}
