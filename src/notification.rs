//! Session-local notification history and lifecycle, owned by the editor.
//!
//! Presentation is deliberately separate: expiry removes a message from the active
//! set, not from history, and acknowledging progress never cancels the work. Callers
//! supply one clock reading per mutation so expiry and ordering are deterministic.

use std::{
    collections::VecDeque,
    fmt,
    time::{Duration, Instant, SystemTime},
};

const DEFAULT_CAPACITY: usize = 1_024;
const DEFAULT_NOTICE_DURATION: Duration = Duration::from_secs(4);
const MAX_SUMMARY_BYTES: usize = 4 * 1_024;
const MAX_DETAILS_BYTES: usize = 64 * 1_024;
const MAX_KEY_BYTES: usize = 1_024;

/// Identity of one history entry. IDs are never reused by a center.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct NotificationId(u64);

impl fmt::Display for NotificationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Commands returned by the message-history surface.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MessageAction {
    Close,
    Next,
    Previous,
    Search,
    Query(String),
    Backspace,
    DeletePreviousWord,
    EndSearch,
    ClearSearch,
    CycleFilter,
    Acknowledge,
    ClearInactive,
    Copy,
    ScrollDown,
    ScrollUp,
}

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

impl NotificationSource {
    pub fn label(&self) -> &str {
        match self {
            Self::Editor => "Red",
            Self::Plugin { name, .. } | Self::LanguageServer { name, .. } => name,
        }
    }
}

/// Semantic importance, independent of whether an operation is still running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Success,
    Warning,
    Error,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Success => "success",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }

    pub fn marker(self) -> &'static str {
        match self {
            Self::Info => "i",
            Self::Success => "✓",
            Self::Warning => "!",
            Self::Error => "×",
        }
    }
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
        self.summary = single_line(&self.summary);
        self.truncated |= truncate_text(&mut self.summary, MAX_SUMMARY_BYTES);
        if let Some(details) = &mut self.details {
            self.truncated |= truncate_text(details, MAX_DETAILS_BYTES);
        }
        self
    }
}

