//! Buffer state and active tab management for the Red editor.

use crate::buffer::{Buffer, BufferId};

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

    /// Returns `true` if there are no open buffers.
    pub fn is_empty(&self) -> bool {
        self.buffers.is_empty()
    }

    /// Switches to the next buffer tab.
    pub fn next_buffer(&mut self) -> usize {
        if self.buffers.is_empty() {
            return 0;
        }
        self.current_index = (self.current_index + 1) % self.buffers.len();
        self.current_index
    }

    /// Switches to the previous buffer tab.
    pub fn previous_buffer(&mut self) -> usize {
        if self.buffers.is_empty() {
            return 0;
        }
        if self.current_index == 0 {
            self.current_index = self.buffers.len() - 1;
        } else {
            self.current_index -= 1;
        }
        self.current_index
    }

    /// Adds a buffer and makes it active.
    pub fn add_buffer(&mut self, buffer: Buffer) -> usize {
        self.buffers.push(buffer);
        self.current_index = self.buffers.len() - 1;
        self.current_index
    }

    /// Returns a reference to the underlying buffers slice.
    pub fn buffers(&self) -> &[Buffer] {
        &self.buffers
    }

    /// Returns a mutable reference to the underlying buffers slice.
    pub fn buffers_mut(&mut self) -> &mut Vec<Buffer> {
        &mut self.buffers
    }

    /// Finds the index of a buffer by its ID.
    pub fn find_index_by_id(&self, id: BufferId) -> Option<usize> {
        self.buffers.iter().position(|b| b.id() == id)
    }
}
