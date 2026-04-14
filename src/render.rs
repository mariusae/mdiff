use crate::terminal_palette::AnsiColor;
use crate::terminal_palette::tint_and_gutter_colors;
use crate::unified_diff::Document;
use crate::unified_diff::Hunk;
use crate::unified_diff::Item;
use crate::unified_diff::Row;
use crossterm::terminal;
use std::collections::HashMap;
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
pub struct PaneLayout {
    pub left_end: usize,
    pub right_start: usize,
    pub content_start: usize,
}

pub fn pane_layout(document: &Document, width: usize) -> PaneLayout {
    if should_render_side_by_side(width) {
        let layout = layout_for(document, width);
        PaneLayout {
            left_end: layout.left_text_width,
            right_start: layout.left_text_width
                + PANE_GAP.len()
                + layout.center_number_width
                + PANE_GAP.len(),
            content_start: 0,
        }
    } else {
        PaneLayout {
            left_end: 0,
            right_start: 0,
            content_start: inline_line_number_width(document) + 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TintPalette {
    pub changed_line_bg: Option<AnsiColor>,
    pub gutter_fg: Option<AnsiColor>,
}

impl TintPalette {
    pub fn detect() -> Self {
        let (changed_line_bg, gutter_fg) = tint_and_gutter_colors();
        Self {
            changed_line_bg,
            gutter_fg,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct GapId {
    pub file_path: String,
    pub hunk_index: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GapDescriptor {
    pub id: GapId,
    pub start_line: usize,
    pub line_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GapState {
    CollapsedSelector,
    Loading(GapExpandRequest),
    Expanded(ExpandedGapState),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GapExpandRequest {
    Full,
    Above,
    Below,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpandedGapState {
    pub lines: Vec<String>,
    pub top_len: usize,
    pub bottom_len: usize,
    pub selector_open: bool,
}

impl ExpandedGapState {
    pub fn fully_expanded(lines: Vec<String>) -> Self {
        let len = lines.len();
        Self {
            lines,
            top_len: len,
            bottom_len: 0,
            selector_open: false,
        }
    }

    pub fn hidden_len(&self) -> usize {
        self.lines
            .len()
            .saturating_sub(self.top_len.saturating_add(self.bottom_len))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceLocation {
    pub file_path: String,
    pub left_line_number: Option<usize>,
    pub right_line_number: Option<usize>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RenderedLineMetadata {
    pub gap: Option<GapDescriptor>,
    pub source: Option<SourceLocation>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RenderedDocument {
    pub lines: Vec<String>,
    pub line_metadata: Vec<RenderedLineMetadata>,
    pub layout: PaneLayout,
}

impl RenderedDocument {
    fn new(layout: PaneLayout) -> Self {
        Self {
            lines: Vec::new(),
            line_metadata: Vec::new(),
            layout,
        }
    }

    fn push_line(
        &mut self,
        text: String,
        gap: Option<GapDescriptor>,
        source: Option<SourceLocation>,
    ) {
        self.lines.push(text);
        self.line_metadata
            .push(RenderedLineMetadata { gap, source });
    }

    pub fn into_output(self) -> String {
        self.lines.join("\n")
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct StyledLine {
    text: String,
    background: Option<AnsiColor>,
    dim: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Layout {
    center_number_width: usize,
    left_text_width: usize,
    right_visible_width: usize,
    right_render_width: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RenderedRow {
    SideBySide {
        left_text: String,
        left_background: Option<AnsiColor>,
        left_dim: bool,
        left_line_number: Option<usize>,
        center_number: String,
        right_line_number: Option<usize>,
        right_gutter: bool,
        right_line: StyledLine,
    },
    FullWidth(String),
}

pub fn render_document(document: &Document, width: usize, palette: &TintPalette) -> String {
    render_document_with_state(document, width, palette, &HashMap::new(), 0).into_output()
}

pub fn render_document_with_state(
    document: &Document,
    width: usize,
    palette: &TintPalette,
    gap_states: &HashMap<GapId, GapState>,
    spinner_frame: usize,
) -> RenderedDocument {
    render_document_with_state_and_file_counts(
        document,
        width,
        palette,
        &HashMap::new(),
        gap_states,
        spinner_frame,
    )
}

pub fn render_document_with_state_and_file_counts(
    document: &Document,
    width: usize,
    palette: &TintPalette,
    file_line_counts: &HashMap<String, usize>,
    gap_states: &HashMap<GapId, GapState>,
    spinner_frame: usize,
) -> RenderedDocument {
    let layout = layout_for(document, width);
    let mut output = RenderedDocument::new(pane_layout(document, width));
    let mut index = 0usize;

    while let Some(item) = document.items.get(index) {
        match item {
            Item::FileHeader(path) => {
                if !output.lines.is_empty() {
                    output.push_line(String::new(), None, None);
                }
                output.push_line(render_file_header(path), None, None);

                let section_end = next_file_header_index(&document.items, index + 1);
                render_file_section(
                    path,
                    &document.items[index + 1..section_end],
                    width,
                    layout,
                    palette,
                    file_line_counts,
                    gap_states,
                    spinner_frame,
                    &mut output,
                );
                index = section_end;
            }
            Item::Meta(line) => {
                output.push_line(clip_plain_text(line, width), None, None);
                index += 1;
            }
            Item::Hunk(_) => {
                let section_end = next_file_header_index(&document.items, index);
                render_file_section(
                    "",
                    &document.items[index..section_end],
                    width,
                    layout,
                    palette,
                    file_line_counts,
                    gap_states,
                    spinner_frame,
                    &mut output,
                );
                index = section_end;
            }
        }
    }

    output
}

pub fn render_inline_document(document: &Document, _width: usize, palette: &TintPalette) -> String {
    render_inline_document_with_state(document, palette, &HashMap::new(), 0).into_output()
}

pub fn render_inline_document_with_state(
    document: &Document,
    palette: &TintPalette,
    gap_states: &HashMap<GapId, GapState>,
    spinner_frame: usize,
) -> RenderedDocument {
    render_inline_document_with_state_and_file_counts(
        document,
        palette,
        &HashMap::new(),
        gap_states,
        spinner_frame,
    )
}

pub fn render_inline_document_with_state_and_file_counts(
    document: &Document,
    palette: &TintPalette,
    file_line_counts: &HashMap<String, usize>,
    gap_states: &HashMap<GapId, GapState>,
    spinner_frame: usize,
) -> RenderedDocument {
    let line_number_width = inline_line_number_width(document);
    let mut output = RenderedDocument::new(PaneLayout {
        left_end: 0,
        right_start: 0,
        content_start: line_number_width + 2,
    });
    let mut index = 0usize;

    while let Some(item) = document.items.get(index) {
        match item {
            Item::FileHeader(path) => {
                if !output.lines.is_empty() {
                    output.push_line(String::new(), None, None);
                }
                output.push_line(render_file_header(path), None, None);

                let section_end = next_file_header_index(&document.items, index + 1);
                render_inline_file_section(
                    path,
                    &document.items[index + 1..section_end],
                    line_number_width,
                    palette,
                    file_line_counts,
                    gap_states,
                    spinner_frame,
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
                    "",
                    &document.items[index..section_end],
                    line_number_width,
                    palette,
                    file_line_counts,
                    gap_states,
                    spinner_frame,
                    &mut output,
                );
                index = section_end;
            }
        }
    }

    output
}

fn render_inline_file_section(
    file_path: &str,
    items: &[Item],
    line_number_width: usize,
    palette: &TintPalette,
    file_line_counts: &HashMap<String, usize>,
    gap_states: &HashMap<GapId, GapState>,
    spinner_frame: usize,
    output: &mut RenderedDocument,
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
                if let Some((start_line, elided)) =
                    omitted_range_above(items, &hunk_positions, hunk_index, hunk)
                {
                    render_inline_gap(
                        GapDescriptor {
                            id: GapId {
                                file_path: file_path.to_owned(),
                                hunk_index,
                            },
                            start_line,
                            line_count: elided,
                        },
                        line_number_width,
                        gap_states,
                        spinner_frame,
                        output,
                    );
                }

                let mut old_line = hunk.old_start;
                let mut new_line = hunk.new_start;

                for row in &hunk.rows {
                    match row {
                        Row::Context(text) => {
                            let line_number = new_line;
                            old_line += 1;
                            new_line += 1;
                            output.push_line(
                                render_inline_context_line(line_number, text, line_number_width),
                                None,
                                source_location(file_path, Some(line_number), Some(line_number)),
                            );
                        }
                        Row::Delete(text) => {
                            let line_number = old_line;
                            old_line += 1;
                            output.push_line(
                                render_inline_deleted_line(
                                    line_number,
                                    text,
                                    line_number_width,
                                    palette,
                                ),
                                None,
                                source_location(file_path, Some(line_number), None),
                            );
                        }
                        Row::Insert(text) => {
                            let line_number = new_line;
                            new_line += 1;
                            output.push_line(
                                render_inline_inserted_line(
                                    line_number,
                                    text,
                                    line_number_width,
                                    palette,
                                ),
                                None,
                                source_location(file_path, None, Some(line_number)),
                            );
                        }
                        Row::Change { old, new } => {
                            let old_number = old_line;
                            let new_number = new_line;
                            old_line += 1;
                            new_line += 1;

                            output.push_line(
                                render_inline_deleted_line(
                                    old_number,
                                    old,
                                    line_number_width,
                                    palette,
                                ),
                                None,
                                source_location(file_path, Some(old_number), None),
                            );
                            output.push_line(
                                render_inline_changed_line(
                                    new_number,
                                    new,
                                    line_number_width,
                                    palette,
                                ),
                                None,
                                source_location(file_path, None, Some(new_number)),
                            );
                        }
                        Row::Annotation(text) => {
                            output.push_line(text.clone(), None, None);
                        }
                    }
                }

                if hunk_index + 1 == hunk_positions.len() {
                    if let Some((start_line, elided)) =
                        omitted_range_below(hunk, file_line_counts.get(file_path).copied())
                    {
                        render_inline_gap(
                            GapDescriptor {
                                id: GapId {
                                    file_path: file_path.to_owned(),
                                    hunk_index: hunk_positions.len(),
                                },
                                start_line,
                                line_count: elided,
                            },
                            line_number_width,
                            gap_states,
                            spinner_frame,
                            output,
                        );
                    }
                }
            }
            Item::Meta(_) | Item::FileHeader(_) => {}
        }
    }
}

fn render_file_section(
    file_path: &str,
    items: &[Item],
    width: usize,
    layout: Layout,
    palette: &TintPalette,
    file_line_counts: &HashMap<String, usize>,
    gap_states: &HashMap<GapId, GapState>,
    spinner_frame: usize,
    output: &mut RenderedDocument,
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
                output.push_line(clip_plain_text(line, width), None, None);
            }
            Item::Hunk(hunk) => {
                let hunk_index = hunk_positions
                    .iter()
                    .position(|position| *position == item_index)
                    .unwrap_or(0);
                let mut show_gap_continuation = false;
                if let Some((start_line, elided)) =
                    omitted_range_above(items, &hunk_positions, hunk_index, hunk)
                {
                    render_gap(
                        GapDescriptor {
                            id: GapId {
                                file_path: file_path.to_owned(),
                                hunk_index,
                            },
                            start_line,
                            line_count: elided,
                        },
                        layout,
                        palette,
                        gap_states,
                        spinner_frame,
                        output,
                    );
                    show_gap_continuation = true;
                }

                let mut old_line = hunk.old_start;
                let mut new_line = hunk.new_start;

                for row in &hunk.rows {
                    let rendered = render_row(row, &mut old_line, &mut new_line, palette);
                    push_rendered_row(
                        output,
                        width,
                        layout,
                        palette,
                        file_path,
                        rendered,
                        &mut show_gap_continuation,
                    );
                }

                if hunk_index + 1 == hunk_positions.len() {
                    if let Some((start_line, elided)) =
                        omitted_range_below(hunk, file_line_counts.get(file_path).copied())
                    {
                        render_gap(
                            GapDescriptor {
                                id: GapId {
                                    file_path: file_path.to_owned(),
                                    hunk_index: hunk_positions.len(),
                                },
                                start_line,
                                line_count: elided,
                            },
                            layout,
                            palette,
                            gap_states,
                            spinner_frame,
                            output,
                        );
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
    let right_visible_width = text_space.saturating_sub(left_text_width);
    let right_render_width = right_visible_width.max(max_right_render_width(document));

    Layout {
        center_number_width,
        left_text_width,
        right_visible_width,
        right_render_width,
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

fn max_right_render_width(document: &Document) -> usize {
    document
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Hunk(hunk) => Some(max_hunk_right_render_width(hunk)),
            Item::FileHeader(_) | Item::Meta(_) => None,
        })
        .max()
        .unwrap_or(0)
}

fn max_hunk_right_render_width(hunk: &Hunk) -> usize {
    hunk.rows
        .iter()
        .map(|row| match row {
            Row::Context(text) | Row::Insert(text) => display_width(&expand_tabs(text)),
            Row::Change { new, .. } => display_width(&expand_tabs(new)),
            Row::Delete(_) | Row::Annotation(_) => 0,
        })
        .max()
        .unwrap_or(0)
}

fn next_file_header_index(items: &[Item], start: usize) -> usize {
    items[start..]
        .iter()
        .position(|item| matches!(item, Item::FileHeader(_)))
        .map(|offset| start + offset)
        .unwrap_or(items.len())
}

fn omitted_range_above(
    items: &[Item],
    hunk_positions: &[usize],
    hunk_index: usize,
    hunk: &Hunk,
) -> Option<(usize, usize)> {
    let (start_line, line_count) = if hunk_index == 0 {
        (1, hunk.new_start.saturating_sub(1))
    } else {
        let previous_hunk = match &items[hunk_positions[hunk_index - 1]] {
            Item::Hunk(previous) => previous,
            Item::FileHeader(_) | Item::Meta(_) => unreachable!(),
        };
        let start_line = hunk_end(previous_hunk) + 1;
        let line_count = hunk.new_start.saturating_sub(start_line);
        (start_line, line_count)
    };

    if line_count == 0 {
        None
    } else {
        Some((start_line, line_count))
    }
}

fn omitted_range_below(hunk: &Hunk, total_lines: Option<usize>) -> Option<(usize, usize)> {
    let total_lines = total_lines?;
    let start_line = hunk_end(hunk) + 1;
    let line_count = total_lines.saturating_sub(start_line.saturating_sub(1));
    if line_count == 0 {
        None
    } else {
        Some((start_line, line_count))
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

fn source_location(
    file_path: &str,
    left_line_number: Option<usize>,
    right_line_number: Option<usize>,
) -> Option<SourceLocation> {
    if file_path.is_empty() || (left_line_number.is_none() && right_line_number.is_none()) {
        return None;
    }

    Some(SourceLocation {
        file_path: file_path.to_owned(),
        left_line_number,
        right_line_number,
    })
}

fn spinner_glyph(frame: usize) -> char {
    const FRAMES: &[char] = &['⠁', '⠂', '⠄', '⠂'];
    FRAMES[frame % FRAMES.len()]
}

fn render_compact_elision_row(layout: Layout) -> String {
    render_compact_marker_row(layout, "⋮")
}

fn render_compact_selector_row(layout: Layout) -> String {
    render_compact_marker_row(layout, "▴⋮▾")
}

fn render_compact_spinner_row(layout: Layout, frame: usize) -> String {
    let marker = spinner_glyph(frame).to_string();
    render_compact_marker_row(layout, &marker)
}

fn render_compact_marker_row(layout: Layout, marker: &str) -> String {
    let mut output = String::new();
    output.push_str(&render_plain_cell("", layout.left_text_width));
    output.push_str(PANE_GAP);
    output.push_str(&render_marker_cell(layout.center_number_width, marker));
    output.push_str(PANE_GAP);
    output.push_str(&" ".repeat(layout.right_visible_width));
    output
}

fn render_inline_ellipsis(line_number_width: usize, _elided: usize) -> String {
    render_inline_marker(line_number_width, "⋮")
}

fn render_inline_selector(line_number_width: usize) -> String {
    render_inline_marker(line_number_width, "▴⋮▾")
}

fn render_inline_spinner(line_number_width: usize, frame: usize) -> String {
    let marker = spinner_glyph(frame).to_string();
    render_inline_marker(line_number_width, &marker)
}

fn render_inline_marker(line_number_width: usize, marker: &str) -> String {
    format!("{marker:>line_number_width$}")
}

fn render_inline_expanded_gap_lines(
    file_path: &str,
    lines: &[String],
    start_line: usize,
    line_number_width: usize,
    output: &mut RenderedDocument,
) {
    for (offset, line) in lines.iter().enumerate() {
        output.push_line(
            render_inline_context_line(start_line + offset, line, line_number_width),
            None,
            source_location(
                file_path,
                Some(start_line + offset),
                Some(start_line + offset),
            ),
        );
    }
}

fn render_inline_gap_state(
    gap: &GapDescriptor,
    line_number_width: usize,
    output: &mut RenderedDocument,
    state: &ExpandedGapState,
) {
    render_inline_expanded_gap_lines(
        &gap.id.file_path,
        &state.lines[..state.top_len],
        gap.start_line,
        line_number_width,
        output,
    );

    if state.hidden_len() > 0 {
        let marker = if state.selector_open {
            render_inline_selector(line_number_width)
        } else {
            render_inline_ellipsis(line_number_width, state.hidden_len())
        };
        output.push_line(marker, Some(gap.clone()), None);
    }

    if state.bottom_len > 0 {
        let start_line = gap.start_line + state.lines.len().saturating_sub(state.bottom_len);
        render_inline_expanded_gap_lines(
            &gap.id.file_path,
            &state.lines[state.lines.len() - state.bottom_len..],
            start_line,
            line_number_width,
            output,
        );
    }
}

fn render_inline_gap(
    gap: GapDescriptor,
    line_number_width: usize,
    gap_states: &HashMap<GapId, GapState>,
    spinner_frame: usize,
    output: &mut RenderedDocument,
) {
    match gap_states.get(&gap.id) {
        Some(GapState::Expanded(state)) => {
            render_inline_gap_state(&gap, line_number_width, output, state);
        }
        Some(GapState::Loading(_)) => {
            output.push_line(
                render_inline_spinner(line_number_width, spinner_frame),
                Some(gap),
                None,
            );
        }
        Some(GapState::CollapsedSelector) => {
            output.push_line(
                render_inline_selector(line_number_width),
                Some(gap),
                None,
            );
        }
        None => {
            output.push_line(
                render_inline_ellipsis(line_number_width, gap.line_count),
                Some(gap),
                None,
            );
        }
    }
}

fn render_inline_context_line(line_number: usize, text: &str, line_number_width: usize) -> String {
    let prefix = format!("{line_number:>line_number_width$}  ");
    format!("{prefix}{}", expand_tabs(text))
}

fn render_inline_deleted_line(
    line_number: usize,
    text: &str,
    line_number_width: usize,
    palette: &TintPalette,
) -> String {
    let expanded = expand_tabs(text);
    let prefix = if palette.changed_line_bg.is_some() {
        format!("{line_number:>line_number_width$}  ")
    } else {
        format!("{line_number:>line_number_width$} -")
    };
    if let Some(bg) = palette.changed_line_bg {
        format!("{}{prefix}\u{1b}[2m{expanded}\u{1b}[0m", ansi_bg(bg))
    } else {
        format!("{prefix}\u{1b}[2m{expanded}\u{1b}[0m")
    }
}

fn render_inline_inserted_line(
    line_number: usize,
    text: &str,
    line_number_width: usize,
    palette: &TintPalette,
) -> String {
    let prefix = format!("{line_number:>line_number_width$} ");
    if let Some(color) = palette.gutter_fg {
        let expanded = expand_tabs(text);
        if expanded.is_empty() {
            return format!("{prefix}{} \u{1b}[0m ", ansi_bg(color));
        }
        return format!("{prefix}{} \u{1b}[0m{expanded}", ansi_bg(color));
    }
    format!("{prefix} {}", expand_tabs(text))
}

fn render_inline_changed_line(
    line_number: usize,
    new: &str,
    line_number_width: usize,
    palette: &TintPalette,
) -> String {
    render_inline_inserted_line(line_number, new, line_number_width, palette)
}

fn render_marker_cell(width: usize, marker: &str) -> String {
    let marker_width = display_width(marker).min(width);
    let clipped = clip_plain_text(marker, width);
    let left_pad = width.saturating_sub(marker_width) / 2;
    let right_pad = width.saturating_sub(marker_width + left_pad);
    format!(
        "{}{}{}",
        " ".repeat(left_pad),
        clipped,
        " ".repeat(right_pad)
    )
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
                left_background: None,
                left_dim: false,
                left_line_number: Some(right_number),
                center_number: format_right_line_number(Some(right_number)),
                right_line_number: Some(right_number),
                right_gutter: false,
                right_line: StyledLine {
                    text: line,
                    ..Default::default()
                },
            }
        }
        Row::Delete(text) => {
            let left_number = *old_line;
            *old_line += 1;

            RenderedRow::SideBySide {
                left_text: expand_tabs(text),
                left_background: palette.changed_line_bg,
                left_dim: true,
                left_line_number: Some(left_number),
                center_number: format_right_line_number(None),
                right_line_number: None,
                right_gutter: false,
                right_line: StyledLine::default(),
            }
        }
        Row::Insert(text) => {
            let right_number = *new_line;
            *new_line += 1;

            RenderedRow::SideBySide {
                left_text: String::new(),
                left_background: None,
                left_dim: false,
                left_line_number: None,
                center_number: format_right_line_number(Some(right_number)),
                right_line_number: Some(right_number),
                right_gutter: true,
                right_line: StyledLine {
                    text: expand_tabs(text),
                    ..Default::default()
                },
            }
        }
        Row::Change { old, new } => {
            let left_number = *old_line;
            let right_number = *new_line;
            *old_line += 1;
            *new_line += 1;

            RenderedRow::SideBySide {
                left_text: expand_tabs(old),
                left_background: palette.changed_line_bg,
                left_dim: true,
                left_line_number: Some(left_number),
                center_number: format_right_line_number(Some(right_number)),
                right_line_number: Some(right_number),
                right_gutter: true,
                right_line: StyledLine {
                    text: expand_tabs(new),
                    ..Default::default()
                },
            }
        }
        Row::Annotation(text) => RenderedRow::FullWidth(text.clone()),
    }
}

fn render_expanded_gap_lines(
    file_path: &str,
    lines: &[String],
    start_line: usize,
    layout: Layout,
    palette: &TintPalette,
    output: &mut RenderedDocument,
) {
    for (offset, line) in lines.iter().enumerate() {
        let expanded = expand_tabs(line);
        let rendered = RenderedRow::SideBySide {
            left_text: expanded.clone(),
            left_background: None,
            left_dim: false,
            left_line_number: Some(start_line + offset),
            center_number: format_right_line_number(Some(start_line + offset)),
            right_line_number: Some(start_line + offset),
            right_gutter: false,
            right_line: StyledLine {
                text: expanded,
                ..Default::default()
            },
        };
        let mut show_gap_continuation = false;
        push_rendered_row(
            output,
            layout.left_text_width + layout.center_number_width + layout.right_visible_width,
            layout,
            palette,
            file_path,
            rendered,
            &mut show_gap_continuation,
        );
    }
}

fn render_gap_state(
    gap: &GapDescriptor,
    layout: Layout,
    palette: &TintPalette,
    output: &mut RenderedDocument,
    state: &ExpandedGapState,
) {
    render_expanded_gap_lines(
        &gap.id.file_path,
        &state.lines[..state.top_len],
        gap.start_line,
        layout,
        palette,
        output,
    );

    if state.hidden_len() > 0 {
        let marker = if state.selector_open {
            render_compact_selector_row(layout)
        } else {
            render_compact_elision_row(layout)
        };
        output.push_line(marker, Some(gap.clone()), None);
    }

    if state.bottom_len > 0 {
        let start_line = gap.start_line + state.lines.len().saturating_sub(state.bottom_len);
        render_expanded_gap_lines(
            &gap.id.file_path,
            &state.lines[state.lines.len() - state.bottom_len..],
            start_line,
            layout,
            palette,
            output,
        );
    }
}

fn render_gap(
    gap: GapDescriptor,
    layout: Layout,
    palette: &TintPalette,
    gap_states: &HashMap<GapId, GapState>,
    spinner_frame: usize,
    output: &mut RenderedDocument,
) {
    match gap_states.get(&gap.id) {
        Some(GapState::Expanded(state)) => {
            render_gap_state(&gap, layout, palette, output, state);
        }
        Some(GapState::Loading(_)) => {
            output.push_line(
                render_compact_spinner_row(layout, spinner_frame),
                Some(gap),
                None,
            );
        }
        Some(GapState::CollapsedSelector) => {
            output.push_line(render_compact_selector_row(layout), Some(gap), None);
        }
        None => {
            output.push_line(render_compact_elision_row(layout), Some(gap), None);
        }
    }
}

fn push_rendered_row(
    output: &mut RenderedDocument,
    width: usize,
    layout: Layout,
    palette: &TintPalette,
    file_path: &str,
    row: RenderedRow,
    show_gap_continuation: &mut bool,
) {
    match row {
        RenderedRow::SideBySide {
            mut left_text,
            left_background,
            left_dim,
            left_line_number,
            center_number,
            right_line_number,
            right_gutter,
            right_line,
        } => {
            if *show_gap_continuation && left_text.is_empty() {
                left_text = "..".to_owned();
            }
            *show_gap_continuation = false;

            let mut line = String::new();
            line.push_str(&render_plain_cell_with_style(
                &left_text,
                layout.left_text_width,
                left_background,
                left_dim,
            ));
            line.push_str(PANE_GAP);
            line.push_str(&render_center_number(
                &center_number,
                layout.center_number_width,
            ));
            if right_gutter {
                if let Some(color) = palette.gutter_fg {
                    line.push(' ');
                    line.push_str(&ansi_bg(color));
                    line.push(' ');
                    line.push_str("\u{1b}[0m");
                } else {
                    line.push_str(PANE_GAP);
                }
            } else {
                line.push_str(PANE_GAP);
            }
            let right_cell = render_styled_cell(&right_line, layout.right_render_width);
            if right_cell.is_empty() && right_gutter {
                line.push(' ');
            } else {
                line.push_str(&right_cell);
            }
            output.push_line(
                line,
                None,
                source_location(file_path, left_line_number, right_line_number),
            );
        }
        RenderedRow::FullWidth(text) => {
            output.push_line(clip_plain_text(&text, width), None, None);
        }
    }
}

fn render_plain_cell(text: &str, width: usize) -> String {
    let clipped = clip_plain_text(text, width);
    let pad = width.saturating_sub(display_width(&clipped));
    format!("{clipped}{}", " ".repeat(pad))
}

fn render_plain_cell_with_style(
    text: &str,
    width: usize,
    background: Option<AnsiColor>,
    dim: bool,
) -> String {
    let visible = clip_plain_text(text, width);
    let pad = " ".repeat(width.saturating_sub(display_width(&visible)));

    match (background, dim, visible.is_empty()) {
        (Some(bg), true, false) => {
            format!(
                "{}\u{1b}[2m{visible}\u{1b}[0m{}{}\u{1b}[0m",
                ansi_bg(bg),
                ansi_bg(bg),
                pad
            )
        }
        (Some(bg), _, _) => format!("{}{visible}{}\u{1b}[0m", ansi_bg(bg), pad),
        (None, true, false) => format!("\u{1b}[2m{visible}\u{1b}[0m{pad}"),
        (None, _, _) => format!("{visible}{pad}"),
    }
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
    let visible = clip_plain_text(&line.text, width);
    if visible.is_empty() {
        return String::new();
    }

    if let Some(bg) = line.background {
        format!("{}{visible}\u{1b}[0m", ansi_bg(bg))
    } else {
        visible
    }
}

fn ansi_bg(color: AnsiColor) -> String {
    match color {
        AnsiColor::Indexed(index) => format!("\u{1b}[48;5;{index}m"),
        AnsiColor::Rgb(r, g, b) => format!("\u{1b}[48;2;{r};{g};{b}m"),
    }
}

fn ansi_fg(color: AnsiColor) -> String {
    match color {
        AnsiColor::Indexed(index) => format!("\u{1b}[38;5;{index}m"),
        AnsiColor::Rgb(r, g, b) => format!("\u{1b}[38;2;{r};{g};{b}m"),
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
    use super::ExpandedGapState;
    use super::GapExpandRequest;
    use super::GapId;
    use super::GapState;
    use super::Layout;
    use super::RenderMode;
    use super::StyledLine;
    use super::TintPalette;
    use super::display_width;
    use super::expand_tabs;
    use super::format_right_line_number;
    use super::render_compact_elision_row;
    use super::render_document;
    use super::render_document_with_state;
    use super::render_document_with_state_and_file_counts;
    use super::render_inline_document;
    use super::render_inline_document_with_state_and_file_counts;
    use super::render_inline_inserted_line;
    use super::render_styled_cell;
    use crate::terminal_palette::AnsiColor;
    use crate::unified_diff::Document;
    use crate::unified_diff::Hunk;
    use crate::unified_diff::Item;
    use crate::unified_diff::Row;
    use std::collections::HashMap;

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
        let document = Document::from_items(vec![
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
        ]);
        let palette = TintPalette {
            changed_line_bg: Some(AnsiColor::Indexed(240)),
            gutter_fg: Some(AnsiColor::Indexed(238)),
        };

        let rendered = render_document(&document, 140, &palette);
        assert!(rendered.contains("\u{1b}[1mdemo.txt\u{1b}[0m"));
        assert!(rendered.contains("⋮"));
        assert!(!rendered.contains("@@ -1 +1 @@"));
        // left (deleted) side has tinted background and dims the removed text payload
        assert!(rendered.contains("\u{1b}[48;5;240m\u{1b}[2mcat\u{1b}[0m"));
        // right (added) side has gutter mark (foreground color)
        assert!(rendered.contains("\u{1b}[48;5;238m \u{1b}[0mcot"));
        assert!(rendered.contains("\u{1b}[1m 3  \u{1b}[0m"));
        assert!(!rendered.contains(" | "));
    }

    #[test]
    fn renders_inline_document_with_markers_and_ellipsis() {
        let document = Document::from_items(vec![
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
        ]);
        let palette = TintPalette {
            changed_line_bg: Some(AnsiColor::Indexed(240)),
            gutter_fg: Some(AnsiColor::Indexed(238)),
        };

        let rendered = render_inline_document(&document, 80, &palette);
        assert!(rendered.contains("\u{1b}[1msrc/unified_diff.rs\u{1b}[0m"));
        assert!(rendered.contains("   ⋮"));
        assert!(rendered.contains("  15      pub old_start: usize,"));
        // deleted lines keep the tinted background and dim the removed text payload
        assert!(
            rendered.contains("\u{1b}[48;5;240m  16  \u{1b}[2m    pub old_len: usize,\u{1b}[0m")
        );
        assert!(rendered.contains("  16  "));
        assert!(rendered.contains("  16      pub new_start: usize,"));
        assert!(rendered.contains(" 127  "));
        // changed/inserted lines have gutter mark
        assert!(rendered.contains("\u{1b}[48;5;238m \u{1b}[0m"));
    }

    #[test]
    fn renders_gap_continuation_marker_before_first_insert_after_elision() {
        let document = Document::from_items(vec![
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
        ]);
        let palette = TintPalette {
            changed_line_bg: Some(AnsiColor::Indexed(240)),
            gutter_fg: Some(AnsiColor::Indexed(238)),
        };

        let rendered = render_document(&document, 140, &palette);
        assert!(rendered.contains(" ⋮ "));
        assert!(rendered.contains(".."));
        assert!(rendered.contains("\u{1b}[1m 10 \u{1b}[0m"));
    }

    #[test]
    fn renders_spinner_for_loading_gap() {
        let document = Document::from_items(vec![
            Item::FileHeader("demo.txt".into()),
            Item::Hunk(Hunk {
                old_start: 3,
                new_start: 3,
                new_len: 1,
                rows: vec![Row::Context("gamma".into())],
            }),
        ]);
        let mut gap_states = HashMap::new();
        gap_states.insert(
            GapId {
                file_path: "demo.txt".into(),
                hunk_index: 0,
            },
            GapState::Loading(GapExpandRequest::Full),
        );

        let rendered =
            render_document_with_state(&document, 140, &TintPalette::default(), &gap_states, 0);

        assert!(rendered.lines[1].contains('⠁'));
        assert_eq!(
            rendered.line_metadata[1]
                .gap
                .as_ref()
                .map(|gap| gap.start_line),
            Some(1)
        );
    }

    #[test]
    fn renders_expanded_gap_content_in_place_of_ellipsis() {
        let document = Document::from_items(vec![
            Item::FileHeader("demo.txt".into()),
            Item::Hunk(Hunk {
                old_start: 3,
                new_start: 3,
                new_len: 1,
                rows: vec![Row::Context("gamma".into())],
            }),
        ]);
        let mut gap_states = HashMap::new();
        gap_states.insert(
            GapId {
                file_path: "demo.txt".into(),
                hunk_index: 0,
            },
            GapState::Expanded(ExpandedGapState::fully_expanded(vec![
                "alpha".into(),
                "beta".into(),
            ])),
        );

        let rendered =
            render_document_with_state(&document, 140, &TintPalette::default(), &gap_states, 0);

        assert!(rendered.lines[1].contains("alpha"));
        assert!(rendered.lines[2].contains("beta"));
        assert!(rendered.line_metadata[1].gap.is_none());
        assert!(!rendered.into_output().contains('⋮'));
    }

    #[test]
    fn inline_insert_uses_gutter_mark() {
        let palette = TintPalette {
            changed_line_bg: Some(AnsiColor::Indexed(240)),
            gutter_fg: Some(AnsiColor::Indexed(238)),
        };

        let line = render_inline_inserted_line(12, "abc", 4, &palette);
        assert!(line.starts_with("  12 "));
        assert!(line.contains("\u{1b}[48;5;238m \u{1b}[0mabc"));
    }

    #[test]
    fn inline_insertions_keep_full_text_for_horizontal_scroll() {
        let palette = TintPalette {
            changed_line_bg: Some(AnsiColor::Indexed(240)),
            gutter_fg: Some(AnsiColor::Indexed(238)),
        };

        let line = render_inline_inserted_line(12, "abcdefghijklmnopqrstuvwxyz", 4, &palette);
        assert!(line.contains("abcdefghijklmnopqrstuvwxyz"));
    }

    #[test]
    fn side_by_side_keeps_full_right_text_for_horizontal_scroll() {
        let long = "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz";
        let document = Document::from_items(vec![
            Item::FileHeader("demo.txt".into()),
            Item::Hunk(Hunk {
                old_start: 1,
                new_start: 1,
                new_len: 2,
                rows: vec![Row::Insert("abc".into()), Row::Insert(long.into())],
            }),
        ]);
        let palette = TintPalette {
            changed_line_bg: Some(AnsiColor::Indexed(240)),
            gutter_fg: Some(AnsiColor::Indexed(238)),
        };

        let rendered = render_document(&document, 140, &palette);
        assert!(rendered.contains(long));
        // inserted lines use gutter mark, not background
        assert!(rendered.contains("\u{1b}[48;5;238m \u{1b}[0mabc"));
    }

    #[test]
    fn non_tinted_side_by_side_cells_do_not_pad_hidden_spaces() {
        let line = StyledLine {
            text: "abc".into(),
            ..Default::default()
        };

        let rendered = render_styled_cell(&line, 20);
        assert_eq!(rendered, "abc");
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
    fn styled_line_with_background_renders_bg() {
        let line = StyledLine {
            text: "abc".into(),
            background: Some(AnsiColor::Indexed(240)),
            dim: false,
        };

        let rendered = render_styled_cell(&line, 20);
        assert_eq!(rendered, "\u{1b}[48;5;240mabc\u{1b}[0m");
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
            right_visible_width: 6,
            right_render_width: 6,
        });
        assert!(header.contains(" ⋮  "));
    }

    #[test]
    fn renders_selector_for_collapsed_gap() {
        let document = Document::from_items(vec![
            Item::FileHeader("demo.txt".into()),
            Item::Hunk(Hunk {
                old_start: 3,
                new_start: 3,
                new_len: 1,
                rows: vec![Row::Context("gamma".into())],
            }),
        ]);
        let mut gap_states = HashMap::new();
        gap_states.insert(
            GapId {
                file_path: "demo.txt".into(),
                hunk_index: 0,
            },
            GapState::CollapsedSelector,
        );

        let rendered =
            render_document_with_state(&document, 140, &TintPalette::default(), &gap_states, 0);

        assert!(rendered.lines[1].contains("▴⋮▾"));
    }

    #[test]
    fn renders_partial_gap_with_selector_between_revealed_slices() {
        let document = Document::from_items(vec![
            Item::FileHeader("demo.txt".into()),
            Item::Hunk(Hunk {
                old_start: 5,
                new_start: 5,
                new_len: 1,
                rows: vec![Row::Context("epsilon".into())],
            }),
        ]);
        let mut gap_states = HashMap::new();
        gap_states.insert(
            GapId {
                file_path: "demo.txt".into(),
                hunk_index: 0,
            },
            GapState::Expanded(ExpandedGapState {
                lines: vec![
                    "alpha".into(),
                    "beta".into(),
                    "gamma".into(),
                    "delta".into(),
                ],
                top_len: 1,
                bottom_len: 1,
                selector_open: true,
            }),
        );

        let rendered =
            render_document_with_state(&document, 140, &TintPalette::default(), &gap_states, 0);

        assert!(rendered.lines[1].contains("alpha"));
        assert!(rendered.lines[2].contains("▴⋮▾"));
        assert!(rendered.lines[3].contains("delta"));
        assert!(rendered.line_metadata[2].gap.is_some());
    }

    #[test]
    fn renders_trailing_gap_after_last_hunk() {
        let document = Document::from_items(vec![
            Item::FileHeader("demo.txt".into()),
            Item::Hunk(Hunk {
                old_start: 1,
                new_start: 1,
                new_len: 1,
                rows: vec![Row::Context("alpha".into())],
            }),
        ]);
        let file_line_counts = HashMap::from([(String::from("demo.txt"), 4usize)]);

        let rendered = render_document_with_state_and_file_counts(
            &document,
            140,
            &TintPalette::default(),
            &file_line_counts,
            &HashMap::new(),
            0,
        );

        assert!(rendered.lines[2].contains('⋮'));
        assert_eq!(
            rendered.line_metadata[2].gap,
            Some(super::GapDescriptor {
                id: GapId {
                    file_path: "demo.txt".into(),
                    hunk_index: 1,
                },
                start_line: 2,
                line_count: 3,
            })
        );
    }

    #[test]
    fn renders_inline_trailing_gap_after_last_hunk() {
        let document = Document::from_items(vec![
            Item::FileHeader("demo.txt".into()),
            Item::Hunk(Hunk {
                old_start: 2,
                new_start: 2,
                new_len: 1,
                rows: vec![Row::Context("beta".into())],
            }),
        ]);
        let file_line_counts = HashMap::from([(String::from("demo.txt"), 5usize)]);

        let rendered = render_inline_document_with_state_and_file_counts(
            &document,
            &TintPalette::default(),
            &file_line_counts,
            &HashMap::new(),
            0,
        );

        assert!(rendered.lines[3].contains('⋮'));
        assert_eq!(
            rendered.line_metadata[3].gap,
            Some(super::GapDescriptor {
                id: GapId {
                    file_path: "demo.txt".into(),
                    hunk_index: 1,
                },
                start_line: 3,
                line_count: 3,
            })
        );
    }
}
