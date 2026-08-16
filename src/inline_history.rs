//! Editor-owned, recoverable inline conversations. Provider threads are disposable;
//! these records are the source of truth for history and historical code views.

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

use crate::{buffer::BufferId, inline_assist::InlineAssistResult, undo::TextRange};

pub const MAX_HISTORY_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_ANSWER_BYTES: usize = 64 * 1024;
const TURN_RESERVE_BYTES: usize = 6 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum HistoryAction {
    Next,
    Previous,
    Expand,
    Collapse,
    ToggleWorkspace,
    Search,
    Query(String),
    Backspace,
    DeletePreviousWord,
    EndSearch,
    ClearSearch,
    ScrollDown,
    ScrollUp,
    CycleView,
    Jump,
    Close,
    Continue,
    Recheck,
    Resolve,
    Forget,
    ConfirmForget,
    Export(String),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InlineTurnState {
    Pending,
    Completed,
    Failed,
    Cancelled,
    Rejected,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InlineDisposition {
    #[default]
    Kept,
    Undone,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineSourceState {
    Unchanged,
    Changed,
    Detached,
}

impl InlineSourceState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unchanged => "source unchanged",
            Self::Changed => "source changed",
            Self::Detached => "detached",
        }
    }
}

/// Process-local identity is rebound from the file on recovery, never deserialized.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InlineLocation {
    pub file: String,
    pub range: TextRange,
    pub start_char: usize,
    pub end_char: usize,
    #[serde(default)]
    pub detached: bool,
    #[serde(default)]
    pub context_before: String,
    #[serde(default)]
    pub context_after: String,
    #[serde(skip)]
    pub buffer_id: Option<BufferId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InlineHistoryTurn {
    pub request_id: String,
    pub created_at_ms: u64,
    pub prompt: String,
    #[serde(default)]
    pub answer: String,
    #[serde(default)]
    pub answer_truncated: bool,
    pub before: String,
    pub original_range: TextRange,
    pub location: InlineLocation,
    pub state: InlineTurnState,
    #[serde(default)]
    pub disposition: InlineDisposition,
    pub result: Option<InlineAssistResult>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub transaction_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub hidden_comments: Vec<usize>,
    #[serde(default)]
    pub comment_fingerprints: Vec<Option<[u8; 32]>>,
    #[serde(default)]
    pub comment_locations: Vec<InlineLocation>,
    #[serde(default)]
    pub comment_source_ids: Vec<Option<String>>,
}

impl InlineHistoryTurn {
    pub fn reviewed(&self) -> &str {
        self.result
            .as_ref()
            .and_then(|result| result.replacement.as_deref())
            .unwrap_or(&self.before)
    }