/// Terminal-safe summary text; original multiline details remain available for copy.
pub(crate) fn single_line(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

pub(crate) fn detail_text(text: &str) -> String {
    text.replace('\t', "    ")
        .chars()
        .map(|ch| {
            if ch != '\n' && ch.is_control() {
                '�'
            } else {
                ch
            }
        })
        .collect()
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

/// Whether retained feedback should ask for the user's attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionPolicy {
    /// Routine feedback stays in history without creating an unread obligation.
    Quiet,
    /// Informational feedback is unread until it has actually been displayed.
    IfMissed,
    /// Reading does not resolve the condition or acknowledge the notice.
    RequiresAcknowledgment,
}

impl AttentionPolicy {
    pub(crate) fn for_severity(severity: Severity) -> Self {
        match severity {
            Severity::Success => Self::Quiet,
            Severity::Info => Self::IfMissed,
            Severity::Warning | Severity::Error => Self::RequiresAcknowledgment,
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
    pub attention: AttentionPolicy,
}

impl Notice {
    pub fn new(source: NotificationSource, severity: Severity, summary: impl Into<String>) -> Self {
        Self {
            source,
            severity,
            content: MessageContent::new(summary),
            key: None,
            lifetime: NoticeLifetime::for_severity(severity),
            attention: AttentionPolicy::for_severity(severity),
        }
    }

    pub fn with_key(mut self, key: impl Into<NotificationKey>) -> Self {
        self.key = Some(key.into());
        self
    }

    pub fn with_attention(mut self, attention: AttentionPolicy) -> Self {
        self.attention = attention;
        if attention == AttentionPolicy::RequiresAcknowledgment {
            self.lifetime = NoticeLifetime::UntilAcknowledged;
        }
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
    pub attention: AttentionPolicy,
    pub(crate) content_version: u64,
    priority: ProgressPriority,
    display: DisplayState,
}

impl Notification {
    /// An unresolved notice that must be acknowledged even if it was read.
    pub fn needs_acknowledgment(&self, now: Instant) -> bool {
        self.attention == AttentionPolicy::RequiresAcknowledgment
            && !self.is_running()
            && self.is_active(now)
    }

    /// Meaningful information that has not been seen or explicitly dismissed.
    /// Expiration removes it from the message line, not from this count.
    pub fn is_unseen(&self) -> bool {
        self.attention == AttentionPolicy::IfMissed
            && !self.read
            && !self.is_running()
            && !matches!(self.display, DisplayState::Hidden)
    }

    pub fn needs_attention(&self, now: Instant) -> bool {
        self.needs_acknowledgment(now) || self.is_unseen()
    }

    pub(crate) fn visible_until(&self) -> Option<Instant> {
        match self.display {
            DisplayState::Until(deadline) => Some(deadline),
            _ => None,
        }
    }

    pub(crate) fn is_dismissed(&self) -> bool {
        matches!(self.display, DisplayState::Hidden)
    }

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
    /// Unresolved notices whose policy requires explicit acknowledgment.
    pub needs_acknowledgment: usize,
    /// Unseen informational notices/completions, excluding running work.
    pub unseen: usize,
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
    revision: u64,
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
            revision: 0,
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
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
            record.attention = notice.attention;
            record.content_version = record.content_version.wrapping_add(1);
            record.content = content;
            record.display = display;
            record.updated_at = now.wall;
            record.occurrences = record.occurrences.saturating_add(1);
            record.read = notice.attention == AttentionPolicy::Quiet;
            self.changed();
            return Ok(id);
        }
        let id = self.allocate(now.monotonic)?;
        self.records.push_back(Notification {
            id,
            source: notice.source,
            key: notice.key,
            severity: notice.severity,
            attention: notice.attention,
            content_version: 0,
            content,
            state: NotificationState::Notice,
            created_at: now.wall,
            updated_at: now.wall,
            occurrences: 1,
            read: notice.attention == AttentionPolicy::Quiet,
            priority: ProgressPriority::Background,
            display,
        });
        self.changed();
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
            attention: AttentionPolicy::IfMissed,
            content_version: 0,
            content: content.bounded(),
            state: NotificationState::Running { percentage: None },
            created_at: now.wall,
            updated_at: now.wall,
            occurrences: 1,
            read: false,
            priority,
            display: DisplayState::UntilAcknowledged,
        });
        self.changed();
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
        self.changed();
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
        record.attention = match outcome {
            ProgressOutcome::Failed => AttentionPolicy::RequiresAcknowledgment,
            ProgressOutcome::Succeeded | ProgressOutcome::Cancelled => AttentionPolicy::IfMissed,
        };
        record.content_version = record.content_version.wrapping_add(1);
        record.content = content.bounded();
        record.updated_at = now.wall;
        record.display = NoticeLifetime::for_severity(record.severity).display_state(now.monotonic);
        record.read = false;
        self.changed();
        Ok(())
    }

    pub fn mark_read(&mut self, id: NotificationId) -> Result<(), NotificationError> {
        let record = self.get_mut(id)?;
        if !record.read {
            record.read = true;
            self.changed();
        }
        Ok(())
    }

    /// Hides a notice/completion. Running work is only marked read, never cancelled.
    pub fn acknowledge(&mut self, id: NotificationId) -> Result<(), NotificationError> {
        let record = self.get_mut(id)?;
        record.read = true;
        if !record.is_running() {
            record.display = DisplayState::Hidden;
        }
        self.changed();
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
        self.changed();
        Ok(())
    }

    pub fn counts(&self, now: Instant) -> NotificationCounts {
        let mut counts = NotificationCounts::default();
        for record in &self.records {
            counts.total += 1;
            counts.active += usize::from(record.is_active(now));
            counts.running += usize::from(record.is_running());
            counts.unread += usize::from(!record.read);
            counts.needs_acknowledgment += usize::from(record.needs_acknowledgment(now));
            counts.unseen += usize::from(record.is_unseen());
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
        let removed = before - self.records.len();
        if removed > 0 {
            self.changed();
        }
        removed
    }
}

#[cfg(test)]
mod tests;
