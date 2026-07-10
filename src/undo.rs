use std::collections::HashMap;

use crate::buffer::Buffer;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum EditOrigin {
    User,
    Agent { session_id: String, turn_id: String },
    Plugin { name: String },
    Lsp { server: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct TextPosition {
    pub line: usize,
    pub character: usize,
}

impl TextPosition {
    pub fn new(line: usize, character: usize) -> Self {
        Self { line, character }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TextEdit {
    Replace {
        range: TextRange,
        start_char: usize,
        old_text: String,
        new_text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevertEdit {
    pub start_char: usize,
    pub end_char: usize,
    pub replacement: String,
}

/// One concrete replacement applied while traversing undo history, expressed in
/// the buffer's character coordinates immediately before that replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppliedTextEdit {
    pub start_char: usize,
    pub end_char: usize,
    pub new_char_len: usize,
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
    pub id: String,
    pub timestamp_ms: u128,
    pub origin: EditOrigin,
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
        origin: EditOrigin,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            origin,
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
struct UndoNode {
    transaction: EditTransaction,
    parent: Option<usize>,
    children: Vec<usize>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct UndoTreeEntry {
    pub index: usize,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub current: bool,
    pub transaction_id: String,
    pub label: String,
    pub origin: EditOrigin,
    pub timestamp_ms: u128,
    pub edits: Vec<TextEdit>,
}

#[derive(Debug, Clone)]
pub struct UndoHistory {
    nodes: Vec<UndoNode>,
    root_children: Vec<usize>,
    current: Option<usize>,
    branch_selection: HashMap<usize, usize>,
    active_transaction: Option<EditTransaction>,
    current_revision: u64,
    saved_revision: u64,
    next_revision: u64,
}

impl Default for UndoHistory {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            root_children: Vec::new(),
            current: None,
            branch_selection: HashMap::new(),
            active_transaction: None,
            current_revision: 0,
            saved_revision: 0,
            next_revision: 1,
        }
    }
}

impl UndoHistory {
    pub fn begin_transaction(&mut self, label: impl Into<String>, before_cursor: CursorSnapshot) {
        self.begin_transaction_with_origin(label, before_cursor, EditOrigin::User);
    }

    pub fn begin_transaction_with_origin(
        &mut self,
        label: impl Into<String>,
        before_cursor: CursorSnapshot,
        origin: EditOrigin,
    ) {
        if self.active_transaction.is_none() {
            self.active_transaction = Some(EditTransaction::new(
                label,
                before_cursor,
                self.current_revision,
                origin,
            ));
        }
    }

    #[must_use]
    pub fn latest_transaction(&self) -> Option<&EditTransaction> {
        self.current
            .and_then(|index| self.nodes.get(index))
            .map(|node| &node.transaction)
    }

    #[must_use]
    pub fn undo_tree(&self) -> Vec<UndoTreeEntry> {
        self.nodes
            .iter()
            .enumerate()
            .map(|(index, node)| UndoTreeEntry {
                index,
                parent: node.parent,
                children: node.children.clone(),
                current: self.current == Some(index),
                transaction_id: node.transaction.id.clone(),
                label: node.transaction.label.clone(),
                origin: node.transaction.origin.clone(),
                timestamp_ms: node.transaction.timestamp_ms,
                edits: node.transaction.edits.clone(),
            })
            .collect()
    }

    pub fn record_replace(
        &mut self,
        range: TextRange,
        start_char: usize,
        old_text: String,
        new_text: String,
    ) {
        if old_text == new_text {
            return;
        }

        if let Some(transaction) = &mut self.active_transaction {
            transaction.edits.push(TextEdit::Replace {
                range,
                start_char,
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
        let parent = self.current;
        let index = self.nodes.len();
        self.nodes.push(UndoNode {
            transaction,
            parent,
            children: Vec::new(),
        });
        let children = if let Some(parent) = parent {
            &mut self.nodes[parent].children
        } else {
            &mut self.root_children
        };
        children.push(index);
        self.branch_selection
            .insert(branch_key(parent), children.len() - 1);
        self.current = Some(index);
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

    pub fn select_next_branch(&mut self) -> Option<(usize, usize)> {
        self.select_branch(/*delta*/ 1)
    }

    pub fn select_previous_branch(&mut self) -> Option<(usize, usize)> {
        self.select_branch(/*delta*/ -1)
    }

    pub fn prepare_revert(
        &self,
        transaction_id: &str,
        buffer: &Buffer,
    ) -> anyhow::Result<Vec<RevertEdit>> {
        let target = self
            .nodes
            .iter()
            .position(|node| node.transaction.id == transaction_id)
            .ok_or_else(|| anyhow::anyhow!("unknown transaction {transaction_id}"))?;
        let mut descendants = Vec::new();
        let mut cursor = self.current;
        while let Some(index) = cursor {
            if index == target {
                break;
            }
            descendants.push(index);
            cursor = self.nodes[index].parent;
        }
        anyhow::ensure!(
            cursor == Some(target),
            "transaction is not on the current undo branch"
        );
        descendants.reverse();

        let target_edits = &self.nodes[target].transaction.edits;
        let mut revert = Vec::with_capacity(target_edits.len());
        for (edit_index, edit) in target_edits.iter().enumerate() {
            let TextEdit::Replace {
                start_char,
                old_text,
                new_text,
                ..
            } = edit;
            let mut start = *start_char;
            let mut end = start + new_text.chars().count();
            for later in target_edits.iter().skip(edit_index + 1).chain(
                descendants
                    .iter()
                    .flat_map(|index| &self.nodes[*index].transaction.edits),
            ) {
                let TextEdit::Replace {
                    start_char: later_start,
                    old_text: later_old,
                    new_text: later_new,
                    ..
                } = later;
                let later_end = later_start + later_old.chars().count();
                anyhow::ensure!(
                    !ranges_overlap(start, end, *later_start, later_end),
                    "transaction post-image was changed by a later edit"
                );
                start =
                    transform_char_index(start, *later_start, later_end, later_new.chars().count());
                end = transform_char_index(end, *later_start, later_end, later_new.chars().count());
            }
            anyhow::ensure!(
                buffer
                    .contents()
                    .chars()
                    .skip(start)
                    .take(end.saturating_sub(start))
                    .collect::<String>()
                    == *new_text,
                "transaction post-image no longer matches the buffer"
            );
            revert.push(RevertEdit {
                start_char: start,
                end_char: end,
                replacement: old_text.clone(),
            });
        }
        Ok(revert)
    }

    fn select_branch(&mut self, delta: isize) -> Option<(usize, usize)> {
        let child_count = self.children_for(self.current).len();
        if child_count == 0 {
            return None;
        }
        let key = branch_key(self.current);
        let selected = self
            .branch_selection
            .get(&key)
            .copied()
            .unwrap_or(child_count - 1);
        let next = selected
            .saturating_add_signed(delta)
            .min(child_count.saturating_sub(1));
        self.branch_selection.insert(key, next);
        Some((next + 1, child_count))
    }

    fn children_for(&self, parent: Option<usize>) -> &[usize] {
        parent.map_or(&self.root_children, |index| &self.nodes[index].children)
    }

    pub fn undo(&mut self, buffer: &mut Buffer) -> Option<(CursorSnapshot, Vec<AppliedTextEdit>)> {
        let index = self.current?;
        let transaction = self.nodes[index].transaction.clone();
        let mut applied_edits = Vec::with_capacity(transaction.edits.len());
        for edit in transaction.edits.iter().rev() {
            match edit {
                TextEdit::Replace {
                    range,
                    old_text,
                    new_text,
                    ..
                } => {
                    let current_range = buffer.range_for_text(range.start, new_text);
                    applied_edits.push(AppliedTextEdit {
                        start_char: buffer.position_to_char_idx(current_range.start),
                        end_char: buffer.position_to_char_idx(current_range.end),
                        new_char_len: old_text.chars().count(),
                    });
                    buffer.replace_range_raw(current_range, old_text);
                }
            }
        }
        let cursor = transaction.before_cursor;
        self.current_revision = transaction.before_revision;
        self.current = self.nodes[index].parent;
        Some((cursor, applied_edits))
    }

    pub fn redo(&mut self, buffer: &mut Buffer) -> Option<(CursorSnapshot, Vec<AppliedTextEdit>)> {
        let children = self.children_for(self.current);
        let selected = self
            .branch_selection
            .get(&branch_key(self.current))
            .copied()
            .unwrap_or_else(|| children.len().saturating_sub(1));
        let index = *children.get(selected)?;
        let transaction = self.nodes[index].transaction.clone();
        let mut applied_edits = Vec::with_capacity(transaction.edits.len());
        for edit in &transaction.edits {
            match edit {
                TextEdit::Replace {
                    range, new_text, ..
                } => {
                    applied_edits.push(AppliedTextEdit {
                        start_char: buffer.position_to_char_idx(range.start),
                        end_char: buffer.position_to_char_idx(range.end),
                        new_char_len: new_text.chars().count(),
                    });
                    buffer.replace_range_raw(*range, new_text);
                }
            }
        }
        let cursor = transaction.after_cursor;
        self.current_revision = transaction.after_revision;
        self.current = Some(index);
        Some((cursor, applied_edits))
    }
}

fn branch_key(parent: Option<usize>) -> usize {
    parent.unwrap_or(usize::MAX)
}

fn ranges_overlap(
    left_start: usize,
    left_end: usize,
    right_start: usize,
    right_end: usize,
) -> bool {
    if left_start == left_end || right_start == right_end {
        return left_start <= right_end && right_start <= left_end;
    }
    left_start < right_end && right_start < left_end
}

fn transform_char_index(
    index: usize,
    edit_start: usize,
    edit_end: usize,
    replacement_len: usize,
) -> usize {
    if index <= edit_start {
        index
    } else if index >= edit_end {
        index
            .saturating_sub(edit_end.saturating_sub(edit_start))
            .saturating_add(replacement_len)
    } else {
        edit_start.saturating_add(replacement_len)
    }
}
