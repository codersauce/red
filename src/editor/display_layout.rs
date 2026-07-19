use unicode_segmentation::UnicodeSegmentation as _;

use crate::unicode_utils::{char_display_width, display_width, trim_line_ending};

/// Minimum number of text columns kept on a wrapped row after applying
/// break-indent, mirroring vim's `breakindentopt` `min:20` default.
const BREAK_INDENT_MIN_TEXT_WIDTH: usize = 20;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// Blank screen columns drawn before the segment's text. Zero on first
    /// segments; on wrapped continuations it aligns the text with the
    /// line's indentation (vim's 'breakindent').
    pub visual_offset: usize,
}

impl LineSegment {
    pub fn contains_display_col(&self, col: usize) -> bool {
        if self.start_col == self.end_col {
            return col == self.start_col;
        }

        col >= self.start_col && col < self.end_col
    }

    pub fn screen_col_for_display_col(&self, col: usize, width: usize) -> usize {
        (self.visual_offset + col.saturating_sub(self.start_col)).min(width.saturating_sub(1))
    }
}

#[derive(Debug, Clone)]
pub struct DisplayLayout {
    pub rows: Vec<LineSegment>,
}

impl DisplayLayout {
    pub fn row(&self, row: usize) -> Option<&LineSegment> {
        self.rows.get(row)
    }

    pub fn segment_for_cursor(&self, line: usize, display_col: usize) -> Option<&LineSegment> {
        self.rows
            .iter()
            .find(|segment| segment.line == line && segment.contains_display_col(display_col))
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
    if config.content_width == 0 || config.height == 0 {
        return DisplayLayout { rows: Vec::new() };
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
        let segments = if config.wrap {
            wrap_line_segments_with_limit(
                line,
                line_index,
                config.content_width,
                line_skipcol,
                config.break_indent,
                config.height - row,
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

    DisplayLayout { rows }
}

pub fn wrap_line_segments(
    line: &str,
    line_index: usize,
    width: usize,
    skipcol: usize,
    break_indent: BreakIndentOptions,
) -> Vec<LineSegment> {
    wrap_line_segments_with_limit(line, line_index, width, skipcol, break_indent, usize::MAX)
}

fn wrap_line_segments_with_limit(
    line: &str,
    line_index: usize,
    width: usize,
    skipcol: usize,
    break_indent: BreakIndentOptions,
    max_segments: usize,
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
    let mut grapheme_count = 0;

    for (grapheme_index, (byte_offset, grapheme)) in line.grapheme_indices(true).enumerate() {
        let first_segment = start_col == 0;
        let segment_width = if first_segment {
            width
        } else {
            continuation_width
        };
        let grapheme_width = if grapheme == "\t" {
            tab_width - (col % tab_width)
        } else {
            display_width(grapheme)
        };

        if col > start_col && col + grapheme_width > start_col + segment_width {
            let end_col = col;
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
                    first_segment,
                    visual_offset: if first_segment { 0 } else { indent },
                });
                if segments.len() == max_segments {
                    return segments;
                }
            }

            start_col = end_col;
            start_grapheme = grapheme_index;
            start_byte = byte_offset;
        }

        col += grapheme_width;
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
            first_segment,
            visual_offset: if first_segment { 0 } else { indent },
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
            first_segment,
            visual_offset: if first_segment { 0 } else { indent },
        });
    }

    segments
}

fn nowrap_line_segment(
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
        first_segment: true,
        visual_offset: 0,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_ascii_line_at_width() {
        let segments = wrap_line_segments("abcdef", 0, 3, 0, BreakIndentOptions::disabled());

        assert_eq!(segments.len(), 2);
        assert_eq!((segments[0].start_col, segments[0].end_col), (0, 3));
        assert_eq!((segments[1].start_col, segments[1].end_col), (3, 6));
        assert!(!segments[1].first_segment);
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
}
