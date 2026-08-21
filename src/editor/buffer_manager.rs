//! Buffer state and active tab management for the Red editor.

use crate::buffer::{Buffer, BufferId};
use std::ops::{Deref, DerefMut};

/// Encapsulates the open buffer list and active buffer selection.
#[derive(Debug)]
pub struct BufferManager {
    buffers: Vec<Buffer>,
    current_index: usize,
    /// Visited buffers, oldest first. IDs survive buffer-list compaction.
    recent_buffers: Vec<BufferId>,
}

impl Default for BufferManager {
    fn default() -> Self {
        Self::new()
    }
}

impl BufferManager {
    /// Creates a new, empty BufferManager.
    pub fn new() -> Self {
        Self::with_buffers(Vec::new())
    }

    /// Creates a BufferManager with an initial set of buffers.
    pub fn with_buffers(buffers: Vec<Buffer>) -> Self {
        let recent_buffers = buffers.first().map(Buffer::id).into_iter().collect();
        Self {
            buffers,
            current_index: 0,
            recent_buffers,
        }
    }

    /// Returns a reference to the active buffer, if any.
    pub fn active_buffer(&self) -> Option<&Buffer> {
        self.buffers.get(self.current_index)
    }

    /// Returns a mutable reference to the active buffer, if any.
    pub fn active_buffer_mut(&mut self) -> Option<&mut Buffer> {
        self.buffers.get_mut(self.current_index)
    }

    /// Returns the active buffer index.
    pub fn active_index(&self) -> usize {
        self.current_index
    }

    /// Selects a buffer and records it as most recently used.
    pub fn set_active_index(&mut self, index: usize) -> usize {
        self.set_active_index_without_history(index);
        self.record_active_buffer();
        self.current_index
    }

    /// Temporarily selects a buffer for background edits, without recording a visit.
    pub fn set_active_index_without_history(&mut self, index: usize) -> usize {
        self.current_index = index;
        self.clamp_active_index();
        self.current_index
    }

    /// Finds the most recently visited open buffer other than the active buffer.
    pub fn alternate_index(&self) -> Option<usize> {
        let active_id = self.active_buffer()?.id();
        self.recent_buffers.iter().rev().find_map(|id| {
            (*id != active_id)
                .then(|| self.buffers.iter().position(|buffer| buffer.id() == *id))
                .flatten()
        })
    }

    /// Returns visited buffer identities from their oldest visit to their newest.
    pub(crate) fn recent_buffer_ids(&self) -> impl DoubleEndedIterator<Item = BufferId> + '_ {
        self.recent_buffers.iter().copied()
    }

    fn record_active_buffer(&mut self) {
        let Some(id) = self.active_buffer().map(Buffer::id) else {
            return;
        };
        if self.recent_buffers.last() != Some(&id) {
            self.recent_buffers.retain(|recent| *recent != id);
            self.recent_buffers.push(id);
        }
    }

    /// Returns the total number of open buffers.
    pub fn len(&self) -> usize {
        self.buffers.len()
    }

    /// Adds a buffer and makes it active in editor tests.
    #[cfg(test)]
    pub fn add_buffer(&mut self, buffer: Buffer) -> usize {
        self.push_buffer(buffer);
        self.set_active_index(self.buffers.len() - 1)
    }

    /// Appends a buffer without changing the active selection.
    pub fn push_buffer(&mut self, buffer: Buffer) {
        self.buffers.push(buffer);
    }

    /// Removes and returns the last buffer while keeping selection in bounds.
    pub fn pop_buffer(&mut self) -> Option<Buffer> {
        let index = self.buffers.len().checked_sub(1)?;
        Some(self.remove_buffer(index))
    }

    /// Removes a buffer by index while keeping selection in bounds.
    pub fn remove_buffer(&mut self, index: usize) -> Buffer {
        let removed = self.buffers.remove(index);
        self.recent_buffers.retain(|id| *id != removed.id());
        if index < self.current_index {
            self.current_index -= 1;
        }
        self.clamp_active_index();
        self.record_active_buffer();
        removed
    }

    /// Replaces every open buffer and resets selection to the first buffer.
    pub fn replace_buffers(&mut self, buffers: Vec<Buffer>) {
        *self = Self::with_buffers(buffers);
    }

    fn clamp_active_index(&mut self) {
        self.current_index = if self.buffers.is_empty() {
            0
        } else {
            self.current_index.min(self.buffers.len() - 1)
        };
    }
}

