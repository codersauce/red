//! Align a unified diff document into side-by-side rows.
//!
//! Pairing follows hunk order. It does not search for similar lines. The
//! document's line indices stay unchanged so staging still addresses the
//! unified patch.

use super::WorkspaceDocumentLine;

/// One screen row in the paired layout.
///
/// A spanning row (hunk header, patch metadata, or an unnumbered note) sets
/// `span` and leaves both cells empty. A context line uses the same document
/// index on both sides. A blank pad is `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PairedRow {
    pub left: Option<usize>,
    pub right: Option<usize>,
    pub span: Option<usize>,
}

impl PairedRow {
    fn span(index: usize) -> Self {
        Self {
            left: None,
            right: None,
            span: Some(index),
        }
    }

    fn cells(left: Option<usize>, right: Option<usize>) -> Self {
        Self {
            left,
            right,
            span: None,
        }
    }

    pub(super) fn min_line(self) -> Option<usize> {
        [self.span, self.left, self.right]
            .into_iter()
            .flatten()
            .min()
    }
}

pub(super) fn pair_visible(lines: &[WorkspaceDocumentLine], visible: &[usize]) -> Vec<PairedRow> {
    let mut rows = Vec::new();
    let mut seen_hunk = false;
    let mut body = Vec::new();
    for &index in visible {
        let Some(line) = lines.get(index) else {
            continue;
        };
        if !seen_hunk {
            rows.push(PairedRow::span(index));
            if line.kind == "hunk" {
                seen_hunk = true;
            }
            continue;
        }
        if is_span_line(line) {
            flush_body(&body, lines, &mut rows);
            body.clear();
            rows.push(PairedRow::span(index));
        } else {
            body.push(index);
        }
    }
    flush_body(&body, lines, &mut rows);
    rows
}

fn flush_body(indices: &[usize], lines: &[WorkspaceDocumentLine], rows: &mut Vec<PairedRow>) {
    let mut cursor = 0;
    while cursor < indices.len() {
        let index = indices[cursor];
        let Some(line) = lines.get(index) else {
            cursor += 1;
            continue;
        };
        if is_newline_marker(line) {
            let previous = cursor
                .checked_sub(1)
                .and_then(|previous| lines.get(indices[previous]));
            rows.push(marker_row(index, previous));
            cursor += 1;
            continue;
        }
        match line.kind.as_str() {
            "context" => {
                rows.push(PairedRow::cells(Some(index), Some(index)));
                cursor += 1;
            }
            "removed" => {
                let removed_start = cursor;
                cursor = take_kind(indices, lines, cursor, "removed");
                let added_start = cursor;
                cursor = take_kind(indices, lines, cursor, "added");
                let removed = &indices[removed_start..added_start];
                let added = &indices[added_start..cursor];
                let count = removed.len().max(added.len());
                for offset in 0..count {
                    rows.push(PairedRow::cells(
                        removed.get(offset).copied(),
                        added.get(offset).copied(),
                    ));
                }
            }
            "added" => {
                rows.push(PairedRow::cells(None, Some(index)));
                cursor += 1;
            }
            _ => {
                rows.push(PairedRow::span(index));
                cursor += 1;
            }
        }
    }
}

fn take_kind(
    indices: &[usize],
    lines: &[WorkspaceDocumentLine],
    mut cursor: usize,
    kind: &str,
) -> usize {
    while cursor < indices.len() {
        let Some(line) = lines.get(indices[cursor]) else {
            break;
        };
        if is_newline_marker(line) || line.kind != kind {
            break;
        }
        cursor += 1;
    }
    cursor
}

fn marker_row(index: usize, previous: Option<&WorkspaceDocumentLine>) -> PairedRow {
    match previous.map(|line| line.kind.as_str()) {
        Some("context") => PairedRow::cells(Some(index), Some(index)),
        Some("removed") => PairedRow::cells(Some(index), None),
        Some("added") => PairedRow::cells(None, Some(index)),
        _ => PairedRow::span(index),
    }
}

fn is_newline_marker(line: &WorkspaceDocumentLine) -> bool {
    line.text.trim_start().starts_with('\\')
}

