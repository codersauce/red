//! Session-local notification history and lifecycle, owned by the editor.
//!
//! Presentation is deliberately separate: expiry removes a message from the active
//! set, not from history, and acknowledging progress never cancels the work. Callers
//! supply one clock reading per mutation so expiry and ordering are deterministic.

use std::{
    collections::VecDeque,
    time::{Duration, Instant, SystemTime},
};

const DEFAULT_CAPACITY: usize = 1_024;
const DEFAULT_NOTICE_DURATION: Duration = Duration::from_secs(4);
const MAX_SUMMARY_BYTES: usize = 4 * 1_024;
const MAX_DETAILS_BYTES: usize = 64 * 1_024;
const MAX_KEY_BYTES: usize = 1_024;

/// Identity of one history entry. IDs are never reused by a center.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NotificationId(u64);

/// Producer-local identity. Numeric LSP tokens never alias text tokens such as "1".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NotificationKey {
    Text(String),
    Number(u64),
}

impl From<String> for NotificationKey {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for NotificationKey {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<u64> for NotificationKey {
    fn from(value: u64) -> Self {
        Self::Number(value)
    }
}

impl NotificationKey {
    fn validate(&self) -> Result<(), NotificationError> {
        match self {
            Self::Text(value) if value.is_empty() || value.len() > MAX_KEY_BYTES => {
                Err(NotificationError::InvalidKey)
            }
            _ => Ok(()),
        }
    }
}

/// Producer identity. Generations distinguish restarted plugin/server instances.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NotificationSource {
    Editor,
    Plugin {
        name: String,
        generation: u64,
    },
    LanguageServer {
        name: String,
        workspace_root: String,
        generation: u64,
    },
}

/// Semantic importance, independent of whether an operation is still running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Success,
    Warning,
    Error,
}

/// A monotonic reading for lifetimes paired with a wall-clock history timestamp.
#[derive(Debug, Clone, Copy)]
pub struct NotificationTime {
    pub monotonic: Instant,
    pub wall: SystemTime,
}

impl NotificationTime {
    pub fn now() -> Self {
        Self {
            monotonic: Instant::now(),
            wall: SystemTime::now(),
        }
    }
}

/// Bounded text retained in history. Details retain their original line breaks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageContent {
    pub summary: String,
    pub details: Option<String>,
    pub truncated: bool,
}

impl MessageContent {
    pub fn new(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            details: None,
            truncated: false,
        }
    }

    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }

    fn bounded(mut self) -> Self {
        self.summary = self.summary.replace(['\r', '\n'], " ");
        self.truncated |= truncate_text(&mut self.summary, MAX_SUMMARY_BYTES);
        if let Some(details) = &mut self.details {
            self.truncated |= truncate_text(details, MAX_DETAILS_BYTES);
        }
        self
    }
}

