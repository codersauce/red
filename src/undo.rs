//! Branch-preserving edit transactions, attribution, dirty-state revisions, and replay.
//!
//! One [`EditTransaction`] represents a logical undo step and contains ordered textual
//! replacements plus cursor state before and after the change. [`UndoHistory`] retains
//! sibling children when editing after an undo, so redo follows the selected branch
//! rather than discarding alternate history.
//!
//! [`TextPosition::character`] is a zero-based Unicode scalar index within a line, not a
//! UTF-8 byte, grapheme, terminal column, or LSP UTF-16 offset. Transactions record raw
//! replacements but do not notify external consumers; the editor owns notification,
//! anchor maintenance, rendering, and restoration around replay.

use std::collections::{HashMap, HashSet};

use crate::buffer::Buffer;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
/// Identifies the subsystem responsible for one committed transaction.
pub enum EditOrigin {
    /// A change initiated directly by editor input or an explicit user command.
    User,
    /// An accepted agent proposal attributed to its Codex session and turn.
    Agent {
        /// Codex conversation that produced the proposal.
        session_id: String,
        /// Turn within the conversation that produced the proposal.
        turn_id: String,
    },
    /// A change requested by a named Husk plugin.
    Plugin {
        /// Registered plugin name.
        name: String,
    },
    /// A change returned by a named language server.
    Lsp {
        /// Managed language-server key.
        server: String,
    },
}

/// Zero-based line and Unicode-scalar position used by the canonical edit boundary.
///
/// `character` is not a UTF-8 byte, user-perceived grapheme, terminal column, or UTF-16
/// code-unit offset. Callers crossing one of those boundaries must convert explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct TextPosition {
    /// Zero-based logical line.
    pub line: usize,
    /// Zero-based Unicode scalar index within `line`.
    pub character: usize,
}

impl TextPosition {
    /// Creates a position from a line and Unicode scalar index.
    pub fn new(line: usize, character: usize) -> Self {
        Self { line, character }
    }
}

/// Half-open range in canonical buffer character coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct TextRange {
    /// Inclusive start position.
    pub start: TextPosition,
    /// Exclusive end position.
    pub end: TextPosition,
}

impl TextRange {
    /// Creates a half-open range without reordering its endpoints.
    ///
    /// Callers must supply `start <= end`; passing reversed endpoints will cause later
    /// buffer conversion to clamp or reject the operation rather than selecting backward.
    pub fn new(start: TextPosition, end: TextPosition) -> Self {
        Self { start, end }
    }

    /// Creates an empty range that inserts at `position`.
    pub fn insertion(position: TextPosition) -> Self {
        Self {
            start: position,
            end: position,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
/// Serialized textual operation retained inside an edit transaction.
pub enum TextEdit {
    /// Replaces a half-open range and retains both images for replay and selective revert.
    Replace {
        /// Canonical line and character range at original application time.
        range: TextRange,
        /// Absolute Ropey character index at original application time.
        start_char: usize,
        /// Text removed by the operation.
        old_text: String,
        /// Text inserted by the operation.
        new_text: String,
    },
}

/// Concrete inverse replacement prepared for selective transaction reversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevertEdit {
    /// Inclusive absolute character index in the current buffer.
    pub start_char: usize,
    /// Exclusive absolute character index in the current buffer.
    pub end_char: usize,
    /// Text that restores the selected transaction's pre-image.
    pub replacement: String,
}

/// One concrete replacement applied while traversing undo history, expressed in
/// the buffer's character coordinates immediately before that replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppliedTextEdit {
    /// Inclusive absolute character index before replay.
    pub start_char: usize,
    /// Exclusive absolute character index before replay.
    pub end_char: usize,
    /// Unicode scalar length of the inserted text.
    pub new_char_len: usize,
}

/// Cursor and viewport state restored around an undo-tree transaction.
///
/// `x` is an editor grapheme index, unlike [`TextPosition::character`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct CursorSnapshot {
    /// Grapheme index within the cursor line.
    pub x: usize,
    /// Zero-based buffer line.
    pub y: usize,
    /// First buffer line visible in the originating window.
    pub vtop: usize,
}

impl CursorSnapshot {
    /// Creates a cursor snapshot in editor coordinates.
    pub fn new(x: usize, y: usize, vtop: usize) -> Self {
        Self { x, y, vtop }
    }
}

/// One logical, attributed undo step and its cursor boundary.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EditTransaction {
    /// Stable UUID used by history and selective-revert commands.
    pub id: String,
    /// Best-effort Unix timestamp in milliseconds.
    pub timestamp_ms: u128,
    /// Subsystem that initiated the change.
    pub origin: EditOrigin,
    /// Human-readable action label.
    pub label: String,
    /// Ordered replacements applied by the transaction.
    pub edits: Vec<TextEdit>,
    /// Cursor state before the first replacement.
    pub before_cursor: CursorSnapshot,
    /// Cursor state after the final replacement.
    pub after_cursor: CursorSnapshot,
    before_revision: u64,
    after_revision: u64,
}