impl Deref for BufferManager {
    type Target = [Buffer];

    fn deref(&self) -> &Self::Target {
        &self.buffers
    }
}

impl DerefMut for BufferManager {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.buffers
    }
}

#[cfg(test)]
mod tests {
    use super::BufferManager;
    use crate::buffer::Buffer;

    fn buffer(name: &str) -> Buffer {
        Buffer::new(Some(name.to_string()), String::new())
    }

    #[test]
    fn alternate_buffer_toggles_only_the_two_latest_visits() {
        let mut manager = BufferManager::with_buffers(vec![buffer("a"), buffer("b"), buffer("c")]);
        assert_eq!(manager.alternate_index(), None);
        manager.set_active_index(1);
        manager.set_active_index(2);
        manager.set_active_index(2);

        for expected in [1, 2, 1, 2] {
            assert_eq!(manager.alternate_index(), Some(expected));
            manager.set_active_index(expected);
        }

        manager.set_active_index(0);
        assert_eq!(manager.alternate_index(), Some(2));
    }

    #[test]
    fn alternate_buffer_survives_removal_and_falls_back_to_older_visits() {
        let mut manager =
            BufferManager::with_buffers(vec![buffer("a"), buffer("b"), buffer("c"), buffer("d")]);
        for index in [2, 1, 3] {
            manager.set_active_index(index);
        }

        manager.remove_buffer(1);
        assert_eq!(manager.active_buffer().unwrap().name(), "d");
        assert_eq!(manager.alternate_index(), Some(1));
        manager.set_active_index(1);
        assert_eq!(manager.active_buffer().unwrap().name(), "c");
        assert_eq!(manager.alternate_index(), Some(2));

        manager.pop_buffer();
        assert_eq!(manager.active_buffer().unwrap().name(), "c");
        assert_eq!(manager.alternate_index(), Some(0));
        manager.remove_buffer(1);
        assert_eq!(manager.active_buffer().unwrap().name(), "a");
        assert_eq!(manager.alternate_index(), None);
    }

    #[test]
    fn recent_buffer_ids_keep_visit_order_without_duplicate_or_removed_entries() {
        let mut manager = BufferManager::with_buffers(vec![buffer("a"), buffer("b"), buffer("c")]);
        let first = manager[0].id();
        let second = manager[1].id();
        let third = manager[2].id();

        manager.set_active_index(1);
        manager.set_active_index(2);
        manager.set_active_index(1);

        assert_eq!(
            manager.recent_buffer_ids().collect::<Vec<_>>(),
            vec![first, third, second]
        );

        manager.remove_buffer(1);

        assert_eq!(
            manager.recent_buffer_ids().collect::<Vec<_>>(),
            vec![first, third]
        );
    }

    #[test]
    fn alternate_buffer_ignores_temporary_edits_and_resets_with_buffers() {
        let mut manager = BufferManager::new();
        assert_eq!(manager.alternate_index(), None);
        manager.add_buffer(buffer("a"));
        assert_eq!(manager.alternate_index(), None);
        manager.add_buffer(buffer("b"));
        manager.push_buffer(buffer("c"));
        manager.set_active_index_without_history(2);
        manager.set_active_index_without_history(1);
        assert_eq!(manager.alternate_index(), Some(0));

        manager.replace_buffers(vec![buffer("d"), buffer("e")]);
        assert_eq!(manager.alternate_index(), None);
        manager.set_active_index(1);
        assert_eq!(manager.alternate_index(), Some(0));
        manager.replace_buffers(Vec::new());
        assert_eq!(manager.alternate_index(), None);
    }

    #[test]
    fn selection_stays_in_bounds_as_buffers_change() {
        let mut manager = BufferManager::with_buffers(vec![buffer("a"), buffer("b")]);
        manager.set_active_index(1);
        manager.remove_buffer(1);
        assert_eq!(manager.active_index(), 0);
        assert_eq!(manager.active_buffer().unwrap().name(), "a");

        manager.add_buffer(buffer("c"));
        assert_eq!(manager.active_index(), 1);
        assert_eq!(manager.active_buffer().unwrap().name(), "c");

        manager.replace_buffers(vec![buffer("d")]);
        assert_eq!(manager.active_index(), 0);
        assert_eq!(manager.active_buffer().unwrap().name(), "d");
    }
}
