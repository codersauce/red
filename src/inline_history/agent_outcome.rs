//! Bounded receipts for actual editor writes, independent of the Git worktree.

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

pub const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InlineAgentState {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl InlineAgentState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "Agent working",
            Self::Completed => "Agent completed",
            Self::Failed => "Agent failed",
            Self::Cancelled => "Agent stopped",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InlineAgentEdit {
    pub before: String,
    pub after: String,
    pub transaction_ids: Vec<String>,
    pub saved: bool,
    changed_lines: Vec<usize>,
}

impl InlineAgentEdit {
    pub fn new(before: String, after: String, transaction_id: String, saved: bool) -> Self {
        let changed_lines = Self::diff_lines(&before, &after);
        Self {
            before,
            after,
            transaction_ids: vec![transaction_id],
            saved,
            changed_lines,
        }
    }

    pub fn changed_lines(&self) -> &[usize] {
        &self.changed_lines
    }

    fn diff_lines(before: &str, after: &str) -> Vec<usize> {
        similar::TextDiff::from_lines(before, after)
            .ops()
            .iter()
            .filter(|op| op.tag() != similar::DiffTag::Equal)
            .map(|op| op.new_range().start)
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InlineAgentFile {
    pub path: String,
    pub created: bool,
    #[serde(default)]
    pub hidden: bool,
    /// Separate steps prevent an interleaved user edit from entering our diff.
    pub edits: Vec<InlineAgentEdit>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InlineAgentOutcome {
    pub session_id: String,
    pub turn_id: String,
    pub state: InlineAgentState,
    pub files: Vec<InlineAgentFile>,
    pub answer: String,
    pub error: Option<String>,
}

impl InlineAgentOutcome {
    pub fn new(session_id: String, turn_id: String) -> Self {
        Self {
            session_id,
            turn_id,
            state: InlineAgentState::Running,
            files: Vec::new(),
            answer: String::new(),
            error: None,
        }
    }

    pub fn record(&mut self, path: String, created: bool, edit: InlineAgentEdit) {
        if edit.before == edit.after {
            return;
        }
        let index = self
            .files
            .iter()
            .position(|file| file.path == path)
            .unwrap_or_else(|| {
                self.files.push(InlineAgentFile {
                    path,
                    created,
                    hidden: false,
                    edits: Vec::new(),
                });
                self.files.len() - 1
            });
        self.files[index].hidden = false;
        let edits = &mut self.files[index].edits;
        if let Some(previous) = edits
            .last_mut()
            .filter(|previous| previous.after == edit.before)
        {
            previous.after = edit.after;
            previous.changed_lines = InlineAgentEdit::diff_lines(&previous.before, &previous.after);
            previous.saved = edit.saved;
            previous.transaction_ids.extend(edit.transaction_ids);
        } else {
            edits.push(edit);
        }
    }

    pub fn change_count(&self) -> usize {
        self.files
            .iter()
            .flat_map(|file| &file.edits)
            .map(|edit| edit.changed_lines().len())
            .sum()
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.session_id.is_empty() && !self.turn_id.is_empty(),
            "invalid Agent outcome identity"
        );
        ensure!(
            self.answer.len() <= super::MAX_ANSWER_BYTES
                && self.error.as_ref().is_none_or(|error| error.len() <= 4096),
            "Agent outcome answer exceeds its limit"
        );
        let mut paths = std::collections::HashSet::new();
        for file in &self.files {
            ensure!(
                !file.path.is_empty() && paths.insert(&file.path) && !file.edits.is_empty(),
                "invalid Agent outcome file"
            );
            for edit in &file.edits {
                ensure!(
                    edit.before.len() <= MAX_IMAGE_BYTES
                        && edit.after.len() <= MAX_IMAGE_BYTES
                        && !edit.transaction_ids.is_empty()
                        && edit.changed_lines
                            == InlineAgentEdit::diff_lines(&edit.before, &edit.after),
                    "Agent outcome edit exceeds its limits"
                );
            }
        }
        Ok(())
    }
}
