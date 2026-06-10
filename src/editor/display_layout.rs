use unicode_segmentation::UnicodeSegmentation as _;

use crate::unicode_utils::{column_to_grapheme, display_width};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineSegment {
    pub line: usize,
    pub row: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub start_grapheme: usize,
    pub end_grapheme: usize,
    pub source_offset: usize,
    pub first_segment: bool,
}

impl LineSegment {
    pub fn contains_display_col(&self, col: usize) -> bool {
        if self.start_col == self.end_col {
            return col == self.start_col;
        }

        col >= self.start_col && col < self.end_col
    }

    pub fn screen_col_for_display_col(&self, col: usize, width: usize) -> usize {
        col.saturating_sub(self.start_col)
            .min(width.saturating_sub(1))
    }
}

#[derive(Debug, Clone)]
pub struct DisplayLayout {
    pub rows: Vec<LineSegment>,
}

impl DisplayLayout {
    pub fn row(&self, row: usize) -> Option<&LineSegment> {
        self.rows.iter().find(|segment| segment.row == row)
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
}

pub fn layout_lines(lines: &[String], line_count: usize, config: LayoutConfig) -> DisplayLayout {
    let mut rows = Vec::new();

    if config.content_width == 0 || config.height == 0 {
        return DisplayLayout { rows };
    }

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
        let line = line_with_newline.trim_end_matches('\n');
        let source_offset = offset;
        offset += line_with_newline.len();

        let line_skipcol = if line_index == config.vtop {
            config.skipcol
        } else {
            0
        };
        let segments = if config.wrap {
            wrap_line_segments(line, line_index, config.content_width, line_skipcol)
        } else {
            nowrap_line_segment(line, line_index, config.content_width, config.vleft)
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
) -> Vec<LineSegment> {
    let mut segments = Vec::new();
    if width == 0 {
        return segments;
    }

    let mut start_col = 0;
    let line_width = display_width(line);
    let skipcol = skipcol.min(line_width);

    while start_col <= line_width {
        let end_col = if line_width == start_col {
            start_col
        } else {
            next_wrap_end(line, start_col, width)
        };

        if end_col > skipcol || (line_width == 0 && skipcol == 0) {
            let start_grapheme = column_to_grapheme(line, start_col);
            let end_grapheme = column_to_grapheme(line, end_col);
            segments.push(LineSegment {
                line: line_index,
                row: 0,
                start_col,
                end_col,
                start_grapheme,
                end_grapheme,
                source_offset: 0,
                first_segment: start_col == 0,
            });
        }

        if end_col >= line_width {
            break;
        }
        start_col = end_col;
    }

    if segments.is_empty() {
        let start_col = skipcol - (skipcol % width);
        segments.push(LineSegment {
            line: line_index,
            row: 0,
            start_col,
            end_col: start_col,
            start_grapheme: column_to_grapheme(line, start_col),
            end_grapheme: column_to_grapheme(line, start_col),
            source_offset: 0,
            first_segment: start_col == 0,
        });
    }

    segments
}

fn nowrap_line_segment(
    line: &str,
    line_index: usize,
    width: usize,
    vleft: usize,
) -> Vec<LineSegment> {
    let line_width = display_width(line);
    let start_col = vleft.min(line_width);
    let end_col = (start_col + width).min(line_width);

    vec![LineSegment {
        line: line_index,
        row: 0,
        start_col,
        end_col,
        start_grapheme: column_to_grapheme(line, start_col),
        end_grapheme: column_to_grapheme(line, end_col),
        source_offset: 0,
        first_segment: true,
    }]
}

fn next_wrap_end(line: &str, start_col: usize, width: usize) -> usize {
    let limit = start_col + width;
    let mut col = 0;

    for grapheme in line.graphemes(true) {
        let grapheme_width = display_width(grapheme);
        if col >= start_col && col + grapheme_width > limit {
            return if col == start_col {
                col + grapheme_width
            } else {
                col
            };
        }
        col += grapheme_width;
    }

    col
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_ascii_line_at_width() {
        let segments = wrap_line_segments("abcdef", 0, 3, 0);

        assert_eq!(segments.len(), 2);
        assert_eq!((segments[0].start_col, segments[0].end_col), (0, 3));
        assert_eq!((segments[1].start_col, segments[1].end_col), (3, 6));
        assert!(!segments[1].first_segment);
    }

    #[test]
    fn skipcol_starts_at_later_wrapped_segment() {
        let segments = wrap_line_segments("abcdefghijklmnopqrstuvwxyz", 0, 10, 10);

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
        let segments = wrap_line_segments("ab🙂cd", 0, 3, 0);

        assert_eq!(segments[0].end_col, 2);
        assert_eq!(segments[1].start_col, 2);
        assert_eq!(segments[1].end_col, 5);
    }
}
