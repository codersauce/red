//! Width-aware mapping between buffer lines, wrapped screen rows, and cursor coordinates.
//!
//! Layout consumes grapheme boundaries and terminal-cell widths to build an immutable
//! [`DisplayLayout`] for one viewport configuration. It owns wrapping, horizontal
//! offsets, continuation indentation, concealed spans, and the conversions used by
//! vertical screen-line motion. It does not paint styles or mutate window state.
//!
//! Callers must include every layout-affecting input in their cache key. Reusing a
//! layout across a buffer revision, viewport width, wrap mode, indentation, or
//! concealment change would produce incorrect cursor and hit-test positions even when
//! the underlying text looks similar.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation as _;

use crate::unicode_utils::{
    char_display_width, display_width, trim_line_ending, truncate_display_width,
};

/// Minimum number of text columns kept on a wrapped row after applying
/// break-indent, mirroring vim's `breakindentopt` `min:20` default.
const BREAK_INDENT_MIN_TEXT_WIDTH: usize = 20;
const MAX_INLINE_COMMENT_TEXT_ROWS: usize = 4;
const INLINE_COMMENT_HORIZONTAL_PADDING: usize = 2;

pub(super) fn wrap_inline_comment(message: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    textwrap::wrap(
        message,
        textwrap::Options::new(width)
            .wrap_algorithm(textwrap::WrapAlgorithm::FirstFit)
            .word_splitter(textwrap::WordSplitter::NoHyphenation),
    )
    .into_iter()
    .map(|row| row.into_owned())
    .collect()
}

/// Geometry shared by viewport composition and cursor visibility checks.
#[derive(Debug, Default)]
pub(super) struct InlineCommentBlock {
    pub rows: Vec<InlineCommentContent>,
    pub width: usize,
    pub text_offset: usize,
}

pub(super) fn inline_comment_block(
    message: &str,
    width: usize,
    available_rows: usize,
) -> InlineCommentBlock {
    if width == 0 || available_rows == 0 {
        return InlineCommentBlock::default();
    }
    // Tiny splits shed padding before sacrificing the source line or text.
    let horizontal_padding = INLINE_COMMENT_HORIZONTAL_PADDING.min(width.saturating_sub(1) / 2);
    let vertical_padding = usize::from(available_rows >= 3);
    let text_width = width - horizontal_padding * 2;
    let limit = MAX_INLINE_COMMENT_TEXT_ROWS.min(available_rows - vertical_padding * 2);
    let wrapped = wrap_inline_comment(message, text_width);
    let truncated = wrapped.len() > limit;
    let text_rows = wrapped
        .into_iter()
        .take(limit)
        .enumerate()
        .map(|(index, text)| {
            if truncated && index + 1 == limit {
                format!("{}…", truncate_display_width(&text, text_width - 1))
            } else {
                truncate_display_width(&text, text_width)
            }
        })
        .collect::<Vec<_>>();
    let block_width = text_rows
        .iter()
        .map(|text| display_width(text))
        .max()
        .unwrap_or(0)
        + horizontal_padding * 2;
    let mut rows = Vec::with_capacity(text_rows.len() + vertical_padding * 2);
    if vertical_padding > 0 {
        rows.push(InlineCommentContent::TopEdge);
    }
    rows.extend(text_rows.into_iter().map(InlineCommentContent::Text));
    if vertical_padding > 0 {
        rows.push(InlineCommentContent::BottomEdge);
    }
    InlineCommentBlock {
        rows,
        width: block_width,
        text_offset: horizontal_padding,
    }
}

/// How wrapped continuation rows are indented, mirroring vim's
/// 'breakindent' option: continuations start with a blank virtual indent
/// matching the line's leading whitespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BreakIndentOptions {
    pub enabled: bool,
    pub tab_width: usize,
}

impl Default for BreakIndentOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            tab_width: 4,
        }
    }
}

impl BreakIndentOptions {
    #[cfg(test)]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            tab_width: 4,
        }
    }
}

pub fn leading_whitespace_display_width(line: &str, tab_width: usize) -> usize {
    leading_whitespace_display_width_up_to(line, tab_width, usize::MAX)
}

fn leading_whitespace_display_width_up_to(line: &str, tab_width: usize, max_width: usize) -> usize {
    if max_width == 0 {
        return 0;
    }

    let tab_width = tab_width.max(1);
    let mut width = 0;
    for ch in line.chars() {
        let next = match ch {
            ' ' => 1,
            '\t' => tab_width - (width % tab_width),
            ch if ch.is_whitespace() => char_display_width(ch).max(1),
            _ => break,
        };
        width = width.saturating_add(next);
        if width >= max_width {
            return max_width;
        }
    }
    width
}

fn break_indent_width(line: &str, width: usize, options: BreakIndentOptions) -> usize {
    if !options.enabled {
        return 0;
    }
    leading_whitespace_display_width_up_to(
        line,
        options.tab_width,
        width.saturating_sub(BREAK_INDENT_MIN_TEXT_WIDTH),
    )
}

/// One screen row of a Markdown image block.
///
/// `index` is the row within the image. `start_col` on the segment repeats
/// that index so viewport scrolling can skip rows with `skipcol`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ImageSpan {
    pub index: u16,
    pub rows: u16,
    pub cols: u16,
    pub placeholder: bool,
}

/// Builds the screen rows reserved for one inline image.
pub(super) fn image_line_segments(
    line: usize,
    rows: u16,
    cols: u16,
    placeholder: bool,
) -> Vec<LineSegment> {
    (0..rows)
        .map(|index| LineSegment {
            line,
            row: usize::from(index),
            start_col: usize::from(index),
            end_col: usize::from(index),
            start_grapheme: 0,
            end_grapheme: 0,
            start_byte: 0,
            end_byte: 0,
            start_grapheme_col: 0,
            source_offset: 0,
            first_segment: index == 0,
            last_segment: index + 1 == rows,
            visual_offset: 0,
            concealed: Vec::new(),
            projection: None,
            projection_left: 0,
            image: Some(ImageSpan {
                index,
                rows,
                cols,
                placeholder,
            }),
        })
        .collect()
}

/// Byte ranges of one source line that occupy no screen cells.
///
/// Ranges are relative to the line without its line ending. They are sorted,
/// non-overlapping, and half-open. Wrapping and screen columns skip them, while
/// source columns stay addressable so a cursor can still enter the hidden text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Concealment {
    hidden_bytes: Vec<Range<usize>>,
}

