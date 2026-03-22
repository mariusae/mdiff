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
            side_by_side: should_render_side_by_side(width),
            width,
        }
    }
}

pub fn should_render_side_by_side(width: usize) -> bool {
    width > THRESHOLD_WIDTH
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
    center_number_width: usize,
    left_text_width: usize,
    right_text_width: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RenderedRow {
    SideBySide {
        left_text: String,
        center_number: String,
        right_line: StyledLine,
    },
    FullWidth(String),
}

pub fn render_document(document: &Document, width: usize, palette: &TintPalette) -> String {
    let layout = layout_for(document, width);
    let mut output = String::new();
    let mut index = 0usize;

    while let Some(item) = document.items.get(index) {
        match item {
            Item::FileHeader(path) => {
                if !output.is_empty() {
                    output.push('\n');
                }
                output.push_str(&render_file_header(path));
                output.push('\n');

                let section_end = next_file_header_index(&document.items, index + 1);
                render_file_section(
                    &document.items[index + 1..section_end],
                    width,
                    layout,
                    palette,
                    &mut output,
                );
                index = section_end;
            }
            Item::Meta(line) => {
                output.push_str(&clip_plain_text(line, width));
                output.push('\n');
                index += 1;
            }
            Item::Hunk(_) => {
                let section_end = next_file_header_index(&document.items, index);
                render_file_section(
                    &document.items[index..section_end],
                    width,
                    layout,
                    palette,
                    &mut output,
                );
                index = section_end;
            }
        }
    }

    output
}

pub fn render_inline_document(document: &Document, width: usize, palette: &TintPalette) -> String {
    let line_number_width = inline_line_number_width(document);
    let mut output = String::new();
    let mut index = 0usize;

    while let Some(item) = document.items.get(index) {
        match item {
            Item::FileHeader(path) => {
                if !output.is_empty() {
                    output.push('\n');
                }
                output.push_str(&render_file_header(path));
                output.push('\n');

                let section_end = next_file_header_index(&document.items, index + 1);
                render_inline_file_section(
                    &document.items[index + 1..section_end],
                    width,
                    line_number_width,
                    palette,
                    &mut output,
                );
                index = section_end;
            }
            Item::Meta(_) => {
                index += 1;
            }
            Item::Hunk(_) => {
                let section_end = next_file_header_index(&document.items, index);
                render_inline_file_section(
                    &document.items[index..section_end],
                    width,
                    line_number_width,
                    palette,
                    &mut output,
                );
                index = section_end;
            }
        }
    }

    output
}

fn render_inline_file_section(
    items: &[Item],
    width: usize,
    line_number_width: usize,
    palette: &TintPalette,
    output: &mut String,
) {
    let hunk_positions: Vec<usize> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| match item {
            Item::Hunk(_) => Some(index),
            Item::FileHeader(_) | Item::Meta(_) => None,
        })
        .collect();

    for (item_index, item) in items.iter().enumerate() {
        match item {
            Item::Hunk(hunk) => {
                let hunk_index = hunk_positions
                    .iter()
                    .position(|position| *position == item_index)
                    .unwrap_or(0);
                let elided = omitted_lines_above(items, &hunk_positions, hunk_index, hunk);
                if elided > 0 {
                    output.push_str(&render_inline_ellipsis(line_number_width, elided));
                    output.push('\n');
                }

                let mut old_line = hunk.old_start;
                let mut new_line = hunk.new_start;

                for row in &hunk.rows {
                    match row {
                        Row::Context(text) => {
                            let line_number = new_line;
                            old_line += 1;
                            new_line += 1;
                            output.push_str(&render_inline_context_line(
                                line_number,
                                text,
                                line_number_width,
                            ));
                            output.push('\n');
                        }
                        Row::Delete(text) => {
                            let line_number = old_line;
                            old_line += 1;
                            output.push_str(&render_inline_deleted_line(
                                line_number,
                                text,
                                line_number_width,
                            ));
                            output.push('\n');
                        }
                        Row::Insert(text) => {
                            let line_number = new_line;
                            new_line += 1;
                            output.push_str(&render_inline_inserted_line(
                                line_number,
                                text,
                                width,
                                line_number_width,
                                palette,
                            ));
                            output.push('\n');
                        }
                        Row::Change { old, new } => {
                            let old_number = old_line;
                            let new_number = new_line;
                            old_line += 1;
                            new_line += 1;

                            output.push_str(&render_inline_deleted_line(
                                old_number,
                                old,
                                line_number_width,
                            ));
                            output.push('\n');
                            output.push_str(&render_inline_changed_line(
                                new_number,
                                old,
                                new,
                                width,
                                line_number_width,
                                palette,
                            ));
                            output.push('\n');
                        }
                        Row::Annotation(text) => {
                            output.push_str(text);
                            output.push('\n');
                        }
                    }
                }
            }
            Item::Meta(_) | Item::FileHeader(_) => {}
        }
    }
}

