use crate::terminal_palette::AnsiColor;
use crate::terminal_palette::user_message_bg;
use crate::unified_diff::Document;
use crate::unified_diff::Item;
use crate::unified_diff::Row;
use crossterm::terminal;
use similar::ChangeTag;
use similar::TextDiff;
use std::io::IsTerminal;
use unicode_width::UnicodeWidthChar;

const THRESHOLD_WIDTH: usize = 120;
const GUTTER: &str = " | ";
const TAB_STOP: usize = 8;

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

pub fn render_document(document: &Document, width: usize, palette: &TintPalette) -> String {
    if width <= GUTTER.len() {
        return String::new();
    }

    let left_width = (width.saturating_sub(GUTTER.len())) / 2;
    let right_width = width.saturating_sub(GUTTER.len() + left_width);
    let mut output = String::new();

    for item in &document.items {
        match item {
            Item::Meta(line) => {
                output.push_str(&clip_plain_text(line, width));
                output.push('\n');
            }
            Item::Hunk(hunk) => {
                output.push_str(&clip_plain_text(&hunk.header, width));
                output.push('\n');

                for row in &hunk.rows {
                    if let Row::Annotation(text) = row {
                        output.push_str(&clip_plain_text(text, width));
                        output.push('\n');
                        continue;
                    }

                    let (left, right) = render_row(row, palette);
                    output.push_str(&render_plain_cell(&left, left_width));
                    output.push_str(GUTTER);
                    output.push_str(&render_styled_cell(&right, right_width));
                    output.push('\n');
                }
            }
        }
    }

    output
}

fn render_row(row: &Row, palette: &TintPalette) -> (String, StyledLine) {
    match row {
        Row::Context(text) => {
            let text = expand_tabs(text);
            (
                text.clone(),
                StyledLine {
                    segments: vec![Segment { text, dim: true }],
                    background: None,
                },
            )
        }
        Row::Delete(text) => (expand_tabs(text), StyledLine::default()),
        Row::Insert(text) => {
            let text = expand_tabs(text);
            (
                String::new(),
                StyledLine {
                    segments: vec![Segment { text, dim: false }],
                    background: palette.changed_line_bg,
                },
            )
        }
        Row::Change { old, new } => {
            let old_text = expand_tabs(old);
            let new_text = expand_tabs(new);
            let segments = diff_segments(&old_text, &new_text);

            (
                old_text,
                StyledLine {
                    segments,
                    background: palette.changed_line_bg,
                },
            )
        }
        Row::Annotation(_) => (String::new(), StyledLine::default()),
    }
}

fn diff_segments(old: &str, new: &str) -> Vec<Segment> {
    let diff = TextDiff::from_chars(old, new);
    let mut segments = Vec::new();

    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => segments.push(Segment {
                text: change.to_string(),
                dim: true,
            }),
            ChangeTag::Insert => segments.push(Segment {
                text: change.to_string(),
                dim: false,
            }),
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

fn render_plain_cell(text: &str, width: usize) -> String {
    let clipped = clip_plain_text(text, width);
    let pad = width.saturating_sub(display_width(&clipped));
    format!("{clipped}{}", " ".repeat(pad))
}

fn render_styled_cell(line: &StyledLine, width: usize) -> String {
    let mut output = String::new();
    let mut used = 0usize;

    for segment in &line.segments {
        if used >= width {
            break;
        }

        let visible = clip_plain_text(&segment.text, width - used);
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

    if used < width {
        if let Some(color) = line.background {
            output.push_str(&ansi_style(Some(color), false));
            output.push_str(&" ".repeat(width - used));
            output.push_str("\u{1b}[0m");
        } else {
            output.push_str(&" ".repeat(width - used));
        }
    }

    output
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
            items: vec![Item::Hunk(Hunk {
                header: "@@ -1 +1 @@".into(),
                rows: vec![Row::Change {
                    old: "cat".into(),
                    new: "cot".into(),
                }],
            })],
        };
        let palette = TintPalette {
            changed_line_bg: Some(AnsiColor::Indexed(240)),
        };

        let rendered = render_document(&document, 140, &palette);
        assert!(rendered.contains("\u{1b}[48;5;240m"));
        assert!(rendered.contains("\u{1b}[2;48;5;240m"));
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
}
