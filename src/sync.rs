use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct SyncState {
    pub changes: Option<HashSet<String>>,

    last_notification: Instant,
    debounce_delay: Duration,
}

impl SyncState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn notify_change(&mut self, file: impl ToString) {
        let changes = self.changes.get_or_insert_with(HashSet::new);
        changes.insert(file.to_string());
        self.last_notification = Instant::now();
    }

    pub fn should_notify(&self) -> bool {
        let now = Instant::now();
        now.duration_since(self.last_notification) > self.debounce_delay
    }

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
