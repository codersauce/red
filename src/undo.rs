/// Undo/Redo system for the text editor
///
/// This module provides a comprehensive undo/redo implementation with:
/// - Per-buffer undo history (each buffer has its own undo/redo stacks)
/// - Automatic grouping of related changes (e.g., insert mode batching)
/// - Memory-efficient storage with configurable limits
/// - Support for all editing operations

use serde::{Deserialize, Serialize};

/// Default maximum number of undo groups to keep in history
pub const DEFAULT_MAX_UNDO_HISTORY: usize = 1000;

/// Represents a single atomic change that can be undone/redone
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum UndoChange {
    /// Insert a character at position
    InsertChar { x: usize, y: usize, c: char },
    /// Delete the character at position (stores what was deleted)
    DeleteChar { x: usize, y: usize, c: char },
    /// Insert a string at position
    InsertString { x: usize, y: usize, s: String },
    /// Delete a string at position (stores what was deleted)
    DeleteString { x: usize, y: usize, s: String },
    /// Insert a line at position
    InsertLine { y: usize, content: String },
    /// Delete a line at position (stores the deleted content)
    DeleteLine { y: usize, content: String },
    /// Replace a line's content
    ReplaceLine { y: usize, old: String, new: String },
    /// Delete a range of text (stores what was deleted)
    DeleteRange {
        x0: usize,
        y0: usize,
        x1: usize,
        y1: usize,
        content: String,
    },
}

impl UndoChange {
    /// Returns the inverse of this change (for undo operations)
    pub fn inverse(&self) -> UndoChange {
        match self {
            UndoChange::InsertChar { x, y, c } => UndoChange::DeleteChar {
                x: *x,
                y: *y,
                c: *c,
            },
            UndoChange::DeleteChar { x, y, c } => UndoChange::InsertChar {
                x: *x,
                y: *y,
                c: *c,
            },
            UndoChange::InsertString { x, y, s } => UndoChange::DeleteString {
                x: *x,
                y: *y,
                s: s.clone(),
            },
            UndoChange::DeleteString { x, y, s } => UndoChange::InsertString {
                x: *x,
                y: *y,
                s: s.clone(),
            },
            UndoChange::InsertLine { y, content } => UndoChange::DeleteLine {
                y: *y,
                content: content.clone(),
            },
            UndoChange::DeleteLine { y, content } => UndoChange::InsertLine {
                y: *y,
                content: content.clone(),
            },
            UndoChange::ReplaceLine { y, old, new } => UndoChange::ReplaceLine {
                y: *y,
                old: new.clone(),
                new: old.clone(),
            },
            UndoChange::DeleteRange {
                x0,
                y0,
                x1: _,
                y1: _,
                content,
            } => {
                // The inverse of deleting a range is inserting the content back
                // This will be handled specially by the undo executor
                UndoChange::InsertString {
                    x: *x0,
                    y: *y0,
                    s: content.clone(),
                }
            }
        }
    }
}

/// A group of changes that should be undone/redone together
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoGroup {
    /// The individual changes in this group (in order of execution)
    pub changes: Vec<UndoChange>,
    /// Cursor position before the changes were made
    pub cursor_before: (usize, usize),
    /// Cursor position after the changes were made
    pub cursor_after: (usize, usize),
}

impl UndoGroup {
    /// Create a new undo group with cursor position
    pub fn new(cursor_pos: (usize, usize)) -> Self {
        Self {
            changes: Vec::new(),
            cursor_before: cursor_pos,
            cursor_after: cursor_pos,
        }
    }

    /// Add a change to this group
    pub fn push(&mut self, change: UndoChange) {
        self.changes.push(change);
    }

    /// Check if the group has any changes
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Get the number of changes in this group
    pub fn len(&self) -> usize {
        self.changes.len()
    }

    /// Set the final cursor position after all changes
    pub fn set_cursor_after(&mut self, pos: (usize, usize)) {
        self.cursor_after = pos;
    }

    /// Create the inverse group for redo
    pub fn inverse(&self) -> UndoGroup {
        UndoGroup {
            // Reverse the order and invert each change
            changes: self.changes.iter().rev().map(|c| c.inverse()).collect(),
            cursor_before: self.cursor_after,
            cursor_after: self.cursor_before,
        }
    }
}

/// Manages undo/redo history for a buffer
#[derive(Debug)]
pub struct UndoHistory {
    /// Stack of undo groups (most recent at the end)
    undo_stack: Vec<UndoGroup>,
    /// Stack of redo groups (most recent at the end)
    redo_stack: Vec<UndoGroup>,
    /// Maximum number of undo groups to keep
    max_items: usize,
    /// Current group being built (for batching changes)
    current_group: Option<UndoGroup>,
    /// Whether we're currently in a grouped operation (e.g., insert mode)
    in_group: bool,
}

impl Default for UndoHistory {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_UNDO_HISTORY)
    }
}

