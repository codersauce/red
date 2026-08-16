//! Source-backed soft wrapping for editable terminal text.
//!
//! The source remains authoritative. Row ranges and cursor offsets count extended
//! graphemes, while display positions count terminal cells. Tabs and omitted soft-
//! wrap separators affect only the projection, never the document or undo history.

use std::{cmp::Reverse, ops::Range};

use unicode_segmentation::UnicodeSegmentation;

use crate::unicode_utils::{display_width, grapheme_len};

/// How a view chooses its soft line breaks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WrapMode {
    /// Preserve the existing embedded-textarea character-wrap behavior.
    #[default]
    Grapheme,
    /// Prefer Unicode line-break opportunities, splitting long words as needed.
    Word,
}

/// View-local layout policy, independent of the underlying document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutOptions {
    pub width: usize,
    pub tab_width: usize,
    pub wrap_mode: WrapMode,
}

impl LayoutOptions {
    #[must_use]
    pub const fn grapheme(width: usize) -> Self {
        Self {
            width,
            tab_width: 4,
            wrap_mode: WrapMode::Grapheme,
        }
    }

    #[must_use]
    pub const fn word(width: usize) -> Self {
        Self {
            wrap_mode: WrapMode::Word,
            ..Self::grapheme(width)
        }
    }
}

/// A zero-based visual row and terminal-cell column.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DisplayPosition {
    pub row: usize,
    pub column: usize,
}

/// Why the next display row starts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LineBreak {
    Soft,
    Hard,
    #[default]
    End,
}

/// One display row and the exact source graphemes it accounts for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutRow {
    pub text: String,
    /// Absolute grapheme range, including an omitted separator or hard newline.
    pub source: Range<usize>,
    pub break_after: LineBreak,
}

impl LayoutRow {
    fn empty(start: usize) -> Self {
        Self {
            text: String::new(),
            source: start..start,
            break_after: LineBreak::End,
        }
    }
}

/// An immutable projection with one position for every source cursor boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextLayout {
    rows: Vec<LayoutRow>,
    positions: Vec<DisplayPosition>,
    wrap_mode: WrapMode,
}

impl TextLayout {
    #[must_use]
    pub fn new(text: &str, options: LayoutOptions) -> Self {
        if options.width == 0 {
            return Self {
                rows: Vec::new(),
                positions: vec![DisplayPosition::default(); grapheme_len(text) + 1],
                wrap_mode: options.wrap_mode,
            };
        }
        match options.wrap_mode {
            WrapMode::Grapheme => grapheme_layout(text, options),
            WrapMode::Word => word_layout(text, options),
        }
    }

    #[must_use]
    pub fn rows(&self) -> &[LayoutRow] {
        &self.rows
    }

    #[must_use]
    pub fn positions(&self) -> &[DisplayPosition] {
        &self.positions
    }

    pub(crate) fn into_parts(self) -> (Vec<LayoutRow>, Vec<DisplayPosition>) {
        (self.rows, self.positions)
    }

    #[must_use]
    pub fn position(&self, grapheme_offset: usize) -> Option<DisplayPosition> {
        self.positions.get(grapheme_offset).copied()
    }

    /// Finds a representable cursor boundary on a visual row.
    ///
    /// A soft-wrap separator can share a cell with the following word. In that
    /// case prefer the word's start, so clicking its first cell does not select
    /// invisible whitespace. Equidistant *different* columns prefer the left.
    #[must_use]
    pub fn nearest_offset_on_row(&self, row: usize, column: usize) -> Option<usize> {
        if row >= self.rows.len() {
            return None;
        }
        if self.wrap_mode == WrapMode::Grapheme {
            return self
                .positions
                .iter()
                .enumerate()
                .filter(|(_, position)| position.row == row)
                .min_by_key(|(_, position)| position.column.abs_diff(column))
                .map(|(offset, _)| offset);
        }
        self.positions
            .iter()
            .enumerate()
            .filter(|(_, position)| position.row == row)
            .min_by_key(|(offset, position)| {
                (
                    position.column.abs_diff(column),
                    position.column,
                    Reverse(*offset),
                )
            })
            .map(|(offset, _)| offset)
    }
}

fn grapheme_width(grapheme: &str, column: usize, tab_width: usize) -> usize {
    if grapheme == "\t" {
        let tab_width = tab_width.max(1);
        tab_width - column % tab_width
    } else {
        display_width(grapheme)
    }
}

fn append_grapheme(row: &mut String, grapheme: &str, width: usize, viewport: usize) -> usize {
    if width > viewport {
        row.push('?');
        1
    } else if grapheme == "\t" {
        row.extend(std::iter::repeat_n(' ', width));
        width
    } else {
        row.push_str(grapheme);
        width
    }
}

