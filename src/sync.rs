//! Debounced accumulation of file identifiers awaiting synchronization.
//!
//! [`SyncState`] coalesces duplicate changes until its quiet period elapses, then
//! transfers the complete set to the caller. The caller owns delivery and retry;
//! retrieving a ready set clears it from this state.

use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

/// Pending file changes and the quiet-period clock used to coalesce them.
#[derive(Debug)]
pub struct SyncState {
    /// Deduplicated changed file identifiers, or `None` when no work is pending.
    pub changes: Option<HashSet<String>>,

    last_notification: Instant,
    debounce_delay: Duration,
}

impl SyncState {
    /// Creates an empty state with the default debounce delay.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a file identifier and restarts the quiet period.
    pub fn notify_change(&mut self, file: impl ToString) {
        let changes = self.changes.get_or_insert_with(HashSet::new);
        changes.insert(file.to_string());
        self.last_notification = Instant::now();
    }

    /// Returns whether the configured quiet period has elapsed.
    pub fn should_notify(&self) -> bool {
        let now = Instant::now();
        now.duration_since(self.last_notification) > self.debounce_delay
    }

    /// Takes the pending set once the quiet period has elapsed.
    ///
    /// Calling this before the deadline preserves the set and returns `None`.
    pub fn get_changes(&mut self) -> Option<HashSet<String>> {
        if self.should_notify() {
            self.last_notification = Instant::now();
            self.changes.take()
        } else {
            None
        }
    }
}

impl Default for SyncState {
    fn default() -> Self {
        Self {
            changes: None,
            last_notification: Instant::now(),
            debounce_delay: Duration::from_millis(500),
        }
    }
}