fn is_span_line(line: &WorkspaceDocumentLine) -> bool {
    if is_newline_marker(line) {
        return false;
    }
    match line.kind.as_str() {
        "hunk" | "meta" => true,
        "context" => line.old_line.is_none() && line.new_line.is_none(),
        "added" | "removed" => false,
        _ => true,
    }
}

/// Column widths for a detail pane, including the one-column rule.
///
/// Each side must be at least 16 columns or the caller renders unified.
pub(super) fn paired_pane_widths(detail_width: usize) -> Option<(usize, usize)> {
    let left = detail_width.saturating_sub(1) / 2;
    let right = detail_width.saturating_sub(left).saturating_sub(1);
    (left >= 16 && right >= 16).then_some((left, right))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(
        kind: &str,
        text: &str,
        old_line: Option<usize>,
        new_line: Option<usize>,
    ) -> WorkspaceDocumentLine {
        WorkspaceDocumentLine {
            id: text.to_string(),
            text: text.to_string(),
            kind: kind.to_string(),
            old_line,
            new_line,
            ..WorkspaceDocumentLine::default()
        }
    }

    #[test]
    fn pairs_replacements_context_and_newline_markers_by_hunk_order() {
        let lines = vec![
            line("meta", "diff --git a/f b/f", None, None),
            line("hunk", "@@ -1,4 +1,4 @@", None, None),
            line("context", "keep", Some(1), Some(1)),
            line("removed", "old", Some(2), None),
            line("removed", "older", Some(3), None),
            line("added", "new", None, Some(2)),
            line("added", "extra", None, Some(3)),
            line("context", "tail", Some(4), Some(4)),
            line("meta", "\\ No newline at end of file", None, None),
            line("hunk", "@@ -8 +8 @@", None, None),
            line("added", "only new", None, Some(8)),
            line("meta", "\\ No newline at end of file", None, None),
            line("context", "… more lines", None, None),
        ];
        let visible: Vec<_> = (0..lines.len()).collect();
        let rows = pair_visible(&lines, &visible);

        assert_eq!(rows[0], PairedRow::span(0));
        assert_eq!(rows[1], PairedRow::span(1));
        assert_eq!(rows[2], PairedRow::cells(Some(2), Some(2)));
        assert_eq!(rows[3], PairedRow::cells(Some(3), Some(5)));
        assert_eq!(rows[4], PairedRow::cells(Some(4), Some(6)));
        assert_eq!(rows[5], PairedRow::cells(Some(7), Some(7)));
        assert_eq!(rows[6], PairedRow::cells(Some(8), Some(8)));
        assert_eq!(rows[7], PairedRow::span(9));
        assert_eq!(rows[8], PairedRow::cells(None, Some(10)));
        assert_eq!(rows[9], PairedRow::cells(None, Some(11)));
        assert_eq!(rows[10], PairedRow::span(12));
    }

    #[test]
    fn a_longer_removed_run_pads_the_right_side() {
        let lines = vec![
            line("hunk", "@@ -1,2 +1 @@", None, None),
            line("removed", "first", Some(1), None),
            line("removed", "second", Some(2), None),
            line("added", "replacement", None, Some(1)),
            line("meta", "\\ No newline at end of file", None, None),
        ];
        let rows = pair_visible(&lines, &[0, 1, 2, 3, 4]);
        assert_eq!(rows[1], PairedRow::cells(Some(1), Some(3)));
        assert_eq!(rows[2], PairedRow::cells(Some(2), None));
        assert_eq!(rows[3], PairedRow::cells(None, Some(4)));
    }

    #[test]
    fn a_removed_run_without_additions_stays_on_the_left() {
        let lines = vec![
            line("hunk", "@@ -1 +0,0 @@", None, None),
            line("removed", "gone", Some(1), None),
            line("meta", "\\ No newline at end of file", None, None),
        ];
        let rows = pair_visible(&lines, &[0, 1, 2]);
        assert_eq!(rows[1], PairedRow::cells(Some(1), None));
        assert_eq!(rows[2], PairedRow::cells(Some(2), None));
    }
}