impl Concealment {
    pub(super) fn new(mut hidden_bytes: Vec<Range<usize>>) -> Self {
        hidden_bytes.retain(|range| range.start < range.end);
        hidden_bytes.sort_by_key(|range| (range.start, range.end));
        let mut merged: Vec<Range<usize>> = Vec::new();
        for range in hidden_bytes {
            if let Some(last) = merged.last_mut() {
                if range.start <= last.end {
                    last.end = last.end.max(range.end);
                    continue;
                }
            }
            merged.push(range);
        }
        Self {
            hidden_bytes: merged,
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.hidden_bytes.is_empty()
    }

    pub(super) fn hides_byte(&self, byte: usize) -> bool {
        let index = self.hidden_bytes.partition_point(|range| range.end <= byte);
        self.hidden_bytes
            .get(index)
            .is_some_and(|range| range.contains(&byte))
    }

    pub(super) fn hides_span(&self, start: usize, len: usize) -> bool {
        if len == 0 {
            return false;
        }
        let end = start + len;
        let index = self
            .hidden_bytes
            .partition_point(|range| range.end <= start);
        self.hidden_bytes
            .get(index)
            .is_some_and(|range| start < range.end && range.start < end)
    }
}

/// Per-buffer-line concealment, indexed by buffer line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct LineConcealments {
    lines: Vec<Concealment>,
}

impl LineConcealments {
    pub(super) fn new(lines: Vec<Concealment>) -> Self {
        Self { lines }
    }

    pub(super) fn line(&self, index: usize) -> Concealment {
        self.lines.get(index).cloned().unwrap_or_default()
    }
}

fn visible_columns_between(concealed: &[Range<usize>], from: usize, to: usize) -> usize {
    if to <= from {
        return 0;
    }
    let mut hidden = 0;
    for range in concealed {
        if range.end <= from {
            continue;
        }
        if range.start >= to {
            break;
        }
        hidden += range.end.min(to).saturating_sub(range.start.max(from));
    }
    (to - from).saturating_sub(hidden)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineSegment {
    pub line: usize,
    pub row: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub start_grapheme: usize,
    pub end_grapheme: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    /// Display column of the first grapheme in the segment. This can precede
    /// `start_col` when horizontal scrolling starts inside a tab or wide
    /// grapheme.
    pub start_grapheme_col: usize,
    pub source_offset: usize,
    pub first_segment: bool,
    /// Whether this segment reaches the source line's end, not just the viewport edge.
    pub last_segment: bool,
    /// Blank screen columns drawn before the segment's text. Zero on first
    /// segments; on wrapped continuations it aligns the text with the
    /// line's indentation (vim's 'breakindent').
    pub visual_offset: usize,
    /// Source display columns drawn with zero width. Empty for ordinary text.
    /// Ranges are half-open, sorted, and non-overlapping.
    pub(super) concealed: Vec<Range<usize>>,
    /// A table row maps disjoint cell ranges instead of one linear source slice.
    pub(super) projection: Option<std::sync::Arc<super::markdown_tables::ProjectedRow>>,
    pub(super) projection_left: usize,
    /// Set when this row is part of an inline Markdown image.
    pub(super) image: Option<ImageSpan>,
}

impl LineSegment {
    pub fn contains_display_col(&self, col: usize) -> bool {
        if self.image.is_some() {
            return col == 0;
        }
        if let Some(projection) = &self.projection {
            return projection.contains(col);
        }
        if self.start_col == self.end_col {
            return col == self.start_col;
        }

        col >= self.start_col && col < self.end_col
    }

    /// Includes the insertion caret after the final character, but keeps
    /// boundaries between wrapped segments exclusive on the preceding row.
    pub fn contains_cursor_col(&self, col: usize) -> bool {
        if let Some(image) = self.image {
            return image.index == 0 && col == 0;
        }
        if self.projection.is_some() {
            return self.contains_display_col(col);
        }
        self.contains_display_col(col) || (self.last_segment && col == self.end_col)
    }

    pub fn screen_col_for_display_col(&self, col: usize, width: usize) -> usize {
        if self.image.is_some() {
            return 0;
        }
        if let Some(projection) = &self.projection {
            return projection
                .column_for_source(col)
                .saturating_sub(self.projection_left)
                .min(width.saturating_sub(1));
        }
        let visible =
            visible_columns_between(&self.concealed, self.start_col, col.max(self.start_col));
        (self.visual_offset + visible).min(width.saturating_sub(1))
    }

    pub fn display_col_for_screen_col(&self, col: usize) -> usize {
        if self.image.is_some() {
            return 0;
        }
        if let Some(projection) = &self.projection {
            return projection.source_for_column(col + self.projection_left);
        }
        let target = col.saturating_sub(self.visual_offset);
        if self.concealed.is_empty() {
            return self.start_col + target;
        }
        let mut source = self.start_col;
        let mut visual = 0usize;
        for range in &self.concealed {
            if range.start <= source {
                source = source.max(range.end);
                continue;
            }
            let gap = range.start.saturating_sub(source);
            if target < visual + gap {
                return source + (target - visual);
            }
            visual += gap;
            source = range.end;
        }
        let trailing = self.end_col.saturating_sub(source);
        if target < visual + trailing {
            return source + (target - visual);
        }
        self.last_visible_source_col()
    }

    pub(super) fn hides_source_column(&self, column: usize) -> bool {
        self.concealed.iter().any(|range| range.contains(&column))
    }

    pub(super) fn visible_columns_before(&self, source_col: usize) -> usize {
        visible_columns_between(
            &self.concealed,
            self.start_col,
            source_col.max(self.start_col),
        )
    }

    /// Screen spans, including break-indent, for a source display-column range.
    /// Concealed columns are omitted so highlights land on the visible text.
    pub(super) fn visible_screen_spans(
        &self,
        start_col: usize,
        end_col: usize,
    ) -> Vec<(usize, usize)> {
        let start = start_col.max(self.start_col);
        let end = end_col.min(self.end_col);
        if end <= start {
            return Vec::new();
        }
        if self.concealed.is_empty() {
            return vec![(
                self.visual_offset + start - self.start_col,
                self.visual_offset + end - self.start_col,
            )];
        }
        let mut spans = Vec::new();
        let mut column = start;
        while column < end {
            if let Some(hidden) = self.concealed.iter().find(|range| range.contains(&column)) {
                column = hidden.end;
                continue;
            }
            let run_start = column;
            while column < end && !self.concealed.iter().any(|range| range.contains(&column)) {
                column += 1;
            }
            let local_start = self.visual_offset + self.visible_columns_before(run_start);
            let local_end = self.visual_offset + self.visible_columns_before(column);
            if local_end > local_start {
                spans.push((local_start, local_end));
            }
        }
        spans
    }

    /// First source column of this row that is actually drawn.
    /// Falls back to `start_col` when the row is entirely concealed.
    pub(super) fn first_visible_source_col(&self) -> usize {
        let mut column = self.start_col;
        for range in &self.concealed {
            if range.end <= column {
                continue;
            }
            if range.start > column {
                return column;
            }
            column = range.end;
        }
        if column < self.end_col {
            column
        } else {
            self.start_col
        }
    }

    /// Inclusive source column of the last drawn cell on this row.
    pub(super) fn last_visible_source_col(&self) -> usize {
        let mut end = self.end_col;
        for range in self.concealed.iter().rev() {
            if range.end < end {
                break;
            }
            if range.start < end {
                end = range.start;
            }
        }
        if end > self.start_col {
            end - 1
        } else {
            self.start_col
        }
    }

    pub(super) fn next_concealed_column_after(&self, column: usize) -> Option<usize> {
        self.concealed
            .iter()
            .map(|range| range.start)
            .find(|start| *start > column)
    }
}

/// Content or a half-height edge in a display-only comment block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InlineCommentContent {
    TopEdge,
    Text(String),
    BottomEdge,
}

