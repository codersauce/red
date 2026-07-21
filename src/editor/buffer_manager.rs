//! Buffer state and active tab management for the Red editor.

use crate::buffer::Buffer;
use std::ops::{Deref, DerefMut};

/// Encapsulates the open buffer list and active buffer selection.
#[derive(Debug)]
pub struct BufferManager {
    buffers: Vec<Buffer>,
    current_index: usize,
}

impl Default for BufferManager {
    fn default() -> Self {
        Self::new()
    }
}

impl BufferManager {
    /// Creates a new, empty BufferManager.
    pub fn new() -> Self {
        Self {
            buffers: Vec::new(),
            current_index: 0,
        }
    }

    /// Creates a BufferManager with an initial set of buffers.
    pub fn with_buffers(buffers: Vec<Buffer>) -> Self {
        Self {
            buffers,
            current_index: 0,
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

    /// Sets the active buffer index, clamping to valid bounds.
    pub fn set_active_index(&mut self, index: usize) -> usize {
        if self.buffers.is_empty() {
            self.current_index = 0;
        } else {
            self.current_index = index.min(self.buffers.len() - 1);
        }
        self.current_index
    }

    /// Returns the total number of open buffers.
    pub fn len(&self) -> usize {
        self.buffers.len()
    }

    /// Adds a buffer and makes it active.
    pub fn add_buffer(&mut self, buffer: Buffer) -> usize {
        self.buffers.push(buffer);
        self.current_index = self.buffers.len() - 1;
        self.current_index
    }

    /// Appends a buffer without changing the active selection.
    pub fn push_buffer(&mut self, buffer: Buffer) {
        self.buffers.push(buffer);
    }

    /// Removes and returns the last buffer while keeping selection in bounds.
    pub fn pop_buffer(&mut self) -> Option<Buffer> {
        let removed = self.buffers.pop();
        self.clamp_active_index();
        removed
    }

    /// Removes a buffer by index while keeping selection in bounds.
    pub fn remove_buffer(&mut self, index: usize) -> Buffer {
        let removed = self.buffers.remove(index);
        self.clamp_active_index();
        removed
    }

    /// Replaces every open buffer and resets selection to the first buffer.
    pub fn replace_buffers(&mut self, buffers: Vec<Buffer>) {
        self.buffers = buffers;
        self.current_index = 0;
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
