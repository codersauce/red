//! The attributed edit history shown by `:AgentHistory`.
//!
//! Undo transactions know what changed but not the surrounding lines, so agent
//! edits also keep a unified-diff preview made when they were applied, while the
//! old and new documents were both at hand. Other transactions are rendered from
//! their recorded replacements. The history covers every open file, newest first,
//! and each entry carries enough labels for a row to say what, where, who and when.

use std::collections::VecDeque;

use super::*;
use crate::editing::{preview_line_text, unified_diff, DiffLine, DiffLineKind, DiffStat};

/// Previews kept for recent agent transactions.
const MAX_EDIT_PREVIEWS: usize = 256;
/// Unchanged lines shown around each change.
const PREVIEW_CONTEXT: usize = 3;
/// Longest preview kept for one transaction.
const MAX_PREVIEW_LINES: usize = 400;
/// Most entries one history request returns.
const MAX_HISTORY_ENTRIES: usize = 500;

#[derive(Debug, Clone)]
struct EditPreview {
    lines: Vec<DiffLine>,
    stat: DiffStat,
    /// Lines in the whole diff; `lines` holds at most MAX_PREVIEW_LINES of them.
    total: usize,
}

#[derive(Debug, Default)]
pub(super) struct EditPreviews {
    previews: HashMap<String, EditPreview>,
    order: VecDeque<String>,
}

impl EditPreviews {
    fn insert(&mut self, transaction_id: String, preview: EditPreview) {
        if self
            .previews
            .insert(transaction_id.clone(), preview)
            .is_none()
        {
            self.order.push_back(transaction_id);
        }
        while self.order.len() > MAX_EDIT_PREVIEWS {
            if let Some(oldest) = self.order.pop_front() {
                self.previews.remove(&oldest);
            }
        }
    }
}

/// Which buffers a history request covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditHistoryScope {
    /// The active buffer, the original `EditHistory` behaviour.
    Buffer,
    /// Every open file, newest first.
    Workspace,
}

impl Editor {
    /// Keeps a readable diff of an agent transaction while both documents exist.
    pub(super) fn record_edit_preview(&mut self, transaction_id: &str, before: &str, after: &str) {
        let (lines, stat, total) = unified_diff(before, after, PREVIEW_CONTEXT, MAX_PREVIEW_LINES);
        self.edit_previews.insert(
            transaction_id.to_string(),
            EditPreview { lines, stat, total },
        );
    }

