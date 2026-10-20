//! LSP client coordination and document synchronization tracking for the Red editor.

use crate::buffer::{Buffer, BufferId};
use crate::lsp::{DocumentChange, Range, TextDocumentContentChangeEvent};
use ropey::Rope;

#[derive(Debug)]
struct PendingChanges {
    before: Rope,
    before_revision: u64,
    after_revision: u64,
    changes: Vec<TextDocumentContentChangeEvent>,
    insert_end: Option<crate::lsp::Position>,
}
use std::collections::{hash_map::Entry, HashMap, HashSet};

/// Coordinates LSP client state, opened workspace document tracking, and buffer revision delivery.
#[derive(Debug, Default)]
pub struct LspCoordinator {
    /// URI strings of documents currently reported as open to LSP servers.
    opened_documents: HashSet<String>,
    /// Latest buffer revision delivered to LSP servers per buffer ID.
    notified_revisions: HashMap<BufferId, u64>,
    pending_changes: HashMap<BufferId, PendingChanges>,
    standard_line_revisions: HashMap<BufferId, (u64, bool)>,
}

impl LspCoordinator {
    /// Creates a coordinator seeded with each open buffer's current revision.
    pub fn with_buffers(buffers: &[Buffer]) -> Self {
        Self {
            opened_documents: HashSet::new(),
            pending_changes: HashMap::new(),
            standard_line_revisions: HashMap::new(),
            notified_revisions: buffers
                .iter()
                .map(|buffer| (buffer.id(), buffer.revision()))
                .collect(),
        }
    }

    /// Returns `true` if the document URI has been opened via LSP.
    pub fn is_document_opened(&self, uri: &str) -> bool {
        self.opened_documents.contains(uri)
    }

    /// Marks a document URI as opened via LSP.
    pub fn mark_document_opened(&mut self, uri: impl Into<String>) -> bool {
        self.opened_documents.insert(uri.into())
    }

    /// Removes a document URI from opened LSP documents.
    pub fn mark_document_closed(&mut self, uri: &str) -> bool {
        self.opened_documents.remove(uri)
    }

    /// Clears all tracked opened document URIs.
    pub fn clear_opened_documents(&mut self) {
        self.opened_documents.clear();
    }

    /// Returns the last notified revision for a buffer, if recorded.
    pub fn last_notified_revision(&self, id: BufferId) -> Option<u64> {
        self.notified_revisions.get(&id).copied()
    }

    /// Updates the last notified revision for a buffer.
    pub fn record_notified_revision(&mut self, id: BufferId, revision: u64) {
        self.notified_revisions.insert(id, revision);
        self.pending_changes.remove(&id);
    }

    /// Seeds a revision only when the buffer has not been tracked before.
    pub fn ensure_notified_revision(&mut self, id: BufferId, revision: u64) {
        self.notified_revisions.entry(id).or_insert(revision);
    }

    /// A contiguous pending edit already owns the only original Rope snapshot it needs.
    pub fn requires_before_snapshot(&self, id: BufferId, revision: u64) -> bool {
        self.pending_changes
            .get(&id)
            .is_none_or(|pending| pending.after_revision != revision)
            || self
                .standard_line_revisions
                .get(&id)
                .is_none_or(|(cached_revision, _)| *cached_revision != revision)
    }

    /// Records a canonical replacement before external publication. A revision
    /// gap (for example undo or a raw workspace replacement) forces full sync.
    /// `before` is required only when [`Self::requires_before_snapshot`] returns true.
    pub fn record_edit(
        &mut self,
        id: BufferId,
        before_revision: u64,
        after_revision: u64,
        before: Option<Rope>,
        range: Range,
        text: &str,
    ) {
        // Ropey recognizes more line separators than the existing LSP codec.
        // Cache eligibility by revision, and retain the legacy full-text path
        // for lone CR and Unicode separators. Canonical ranges cannot split CRLF.
        let standard_before = self
            .standard_line_revisions
            .get(&id)
            .filter(|(revision, _)| *revision == before_revision)
            .map(|(_, standard)| *standard)
            .or_else(|| {
                before
                    .as_ref()
                    .map(|source| standard_lsp_lines(source.chars()))
            });
        let Some(standard_before) = standard_before else {
            self.pending_changes.remove(&id);
            return;
        };
        let standard = standard_before && standard_lsp_lines(text.chars());
        self.standard_line_revisions
            .insert(id, (after_revision, standard));
        if !standard {
            self.pending_changes.remove(&id);
            return;
        }
        if self
            .pending_changes
            .get(&id)
            .is_some_and(|pending| pending.after_revision != before_revision)
        {
            self.pending_changes.remove(&id);
        }
        let pending = match self.pending_changes.entry(id) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let Some(before) = before else {
                    return;
                };
                entry.insert(PendingChanges {
                    before,
                    before_revision,
                    after_revision: before_revision,
                    changes: Vec::new(),
                    insert_end: None,
                })
            }
        };
        pending.after_revision = after_revision;
        // Cache the end coordinate instead of rescanning a growing insertion.
        let insert_end = (range.start == range.end && !text.contains(['\r', '\n'])).then(|| {
            crate::lsp::Position {
                line: range.start.line,
                character: range.start.character + text.encode_utf16().count(),
            }
        });
        if insert_end.is_some() && pending.insert_end.as_ref() == Some(&range.start) {
            if let Some(previous) = pending.changes.last_mut() {
                previous.text.push_str(text);
                pending.insert_end = insert_end;
                return;
            }
        }
        pending.insert_end = insert_end;
        pending.changes.push(TextDocumentContentChangeEvent {
            range: Some(range),
            range_length: None,
            text: text.to_string(),
        });
    }

    pub fn pending_change(
        &self,
        id: BufferId,
        revision: u64,
        after: Rope,
    ) -> Option<DocumentChange> {
        let pending = self.pending_changes.get(&id)?;
        (pending.after_revision == revision
            && self.last_notified_revision(id) == Some(pending.before_revision))
        .then(|| DocumentChange {
            before: pending.before.clone(),
            after,
            changes: pending.changes.clone(),
        })
    }

    /// Returns whether the exact revision has already been delivered.
    pub fn is_revision_notified(&self, id: BufferId, revision: u64) -> bool {
        self.last_notified_revision(id) == Some(revision)
    }

    /// Removes tracked revision for a closed buffer.
    pub fn forget_buffer(&mut self, id: BufferId) -> Option<u64> {
        self.standard_line_revisions.remove(&id);
        self.pending_changes.remove(&id);
        self.notified_revisions.remove(&id)
    }
}

