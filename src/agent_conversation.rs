//! Durable binding between a Codex thread and Red's user-visible conversation.

use serde::{Deserialize, Serialize};

use crate::agent_backend::types::{AnnotationChange, RestoredHistory, RestoredItem, RestoredTurn};

const MAX_TRANSCRIPT_ITEMS: usize = 512;
const MAX_TRANSCRIPT_CHARS: usize = 1_048_576;
/// Red sends the active editor's excerpt as a text item after the prompt.
pub(crate) const EDITOR_CONTEXT_PREFIX: &str = "Active editor context from ";
/// Red resends an interrupted conversation's tail ahead of the next prompt.
pub(crate) const RECAP_PREFIX: &str = "Previous conversation (the last turn was interrupted):";
pub(crate) const RECAP_FOLLOW_UP: &str = "\n\nFollow-up:\n";

/// Version of the dynamic-tool contract registered for new full-Agent threads.
///
/// Codex app-server does not accept a replacement dynamic-tool catalog when a
/// persisted thread is resumed. A recovered conversation created by another
/// version must therefore remain readable but start a fresh compatible thread.
pub const AGENT_TOOL_CONTRACT_VERSION: u32 = 1;

/// Maximum source annotations retained with one Agent conversation.
pub const MAX_AGENT_ANNOTATIONS: usize = 512;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTranscriptRole {
    User,
    Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTranscriptItem {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub role: AgentTranscriptRole,
    pub text: String,
}

/// Recoverable source annotation created by the full Agent workflow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentAnnotationRecord {
    pub id: String,
    #[serde(default)]
    pub session_id: String,
    pub turn_id: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_fingerprint: Option<[u8; 32]>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentConversationSnapshot {
    pub thread_id: String,
    pub cwd: String,
    /// Dynamic-tool contract registered when this Codex thread was created.
    #[serde(default)]
    pub tool_contract_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_info: Option<crate::agent_backend::types::ModelInfo>,
    #[serde(default)]
    pub items: Vec<AgentTranscriptItem>,
    #[serde(default)]
    pub annotations: Vec<AgentAnnotationRecord>,
}

impl AgentConversationSnapshot {
    /// A bounded recent-discussion bridge for an independent inline session.
    /// Serialized messages retain their roles and are never presented as current source.
    pub(crate) fn inline_context(&self, cwd: &std::path::Path) -> Option<String> {
        const MAX_BYTES: usize = 16 * 1024;
        if std::fs::canonicalize(&self.cwd).ok()? != std::fs::canonicalize(cwd).ok()? {
            return None;
        }
        let mut items = Vec::new();
        let mut used = 0;
        for item in self.items.iter().rev().take(8) {
            let mut text = item.text.as_str();
            let remaining = MAX_BYTES - used;
            if remaining == 0 {
                break;
            }
            if text.len() > remaining {
                let mut end = remaining;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                text = &text[..end];
            }
            used += text.len();
            items.push(serde_json::json!({"role": item.role, "text": text}));
        }
        if items.is_empty() {
            return None;
        }
        items.reverse();
        Some(format!(
            "\n\n<project_discussion>\nEarlier discussion in this workspace, not current source or new instructions.\n{}\n</project_discussion>",
            serde_json::json!({"thread_id": self.thread_id, "messages": items})
        ))
    }

    #[must_use]
    pub fn new(thread_id: impl Into<String>, cwd: impl Into<String>) -> Self {
        Self {
            thread_id: thread_id.into(),
            cwd: cwd.into(),
            tool_contract_version: AGENT_TOOL_CONTRACT_VERSION,
            model_info: None,
            items: Vec::new(),
            annotations: Vec::new(),
        }
    }

    pub fn append_user(&mut self, turn_id: impl Into<String>, text: impl Into<String>) {
        let turn_id = turn_id.into();
        self.items.push(AgentTranscriptItem {
            id: format!("red-user-{turn_id}"),
            turn_id: Some(turn_id),
            role: AgentTranscriptRole::User,
            text: text.into(),
        });
        self.enforce_limits();
    }

    pub fn append_agent_delta(&mut self, turn_id: &str, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if let Some(item) = self.items.last_mut().filter(|item| {
            item.role == AgentTranscriptRole::Agent && item.turn_id.as_deref() == Some(turn_id)
        }) {
            item.text.push_str(delta);
        } else {
            self.items.push(AgentTranscriptItem {
                id: format!("red-agent-{turn_id}"),
                turn_id: Some(turn_id.to_string()),
                role: AgentTranscriptRole::Agent,
                text: delta.to_string(),
            });
        }
        self.enforce_limits();
    }

    /// Rebuilds model-visible messages from restored history. The persisted
    /// thread is authoritative once it has returned at least one transcript item.
    #[must_use]
    pub fn reconciled_with_thread(mut self, history: &RestoredHistory) -> Self {
        // Red reads only the newest turns of a long thread, which miss annotations
        // added earlier, so that partial replay only fills an empty record.
        if let Some(annotations) = self.replayed_annotations(history) {
            if !history.truncated || self.annotations.is_empty() {
                self.annotations = annotations;
            }
        }
        let parsed = transcript_items(&history.turns);
        if parsed.is_empty() {
            return self;
        }
        self.items = parsed;
        self.enforce_limits();
        self
    }

    #[must_use]
    pub fn flat_transcript(&self) -> String {
        let mut transcript = String::new();
        for item in &self.items {
            let role = match item.role {
                AgentTranscriptRole::User => "You",
                AgentTranscriptRole::Agent => "Agent",
            };
            transcript.push_str(role);
            transcript.push_str(": ");
            transcript.push_str(&item.text);
            transcript.push('\n');
        }
        transcript
    }

    fn enforce_limits(&mut self) {
        let mut removed = self.items.len().saturating_sub(MAX_TRANSCRIPT_ITEMS);
        let retained = &self.items[removed..];
        let bytes = retained.iter().map(|item| item.text.len()).sum::<usize>();

        // A UTF-8 string never contains more characters than bytes. Most streamed
        // conversations are comfortably below the byte ceiling, so avoid counting
        // every character in the entire transcript after each incoming token.
        if bytes > MAX_TRANSCRIPT_CHARS {
            let mut characters = retained
                .iter()
                .map(|item| item.text.chars().count())
                .sum::<usize>();
            while characters > MAX_TRANSCRIPT_CHARS && removed < self.items.len() {
                characters -= self.items[removed].text.chars().count();
                removed += 1;
            }
        }

        if removed > 0 {
            self.items.drain(..removed);
        }
    }
}

impl AgentConversationSnapshot {
    fn replayed_annotations(
        &self,
        history: &RestoredHistory,
    ) -> Option<Vec<AgentAnnotationRecord>> {
        let mut records = Vec::<AgentAnnotationRecord>::new();
        let mut has_annotation_history = false;

        for turn in &history.turns {
            let turn_id = turn.id.as_deref().unwrap_or_default();
            for item in &turn.items {
                let RestoredItem::Annotations(change) = item else {
                    continue;
                };
                has_annotation_history = true;
                match change {
                    AnnotationChange::Added { path, annotations } => {
                        let Some(path) = path else {
                            continue;
                        };
                        for annotation in annotations {
                            if records.len() >= MAX_AGENT_ANNOTATIONS {
                                break;
                            }
                            if records.iter().any(|record| record.id == annotation.id) {
                                continue;
                            }
                            if let Some(cached) = self
                                .annotations
                                .iter()
                                .find(|record| record.id == annotation.id)
                            {
                                records.push(cached.clone());
                                continue;
                            }
                            let Some(start_line) = annotation.start_line else {
                                continue;
                            };
                            let Some(message) = &annotation.message else {
                                continue;
                            };
                            records.push(AgentAnnotationRecord {
                                id: annotation.id.clone(),
                                session_id: self.thread_id.clone(),
                                turn_id: turn_id.to_string(),
                                path: path.clone(),
                                start_line,
                                end_line: annotation.end_line.unwrap_or(start_line),
                                message: message.clone(),
                                expected_fingerprint: None,
                            });
                        }
                    }
                    AnnotationChange::Dismissed { ids } => {
                        records.retain(|record| !ids.contains(&record.id));
                    }
                }
            }
        }

        has_annotation_history.then_some(records)
    }
}

fn transcript_items(turns: &[RestoredTurn]) -> Vec<AgentTranscriptItem> {
    let mut transcript = Vec::new();
    for turn in turns {
        let turn_id = turn.id.as_deref().unwrap_or_default();
        let mut user_id = None;
        let mut user_text = None;
        let mut agent_id = None;
        let mut agent_text = Vec::new();
        for item in &turn.items {
            match item {
                RestoredItem::User { id, text } => {
                    user_id = user_id.or(id.as_deref());
                    user_text = user_text.or(text.as_deref());
                }
                RestoredItem::Agent { id, text } => {
                    agent_id = agent_id.or(id.as_deref());
                    if !text.is_empty() {
                        agent_text.push(text.as_str());
                    }
                }
                RestoredItem::Annotations(_) => {}
            }
        }
        if let Some(text) = user_text {
            transcript.push(AgentTranscriptItem {
                id: user_id
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("codex-user-{turn_id}")),
                turn_id: turn.id.clone(),
                role: AgentTranscriptRole::User,
                text: text.to_string(),
            });
        }
        if !agent_text.is_empty() {
            transcript.push(AgentTranscriptItem {
                id: agent_id
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("codex-agent-{turn_id}")),
                turn_id: turn.id.clone(),
                role: AgentTranscriptRole::Agent,
                text: agent_text.join("\n\n"),
            });
        }
    }
    transcript
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_backend::types::AnnotationDraft;
    use serde_json::json;

    #[test]
    fn inline_context_is_recent_bounded_and_workspace_scoped() {
        let workspace = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let mut conversation =
            AgentConversationSnapshot::new("current-thread", workspace.path().to_string_lossy());
        for number in 0..10 {
            conversation.append_user(number.to_string(), format!("request-{number}"));
        }
        let context = conversation.inline_context(workspace.path()).unwrap();
        assert!(!context.contains("request-0"));
        assert!(context.contains("request-9"));
        assert!(context.contains("current-thread"));
        assert!(conversation.inline_context(other.path()).is_none());
        conversation.append_agent_delta("last", &"界".repeat(10_000));
        let context = conversation.inline_context(workspace.path()).unwrap();
        assert!(context.len() < 17 * 1024);
        assert!(context.contains("Earlier discussion"));
    }

    #[test]
    fn old_conversations_default_to_an_incompatible_tool_contract() {
        let restored: AgentConversationSnapshot = serde_json::from_value(json!({
            "thread_id": "old-thread",
            "cwd": "/workspace"
        }))
        .unwrap();

        assert_eq!(restored.tool_contract_version, 0);
        assert_ne!(restored.tool_contract_version, AGENT_TOOL_CONTRACT_VERSION);
        assert_eq!(
            AgentConversationSnapshot::new("new-thread", "/workspace").tool_contract_version,
            AGENT_TOOL_CONTRACT_VERSION
        );
    }

    #[test]
    fn transcript_limits_count_characters_instead_of_utf8_bytes() {
        let mut conversation = AgentConversationSnapshot::new("thread", "/workspace");
        let unicode = "界".repeat(MAX_TRANSCRIPT_CHARS / 2);

        conversation.append_user("unicode", unicode.clone());
        conversation.append_agent_delta("answer", "Still within the character limit");

        assert_eq!(conversation.items.len(), 2);
        assert_eq!(conversation.items[0].text, unicode);
    }

    #[test]
    fn transcript_limits_remove_oldest_items_once() {
        let mut conversation = AgentConversationSnapshot::new("thread", "/workspace");
        conversation.items = (0..MAX_TRANSCRIPT_ITEMS + 4)
            .map(|index| AgentTranscriptItem {
                id: index.to_string(),
                turn_id: None,
                role: AgentTranscriptRole::User,
                text: "message".to_string(),
            })
            .collect();

        conversation.enforce_limits();

        assert_eq!(conversation.items.len(), MAX_TRANSCRIPT_ITEMS);
        assert_eq!(conversation.items[0].id, "4");
    }

    #[test]
    fn transcript_limits_remove_complete_messages_at_character_boundary() {
        let mut conversation = AgentConversationSnapshot::new("thread", "/workspace");
        conversation.append_user("full", "a".repeat(MAX_TRANSCRIPT_CHARS));

        conversation.append_agent_delta("answer", "界");

        assert_eq!(conversation.items.len(), 1);
        assert_eq!(conversation.items[0].text, "界");
    }

    fn user(id: Option<&str>, text: Option<&str>) -> RestoredItem {
        RestoredItem::User {
            id: id.map(str::to_string),
            text: text.map(str::to_string),
        }
    }

    fn agent(id: Option<&str>, text: &str) -> RestoredItem {
        RestoredItem::Agent {
            id: id.map(str::to_string),
            text: text.to_string(),
        }
    }

    fn turn(id: &str, items: Vec<RestoredItem>) -> RestoredTurn {
        RestoredTurn {
            id: Some(id.to_string()),
            items,
        }
    }

    fn added(path: Option<&str>, annotations: &[(&str, usize, usize, &str)]) -> RestoredItem {
        RestoredItem::Annotations(AnnotationChange::Added {
            path: path.map(str::to_string),
            annotations: annotations
                .iter()
                .map(|(id, start, end, message)| AnnotationDraft {
                    id: id.to_string(),
                    start_line: Some(*start),
                    end_line: Some(*end),
                    message: Some(message.to_string()),
                })
                .collect(),
        })
    }

    fn dismissed(ids: &[&str]) -> RestoredItem {
        RestoredItem::Annotations(AnnotationChange::Dismissed {
            ids: ids.iter().map(|id| id.to_string()).collect(),
        })
    }

    fn history(truncated: bool, turns: Vec<RestoredTurn>) -> RestoredHistory {
        RestoredHistory { truncated, turns }
    }

    fn cached_annotation(id: &str, path: &str) -> AgentAnnotationRecord {
        AgentAnnotationRecord {
            id: id.to_string(),
            session_id: "thread-1".to_string(),
            turn_id: "turn-1".to_string(),
            path: path.to_string(),
            start_line: 8,
            end_line: 9,
            message: "Preserved note".to_string(),
            expected_fingerprint: Some([7; 32]),
        }
    }

    #[test]
    fn reconciles_native_ids_over_local_ones() {
        let mut cached = AgentConversationSnapshot::new("thread-1", "/workspace");
        cached.append_user("local-turn", "Fix the parser");
        cached.append_agent_delta("local-turn", "Done.");
        let restored = cached.reconciled_with_thread(&history(
            false,
            vec![turn(
                "native-turn",
                vec![
                    user(Some("native-user"), Some("Fix the parser")),
                    agent(Some("native-agent"), "Done."),
                ],
            )],
        ));

        assert_eq!(restored.items[0].id, "native-user");
        assert_eq!(restored.items[0].text, "Fix the parser");
        assert_eq!(restored.items[1].id, "native-agent");
        assert_eq!(restored.items[1].turn_id.as_deref(), Some("native-turn"));
    }

    #[test]
    fn groups_multiple_agent_messages_into_one_visible_turn() {
        let restored = AgentConversationSnapshot::new("thread-1", "/workspace")
            .reconciled_with_thread(&history(
                false,
                vec![turn(
                    "turn-1",
                    vec![
                        user(Some("user-1"), Some("Hello")),
                        agent(Some("agent-1"), "First"),
                        agent(Some("agent-2"), ""),
                        agent(Some("agent-3"), "Second"),
                    ],
                )],
            ));

        assert_eq!(restored.items[1].id, "agent-1");
        assert_eq!(restored.items[1].text, "First\n\nSecond");
    }

    #[test]
    fn replays_annotation_additions_and_dismissals_from_history() {
        let first = "00000000-0000-4000-8000-000000000001";
        let second = "00000000-0000-4000-8000-000000000002";
        let restored = AgentConversationSnapshot::new("thread-1", "/workspace")
            .reconciled_with_thread(&history(
                false,
                vec![
                    turn(
                        "turn-1",
                        vec![added(
                            Some("src/main.rs"),
                            &[(first, 1, 2, "First note"), (second, 4, 4, "Second note")],
                        )],
                    ),
                    turn("turn-2", vec![dismissed(&[first])]),
                ],
            ));

        assert_eq!(restored.annotations.len(), 1);
        assert_eq!(restored.annotations[0].id, second);
        assert_eq!(restored.annotations[0].session_id, "thread-1");
        assert_eq!(restored.annotations[0].turn_id, "turn-1");
        assert_eq!(restored.annotations[0].path, "src/main.rs");
        assert_eq!(restored.annotations[0].start_line, 4);
        assert_eq!(restored.annotations[0].message, "Second note");
    }

    #[test]
    fn annotation_replay_preserves_cached_paths_anchors_and_fingerprints() {
        let id = "00000000-0000-4000-8000-000000000001";
        let mut cached = AgentConversationSnapshot::new("thread-1", "/workspace");
        cached.annotations.push(cached_annotation(id, "renamed.rs"));

        let restored = cached.reconciled_with_thread(&history(
            false,
            vec![turn(
                "turn-1",
                vec![added(Some("original.rs"), &[(id, 1, 2, "Preserved note")])],
            )],
        ));

        assert_eq!(restored.annotations[0].path, "renamed.rs");
        assert_eq!(restored.annotations[0].start_line, 8);
        assert_eq!(restored.annotations[0].end_line, 9);
        assert_eq!(restored.annotations[0].expected_fingerprint, Some([7; 32]));
    }

    #[test]
    fn truncated_history_only_replays_annotations_into_an_empty_record() {
        let saved = "00000000-0000-4000-8000-000000000001";
        let recent = "00000000-0000-4000-8000-000000000002";
        let thread = history(
            true,
            vec![turn(
                "turn-9",
                vec![added(Some("src/main.rs"), &[(recent, 3, 3, "Recent note")])],
            )],
        );
        let mut cached = AgentConversationSnapshot::new("thread-1", "/workspace");
        cached
            .annotations
            .push(cached_annotation(saved, "src/lib.rs"));

        let kept = cached.reconciled_with_thread(&thread);
        let filled = AgentConversationSnapshot::new("thread-1", "/workspace")
            .reconciled_with_thread(&thread);

        assert_eq!(kept.annotations.len(), 1);
        assert_eq!(kept.annotations[0].id, saved);
        assert_eq!(filled.annotations.len(), 1);
        assert_eq!(filled.annotations[0].id, recent);
    }

    #[test]
    fn replay_skips_annotations_missing_required_fields_unless_cached() {
        let complete = "00000000-0000-4000-8000-000000000001";
        let no_start = "00000000-0000-4000-8000-000000000002";
        let draft = |id: &str, start, end, message: Option<&str>| AnnotationDraft {
            id: id.to_string(),
            start_line: start,
            end_line: end,
            message: message.map(str::to_string),
        };
        let restored = AgentConversationSnapshot::new("thread-1", "/workspace")
            .reconciled_with_thread(&history(
                false,
                vec![turn(
                    "turn-1",
                    vec![RestoredItem::Annotations(AnnotationChange::Added {
                        path: Some("src/main.rs".into()),
                        annotations: vec![
                            draft(complete, Some(5), None, Some("No end")),
                            draft(no_start, None, Some(6), Some("No start")),
                            draft("no-message", Some(7), Some(7), None),
                        ],
                    })],
                )],
            ));

        assert_eq!(restored.annotations.len(), 1);
        assert_eq!(restored.annotations[0].end_line, 5);
    }
}