fn caret_after(row: usize, column: usize, width: usize) -> DisplayPosition {
    if column == width {
        DisplayPosition {
            row: row + 1,
            column: 0,
        }
    } else {
        DisplayPosition { row, column }
    }
}

// Keep this path byte-for-byte compatible with the old UI wrap_text projection.
// Existing consumers opt into Word explicitly instead of changing their defaults.
fn grapheme_layout(text: &str, options: LayoutOptions) -> TextLayout {
    let mut rows = vec![LayoutRow::empty(0)];
    let mut positions = Vec::with_capacity(grapheme_len(text) + 1);
    let mut row = 0;
    let mut column = 0;
    positions.push(DisplayPosition::default());

    for (index, grapheme) in text.graphemes(true).enumerate() {
        if grapheme == "\n" {
            rows[row].source.end = index + 1;
            rows[row].break_after = LineBreak::Hard;
            row += 1;
            column = 0;
            rows.push(LayoutRow::empty(index + 1));
            positions.push(DisplayPosition { row, column });
            continue;
        }
        if column == options.width {
            rows[row].break_after = LineBreak::Soft;
            row += 1;
            column = 0;
            rows.push(LayoutRow::empty(index));
        }
        let mut width = grapheme_width(grapheme, column, options.tab_width);
        if width > options.width.saturating_sub(column) && column > 0 {
            rows[row].break_after = LineBreak::Soft;
            row += 1;
            column = 0;
            rows.push(LayoutRow::empty(index));
            width = grapheme_width(grapheme, column, options.tab_width);
        }
        column += append_grapheme(&mut rows[row].text, grapheme, width, options.width);
        rows[row].source.end = index + 1;
        positions.push(caret_after(row, column, options.width));
    }
    if positions
        .last()
        .is_some_and(|position| position.row >= rows.len())
    {
        rows[row].break_after = LineBreak::Soft;
        rows.push(LayoutRow::empty(positions.len() - 1));
    }
    TextLayout {
        rows,
        positions,
        wrap_mode: options.wrap_mode,
    }
}

struct Grapheme<'a> {
    text: &'a str,
    break_after: bool,
}

impl Grapheme<'_> {
    fn separator(&self) -> bool {
        // Only ordinary word separators are elided. In particular NBSP and
        // narrow NBSP must not silently become breakable spaces.
        matches!(self.text, " " | "\t")
    }

    fn newline(&self) -> bool {
        matches!(self.text, "\n" | "\r\n" | "\r")
    }
}

struct RowPlan {
    source: Range<usize>,
    visible_end: usize,
    break_after: LineBreak,
}

fn word_layout(text: &str, options: LayoutOptions) -> TextLayout {
    let mut breaks = unicode_linebreak::linebreaks(text).peekable();
    let graphemes = text
        .grapheme_indices(true)
        .map(|(byte, grapheme)| {
            let end = byte + grapheme.len();
            while breaks.peek().is_some_and(|(offset, _)| *offset < end) {
                breaks.next();
            }
            Grapheme {
                text: grapheme,
                break_after: breaks.peek().is_some_and(|(offset, _)| *offset == end),
            }
        })
        .collect::<Vec<_>>();
    let mut plans = Vec::new();
    let mut line_start = 0;
    for (index, grapheme) in graphemes.iter().enumerate() {
        if grapheme.newline() {
            plan_logical_line(&graphemes, line_start..index, options, &mut plans);
            let last = plans.last_mut().expect("a logical line has a display row");
            last.source.end = index + 1;
            last.break_after = LineBreak::Hard;
            line_start = index + 1;
        }
    }
    plan_logical_line(&graphemes, line_start..graphemes.len(), options, &mut plans);

    let mut rows = Vec::with_capacity(plans.len());
    let mut positions = vec![DisplayPosition::default(); graphemes.len() + 1];
    for (row, plan) in plans.into_iter().enumerate() {
        let mut text = String::new();
        let mut column = 0;
        for index in plan.source.start..plan.visible_end {
            positions[index] = caret_after(row, column, options.width);
            let grapheme = graphemes[index].text;
            let width = grapheme_width(grapheme, column, options.tab_width);
            column += append_grapheme(&mut text, grapheme, width, options.width);
        }
        let end = caret_after(row, column, options.width);
        positions[plan.visible_end..=plan.source.end].fill(end);
        rows.push(LayoutRow {
            text,
            source: plan.source,
            break_after: plan.break_after,
        });
    }
    if positions
        .last()
        .is_some_and(|position| position.row >= rows.len())
    {
        rows.last_mut().expect("text has a display row").break_after = LineBreak::Soft;
        rows.push(LayoutRow::empty(graphemes.len()));
    }
    TextLayout {
        rows,
        positions,
        wrap_mode: options.wrap_mode,
    }
}

