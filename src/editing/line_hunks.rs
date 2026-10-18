//! Line-aligned differences between two versions of a document.
//!
//! Agent tools hand the editor a complete new image of a file. Replacing the whole
//! buffer with it moves the user's cursor, records the entire file in undo history,
//! and turns `:AgentHistory` into a before/after dump. These helpers reduce the
//! change to the lines that actually differ, so an edit can be applied hunk by hunk
//! and rendered as an ordinary unified diff.

use std::{ops::Range, time::Duration};

use similar::{DiffOp, DiffTag, TextDiff};

/// Bounds diffing on very large or very different files; a timed-out diff is still
/// correct, only less minimal.
const DIFF_TIMEOUT: Duration = Duration::from_millis(250);
/// Longest line text a diff preview keeps, so a changed line in a minified or
/// generated file does not copy the whole file into the preview.
pub(crate) const MAX_DIFF_LINE_CHARS: usize = 1_000;

/// Drops line endings and clips a line to [`MAX_DIFF_LINE_CHARS`].
pub(crate) fn preview_line_text(line: &str) -> String {
    let line = line.trim_end_matches(['\n', '\r']);
    match line.char_indices().nth(MAX_DIFF_LINE_CHARS) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_string(),
    }
}

/// One contiguous run of changed lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LineHunk {
    /// Replaced lines in the old document.
    pub old_lines: Range<usize>,
    /// Lines that replace them in the new document.
    pub new_lines: Range<usize>,
    /// Character range of `old_lines` in the old document.
    pub old_chars: Range<usize>,
    /// Text of `new_lines`, including line endings.
    pub new_text: String,
}

/// Returns the changed line runs from `before` to `after`, in document order.
pub(crate) fn line_hunks(before: &str, after: &str) -> Vec<LineHunk> {
    let diff = TextDiff::configure()
        .timeout(DIFF_TIMEOUT)
        .diff_lines(before, after);
    let old_starts = line_char_starts(before);
    let new_lines: Vec<&str> = diff.new_slices().to_vec();
    let mut hunks: Vec<LineHunk> = Vec::new();
    for op in diff.ops() {
        if op.tag() == DiffTag::Equal {
            continue;
        }
        let (old, new) = (op.old_range(), op.new_range());
        match hunks.last_mut() {
            Some(last) if last.old_lines.end == old.start && last.new_lines.end == new.start => {
                last.old_lines.end = old.end;
                last.new_lines.end = new.end;
            }
            _ => hunks.push(LineHunk {
                old_lines: old,
                new_lines: new,
                old_chars: 0..0,
                new_text: String::new(),
            }),
        }
    }
    for hunk in &mut hunks {
        hunk.old_chars = old_starts[hunk.old_lines.start]..old_starts[hunk.old_lines.end];
        hunk.new_text = new_lines[hunk.new_lines.clone()].concat();
    }
    hunks
}

/// Character offset of the start of every line, plus the document length.
fn line_char_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        offset += line.chars().count();
        starts.push(offset);
    }
    starts
}

/// Where a line of the old document ends up after the hunks are applied. A line
/// inside a replaced run stays at the same offset within its replacement, clamped to
/// the replacement's last line.
pub(crate) fn map_line(line: usize, hunks: &[LineHunk]) -> usize {
    let mut shift: isize = 0;
    for hunk in hunks {
        if line < hunk.old_lines.start {
            break;
        }
        if line < hunk.old_lines.end {
            let offset = (line - hunk.old_lines.start).min(hunk.new_lines.len().saturating_sub(1));
            return hunk.new_lines.start + offset;
        }
        shift += hunk.new_lines.len() as isize - hunk.old_lines.len() as isize;
    }
    line.saturating_add_signed(shift)
}

/// The role of one line in a unified diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DiffLineKind {
    /// `@@ -a,b +c,d @@`
    Hunk,
    Context,
    Added,
    Removed,
}

/// One rendered line of a unified diff. Line numbers are one-based.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct DiffLine {
    pub kind: DiffLineKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_line: Option<usize>,
    pub text: String,
}

/// Line-change totals for a diff.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct DiffStat {
    pub added: usize,
    pub removed: usize,
}

/// Renders `before` → `after` as unified diff lines with `context` unchanged lines
/// around each change. Keeps the first `max_lines` lines and also returns how many
/// lines the whole diff has.
pub(crate) fn unified_diff(
    before: &str,
    after: &str,
    context: usize,
    max_lines: usize,
) -> (Vec<DiffLine>, DiffStat, usize) {
    let diff = TextDiff::configure()
        .timeout(DIFF_TIMEOUT)
        .diff_lines(before, after);
    let old = diff.old_slices();
    let new = diff.new_slices();
    let mut lines = Vec::new();
    let mut stat = DiffStat::default();
    let mut total = 0;
    let clean = preview_line_text;
    for group in diff.grouped_ops(context) {
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        let old_start = first.old_range().start;
        let new_start = first.new_range().start;
        let old_len = last.old_range().end - old_start;
        let new_len = last.new_range().end - new_start;
        let mut push = |line: DiffLine, lines: &mut Vec<DiffLine>| {
            total += 1;
            if lines.len() < max_lines {
                lines.push(line);
            }
        };
        push(
            DiffLine {
                kind: DiffLineKind::Hunk,
                old_line: None,
                new_line: None,
                text: format!(
                    "@@ -{} +{} @@",
                    hunk_range(old_start, old_len),
                    hunk_range(new_start, new_len)
                ),
            },
            &mut lines,
        );
        for op in &group {
            let (tag, old_range, new_range) = op_ranges(op);
            match tag {
                DiffTag::Equal => {
                    for (offset, line) in old[old_range.clone()].iter().enumerate() {
                        push(
                            DiffLine {
                                kind: DiffLineKind::Context,
                                old_line: Some(old_range.start + offset + 1),
                                new_line: Some(new_range.start + offset + 1),
                                text: clean(line),
                            },
                            &mut lines,
                        );
                    }
                }
                DiffTag::Delete | DiffTag::Insert | DiffTag::Replace => {
                    for (offset, line) in old[old_range.clone()].iter().enumerate() {
                        stat.removed += 1;
                        push(
                            DiffLine {
                                kind: DiffLineKind::Removed,
                                old_line: Some(old_range.start + offset + 1),
                                new_line: None,
                                text: clean(line),
                            },
                            &mut lines,
                        );
                    }
                    for (offset, line) in new[new_range.clone()].iter().enumerate() {
                        stat.added += 1;
                        push(
                            DiffLine {
                                kind: DiffLineKind::Added,
                                old_line: None,
                                new_line: Some(new_range.start + offset + 1),
                                text: clean(line),
                            },
                            &mut lines,
                        );
                    }
                }
            }
        }
    }
    (lines, stat, total)
}