    /// Builds the `EditHistory` response.
    pub(super) fn edit_history_payload(&self, scope: EditHistoryScope) -> Value {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        let buffers: Vec<&Buffer> = match scope {
            EditHistoryScope::Buffer => vec![self.current_buffer()],
            EditHistoryScope::Workspace => self
                .buffer_manager
                .iter()
                .filter(|buffer| {
                    buffer.file.is_some() && !self.scratch_buffers.contains_key(&buffer.id())
                })
                .collect(),
        };
        // Pick the newest entries from cheap metadata first, so only those are
        // serialized and diffed; histories can hold thousands of large edits.
        let mut candidates = Vec::new();
        let mut applied: Vec<HashSet<usize>> = Vec::with_capacity(buffers.len());
        for (slot, buffer) in buffers.iter().enumerate() {
            let history = &buffer.undo_history;
            let parents: HashMap<usize, Option<usize>> = history
                .node_summaries()
                .map(|(index, parent, timestamp)| {
                    candidates.push((timestamp, index, slot));
                    (index, parent)
                })
                .collect();
            let mut path = HashSet::new();
            let mut node = history.current_index();
            while let Some(index) = node {
                path.insert(index);
                node = parents.get(&index).copied().flatten();
            }
            applied.push(path);
        }
        let total = candidates.len();
        // The original buffer scope returns its whole tree in node order. The
        // workspace scope is newest first, within one millisecond later nodes being
        // newer, and capped.
        let with_edits = scope == EditHistoryScope::Buffer;
        if !with_edits {
            candidates.sort_by_key(|&(timestamp, index, _)| std::cmp::Reverse((timestamp, index)));
            candidates.truncate(MAX_HISTORY_ENTRIES);
        }
        let mut entries = Vec::with_capacity(candidates.len());
        for (_, index, slot) in candidates {
            let buffer = buffers[slot];
            let (Some(entry), Some(transaction)) = (
                buffer.undo_history.undo_tree_entry(index, with_edits),
                buffer.undo_history.node_transaction(index),
            ) else {
                continue;
            };
            // A workspace history leaves out the unbounded replacement texts; the
            // bounded `document` stands in for them.
            let mut value = serde_json::to_value(&entry).unwrap_or(Value::Null);
            if !with_edits {
                if let Some(fields) = value.as_object_mut() {
                    fields.remove("edits");
                }
            }
            let (mut lines, stat, total_lines) =
                match self.edit_previews.previews.get(&entry.transaction_id) {
                    Some(preview) => (preview.lines.clone(), preview.stat, preview.total),
                    None => replacement_diff(&transaction.edits),
                };
            // The workspace offers a full patch for truncated documents, which a
            // history preview cannot provide, so omitted lines are noted in the
            // diff itself and the document is complete as shown.
            let omitted = total_lines.saturating_sub(lines.len());
            if omitted > 0 {
                lines.push(DiffLine {
                    kind: DiffLineKind::Meta,
                    old_line: None,
                    new_line: None,
                    text: format!(
                        "… {omitted} more diff lines not shown (the preview keeps {MAX_PREVIEW_LINES})"
                    ),
                });
            }
            let path = buffer
                .file
                .as_deref()
                .map(|file| self.history_display_path(file))
                .unwrap_or_else(|| buffer.name().to_string());
            value["file"] = json!(path);
            value["buffer_id"] = json!(buffer.id());
            value["applied"] = json!(applied[slot].contains(&index));
            value["age"] = json!(age_label(now_ms.saturating_sub(entry.timestamp_ms)));
            value["added"] = json!(stat.added);
            value["removed"] = json!(stat.removed);
            value["document"] = json!({
                "path": path,
                "lines": lines
                    .iter()
                    .enumerate()
                    .map(|(line_index, line)| json!({
                        "id": format!("{}:{line_index}", entry.transaction_id),
                        "text": line.text,
                        "kind": line.kind,
                        "old_line": line.old_line,
                        "new_line": line.new_line,
                    }))
                    .collect::<Vec<_>>(),
                "added": stat.added,
                "removed": stat.removed,
                "total_lines": total_lines,
                "truncated": false,
            });
            if let EditOrigin::Agent { turn_id, .. } = &entry.origin {
                if let Some((turn, prompt)) = self.agent_turn_summary(turn_id) {
                    value["turn"] = json!(turn);
                    value["prompt"] = json!(prompt);
                }
            }
            entries.push(value);
        }
        json!({
            "scope": if with_edits { "buffer" } else { "workspace" },
            "entries": entries,
            "total": total,
        })
    }

    pub(super) fn history_display_path(&self, file: &str) -> String {
        let path = Path::new(file);
        self.agent_manager
            .root()
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .and_then(|root| path.strip_prefix(root).ok().map(Path::to_path_buf))
            .unwrap_or_else(|| path.to_path_buf())
            .display()
            .to_string()
    }

    /// One-based turn number and a short excerpt of the prompt that started it.
    fn agent_turn_summary(&self, turn_id: &str) -> Option<(usize, String)> {
        let conversation = self.agent_manager.conversation_snapshot()?;
        let prompts = conversation
            .items
            .iter()
            .filter(|item| item.role == crate::agent_conversation::AgentTranscriptRole::User);
        prompts
            .enumerate()
            .find(|(_, item)| item.turn_id.as_deref() == Some(turn_id))
            .map(|(index, item)| (index + 1, prompt_excerpt(&item.text)))
    }

    /// Finds the buffer whose history holds a transaction.
    pub(super) fn transaction_buffer_index(&self, transaction_id: &str) -> Option<usize> {
        self.buffer_manager.iter().position(|buffer| {
            let history = &buffer.undo_history;
            history.node_summaries().any(|(index, _, _)| {
                history
                    .node_transaction(index)
                    .is_some_and(|transaction| transaction.id == transaction_id)
            })
        })
    }
}

