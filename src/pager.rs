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

const MOUSE_SCROLL_LINES: usize = 3;

pub fn maybe_page(output: &str) -> Result<bool> {
    if !std::io::stdout().is_terminal() {
        return Ok(false);
    }

    let (_, rows) = terminal::size().context("failed to read terminal size")?;
    if line_count(output) <= rows as usize {
        return Ok(false);
    }

    page(output)?;
    Ok(true)
}

fn page(output: &str) -> Result<()> {
    let mut stdout = io::stdout();
    let mut state = PagerState::new(output);

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

fn run_pager(stdout: &mut io::Stdout, state: &mut PagerState) -> Result<()> {
    loop {
        state.refresh_dimensions()?;
        draw(stdout, state)?;

        match event::read().context("failed to read pager input")? {
            Event::Key(key) => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Up | KeyCode::PageUp => state.page_up(),
                KeyCode::Down | KeyCode::PageDown | KeyCode::Char(' ') => state.page_down(),
                KeyCode::Home | KeyCode::Char('g') => state.to_top(),
                KeyCode::End | KeyCode::Char('G') => state.to_bottom(),
                _ => {}
            },
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

fn draw(stdout: &mut io::Stdout, state: &PagerState) -> Result<()> {
    let height = state.viewport_height();
    queue!(stdout, cursor::MoveTo(0, 0), Clear(ClearType::All))
        .context("failed to clear pager screen")?;

    for row in 0..height {
        queue!(stdout, cursor::MoveTo(0, row as u16)).context("failed to move cursor")?;
        if let Some(line) = state.line_at(row) {
            queue!(stdout, Print(clip_ansi_text(line, state.width))).context("failed to draw line")?;
        }
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

        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
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

#[derive(Debug)]
struct PagerState {
    lines: Vec<String>,
    offset: usize,
    width: usize,
    height: usize,
}

impl PagerState {
    fn new(output: &str) -> Self {
        let lines = output.lines().map(str::to_owned).collect();
        Self {
            lines,
            offset: 0,
            width: 0,
            height: 0,
        }
    }

    fn refresh_dimensions(&mut self) -> Result<()> {
        let (width, height) = terminal::size().context("failed to read terminal size")?;
        self.width = width as usize;
        self.height = height as usize;
        self.clamp_offset();
        Ok(())
    }

    fn viewport_height(&self) -> usize {
        self.height
    }

    fn line_at(&self, row: usize) -> Option<&str> {
        self.lines.get(self.offset + row).map(String::as_str)
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
}

#[cfg(test)]
mod tests {
    use super::PagerState;
    use super::clip_ansi_text;

    #[test]
    fn clip_ansi_text_preserves_escape_sequences() {
        let text = "\u{1b}[1mheader\u{1b}[0m";
        let clipped = clip_ansi_text(text, 3);
        assert!(clipped.contains("\u{1b}[1m"));
        assert!(clipped.contains("\u{1b}[0m"));
    }

    #[test]
    fn pager_page_movement_is_page_sized() {
        let mut state = PagerState::new("1\n2\n3\n4\n5\n6\n7\n8\n9\n10");
        state.height = 3;
        state.page_down();
        assert_eq!(state.offset, 3);
        state.page_up();
        assert_eq!(state.offset, 0);
    }

    #[test]
    fn pager_scroll_is_line_sized() {
        let mut state = PagerState::new("1\n2\n3\n4\n5");
        state.height = 2;
        state.scroll_down(3);
        assert_eq!(state.offset, 3);
        state.scroll_up(2);
        assert_eq!(state.offset, 1);
    }
}
