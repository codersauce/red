//! Agent manager sub-controller for Codex AI app-server integration and tool channels.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Instant,
};

use crate::{
    agent_conversation::{AgentAnnotationRecord, AgentConversationSnapshot, AgentThreadMode},
    agent_tools::{PendingEditorTool, PendingEditorToolResponse},
    codex::CodexBridge,
};
use serde::Serialize;

/// Encapsulates background AI agent task state, active turn metrics, and tool channels.
#[derive(Default)]
pub struct AgentManager {
    bridge: Option<CodexBridge>,
    task: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
    root: Option<PathBuf>,
    tool_requests: Option<tokio::sync::mpsc::Receiver<PendingEditorTool>>,
    playback_tool: Option<PendingEditorTool>,
    playback_ready_at: Option<Instant>,
    playback_response: Option<PendingEditorToolResponse>,
    playback_response_ready_at: Option<Instant>,
    active_sessions: HashSet<String>,
    active_turn_ids: HashMap<String, String>,
    turn_started_at: HashMap<String, Instant>,
    pending_commit_messages: HashSet<i64>,
    pending_model_requests: HashSet<i64>,
    model_only_bridge: bool,
    next_model: Option<crate::codex::AgentModelSelection>,
    conversations: HashMap<String, AgentConversationSnapshot>,
    conversation_order: Vec<String>,
    selected_conversation: Option<String>,
    live_sessions: HashSet<String>,
    attention_sessions: HashSet<String>,
    review_ready_sessions: HashSet<String>,
    cancelled_sessions: HashSet<String>,
    failed_sessions: HashMap<String, String>,
    thread_activity: HashMap<String, AgentThreadActivity>,
    pending_delegate: Option<PendingDelegate>,
    forgotten_conversations: HashSet<String>,
}

#[derive(Debug, Clone)]
pub struct PendingDelegate {
    pub cwd: PathBuf,
    pub title: String,
    pub branch: String,
    pub base_cwd: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentThreadActivity {
    pub title: String,
    pub full_title: String,
    pub status: String,
    pub detail: String,
}

impl AgentManager {
    /// Creates a new, empty AgentManager instance.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if a Codex bridge connection is currently attached.
    pub fn has_bridge(&self) -> bool {
        self.bridge.is_some()
    }

    pub fn bridge(&self) -> Option<&CodexBridge> {
        self.bridge.as_ref()
    }

    pub fn bridge_mut(&mut self) -> Option<&mut CodexBridge> {
        self.bridge.as_mut()
    }

    pub fn set_bridge(&mut self, bridge: CodexBridge) {
        self.model_only_bridge = false;
        self.bridge = Some(bridge);
    }

    pub fn take_bridge(&mut self) -> Option<CodexBridge> {
        self.bridge.take()
    }

    pub fn is_task_finished(&self) -> bool {
        self.task
            .as_ref()
            .is_some_and(tokio::task::JoinHandle::is_finished)
    }

    pub fn set_task(&mut self, task: tokio::task::JoinHandle<anyhow::Result<()>>) {
        self.task = Some(task);
    }

