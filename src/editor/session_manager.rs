//! Session recovery and snapshot management for Red editor sessions.

use crate::session::SessionStore;
use std::time::{Duration, Instant};

/// Default interval for background session snapshot flushes (10 seconds).
pub const DEFAULT_SESSION_SNAPSHOT_INTERVAL: Duration = Duration::from_secs(10);

/// Manages session persistence, disk divergence detection, and crash recovery.
#[derive(Debug)]
pub struct SessionManager {
    store: Option<SessionStore>,
    last_snapshot_at: Instant,
    snapshot_interval: Duration,
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
            warning: None,
        }
    }

    /// Creates a SessionManager with a given SessionStore.
    pub fn with_store(store: SessionStore) -> Self {
        Self {
            store: Some(store),
            last_snapshot_at: Instant::now(),
            snapshot_interval: DEFAULT_SESSION_SNAPSHOT_INTERVAL,
            warning: None,
        }
    }

    /// Sets or replaces the session store.
    pub fn set_store(&mut self, store: SessionStore) {
        self.store = Some(store);
    }

    /// Returns a reference to the active session store, if present.
    pub fn store(&self) -> Option<&SessionStore> {
        self.store.as_ref()
    }

    /// Returns whether a session store is configured.
    pub fn has_store(&self) -> bool {
        self.store.is_some()
    }

    /// Checks if a session snapshot is due based on configured interval.
    pub fn should_snapshot(&self) -> bool {
        self.store.is_some() && self.last_snapshot_at.elapsed() >= self.snapshot_interval
    }

    /// Records that a snapshot was taken at the current timestamp.
    pub fn mark_snapshot_taken(&mut self) {
        self.last_snapshot_at = Instant::now();
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