fn render_file_section(
    items: &[Item],
    width: usize,
    layout: Layout,
    palette: &TintPalette,
    output: &mut String,
) {
    let hunk_positions: Vec<usize> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| match item {
            Item::Hunk(_) => Some(index),
            Item::FileHeader(_) | Item::Meta(_) => None,
        })
        .collect();

    for (item_index, item) in items.iter().enumerate() {
        match item {
            Item::Meta(line) => {
                output.push_str(&clip_plain_text(line, width));
                output.push('\n');
            }
            Item::Hunk(hunk) => {
                let hunk_index = hunk_positions
                    .iter()
                    .position(|position| *position == item_index)
                    .unwrap_or(0);
                let elided = omitted_lines_above(items, &hunk_positions, hunk_index, hunk);
                let mut show_gap_continuation = false;
                if elided > 0 {
                    output.push_str(&render_compact_elision_row(layout));
                    output.push('\n');
                    show_gap_continuation = true;
                }

                let mut old_line = hunk.old_start;
                let mut new_line = hunk.new_start;

                for row in &hunk.rows {
                    match render_row(row, &mut old_line, &mut new_line, palette) {
                        RenderedRow::SideBySide {
                            mut left_text,
                            center_number,
                            right_line,
                        } => {
                            if show_gap_continuation && left_text.is_empty() {
                                left_text = "..".to_owned();
                            }
                            show_gap_continuation = false;
                            output.push_str(&render_plain_cell(&left_text, layout.left_text_width));
                            output.push_str(PANE_GAP);
                            output.push_str(&render_center_number(
                                &center_number,
                                layout.center_number_width,
                            ));
                            output.push_str(PANE_GAP);
                            output.push_str(&render_styled_cell(
                                &right_line,
                                layout.right_text_width,
                            ));
                            output.push('\n');
                        }
                        RenderedRow::FullWidth(text) => {
                            output.push_str(&clip_plain_text(&text, width));
                            output.push('\n');
                        }
                    }
                }
            }
            Item::FileHeader(_) => {}
        }
    }
}

fn layout_for(document: &Document, width: usize) -> Layout {
    let max_right_line = document
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Hunk(hunk) => Some(max_hunk_new_line(hunk)),
            Item::FileHeader(_) | Item::Meta(_) => None,
        })
        .max()
        .unwrap_or(0);
    let center_number_width = MIN_LINE_NUMBER_WIDTH.max(digit_count(max_right_line));
    let reserved = center_number_width + PANE_GAP.len() * 2;
    let text_space = width.saturating_sub(reserved);
    let left_text_width = text_space / 2;
    let right_text_width = text_space.saturating_sub(left_text_width);

    Layout {
        center_number_width,
        left_text_width,
        right_text_width,
    }
}

fn inline_line_number_width(document: &Document) -> usize {
    let max_line = document
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Hunk(hunk) => Some(max_inline_hunk_line(hunk)),
            Item::FileHeader(_) | Item::Meta(_) => None,
        })
        .max()
        .unwrap_or(0);
    MIN_LINE_NUMBER_WIDTH.max(digit_count(max_line))
}

