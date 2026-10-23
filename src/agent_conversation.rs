//! Durable binding between a Codex thread and Red's user-visible conversation.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    pub model_info: Option<crate::codex::AgentModelInfo>,
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

    /// Rebuilds model-visible messages from Codex history. The persisted thread
    /// is authoritative once it has returned at least one transcript item.
    #[must_use]
    pub fn reconciled_with_thread(mut self, thread: &Value) -> Self {
        // Red reads only the newest turns of a long thread, which miss annotations
        // added earlier, so that partial replay only fills an empty record.
        let truncated = thread.get("historyTruncated").and_then(Value::as_bool) == Some(true);
        if let Some(annotations) = annotation_records_from_thread(&self, thread) {
            if !truncated || self.annotations.is_empty() {
                self.annotations = annotations;
            }
        }
        let parsed = transcript_items_from_thread(thread);
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

fn annotation_records_from_thread(
    conversation: &AgentConversationSnapshot,
    thread: &Value,
) -> Option<Vec<AgentAnnotationRecord>> {
    let turns = thread.get("turns")?.as_array()?;
    let mut records = Vec::<AgentAnnotationRecord>::new();
    let mut has_annotation_history = false;

    for turn in turns {
        let turn_id = turn.get("id").and_then(Value::as_str).unwrap_or_default();
        let Some(items) = turn.get("items").and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            let Some(output) = successful_annotation_output(item) else {
                continue;
            };
            match item.get("tool").and_then(Value::as_str) {
                Some("add_annotations") => {
                    has_annotation_history = true;
                    let Some(path) = output
                        .get("path")
                        .and_then(Value::as_str)
                        .or_else(|| item.pointer("/arguments/path").and_then(Value::as_str))
                    else {
                        continue;
                    };
                    let Some(annotations) = output.get("annotations").and_then(Value::as_array)
                    else {
                        continue;
                    };
                    for annotation in annotations {
                        if records.len() >= MAX_AGENT_ANNOTATIONS {
                            break;
                        }
                        let Some(id) = annotation.get("id").and_then(Value::as_str) else {
                            continue;
                        };
                        if records.iter().any(|record| record.id == id) {
                            continue;
                        }
                        if let Some(cached) = conversation
                            .annotations
                            .iter()
                            .find(|record| record.id == id)
                        {
                            records.push(cached.clone());
                            continue;
                        }
                        let Some(start_line) = annotation
                            .get("start_line")
                            .and_then(Value::as_u64)
                            .and_then(|line| usize::try_from(line).ok())
                        else {
                            continue;
                        };
                        let end_line = annotation
                            .get("end_line")
                            .and_then(Value::as_u64)
                            .and_then(|line| usize::try_from(line).ok())
                            .unwrap_or(start_line);
                        let Some(message) = annotation.get("message").and_then(Value::as_str)
                        else {
                            continue;
                        };
                        records.push(AgentAnnotationRecord {
                            id: id.to_string(),
                            session_id: conversation.thread_id.clone(),
                            turn_id: turn_id.to_string(),
                            path: path.to_string(),
                            start_line,
                            end_line,
                            message: message.to_string(),
                            expected_fingerprint: None,
                        });
                    }
                }
                Some("dismiss_annotations") => {
                    has_annotation_history = true;
                    let Some(dismissed) = output.get("dismissed").and_then(Value::as_array) else {
                        continue;
                    };
                    records.retain(|record| {
                        !dismissed
                            .iter()
                            .any(|id| id.as_str() == Some(record.id.as_str()))
                    });
                }
                _ => {}
            }
        }
    }

    has_annotation_history.then_some(records)
}

fn successful_annotation_output(item: &Value) -> Option<Value> {
    if item.get("type").and_then(Value::as_str) != Some("dynamicToolCall")
        || item.get("status").and_then(Value::as_str) != Some("completed")
        || item.get("success").and_then(Value::as_bool) != Some(true)
        || !matches!(
            item.get("tool").and_then(Value::as_str),
            Some("add_annotations" | "dismiss_annotations")
        )
    {
        return None;
    }
    item.get("contentItems")?
        .as_array()?
        .iter()
        .filter_map(|content| content.get("text").and_then(Value::as_str))
        .filter_map(|text| serde_json::from_str::<Value>(text).ok())
        .find(|output| output.get("ok").and_then(Value::as_bool) == Some(true))
}

