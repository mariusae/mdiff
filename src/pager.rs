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

pub fn page_or_render<F>(render: F) -> Result<Option<String>>
where
    F: Fn(usize) -> String,
{
    if !std::io::stdout().is_terminal() {
        return Ok(Some(render(0)));
    }

    let (width, rows) = terminal::size().context("failed to read terminal size")?;
    let width = width as usize;
    let rows = rows as usize;
    let initial_output = render(width);

    if line_count(&initial_output) <= rows {
        return Ok(Some(initial_output));
    }

    page(render, width, rows, initial_output)?;
    Ok(None)
}

fn page<F>(render: F, width: usize, height: usize, initial_output: String) -> Result<()>
where
    F: Fn(usize) -> String,
{
    let mut stdout = io::stdout();
    let mut state = PagerState::new(render, width, height, initial_output);

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
    F: Fn(usize) -> String,
{
    loop {
        state.refresh_dimensions()?;
        draw(stdout, state)?;

        match event::read().context("failed to read pager input")? {
            Event::Key(key) => {
                if state.handle_search_key(key.code) {
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
    F: Fn(usize) -> String,
{
    let height = state.viewport_height();
    queue!(stdout, cursor::MoveTo(0, 0), Clear(ClearType::All))
        .context("failed to clear pager screen")?;

    for row in 0..height {
        queue!(stdout, cursor::MoveTo(0, row as u16)).context("failed to move cursor")?;
        if let Some(line) = state.rendered_line_at(row) {
            queue!(stdout, Print(line)).context("failed to draw line")?;
        }
    }

    if let Some(hud) = state.search_hud_line() {
        let hud_row = state.height.saturating_sub(1) as u16;
        queue!(stdout, cursor::MoveTo(0, hud_row), Print(hud))
            .context("failed to draw search hud")?;
    }

    if let Some((column, row)) = state.search_cursor_position() {
        queue!(stdout, cursor::MoveTo(column, row), cursor::Show)
            .context("failed to place search cursor")?;
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
    } else if text_width < width {
        text.push_str(&" ".repeat(width - text_width));
    }

    if text_width >= width && status.is_none() {
        text = clip_plain_text(&text, width);
    }

    match background {
        Some(color) => format!(
            "{}{}\u{1b}[0m",
            style_prefix(TextStyle {
                background: Some(color),
                ..TextStyle::default()
            })
            .unwrap_or_default(),
            text
        ),
        None => text,
    }
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
    F: Fn(usize) -> String,
{
    render: F,
    lines: Vec<String>,
    plain_lines: Vec<String>,
    offset: usize,
    width: usize,
    height: usize,
    search: SearchMode,
    search_bg: Option<AnsiColor>,
}

impl<F> PagerState<F>
where
    F: Fn(usize) -> String,
{
    fn new(render: F, width: usize, height: usize, initial_output: String) -> Self {
        let lines: Vec<String> = initial_output.lines().map(str::to_owned).collect();
        let plain_lines = rebuild_plain_lines(&lines);
        Self {
            render,
            lines,
            plain_lines,
            offset: 0,
            width,
            height,
            search: SearchMode::Inactive,
            search_bg: search_highlight_bg(),
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
            self.rerender();
        } else {
            self.clamp_offset();
        }
    }

    fn rerender(&mut self) {
        let output = (self.render)(self.width);
        self.lines = output.lines().map(str::to_owned).collect();
        self.plain_lines = rebuild_plain_lines(&self.lines);
        self.refresh_search_matches();
        self.clamp_offset();
    }

    fn viewport_height(&self) -> usize {
        if self.search_visible() {
            self.height.saturating_sub(1)
        } else {
            self.height
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

    fn handle_search_key(&mut self, key: KeyCode) -> bool {
        match (&mut self.search, key) {
            (SearchMode::Inactive, KeyCode::Char('/')) => {
                self.search = SearchMode::Prompt {
                    query: String::new(),
                };
                true
            }
            (SearchMode::Prompt { .. }, KeyCode::Esc) => {
                self.search = SearchMode::Inactive;
                self.clamp_offset();
                true
            }
            (SearchMode::Prompt { query }, KeyCode::Backspace) => {
                query.pop();
                true
            }
            (SearchMode::Prompt { .. }, KeyCode::Enter) => {
                self.commit_search();
                true
            }
            (SearchMode::Prompt { query }, KeyCode::Char(ch)) => {
                query.push(ch);
                true
            }
            (SearchMode::Active { .. }, KeyCode::Esc) => {
                self.search = SearchMode::Inactive;
                self.clamp_offset();
                true
            }
            (SearchMode::Active { .. }, KeyCode::Char('/')) => {
                let query = self.search_query().unwrap_or_default();
                self.search = SearchMode::Prompt { query };
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

    fn commit_search(&mut self) {
        let SearchMode::Prompt { query } = &self.search else {
            return;
        };

        if query.is_empty() {
            self.search = SearchMode::Inactive;
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
        self.jump_to_current_match();
    }

    fn refresh_search_matches(&mut self) {
        let SearchMode::Active {
            query,
            matches: _,
            current,
        } = &self.search
        else {
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
            *current = (*current + 1) % matches.len();
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
            *current = if *current == 0 {
                matches.len() - 1
            } else {
                *current - 1
            };
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

    fn search_hud_line(&self) -> Option<String> {
        match &self.search {
            SearchMode::Inactive => None,
            SearchMode::Prompt { query } => {
                Some(render_search_hud(query, self.width, self.search_bg, None))
            }
            SearchMode::Active {
                query,
                matches,
                current,
            } => {
                let status = if matches.is_empty() {
                    "0/0".to_owned()
                } else {
                    format!("{}/{}", current + 1, matches.len())
                };
                Some(render_search_hud(
                    query,
                    self.width,
                    self.search_bg,
                    Some(&status),
                ))
            }
        }
    }

    fn search_cursor_position(&self) -> Option<(u16, u16)> {
        let SearchMode::Prompt { query } = &self.search else {
            return None;
        };

        let prefix = clip_plain_text(&format!("/{query}"), self.width);
        Some((
            display_width(&prefix) as u16,
            self.height.saturating_sub(1) as u16,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::PagerState;
    use super::SearchMode;
    use super::clip_ansi_text;
    use super::find_matches;
    use super::render_highlighted_line;
    use super::render_search_hud;
    use super::strip_ansi_text;
    use crate::terminal_palette::AnsiColor;
    use crossterm::event::KeyCode;

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
            |_| "1\n2\n3\n4\n5\n6\n7\n8\n9\n10".into(),
            80,
            3,
            "1\n2\n3\n4\n5\n6\n7\n8\n9\n10".into(),
        );
        state.page_down();
        assert_eq!(state.offset, 3);
        state.page_up();
        assert_eq!(state.offset, 0);
    }

    #[test]
    fn pager_scroll_is_line_sized() {
        let mut state = PagerState::new(|_| "1\n2\n3\n4\n5".into(), 80, 2, "1\n2\n3\n4\n5".into());
        state.scroll_down(3);
        assert_eq!(state.offset, 3);
        state.scroll_up(2);
        assert_eq!(state.offset, 1);
    }

    #[test]
    fn rerenders_when_width_changes() {
        let mut state =
            PagerState::new(|width| format!("width={width}"), 80, 10, "width=80".into());
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
            |_| "\u{1b}[1malpha\u{1b}[0m\nbeta".into(),
            80,
            4,
            "\u{1b}[1malpha\u{1b}[0m\nbeta".into(),
        );

        assert!(state.handle_search_key(KeyCode::Char('/')));
        assert!(state.handle_search_key(KeyCode::Char('a')));
        assert!(state.handle_search_key(KeyCode::Char('l')));
        assert!(state.handle_search_key(KeyCode::Enter));

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
        let mut state = PagerState::new(|_| "a\nb\nc\nd".into(), 80, 4, "a\nb\nc\nd".into());
        assert!(state.handle_search_key(KeyCode::Char('/')));
        assert!(state.handle_search_key(KeyCode::Char('b')));
        assert!(state.handle_search_key(KeyCode::Enter));
        assert_eq!(state.viewport_height(), 3);
        assert!(state.search_hud_line().is_some());
    }

    #[test]
    fn search_navigation_cycles_matches() {
        let mut state = PagerState::new(
            |_| "alpha\nbeta alpha\ngamma alpha".into(),
            80,
            3,
            "alpha\nbeta alpha\ngamma alpha".into(),
        );
        assert!(state.handle_search_key(KeyCode::Char('/')));
        assert!(state.handle_search_key(KeyCode::Char('a')));
        assert!(state.handle_search_key(KeyCode::Char('l')));
        assert!(state.handle_search_key(KeyCode::Char('p')));
        assert!(state.handle_search_key(KeyCode::Char('h')));
        assert!(state.handle_search_key(KeyCode::Char('a')));
        assert!(state.handle_search_key(KeyCode::Enter));

        state.next_match();
        assert!(state.handle_search_key(KeyCode::Char('N')));

        match &state.search {
            SearchMode::Active { current, .. } => assert_eq!(*current, 0),
            SearchMode::Inactive | SearchMode::Prompt { .. } => panic!("search not active"),
        }
    }

    #[test]
    fn hud_is_full_width_and_uses_background() {
        let hud = render_search_hud("alpha", 12, Some(AnsiColor::Indexed(240)), Some("1/3"));
        assert!(hud.contains("\u{1b}[48;5;240m"));
        assert!(hud.contains("/alpha"));
        assert!(hud.contains("1/3"));
    }
}