fn truncate_text(text: &mut String, max_bytes: usize) -> bool {
    if text.len() <= max_bytes {
        if text.capacity() > max_bytes {
            text.shrink_to_fit();
        }
        return false;
    }
    let mut end = max_bytes - '…'.len_utf8();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push('…');
    text.shrink_to_fit();
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLifetime {
    Transient(Duration),
    UntilAcknowledged,
}

impl NoticeLifetime {
    fn for_severity(severity: Severity) -> Self {
        match severity {
            Severity::Info | Severity::Success => Self::Transient(DEFAULT_NOTICE_DURATION),
            Severity::Warning | Severity::Error => Self::UntilAcknowledged,
        }
    }

    fn display_state(self, now: Instant) -> DisplayState {
        match self {
            Self::Transient(duration) => now
                .checked_add(duration)
                .map_or(DisplayState::UntilAcknowledged, DisplayState::Until),
            Self::UntilAcknowledged => DisplayState::UntilAcknowledged,
        }
    }
}

/// A one-shot notice, optionally coalesced by a producer-scoped key.
#[derive(Debug, Clone)]
pub struct Notice {
    pub source: NotificationSource,
    pub severity: Severity,
    pub content: MessageContent,
    pub key: Option<NotificationKey>,
    pub lifetime: NoticeLifetime,
}

impl Notice {
    pub fn new(source: NotificationSource, severity: Severity, summary: impl Into<String>) -> Self {
        Self {
            source,
            severity,
            content: MessageContent::new(summary),
            key: None,
            lifetime: NoticeLifetime::for_severity(severity),
        }
    }

    pub fn with_key(mut self, key: impl Into<NotificationKey>) -> Self {
        self.key = Some(key.into());
        self
    }

    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.content = self.content.with_details(details);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressPriority {
    Background,
    UserInitiated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressOutcome {
    Succeeded,
    Failed,
    Cancelled,
}

impl ProgressOutcome {
    fn severity(self) -> Severity {
        match self {
            Self::Succeeded => Severity::Success,
            Self::Failed => Severity::Error,
            Self::Cancelled => Severity::Info,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationState {
    Notice,
    Running { percentage: Option<u8> },
    Finished(ProgressOutcome),
}

#[derive(Debug, Clone, Copy)]
enum DisplayState {
    Until(Instant),
    UntilAcknowledged,
    Hidden,
}

/// Read-only history entry; lifecycle changes go through [`NotificationCenter`].
#[derive(Debug, Clone)]
pub struct Notification {
    pub id: NotificationId,
    pub source: NotificationSource,
    pub key: Option<NotificationKey>,
    pub severity: Severity,
    pub content: MessageContent,
    pub state: NotificationState,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    pub occurrences: u64,
    pub read: bool,
    priority: ProgressPriority,
    display: DisplayState,
}

impl Notification {
    pub fn is_running(&self) -> bool {
        matches!(self.state, NotificationState::Running { .. })
    }

    pub fn is_active(&self, now: Instant) -> bool {
        self.is_running()
            || match self.display {
                DisplayState::Until(deadline) => now < deadline,
                DisplayState::UntilAcknowledged => true,
                DisplayState::Hidden => false,
            }
    }

    fn rank(&self) -> u8 {
        match self.severity {
            Severity::Error => 4,
            Severity::Warning => 3,
            _ if self.is_running() => match self.priority {
                ProgressPriority::UserInitiated => 2,
                ProgressPriority::Background => 1,
            },
            Severity::Info | Severity::Success => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NotificationCounts {
    pub total: usize,
    pub active: usize,
    pub running: usize,
    pub unread: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NotificationError {
    #[error("notification text key must contain 1 to 1024 bytes")]
    InvalidKey,
    #[error("notification history is full of active messages")]
    CapacityReached,
    #[error("notification key belongs to another active message")]
    KeyInUse,
    #[error("notification no longer exists")]
    UnknownId,
    #[error("notification is no longer running")]
    NotRunning,
    #[error("running notifications must be completed with an outcome")]
    StillRunning,
}

/// Bounded history with explicit overflow: active entries are never evicted.
///
/// When full, the oldest inactive entry is discarded. If every retained entry is
/// active, publication returns [`NotificationError::CapacityReached`] so the caller
/// can report the failure instead of silently losing an operation.
#[derive(Debug)]
pub struct NotificationCenter {
    records: VecDeque<Notification>,
    capacity: usize,
    next_id: u64,
}

impl Default for NotificationCenter {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }
}

impl NotificationCenter {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            records: VecDeque::new(),
            capacity,
            next_id: 1,
        }
    }

    /// Entries in creation order; callers may reverse this iterator for history UI.
    pub fn records(&self) -> impl DoubleEndedIterator<Item = &Notification> + ExactSizeIterator {
        self.records.iter()
    }

    pub fn get(&self, id: NotificationId) -> Option<&Notification> {
        self.records.iter().find(|record| record.id == id)
    }

    fn get_mut(&mut self, id: NotificationId) -> Result<&mut Notification, NotificationError> {
        self.records
            .iter_mut()
            .find(|record| record.id == id)
            .ok_or(NotificationError::UnknownId)
    }

    fn active_key(
        &self,
        source: &NotificationSource,
        key: &NotificationKey,
        now: Instant,
    ) -> Option<NotificationId> {
        self.records
            .iter()
            .find(|record| {
                &record.source == source
                    && record.key.as_ref() == Some(key)
                    && !matches!(record.state, NotificationState::Finished(_))
                    && record.is_active(now)
            })
            .map(|record| record.id)
    }

    fn allocate(&mut self, now: Instant) -> Result<NotificationId, NotificationError> {
        if self.records.len() >= self.capacity {
            let Some(index) = self
                .records
                .iter()
                .position(|record| !record.is_active(now))
            else {
                return Err(NotificationError::CapacityReached);
            };
            self.records.remove(index);
        }
        let id = NotificationId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("notification ID exhausted");
        Ok(id)
    }

    pub fn publish(
        &mut self,
        notice: Notice,
        now: NotificationTime,
    ) -> Result<NotificationId, NotificationError> {
        if let Some(key) = &notice.key {
            key.validate()?;
        }
        let content = notice.content.bounded();
        let display = notice.lifetime.display_state(now.monotonic);
        if let Some(id) = notice
            .key
            .as_ref()
            .and_then(|key| self.active_key(&notice.source, key, now.monotonic))
        {
            let record = self.get_mut(id)?;
            if record.state != NotificationState::Notice {
                return Err(NotificationError::KeyInUse);
            }
            record.severity = notice.severity;
            record.content = content;
            record.display = display;
            record.updated_at = now.wall;
            record.occurrences = record.occurrences.saturating_add(1);
            record.read = false;
            return Ok(id);
        }
        let id = self.allocate(now.monotonic)?;
        self.records.push_back(Notification {
            id,
            source: notice.source,
            key: notice.key,
            severity: notice.severity,
            content,
            state: NotificationState::Notice,
            created_at: now.wall,
            updated_at: now.wall,
            occurrences: 1,
            read: false,
            priority: ProgressPriority::Background,
            display,
        });
        Ok(id)
    }

    pub fn begin_progress(
        &mut self,
        source: NotificationSource,
        key: impl Into<NotificationKey>,
        content: MessageContent,
        priority: ProgressPriority,
        now: NotificationTime,
    ) -> Result<NotificationId, NotificationError> {
        let key = key.into();
        key.validate()?;
        if self.active_key(&source, &key, now.monotonic).is_some() {
            return Err(NotificationError::KeyInUse);
        }
        let id = self.allocate(now.monotonic)?;
        self.records.push_back(Notification {
            id,
            source,
            key: Some(key),
            severity: Severity::Info,
            content: content.bounded(),
            state: NotificationState::Running { percentage: None },
            created_at: now.wall,
            updated_at: now.wall,
            occurrences: 1,
            read: false,
            priority,
            display: DisplayState::UntilAcknowledged,
        });
        Ok(id)
    }

    /// Replaces progress content in place. `None` means indeterminate progress.
    /// Percentages above 100 are clamped at this boundary.
    pub fn update_progress(
        &mut self,
        id: NotificationId,
        content: MessageContent,
        percentage: Option<u8>,
        now: NotificationTime,
    ) -> Result<bool, NotificationError> {
        let record = self.get_mut(id)?;
        if !record.is_running() {
            return Err(NotificationError::NotRunning);
        }
        let content = content.bounded();
        let state = NotificationState::Running {
            percentage: percentage.map(|value| value.min(100)),
        };
        if record.content == content && record.state == state {
            return Ok(false);
        }
        record.content = content;
        record.state = state;
        record.updated_at = now.wall;
        Ok(true)
    }

    pub fn finish_progress(
        &mut self,
        id: NotificationId,
        outcome: ProgressOutcome,
        content: MessageContent,
        now: NotificationTime,
    ) -> Result<(), NotificationError> {
        let record = self.get_mut(id)?;
        if !record.is_running() {
            return Err(NotificationError::NotRunning);
        }
        record.state = NotificationState::Finished(outcome);
        record.severity = outcome.severity();
        record.content = content.bounded();
        record.updated_at = now.wall;
        record.display = NoticeLifetime::for_severity(record.severity).display_state(now.monotonic);
        record.read = false;
        Ok(())
    }

    pub fn mark_read(&mut self, id: NotificationId) -> Result<(), NotificationError> {
        self.get_mut(id)?.read = true;
        Ok(())
    }

    /// Hides a notice/completion. Running work is only marked read, never cancelled.
    pub fn acknowledge(&mut self, id: NotificationId) -> Result<(), NotificationError> {
        let record = self.get_mut(id)?;
        record.read = true;
        if !record.is_running() {
            record.display = DisplayState::Hidden;
        }
        Ok(())
    }

    /// Resolves a condition without claiming the user has read it.
    /// Running operations must be finished with their actual outcome instead.
    pub fn resolve(&mut self, id: NotificationId) -> Result<(), NotificationError> {
        let record = self.get_mut(id)?;
        if record.is_running() {
            return Err(NotificationError::StillRunning);
        }
        record.display = DisplayState::Hidden;
        Ok(())
    }

    pub fn counts(&self, now: Instant) -> NotificationCounts {
        let mut counts = NotificationCounts::default();
        for record in &self.records {
            counts.total += 1;
            counts.active += usize::from(record.is_active(now));
            counts.running += usize::from(record.is_running());
            counts.unread += usize::from(!record.read);
        }
        counts
    }

    /// Highest-priority active entry. Updates do not change creation-order ties.
    pub fn primary(&self, now: Instant) -> Option<&Notification> {
        self.records
            .iter()
            .filter(|record| record.is_active(now))
            .max_by_key(|record| (record.rank(), record.id))
    }

    /// Explicit history clearing never removes active messages or running work.
    pub fn clear_inactive(&mut self, now: Instant) -> usize {
        let before = self.records.len();
        self.records.retain(|record| record.is_active(now));
        before - self.records.len()
    }
}

#[cfg(test)]
mod tests;
