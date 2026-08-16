//! Conservative, display-only inline highlights. Patch text and line indices
//! remain untouched so staging and discarding still use the original diff.

use std::ops::Range;

use similar::{ChangeTag, DiffTag, TextDiff};
use unicode_segmentation::UnicodeSegmentation;

use super::{WorkspaceDocument, WorkspaceDocumentLine};

const MAX_BLOCK_LINES: usize = 128;
const MAX_BLOCK_BYTES: usize = 32 * 1024;
const MAX_PAIR_BYTES: usize = 8192;
const MAX_TOKEN_COMPARISONS: usize = 256 * 1024;
const MIN_SIMILARITY: usize = 65;

pub(super) fn word_changes(document: Option<&WorkspaceDocument>) -> Vec<Vec<Range<usize>>> {
    let Some(document) = document else {
        return Vec::new();
    };
    let mut result = vec![Vec::new(); document.lines.len()];
    let mut index = 0;
    while index < document.lines.len() {
        if document.lines[index].kind != "removed" {
            index += 1;
            continue;
        }
        let old_start = index;
        while index < document.lines.len() && document.lines[index].kind == "removed" {
            index += 1;
        }
        let new_start = index;
        while index < document.lines.len() && document.lines[index].kind == "added" {
            index += 1;
        }
        let old = &document.lines[old_start..new_start];
        let new = &document.lines[new_start..index];
        if old.is_empty()
            || new.is_empty()
            || old.len().max(new.len()) > MAX_BLOCK_LINES
            || old
                .iter()
                .chain(new)
                .map(|line| line.text.len())
                .sum::<usize>()
                > MAX_BLOCK_BYTES
        {
            continue;
        }

        // Anchor on matching code before considering replacements. Inserting a
        // blank line or changing a block's indentation must not shift every pair.
        let old_text = old.iter().map(|line| line.text.trim()).collect::<Vec<_>>();
        let new_text = new.iter().map(|line| line.text.trim()).collect::<Vec<_>>();
        let lines = TextDiff::from_slices(&old_text, &new_text);
        for operation in lines.ops() {
            if operation.tag() != DiffTag::Replace {
                continue;
            }
            let old_range = operation.old_range();
            let new_range = operation.new_range();
            for (old_offset, new_offset) in
                align_replacements(&old[old_range.clone()], &new[new_range.clone()])
            {
                let old_index = old_start + old_range.start + old_offset;
                let new_index = new_start + new_range.start + new_offset;
                let (removed, added) = changed_words(
                    &document.lines[old_index].text,
                    &document.lines[new_index].text,
                );
                result[old_index] = removed;
                result[new_index] = added;
            }
        }
    }
    result
}

fn tokens(text: &str) -> Vec<&str> {
    text.split_word_bounds()
        .filter(|token| !token.chars().all(char::is_whitespace))
        .collect()
}

fn similarity(old: &[&str], new: &[&str]) -> usize {
    if old.is_empty() || new.is_empty() {
        return 0;
    }
    let diff = TextDiff::from_slices(old, new);
    let mut equal = 0;
    let mut shares_word = false;
    for change in diff.iter_all_changes() {
        if change.tag() == ChangeTag::Equal {
            equal += 1;
            shares_word |= change.value().chars().any(char::is_alphanumeric);
        }
    }
    let score = 200 * equal / (old.len() + new.len());
    if shares_word && score >= MIN_SIMILARITY {
        score
    } else {
        0
    }
}