fn standard_lsp_lines(chars: impl Iterator<Item = char>) -> bool {
    let mut chars = chars;
    while let Some(ch) = chars.next() {
        match ch {
            '\r' if chars.next() != Some('\n') => return false,
            '\u{000B}' | '\u{000C}' | '\u{0085}' | '\u{2028}' | '\u{2029}' => return false,
            _ => {}
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::LspCoordinator;
    use crate::buffer::Buffer;

    #[test]
    fn canonical_edits_merge_insertions_and_reject_revision_gaps() {
        use crate::{
            lsp::{Position, Range},
            undo::{TextPosition, TextRange},
        };
        let mut buffer = Buffer::new(Some("fixture.rs".into()), "😀value\r\nnext".into());
        let id = buffer.id();
        let mut coordinator = LspCoordinator::with_buffers(std::slice::from_ref(&buffer));
        for text in ["λ", "x"] {
            let revision = buffer.revision();
            let before = coordinator
                .requires_before_snapshot(id, revision)
                .then(|| buffer.contents_snapshot());
            assert_eq!(before.is_some(), text == "λ");
            let offset = if text == "λ" { 1 } else { 2 };
            let position = TextPosition::new(0, offset);
            let start = buffer.position_to_lsp(position);
            buffer.replace_range_raw(TextRange::insertion(position), text);
            coordinator.record_edit(
                id,
                revision,
                buffer.revision(),
                before,
                Range { start, end: start },
                text,
            );
        }
        let change = coordinator
            .pending_change(id, buffer.revision(), buffer.contents_snapshot())
            .unwrap();
        assert_eq!(change.changes.len(), 1);
        assert_eq!(change.changes[0].text, "λx");
        assert_eq!(
            change.changes[0].range.as_ref().unwrap().start,
            Position {
                line: 0,
                character: 2
            }
        );
        assert_eq!(
            buffer.position_to_lsp(TextPosition::new(1, 0)),
            Position {
                line: 1,
                character: 0
            }
        );
        buffer.insert_str(0, 0, "raw");
        assert!(coordinator
            .pending_change(id, buffer.revision(), buffer.contents_snapshot())
            .is_none());
        coordinator.record_notified_revision(id, buffer.revision());
        assert!(coordinator.pending_changes.is_empty());
    }

    #[test]
    fn unusual_line_separators_keep_the_full_text_fallback() {
        use crate::{
            lsp::Range,
            undo::{TextPosition, TextRange},
        };
        for source in ["one\rtwo", "one\u{2028}two", "one\u{0085}two"] {
            let mut buffer = Buffer::new(Some("fixture.rs".into()), source.into());
            let id = buffer.id();
            let mut coordinator = LspCoordinator::with_buffers(std::slice::from_ref(&buffer));
            let before = buffer.contents_snapshot();
            let revision = buffer.revision();
            let position = TextPosition::new(1, 0);
            let start = buffer.position_to_lsp(position);
            buffer.replace_range_raw(TextRange::insertion(position), "x");
            coordinator.record_edit(
                id,
                revision,
                buffer.revision(),
                Some(before),
                Range { start, end: start },
                "x",
            );
            assert!(coordinator
                .pending_change(id, buffer.revision(), buffer.contents_snapshot())
                .is_none());
        }
    }

    #[test]
    fn tracks_documents_and_seeded_buffer_revisions() {
        let buffer = Buffer::new(None, "text".to_string());
        let id = buffer.id();
        let revision = buffer.revision();
        let mut coordinator = LspCoordinator::with_buffers(&[buffer]);

        assert!(coordinator.is_revision_notified(id, revision));
        assert!(coordinator.mark_document_opened("file:///tmp/example.rs"));
        assert!(!coordinator.mark_document_opened("file:///tmp/example.rs"));
        assert!(coordinator.mark_document_closed("file:///tmp/example.rs"));
        assert_eq!(coordinator.forget_buffer(id), Some(revision));
    }
}
