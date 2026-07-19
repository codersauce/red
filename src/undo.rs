use std::collections::{HashMap, HashSet};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        let node_count = self.nodes.len();
        let mut greatest_revision = self.current_revision.max(self.saved_revision);
        let mut transaction_ids = HashSet::with_capacity(node_count);
        let root_revision = 0;
        if let Some(current) = self.current {
            anyhow::ensure!(
                current < node_count,
                "undo-tree current index {current} is outside {node_count} nodes"
            );
        }

        let mut incoming = vec![0usize; node_count];
        for &root in &self.root_children {
            anyhow::ensure!(
                root < node_count,
                "undo-tree root child index {root} is outside {node_count} nodes"
            );
            anyhow::ensure!(
                self.nodes[root].parent.is_none(),
                "undo-tree root child {root} has a parent"
            );
            anyhow::ensure!(
                self.nodes[root].transaction.before_revision == root_revision,
                "undo-tree root child {root} starts at an inconsistent revision"
            );
            incoming[root] += 1;
            anyhow::ensure!(
                incoming[root] == 1,
                "undo-tree root child {root} is duplicated"
            );
        }

        let mut previous_after_revision = root_revision;
        for (index, node) in self.nodes.iter().enumerate() {
            anyhow::ensure!(
                !node.transaction.id.is_empty(),
                "undo-tree node {index} has an empty transaction id"
            );
            anyhow::ensure!(
                transaction_ids.insert(node.transaction.id.as_str()),
                "undo-tree node {index} has a duplicate transaction id"
            );
            greatest_revision = greatest_revision
                .max(node.transaction.before_revision)
                .max(node.transaction.after_revision);
            anyhow::ensure!(
                node.transaction.after_revision > node.transaction.before_revision,
                "undo-tree node {index} does not advance its revision"
            );
            anyhow::ensure!(
                node.transaction.after_revision > previous_after_revision,
                "undo-tree node {index} reuses or precedes an earlier revision"
            );
            previous_after_revision = node.transaction.after_revision;
            if let Some(parent) = node.parent {
                anyhow::ensure!(
                    parent < index,
                    "undo-tree node {index} has an invalid parent index {parent}"
                );
                anyhow::ensure!(
                    node.transaction.before_revision
                        == self.nodes[parent].transaction.after_revision,
                    "undo-tree node {index} starts at a different revision than parent {parent}"
                );
            }
            for &child in &node.children {
                anyhow::ensure!(
                    child < node_count,
                    "undo-tree node {index} has an out-of-range child index {child}"
                );
                anyhow::ensure!(
                    child > index,
                    "undo-tree node {index} has a cyclic child index {child}"
                );
                anyhow::ensure!(
                    self.nodes[child].parent == Some(index),
                    "undo-tree child {child} does not reference parent {index}"
                );
                incoming[child] += 1;
                anyhow::ensure!(
                    incoming[child] == 1,
                    "undo-tree child {child} is referenced more than once"
                );
            }
            validate_transaction(&node.transaction, index)?;
        }

        for (index, count) in incoming.into_iter().enumerate() {
            anyhow::ensure!(count == 1, "undo-tree node {index} is unreachable");
        }

        for (&parent, &selection) in &self.branch_selection {
            let children = if parent == usize::MAX {
                &self.root_children
            } else {
                anyhow::ensure!(
                    parent < node_count,
                    "undo-tree branch parent index {parent} is outside {node_count} nodes"
                );
                &self.nodes[parent].children
            };
            anyhow::ensure!(
                selection < children.len(),
                "undo-tree branch selection {selection} is outside {} children",
                children.len()
            );
        }

        let selected_revision = self.current.map_or(root_revision, |current| {
            self.nodes[current].transaction.after_revision
        });
        anyhow::ensure!(
            self.current_revision == selected_revision,
            "undo-tree current revision {} does not match the selected node revision {selected_revision}",
            self.current_revision
        );
        anyhow::ensure!(
            self.saved_revision == root_revision
                || self
                    .nodes
                    .iter()
                    .any(|node| node.transaction.after_revision == self.saved_revision),
            "undo-tree saved revision {} does not belong to the tree",
            self.saved_revision
        );

        if let Some(transaction) = &self.active_transaction {
            greatest_revision = greatest_revision
                .max(transaction.before_revision)
                .max(transaction.after_revision);
            anyhow::ensure!(
                transaction.before_revision == self.current_revision
                    && transaction.after_revision == self.current_revision,
                "undo-tree active transaction does not start at the current revision"
            );
            validate_transaction(transaction, node_count)?;
        }
        anyhow::ensure!(
            self.next_revision > greatest_revision && self.next_revision < u64::MAX,
            "undo-tree next revision {} is invalid after revision {greatest_revision}",
            self.next_revision
        );
        Ok(())
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
                let replacement_len = later_new.chars().count();
                start = transform_char_index(
                    start,
                    *later_start,
                    later_end,
                    replacement_len,
                    IndexAffinity::Right,
                );
                end = transform_char_index(
                    end,
                    *later_start,
                    later_end,
                    replacement_len,
                    IndexAffinity::Left,
                );
            }
            anyhow::ensure!(
                buffer.text_in_char_range_matches(start, end, new_text),
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
        let transaction = &self.nodes[index].transaction;
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
        let transaction = &self.nodes[index].transaction;
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

fn validate_transaction(transaction: &EditTransaction, node: usize) -> anyhow::Result<()> {
    for (edit_index, edit) in transaction.edits.iter().enumerate() {
        let TextEdit::Replace {
            range,
            start_char,
            old_text,
            new_text,
        } = edit;
        anyhow::ensure!(
            (range.start.line, range.start.character) <= (range.end.line, range.end.character),
            "undo-tree node {node} edit {edit_index} has an inverted range"
        );
        anyhow::ensure!(
            start_char.checked_add(old_text.chars().count()).is_some()
                && start_char.checked_add(new_text.chars().count()).is_some(),
            "undo-tree node {node} edit {edit_index} overflows its character range"
        );
        let mut line = range.start.line;
        let mut character = range.start.character;
        for value in new_text.chars() {
            if value == '\n' {
                line = line.checked_add(1).ok_or_else(|| {
                    anyhow::anyhow!(
                        "undo-tree node {node} edit {edit_index} overflows its line range"
                    )
                })?;
                character = 0;
            } else {
                character = character.checked_add(1).ok_or_else(|| {
                    anyhow::anyhow!(
                        "undo-tree node {node} edit {edit_index} overflows its column range"
                    )
                })?;
            }
        }
    }
    Ok(())
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
    match (left_start == left_end, right_start == right_end) {
        (true, true) => left_start == right_start,
        (true, false) => right_start < left_start && left_start < right_end,
        (false, true) => left_start < right_start && right_start < left_end,
        (false, false) => left_start < right_end && right_start < left_end,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IndexAffinity {
    Left,
    Right,
}

fn transform_char_index(
    index: usize,
    edit_start: usize,
    edit_end: usize,
    replacement_len: usize,
    affinity: IndexAffinity,
) -> usize {
    if edit_start == edit_end && index == edit_start {
        return match affinity {
            IndexAffinity::Left => index,
            IndexAffinity::Right => index.saturating_add(replacement_len),
        };
    }
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

#[cfg(test)]
mod tests {
    use super::{CursorSnapshot, TextPosition, TextRange, UndoHistory};

    fn commit_insertion(history: &mut UndoHistory, character: usize, text: &str) {
        history.begin_transaction("insert", CursorSnapshot::default());
        history.record_replace(
            TextRange::insertion(TextPosition::new(0, character)),
            character,
            String::new(),
            text.to_string(),
        );
        assert!(history.commit_transaction(CursorSnapshot::default()));
    }

    #[test]
    fn validate_rejects_disconnected_undo_revisions() {
        let mut history = UndoHistory::default();
        commit_insertion(&mut history, 0, "a");
        commit_insertion(&mut history, 1, "b");
        history.validate().unwrap();

        let mut invalid_current = history.clone();
        invalid_current.current_revision = 1;
        assert!(invalid_current.validate().is_err());

        let mut invalid_child = history.clone();
        invalid_child.nodes[1].transaction.before_revision = 0;
        assert!(invalid_child.validate().is_err());

        let mut invalid_transaction = history.clone();
        invalid_transaction.nodes[1].transaction.after_revision =
            invalid_transaction.nodes[1].transaction.before_revision;
        assert!(invalid_transaction.validate().is_err());

        let mut invalid_saved = history.clone();
        invalid_saved.saved_revision = 3;
        invalid_saved.next_revision = 4;
        assert!(invalid_saved.validate().is_err());

        let mut active = history.clone();
        active.begin_transaction("insert", CursorSnapshot::default());
        active.validate().unwrap();
        active.active_transaction.as_mut().unwrap().before_revision = 0;
        assert!(active.validate().is_err());

        let mut active = history;
        active.begin_transaction("insert", CursorSnapshot::default());
        active.active_transaction.as_mut().unwrap().after_revision = 1;
        assert!(active.validate().is_err());
    }

    #[test]
    fn validate_rejects_disconnected_root_revisions() {
        let mut history = UndoHistory::default();
        commit_insertion(&mut history, 0, "a");
        history.current = None;
        history.current_revision = 0;
        commit_insertion(&mut history, 0, "b");
        history.validate().unwrap();

        history.nodes[1].transaction.before_revision = 1;
        assert!(history.validate().is_err());

        history.nodes[1].transaction.before_revision = 0;
        history.nodes[0].transaction.after_revision = 2;
        history.nodes[1].transaction.after_revision = 1;
        history.current_revision = 1;
        assert!(history.validate().is_err());
    }
}