    pub fn take_task(&mut self) -> Option<tokio::task::JoinHandle<anyhow::Result<()>>> {
        self.task.take()
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    pub fn set_root(&mut self, root: Option<PathBuf>) {
        self.root = root;
    }

    pub fn set_tool_requests(&mut self, requests: tokio::sync::mpsc::Receiver<PendingEditorTool>) {
        self.tool_requests = Some(requests);
    }

    pub fn clear_tool_requests(&mut self) {
        self.tool_requests = None;
        self.playback_tool = None;
        self.playback_ready_at = None;
        self.playback_response = None;
        self.playback_response_ready_at = None;
    }

    pub fn try_recv_tool_request(&mut self) -> Option<PendingEditorTool> {
        self.tool_requests
            .as_mut()
            .and_then(|requests| requests.try_recv().ok())
    }

    pub fn has_playback_work(&self) -> bool {
        self.playback_tool.is_some() || self.playback_response.is_some()
    }

    pub fn stage_playback_tool(&mut self, tool: PendingEditorTool, ready_at: Instant) {
        self.playback_tool = Some(tool);
        self.playback_ready_at = Some(ready_at);
    }

    pub fn take_ready_playback_tool(&mut self, now: Instant) -> Option<PendingEditorTool> {
        if self
            .playback_ready_at
            .is_some_and(|deadline| now >= deadline)
        {
            self.playback_ready_at = None;
            self.playback_tool.take()
        } else {
            None
        }
    }

    pub fn stage_playback_response(
        &mut self,
        response: PendingEditorToolResponse,
        ready_at: Instant,
    ) {
        self.playback_response = Some(response);
        self.playback_response_ready_at = Some(ready_at);
    }

    pub fn take_ready_playback_response(
        &mut self,
        now: Instant,
    ) -> Option<PendingEditorToolResponse> {
        if self
            .playback_response_ready_at
            .is_some_and(|deadline| now >= deadline)
        {
            self.playback_response_ready_at = None;
            self.playback_response.take()
        } else {
            None
        }
    }

    /// Marks a session as active.
    pub fn mark_session_active(&mut self, session_id: impl Into<String>) {
        let session_id = session_id.into();
        self.attention_sessions.remove(&session_id);
        self.review_ready_sessions.remove(&session_id);
        self.cancelled_sessions.remove(&session_id);
        self.failed_sessions.remove(&session_id);
        self.thread_activity.remove(&session_id);
        self.active_sessions.insert(session_id);
    }

    /// Marks a session as inactive.
    pub fn mark_session_inactive(&mut self, session_id: &str) {
        self.active_sessions.remove(session_id);
        self.active_turn_ids.remove(session_id);
    }

    pub fn is_session_active(&self, session_id: &str) -> bool {
        self.active_sessions.contains(session_id)
    }

    pub fn has_active_sessions(&self) -> bool {
        !self.active_sessions.is_empty()
    }

    pub fn clear_active_sessions(&mut self) {
        self.active_sessions.clear();
        self.active_turn_ids.clear();
    }

    pub fn set_turn_id(&mut self, session_id: impl Into<String>, turn_id: impl Into<String>) {
        self.active_turn_ids
            .insert(session_id.into(), turn_id.into());
    }

    pub fn turn_id(&self, session_id: &str) -> Option<&str> {
        self.active_turn_ids.get(session_id).map(String::as_str)
    }

    /// Records turn start timestamp for turn duration metrics.
    pub fn record_turn_start(&mut self, turn_id: impl Into<String>) {
        self.turn_started_at.insert(turn_id.into(), Instant::now());
    }

    /// Takes turn start timestamp and returns elapsed duration if recorded.
    pub fn elapsed_turn_duration(&mut self, turn_id: &str) -> Option<std::time::Duration> {
        self.turn_started_at
            .remove(turn_id)
            .map(|start| start.elapsed())
    }

    pub fn discard_turn(&mut self, turn_id: &str) {
        self.turn_started_at.remove(turn_id);
    }

    pub fn clear_turns(&mut self) {
        self.turn_started_at.clear();
    }

    pub fn mark_commit_message_pending(&mut self, request_id: i64) {
        self.pending_commit_messages.insert(request_id);
    }

    pub fn finish_commit_message(&mut self, request_id: i64) {
        self.pending_commit_messages.remove(&request_id);
    }

    pub fn take_pending_commit_messages(&mut self) -> Vec<i64> {
        self.pending_commit_messages.drain().collect()
    }

    pub fn mark_model_request_pending(&mut self, request_id: i64) {
        self.pending_model_requests.insert(request_id);
    }

    pub fn finish_model_request(&mut self, request_id: i64) {
        self.pending_model_requests.remove(&request_id);
    }

    pub fn take_pending_model_requests(&mut self) -> Vec<i64> {
        self.pending_model_requests.drain().collect()
    }

    pub fn mark_model_only_bridge(&mut self) {
        self.model_only_bridge = true;
    }

    pub fn mark_conversation_requested(&mut self) {
        self.model_only_bridge = false;
    }

    pub fn is_model_only_bridge(&self) -> bool {
        self.model_only_bridge
    }

    pub fn set_next_model(&mut self, selection: crate::codex::AgentModelSelection) {
        self.next_model = Some(selection);
    }

    pub fn next_model(&self) -> Option<&crate::codex::AgentModelSelection> {
        self.next_model.as_ref()
    }

    pub fn take_next_model(&mut self) -> Option<crate::codex::AgentModelSelection> {
        self.next_model.take()
    }

    pub fn set_conversation_model(
        &mut self,
        session_id: &str,
        model_info: crate::codex::AgentModelInfo,
    ) {
        if let Some(conversation) = self.conversations.get_mut(session_id) {
            conversation.model_info = Some(model_info);
        }
    }

    pub fn begin_conversation(&mut self, thread_id: impl Into<String>, cwd: &Path) {
        let thread_id = thread_id.into();
        self.forgotten_conversations.remove(&thread_id);
        let mut conversation =
            AgentConversationSnapshot::new(thread_id.clone(), cwd.to_string_lossy());
        let delegate = if self
            .pending_delegate
            .as_ref()
            .is_some_and(|delegate| delegate.cwd == cwd)
        {
            self.pending_delegate.take()
        } else {
            None
        };
        let select = delegate.is_none();
        if let Some(delegate) = delegate {
            conversation.mode = AgentThreadMode::Delegate;
            conversation.title = delegate.title;
            conversation.branch = Some(delegate.branch);
            conversation.base_cwd = Some(delegate.base_cwd.to_string_lossy().into_owned());
        }
        self.insert_conversation(conversation, select);
        self.live_sessions.insert(thread_id);
    }

    pub fn restore_conversation(&mut self, conversation: AgentConversationSnapshot) {
        self.root = Some(PathBuf::from(&conversation.cwd));
        self.insert_conversation(conversation, /*select*/ true);
    }

    pub fn restore_conversations(
        &mut self,
        conversations: Vec<AgentConversationSnapshot>,
        selected: Option<&str>,
    ) {
        self.conversations.clear();
        self.conversation_order.clear();
        self.selected_conversation = None;
        for conversation in conversations {
            self.insert_conversation(conversation, /*select*/ false);
        }
        if let Some(selected) = selected.filter(|id| self.conversations.contains_key(*id)) {
            self.selected_conversation = Some(selected.to_string());
        } else {
            self.selected_conversation = self.conversation_order.last().cloned();
        }
        if let Some(cwd) = self
            .conversation_snapshot()
            .map(|conversation| PathBuf::from(conversation.cwd))
        {
            self.root = Some(cwd);
        }
    }

    pub fn reconcile_conversation(
        &mut self,
        thread_id: &str,
        cwd: &Path,
        thread: &serde_json::Value,
    ) -> Option<&AgentConversationSnapshot> {
        let cached = self
            .conversations
            .remove(thread_id)
            .unwrap_or_else(|| AgentConversationSnapshot::new(thread_id, cwd.to_string_lossy()));
        self.conversations
            .insert(thread_id.to_string(), cached.reconciled_with_thread(thread));
        self.conversations.get(thread_id)
    }

    pub fn conversation_snapshot(&self) -> Option<AgentConversationSnapshot> {
        self.selected_conversation
            .as_deref()
            .and_then(|id| self.conversations.get(id))
            .cloned()
    }

    pub fn conversation_snapshots(&self) -> Vec<AgentConversationSnapshot> {
        self.conversation_order
            .iter()
            .filter_map(|id| self.conversations.get(id).cloned())
            .collect()
    }

    pub fn select_conversation(&mut self, session_id: &str) -> Option<AgentConversationSnapshot> {
        let conversation = self.conversations.get(session_id)?.clone();
        self.selected_conversation = Some(session_id.to_string());
        self.review_ready_sessions.remove(session_id);
        Some(conversation)
    }

    pub fn selected_conversation_id(&self) -> Option<&str> {
        self.selected_conversation.as_deref()
    }

    pub fn register_delegate(
        &mut self,
        cwd: PathBuf,
        title: String,
        branch: String,
        base_cwd: PathBuf,
    ) {
        self.pending_delegate = Some(PendingDelegate {
            cwd,
            title,
            branch,
            base_cwd,
        });
    }

    pub fn root_for_session(&self, session_id: &str) -> Option<&Path> {
        self.conversations
            .get(session_id)
            .map(|conversation| Path::new(&conversation.cwd))
            .or_else(|| self.root())
    }

    pub fn is_session_live(&self, session_id: &str) -> bool {
        self.live_sessions.contains(session_id)
    }

    pub fn mark_session_live(&mut self, session_id: impl Into<String>) {
        self.live_sessions.insert(session_id.into());
    }

    pub fn clear_live_sessions(&mut self) {
        self.live_sessions.clear();
    }

    pub fn is_delegate(&self, session_id: &str) -> bool {
        self.conversations
            .get(session_id)
            .is_some_and(|conversation| conversation.mode == AgentThreadMode::Delegate)
    }

    pub fn mark_session_attention(&mut self, session_id: impl Into<String>) {
        let session_id = session_id.into();
        self.attention_sessions.insert(session_id.clone());
        self.review_ready_sessions.remove(&session_id);
    }

    pub fn clear_session_attention(&mut self, session_id: &str) {
        self.attention_sessions.remove(session_id);
    }

    pub fn mark_session_finished(&mut self, session_id: &str) {
        self.attention_sessions.remove(session_id);
        self.failed_sessions.remove(session_id);
        if !self.cancelled_sessions.contains(session_id)
            && self
                .conversations
                .get(session_id)
                .is_some_and(|conversation| conversation.mode == AgentThreadMode::Delegate)
        {
            self.review_ready_sessions.insert(session_id.to_string());
        }
    }

    pub fn mark_session_failed(&mut self, session_id: &str, message: impl Into<String>) {
        self.attention_sessions.remove(session_id);
        self.review_ready_sessions.remove(session_id);
        self.cancelled_sessions.remove(session_id);
        self.failed_sessions
            .insert(session_id.to_string(), message.into());
    }

    pub fn mark_session_cancelled(&mut self, session_id: &str) {
        self.attention_sessions.remove(session_id);
        self.review_ready_sessions.remove(session_id);
        self.failed_sessions.remove(session_id);
        self.cancelled_sessions.insert(session_id.to_string());
        if let Some(activity) = self.thread_activity.get_mut(session_id) {
            activity.status = "cancelled".to_string();
            activity.detail = "Stopped by user".to_string();
        }
    }

    pub fn record_thread_activity(&mut self, session_id: &str, update: &serde_json::Value) {
        if !matches!(
            update
                .get("session_update")
                .and_then(serde_json::Value::as_str),
            Some("tool_call" | "tool_call_update")
        ) {
            return;
        }
        let existing = self.thread_activity.get(session_id);
        let field = |name: &str| {
            update
                .get(name)
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        let title = field("title").or_else(|| existing.map(|activity| activity.title.clone()));
        let Some(title) = title else { return };
        let full_title = field("full_title")
            .or_else(|| existing.map(|activity| activity.full_title.clone()))
            .unwrap_or_else(|| title.clone());
        let status = field("status")
            .or_else(|| existing.map(|activity| activity.status.clone()))
            .unwrap_or_else(|| "in_progress".to_string());
        let detail = field("detail")
            .or_else(|| existing.map(|activity| activity.detail.clone()))
            .unwrap_or_default();
        self.thread_activity.insert(
            session_id.to_string(),
            AgentThreadActivity {
                title,
                full_title,
                status,
                detail,
            },
        );
    }

    pub fn thread_activity(&self, session_id: &str) -> Option<&AgentThreadActivity> {
        self.thread_activity.get(session_id)
    }

    pub fn thread_status(&self, session_id: &str) -> (&'static str, Option<&str>) {
        if let Some(message) = self.failed_sessions.get(session_id) {
            return ("Failed", Some(message));
        }
        if self.attention_sessions.contains(session_id) {
            return ("Needs you", None);
        }
        if self.active_sessions.contains(session_id) {
            return ("Running", None);
        }
        if self.review_ready_sessions.contains(session_id) {
            return ("Ready to review", None);
        }
        if self.cancelled_sessions.contains(session_id) {
            return ("Stopped", Some("Stopped by user"));
        }
        if self.selected_conversation.as_deref() == Some(session_id) {
            return ("Current", None);
        }
        ("History", None)
    }

    pub fn replace_annotation_records(&mut self, annotations: Vec<AgentAnnotationRecord>) {
        for conversation in self.conversations.values_mut() {
            conversation.annotations.clear();
        }
        for annotation in annotations {
            let session_id = if annotation.session_id.is_empty() {
                self.selected_conversation.as_deref()
            } else {
                Some(annotation.session_id.as_str())
            };
            if let Some(conversation) = session_id.and_then(|id| self.conversations.get_mut(id)) {
                conversation.annotations.push(annotation);
            }
        }
    }

    pub fn forget_conversation(&mut self, session_id: &str) {
        self.next_model = None;
        self.forgotten_conversations.insert(session_id.to_string());
        self.conversations.remove(session_id);
        self.conversation_order.retain(|id| id != session_id);
        self.live_sessions.remove(session_id);
        self.attention_sessions.remove(session_id);
        self.review_ready_sessions.remove(session_id);
        self.cancelled_sessions.remove(session_id);
        self.failed_sessions.remove(session_id);
        self.thread_activity.remove(session_id);
        if self.selected_conversation.as_deref() == Some(session_id) {
            self.selected_conversation = self.conversation_order.last().cloned();
        }
    }

    pub fn take_forgotten_conversation(&mut self, session_id: &str) -> bool {
        self.forgotten_conversations.remove(session_id)
    }

    pub fn record_user_message(&mut self, session_id: &str, turn_id: &str, text: &str) {
        if let Some(conversation) = self.conversations.get_mut(session_id) {
            conversation.append_user(turn_id, text);
        }
    }

    pub fn record_agent_delta(&mut self, session_id: &str, text: &str) {
        let Some(turn_id) = self.active_turn_ids.get(session_id) else {
            return;
        };
        if let Some(conversation) = self.conversations.get_mut(session_id) {
            conversation.append_agent_delta(turn_id, text);
        }
    }

    pub fn complete_agent_message(&mut self, session_id: &str, text: &str) {
        let Some(turn_id) = self.active_turn_ids.get(session_id) else {
            return;
        };
        let Some(conversation) = self.conversations.get_mut(session_id) else {
            return;
        };
        if let Some(item) = conversation.items.iter_mut().rev().find(|item| {
            item.role == crate::agent_conversation::AgentTranscriptRole::Agent
                && item.turn_id.as_deref() == Some(turn_id)
        }) {
            item.text = text.to_string();
        } else {
            conversation.append_agent_delta(turn_id, text);
        }
    }

    fn insert_conversation(&mut self, conversation: AgentConversationSnapshot, select: bool) {
        let thread_id = conversation.thread_id.clone();
        self.conversation_order.retain(|id| id != &thread_id);
        self.conversation_order.push(thread_id.clone());
        self.conversations.insert(thread_id.clone(), conversation);
        if select || self.selected_conversation.is_none() {
            self.selected_conversation = Some(thread_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AgentManager;
    use crate::agent_conversation::AgentThreadMode;
    use crate::agent_tools::PendingEditorToolResponse;
    use serde_json::json;
    use std::{
        path::Path,
        time::{Duration, Instant},
    };

    #[test]
    fn owns_session_and_turn_lifecycle() {
        let mut manager = AgentManager::new();
        manager.begin_conversation("session-1", Path::new("/workspace"));
        manager.mark_session_active("session-1");
        manager.set_turn_id("session-1", "turn-1");
        manager.record_user_message("session-1", "turn-1", "Question");
        manager.record_agent_delta("session-1", "Answer");
        manager.record_turn_start("session-1");

        assert!(manager.is_session_active("session-1"));
        assert!(manager.elapsed_turn_duration("session-1").is_some());

        manager.mark_session_inactive("session-1");
        assert!(!manager.is_session_active("session-1"));
        let conversation = manager.conversation_snapshot().unwrap();
        assert_eq!(conversation.items.len(), 2);
        assert_eq!(conversation.items[1].text, "Answer");

        manager.forget_conversation("session-1");
        assert!(manager.conversation_snapshot().is_none());
        assert!(manager.take_forgotten_conversation("session-1"));
        assert!(!manager.take_forgotten_conversation("session-1"));
    }

    #[test]
    fn delegate_threads_keep_the_pair_selected_and_use_their_own_root() {
        let mut manager = AgentManager::new();
        manager.begin_conversation("pair", Path::new("/workspace"));
        manager.register_delegate(
            "/workspace.delegate-task".into(),
            "Implement task".to_string(),
            "red/delegate/task".to_string(),
            "/workspace".into(),
        );
        manager.begin_conversation("delegate", Path::new("/workspace.delegate-task"));

        assert_eq!(manager.selected_conversation_id(), Some("pair"));
        assert_eq!(
            manager.root_for_session("delegate"),
            Some(Path::new("/workspace.delegate-task"))
        );
        let delegate = manager
            .conversation_snapshots()
            .into_iter()
            .find(|conversation| conversation.thread_id == "delegate")
            .unwrap();
        assert_eq!(delegate.mode, AgentThreadMode::Delegate);
        assert_eq!(delegate.branch.as_deref(), Some("red/delegate/task"));

        manager.mark_session_active("delegate");
        assert_eq!(manager.thread_status("delegate").0, "Running");
        manager.record_thread_activity(
            "delegate",
            &json!({
                "session_update": "tool_call",
                "title": "Running cargo test",
                "full_title": "Running cargo test in .",
                "status": "in_progress",
            }),
        );
        assert_eq!(
            manager.thread_activity("delegate").unwrap().title,
            "Running cargo test"
        );
        manager.mark_session_inactive("delegate");
        manager.mark_session_finished("delegate");
        assert_eq!(manager.thread_status("delegate").0, "Ready to review");

        assert_eq!(
            manager.select_conversation("delegate").unwrap().thread_id,
            "delegate"
        );
        assert_eq!(manager.selected_conversation_id(), Some("delegate"));
    }

    #[test]
    fn stopped_delegate_does_not_become_ready_when_interruption_finishes() {
        let mut manager = AgentManager::new();
        manager.register_delegate(
            "/workspace.delegate-task".into(),
            "Run tests".to_string(),
            "red/delegate/tests".to_string(),
            "/workspace".into(),
        );
        manager.begin_conversation("delegate", Path::new("/workspace.delegate-task"));
        manager.mark_session_active("delegate");
        manager.mark_session_cancelled("delegate");
        manager.mark_session_inactive("delegate");
        manager.mark_session_finished("delegate");

        assert_eq!(
            manager.thread_status("delegate"),
            ("Stopped", Some("Stopped by user"))
        );
    }

    #[tokio::test]
    async fn holds_completed_edits_until_the_follow_deadline() {
        let mut manager = AgentManager::new();
        let (response, received) = tokio::sync::oneshot::channel();
        let now = Instant::now();
        manager.stage_playback_response(
            PendingEditorToolResponse {
                response,
                result: Ok(json!({ "saved": true })),
            },
            now + Duration::from_millis(700),
        );

        assert!(manager.has_playback_work());
        assert!(manager.take_ready_playback_response(now).is_none());
        let completed = manager
            .take_ready_playback_response(now + Duration::from_millis(700))
            .unwrap();
        completed.response.send(completed.result).unwrap();
        assert_eq!(received.await.unwrap().unwrap()["saved"], true);
        assert!(!manager.has_playback_work());
    }
}
