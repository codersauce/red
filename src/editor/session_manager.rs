//! Session recovery and snapshot management for Red editor sessions.

use crate::session::SessionStore;
use std::time::{Duration, Instant};

/// Default interval for background session snapshot flushes.
pub const DEFAULT_SESSION_SNAPSHOT_INTERVAL: Duration = Duration::from_secs(5);
pub type SessionSnapshotGeneration = (u64, Option<u64>);
pub type SessionSnapshotWriter = std::thread::JoinHandle<anyhow::Result<SessionSnapshotGeneration>>;

/// Manages session persistence, disk divergence detection, and crash recovery.
#[derive(Debug)]
pub struct SessionManager {
    store: Option<SessionStore>,
    last_snapshot_at: Instant,
    snapshot_interval: Duration,
    last_generation: Option<SessionSnapshotGeneration>,
    writer: Option<SessionSnapshotWriter>,
    warning: Option<&'static str>,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionManager {
    /// Creates a new SessionManager without an active store.
    pub fn new() -> Self {
        Self {
            store: None,
            last_snapshot_at: Instant::now(),
            snapshot_interval: DEFAULT_SESSION_SNAPSHOT_INTERVAL,
            last_generation: None,
            writer: None,
            warning: None,
        }
    }

    /// Sets or replaces the session store.
    pub fn set_store(&mut self, store: SessionStore) {
        self.store = Some(store);
        self.last_snapshot_at = Instant::now();
        self.last_generation = None;
    }

    /// Returns a reference to the active session store, if present.
    pub fn store(&self) -> Option<&SessionStore> {
        self.store.as_ref()
    }

    /// Checks if a session snapshot is due based on configured interval.
    pub fn should_snapshot(&self) -> bool {
        self.store.is_some() && self.last_snapshot_at.elapsed() >= self.snapshot_interval
    }

    /// Records that a snapshot was taken at the current timestamp.
    pub fn mark_snapshot_taken(&mut self) {
        self.last_snapshot_at = Instant::now();
    }

    /// Moves the due time into the past for deterministic tests.
    pub fn force_snapshot_due(&mut self) {
        self.last_snapshot_at = Instant::now() - self.snapshot_interval;
    }

    /// Returns whether snapshot attempts are still inside the backoff interval.
    pub fn is_backing_off(&self) -> bool {
        self.last_snapshot_at.elapsed() < self.snapshot_interval
    }

    /// Returns whether this exact editor generation was already persisted.
    pub fn generation_is_current(&self, generation: SessionSnapshotGeneration) -> bool {
        self.last_generation == Some(generation)
    }

    /// Records a successfully persisted editor generation.
    pub fn record_generation(&mut self, generation: SessionSnapshotGeneration) {
        self.last_generation = Some(generation);
    }

    /// Takes ownership of an in-flight snapshot writer, if present.
    pub fn take_writer(&mut self) -> Option<SessionSnapshotWriter> {
        self.writer.take()
    }

    /// Stores an in-flight snapshot writer.
    pub fn set_writer(&mut self, writer: SessionSnapshotWriter) {
        self.writer = Some(writer);
    }

    /// Returns active session recovery warning, if any.
    pub fn warning(&self) -> Option<&'static str> {
        self.warning
    }

    /// Sets an active session recovery warning.
    pub fn set_warning(&mut self, warning: Option<&'static str>) {
        self.warning = warning;
    }
}

#[cfg(test)]
mod tests {
    use super::SessionManager;

    #[test]
    fn owns_snapshot_generation_timing_and_warning_state() {
        let mut manager = SessionManager::new();
        assert!(!manager.should_snapshot());

        manager.record_generation((7, Some(3)));
        assert!(manager.generation_is_current((7, Some(3))));

        manager.set_warning(Some("snapshot failed"));
        assert_eq!(manager.warning(), Some("snapshot failed"));
        manager.set_warning(None);
        assert_eq!(manager.warning(), None);
    }
}