    pub fn answer_text(&self) -> String {
        if !self.answer.trim().is_empty() {
            return if self.answer_truncated {
                format!("{}\n[answer exceeded the retained-text limit]", self.answer)
            } else {
                self.answer.clone()
            };
        }
        if let Some(result) = &self.result {
            let comments = result
                .comments
                .iter()
                .map(|comment| comment.message.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            if !comments.is_empty() {
                return comments;
            }
            return if result
                .replacement
                .as_deref()
                .is_some_and(|text| text != self.before)
            {
                "Applied the requested code change.".into()
            } else {
                "No changes or comments needed.".into()
            };
        }
        self.error
            .clone()
            .unwrap_or_else(|| "Waiting for an answer…".into())
    }

    pub fn status(&self) -> &'static str {
        match (self.state, self.disposition) {
            (InlineTurnState::Pending, _) => "pending",
            (InlineTurnState::Failed, _) => "failed",
            (InlineTurnState::Cancelled, _) => "cancelled",
            (InlineTurnState::Rejected, _) => "not applied",
            (_, InlineDisposition::Undone) => "undone",
            (_, InlineDisposition::Superseded) => "superseded",
            (_, InlineDisposition::Kept) => "kept",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InlineConversation {
    pub id: String,
    pub cwd: String,
    pub file: String,
    pub turns: Vec<InlineHistoryTurn>,
    #[serde(default)]
    pub resolved: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct InlineHistory {
    #[serde(default)]
    pub conversations: Vec<InlineConversation>,
    /// Content-addressed full-line snapshots shared by overlapping annotations.
    #[serde(default)]
    pub sources: std::collections::BTreeMap<String, String>,
}

impl InlineHistory {
    pub fn retain_source(&mut self, source: String) -> Option<String> {
        use sha2::{Digest as _, Sha256};
        if source.len() > 256 * 1024 {
            return None;
        }
        let id = format!("{:x}", Sha256::digest(source.as_bytes()));
        self.sources.entry(id.clone()).or_insert(source);
        Some(id)
    }

    pub fn remove_unused_sources(&mut self) {
        let used = self
            .conversations
            .iter()
            .flat_map(|conversation| &conversation.turns)
            .flat_map(|turn| turn.comment_source_ids.iter().flatten())
            .collect::<std::collections::HashSet<_>>();
        self.sources.retain(|id, _| used.contains(id));
    }
    /// Refuse a new turn before dispatch rather than silently evicting old history.
    pub fn check_capacity(&self, prompt: &str, before: &str) -> Result<()> {
        ensure!(
            prompt.len() <= MAX_ANSWER_BYTES,
            "inline question is too large to retain"
        );
        ensure!(
            before.len() <= crate::inline_assist::MAX_REPLACEMENT_BYTES,
            "inline target is too large to retain"
        );
        let used = serde_json::to_vec(self)?.len();
        ensure!(
            used.saturating_add(TURN_RESERVE_BYTES) <= MAX_HISTORY_BYTES,
            "inline history is full; export or forget old conversations before continuing"
        );
        Ok(())
    }

    pub fn turn(&self, request: &str) -> Option<&InlineHistoryTurn> {
        self.conversations
            .iter()
            .flat_map(|conversation| &conversation.turns)
            .find(|turn| turn.request_id == request)
    }

    pub fn turn_mut(&mut self, request: &str) -> Option<&mut InlineHistoryTurn> {
        self.conversations
            .iter_mut()
            .flat_map(|conversation| &mut conversation.turns)
            .find(|turn| turn.request_id == request)
    }

    pub fn append_answer(&mut self, request: &str, delta: &str) {
        if let Some(turn) = self.turn_mut(request) {
            let delta = delta
                .chars()
                .filter(|ch| {
                    (!ch.is_control() || matches!(ch, '\n' | '\t'))
                        && !matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                })
                .collect::<String>();
            let remaining = MAX_ANSWER_BYTES.saturating_sub(turn.answer.len());
            let mut end = delta.len().min(remaining);
            while !delta.is_char_boundary(end) {
                end -= 1;
            }
            turn.answer.push_str(&delta[..end]);
            turn.answer_truncated |= end < delta.len();
        }
    }

    pub fn finish(&mut self, request: &str, state: InlineTurnState, error: Option<String>) {
        if let Some(turn) = self.turn_mut(request) {
            if turn.state == InlineTurnState::Pending {
                turn.state = state;
                turn.error = error;
            }
        }
    }

    pub fn recover(&mut self) {
        for turn in self
            .conversations
            .iter_mut()
            .flat_map(|conversation| &mut conversation.turns)
        {
            turn.location.buffer_id = None;
            for location in &mut turn.comment_locations {
                location.buffer_id = None;
            }
            if turn.state == InlineTurnState::Pending {
                turn.state = InlineTurnState::Cancelled;
                turn.error = Some("The editor stopped before this request completed.".into());
            }
        }
    }

    pub fn validate(&self) -> Result<()> {
        use sha2::{Digest as _, Sha256};
        ensure!(
            serde_json::to_vec(self)?.len() <= MAX_HISTORY_BYTES,
            "inline history exceeds its storage limit"
        );
        let mut requests = std::collections::HashSet::new();
        for (id, source) in &self.sources {
            ensure!(
                source.len() <= 256 * 1024
                    && *id == format!("{:x}", Sha256::digest(source.as_bytes())),
                "invalid inline history source snapshot"
            );
        }
        for conversation in &self.conversations {
            for turn in &conversation.turns {
                ensure!(
                    requests.insert(&turn.request_id),
                    "duplicate inline history request"
                );
                ensure!(
                    turn.before.len() <= crate::inline_assist::MAX_REPLACEMENT_BYTES
                        && turn.answer.len() <= MAX_ANSWER_BYTES
                        && turn.prompt.len() <= MAX_ANSWER_BYTES,
                    "inline history turn exceeds its limits"
                );
                ensure!(
                    turn.location.start_char <= turn.location.end_char,
                    "invalid inline history location"
                );
                ensure!(
                    turn.comment_source_ids
                        .iter()
                        .flatten()
                        .all(|id| self.sources.contains_key(id)),
                    "missing inline history source snapshot"
                );
                if let Some(result) = &turn.result {
                    result.validate()?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_reviewed_sources_are_deduplicated_and_validated() {
        let mut history = InlineHistory::default();
        assert_eq!(
            history.retain_source("same\n".into()),
            history.retain_source("same\n".into())
        );
        assert_eq!(history.sources.len(), 1);
        history.validate().unwrap();
        history.sources.values_mut().next().unwrap().push('x');
        assert!(history.validate().is_err());
    }

    #[test]
    fn capacity_rejects_new_work_without_evicting_old_history() {
        let mut history = InlineHistory::default();
        history
            .sources
            .insert("large".into(), "x".repeat(MAX_HISTORY_BYTES));
        assert!(history.check_capacity("question", "code").is_err());
        assert_eq!(history.sources["large"].len(), MAX_HISTORY_BYTES);
    }
}
