use crate::buffer::Buffer;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextPosition {
    pub line: usize,
    pub character: usize,
}

impl TextPosition {
    pub fn new(line: usize, character: usize) -> Self {
        Self { line, character }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextRange {
    pub start: TextPosition,
    pub end: TextPosition,
}

impl TextRange {
    pub fn new(start: TextPosition, end: TextPosition) -> Self {
        Self { start, end }
    }

    pub fn insertion(position: TextPosition) -> Self {
        Self {
            start: position,
            end: position,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextEdit {
    Replace {
        range: TextRange,
        old_text: String,
        new_text: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CursorSnapshot {
    pub x: usize,
    pub y: usize,
    pub vtop: usize,
}

impl CursorSnapshot {
    pub fn new(x: usize, y: usize, vtop: usize) -> Self {
        Self { x, y, vtop }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditTransaction {
    pub label: String,
    pub edits: Vec<TextEdit>,
    pub before_cursor: CursorSnapshot,
    pub after_cursor: CursorSnapshot,
    before_revision: u64,
    after_revision: u64,
}

impl EditTransaction {
    pub fn new(
        label: impl Into<String>,
        before_cursor: CursorSnapshot,
        before_revision: u64,
    ) -> Self {
        Self {
            label: label.into(),
            edits: Vec::new(),
            before_cursor,
            after_cursor: before_cursor,
            before_revision,
            after_revision: before_revision,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct UndoHistory {
    undo_stack: Vec<EditTransaction>,
    redo_stack: Vec<EditTransaction>,
    active_transaction: Option<EditTransaction>,
    current_revision: u64,
    saved_revision: u64,
    next_revision: u64,
}

impl Default for UndoHistory {
    fn default() -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            active_transaction: None,
            current_revision: 0,
            saved_revision: 0,
            next_revision: 1,
        }
    }
}

impl UndoHistory {
    pub fn begin_transaction(&mut self, label: impl Into<String>, before_cursor: CursorSnapshot) {
        if self.active_transaction.is_none() {
            self.active_transaction = Some(EditTransaction::new(
                label,
                before_cursor,
                self.current_revision,
            ));
        }
    }

    pub fn record_replace(&mut self, range: TextRange, old_text: String, new_text: String) {
        if old_text == new_text {
            return;
        }

        if let Some(transaction) = &mut self.active_transaction {
            transaction.edits.push(TextEdit::Replace {
                range,
                old_text,
                new_text,
            });
        }
    }

    pub fn commit_transaction(&mut self, after_cursor: CursorSnapshot) -> bool {
        let Some(mut transaction) = self.active_transaction.take() else {
            return false;
        };

        if transaction.is_empty() {
            return false;
        }

        transaction.after_cursor = after_cursor;
        transaction.after_revision = self.next_revision;
        self.next_revision += 1;
        self.current_revision = transaction.after_revision;
        self.undo_stack.push(transaction);
        self.redo_stack.clear();
        true
    }

    pub fn cancel_transaction_if_empty(&mut self) {
        if self
            .active_transaction
            .as_ref()
            .is_some_and(EditTransaction::is_empty)
        {
            self.active_transaction = None;
        }
    }

    pub fn is_transaction_active(&self) -> bool {
        self.active_transaction.is_some()
    }

    pub fn mark_saved(&mut self) {
        self.saved_revision = self.current_revision;
    }

    pub fn is_dirty(&self) -> bool {
        self.current_revision != self.saved_revision
    }

    pub fn undo(&mut self, buffer: &mut Buffer) -> Option<CursorSnapshot> {
        let transaction = self.undo_stack.pop()?;

        for edit in transaction.edits.iter().rev() {
            match edit {
                TextEdit::Replace {
                    range,
                    old_text,
                    new_text,
                } => {
                    let current_range = buffer.range_for_text(range.start, new_text);
                    buffer.replace_range_raw(current_range, old_text);
                }
            }
        }

        let cursor = transaction.before_cursor;
        self.current_revision = transaction.before_revision;
        self.redo_stack.push(transaction);
        Some(cursor)
    }

    pub fn redo(&mut self, buffer: &mut Buffer) -> Option<CursorSnapshot> {
        let transaction = self.redo_stack.pop()?;

        for edit in &transaction.edits {
            match edit {
                TextEdit::Replace {
                    range, new_text, ..
                } => {
                    buffer.replace_range_raw(*range, new_text);
                }
            }
        }

        let cursor = transaction.after_cursor;
        self.current_revision = transaction.after_revision;
        self.undo_stack.push(transaction);
        Some(cursor)
    }
}