fn plan_logical_line(
    graphemes: &[Grapheme<'_>],
    line: Range<usize>,
    options: LayoutOptions,
    rows: &mut Vec<RowPlan>,
) {
    let mut start = line.start;
    loop {
        let (end, visible_end) = word_row_end(graphemes, start..line.end, options);
        rows.push(RowPlan {
            source: start..end,
            visible_end,
            break_after: if end < line.end {
                LineBreak::Soft
            } else {
                LineBreak::End
            },
        });
        if end == line.end {
            break;
        }
        start = end;
    }
}

fn word_row_end(
    graphemes: &[Grapheme<'_>],
    source: Range<usize>,
    options: LayoutOptions,
) -> (usize, usize) {
    let mut index = source.start;
    let mut column = 0;
    let mut separator_start = None;
    let mut candidate = None;
    while index < source.end {
        let grapheme = &graphemes[index];
        let width = grapheme_width(grapheme.text, column, options.tab_width);
        let width = if column == 0 && width > options.width {
            1
        } else {
            width
        };
        if width > options.width.saturating_sub(column) {
            break;
        }
        column += width;
        if grapheme.separator() {
            separator_start.get_or_insert(index);
        } else {
            separator_start = None;
        }
        index += 1;
        if grapheme.break_after && index < source.end {
            let visible_end = separator_start.unwrap_or(index);
            // Leading indentation is real content, not a disposable separator.
            if visible_end > source.start {
                candidate = Some((index, visible_end));
            }
        }
    }
    if index == source.end {
        return (index, index);
    }

    // A word that exactly fills the row must not leave its separating space on
    // a row by itself. Consume the whole separator run only if another word
    // follows; real trailing whitespace remains visible/editable.
    let run_start = separator_start.unwrap_or(index);
    if graphemes[index].separator() && run_start > source.start {
        let mut end = index;
        while end < source.end && graphemes[end].separator() {
            end += 1;
        }
        if end < source.end && graphemes[end - 1].break_after {
            return (end, run_start);
        }
    }
    candidate.unwrap_or_else(|| {
        let end = index.max(source.start + 1);
        (end, end)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(layout: &TextLayout) -> Vec<&str> {
        layout.rows().iter().map(|row| row.text.as_str()).collect()
    }

    fn position(row: usize, column: usize) -> DisplayPosition {
        DisplayPosition { row, column }
    }

    #[test]
    fn word_wrap_prefers_breaks_and_reflows_the_complete_remaining_word() {
        for (text, width, expected) in [
            ("one two three", 8, vec!["one two", "three"]),
            ("abad abcde", 4, vec!["abad", "abcd", "e"]),
            ("hello world", 5, vec!["hello", "world", ""]),
            ("one   two", 7, vec!["one", "two"]),
            ("abc   ", 4, vec!["abc ", "  "]),
            ("ab\u{a0}cd ef", 6, vec!["ab\u{a0}cd", "ef"]),
            ("你好 世界", 5, vec!["你好", "世界"]),
            ("abcdefghijkl", 5, vec!["abcde", "fghij", "kl"]),
        ] {
            assert_eq!(
                rows(&TextLayout::new(text, LayoutOptions::word(width))),
                expected,
                "{text:?} at {width}"
            );
        }
    }

    #[test]
    fn source_ranges_preserve_separators_hard_breaks_and_indentation() {
        let layout = TextLayout::new("one   two\n\n  x\t \n", LayoutOptions::word(7));
        assert_eq!(rows(&layout), ["one", "two", "", "  x  ", ""]);
        assert_eq!(layout.rows()[0].source, 0..6);
        assert_eq!(layout.rows()[0].break_after, LineBreak::Soft);
        assert_eq!(layout.rows()[1].source, 6..10);
        assert_eq!(layout.rows()[1].break_after, LineBreak::Hard);
        assert_eq!(layout.rows()[2].source, 10..11);
        assert_eq!(layout.rows()[2].break_after, LineBreak::Hard);
        assert_eq!(layout.position(3), Some(position(0, 3)));
        assert_eq!(layout.position(5), Some(position(0, 3)));
        assert_eq!(layout.position(6), Some(position(1, 0)));
        assert_eq!(layout.nearest_offset_on_row(1, 0), Some(6));
    }

    #[test]
    fn full_rows_have_a_bounded_caret_and_forward_soft_break_affinity() {
        let layout = TextLayout::new("hello world", LayoutOptions::word(5));
        assert_eq!(layout.position(5), Some(position(1, 0)));
        assert_eq!(layout.position(6), Some(position(1, 0)));
        assert_eq!(layout.nearest_offset_on_row(1, 0), Some(6));
        assert_eq!(layout.position(11), Some(position(2, 0)));
        assert_eq!(layout.rows()[2].source, 11..11);

        let hard = TextLayout::new("hello\nworld", LayoutOptions::word(5));
        assert_eq!(rows(&hard), ["hello", "world", ""]);
        assert_eq!(hard.rows()[0].break_after, LineBreak::Hard);
        assert_eq!(hard.position(6), Some(position(1, 0)));
    }

    #[test]
    fn wide_graphemes_tabs_and_zero_width_views_are_bounded() {
        let text = "e\u{301} 👨‍👩‍👧‍👦\t漢";
        let zero = TextLayout::new(text, LayoutOptions::word(0));
        assert!(zero.rows().is_empty());
        assert_eq!(zero.positions().len(), grapheme_len(text) + 1);
        assert_eq!(zero.nearest_offset_on_row(0, 0), None);
        assert_eq!(
            rows(&TextLayout::new("漢", LayoutOptions::word(1))),
            ["?", ""]
        );
        assert_eq!(
            rows(&TextLayout::new(
                "a\tb",
                LayoutOptions {
                    width: 9,
                    tab_width: 8,
                    wrap_mode: WrapMode::Word
                }
            )),
            ["a       b", ""]
        );
        let wide = TextLayout::new("漢x", LayoutOptions::word(4));
        assert_eq!(wide.nearest_offset_on_row(0, 1), Some(0));
    }

    #[test]
    fn word_layout_partitions_source_and_maps_every_cursor_at_many_widths() {
        let samples = [
            "",
            "\n",
            "\n\n",
            "abcd\n",
            "one two three",
            "abad abcde",
            "  leading   and trailing  ",
            "\t\tword\t next\t",
            "漢字かな カナ",
            "e\u{301} 👨‍👩‍👧‍👦 🇧🇷 x",
            "abcd\u{200b}\u{200d}x\u{301}",
            "\u{301}a\u{200b}\n",
            "non\u{a0}breaking narrow\u{202f}space",
            "https://example.test/a-very-long-path?q=one_two",
            "\r\nlast\r",
        ];
        for text in samples {
            let graphemes = text.graphemes(true).collect::<Vec<_>>();
            for width in 1..=16 {
                let layout = TextLayout::new(text, LayoutOptions::word(width));
                assert_eq!(layout.positions().len(), graphemes.len() + 1);
                let mut end = 0;
                let mut reconstructed = String::new();
                for row in layout.rows() {
                    assert_eq!(row.source.start, end, "{text:?} at {width}");
                    assert!(
                        display_width(&row.text) <= width,
                        "{text:?} at {width}: {row:?}"
                    );
                    reconstructed.extend(graphemes[row.source.clone()].iter().copied());
                    end = row.source.end;
                }
                assert_eq!(end, graphemes.len());
                assert_eq!(reconstructed, text);
                for position in layout.positions() {
                    assert!(
                        position.row < layout.rows().len(),
                        "{text:?} at {width}: {position:?}"
                    );
                    assert!(position.column < width, "{text:?} at {width}: {position:?}");
                }
                assert!(
                    layout.positions().windows(2).all(|pair| {
                        (pair[0].row, pair[0].column) <= (pair[1].row, pair[1].column)
                    }),
                    "{text:?} at {width}"
                );
            }
        }
    }

    #[test]
    fn legacy_layout_keeps_existing_rows_positions_and_tie_breaks() {
        let layout = TextLayout::new("ab 漢\n\tZ", LayoutOptions::grapheme(4));
        assert_eq!(rows(&layout), ["ab ", "漢", "    ", "Z"]);
        assert_eq!(
            layout.positions(),
            [
                position(0, 0),
                position(0, 1),
                position(0, 2),
                position(0, 3),
                position(1, 2),
                position(2, 0),
                position(3, 0),
                position(3, 1)
            ]
        );
        let exact = TextLayout::new("abcd\nx", LayoutOptions::grapheme(4));
        assert_eq!(rows(&exact), ["abcd", "x"]);
        assert_eq!(exact.nearest_offset_on_row(1, 0), Some(4));
        let zero = TextLayout::new("漢x", LayoutOptions::grapheme(0));
        assert!(zero.rows().is_empty());
        assert_eq!(zero.positions(), [position(0, 0); 3]);
    }
}