fn transcript_items_from_thread(thread: &Value) -> Vec<AgentTranscriptItem> {
    let mut transcript = Vec::new();
    let Some(turns) = thread.get("turns").and_then(Value::as_array) else {
        return transcript;
    };
    for turn in turns {
        let turn_id = turn.get("id").and_then(Value::as_str).unwrap_or_default();
        let Some(items) = turn.get("items").and_then(Value::as_array) else {
            continue;
        };
        let mut user_text = Vec::new();
        let mut agent_text = Vec::new();
        let mut user_id = None;
        let mut agent_id = None;
        for item in items {
            match item.get("type").and_then(Value::as_str) {
                Some("userMessage") => {
                    user_id = user_id.or_else(|| item.get("id").and_then(Value::as_str));
                    if let Some(content) = item.get("content").and_then(Value::as_array) {
                        for input in content {
                            if input.get("type").and_then(Value::as_str) == Some("text") {
                                if let Some(text) = input.get("text").and_then(Value::as_str) {
                                    user_text.push(text.to_string());
                                }
                            }
                        }
                    }
                }
                Some("agentMessage") => {
                    agent_id = agent_id.or_else(|| item.get("id").and_then(Value::as_str));
                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                        if !text.is_empty() {
                            agent_text.push(text.to_string());
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(first) = user_text.first() {
            transcript.push(AgentTranscriptItem {
                id: user_id
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("codex-user-{turn_id}")),
                turn_id: (!turn_id.is_empty()).then(|| turn_id.to_string()),
                role: AgentTranscriptRole::User,
                text: clean_persisted_user_text(first),
            });
        }
        if !agent_text.is_empty() {
            transcript.push(AgentTranscriptItem {
                id: agent_id
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("codex-agent-{turn_id}")),
                turn_id: (!turn_id.is_empty()).then(|| turn_id.to_string()),
                role: AgentTranscriptRole::Agent,
                text: agent_text.join("\n\n"),
            });
        }
    }
    transcript
}

fn clean_persisted_user_text(text: &str) -> String {
    match typed_prompt(text) {
        "" => text,
        prompt => prompt,
    }
    .to_string()
}

/// What the person typed, without the editor context and recap Red sent with
/// it. Codex joins a turn's text items with no separator in a thread preview.
pub(crate) fn typed_prompt(text: &str) -> &str {
    let text = match text.strip_prefix(RECAP_PREFIX) {
        Some(recap) => recap
            .rsplit_once(RECAP_FOLLOW_UP)
            .map_or("", |(_, follow_up)| follow_up),
        None => text,
    };
    text.split_once(EDITOR_CONTEXT_PREFIX)
        .map_or(text, |(prompt, _)| prompt)
        .trim_end()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn restored_prompts_leave_out_the_recap_and_context_red_sent() {
        assert_eq!(
            clean_persisted_user_text(
                "Previous conversation (the last turn was interrupted):\nYou: earlier\nAgent: partial\n\nFollow-up:\nTry again\n\nActive editor context from file:///a.rs:\n\n```text\nx\n```"
            ),
            "Try again"
        );
        assert_eq!(
            typed_prompt("Fix it.Active editor context from file:///a.rs:"),
            "Fix it."
        );
        assert_eq!(clean_persisted_user_text("Plain prompt"), "Plain prompt");
    }

    fn annotation_tool_item(tool: &str, arguments: Value, output: Value) -> Value {
        json!({
            "id": format!("call-{tool}"),
            "type": "dynamicToolCall",
            "tool": tool,
            "arguments": arguments,
            "status": "completed",
            "success": true,
            "contentItems": [{"type": "inputText", "text": output.to_string()}],
        })
    }

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
    fn reconciles_native_ids_without_exposing_editor_context() {
        let mut cached = AgentConversationSnapshot::new("thread-1", "/workspace");
        cached.append_user("local-turn", "Fix the parser");
        cached.append_agent_delta("local-turn", "Done.");
        let thread = json!({
            "turns": [{
                "id": "native-turn",
                "items": [
                    {
                        "id": "native-user",
                        "type": "userMessage",
                        "content": [{
                            "type": "text",
                            "text": "Fix the parser\n\nActive editor context from file:///parser.rs:\n\n```text\nsource\n```"
                        }]
                    },
                    {"id": "native-agent", "type": "agentMessage", "text": "Done."}
                ]
            }]
        });

        let restored = cached.reconciled_with_thread(&thread);

        assert_eq!(restored.items[0].id, "native-user");
        assert_eq!(restored.items[0].text, "Fix the parser");
        assert_eq!(restored.items[1].id, "native-agent");
        assert_eq!(restored.items[1].turn_id.as_deref(), Some("native-turn"));
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
    fn groups_multiple_agent_messages_into_one_visible_turn() {
        let thread = json!({
            "turns": [{
                "id": "turn-1",
                "items": [
                    {"id": "user-1", "type": "userMessage", "content": [{"type": "text", "text": "Hello"}]},
                    {"id": "agent-1", "type": "agentMessage", "text": "First"},
                    {"id": "agent-2", "type": "agentMessage", "text": "Second"}
                ]
            }]
        });

        let restored = AgentConversationSnapshot::new("thread-1", "/workspace")
            .reconciled_with_thread(&thread);

        assert_eq!(restored.items[1].text, "First\n\nSecond");
    }

    #[test]
    fn replays_successful_annotation_additions_and_dismissals_from_thread_history() {
        let first = "00000000-0000-4000-8000-000000000001";
        let second = "00000000-0000-4000-8000-000000000002";
        let add = annotation_tool_item(
            "add_annotations",
            json!({"path": "src/main.rs", "expected_revision": 3}),
            json!({
                "ok": true,
                "path": "src/main.rs",
                "annotations": [
                    {"id": first, "start_line": 1, "end_line": 2, "message": "First note"},
                    {"id": second, "start_line": 4, "end_line": 4, "message": "Second note"}
                ]
            }),
        );
        let dismiss = annotation_tool_item(
            "dismiss_annotations",
            json!({"annotation_ids": [first]}),
            json!({"ok": true, "dismissed": [first]}),
        );
        let mut failed = annotation_tool_item(
            "add_annotations",
            json!({"path": "ignored.rs"}),
            json!({"ok": true, "path": "ignored.rs", "annotations": []}),
        );
        failed["success"] = json!(false);
        let thread = json!({"turns": [
            {"id": "turn-1", "items": [add]},
            {"id": "turn-2", "items": [dismiss, failed]}
        ]});

        let restored = AgentConversationSnapshot::new("thread-1", "/workspace")
            .reconciled_with_thread(&thread);

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
        cached.annotations.push(AgentAnnotationRecord {
            id: id.to_string(),
            session_id: "thread-1".to_string(),
            turn_id: "turn-1".to_string(),
            path: "renamed.rs".to_string(),
            start_line: 8,
            end_line: 9,
            message: "Preserved note".to_string(),
            expected_fingerprint: Some([7; 32]),
        });
        let add = annotation_tool_item(
            "add_annotations",
            json!({"path": "original.rs"}),
            json!({
                "ok": true,
                "path": "original.rs",
                "annotations": [{"id": id, "start_line": 1, "end_line": 2, "message": "Preserved note"}]
            }),
        );
        let thread = json!({"turns": [{"id": "turn-1", "items": [add]}]});

        let restored = cached.reconciled_with_thread(&thread);

        assert_eq!(restored.annotations[0].path, "renamed.rs");
        assert_eq!(restored.annotations[0].start_line, 8);
        assert_eq!(restored.annotations[0].end_line, 9);
        assert_eq!(restored.annotations[0].expected_fingerprint, Some([7; 32]));
    }

    #[test]
    fn truncated_history_only_replays_annotations_into_an_empty_record() {
        let saved = "00000000-0000-4000-8000-000000000001";
        let recent = "00000000-0000-4000-8000-000000000002";
        let add = annotation_tool_item(
            "add_annotations",
            json!({"path": "src/main.rs"}),
            json!({
                "ok": true,
                "path": "src/main.rs",
                "annotations": [{"id": recent, "start_line": 3, "end_line": 3, "message": "Recent note"}]
            }),
        );
        let thread = json!({
            "historyTruncated": true,
            "turns": [{"id": "turn-9", "items": [add]}]
        });
        let mut cached = AgentConversationSnapshot::new("thread-1", "/workspace");
        cached.annotations.push(AgentAnnotationRecord {
            id: saved.to_string(),
            session_id: "thread-1".to_string(),
            turn_id: "turn-1".to_string(),
            path: "src/lib.rs".to_string(),
            start_line: 1,
            end_line: 1,
            message: "Saved note".to_string(),
            expected_fingerprint: None,
        });

        let kept = cached.reconciled_with_thread(&thread);
        let filled = AgentConversationSnapshot::new("thread-1", "/workspace")
            .reconciled_with_thread(&thread);

        assert_eq!(kept.annotations.len(), 1);
        assert_eq!(kept.annotations[0].id, saved);
        assert_eq!(filled.annotations.len(), 1);
        assert_eq!(filled.annotations[0].id, recent);
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
}