impl InlineCommentContent {
    pub fn text(&self) -> &str {
        match self {
            Self::Text(text) => text,
            Self::TopEdge | Self::BottomEdge => "",
        }
    }
}

/// A non-editable screen row attached immediately above a source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineCommentRow {
    pub line: usize,
    pub row: usize,
    pub content: InlineCommentContent,
    /// The first text row is where the gutter rail joins the comment box.
    pub starts_connection: bool,
    pub block_width: usize,
    pub text_offset: usize,
}

/// One row in a transient insertion preview. Byte offsets refer to the projected
/// logical line; removing the insertion maps them back to source highlighting.
#[derive(Debug, Clone)]
pub(super) struct InlinePredictionRow {
    pub row: usize,
    pub text: String,
    pub display_col: usize,
    pub visual_offset: usize,
    pub projected_start: usize,
    pub insertion: std::ops::Range<usize>,
    pub source_offset: usize,
}

#[derive(Debug, Clone, Default)]
pub struct DisplayLayout {
    /// Source segments only. `segment.row` is the physical screen row and may
    /// have gaps occupied by inline comments.
    pub rows: Vec<LineSegment>,
    pub inline_comments: Vec<InlineCommentRow>,
    pub(super) inline_prediction: Vec<InlinePredictionRow>,
}

impl DisplayLayout {
    pub fn row(&self, row: usize) -> Option<&LineSegment> {
        if self.inline_comments.is_empty() && self.inline_prediction.is_empty() {
            return self.rows.get(row);
        }
        self.rows
            .binary_search_by_key(&row, |segment| segment.row)
            .ok()
            .map(|index| &self.rows[index])
    }

    pub fn inline_comment_row(&self, row: usize) -> Option<&InlineCommentRow> {
        self.inline_comments
            .binary_search_by_key(&row, |comment| comment.row)
            .ok()
            .map(|index| &self.inline_comments[index])
    }

    pub fn screen_height(&self) -> usize {
        self.rows
            .last()
            .map_or(0, |segment| segment.row + 1)
            .max(
                self.inline_comments
                    .last()
                    .map_or(0, |comment| comment.row + 1),
            )
            .max(self.inline_prediction.last().map_or(0, |row| row.row + 1))
    }

    /// Projects an insertion without putting virtual bytes into source segments.
    /// Source motions dismiss the preview before consulting this layout.
    pub(super) fn with_inline_prediction(
        mut self,
        line_index: usize,
        line: &str,
        cursor_byte: usize,
        insertion: &str,
        config: LayoutConfig,
    ) -> Self {
        if insertion.is_empty() || !line.is_char_boundary(cursor_byte) {
            return self;
        }
        let cursor_col = crate::unicode_utils::display_width_with_tabs(
            &line[..cursor_byte],
            config.break_indent.tab_width,
        );
        let Some(mut anchor) = self.segment_for_cursor(line_index, cursor_col).cloned() else {
            return self;
        };
        let start_row = anchor.row;
        let old_end = self
            .rows
            .iter()
            .filter(|row| row.line == line_index)
            .map(|row| row.row + 1)
            .max()
            .unwrap_or(start_row + 1);
        let insertion = insertion.replace("\r\n", "\n");
        let projected = format!(
            "{}{}{}",
            &line[..cursor_byte],
            insertion,
            &line[cursor_byte..]
        );
        let lines = projected
            .split('\n')
            .map(|line| format!("{line}\n"))
            .collect::<Vec<_>>();
        let preview = layout_lines(
            &lines,
            lines.len(),
            LayoutConfig {
                vtop: 0,
                vleft: if config.wrap { 0 } else { anchor.start_col },
                skipcol: if config.wrap { anchor.start_col } else { 0 },
                height: config.height.saturating_sub(start_row),
                ..config
            },
        );
        let insertion_range = cursor_byte..cursor_byte + insertion.len();
        self.inline_prediction = preview
            .rows
            .into_iter()
            .map(|segment| {
                let text = trim_line_ending(&lines[segment.line]);
                InlinePredictionRow {
                    row: start_row + segment.row,
                    text: text[segment.start_byte..segment.end_byte].to_string(),
                    display_col: segment.start_grapheme_col,
                    visual_offset: segment.visual_offset
                        + segment.start_grapheme_col.saturating_sub(segment.start_col),
                    projected_start: segment.source_offset + segment.start_byte,
                    insertion: insertion_range.clone(),
                    source_offset: anchor.source_offset,
                }
            })
            .collect();
        if self.inline_prediction.is_empty() {
            return self;
        }
        let new_end = start_row + self.inline_prediction.len();
        let shift = |row: usize| new_end + row.saturating_sub(old_end);
        self.rows
            .retain(|row| row.row < start_row || row.row >= old_end);
        for row in &mut self.rows {
            if row.row >= old_end {
                row.row = shift(row.row);
            }
        }
        anchor.end_col = cursor_col;
        anchor.last_segment = true;
        self.rows.push(anchor);
        self.rows.sort_unstable_by_key(|row| row.row);
        self.rows.retain(|row| row.row < config.height);
        for comment in &mut self.inline_comments {
            if comment.row >= old_end {
                comment.row = shift(comment.row);
            }
        }
        self.inline_comments.retain(|row| row.row < config.height);
        self
    }