/// Find the highest-scoring order-preserving pairs, allowing either side to
/// contain unmatched lines. Work is bounded because this runs on the UI thread.
fn align_replacements(
    old: &[WorkspaceDocumentLine],
    new: &[WorkspaceDocumentLine],
) -> Vec<(usize, usize)> {
    let old_tokens = old
        .iter()
        .map(|line| tokens(&line.text))
        .collect::<Vec<_>>();
    let new_tokens = new
        .iter()
        .map(|line| tokens(&line.text))
        .collect::<Vec<_>>();
    let old_count = old_tokens.iter().map(Vec::len).sum::<usize>();
    let new_count = new_tokens.iter().map(Vec::len).sum::<usize>();
    if old_count.saturating_mul(new_count) > MAX_TOKEN_COMPARISONS {
        return Vec::new();
    }

    let columns = new.len() + 1;
    let mut scores = vec![0; old.len() * new.len()];
    let mut best = vec![0; (old.len() + 1) * columns];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            let score = if old[i].text.len().saturating_add(new[j].text.len()) <= MAX_PAIR_BYTES {
                similarity(&old_tokens[i], &new_tokens[j])
            } else {
                0
            };
            scores[i * new.len() + j] = score;
            best[i * columns + j] = best[(i + 1) * columns + j]
                .max(best[i * columns + j + 1])
                .max(score + best[(i + 1) * columns + j + 1]);
        }
    }

    let mut pairs = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < old.len() && j < new.len() {
        let score = scores[i * new.len() + j];
        if score > 0 && best[i * columns + j] == score + best[(i + 1) * columns + j + 1] {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if best[(i + 1) * columns + j] >= best[i * columns + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

fn changed_words(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let old_words = old.trim().split_word_bounds().collect::<Vec<_>>();
    let new_words = new.trim().split_word_bounds().collect::<Vec<_>>();
    let diff = TextDiff::from_slices(&old_words, &new_words);
    let mut old_byte = old.len() - old.trim_start().len();
    let mut new_byte = new.len() - new.trim_start().len();
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    for change in diff.iter_all_changes() {
        let length = change.value().len();
        match change.tag() {
            ChangeTag::Equal => {
                old_byte += length;
                new_byte += length;
            }
            ChangeTag::Delete => {
                push_range(&mut removed, old, old_byte..old_byte + length);
                old_byte += length;
            }
            ChangeTag::Insert => {
                push_range(&mut added, new, new_byte..new_byte + length);
                new_byte += length;
            }
        }
    }
    (removed, added)
}

/// Join adjacent edits and tiny whitespace gaps into one readable phrase.
fn push_range(ranges: &mut Vec<Range<usize>>, text: &str, next: Range<usize>) {
    if let Some(previous) = ranges.last_mut() {
        let gap = &text[previous.end..next.start];
        if gap.len() <= 3 && gap.bytes().all(|byte| matches!(byte, b' ' | b'\t')) {
            previous.end = next.end;
            return;
        }
    }
    ranges.push(next);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn highlights(old: &[&str], new: &[&str]) -> Vec<Vec<String>> {
        let document = WorkspaceDocument {
            lines: old
                .iter()
                .map(|text| ("removed", text))
                .chain(new.iter().map(|text| ("added", text)))
                .map(|(kind, text)| WorkspaceDocumentLine {
                    kind: kind.to_string(),
                    text: (*text).to_string(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        word_changes(Some(&document))
            .into_iter()
            .zip(&document.lines)
            .map(|(ranges, line)| {
                ranges
                    .into_iter()
                    .map(|range| line.text[range].to_string())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn blank_line_insertion_does_not_shift_replacement_pairs() {
        let marks = highlights(
            &["    let count = 1;", "    return count;"],
            &["", "let count = 4;", "return count;"],
        );
        assert_eq!(marks[0], ["1"]);
        assert_eq!(marks[3], ["4"]);
        assert!([1, 2, 4].into_iter().all(|index| marks[index].is_empty()));
    }

    #[test]
    fn reindented_guard_clause_leaves_the_moved_body_quiet() {
        let marks = highlights(
            &[
                "    if (CheckCollision(a, b)) {",
                "        b->active = false;",
                "",
                "        switch (a->size) {",
                "            case ASTEROID_LARGE:",
                "                SpawnAsteroidsFrom(g, a, ASTEROID_MEDIUM);",
                "                break;",
                "        }",
            ],
            &[
                "    if (!CheckCollision(a, b)) continue;",
                "",
                "    b->active = false;",
                "",
                "    switch (a->size) {",
                "        case ASTEROID_LARGE:",
                "            SpawnAsteroidsFrom(g, a, ASTEROID_MEDIUM);",
                "            break;",
                "    }",
            ],
        );
        assert!(!marks[0].is_empty());
        assert!(marks[8].iter().any(|mark| mark.contains('!')));
        assert!(marks
            .iter()
            .enumerate()
            .all(|(index, ranges)| { index == 0 || index == 8 || ranges.is_empty() }));
    }

    #[test]
    fn unrelated_lines_and_whitespace_only_changes_stay_line_only() {
        for (old, new) in [
            ("destroy_world(old);", "render_frame(next);"),
            ("    do_work();", "\tdo_work();  "),
            ("   ", ""),
        ] {
            assert!(highlights(&[old], &[new]).iter().all(Vec::is_empty));
        }
    }

    #[test]
    fn replacement_alignment_skips_unrelated_insertions() {
        let marks = highlights(
            &["let count = 1;", "return count;"],
            &["log_start();", "let count = 4;", "return count + 1;"],
        );
        assert_eq!(marks[0], ["1"]);
        assert!(marks[2].is_empty());
        assert_eq!(marks[3], ["4"]);
        assert!(marks[4].iter().any(|mark| mark.contains("+ 1")));
    }

    #[test]
    fn changed_phrases_include_the_spaces_between_words() {
        let (old, new) = changed_words("    call(old first, keep);", "  call(new second, keep);");
        assert_eq!(&"    call(old first, keep);"[old[0].clone()], "old first");
        assert_eq!(&"  call(new second, keep);"[new[0].clone()], "new second");
        assert_eq!((old.len(), new.len()), (1, 1));
    }

    #[test]
    fn unicode_ranges_are_original_utf8_offsets() {
        let marks = highlights(&["    let café = \"旧\";"], &["  let café = \"新\";"]);
        assert_eq!(marks, [vec!["旧"], vec!["新"]]);
    }

    #[test]
    fn excessive_change_blocks_fall_back_to_line_colors() {
        let old = vec!["let count = 1;"; MAX_BLOCK_LINES + 1];
        let new = vec!["let count = 4;"; MAX_BLOCK_LINES + 1];
        assert!(highlights(&old, &new).iter().all(Vec::is_empty));
        let old = format!("{}old", "x ".repeat(MAX_PAIR_BYTES));
        let new = format!("{}new", "x ".repeat(MAX_PAIR_BYTES));
        assert!(highlights(&[&old], &[&new]).iter().all(Vec::is_empty));
    }
}