impl UndoHistory {
    /// Create a new undo history with specified max items
    pub fn new(max_items: usize) -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            max_items,
            current_group: None,
            in_group: false,
        }
    }

    /// Record a single change (creates its own group if not in a grouped operation)
    pub fn record(&mut self, change: UndoChange, cursor_pos: (usize, usize)) {
        // Clear redo stack on any new change
        self.redo_stack.clear();

        if self.in_group {
            // Add to current group
            if let Some(ref mut group) = self.current_group {
                group.push(change);
                group.set_cursor_after(cursor_pos);
            }
        } else {
            // Create a single-change group
            let mut group = UndoGroup::new(cursor_pos);
            group.push(change);
            group.set_cursor_after(cursor_pos);
            self.push_group(group);
        }
    }

    /// Start a new grouped operation (e.g., entering insert mode)
    pub fn start_group(&mut self, cursor_pos: (usize, usize)) {
        // Finish any existing group first
        self.end_group();
        self.current_group = Some(UndoGroup::new(cursor_pos));
        self.in_group = true;
    }

    /// End the current grouped operation
    pub fn end_group(&mut self) {
        if let Some(group) = self.current_group.take() {
            if !group.is_empty() {
                self.push_group(group);
            }
        }
        self.in_group = false;
    }

    /// Check if we're currently in a grouped operation
    pub fn in_group(&self) -> bool {
        self.in_group
    }

    /// Push a completed group onto the undo stack
    fn push_group(&mut self, group: UndoGroup) {
        self.undo_stack.push(group);

        // Enforce max history limit
        while self.undo_stack.len() > self.max_items {
            self.undo_stack.remove(0);
        }
    }

    /// Pop and return the most recent undo group
    /// Returns the inverse group that should be pushed to redo stack after execution
    pub fn undo(&mut self) -> Option<UndoGroup> {
        // End any current group first
        self.end_group();

        self.undo_stack.pop().inspect(|group| {
            // Push the inverse to redo stack
            self.redo_stack.push(group.inverse());
        })
    }

    /// Pop and return the most recent redo group
    pub fn redo(&mut self) -> Option<UndoGroup> {
        // End any current group first
        self.end_group();

        self.redo_stack.pop().inspect(|group| {
            // Push the inverse back to undo stack
            self.undo_stack.push(group.inverse());
        })
    }

    /// Check if undo is available
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty() || self.current_group.as_ref().is_some_and(|g| !g.is_empty())
    }

    /// Check if redo is available
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Clear all history
    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.current_group = None;
        self.in_group = false;
    }

    /// Get the number of undo groups available
    pub fn undo_count(&self) -> usize {
        self.undo_stack.len()
    }

    /// Get the number of redo groups available
    pub fn redo_count(&self) -> usize {
        self.redo_stack.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_change_undo() {
        let mut history = UndoHistory::new(100);

        // Record an insertion
        history.record(
            UndoChange::InsertChar { x: 0, y: 0, c: 'a' },
            (0, 0),
        );

        assert!(history.can_undo());
        assert!(!history.can_redo());

        // Undo should return the group
        let group = history.undo().unwrap();
        assert_eq!(group.changes.len(), 1);

        // Now redo should be available
        assert!(!history.can_undo());
        assert!(history.can_redo());

        // Redo should work
        let redo_group = history.redo().unwrap();
        assert_eq!(redo_group.changes.len(), 1);

        assert!(history.can_undo());
        assert!(!history.can_redo());
    }

    #[test]
    fn test_grouped_changes() {
        let mut history = UndoHistory::new(100);

        // Start a group (like entering insert mode)
        history.start_group((0, 0));

        // Record multiple changes
        history.record(UndoChange::InsertChar { x: 0, y: 0, c: 'h' }, (1, 0));
        history.record(UndoChange::InsertChar { x: 1, y: 0, c: 'i' }, (2, 0));

        // End the group
        history.end_group();

        // Should be just one undo operation
        assert_eq!(history.undo_count(), 1);

        let group = history.undo().unwrap();
        assert_eq!(group.changes.len(), 2);
    }

    #[test]
    fn test_new_change_clears_redo() {
        let mut history = UndoHistory::new(100);

        history.record(UndoChange::InsertChar { x: 0, y: 0, c: 'a' }, (0, 0));
        history.undo();

        assert!(history.can_redo());

        // New change should clear redo
        history.record(UndoChange::InsertChar { x: 0, y: 0, c: 'b' }, (0, 0));

        assert!(!history.can_redo());
    }

    #[test]
    fn test_max_history_limit() {
        let mut history = UndoHistory::new(3);

        for i in 0..5 {
            history.record(
                UndoChange::InsertChar { x: i, y: 0, c: 'x' },
                (i, 0),
            );
        }

        // Should only have 3 items
        assert_eq!(history.undo_count(), 3);
    }

    #[test]
    fn test_change_inverse() {
        let insert = UndoChange::InsertChar { x: 0, y: 0, c: 'a' };
        let delete = insert.inverse();

        assert!(matches!(delete, UndoChange::DeleteChar { x: 0, y: 0, c: 'a' }));

        let inverse_back = delete.inverse();
        assert_eq!(insert, inverse_back);
    }
}
