//! Read-only table projections with a source position for every rendered cell.
//!
//! Parsing is cached by document revision; geometry is cached independently of
//! the viewport. Neither phase edits Markdown or participates in undo history.

use std::{
    collections::{BTreeMap, HashMap},
    ops::Range,
    sync::Arc,
};

use pulldown_cmark::{Alignment, Event, Options, Parser, Tag, TagEnd};
use unicode_segmentation::UnicodeSegmentation as _;

use super::display_layout::LineSegment;
use crate::{
    text_layout::{LayoutOptions, TextLayout},
    unicode_utils::{display_width, display_width_with_tabs, trim_line_ending},
};

/// Avoid flattening or parsing huge documents in the synchronous layout path.
pub(super) const MAX_TABLE_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct CellStyle {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: bool,
    pub chrome: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DisplayGlyph {
    pub text: String,
    pub column: usize,
    pub width: usize,
    pub style: CellStyle,
    /// Source display columns. Chrome has no selectable source characters.
    pub source: Option<Range<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourcePosition {
    source: Range<usize>,
    column: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ProjectedRow {
    pub glyphs: Vec<DisplayGlyph>,
    positions: Vec<SourcePosition>,
    pub width: usize,
}

impl ProjectedRow {
    pub fn nearest_source_column(&self, source_column: usize) -> usize {
        self.positions
            .iter()
            .map(|position| {
                source_column.clamp(position.source.start, position.source.end.saturating_sub(1))
            })
            .min_by_key(|column| column.abs_diff(source_column))
            .unwrap_or(0)
    }

    pub fn contains(&self, source_column: usize) -> bool {
        self.positions
            .iter()
            .any(|position| position.source.contains(&source_column))
    }

    pub fn column_for_source(&self, source_column: usize) -> usize {
        self.positions
            .iter()
            .min_by_key(|position| {
                if position.source.contains(&source_column) {
                    0
                } else {
                    position.source.start.abs_diff(source_column)
                }
            })
            .map_or(0, |position| position.column)
    }

    pub fn source_for_column(&self, column: usize) -> usize {
        // Prefer visible text over hidden delimiters sharing its screen cell.
        self.glyphs
            .iter()
            .filter_map(|glyph| {
                let source = glyph.source.as_ref()?;
                let distance = if (glyph.column..glyph.column + glyph.width).contains(&column) {
                    0
                } else {
                    glyph.column.abs_diff(column)
                };
                Some((distance, 0, glyph.column, source.start))
            })
            .chain(self.positions.iter().map(|position| {
                (
                    position.column.abs_diff(column),
                    1,
                    position.column,
                    position.source.start,
                )
            }))
            .min()
            .map_or(0, |(_, _, _, source)| source)
    }
}

#[derive(Debug)]
struct SourceGlyph {
    text: String,
    bytes: Range<usize>,
    style: CellStyle,
}

#[derive(Debug, Default)]
struct Cell {
    bytes: Range<usize>,
    glyphs: Vec<SourceGlyph>,
}

#[derive(Debug)]
struct TableRow {
    line: usize,
    text: String,
    cells: Vec<Cell>,
    header: bool,
}

#[derive(Debug)]
struct Table {
    lines: Range<usize>,
    alignments: Vec<Alignment>,
    rows: Vec<TableRow>,
    delimiter: String,
}

#[derive(Debug, Default)]
pub(super) struct MarkdownTables {
    tables: Vec<Table>,
}

#[derive(Debug)]
pub(super) struct TableLine {
    pub table: Range<usize>,
    pub rows: Vec<LineSegment>,
}

#[derive(Debug, Default)]
pub(super) struct TableLayout {
    pub lines: BTreeMap<usize, TableLine>,
}

#[derive(Debug)]
pub(super) struct CachedTables {
    revision: u64,
    parsed: MarkdownTables,
    layouts: HashMap<(usize, usize, bool, bool), Arc<TableLayout>>,
}

impl super::Editor {
    pub(super) fn markdown_tables_enabled(&self, window: &crate::window::Window) -> bool {
        window
            .markdown_tables
            .unwrap_or(self.config.markdown_tables.unwrap_or(true))
            && self
                .highlight_language_id_for_buffer_index(window.buffer_index)
                .as_deref()
                == Some("markdown")
    }

    pub(super) fn markdown_table_layout(
        &self,
        window: &crate::window::Window,
    ) -> Option<Arc<TableLayout>> {
        if !self.markdown_tables_enabled(window) {
            return None;
        }
        let buffer = self.buffer_manager.get(window.buffer_index)?;
        if buffer.byte_len() > MAX_TABLE_DOCUMENT_BYTES {
            return None;
        }
        let mut cache = self.markdown_table_cache.borrow_mut();
        if cache.len() >= 8 && !cache.contains_key(&buffer.id()) {
            cache.clear();
        }
        let cached = cache.entry(buffer.id()).or_insert_with(|| CachedTables {
            revision: buffer.revision(),
            parsed: MarkdownTables::parse(&buffer.contents()),
            layouts: HashMap::new(),
        });
        if cached.revision != buffer.revision() {
            *cached = CachedTables {
                revision: buffer.revision(),
                parsed: MarkdownTables::parse(&buffer.contents()),
                layouts: HashMap::new(),
            };
        }
        let key = (
            self.window_content_width(window),
            self.tab_width_for_buffer_index(window.buffer_index),
            window.wrap,
            self.config.window_borders_ascii,
        );
        if cached.layouts.len() >= 4 && !cached.layouts.contains_key(&key) {
            cached.layouts.clear();
        }
        Some(
            cached
                .layouts
                .entry(key)
                .or_insert_with(|| Arc::new(cached.parsed.layout(key.0, key.1, key.2, key.3)))
                .clone(),
        )
    }

    pub(super) fn markdown_reveal_range(
        &self,
        window: &crate::window::Window,
    ) -> Option<Range<usize>> {
        if !window.active || !self.markdown_tables_enabled(window) {
            return None;
        }
        let line = window.vtop + window.cy;
        // Multiple editing sites can span unrelated tables. Keep every caret
        // in source coordinates until the multi-cursor session ends.
        let requested = if self.has_multi_cursor_session() {
            0..self.current_buffer().len()
        } else if self.is_visual() {
            let start = self.selection_start.map_or(line, |point| point.y);
            start.min(line)..start.max(line) + 1
        } else if self.is_insert()
            || self.is_search()
            || self.pending_operator.is_some()
            || self.pending_replace
            || self.pending_character_motion.is_some()
            || self.markdown_hidden_search_match(window, line)
        {
            line..line + 1
        } else {
            return None;
        };
        // Cache and redraw only when the set of revealed tables changes.
        let layout = self.markdown_table_layout(window)?;
        let mut touched = layout.lines.range(requested);
        let first = touched.next()?.1;
        let end = touched
            .next_back()
            .map_or(first.table.end, |(_, line)| line.table.end);
        Some(first.table.start..end)
    }

    /// A search landing in concealed syntax must still show what matched.
    fn markdown_hidden_search_match(&self, window: &crate::window::Window, line: usize) -> bool {
        if self.search_highlights_suppressed || self.search_term.is_empty() {
            return false;
        }
        let Some(buffer) = self.buffer_manager.get(window.buffer_index) else {
            return false;
        };
        let Some(cache) = self.search_match_cache.as_ref().filter(|cache| {
            cache.buffer_id == buffer.id()
                && cache.revision == buffer.revision()
                && cache.pattern == self.search_term
        }) else {
            return false;
        };
        let Some(text) = buffer.get(line) else {
            return false;
        };
        let prefix = text.graphemes(true).take(window.cx).collect::<String>();
        if cache
            .matches
            .binary_search_by_key(&(line, prefix.chars().count()), |matched| {
                (matched.start_y, matched.start_x)
            })
            .is_err()
        {
            return false;
        }
        let Some(layout) = self.markdown_table_layout(window) else {
            return false;
        };
        let Some(table_line) = layout.lines.get(&line) else {
            return false;
        };
        let column = display_width_with_tabs(
            &prefix,
            self.tab_width_for_buffer_index(window.buffer_index),
        );
        !table_line.rows.iter().any(|row| {
            row.projection.as_ref().is_some_and(|projection| {
                projection.glyphs.iter().any(|glyph| {
                    glyph
                        .source
                        .as_ref()
                        .is_some_and(|source| source.contains(&column))
                })
            })
        })
    }

    pub(super) fn active_has_markdown_tables(&self) -> bool {
        self.active_window_with_editor_view()
            .and_then(|window| self.markdown_table_layout(&window))
            .is_some_and(|layout| !layout.lines.is_empty())
    }

    pub(super) fn markdown_line_segments(
        &self,
        line: usize,
        width: usize,
    ) -> Option<Vec<LineSegment>> {
        let mut window = self.active_window_with_editor_view()?;
        // The helper is also used for offscreen screen-line motions.
        let gutter = self.gutter_width_for_window(&window) + 1;
        window.size.0 = width + gutter + window.size.0.saturating_sub(window.inner_width());
        let tables = self.markdown_table_layout(&window)?;
        Some(
            tables
                .line(line, self.markdown_reveal_range(&window).as_ref())?
                .rows
                .clone(),
        )
    }

    pub(super) fn markdown_cursor_screen_row(&self) -> Option<usize> {
        let window = self.active_window_with_editor_view()?;
        let tables = self.markdown_table_layout(&window)?;
        if !tables.lines.contains_key(&self.buffer_line()) {
            return None;
        }
        self.layout_for_window(&window)
            .segment_for_cursor(self.buffer_line(), self.current_cursor_display_col())
            .map(|segment| segment.row)
    }

    pub(super) fn restore_markdown_cursor_row(&mut self, screen_row: usize) {
        let line = self.buffer_line();
        let column = self.current_cursor_display_col();
        let segments = self.wrapped_line_segments_for_width(line, self.active_content_width());
        let Some(segment) = segments
            .iter()
            .find(|segment| segment.contains_cursor_col(column))
        else {
            return;
        };
        self.vtop = line;
        self.cy = 0;
        self.skipcol = segment.start_col;
        for _ in 0..screen_row {
            if !self.scroll_wrapped_viewport_up_one_screen_line() {
                break;
            }
            self.cy = line.saturating_sub(self.vtop);
        }
    }

    pub(super) fn scroll_markdown_viewport(&mut self, down: bool, amount: usize) {
        let line = self.buffer_line();
        for _ in 0..amount {
            let moved = if down {
                self.scroll_wrapped_viewport_down_one_screen_line()
            } else {
                self.scroll_wrapped_viewport_up_one_screen_line()
            };
            self.cy = line.saturating_sub(self.vtop);
            if !moved {
                break;
            }
        }
        self.clamp_cursor_to_scrolled_viewport();
    }

    pub(super) fn ensure_markdown_cursor_visible(
        &mut self,
        line: usize,
        column: usize,
        width: usize,
    ) {
        if line < self.vtop {
            self.vtop = line;
            self.skipcol = 0;
        }
        self.cy = line.saturating_sub(self.vtop);
        if !self.visible_cursor_segment(line, column) {
            let below = line > self.vtop || column >= self.skipcol;
            for _ in 0..self.vheight().max(1) {
                let moved = if below {
                    self.scroll_wrapped_viewport_down_one_screen_line()
                } else {
                    self.scroll_wrapped_viewport_up_one_screen_line()
                };
                self.cy = line.saturating_sub(self.vtop);
                if !moved || self.visible_cursor_segment(line, column) {
                    break;
                }
            }
            if !self.visible_cursor_segment(line, column) {
                self.vtop = line;
                self.cy = 0;
                self.skipcol = self
                    .wrapped_line_segments_for_width(line, width)
                    .iter()
                    .find(|segment| segment.contains_cursor_col(column))
                    .map_or(0, |segment| segment.start_col);
            }
        }
        let projected = self
            .markdown_line_segments(line, width)
            .and_then(|rows| rows.into_iter().find(|row| row.contains_cursor_col(column)))
            .and_then(|row| row.projection.clone());
        let mut left_margin = self.sidescrolloff(width);
        let mut right_margin = left_margin;
        let horizontal = if let Some(row) = projected {
            let x = row.column_for_source(column);
            let glyph_width = row
                .glyphs
                .iter()
                .find(|glyph| (glyph.column..glyph.column + glyph.width).contains(&x))
                .map_or(1, |glyph| glyph.width);
            let indicator = usize::from(row.width > width && width >= 4);
            left_margin = left_margin.max(indicator);
            right_margin = right_margin.max(indicator + glyph_width.saturating_sub(1));
            (row.width > width).then_some(x)
        } else {
            (!self.wrap).then_some(column)
        };
        if let Some(column) = horizontal {
            if column < self.vleft + left_margin {
                self.vleft = column.saturating_sub(left_margin);
            } else if column >= (self.vleft + width).saturating_sub(right_margin) {
                self.vleft = (column + right_margin + 1).saturating_sub(width);
            }
        } else {
            self.vleft = 0;
        }
    }
}

impl TableLayout {
    pub fn line(&self, line: usize, reveal: Option<&Range<usize>>) -> Option<&TableLine> {
        let projected = self.lines.get(&line)?;
        if reveal.is_some_and(|reveal| {
            reveal.start < projected.table.end && projected.table.start < reveal.end
        }) {
            return None;
        }
        Some(projected)
    }
}

impl MarkdownTables {
    pub fn parse(source: &str) -> Self {
        let mut result = Self::default();
        let mut offsets = vec![0];
        offsets.extend(source.match_indices('\n').map(|(index, _)| index + 1));
        let line_at = |byte: usize| {
            offsets
                .partition_point(|offset| *offset <= byte)
                .saturating_sub(1)
        };
        let source_line = |line: usize| {
            trim_line_ending(
                &source[offsets[line]..offsets.get(line + 1).copied().unwrap_or(source.len())],
            )
            .to_string()
        };
        let mut table: Option<Table> = None;
        let mut row: Option<TableRow> = None;
        let mut cell: Option<Cell> = None;
        let mut styles = vec![CellStyle::default()];
        let mut container_depth = 0usize;
        let mut valid = true;
        let options = Options::ENABLE_TABLES;
        for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
            let is_code = matches!(event, Event::Code(_));
            match event {
                Event::Start(Tag::BlockQuote(_) | Tag::List(_) | Tag::Item) => container_depth += 1,
                Event::End(TagEnd::BlockQuote(_) | TagEnd::List(_) | TagEnd::Item) => {
                    container_depth = container_depth.saturating_sub(1)
                }
                Event::Start(Tag::Table(alignments)) if container_depth == 0 => {
                    let start = line_at(range.start);
                    let end = line_at(range.end.saturating_sub(1)) + 1;
                    table = Some(Table {
                        lines: start..end,
                        alignments,
                        rows: Vec::new(),
                        delimiter: source_line(start + 1),
                    });
                    valid = true;
                }
                Event::Start(Tag::TableHead | Tag::TableRow) if table.is_some() => {
                    let line = line_at(range.start);
                    row = Some(TableRow {
                        line,
                        text: source_line(line),
                        cells: Vec::new(),
                        header: matches!(event, Event::Start(Tag::TableHead)),
                    });
                }
                Event::Start(Tag::TableCell) if row.is_some() => {
                    cell = Some(Cell::default());
                    styles.truncate(1);
                    styles[0] = CellStyle {
                        bold: row.as_ref().is_some_and(|row| row.header),
                        ..CellStyle::default()
                    };
                }
                Event::Start(tag) if cell.is_some() => {
                    let mut style = *styles.last().expect("base cell style exists");
                    match tag {
                        Tag::Strong => style.bold = true,
                        Tag::Emphasis => style.italic = true,
                        Tag::Link { .. } => style.link = true,
                        _ => {}
                    }
                    styles.push(style);
                }
                Event::End(TagEnd::TableCell) => {
                    if let (Some(row), Some(mut cell)) = (&mut row, cell.take()) {
                        merge_cell_graphemes(&mut cell);
                        row.cells.push(cell);
                    }
                }
                Event::End(TagEnd::TableHead | TagEnd::TableRow) => {
                    if let (Some(table), Some(mut row)) = (&mut table, row.take()) {
                        let ranges = pipe_cells(&row.text);
                        // GFM drops extra body cells. Keep such rows in source so
                        // looking at a malformed table never hides their contents.
                        if ranges.len() > table.alignments.len() {
                            valid = false;
                        }
                        row.cells.resize_with(table.alignments.len(), Cell::default);
                        for (index, cell) in row.cells.iter_mut().enumerate() {
                            cell.bytes = ranges
                                .get(index)
                                .cloned()
                                .unwrap_or(row.text.len()..row.text.len());
                        }
                        table.rows.push(row);
                    }
                }
                Event::End(TagEnd::Table) => {
                    if let Some(table) =
                        table.take().filter(|table| valid && !table.rows.is_empty())
                    {
                        result.tables.push(table);
                    }
                }
                Event::End(_) if cell.is_some() => {
                    if styles.len() > 1 {
                        styles.pop();
                    }
                }
                Event::Text(text)
                | Event::Code(text)
                | Event::InlineHtml(text)
                | Event::Html(text)
                    if cell.is_some() =>
                {
                    let Some(row) = &row else {
                        continue;
                    };
                    let mut style = *styles.last().expect("base cell style exists");
                    style.code |= is_code;
                    let line_start = offsets[row.line];
                    let mut next = range.start;
                    for grapheme in text.graphemes(true) {
                        let found = source[next..range.end]
                            .find(grapheme)
                            .map(|offset| next + offset);
                        let bytes = if let Some(start) = found {
                            next = start + grapheme.len();
                            start..next
                        } else {
                            // Entities and normalized code whitespace can have
                            // several source characters for one visible grapheme.
                            let original = next..range.end;
                            next = range.end;
                            original
                        };
                        cell.as_mut()
                            .expect("table cell exists")
                            .glyphs
                            .push(SourceGlyph {
                                text: if grapheme == "\t" {
                                    " ".to_string()
                                } else {
                                    grapheme.to_string()
                                },
                                bytes: bytes.start.saturating_sub(line_start)
                                    ..bytes.end.saturating_sub(line_start),
                                style,
                            });
                    }
                }
                _ => {}
            }
        }
        result
    }

    pub fn layout(&self, width: usize, tab_width: usize, wrap: bool, ascii: bool) -> TableLayout {
        let mut layout = TableLayout::default();
        for table in &self.tables {
            let columns = table.alignments.len();
            if columns == 0 || width == 0 {
                continue;
            }
            let preferred = (0..columns)
                .map(|column| {
                    table
                        .rows
                        .iter()
                        .map(|row| {
                            row.cells[column]
                                .glyphs
                                .iter()
                                .map(|glyph| display_width(&glyph.text))
                                .sum::<usize>()
                        })
                        .max()
                        .unwrap_or(0)
                        .max(3)
                })
                .collect::<Vec<_>>();
            let minimum = vec![3; columns];
            let overhead = columns * 3 + 1;
            let widths = if wrap {
                crate::text_layout::fit_column_widths(
                    &preferred,
                    &minimum,
                    width.saturating_sub(overhead),
                )
                .unwrap_or(minimum)
            } else {
                preferred
            };
            for row in &table.rows {
                let rows = project_row(row, &widths, &table.alignments, tab_width, ascii);
                layout.lines.insert(
                    row.line,
                    TableLine {
                        table: table.lines.clone(),
                        rows,
                    },
                );
            }
            let line = table.lines.start + 1;
            let rows = vec![project_delimiter(
                line,
                &table.delimiter,
                &widths,
                tab_width,
                ascii,
            )];
            layout.lines.insert(
                line,
                TableLine {
                    table: table.lines.clone(),
                    rows,
                },
            );
        }
        layout
    }
}

/// Split only after a real parser recognized a table. Escaped pipes also stay
/// inside code spans in GFM; an unescaped pipe remains a delimiter there.
fn pipe_cells(line: &str) -> Vec<Range<usize>> {
    let mut cells = Vec::new();
    let mut start = 0;
    let mut escaped = false;
    for (index, ch) in line.char_indices() {
        if ch == '|' && !escaped {
            cells.push(start..index);
            start = index + 1;
        }
        escaped = ch == '\\' && !escaped;
    }
    cells.push(start..line.len());
    if cells.len() > 1 && line[cells[0].clone()].trim().is_empty() {
        cells.remove(0);
    }
    if cells.len() > 1
        && line[cells.last().expect("pipe cells are nonempty").clone()]
            .trim()
            .is_empty()
    {
        cells.pop();
    }
    cells
}

/// Inline markup can split one displayed grapheme across parser events (for
/// example a base letter and an emphasized combining accent). Normalize those
/// boundaries before pairing glyphs with TextLayout's grapheme positions.
fn merge_cell_graphemes(cell: &mut Cell) {
    let text = cell
        .glyphs
        .iter()
        .map(|glyph| glyph.text.as_str())
        .collect::<String>();
    let mut source_index = 0;
    let mut source_offset = 0;
    let mut merged = Vec::new();
    for (offset, grapheme) in text.grapheme_indices(true) {
        let first = &cell.glyphs[source_index];
        let mut bytes = first.bytes.clone();
        let mut style = first.style;
        while source_index < cell.glyphs.len() && source_offset < offset + grapheme.len() {
            let glyph = &cell.glyphs[source_index];
            bytes.end = glyph.bytes.end;
            style.bold |= glyph.style.bold;
            style.italic |= glyph.style.italic;
            style.code |= glyph.style.code;
            style.link |= glyph.style.link;
            source_offset += glyph.text.len();
            source_index += 1;
        }
        merged.push(SourceGlyph {
            text: grapheme.to_string(),
            bytes,
            style,
        });
    }
    cell.glyphs = merged;
}

struct SourceColumns(Vec<(usize, usize)>);

impl SourceColumns {
    fn new(text: &str, tab_width: usize) -> Self {
        let mut columns = Vec::new();
        let mut column = 0;
        for (byte, grapheme) in text.grapheme_indices(true) {
            columns.push((byte, column));
            column += if grapheme == "\t" {
                tab_width.max(1) - column % tab_width.max(1)
            } else {
                display_width(grapheme)
            };
        }
        columns.push((text.len(), column));
        Self(columns)
    }

    fn grapheme_count(&self) -> usize {
        // One entry per source grapheme, followed by the end-of-line sentinel.
        self.0.len() - 1
    }

    fn at(&self, byte: usize) -> usize {
        self.0[self
            .0
            .partition_point(|(start, _)| *start <= byte)
            .saturating_sub(1)]
        .1
    }
}

fn chrome(text: &str, column: usize) -> DisplayGlyph {
    DisplayGlyph {
        text: text.to_string(),
        column,
        width: display_width(text),
        source: None,
        style: CellStyle {
            chrome: true,
            ..CellStyle::default()
        },
    }
}

fn project_row(
    row: &TableRow,
    widths: &[usize],
    alignments: &[Alignment],
    tab_width: usize,
    ascii: bool,
) -> Vec<LineSegment> {
    let source_columns = SourceColumns::new(&row.text, tab_width);
    let cells = row
        .cells
        .iter()
        .zip(widths)
        .map(|(cell, width)| {
            let text = cell
                .glyphs
                .iter()
                .map(|glyph| glyph.text.as_str())
                .collect::<String>();
            TextLayout::new(&text, LayoutOptions::word(*width))
        })
        .collect::<Vec<_>>();
    let height = cells
        .iter()
        // TextLayout reserves a trailing caret row when text exactly fills
        // its width. A read-only grid only needs rows containing cell text.
        .map(|cell| {
            cell.rows()
                .iter()
                .rposition(|row| !row.text.is_empty())
                .map_or(1, |index| index + 1)
        })
        .max()
        .unwrap_or(1)
        .max(1);
    let total_width = widths.iter().sum::<usize>() + widths.len() * 3 + 1;
    let mut projected = vec![
        ProjectedRow {
            width: total_width,
            ..ProjectedRow::default()
        };
        height
    ];
    let vertical = if ascii { "|" } else { "│" };
    for row in &mut projected {
        row.glyphs.push(chrome(vertical, 0));
    }
    let mut column = 2;
    for (index, (cell, cell_layout)) in row.cells.iter().zip(&cells).enumerate() {
        let shifts = (0..height)
            .map(|line| {
                let used = cell_layout
                    .rows()
                    .get(line)
                    .map_or(0, |row| display_width(&row.text));
                let padding = widths[index].saturating_sub(used);
                column
                    + match alignments[index] {
                        Alignment::Right => padding,
                        Alignment::Center => padding / 2,
                        _ => 0,
                    }
            })
            .collect::<Vec<_>>();
        for (offset, glyph) in cell.glyphs.iter().enumerate() {
            let position = cell_layout.positions()[offset];
            let next = cell_layout.positions()[offset + 1];
            if position.row >= height
                || position == next
                || (position.row != next.row && glyph.text.trim().is_empty())
            {
                continue;
            }
            projected[position.row].glyphs.push(DisplayGlyph {
                text: glyph.text.clone(),
                column: shifts[position.row] + position.column,
                width: display_width(&glyph.text),
                style: glyph.style,
                source: Some(
                    source_columns.at(glyph.bytes.start)..source_columns.at(glyph.bytes.end),
                ),
            });
        }
        // Keep pipes and trimmed whitespace on the grid's borders and padding.
        // Concealed inline syntax can share a screen cell with visible text.
        let cell_source = &row.text[cell.bytes.clone()];
        let content_start = cell.bytes.end - cell_source.trim_start().len();
        let content_end = cell.bytes.start + cell_source.trim_end().len();
        let left_border = column - 2;
        let right_border = column + widths[index] + 1;
        let start = if index == 0 { 0 } else { cell.bytes.start };
        let end = row
            .cells
            .get(index + 1)
            .map_or(row.text.len(), |next| next.bytes.start);
        for (offset, grapheme) in row.text[start..end].grapheme_indices(true) {
            let byte = start + offset;
            let glyph_index = nearest_glyph(&cell.glyphs, byte);
            let position = cell_layout
                .positions()
                .get(glyph_index)
                .copied()
                .unwrap_or_default();
            let screen_row = position.row.min(height - 1);
            let source_start = source_columns.at(byte);
            let source_end = source_columns
                .at(byte + grapheme.len())
                .max(source_start + 1);
            let text_column = shifts[screen_row] + position.column;
            let screen_column = if byte < cell.bytes.start {
                left_border
            } else if byte >= cell.bytes.end {
                right_border
            } else if byte < content_start {
                text_column.saturating_sub(1).max(left_border + 1)
            } else if byte >= content_end {
                let glyph_width = cell
                    .glyphs
                    .get(glyph_index)
                    .map_or(0, |glyph| display_width(&glyph.text));
                (text_column + glyph_width).min(right_border - 1)
            } else {
                text_column
            };
            projected[screen_row].positions.push(SourcePosition {
                source: source_start..source_end,
                column: screen_column,
            });
        }
        if cell.glyphs.is_empty() {
            projected[0].glyphs.push(DisplayGlyph {
                text: String::new(),
                column: shifts[0],
                width: 0,
                style: CellStyle::default(),
                source: Some(
                    source_columns.at(cell.bytes.start)..source_columns.at(cell.bytes.end),
                ),
            });
        }
        column += widths[index] + 3;
        for row in &mut projected {
            row.glyphs.push(chrome(vertical, column - 2));
        }
    }
    let end = display_width_with_tabs(&row.text, tab_width);
    if let Some(position) = projected
        .iter()
        .flat_map(|row| &row.positions)
        .max_by_key(|position| position.source.start)
        .cloned()
    {
        if let Some(row) = projected
            .iter_mut()
            .find(|row| row.positions.contains(&position))
        {
            row.positions.push(SourcePosition {
                source: end..end + 1,
                column: position.column,
            });
        }
    }
    let count = projected.len();
    let source_graphemes = source_columns.grapheme_count();
    projected
        .into_iter()
        .enumerate()
        .map(|(index, projection)| {
            projected_segment(
                row.line,
                &row.text,
                source_graphemes,
                projection,
                index,
                count,
            )
        })
        .collect()
}

fn nearest_glyph(glyphs: &[SourceGlyph], byte: usize) -> usize {
    let next = glyphs.partition_point(|glyph| glyph.bytes.end <= byte);
    let previous = next.saturating_sub(1);
    if let Some(glyph) = glyphs.get(next) {
        if glyph.bytes.contains(&byte)
            || next == 0
            || glyph.bytes.start.abs_diff(byte) < byte.saturating_sub(glyphs[previous].bytes.end)
        {
            return next;
        }
    }
    previous
}

fn project_delimiter(
    line: usize,
    text: &str,
    widths: &[usize],
    tab_width: usize,
    ascii: bool,
) -> LineSegment {
    let source_columns = SourceColumns::new(text, tab_width);
    let (left, middle, right, horizontal) = if ascii {
        ("+", "+", "+", "-")
    } else {
        ("├", "┼", "┤", "─")
    };
    let mut projected = ProjectedRow::default();
    projected.glyphs.push(chrome(left, 0));
    let mut column = 1;
    for (index, width) in widths.iter().enumerate() {
        projected
            .glyphs
            .push(chrome(&horizontal.repeat(width + 2), column));
        column += width + 2;
        projected.glyphs.push(chrome(
            if index + 1 == widths.len() {
                right
            } else {
                middle
            },
            column,
        ));
        column += 1;
    }
    projected.width = column;
    let ranges = pipe_cells(text);
    let mut x = 2;
    for (index, range) in ranges.iter().enumerate() {
        let left_border = x - 2;
        let right_border = x + widths.get(index).copied().unwrap_or(0) + 1;
        let start = if index == 0 { 0 } else { range.start };
        let end = ranges
            .get(index + 1)
            .map_or(text.len(), |range| range.start);
        for (offset, grapheme) in text[start..end].grapheme_indices(true) {
            let byte = start + offset;
            let source = source_columns.at(byte)..source_columns.at(byte + grapheme.len());
            let column = if byte < range.start {
                left_border
            } else if byte >= range.end {
                right_border
            } else {
                (left_border + 1 + source.start - source_columns.at(range.start))
                    .min(right_border - 1)
            };
            projected.positions.push(SourcePosition { source, column });
        }
        x += widths.get(index).copied().unwrap_or(0) + 3;
    }
    let end = display_width_with_tabs(text, tab_width);
    projected.positions.push(SourcePosition {
        source: end..end + 1,
        column: column.saturating_sub(2),
    });
    projected_segment(line, text, source_columns.grapheme_count(), projected, 0, 1)
}

fn projected_segment(
    line: usize,
    text: &str,
    source_graphemes: usize,
    projection: ProjectedRow,
    index: usize,
    count: usize,
) -> LineSegment {
    let start_col = projection
        .positions
        .iter()
        .map(|position| position.source.start)
        .min()
        .unwrap_or(0);
    let end_col = projection
        .positions
        .iter()
        .map(|position| position.source.end)
        .max()
        .unwrap_or(0);
    LineSegment {
        line,
        row: index,
        start_col,
        end_col,
        start_grapheme: 0,
        end_grapheme: source_graphemes,
        start_byte: 0,
        end_byte: text.len(),
        start_grapheme_col: 0,
        source_offset: 0,
        first_segment: index == 0,
        last_segment: index + 1 == count,
        visual_offset: 0,
        projection: Some(Arc::new(projection)),
        projection_left: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        buffer::Buffer,
        config::Config,
        editor::{render_buffer::RenderBuffer, Action, Editor, Mode},
        theme::{Style, Theme},
    };
    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    fn editor(source: &str, width: usize, height: usize) -> Editor {
        let mut config: Config = toml::from_str(include_str!("../../default_config.toml")).unwrap();
        config.lsp.servers.clear();
        config.clipboard.enabled = false;
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            lsp,
            width,
            height,
            config,
            Theme::default(),
            vec![Buffer::new(
                Some("/tmp/red-pretty-tables-test.md".into()),
                source.into(),
            )],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        editor
    }

    fn render(editor: &mut Editor) -> String {
        let mut screen = RenderBuffer::new(
            editor.size.0 as usize,
            editor.size.1 as usize,
            &Style::default(),
        );
        editor.render(&mut screen).unwrap();
        screen
            .cells
            .chunks(screen.width)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn projected(editor: &Editor, line: usize) -> bool {
        let window = editor.active_window_with_editor_view().unwrap();
        editor
            .layout_for_window(&window)
            .rows
            .iter()
            .any(|row| row.line == line && row.projection.is_some())
    }

    async fn key(editor: &mut Editor, code: KeyCode) {
        editor
            .test_execute_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
            .await
            .unwrap();
    }

    fn plain(segment: &LineSegment) -> String {
        let row = segment.projection.as_ref().expect("projected table row");
        let mut cells = vec![" ".to_string(); row.width];
        for glyph in &row.glyphs {
            let mut column = glyph.column;
            for grapheme in glyph.text.graphemes(true) {
                let width = display_width(grapheme);
                cells[column] = grapheme.to_string();
                for cell in &mut cells[column + 1..column + width] {
                    cell.clear();
                }
                column += width;
            }
        }
        cells.concat()
    }

    #[test]
    fn cursor_motion_repaints_the_same_table_as_a_full_frame() {
        let source = "Before\n\n| Name | Description |\n| --- | ---: |\n| **界🙂** | a longer [link](https://example.test) with `code` |\n| Apples | Fresh fruit |\n\nAfter\n";
        for (width, wrap, ascii) in [
            (100, true, false),
            (40, true, false),
            (40, true, true),
            (40, false, false),
        ] {
            let mut editor = editor(source, width, 30);
            editor.wrap = wrap;
            editor.config.window_borders_ascii = ascii;
            let mut frame = RenderBuffer::new(width, 30, &Style::default());
            editor.render(&mut frame).unwrap();

            // Exercise the partial repaint directly: tests disable terminal I/O,
            // which otherwise makes ordinary key events choose a full frame.
            for line in [2, 3, 4, 5, 4, 3, 2, 1] {
                let columns = source.lines().nth(line).unwrap().graphemes(true).count();
                for column in (0..columns.max(1)).chain((0..columns).rev()) {
                    editor.cy = line - editor.vtop;
                    editor.cx = column;
                    editor.refresh_cursor_goal();
                    editor.render_cursor_motion_delta(&mut frame).unwrap();

                    let mut full = RenderBuffer::new(width, 30, &Style::default());
                    editor.render(&mut full).unwrap();
                    for (position, (actual, expected)) in
                        frame.cells.iter().zip(&full.cells).enumerate()
                    {
                        assert_eq!(
                            actual, expected,
                            "cell {position}, source {line}:{column}, width {width}, wrap {wrap}, ascii {ascii}"
                        );
                    }
                }
            }
            assert_eq!(editor.current_buffer().contents(), source);
        }
    }

    #[test]
    fn aligns_columns_without_rewriting_source_spacing() {
        let source = "| Item | Count |\n| --- | ---: |\n| Apples|12 |\n|Oranges | 3|\n";
        let tables = MarkdownTables::parse(source);
        let layout = tables.layout(80, 4, true, false);
        assert_eq!(layout.lines.len(), 4);
        assert!(layout.lines.values().all(|line| line.rows.len() == 1));
        assert_eq!(plain(&layout.lines[&0].rows[0]), "│ Item    │ Count │");
        assert_eq!(plain(&layout.lines[&1].rows[0]), "├─────────┼───────┤");
        assert_eq!(plain(&layout.lines[&2].rows[0]), "│ Apples  │    12 │");
        assert_eq!(plain(&layout.lines[&3].rows[0]), "│ Oranges │     3 │");
    }

    #[test]
    fn borders_and_padding_are_reachable_by_keyboard_and_mouse() {
        let source = "| A | B |\n| --- | --- |\n| Cat | Dog |\n";
        let layout = MarkdownTables::parse(source).layout(60, 4, true, false);
        for (line, columns) in [
            (0, vec![0, 1, 2, 3, 6, 7, 8, 9, 12]),
            (1, (0..13).collect()),
            (2, (0..13).collect()),
        ] {
            let row = &layout.lines[&line].rows[0];
            for (source_column, screen_column) in columns.into_iter().enumerate() {
                assert_eq!(
                    row.screen_col_for_display_col(source_column, 60),
                    screen_column,
                    "keyboard position on line {line}, column {source_column}"
                );
                assert_eq!(
                    row.display_col_for_screen_col(screen_column),
                    source_column,
                    "mouse position on line {line}, column {screen_column}"
                );
            }
        }
    }

    #[test]
    fn reveals_entire_intersecting_table_only() {
        let source = "A | B\n--- | ---\none | two\n\nC | D\n--- | ---\nthree | four\n";
        let layout = MarkdownTables::parse(source).layout(40, 4, true, false);
        assert!(layout.line(0, Some(&(2..3))).is_none());
        assert!(layout.line(1, Some(&(2..3))).is_none());
        assert!(layout.line(4, Some(&(2..3))).is_some());
        assert!(layout.line(6, Some(&(2..6))).is_none());
    }

    #[test]
    fn ignores_fences_containers_and_malformed_headers() {
        for source in [
            "```md\nA | B\n--- | ---\nx | y\n```\n",
            "> A | B\n> --- | ---\n> x | y\n",
            "A | B\n---\nx | y\n",
            "A | B\n--- | ---\nx | y | hidden data\n",
        ] {
            assert!(
                MarkdownTables::parse(source).tables.is_empty(),
                "{source:?}"
            );
        }
    }

    #[test]
    fn maps_wrapped_unicode_cells_and_hidden_markup_to_source() {
        let source = "Name | Description\n--- | ---\n**界🙂** | a longer [link](https://example.com) with `a\\|b` &amp; text\n";
        let layout = MarkdownTables::parse(source).layout(25, 4, true, false);
        let rows = &layout.lines[&2].rows;
        assert!(rows.len() > 1);
        for row in rows {
            assert!(display_width(&plain(row)) <= 25, "{}", plain(row));
        }
        let source_line = source.lines().nth(2).unwrap();
        let columns = SourceColumns::new(source_line, 4);
        for (byte, _) in source_line.grapheme_indices(true) {
            let column = columns.at(byte);
            assert_eq!(
                rows.iter()
                    .filter(|row| row.contains_cursor_col(column))
                    .count(),
                1,
                "unmapped or ambiguous source column {column}"
            );
        }
        let visible = rows.iter().map(plain).collect::<Vec<_>>().join("\n");
        assert!(visible.contains("界🙂"), "{visible}");
        assert!(visible.contains("a|b"), "{visible}");
        assert!(!visible.contains("https"), "{visible}");
        assert!(!visible.contains("&amp;"), "{visible}");
        for row in rows {
            let projection = row.projection.as_ref().unwrap();
            for glyph in projection
                .glyphs
                .iter()
                .filter(|glyph| glyph.source.is_some() && glyph.width > 0)
            {
                assert_eq!(
                    row.display_col_for_screen_col(glyph.column),
                    glyph.source.as_ref().unwrap().start,
                    "hit on {:?}",
                    glyph.text
                );
            }
        }
    }

    #[test]
    fn long_wrapped_unicode_rows_preserve_source_ranges_after_resizing() {
        let source_line = format!("| {} | tail |", "e\u{301} 界🙂 ".repeat(1024));
        let source = format!("| Value | Note |\n| --- | --- |\n{source_line}\n");
        let tables = MarkdownTables::parse(&source);
        let source_graphemes = source_line.graphemes(true).count();
        let last_emoji = source_line.rfind('🙂').unwrap();
        let source_column = display_width(&source_line[..last_emoji]);

        for width in [24, 80, 40] {
            let layout = tables.layout(width, 4, true, false);
            let rows = &layout.lines[&2].rows;
            assert!(
                rows.len() > 32,
                "expected many wrapped rows at width {width}"
            );
            for row in rows {
                assert_eq!(
                    (row.start_grapheme, row.end_grapheme),
                    (0, source_graphemes),
                    "source grapheme range at width {width}, row {}",
                    row.row
                );
                assert_eq!((row.start_byte, row.end_byte), (0, source_line.len()));
            }

            let mut matching = rows
                .iter()
                .filter(|row| row.contains_cursor_col(source_column));
            let row = matching
                .next()
                .expect("the last emoji has a source mapping");
            assert!(matching.next().is_none(), "source mapping must be unique");
            let screen_column = row.screen_col_for_display_col(source_column, width);
            assert_eq!(row.display_col_for_screen_col(screen_column), source_column);
        }
    }

    #[test]
    fn keeps_missing_cells_empty_and_handles_crlf_and_ascii_borders() {
        let source = "| A | B |\r\n| --- | --- |\r\n| one |\r\n";
        let layout = MarkdownTables::parse(source).layout(40, 4, true, true);
        assert_eq!(plain(&layout.lines[&2].rows[0]), "| one |     |");
        assert_eq!(plain(&layout.lines[&1].rows[0]), "+-----+-----+");
    }

    #[test]
    fn joins_graphemes_split_by_inline_markup() {
        let source = "A | B\n--- | ---\na**\u{301}** | 👩**\u{200d}**💻\n";
        let layout = MarkdownTables::parse(source).layout(30, 4, true, false);
        let rendered = plain(&layout.lines[&2].rows[0]);
        assert!(rendered.contains("a\u{301}"), "{rendered}");
        assert!(rendered.contains("👩\u{200d}💻"), "{rendered}");
    }

    #[tokio::test]
    async fn search_reveals_hidden_link_target_and_yank_copies_source() {
        let source = "Name | Value\n--- | ---\n[label](https://example.test) | text\n";
        let mut editor = editor(source, 70, 15);
        render(&mut editor);
        key(&mut editor, KeyCode::Char('/')).await;
        for ch in "example".chars() {
            key(&mut editor, KeyCode::Char(ch)).await;
        }
        key(&mut editor, KeyCode::Enter).await;
        assert!(
            !projected(&editor, 2),
            "the hidden search result must be readable: mode={:?}, term={:?}, cursor=({}, {}), cache={:?}",
            editor.mode, editor.search_term, editor.cx, editor.buffer_line(), editor.search_match_cache
        );
        assert!(render(&mut editor).contains("https://example.test"));
        key(&mut editor, KeyCode::Char('0')).await;
        assert!(projected(&editor, 2));
        key(&mut editor, KeyCode::Char('V')).await;
        key(&mut editor, KeyCode::Char('y')).await;
        assert_eq!(
            editor.registers[&'"'].text,
            "[label](https://example.test) | text\n"
        );
        assert_eq!(editor.current_buffer().contents(), source);
    }

    #[tokio::test]
    async fn revealing_near_document_start_keeps_cursor_visible() {
        let source = format!("A | B\n--- | ---\n{} | tail\n", "words ".repeat(45));
        let mut editor = editor(&source, 36, 12);
        editor.cy = 2;
        editor.cx = 95;
        render(&mut editor);
        for mode in [Mode::Insert, Mode::Normal] {
            editor
                .test_execute_production_action(Action::EnterMode(mode))
                .await
                .unwrap();
            assert!(editor.render_cursor_position().is_some());
        }
        assert_eq!(editor.current_buffer().contents(), source);
    }

    #[tokio::test]
    async fn editor_mode_changes_preserve_bytes_revision_dirty_state_and_undo() {
        let source = "Before\n\n| Item | Count |\n|---|---:|\n|Apples|12|\n\nAfter\n";
        let mut editor = editor(source, 60, 20);
        editor.cy = 4;
        editor.cx = 2;
        editor.refresh_cursor_goal();
        assert!(render(&mut editor).contains("│ Apples"));
        let revision = editor.current_buffer().revision();
        let dirty = editor.current_buffer().is_dirty();
        let undo = editor.test_undo_tree().len();
        for mode in [Mode::Insert, Mode::Normal, Mode::Visual, Mode::Normal] {
            editor
                .test_execute_production_action(Action::EnterMode(mode))
                .await
                .unwrap();
            render(&mut editor);
            assert_eq!(projected(&editor, 2), mode == Mode::Normal);
            assert_eq!(editor.current_buffer().contents(), source);
            assert_eq!(editor.current_buffer().revision(), revision);
            assert_eq!(editor.current_buffer().is_dirty(), dirty);
            assert_eq!(editor.test_undo_tree().len(), undo);
        }
        editor
            .test_execute_production_action(Action::EnterMode(Mode::Insert))
            .await
            .unwrap();
        editor
            .test_execute_production_action(Action::InsertCharAtCursorPos('X'))
            .await
            .unwrap();
        editor
            .test_execute_production_action(Action::EnterMode(Mode::Normal))
            .await
            .unwrap();
        assert!(editor.current_buffer().contents().contains("AXpples"));
        editor
            .test_execute_production_action(Action::Undo)
            .await
            .unwrap();
        assert_eq!(editor.current_buffer().contents(), source);
        assert!(projected(&editor, 4));
    }

    #[tokio::test]
    async fn pending_operators_and_replacement_reveal_source() {
        let source = "A | B\n--- | ---\napple | pear\n";
        let mut editor = editor(source, 60, 15);
        editor.cy = 2;
        editor.cx = 0;
        render(&mut editor);
        key(&mut editor, KeyCode::Char('r')).await;
        assert!(!projected(&editor, 0));
        key(&mut editor, KeyCode::Esc).await;
        assert!(projected(&editor, 0));
        key(&mut editor, KeyCode::Char('d')).await;
        assert!(!projected(&editor, 0));
        key(&mut editor, KeyCode::Char('w')).await;
        assert!(!editor.current_buffer().contents().contains("apple"));
        editor
            .test_execute_production_action(Action::Undo)
            .await
            .unwrap();
        assert_eq!(editor.current_buffer().contents(), source);
    }

    #[tokio::test]
    async fn multiple_cursors_reveal_all_tables_until_cleared() {
        let source = "A | B\n--- | ---\napple | pear\n\nC | D\n--- | ---\napple | plum\n";
        let mut editor = editor(source, 60, 20);
        editor.cy = 2;
        render(&mut editor);
        for _ in 0..2 {
            editor
                .test_execute_production_action(Action::SelectNextOccurrence)
                .await
                .unwrap();
        }
        assert!(editor.has_multi_cursor_session());
        assert!(!projected(&editor, 0));
        assert!(!projected(&editor, 4));
        editor
            .test_execute_production_action(Action::ClearMultiSelection)
            .await
            .unwrap();
        render(&mut editor);
        assert!(projected(&editor, 4));
        assert_eq!(editor.current_buffer().contents(), source);
    }

    #[tokio::test]
    async fn mouse_click_uses_rendered_cell_position() {
        let source = "A | B\n--- | ---:\nshort | 12345\n";
        let mut editor = editor(source, 60, 15);
        render(&mut editor);
        let window = editor.active_window_with_editor_view().unwrap();
        let layout = editor.layout_for_window(&window);
        let row = layout.rows.iter().find(|row| row.line == 2).unwrap();
        let glyph = row
            .projection
            .as_ref()
            .unwrap()
            .glyphs
            .iter()
            .find(|glyph| glyph.text == "3")
            .unwrap();
        let x = window.position.x + editor.gutter_width_for_window(&window) + 1 + glyph.column;
        let y = editor.window_to_terminal_y(&window, row.row);
        editor
            .test_execute_event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x as u16,
                row: y as u16,
                modifiers: KeyModifiers::NONE,
            }))
            .await
            .unwrap();
        assert_eq!(editor.buffer_line(), 2);
        assert_eq!(editor.cx, 10, "click should hit source digit 3");
        let clicked_position = editor.render_cursor_position();
        key(&mut editor, KeyCode::Char('h')).await;
        assert_eq!(editor.cx, 9);
        assert_eq!(editor.render_cursor_position(), Some((x - 1, y)));
        key(&mut editor, KeyCode::Char('l')).await;
        assert_eq!(editor.cx, 10);
        assert_eq!(editor.render_cursor_position(), clicked_position);
        for motion in ['k', 'j'] {
            key(&mut editor, KeyCode::Char(motion)).await;
        }
        assert_eq!(editor.render_cursor_position(), clicked_position);
    }

    #[tokio::test]
    async fn first_mouse_drag_maps_before_revealing_source() {
        let source = "A | B\n--- | ---:\nshort | 12345\n";
        let mut editor = editor(source, 60, 15);
        render(&mut editor);
        let window = editor.active_window_with_editor_view().unwrap();
        let layout = editor.layout_for_window(&window);
        let row = layout.rows.iter().find(|row| row.line == 2).unwrap();
        let projection = row.projection.as_ref().unwrap();
        let left = window.position.x + editor.gutter_width_for_window(&window) + 1;
        let y = editor.window_to_terminal_y(&window, row.row);
        for (text, kind) in [
            ("1", MouseEventKind::Down(MouseButton::Left)),
            ("3", MouseEventKind::Drag(MouseButton::Left)),
        ] {
            let x = projection
                .glyphs
                .iter()
                .find(|glyph| glyph.text == text)
                .unwrap()
                .column;
            editor
                .test_execute_event(Event::Mouse(MouseEvent {
                    kind,
                    column: (left + x) as u16,
                    row: y as u16,
                    modifiers: KeyModifiers::NONE,
                }))
                .await
                .unwrap();
        }
        assert!(editor.is_visual());
        assert!(!projected(&editor, 2));
        assert_eq!(editor.cx, 10);
        key(&mut editor, KeyCode::Char('y')).await;
        assert_eq!(editor.registers[&'"'].text, "123");
    }

    #[tokio::test]
    async fn clicking_visual_source_maps_before_restoring_the_grid() {
        let source = "A | B\n--- | ---:\nshort | 12345\n";
        let mut editor = editor(source, 60, 15);
        editor.cy = 2;
        editor
            .test_execute_production_action(Action::EnterMode(Mode::Visual))
            .await
            .unwrap();
        let window = editor.active_window_with_editor_view().unwrap();
        let layout = editor.layout_for_window(&window);
        let row = layout.rows.iter().find(|row| row.line == 2).unwrap();
        assert!(row.projection.is_none());
        let x = window.position.x + editor.gutter_width_for_window(&window) + 1 + 10;
        let y = editor.window_to_terminal_y(&window, row.row);
        editor
            .test_execute_event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x as u16,
                row: y as u16,
                modifiers: KeyModifiers::NONE,
            }))
            .await
            .unwrap();
        assert_eq!(editor.mode, Mode::Normal);
        assert_eq!(editor.cx, 10, "the source digit 3 was under the pointer");
        assert!(projected(&editor, 2));
        assert_eq!(editor.current_buffer().contents(), source);
    }

    #[tokio::test]
    async fn wheel_scroll_clamps_to_a_real_visible_cell_in_wrapped_rows() {
        let source = format!(
            "A | B\n--- | ---\n{} | {}\n",
            "alpha ".repeat(45),
            "bravo ".repeat(45)
        );
        let mut editor = editor(&source, 36, 12);
        editor.cy = 2;
        render(&mut editor);
        for _ in 0..8 {
            editor
                .test_execute_production_action(Action::ScrollDown)
                .await
                .unwrap();
            assert!(
                editor.render_cursor_position().is_some(),
                "scroll left the cursor outside the projected viewport"
            );
        }
        assert!(editor.skipcol > 0);
        assert_eq!(editor.current_buffer().contents(), source);
    }

    #[test]
    fn horizontal_overflow_does_not_cover_the_cursor_text() {
        for tail in ["TAIL", "界🙂"] {
            let source = format!("A | B\n--- | ---\nleft | a long description ending in {tail}|\n");
            let mut editor = editor(&source, 30, 15);
            editor.wrap = false;
            editor.cy = 2;
            editor.cx = source.lines().nth(2).unwrap().graphemes(true).count() - 2;
            editor.refresh_cursor_goal();
            let mut screen = RenderBuffer::new(30, 15, &Style::default());
            editor.render(&mut screen).unwrap();
            let (x, y) = editor.render_cursor_position().unwrap();
            assert_eq!(
                screen.cells[y * screen.width + x].text,
                tail.graphemes(true).next_back().unwrap()
            );
            assert!(editor.vleft > 0);

            editor.cx += 1;
            editor.refresh_cursor_goal();
            editor.render_cursor_motion_delta(&mut screen).unwrap();
            let (x, y) = editor.render_cursor_position().unwrap();
            assert_eq!(screen.cells[y * screen.width + x].text, "│");
        }
    }

    #[test]
    fn reveal_changes_layout_only_for_intersected_markdown_tables() {
        let mut editor = editor("Intro\n\nA | B\n--- | ---\none | two\n", 60, 15);
        editor.mode = Mode::Insert;
        let window = editor.active_window_with_editor_view().unwrap();
        assert_eq!(editor.markdown_reveal_range(&window), None);
        editor.cy = 3;
        let window = editor.active_window_with_editor_view().unwrap();
        assert_eq!(editor.markdown_reveal_range(&window), Some(2..5));
        editor.cy = 4;
        let window = editor.active_window_with_editor_view().unwrap();
        assert_eq!(editor.markdown_reveal_range(&window), Some(2..5));
        editor.current_buffer_mut().file = Some("example.rs".into());
        assert_eq!(editor.markdown_reveal_range(&window), None);
    }

    #[tokio::test]
    async fn source_toggle_and_split_presentation_are_independent() {
        let source = "A | B\n--- | ---\none | two\n";
        let mut editor = editor(source, 100, 20);
        render(&mut editor);
        editor
            .test_execute_production_action(Action::SplitVertical)
            .await
            .unwrap();
        editor
            .test_execute_production_action(Action::Command("markdown-tables".into()))
            .await
            .unwrap();
        render(&mut editor);
        assert!(!projected(&editor, 0));
        let other = editor
            .window_manager
            .windows()
            .into_iter()
            .find(|window| !window.active)
            .unwrap();
        assert!(editor
            .layout_for_window(other)
            .rows
            .iter()
            .any(|row| row.projection.is_some()));
        assert_eq!(editor.current_buffer().contents(), source);
    }

    #[tokio::test]
    async fn wrapped_navigation_and_reveal_keep_cursor_visible_and_anchored() {
        let source = format!(
            "{}A | B\n--- | ---\n{} | tail\n\nAfter\n",
            "Before\n".repeat(20),
            "words ".repeat(45)
        );
        let mut editor = editor(&source, 36, 12);
        editor.vtop = 18;
        editor.cy = 4;
        editor.cx = 95;
        editor.refresh_cursor_goal();
        render(&mut editor);
        let before = editor
            .render_cursor_position()
            .expect("visible rendered cursor");
        editor
            .test_execute_production_action(Action::EnterMode(Mode::Insert))
            .await
            .unwrap();
        let during = editor
            .render_cursor_position()
            .expect("visible source cursor");
        assert_eq!(before.1, during.1, "reveal should preserve screen row");
        editor
            .test_execute_production_action(Action::EnterMode(Mode::Normal))
            .await
            .unwrap();
        let after = editor
            .render_cursor_position()
            .expect("visible restored cursor");
        assert_eq!(before.1, after.1);
        for _ in 0..25 {
            editor
                .test_execute_production_action(Action::MoveScreenLineDown)
                .await
                .unwrap();
            assert!(editor.render_cursor_position().is_some());
        }
        assert_eq!(editor.buffer_line(), 24);
        assert_eq!(editor.current_buffer().contents(), source);
    }

    #[test]
    fn scrolling_and_resizing_keep_full_table_widths_and_source_positions() {
        let source = "Short | B\n--- | ---\nx | value\na much wider value | z\n";
        let mut editor = editor(source, 50, 12);
        render(&mut editor);
        let window = editor.active_window_with_editor_view().unwrap();
        let first = editor.layout_for_window(&window);
        let width = first
            .rows
            .iter()
            .find(|row| row.line == 2)
            .unwrap()
            .projection
            .as_ref()
            .unwrap()
            .width;
        editor.vtop = 2;
        editor.cy = 0;
        render(&mut editor);
        let window = editor.active_window_with_editor_view().unwrap();
        let scrolled = editor.layout_for_window(&window);
        assert_eq!(scrolled.rows[0].projection.as_ref().unwrap().width, width);
        editor.test_set_size(24, 12);
        render(&mut editor);
        assert!(editor.render_cursor_position().is_some());
        assert_eq!(editor.current_buffer().contents(), source);
    }
}