    /// Inserts bounded display-only comments without changing syntax offsets
    /// or the source-only sequence used by Vim motions.
    pub fn with_inline_comments(
        self,
        comments: &[(usize, &str)],
        width: usize,
        height: usize,
    ) -> Self {
        if comments.is_empty() || width == 0 || height == 0 {
            return self;
        }
        let mut layout = Self::default();
        let mut row = 0;
        for mut segment in self.rows {
            if row >= height {
                break;
            }
            if segment.first_segment {
                for &(_, message) in comments.iter().filter(|(line, _)| *line == segment.line) {
                    // Size cards for the whole pane, reserving a source row.
                    // Near the bottom, clip the card instead of changing its
                    // padding, wrapping, or preview truncation as it scrolls.
                    let block = inline_comment_block(message, width, height.saturating_sub(1));
                    let mut waiting_for_text = true;
                    for content in block.rows.into_iter().take(height - row) {
                        let starts_connection =
                            waiting_for_text && matches!(content, InlineCommentContent::Text(_));
                        if starts_connection {
                            waiting_for_text = false;
                        }
                        layout.inline_comments.push(InlineCommentRow {
                            line: segment.line,
                            row,
                            content,
                            starts_connection,
                            block_width: block.width,
                            text_offset: block.text_offset,
                        });
                        row += 1;
                    }
                    if row == height {
                        return layout;
                    }
                }
            }
            segment.row = row;
            layout.rows.push(segment);
            row += 1;
        }
        layout
    }