fn op_ranges(op: &DiffOp) -> (DiffTag, Range<usize>, Range<usize>) {
    (op.tag(), op.old_range(), op.new_range())
}

/// `start,len` in unified-diff notation, where an empty range names the line before it.
fn hunk_range(start: usize, len: usize) -> String {
    match len {
        0 => format!("{start},0"),
        1 => format!("{}", start + 1),
        _ => format!("{},{len}", start + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(before: &str, hunks: &[LineHunk]) -> String {
        let mut chars: Vec<char> = before.chars().collect();
        for hunk in hunks.iter().rev() {
            chars.splice(hunk.old_chars.clone(), hunk.new_text.chars());
        }
        chars.into_iter().collect()
    }

    #[test]
    fn a_one_line_change_in_a_long_file_is_one_small_hunk() {
        let before: String = (0..1000).map(|line| format!("line {line}\n")).collect();
        let after = before.replace("line 500\n", "line five hundred\n");
        let hunks = line_hunks(&before, &after);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].old_lines, 500..501);
        assert_eq!(hunks[0].new_text, "line five hundred\n");
        assert_eq!(apply(&before, &hunks), after);
    }

    #[test]
    fn hunks_reproduce_the_new_document_for_inserts_deletes_and_unicode() {
        let cases = [
            ("a\nb\nc\n", "a\nb\nc\n"),
            ("a\nb\nc\n", "x\na\nc\nd\n"),
            ("", "new\nfile"),
            ("no newline", "no newline\n"),
            ("é\n🦀\nz\n", "é\nz\n🦀\n"),
            ("one\r\ntwo\r\n", "one\r\n2\r\n"),
        ];
        for (before, after) in cases {
            assert_eq!(
                apply(before, &line_hunks(before, after)),
                after,
                "{before:?}"
            );
        }
        assert!(line_hunks("same\n", "same\n").is_empty());
    }

    #[test]
    fn lines_map_through_inserts_above_and_stay_inside_replacements() {
        let before = "a\nb\nc\nd\ne\n";
        let after = "new\na\nB\nc\ne\n";
        let hunks = line_hunks(before, after);
        assert_eq!(map_line(0, &hunks), 1, "pushed down by the insert above");
        assert_eq!(map_line(1, &hunks), 2, "replaced line keeps its place");
        assert_eq!(map_line(2, &hunks), 3);
        assert_eq!(
            map_line(3, &hunks),
            4,
            "a deleted line lands on the line after it"
        );
        assert_eq!(map_line(4, &hunks), 4);
    }

    #[test]
    fn preview_lines_are_clipped_so_minified_files_stay_small() {
        let before = format!("{}\n", "a".repeat(50_000));
        let after = format!("b{}\n", "a".repeat(50_000));
        let (lines, stat, _) = unified_diff(&before, &after, 3, 100);
        assert_eq!(
            stat,
            DiffStat {
                added: 1,
                removed: 1
            }
        );
        for line in &lines {
            assert!(line.text.chars().count() <= MAX_DIFF_LINE_CHARS + 1);
        }
        assert!(lines[2].text.starts_with('b') && lines[2].text.ends_with('…'));
        assert_eq!(preview_line_text("short\r\n"), "short");
    }

    #[test]
    fn unified_diff_has_headers_context_numbers_and_stats() {
        let before: String = (1..=20).map(|line| format!("line {line}\n")).collect();
        let after = before
            .replace("line 5\n", "line five\n")
            .replace("line 15\n", "");
        let (lines, stat, total) = unified_diff(&before, &after, 2, 100);
        assert_eq!(total, lines.len());
        assert_eq!(
            stat,
            DiffStat {
                added: 1,
                removed: 2
            }
        );
        let text: Vec<String> = lines
            .iter()
            .map(|line| {
                let mark = match line.kind {
                    DiffLineKind::Hunk => "",
                    DiffLineKind::Context => " ",
                    DiffLineKind::Added => "+",
                    DiffLineKind::Removed => "-",
                };
                format!("{mark}{}", line.text)
            })
            .collect();
        assert_eq!(
            text,
            [
                "@@ -3,5 +3,5 @@",
                " line 3",
                " line 4",
                "-line 5",
                "+line five",
                " line 6",
                " line 7",
                "@@ -13,5 +13,4 @@",
                " line 13",
                " line 14",
                "-line 15",
                " line 16",
                " line 17",
            ]
        );
        assert_eq!(lines[3].old_line, Some(5));
        assert_eq!(lines[4].new_line, Some(5));

        let (lines, _, total) = unified_diff(&before, &after, 2, 4);
        assert_eq!(lines.len(), 4);
        assert_eq!(total, 13, "the full length is kept when lines are cut");
    }
}