/// A diff rendered from a transaction's own replacements, for edits without a
/// preview. Line numbers are those at the time of each replacement.
fn replacement_diff(edits: &[crate::undo::TextEdit]) -> (Vec<DiffLine>, DiffStat, usize) {
    let mut lines = Vec::new();
    let mut stat = DiffStat::default();
    let mut total = 0;
    // Counts cover every line; only the first MAX_PREVIEW_LINES are kept, so one
    // whole-document replacement cannot build a huge preview.
    let mut push = |line: DiffLine, lines: &mut Vec<DiffLine>| {
        total += 1;
        if lines.len() < MAX_PREVIEW_LINES {
            lines.push(line);
        }
    };
    for crate::undo::TextEdit::Replace {
        range,
        old_text,
        new_text,
        ..
    } in edits
    {
        let start = range.start.line + 1;
        push(
            DiffLine {
                kind: DiffLineKind::Hunk,
                old_line: None,
                new_line: None,
                text: format!("@@ line {start} @@"),
            },
            &mut lines,
        );
        for (offset, text) in split_lines(old_text).into_iter().enumerate() {
            stat.removed += 1;
            push(
                DiffLine {
                    kind: DiffLineKind::Removed,
                    old_line: Some(start + offset),
                    new_line: None,
                    text: preview_line_text(text),
                },
                &mut lines,
            );
        }
        for (offset, text) in split_lines(new_text).into_iter().enumerate() {
            stat.added += 1;
            push(
                DiffLine {
                    kind: DiffLineKind::Added,
                    old_line: None,
                    new_line: Some(start + offset),
                    text: preview_line_text(text),
                },
                &mut lines,
            );
        }
    }
    (lines, stat, total)
}

fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    text.strip_suffix('\n')
        .unwrap_or(text)
        .split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .collect()
}

fn prompt_excerpt(text: &str) -> String {
    const MAX_CHARS: usize = 60;
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.chars().count() <= MAX_CHARS {
        return line.to_string();
    }
    let cut: String = line.chars().take(MAX_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

fn age_label(elapsed_ms: u128) -> String {
    let seconds = elapsed_ms / 1000;
    match seconds {
        0..=9 => "just now".to_string(),
        10..=59 => format!("{seconds}s ago"),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86_399 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_diffs_split_lines_and_count_changes() {
        let edits = vec![crate::undo::TextEdit::Replace {
            range: TextRange::new(TextPosition::new(4, 0), TextPosition::new(5, 0)),
            start_char: 0,
            old_text: "old\n".to_string(),
            new_text: "new one\nnew two\n".to_string(),
        }];
        let (lines, stat, total) = replacement_diff(&edits);
        assert_eq!(total, lines.len());
        assert_eq!(
            stat,
            DiffStat {
                added: 2,
                removed: 1
            }
        );
        let text: Vec<_> = lines.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(text, ["@@ line 5 @@", "old", "new one", "new two"]);
        assert_eq!(lines[3].new_line, Some(6));
    }

    #[test]
    fn a_whole_document_replacement_keeps_a_bounded_preview_and_full_counts() {
        let old_text: String = (0..1_000).map(|line| format!("old {line}\n")).collect();
        let new_text: String = (0..1_500).map(|line| format!("new {line}\n")).collect();
        let edits = vec![crate::undo::TextEdit::Replace {
            range: TextRange::new(TextPosition::new(0, 0), TextPosition::new(1_000, 0)),
            start_char: 0,
            old_text,
            new_text,
        }];
        let (lines, stat, total) = replacement_diff(&edits);
        assert_eq!(lines.len(), MAX_PREVIEW_LINES);
        assert_eq!(total, 1 + 1_000 + 1_500);
        assert_eq!(
            stat,
            DiffStat {
                added: 1_500,
                removed: 1_000
            }
        );
    }

    #[test]
    fn labels_read_naturally() {
        assert_eq!(age_label(3_000), "just now");
        assert_eq!(age_label(42_000), "42s ago");
        assert_eq!(age_label(5 * 60_000), "5m ago");
        assert_eq!(age_label(3 * 3_600_000), "3h ago");
        assert_eq!(
            prompt_excerpt("\n  Fix the sign bug\nthen more"),
            "Fix the sign bug"
        );
        assert_eq!(prompt_excerpt(&"x".repeat(80)).chars().count(), 60);
    }
}
