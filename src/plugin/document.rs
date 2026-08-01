//! Revision-checked editor document requests for external plugins.
//!
//! Plugins describe edits against an exact Red buffer revision and optional textual
//! preimages. The editor applies the batch as one attributed undo transaction. It
//! remains responsible for buffers, dirty state, LSP notifications, and undo history.

use serde::{Deserialize, Serialize};

use crate::undo::TextRange;

/// Read-only document state returned across the plugin host boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentSnapshot {
    pub buffer_index: usize,
    pub path: Option<String>,
    pub revision: u64,
    pub text: String,
}

/// One replacement within an attributed document transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentEdit {
    pub range: TextRange,
    pub text: String,
    /// Optional exact text expected at `range` before the transaction begins.
    pub expected_text: Option<String>,
}