    pub fn segment_for_cursor(&self, line: usize, display_col: usize) -> Option<&LineSegment> {
        self.rows
            .iter()
            .find(|segment| segment.line == line && segment.contains_cursor_col(display_col))
            .or_else(|| self.rows.iter().rev().find(|segment| segment.line == line))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LayoutConfig {
    pub content_width: usize,
    pub height: usize,
    pub wrap: bool,
    pub vtop: usize,
    pub vleft: usize,
    pub skipcol: usize,
    pub break_indent: BreakIndentOptions,
}

pub fn layout_lines(lines: &[String], line_count: usize, config: LayoutConfig) -> DisplayLayout {
    layout_lines_with_tables(lines, line_count, config, None, None, None, None, None)
}

/// The declaration replaces the image's first row while the cursor is on it.
/// The remaining image rows stay, so the block keeps the same height.
fn image_rows_with_declaration(
    line: &str,
    line_index: usize,
    image_rows: &[LineSegment],
    show_declaration: bool,
    line_skipcol: usize,
    config: LayoutConfig,
) -> Vec<LineSegment> {
    let mut segments = Vec::new();
    if show_declaration && line_skipcol == 0 {
        segments.extend(nowrap_line_segment(
            line,
            line_index,
            config.content_width,
            config.vleft,
            config.break_indent.tab_width,
        ));
    }
    segments.extend(
        image_rows
            .iter()
            .filter(|segment| {
                let index = segment
                    .image
                    .map(|span| usize::from(span.index))
                    .unwrap_or(0);
                if show_declaration && index == 0 {
                    return false;
                }
                line_skipcol == 0 || segment.start_col >= line_skipcol
            })
            .cloned(),
    );
    segments
}

#[allow(clippy::too_many_arguments)]
pub(super) fn layout_lines_with_tables(
    lines: &[String],
    line_count: usize,
    config: LayoutConfig,
    tables: Option<&super::markdown_tables::TableLayout>,
    reveal: Option<&Range<usize>>,
    concealments: Option<&LineConcealments>,
    conceal_reveal: Option<&Range<usize>>,
    images: Option<&super::markdown_images::ImageLayout>,
) -> DisplayLayout {
    if config.content_width == 0 || config.height == 0 {
        return DisplayLayout::default();
    }
    let mut rows = Vec::with_capacity(config.height);

    // Byte offset of the current line within the viewport lines laid end to
    // end. Highlight spans from `viewport_highlight_spans` use the same
    // coordinate space.
    let mut offset = 0;
    let mut line_index = config.vtop;
    let mut row = 0;
    while row < config.height && line_index < line_count {
        let line_with_newline = lines
            .get(line_index.saturating_sub(config.vtop))
            .map(String::as_str)
            .unwrap_or_default();
        let line = trim_line_ending(line_with_newline);
        let source_offset = offset;
        offset += line_with_newline.len();

        let line_skipcol = if line_index == config.vtop {
            config.skipcol
        } else {
            0
        };
        let segments =
            if let Some(image_rows) = images.and_then(|images| images.rendered(line_index)) {
                image_rows_with_declaration(
                    line,
                    line_index,
                    image_rows,
                    images.is_some_and(|images| images.raw_lines.contains(&line_index)),
                    line_skipcol,
                    config,
                )
            } else if let Some(table) = tables.and_then(|tables| tables.line(line_index, reveal)) {
                table
                    .rows
                    .iter()
                    .filter(|segment| line_skipcol == 0 || segment.start_col >= line_skipcol)
                    .map(|segment| {
                        let mut segment = segment.clone();
                        segment.projection_left = config.vleft;
                        segment
                    })
                    .collect()
            } else if config.wrap {
                let show_source = reveal.is_some_and(|range| range.contains(&line_index))
                    || conceal_reveal.is_some_and(|range| range.contains(&line_index))
                    || images.is_some_and(|images| images.raw_lines.contains(&line_index));
                let concealment = if show_source {
                    Concealment::default()
                } else {
                    concealments
                        .map(|concealments| concealments.line(line_index))
                        .unwrap_or_default()
                };
                wrap_line_segments_with_limit(
                    line,
                    line_index,
                    config.content_width,
                    line_skipcol,
                    config.break_indent,
                    config.height - row,
                    &concealment,
                )
            } else {
                nowrap_line_segment(
                    line,
                    line_index,
                    config.content_width,
                    config.vleft,
                    config.break_indent.tab_width,
                )
            };

        for mut segment in segments {
            if row >= config.height {
                break;
            }
            segment.row = row;
            segment.source_offset += source_offset;
            rows.push(segment);
            row += 1;
        }

        line_index += 1;
    }

    DisplayLayout {
        rows,
        ..DisplayLayout::default()
    }
}

pub(super) fn wrap_concealed_line_segments(
    line: &str,
    line_index: usize,
    width: usize,
    skipcol: usize,
    break_indent: BreakIndentOptions,
    concealment: &Concealment,
) -> Vec<LineSegment> {
    wrap_line_segments_with_limit(
        line,
        line_index,
        width,
        skipcol,
        break_indent,
        usize::MAX,
        concealment,
    )
}

pub fn wrap_line_segments(
    line: &str,
    line_index: usize,
    width: usize,
    skipcol: usize,
    break_indent: BreakIndentOptions,
) -> Vec<LineSegment> {
    wrap_line_segments_with_limit(
        line,
        line_index,
        width,
        skipcol,
        break_indent,
        usize::MAX,
        &Concealment::default(),
    )
}

fn wrap_line_segments_with_limit(
    line: &str,
    line_index: usize,
    width: usize,
    skipcol: usize,
    break_indent: BreakIndentOptions,
    max_segments: usize,
    concealment: &Concealment,
) -> Vec<LineSegment> {
    let mut segments = Vec::new();
    if width == 0 || max_segments == 0 {
        return segments;
    }

    let indent = break_indent_width(line, width, break_indent);
    // Width available for text on continuation rows.
    let continuation_width = (width - indent).max(1);

    let tab_width = break_indent.tab_width.max(1);
    let mut start_col = 0;
    let mut start_grapheme = 0;
    let mut start_byte = 0;
    let mut col = 0;
    let mut visual_col = 0;
    let mut segment_visual = 0;
    let mut grapheme_count = 0;
    let mut concealed_ranges = Vec::new();

    for (grapheme_index, (byte_offset, grapheme)) in line.grapheme_indices(true).enumerate() {
        let first_segment = start_col == 0;
        let segment_width = if first_segment {
            width
        } else {
            continuation_width
        };
        let raw_width = if grapheme == "\t" {
            tab_width - (col % tab_width)
        } else {
            display_width(grapheme)
        };
        let hidden = concealment.hides_span(byte_offset, grapheme.len());
        let visual_width = if hidden { 0 } else { raw_width };
        let occupied = visual_col - segment_visual;

        // Concealed syntax has no cells, so it never opens a new screen row.
        if visual_width > 0 && occupied > 0 && occupied + visual_width > segment_width {
            let end_col = col;
            let ranges = std::mem::take(&mut concealed_ranges);
            if end_col > skipcol {
                segments.push(LineSegment {
                    line: line_index,
                    row: 0,
                    start_col,
                    end_col,
                    start_grapheme,
                    end_grapheme: grapheme_index,
                    start_byte,
                    end_byte: byte_offset,
                    start_grapheme_col: start_col,
                    source_offset: 0,
                    projection: None,
                    projection_left: 0,
                    first_segment,
                    last_segment: false,
                    visual_offset: if first_segment { 0 } else { indent },
                    concealed: ranges,
                    image: None,
                });
                if segments.len() == max_segments {
                    return segments;
                }
            }

            start_col = end_col;
            segment_visual = visual_col;
            start_grapheme = grapheme_index;
            start_byte = byte_offset;
        }

        if hidden && raw_width > 0 {
            if let Some(last) = concealed_ranges.last_mut().filter(|range| range.end == col) {
                last.end = col + raw_width;
            } else {
                concealed_ranges.push(col..col + raw_width);
            }
        }

        col += raw_width;
        visual_col += visual_width;
        grapheme_count = grapheme_index + 1;
    }

    let line_width = col;
    let first_segment = start_col == 0;
    if line_width > skipcol || (line_width == 0 && skipcol == 0) {
        segments.push(LineSegment {
            line: line_index,
            row: 0,
            start_col,
            end_col: line_width,
            start_grapheme,
            end_grapheme: grapheme_count,
            start_byte,
            end_byte: line.len(),
            start_grapheme_col: start_col,
            source_offset: 0,
            projection: None,
            projection_left: 0,
            first_segment,
            last_segment: true,
            visual_offset: if first_segment { 0 } else { indent },
            concealed: concealed_ranges,
            image: None,
        });
    }

    if segments.is_empty() {
        // skipcol points past the line's last row; align to the start of the
        // row that contains it. Row starts are 0, width, width +
        // continuation_width, width + 2 * continuation_width, ...
        let skipcol = skipcol.min(line_width);
        let start_col = if skipcol < width {
            0
        } else {
            width + ((skipcol - width) / continuation_width) * continuation_width
        };
        let first_segment = start_col == 0;
        let (start_grapheme, start_byte, start_grapheme_col) = line
            .grapheme_indices(true)
            .enumerate()
            .scan(0, |col, (index, (byte_offset, grapheme))| {
                let width = if grapheme == "\t" {
                    tab_width - (*col % tab_width)
                } else {
                    display_width(grapheme)
                };
                let grapheme_col = *col;
                *col += width;
                Some((index, byte_offset, grapheme_col, *col))
            })
            .find_map(|(index, byte_offset, grapheme_col, end_col)| {
                (end_col > start_col).then_some((index, byte_offset, grapheme_col))
            })
            .unwrap_or((grapheme_count, line.len(), line_width));

        segments.push(LineSegment {
            line: line_index,
            row: 0,
            start_col,
            end_col: start_col,
            start_grapheme,
            end_grapheme: start_grapheme,
            start_byte,
            end_byte: start_byte,
            start_grapheme_col,
            source_offset: 0,
            projection: None,
            projection_left: 0,
            first_segment,
            last_segment: start_col == line_width,
            visual_offset: if first_segment { 0 } else { indent },
            concealed: Vec::new(),
            image: None,
        });
    }

    segments
}

pub(super) fn nowrap_line_segment(
    line: &str,
    line_index: usize,
    width: usize,
    vleft: usize,
    tab_width: usize,
) -> Vec<LineSegment> {
    let tab_width = tab_width.max(1);
    let requested_end = vleft.saturating_add(width);
    let mut start = None;
    let mut end = None;
    let mut col = 0;
    let mut grapheme_count = 0;

    for (grapheme_index, (byte_offset, grapheme)) in line.grapheme_indices(true).enumerate() {
        let grapheme_width = if grapheme == "\t" {
            tab_width - (col % tab_width)
        } else {
            display_width(grapheme)
        };
        let next_col = col + grapheme_width;

        if start.is_none() && next_col > vleft {
            start = Some((grapheme_index, byte_offset, col));
        }
        if next_col > requested_end {
            end = Some((grapheme_index, byte_offset));
            break;
        }

        col = next_col;
        grapheme_count = grapheme_index + 1;
    }

    let line_width = if end.is_some() { requested_end } else { col };
    let start_col = vleft.min(line_width);
    let end_col = requested_end.min(line_width);
    let (start_grapheme, start_byte, start_grapheme_col) =
        start.unwrap_or((grapheme_count, line.len(), line_width));
    let (end_grapheme, end_byte) = end.unwrap_or((grapheme_count, line.len()));

    vec![LineSegment {
        line: line_index,
        row: 0,
        start_col,
        end_col,
        start_grapheme,
        end_grapheme,
        start_byte,
        end_byte,
        start_grapheme_col,
        source_offset: 0,
        projection: None,
        projection_left: 0,
        first_segment: true,
        last_segment: end_byte == line.len(),
        visual_offset: 0,
        concealed: Vec::new(),
        image: None,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_prediction_inserts_virtual_rows_and_preserves_source_offsets() {
        let config = LayoutConfig {
            content_width: 12,
            height: 10,
            wrap: true,
            vtop: 0,
            vleft: 0,
            skipcol: 0,
            break_indent: BreakIndentOptions::disabled(),
        };
        let lines = vec!["foo()\n".into(), "after\n".into()];
        let layout = layout_lines(&lines, 2, config).with_inline_prediction(
            0,
            "foo()",
            3,
            "bar\nnext",
            config,
        );
        assert_eq!(
            layout
                .inline_prediction
                .iter()
                .map(|row| row.text.as_str())
                .collect::<Vec<_>>(),
            vec!["foobar", "next()"]
        );
        assert_eq!(layout.row(0).unwrap().line, 0);
        assert!(layout.row(1).is_none());
        assert_eq!(layout.row(2).unwrap().line, 1);
        assert_eq!(layout.row(2).unwrap().source_offset, 6);
        assert_eq!(layout.segment_for_cursor(0, 3).unwrap().row, 0);
    }

    #[test]
    fn inline_prediction_wraps_unicode_and_keeps_later_comments_aligned() {
        let config = LayoutConfig {
            content_width: 5,
            height: 12,
            wrap: true,
            vtop: 0,
            vleft: 0,
            skipcol: 0,
            break_indent: BreakIndentOptions::disabled(),
        };
        let lines = vec!["a界z\n".into(), "after\n".into()];
        let original = layout_lines(&lines, 2, config).with_inline_comments(&[(1, "note")], 5, 12);
        let original_comment = original.inline_comments[0].row;
        let layout = original.with_inline_prediction(0, "a界z", "a界".len(), "12\n😀", config);
        assert!(layout
            .inline_prediction
            .iter()
            .all(|row| display_width(&row.text) <= 5));
        assert!(layout.inline_comments[0].row > original_comment);
        assert_eq!(layout.segment_for_cursor(0, 3).unwrap().row, 0);
        assert_eq!(layout.rows.last().unwrap().line, 1);
    }

    #[test]
    fn wraps_ascii_line_at_width() {
        let segments = wrap_line_segments("abcdef", 0, 3, 0, BreakIndentOptions::disabled());

        assert_eq!(segments.len(), 2);
        assert_eq!((segments[0].start_col, segments[0].end_col), (0, 3));
        assert_eq!((segments[1].start_col, segments[1].end_col), (3, 6));
        assert!(!segments[1].first_segment);
    }

    #[test]
    fn cursor_columns_include_only_the_actual_line_end() {
        let segments = wrap_line_segments("abcdef", 0, 3, 0, BreakIndentOptions::disabled());
        assert!(!segments[0].last_segment);
        assert!(!segments[0].contains_cursor_col(3));
        assert!(segments[1].last_segment);
        assert!(segments[1].contains_cursor_col(3));
        assert!(!segments[1].contains_display_col(6));
        assert!(segments[1].contains_cursor_col(6));
        assert!(!segments[1].contains_cursor_col(7));

        let clipped = wrap_line_segments_with_limit(
            "abcdef",
            0,
            3,
            0,
            BreakIndentOptions::disabled(),
            1,
            &Concealment::default(),
        );
        assert_eq!(clipped.len(), 1);
        assert!(!clipped[0].contains_cursor_col(3));

        let nowrap = nowrap_line_segment("abcdef", 0, 3, 0, 4);
        assert!(!nowrap[0].contains_cursor_col(3));
        let nowrap_end = nowrap_line_segment("abcdef", 0, 3, 3, 4);
        assert!(nowrap_end[0].contains_cursor_col(6));

        let unicode = wrap_line_segments("a\t界", 0, 4, 0, BreakIndentOptions::disabled());
        assert!(!unicode[0].contains_cursor_col(4));
        assert!(unicode[1].contains_cursor_col(6));
        let empty = wrap_line_segments("", 0, 3, 0, BreakIndentOptions::disabled());
        assert!(empty[0].contains_cursor_col(0));
    }

    #[test]
    fn layout_lines_does_not_render_crlf_carriage_returns() {
        let lines = vec!["abcdef\r\n".to_string()];
        let layout = layout_lines(
            &lines,
            1,
            LayoutConfig {
                content_width: 3,
                height: 3,
                wrap: true,
                vtop: 0,
                vleft: 0,
                skipcol: 0,
                break_indent: BreakIndentOptions::disabled(),
            },
        );

        assert_eq!(layout.rows.len(), 2);
        assert_eq!((layout.rows[0].start_col, layout.rows[0].end_col), (0, 3));
        assert_eq!((layout.rows[1].start_col, layout.rows[1].end_col), (3, 6));
    }

    #[test]
    fn terminal_controls_do_not_change_wrapping_columns() {
        let line = "ab\u{1b}[31mcd";
        let segments = wrap_line_segments(line, 0, 4, 0, BreakIndentOptions::disabled());

        assert_eq!(segments.len(), 2);
        assert_eq!((segments[0].start_col, segments[0].end_col), (0, 4));
        assert_eq!((segments[1].start_col, segments[1].end_col), (4, 8));
        assert_eq!(segments[0].start_byte, 0);
        assert_eq!(segments[1].end_byte, line.len());
    }

    #[test]
    fn break_indent_aligns_continuations_to_leading_whitespace() {
        // 4-space indent, line width 40, window width 30: first row holds 30
        // cols, continuations hold 30 - 4 = 26 cols starting at screen col 4.
        let line = format!("{}{}", " ".repeat(4), "x".repeat(36));
        let segments = wrap_line_segments(&line, 0, 30, 0, BreakIndentOptions::default());

        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].visual_offset, 0);
        assert_eq!((segments[0].start_col, segments[0].end_col), (0, 30));
        assert_eq!(segments[1].visual_offset, 4);
        assert_eq!((segments[1].start_col, segments[1].end_col), (30, 40));
        assert_eq!(segments[1].screen_col_for_display_col(30, 30), 4);
        assert_eq!(segments[1].screen_col_for_display_col(35, 30), 9);
    }

    #[test]
    fn break_indent_keeps_minimum_text_width() {
        // 25-space indent at width 30 would leave only 5 text columns; the
        // indent is clamped so 20 remain.
        let line = format!("{}{}", " ".repeat(25), "x".repeat(40));
        let segments = wrap_line_segments(&line, 0, 30, 0, BreakIndentOptions::default());

        assert!(segments.len() > 1);
        assert_eq!(segments[1].visual_offset, 10);
        assert_eq!(segments[1].end_col - segments[1].start_col, 20);
    }

    #[test]
    fn break_indent_expands_tabs() {
        let line = format!("\t\t{}", "x".repeat(40));
        let options = BreakIndentOptions {
            enabled: true,
            tab_width: 4,
        };
        let segments = wrap_line_segments(&line, 0, 30, 0, options);

        assert!(segments.len() > 1);
        assert_eq!(segments[1].visual_offset, 8);
    }

    #[test]
    fn break_indent_disabled_uses_full_width() {
        let line = format!("{}{}", " ".repeat(4), "x".repeat(56));
        let segments = wrap_line_segments(&line, 0, 30, 0, BreakIndentOptions::disabled());

        assert_eq!(segments.len(), 2);
        assert_eq!(segments[1].visual_offset, 0);
        assert_eq!((segments[1].start_col, segments[1].end_col), (30, 60));
    }

    #[test]
    fn break_indent_skipcol_fallback_aligns_to_row_starts() {
        // Rows: [0, 30), [30, 56), [56, 82), ... continuation width 26.
        let line = format!("{}{}", " ".repeat(4), "x".repeat(60));
        let segments = wrap_line_segments(&line, 0, 30, 70, BreakIndentOptions::default());

        assert_eq!(segments[0].start_col, 56);
        assert_eq!(segments[0].visual_offset, 4);
    }

    #[test]
    fn skipcol_starts_at_later_wrapped_segment() {
        let segments = wrap_line_segments(
            "abcdefghijklmnopqrstuvwxyz",
            0,
            10,
            10,
            BreakIndentOptions::disabled(),
        );

        assert_eq!(segments[0].start_col, 10);
        assert_eq!(segments[0].end_col, 20);
        assert!(!segments[0].first_segment);
    }

    #[test]
    fn skipcol_only_applies_to_first_viewport_line() {
        let lines = vec![
            "abcdefghijklmnopqrstuvwxyz\n".to_string(),
            "short\n".to_string(),
        ];
        let layout = layout_lines(
            &lines,
            2,
            LayoutConfig {
                content_width: 10,
                height: 3,
                wrap: true,
                vtop: 0,
                vleft: 0,
                skipcol: 10,
                break_indent: BreakIndentOptions::disabled(),
            },
        );

        assert_eq!(layout.rows[0].line, 0);
        assert_eq!(layout.rows[0].start_col, 10);
        assert_eq!(layout.rows[1].line, 0);
        assert_eq!(layout.rows[1].start_col, 20);
        assert_eq!(layout.rows[2].line, 1);
        assert_eq!(layout.rows[2].start_col, 0);
    }

    #[test]
    fn wide_grapheme_does_not_split_at_boundary() {
        let segments = wrap_line_segments("ab🙂cd", 0, 3, 0, BreakIndentOptions::disabled());

        assert_eq!(segments[0].end_col, 2);
        assert_eq!(segments[1].start_col, 2);
        assert_eq!(segments[1].end_col, 5);
    }

    #[test]
    fn wrapped_segments_preserve_tab_and_unicode_boundaries() {
        let line = "a\u{0301}\t界🙂b";
        let segments = wrap_line_segments(line, 0, 4, 0, BreakIndentOptions::disabled());

        assert_eq!(segments.len(), 3);
        assert_eq!(
            (
                segments[0].start_col,
                segments[0].end_col,
                segments[0].start_grapheme,
                segments[0].end_grapheme,
                segments[0].start_byte,
                segments[0].end_byte,
            ),
            (0, 4, 0, 2, 0, 4)
        );
        assert_eq!(
            (
                segments[1].start_col,
                segments[1].end_col,
                segments[1].start_grapheme,
                segments[1].end_grapheme,
                segments[1].start_byte,
                segments[1].end_byte,
            ),
            (4, 8, 2, 4, 4, 11)
        );
        assert_eq!(
            (
                segments[2].start_col,
                segments[2].end_col,
                segments[2].start_grapheme,
                segments[2].end_grapheme,
                segments[2].start_byte,
                segments[2].end_byte,
            ),
            (8, 9, 4, 5, 11, 12)
        );
        assert_eq!(
            &line[segments[0].start_byte..segments[0].end_byte],
            "a\u{0301}\t"
        );
        assert_eq!(&line[segments[1].start_byte..segments[1].end_byte], "界🙂");
        assert_eq!(&line[segments[2].start_byte..segments[2].end_byte], "b");
    }

    #[test]
    fn layout_limits_a_very_long_wrapped_line_to_the_viewport() {
        let lines = vec!["x".repeat(1_000_000)];
        let layout = layout_lines(
            &lines,
            1,
            LayoutConfig {
                content_width: 80,
                height: 4,
                wrap: true,
                vtop: 0,
                vleft: 0,
                skipcol: 0,
                break_indent: BreakIndentOptions::disabled(),
            },
        );

        assert_eq!(layout.rows.len(), 4);
        assert_eq!(layout.rows[0].start_byte, 0);
        assert_eq!(layout.rows[3].start_byte, 240);
        assert_eq!(layout.rows[3].end_byte, 320);
    }

    #[test]
    fn layout_limits_a_very_long_indented_line_to_the_viewport() {
        let lines = vec![format!("{}x", " ".repeat(1_000_000))];
        let layout = layout_lines(
            &lines,
            1,
            LayoutConfig {
                content_width: 80,
                height: 4,
                wrap: true,
                vtop: 0,
                vleft: 0,
                skipcol: 0,
                break_indent: BreakIndentOptions::default(),
            },
        );

        assert_eq!(layout.rows.len(), 4);
        assert_eq!(layout.rows[1].visual_offset, 60);
        assert_eq!(layout.rows[3].start_byte, 120);
        assert_eq!(layout.rows[3].end_byte, 140);
    }

    #[test]
    fn nowrap_keeps_grapheme_column_when_vleft_splits_a_tab() {
        let lines = vec!["a\t界🙂z".to_string()];
        let layout = layout_lines(
            &lines,
            1,
            LayoutConfig {
                content_width: 5,
                height: 1,
                wrap: false,
                vtop: 0,
                vleft: 2,
                skipcol: 0,
                break_indent: BreakIndentOptions::disabled(),
            },
        );

        let segment = &layout.rows[0];
        assert_eq!((segment.start_col, segment.end_col), (2, 7));
        assert_eq!((segment.start_grapheme, segment.end_grapheme), (1, 3));
        assert_eq!((segment.start_byte, segment.end_byte), (1, 5));
        assert_eq!(segment.start_grapheme_col, 1);
    }

    #[test]
    fn nowrap_only_lays_out_the_visible_prefix_of_a_very_long_line() {
        let lines = vec!["x".repeat(1_000_000)];
        let layout = layout_lines(
            &lines,
            1,
            LayoutConfig {
                content_width: 80,
                height: 1,
                wrap: false,
                vtop: 0,
                vleft: 0,
                skipcol: 0,
                break_indent: BreakIndentOptions::disabled(),
            },
        );

        assert_eq!(layout.rows.len(), 1);
        assert_eq!(layout.rows[0].start_byte, 0);
        assert_eq!(layout.rows[0].end_byte, 80);
    }

    #[test]
    fn row_indexes_the_contiguous_layout_rows() {
        let lines = vec![
            "one\n".to_string(),
            "two\n".to_string(),
            "three\n".to_string(),
        ];
        let layout = layout_lines(
            &lines,
            3,
            LayoutConfig {
                content_width: 80,
                height: 3,
                wrap: true,
                vtop: 0,
                vleft: 0,
                skipcol: 0,
                break_indent: BreakIndentOptions::disabled(),
            },
        );

        assert_eq!(layout.row(0).map(|segment| segment.line), Some(0));
        assert_eq!(layout.row(2).map(|segment| segment.line), Some(2));
        assert!(layout.row(3).is_none());
    }

    #[test]
    fn concealed_spans_wrap_and_map_cursors_by_visible_cells() {
        let line = "See [docs](https://example.com/very/long/path) now please";
        let label = line.find("docs").unwrap();
        let after = line.find(" now").unwrap();
        let hidden = Concealment::new(vec![line.find('[').unwrap()..label, label + 4..after]);
        let raw = wrap_line_segments(line, 0, 12, 0, BreakIndentOptions::disabled());
        let wrapped = wrap_line_segments_with_limit(
            line,
            0,
            12,
            0,
            BreakIndentOptions::disabled(),
            usize::MAX,
            &hidden,
        );

        assert!(wrapped.len() < raw.len());
        assert_eq!(wrapped.len(), 2);
        for segment in &wrapped {
            let visible =
                visible_columns_between(&segment.concealed, segment.start_col, segment.end_col);
            assert!(visible <= 12, "visible width {visible}");
            assert!(visible > 0);
        }
        let url = line.find("https").unwrap();
        let segment = wrapped
            .iter()
            .find(|segment| segment.contains_cursor_col(url))
            .unwrap();
        assert!(segment.hides_source_column(url));
        assert_eq!(
            segment.screen_col_for_display_col(url, 12),
            segment.screen_col_for_display_col(after, 12)
        );
        assert_eq!(segment.display_col_for_screen_col(8), after);
        assert_eq!(segment.first_visible_source_col(), 0);
        assert_eq!(segment.last_visible_source_col(), after + 3);
        for column in 0..8 {
            let source = segment.display_col_for_screen_col(column);
            assert!(!segment.hides_source_column(source), "{column} -> {source}");
            assert_eq!(segment.screen_col_for_display_col(source, 12), column);
        }
    }

    #[test]
    fn concealed_unicode_keeps_its_visible_width_and_source_bytes() {
        let line = "See [文档](https://example.com/path) end";
        let label = line.find("文档").unwrap();
        let after = line.find(" end").unwrap();
        let hidden = Concealment::new(vec![
            line.find('[').unwrap()..label,
            label + "文档".len()..after,
        ]);
        let wrapped = wrap_line_segments_with_limit(
            line,
            0,
            10,
            0,
            BreakIndentOptions::disabled(),
            usize::MAX,
            &hidden,
        );
        // "See 文档 end" is 3 + 1 + 4 + 1 + 3 = 12 visible columns, so it wraps once.
        assert_eq!(wrapped.len(), 2);
        let url_col = display_width(&line[..line.find("https").unwrap()]);
        let after_col = display_width(&line[..after]);
        let segment = wrapped
            .iter()
            .find(|segment| segment.contains_cursor_col(url_col))
            .unwrap();
        assert_eq!(
            segment.screen_col_for_display_col(url_col, 10),
            segment.screen_col_for_display_col(after_col, 10)
        );
        assert_eq!(segment.display_col_for_screen_col(8), after_col);
        assert_eq!(&line[label..label + "文档".len()], "文档");
    }
}
