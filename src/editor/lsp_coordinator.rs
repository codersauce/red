//! LSP client coordination and document synchronization tracking for the Red editor.

use crate::buffer::{Buffer, BufferId};
use std::collections::{HashMap, HashSet};

/// Coordinates LSP client state, opened workspace document tracking, and buffer revision delivery.
#[derive(Debug, Default)]
pub struct LspCoordinator {
    /// URI strings of documents currently reported as open to LSP servers.
    opened_documents: HashSet<String>,
    /// Latest buffer revision delivered to LSP servers per buffer ID.
    notified_revisions: HashMap<BufferId, u64>,
}

impl LspCoordinator {
    /// Creates a coordinator seeded with each open buffer's current revision.
    pub fn with_buffers(buffers: &[Buffer]) -> Self {
        Self {
            opened_documents: HashSet::new(),
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
    }

    /// Seeds a revision only when the buffer has not been tracked before.
    pub fn ensure_notified_revision(&mut self, id: BufferId, revision: u64) {
        self.notified_revisions.entry(id).or_insert(revision);
    }

    /// Returns whether the exact revision has already been delivered.
    pub fn is_revision_notified(&self, id: BufferId, revision: u64) -> bool {
        self.last_notified_revision(id) == Some(revision)
    }

    /// Removes tracked revision for a closed buffer.
    pub fn forget_buffer(&mut self, id: BufferId) -> Option<u64> {
        self.notified_revisions.remove(&id)
    }
}

#[cfg(test)]
mod tests {
    use super::LspCoordinator;
    use crate::buffer::Buffer;

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
