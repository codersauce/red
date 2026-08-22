//! Shared replacement accounting beneath editor-owned and embedded transactions.

use crate::{
    buffer::Buffer,
    undo::{AppliedTextEdit, TextRange},
};

/// Records and applies one replacement inside an already-open undo transaction.
///
/// Hosts remain responsible for anchor maintenance, dirty-state refresh after commit,
/// external notifications, and repainting. Returning the concrete applied coordinates
/// allows the main editor to update marks synchronously without duplicating mutation.
pub(crate) fn apply_transactional_replacement(
    buffer: &mut Buffer,
    range: TextRange,
    replacement: &str,
) -> Option<AppliedTextEdit> {
    let start_char = buffer.position_to_char_idx(range.start);
    let end_char = buffer.position_to_char_idx(range.end);
    let previous = buffer.text_in_char_range(start_char, end_char);
    if previous == replacement {
        return None;
    }
    assert!(
        buffer.undo_history.is_transaction_active(),
        "editor content mutations must occur inside an edit transaction"
    );

    let edit = AppliedTextEdit {
        start_char,
        end_char,
        new_char_len: replacement.chars().count(),
    };
    buffer.replace_char_range_raw(start_char, end_char, replacement);
    buffer
        .undo_history
        .record_replace(range, edit.start_char, previous, replacement.to_string());
    Some(edit)
}