impl EditTransaction {
    /// Starts an empty transaction at the current history revision.
    ///
    /// The transaction is not part of history until at least one edit is recorded and
    /// [`UndoHistory::commit_transaction`] succeeds.
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

    /// Returns whether the transaction contains no effective replacements.
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

/// Read-only projection of one node used by undo-tree UI and diagnostics.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct UndoTreeEntry {
    /// Internal node index within the projected tree.
    pub index: usize,
    /// Parent node, or the virtual root when absent.
    pub parent: Option<usize>,
    /// Child node indexes in creation order.
    pub children: Vec<usize>,
    /// Whether this node is the history's current state.
    pub current: bool,
    /// Stable transaction UUID.
    pub transaction_id: String,
    /// Human-readable transaction label.
    pub label: String,
    /// Attributed transaction origin.
    pub origin: EditOrigin,
    /// Best-effort Unix timestamp in milliseconds.
    pub timestamp_ms: u128,
    /// Recorded replacements in application order.
    pub edits: Vec<TextEdit>,
}

pub const DEFAULT_MAX_UNDO_NODES: usize = 10_000;

fn default_max_undo_nodes() -> usize {
    DEFAULT_MAX_UNDO_NODES
}

/// Buffer-local branching transaction history and saved-revision marker.
///
/// Mutation is single-owner through the editor. While a transaction is active,
/// replacements are collected but the current revision does not advance; commit assigns
/// one new revision to the entire logical change.
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
    #[serde(default = "default_max_undo_nodes")]
    max_nodes: usize,
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
            max_nodes: DEFAULT_MAX_UNDO_NODES,
        }
    }
}

impl UndoHistory {
    /// Returns the total number of undo transactions retained in history.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Returns the configured maximum undo transaction nodes.
    pub fn max_nodes(&self) -> usize {
        self.max_nodes
    }

    /// Sets the maximum undo transaction node capacity.
    pub fn set_max_nodes(&mut self, max: usize) {
        self.max_nodes = max.max(100);
    }

    /// Begins a user transaction unless another transaction is already active.
    pub fn begin_transaction(&mut self, label: impl Into<String>, before_cursor: CursorSnapshot) {
        self.begin_transaction_with_origin(label, before_cursor, EditOrigin::User);
    }

    /// Begins an attributed transaction unless another transaction is already active.
    ///
    /// Nested calls intentionally keep the first transaction's label, cursor, and origin.
    /// A caller that assumes this replaces active metadata could misattribute later edits.
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
    /// Returns the transaction at the current history node.
    pub fn latest_transaction(&self) -> Option<&EditTransaction> {
        self.current
            .and_then(|index| self.nodes.get(index))
            .map(|node| &node.transaction)
    }

    #[must_use]
    /// Returns a serializable projection of every retained undo branch.
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

    /// Records one effective replacement in the active transaction.
    ///
    /// Equal old and new text is ignored. Calling this without an active transaction
    /// records nothing, so production mutation must enter through the editor assertion
    /// that guarantees a transaction exists.
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

    /// Commits the active non-empty transaction as a new child of the current node.
    ///
    /// Returns `true` only when history advanced. Empty transactions are discarded and
    /// sibling branches remain available.
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

    /// Drops the active transaction when it contains no replacements.
    pub fn cancel_transaction_if_empty(&mut self) {
        if self
            .active_transaction
            .as_ref()
            .is_some_and(EditTransaction::is_empty)
        {
            self.active_transaction = None;
        }
    }

    /// Returns whether replacements are currently being collected.
    pub fn is_transaction_active(&self) -> bool {
        self.active_transaction.is_some()
    }

    /// Marks the current history revision as the on-disk saved state.
    pub fn mark_saved(&mut self) {
        self.saved_revision = self.current_revision;
    }

    /// Returns whether the selected history revision differs from the saved revision.
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

    /// Selects the next sibling branch available to a subsequent redo.
    ///
    /// Returns the selected one-based position and sibling count.
    pub fn select_next_branch(&mut self) -> Option<(usize, usize)> {
        self.select_branch(/*delta*/ 1)
    }

    /// Selects the previous sibling branch available to a subsequent redo.
    ///
    /// Returns the selected one-based position and sibling count.
    pub fn select_previous_branch(&mut self) -> Option<(usize, usize)> {
        self.select_branch(/*delta*/ -1)
    }

    /// Prepares inverse edits when a committed transaction still matches the current text.
    ///
    /// The method does not mutate the buffer. If later edits overlap the transaction's
    /// post-image, it returns the current conflicting text so review UI can avoid
    /// overwriting newer work.
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

    /// Replays the current transaction backward and selects its parent.
    ///
    /// Returns the cursor to restore and concrete replacements for anchor maintenance.
    /// External notifications and rendering remain the editor's responsibility.
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

    /// Replays the selected child transaction and makes it current.
    ///
    /// Returns the cursor to restore and concrete replacements for anchor maintenance.
    /// External notifications and rendering remain the editor's responsibility.
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