fn max_inline_hunk_line(hunk: &Hunk) -> usize {
    let mut old_line = hunk.old_start;
    let mut new_line = hunk.new_start;
    let mut max_line = old_line.max(new_line);

    for row in &hunk.rows {
        match row {
            Row::Context(_) => {
                max_line = max_line.max(new_line);
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

fn max_hunk_new_line(hunk: &Hunk) -> usize {
    let mut new_line = hunk.new_start;
    let mut max_line = new_line;

    for row in &hunk.rows {
        match row {
            Row::Context(_) => {
                max_line = max_line.max(new_line);
                new_line += 1;
            }
            Row::Delete(_) => {}
            Row::Insert(_) => {
                max_line = max_line.max(new_line);
                new_line += 1;
            }
            Row::Change { .. } => {
                max_line = max_line.max(new_line);
                new_line += 1;
            }
            Row::Annotation(_) => {}
        }
    }

    max_line
}

fn next_file_header_index(items: &[Item], start: usize) -> usize {
    items[start..]
        .iter()
        .position(|item| matches!(item, Item::FileHeader(_)))
        .map(|offset| start + offset)
        .unwrap_or(items.len())
}

fn omitted_lines_above(
    items: &[Item],
    hunk_positions: &[usize],
    hunk_index: usize,
    hunk: &Hunk,
) -> usize {
    if hunk_index == 0 {
        hunk.new_start.saturating_sub(1)
    } else {
        let previous_hunk = match &items[hunk_positions[hunk_index - 1]] {
            Item::Hunk(previous) => previous,
            Item::FileHeader(_) | Item::Meta(_) => unreachable!(),
        };
        hunk.new_start.saturating_sub(hunk_end(previous_hunk) + 1)
    }
}

fn hunk_end(hunk: &Hunk) -> usize {
    if hunk.new_len == 0 {
        hunk.new_start.saturating_sub(1)
    } else {
        hunk.new_start + hunk.new_len - 1
    }
}

fn render_file_header(path: &str) -> String {
    format!("\u{1b}[1m{path}\u{1b}[0m")
}

fn render_compact_elision_row(layout: Layout) -> String {
    let mut output = String::new();
    output.push_str(&render_plain_cell("", layout.left_text_width));
    output.push_str(PANE_GAP);
    output.push_str(&render_elided_marker_cell(layout.center_number_width));
    output.push_str(PANE_GAP);
    output.push_str(&" ".repeat(layout.right_text_width));
    output
}

fn render_inline_ellipsis(line_number_width: usize, _elided: usize) -> String {
    format!("{:>line_number_width$}", "⋮")
}

fn render_inline_context_line(line_number: usize, text: &str, line_number_width: usize) -> String {
    let prefix = format!("{line_number:>line_number_width$}  ");
    format!("{prefix}\u{1b}[2m{}\u{1b}[0m", expand_tabs(text))
}

fn render_inline_deleted_line(line_number: usize, text: &str, line_number_width: usize) -> String {
    let prefix = format!("{line_number:>line_number_width$} -");
    format!("{prefix}{}", expand_tabs(text))
}

fn render_inline_inserted_line(
    line_number: usize,
    text: &str,
    width: usize,
    line_number_width: usize,
    palette: &TintPalette,
) -> String {
    let prefix = format!("{line_number:>line_number_width$} +");
    render_inline_styled_line(
        prefix,
        StyledLine {
            segments: vec![Segment {
                text: expand_tabs(text),
                dim: false,
            }],
            background: palette.changed_line_bg,
        },
        width,
    )
}

fn render_inline_changed_line(
    line_number: usize,
    old: &str,
    new: &str,
    width: usize,
    line_number_width: usize,
    palette: &TintPalette,
) -> String {
    let prefix = format!("{line_number:>line_number_width$} +");
    render_inline_styled_line(
        prefix,
        StyledLine {
            segments: diff_segments(&expand_tabs(old), &expand_tabs(new)),
            background: palette.changed_line_bg,
        },
        width,
    )
}

fn render_inline_styled_line(prefix: String, line: StyledLine, width: usize) -> String {
    let mut output = prefix;
    let available_width = inline_available_width(width, &output);
    let mut used = 0usize;

    for segment in line.segments {
        if used >= available_width {
            break;
        }

        let visible = if width == 0 {
            segment.text
        } else {
            clip_plain_text(&segment.text, available_width - used)
        };
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

    if width > 0
        && let Some(color) = line.background
        && used < available_width
    {
        output.push_str(&ansi_style(Some(color), false));
        output.push_str(&" ".repeat(available_width - used));
        output.push_str("\u{1b}[0m");
    }
    output
}

fn inline_available_width(width: usize, prefix: &str) -> usize {
    if width == 0 {
        usize::MAX
    } else {
        width.saturating_sub(display_width(prefix))
    }
}

fn render_elided_marker_cell(width: usize) -> String {
    let left_pad = width.saturating_sub(1) / 2;
    let right_pad = width.saturating_sub(1 + left_pad);
    format!("{}⋮{}", " ".repeat(left_pad), " ".repeat(right_pad))
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
            let right_number = *new_line;
            *old_line += 1;
            *new_line += 1;

            RenderedRow::SideBySide {
                left_text: line.clone(),
                center_number: format_right_line_number(Some(right_number)),
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
            *old_line += 1;

            RenderedRow::SideBySide {
                left_text: expand_tabs(text),
                center_number: format_right_line_number(None),
                right_line: StyledLine::default(),
            }
        }
        Row::Insert(text) => {
            let right_number = *new_line;
            *new_line += 1;

            RenderedRow::SideBySide {
                left_text: String::new(),
                center_number: format_right_line_number(Some(right_number)),
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
            let right_number = *new_line;
            *old_line += 1;
            *new_line += 1;

            let old_text = expand_tabs(old);
            let new_text = expand_tabs(new);

            RenderedRow::SideBySide {
                left_text: old_text.clone(),
                center_number: format_right_line_number(Some(right_number)),
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

fn render_plain_cell(text: &str, width: usize) -> String {
    let clipped = clip_plain_text(text, width);
    let pad = width.saturating_sub(display_width(&clipped));
    format!("{clipped}{}", " ".repeat(pad))
}

fn render_center_number(label: &str, width: usize) -> String {
    let clipped = clip_plain_text(label, width);
    let clipped_width = display_width(&clipped);
    let left_pad = width.saturating_sub(clipped_width) / 2;
    let right_pad = width.saturating_sub(clipped_width + left_pad);
    format!(
        "\u{1b}[1m{}{}{}\u{1b}[0m",
        " ".repeat(left_pad),
        clipped,
        " ".repeat(right_pad)
    )
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

fn digit_count(value: usize) -> usize {
    value.max(1).to_string().len()
}

fn format_right_line_number(right: Option<usize>) -> String {
    right.map(|line| line.to_string()).unwrap_or_default()
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
    use super::Layout;
    use super::RenderMode;
    use super::Segment;
    use super::StyledLine;
    use super::TintPalette;
    use super::diff_segments;
    use super::display_width;
    use super::expand_tabs;
    use super::format_right_line_number;
    use super::render_compact_elision_row;
    use super::render_document;
    use super::render_inline_document;
    use super::render_inline_inserted_line;
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
                    old_start: 3,
                    new_start: 3,
                    new_len: 1,
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
        assert!(rendered.contains("⋮"));
        assert!(!rendered.contains("@@ -1 +1 @@"));
        assert!(rendered.contains("\u{1b}[48;5;240m"));
        assert!(rendered.contains("\u{1b}[2;48;5;240m"));
        assert!(rendered.contains("\u{1b}[1m 3  \u{1b}[0m"));
        assert!(!rendered.contains(" | "));
        assert!(!rendered.contains("+ cot"));
    }

    #[test]
    fn renders_inline_document_with_markers_and_ellipsis() {
        let document = Document {
            items: vec![
                Item::FileHeader("src/unified_diff.rs".into()),
                Item::Hunk(Hunk {
                    old_start: 15,
                    new_start: 15,
                    new_len: 3,
                    rows: vec![
                        Row::Context("    pub old_start: usize,".into()),
                        Row::Delete("    pub old_len: usize,".into()),
                        Row::Context("    pub new_start: usize,".into()),
                    ],
                }),
                Item::Hunk(Hunk {
                    old_start: 127,
                    new_start: 126,
                    new_len: 3,
                    rows: vec![
                        Row::Change {
                            old: "    let (old_start, old_len, new_start, new_len) = parse_hunk_header(&header).unwrap_or((0, 0, 0, 0));".into(),
                            new: "    let (old_start, new_start, new_len) = parse_hunk_header(&header).unwrap_or((0, 0, 0));".into(),
                        },
                        Row::Context("    let rows = build_rows(std::mem::take(raw_rows));".into()),
                    ],
                }),
            ],
        };
        let palette = TintPalette {
            changed_line_bg: Some(AnsiColor::Indexed(240)),
        };

        let rendered = render_inline_document(&document, 80, &palette);
        assert!(rendered.contains("\u{1b}[1msrc/unified_diff.rs\u{1b}[0m"));
        assert!(rendered.contains("   ⋮"));
        assert!(rendered.contains("  15  \u{1b}[2m    pub old_start: usize,"));
        assert!(rendered.contains("  16 -    pub old_len: usize,"));
        assert!(rendered.contains("  16  \u{1b}[2m    pub new_start: usize,"));
        assert!(rendered.contains(" 127 -    let (old_start, old_len, new_start, new_len)"));
        assert!(rendered.contains(" 126 +"));
        assert!(rendered.contains("\u{1b}[48;5;240m") || rendered.contains("\u{1b}[2;48;5;240m"));
    }

    #[test]
    fn renders_gap_continuation_marker_before_first_insert_after_elision() {
        let document = Document {
            items: vec![
                Item::FileHeader("demo.txt".into()),
                Item::Hunk(Hunk {
                    old_start: 1,
                    new_start: 1,
                    new_len: 1,
                    rows: vec![Row::Context("alpha".into())],
                }),
                Item::Hunk(Hunk {
                    old_start: 10,
                    new_start: 10,
                    new_len: 1,
                    rows: vec![Row::Insert("beta".into())],
                }),
            ],
        };
        let palette = TintPalette {
            changed_line_bg: Some(AnsiColor::Indexed(240)),
        };

        let rendered = render_document(&document, 140, &palette);
        assert!(rendered.contains(" ⋮ "));
        assert!(rendered.contains(".."));
        assert!(rendered.contains("\u{1b}[1m 10 \u{1b}[0m"));
    }

    #[test]
    fn inline_tint_extends_to_terminal_edge() {
        let palette = TintPalette {
            changed_line_bg: Some(AnsiColor::Indexed(240)),
        };

        let line = render_inline_inserted_line(12, "abc", 20, 4, &palette);
        assert!(line.starts_with("  12 +"));
        assert!(line.contains("\u{1b}[48;5;240mabc"));
        assert!(line.ends_with("       \u{1b}[0m"));
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
        let segments = diff_segments(
            "    if render_mode.side_by_side {",
            "    let rendered = if render_mode.side_by_side {",
        );
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0].text, "    ");
        assert!(segments[0].dim);
        assert_eq!(segments[1].text, "let rendered = ");
        assert!(!segments[1].dim);
        assert_eq!(segments[2].text, "if render_mode.side_by_side {");
        assert!(segments[2].dim);
    }

    #[test]
    fn formats_right_line_numbers_only() {
        assert_eq!(format_right_line_number(Some(12)), "12");
        assert_eq!(format_right_line_number(None), "");
    }

    #[test]
    fn renders_centered_italic_chunk_header() {
        let header = render_compact_elision_row(Layout {
            center_number_width: 4,
            left_text_width: 6,
            right_text_width: 6,
        });
        assert!(header.contains(" ⋮  "));
    }
}
