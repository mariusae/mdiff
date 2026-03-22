use crate::terminal_palette::AnsiColor;
use crate::terminal_palette::user_message_bg;
use crate::unified_diff::Document;
use crate::unified_diff::Hunk;
use crate::unified_diff::Item;
use crate::unified_diff::Row;
use crossterm::terminal;
use similar::ChangeTag;
use similar::TextDiff;
use std::io::IsTerminal;
use unicode_width::UnicodeWidthChar;

const THRESHOLD_WIDTH: usize = 120;
const PANE_GAP: &str = "  ";
const TAB_STOP: usize = 8;
const MIN_LINE_NUMBER_WIDTH: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderMode {
    pub side_by_side: bool,
    pub width: usize,
}

impl RenderMode {
    pub fn detect() -> Self {
        let stdout = std::io::stdout();
        if !stdout.is_terminal() {
            return Self {
                side_by_side: false,
                width: 0,
            };
        }

        let width = terminal::size()
            .map(|(cols, _)| cols as usize)
            .unwrap_or_default();

        Self {
            side_by_side: width > THRESHOLD_WIDTH,
            width,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TintPalette {
    pub changed_line_bg: Option<AnsiColor>,
}

impl TintPalette {
    pub fn detect() -> Self {
        Self {
            changed_line_bg: user_message_bg(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Segment {
    text: String,
    dim: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct StyledLine {
    segments: Vec<Segment>,
    background: Option<AnsiColor>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Layout {
    line_number_width: usize,
    left_text_width: usize,
    right_text_width: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RenderedRow {
    SideBySide {
        left_number: Option<usize>,
        left_text: String,
        right_number: Option<usize>,
        right_line: StyledLine,
    },
    FullWidth(String),
}

pub fn render_document(document: &Document, width: usize, palette: &TintPalette) -> String {
    let layout = layout_for(document, width);
    let mut output = String::new();
    let mut previous_was_file_header = false;

    for item in &document.items {
        match item {
            Item::FileHeader(path) => {
                if !output.is_empty() && !previous_was_file_header {
                    output.push('\n');
                }
                output.push_str(&render_file_header(path, width));
                output.push('\n');
                previous_was_file_header = true;
            }
            Item::Meta(line) => {
                output.push_str(&clip_plain_text(line, width));
                output.push('\n');
                previous_was_file_header = false;
            }
            Item::Hunk(hunk) => {
                output.push_str(&clip_plain_text(&hunk.header, width));
                output.push('\n');

                let mut old_line = hunk.old_start;
                let mut new_line = hunk.new_start;

                for row in &hunk.rows {
                    match render_row(row, &mut old_line, &mut new_line, palette) {
                        RenderedRow::SideBySide {
                            left_number,
                            left_text,
                            right_number,
                            right_line,
                        } => {
                            output.push_str(&render_plain_cell(left_number, &left_text, layout));
                            output.push_str(PANE_GAP);
                            output.push_str(&render_styled_cell(right_number, &right_line, layout));
                            output.push('\n');
                        }
                        RenderedRow::FullWidth(text) => {
                            output.push_str(&clip_plain_text(&text, width));
                            output.push('\n');
                        }
                    }
                }
                previous_was_file_header = false;
            }
        }
    }

    output
}

fn layout_for(document: &Document, width: usize) -> Layout {
    let max_line = document
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Hunk(hunk) => Some(max_hunk_line(hunk)),
            Item::FileHeader(_) | Item::Meta(_) => None,
        })
        .max()
        .unwrap_or(0);
    let line_number_width = MIN_LINE_NUMBER_WIDTH.max(digit_count(max_line));
    let reserved = line_number_width * 2 + 2 + PANE_GAP.len();
    let text_space = width.saturating_sub(reserved);
    let left_text_width = text_space / 2;
    let right_text_width = text_space.saturating_sub(left_text_width);

    Layout {
        line_number_width,
        left_text_width,
        right_text_width,
    }
}

fn max_hunk_line(hunk: &Hunk) -> usize {
    let mut old_line = hunk.old_start;
    let mut new_line = hunk.new_start;
    let mut max_line = old_line.max(new_line);

    for row in &hunk.rows {
        match row {
            Row::Context(_) => {
                max_line = max_line.max(old_line).max(new_line);
                old_line += 1;
                new_line += 1;
            }
            Row::Delete(_) => {
                max_line = max_line.max(old_line);
                old_line += 1;
            }
            Row::Insert(_) => {
                max_line = max_line.max(new_line);
                new_line += 1;
            }
            Row::Change { .. } => {
                max_line = max_line.max(old_line).max(new_line);
                old_line += 1;
                new_line += 1;
            }
            Row::Annotation(_) => {}
        }
    }

    max_line
}

fn render_file_header(path: &str, width: usize) -> String {
    let plain_width = display_width(path);
    let fill_width = width.saturating_sub(plain_width + 1);
    let fill = "-".repeat(fill_width);
    format!("\u{1b}[1m{path}\u{1b}[0m {fill}")
}

fn render_row(
    row: &Row,
    old_line: &mut usize,
    new_line: &mut usize,
    palette: &TintPalette,
) -> RenderedRow {
    match row {
        Row::Context(text) => {
            let line = expand_tabs(text);
            let left_number = *old_line;
            let right_number = *new_line;
            *old_line += 1;
            *new_line += 1;

            RenderedRow::SideBySide {
                left_number: Some(left_number),
                left_text: line.clone(),
                right_number: Some(right_number),
                right_line: StyledLine {
                    segments: vec![Segment {
                        text: line,
                        dim: true,
                    }],
                    background: None,
                },
            }
        }
        Row::Delete(text) => {
            let left_number = *old_line;
            *old_line += 1;

            RenderedRow::SideBySide {
                left_number: Some(left_number),
                left_text: expand_tabs(text),
                right_number: None,
                right_line: StyledLine::default(),
            }
        }
        Row::Insert(text) => {
            let right_number = *new_line;
            *new_line += 1;

            RenderedRow::SideBySide {
                left_number: None,
                left_text: String::new(),
                right_number: Some(right_number),
                right_line: StyledLine {
                    segments: vec![Segment {
                        text: expand_tabs(text),
                        dim: false,
                    }],
                    background: palette.changed_line_bg,
                },
            }
        }
        Row::Change { old, new } => {
            let left_number = *old_line;
            let right_number = *new_line;
            *old_line += 1;
            *new_line += 1;

            let old_text = expand_tabs(old);
            let new_text = expand_tabs(new);

            RenderedRow::SideBySide {
                left_number: Some(left_number),
                left_text: old_text.clone(),
                right_number: Some(right_number),
                right_line: StyledLine {
                    segments: diff_segments(&old_text, &new_text),
                    background: palette.changed_line_bg,
                },
            }
        }
        Row::Annotation(text) => RenderedRow::FullWidth(text.clone()),
    }
}

fn diff_segments(old: &str, new: &str) -> Vec<Segment> {
    let diff = TextDiff::from_chars(old, new);
    let mut segments = Vec::new();

    for change in diff.iter_all_changes() {
        let value = change.value().to_string();
        match change.tag() {
            ChangeTag::Equal => push_segment(&mut segments, value, true),
            ChangeTag::Insert => push_segment(&mut segments, value, false),
            ChangeTag::Delete => {}
        }
    }

    if segments.is_empty() {
        segments.push(Segment {
            text: new.to_owned(),
            dim: false,
        });
    }

    segments
}

fn push_segment(segments: &mut Vec<Segment>, text: String, dim: bool) {
    if text.is_empty() {
        return;
    }

    if let Some(last) = segments.last_mut()
        && last.dim == dim
    {
        last.text.push_str(&text);
        return;
    }

    segments.push(Segment { text, dim });
}

fn render_plain_cell(number: Option<usize>, text: &str, layout: Layout) -> String {
    let prefix = render_line_number(number, layout.line_number_width);
    let clipped = clip_plain_text(text, layout.left_text_width);
    let pad = layout.left_text_width.saturating_sub(display_width(&clipped));
    format!("{prefix}{clipped}{}", " ".repeat(pad))
}

fn render_styled_cell(number: Option<usize>, line: &StyledLine, layout: Layout) -> String {
    let mut output = String::new();
    let prefix = render_line_number(number, layout.line_number_width);
    let prefix_style = ansi_style(line.background, line.background.is_none());
    if !prefix_style.is_empty() {
        output.push_str(&prefix_style);
        output.push_str(&prefix);
        output.push_str("\u{1b}[0m");
    } else {
        output.push_str(&prefix);
    }

    let mut used = 0usize;
    for segment in &line.segments {
        if used >= layout.right_text_width {
            break;
        }

        let visible = clip_plain_text(&segment.text, layout.right_text_width - used);
        if visible.is_empty() {
            continue;
        }

        let style = ansi_style(line.background, segment.dim);
        if !style.is_empty() {
            output.push_str(&style);
        }
        output.push_str(&visible);
        if !style.is_empty() {
            output.push_str("\u{1b}[0m");
        }

        used += display_width(&visible);
    }

    if used < layout.right_text_width {
        if let Some(color) = line.background {
            output.push_str(&ansi_style(Some(color), false));
            output.push_str(&" ".repeat(layout.right_text_width - used));
            output.push_str("\u{1b}[0m");
        } else {
            output.push_str(&" ".repeat(layout.right_text_width - used));
        }
    }

    output
}

fn render_line_number(number: Option<usize>, width: usize) -> String {
    let content = match number {
        Some(number) => format!("{number:>width$}", width = width),
        None => " ".repeat(width),
    };
    format!("{content} ")
}

fn ansi_style(background: Option<AnsiColor>, dim: bool) -> String {
    let mut codes = Vec::new();
    if dim {
        codes.push("2".to_owned());
    }

    if let Some(color) = background {
        match color {
            AnsiColor::Indexed(index) => codes.push(format!("48;5;{index}")),
            AnsiColor::Rgb(r, g, b) => codes.push(format!("48;2;{r};{g};{b}")),
        }
    }

    if codes.is_empty() {
        String::new()
    } else {
        format!("\u{1b}[{}m", codes.join(";"))
    }
}

fn expand_tabs(text: &str) -> String {
    let mut rendered = String::new();
    let mut column = 0usize;

    for ch in text.chars() {
        if ch == '\t' {
            let spaces = TAB_STOP - (column % TAB_STOP);
            rendered.push_str(&" ".repeat(spaces));
            column += spaces;
            continue;
        }

        rendered.push(ch);
        column += char_width(ch);
    }

    rendered
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
    let mut width = 0usize;
    for ch in text.chars() {
        width += char_width(ch);
    }
    width
}

fn digit_count(value: usize) -> usize {
    value.max(1).to_string().len()
}

fn char_width(ch: char) -> usize {
    if ch == '\t' {
        TAB_STOP
    } else {
        UnicodeWidthChar::width(ch).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::RenderMode;
    use super::Segment;
    use super::StyledLine;
    use super::TintPalette;
    use super::display_width;
    use super::diff_segments;
    use super::expand_tabs;
    use super::render_document;
    use crate::terminal_palette::AnsiColor;
    use crate::unified_diff::Document;
    use crate::unified_diff::Hunk;
    use crate::unified_diff::Item;
    use crate::unified_diff::Row;

    #[test]
    fn expands_tabs_from_column_zero() {
        assert_eq!(expand_tabs("\tX"), "        X");
    }

    #[test]
    fn display_width_matches_ascii_length() {
        assert_eq!(display_width("abcdef"), 6);
    }

    #[test]
    fn renders_changed_rows_with_background_escape() {
        let document = Document {
            items: vec![
                Item::FileHeader("demo.txt".into()),
                Item::Hunk(Hunk {
                    header: "@@ -1 +1 @@".into(),
                    old_start: 1,
                    new_start: 1,
                    rows: vec![Row::Change {
                        old: "cat".into(),
                        new: "cot".into(),
                    }],
                }),
            ],
        };
        let palette = TintPalette {
            changed_line_bg: Some(AnsiColor::Indexed(240)),
        };

        let rendered = render_document(&document, 140, &palette);
        assert!(rendered.contains("\u{1b}[1mdemo.txt\u{1b}[0m"));
        assert!(rendered.contains("\u{1b}[48;5;240m"));
        assert!(rendered.contains("\u{1b}[2;48;5;240m"));
        assert!(!rendered.contains(" | "));
        assert!(!rendered.contains("+ cot"));
    }

    #[test]
    fn render_mode_threshold_is_strictly_greater_than_120() {
        let mode = RenderMode {
            side_by_side: 121 > super::THRESHOLD_WIDTH,
            width: 121,
        };
        assert!(mode.side_by_side);
    }

    #[test]
    fn styled_line_tracks_segments() {
        let line = StyledLine {
            segments: vec![Segment {
                text: "abc".into(),
                dim: true,
            }],
            background: None,
        };

        assert_eq!(line.segments.len(), 1);
    }

    #[test]
    fn merges_adjacent_diff_runs() {
        let segments = diff_segments("    if render_mode.side_by_side {", "    let rendered = if render_mode.side_by_side {");
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0].text, "    ");
        assert!(segments[0].dim);
        assert_eq!(segments[1].text, "let rendered = ");
        assert!(!segments[1].dim);
        assert_eq!(segments[2].text, "if render_mode.side_by_side {");
        assert!(segments[2].dim);
    }
}
