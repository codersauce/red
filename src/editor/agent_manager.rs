//! Agent manager sub-controller for Codex AI app-server integration and tool channels.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    time::Instant,
};

use crate::{
    agent_conversation::{AgentAnnotationRecord, AgentConversationSnapshot},
    agent_tools::{PendingEditorTool, PendingEditorToolResponse},
    codex::CodexBridge,
};

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
    /// Sessions with a turn in flight; absent sessions are idle.
    turn_phases: HashMap<String, AgentTurnPhase>,
    active_turn_ids: HashMap<String, String>,
    turn_started_at: HashMap<String, Instant>,
    pending_commit_messages: HashSet<i64>,
    pending_model_requests: HashSet<i64>,
    pending_session_requests: HashSet<i64>,
    pending_session_preview_requests: HashSet<i64>,
    model_only_bridge: bool,
    next_model: Option<crate::codex::AgentModelSelection>,
    conversation: Option<AgentConversationSnapshot>,
    /// Bumped on every change to `conversation`, so the editor can tell when the
    /// saved copy is out of date without comparing transcripts.
    conversation_revision: u64,
    forgotten_conversations: HashSet<String>,
    /// Prompts submitted while their session was busy, sent once its turn ends.
    queued_prompts: HashMap<String, VecDeque<QueuedAgentPrompt>>,
    /// The Agent pane's conversation lifecycle.
    pane: super::agent_pane::AgentPaneLifecycle,
    /// Effects returned by pane transitions, run at the editor's next drain point.
    pane_effects: Vec<super::agent_pane::AgentPaneEffect>,
    /// Last snapshot sent to the pane, so unchanged state is not re-sent.
    published_pane: Option<super::agent_pane::AgentPaneSnapshot>,
}

/// Where a session's current turn is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTurnPhase {
    /// The turn is running and may call editor tools.
    Running,
    /// A cancel was sent; editor tools are refused until Codex reports the end.
    Stopping,
}

/// Return values of pane transitions that may carry effects for the editor.
pub trait PaneTransition {
    fn effects(&self) -> Vec<super::agent_pane::AgentPaneEffect>;
}

impl PaneTransition for () {
    fn effects(&self) -> Vec<super::agent_pane::AgentPaneEffect> {
        Vec::new()
    }
}

impl PaneTransition for usize {
    fn effects(&self) -> Vec<super::agent_pane::AgentPaneEffect> {
        Vec::new()
    }
}

impl PaneTransition for bool {
    fn effects(&self) -> Vec<super::agent_pane::AgentPaneEffect> {
        Vec::new()
    }
}

impl PaneTransition for Vec<super::agent_pane::AgentPaneEffect> {
    fn effects(&self) -> Vec<super::agent_pane::AgentPaneEffect> {
        self.clone()
    }
}

impl PaneTransition for Option<Vec<super::agent_pane::AgentPaneEffect>> {
    fn effects(&self) -> Vec<super::agent_pane::AgentPaneEffect> {
        self.clone().unwrap_or_default()
    }
}

impl PaneTransition
    for (
        super::agent_pane::AgentSubmitResult,
        Vec<super::agent_pane::AgentPaneEffect>,
    )
{
    fn effects(&self) -> Vec<super::agent_pane::AgentPaneEffect> {
        self.1.clone()
    }
}

/// A prompt held until its session's previous turn has fully ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedAgentPrompt {
    pub text: String,
    pub context: Option<(String, String)>,
}

impl AgentManager {
    /// Creates a new, empty AgentManager instance.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies a pane transition and keeps its effects for the next drain.
    pub fn update_pane<R>(
        &mut self,
        transition: impl FnOnce(&mut super::agent_pane::AgentPaneLifecycle) -> R,
    ) -> R
    where
        R: PaneTransition,
    {
        let result = transition(&mut self.pane);
        self.pane_effects.extend(result.effects());
        result
    }

    pub fn take_pane_effects(&mut self) -> Vec<super::agent_pane::AgentPaneEffect> {
        std::mem::take(&mut self.pane_effects)
    }

    /// Returns the pane snapshot when it differs from the last one published.
    pub fn take_changed_pane_snapshot(&mut self) -> Option<super::agent_pane::AgentPaneSnapshot> {
        let snapshot = self.pane.snapshot();
        if self.published_pane.as_ref() == Some(&snapshot) {
            return None;
        }
        self.published_pane = Some(snapshot.clone());
        Some(snapshot)
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

    /// Marks a session's turn as running.
    pub fn mark_session_active(&mut self, session_id: impl Into<String>) {
        self.turn_phases
            .insert(session_id.into(), AgentTurnPhase::Running);
    }

    /// Stops treating a running turn as active. A stopping turn stays busy until
    /// `finish_turn`, because Codex has not confirmed that it ended.
    pub fn mark_session_inactive(&mut self, session_id: &str) {
        if self.turn_phases.get(session_id) != Some(&AgentTurnPhase::Stopping) {
            self.turn_phases.remove(session_id);
            self.active_turn_ids.remove(session_id);
        }
    }

    pub fn is_session_active(&self, session_id: &str) -> bool {
        self.turn_phases.get(session_id) == Some(&AgentTurnPhase::Running)
    }

    pub fn has_active_sessions(&self) -> bool {
        self.turn_phases
            .values()
            .any(|phase| *phase == AgentTurnPhase::Running)
    }

    /// Returns the most advanced phase across sessions, for status display.
    pub fn turn_phase(&self) -> Option<AgentTurnPhase> {
        if self.has_active_sessions() {
            Some(AgentTurnPhase::Running)
        } else {
            self.turn_phases.values().next().copied()
        }
    }

    pub fn queued_prompt_count(&self) -> usize {
        self.queued_prompts.values().map(VecDeque::len).sum()
    }

    pub fn clear_active_sessions(&mut self) {
        self.turn_phases.clear();
        self.active_turn_ids.clear();
    }

    /// Records a cancel request. A running turn keeps the session busy until
    /// Codex reports the interrupted turn's terminal event, while its editor
    /// tools stop immediately because the session is no longer active.
    pub fn begin_stopping(&mut self, session_id: &str) {
        if self.is_session_active(session_id) {
            self.turn_phases
                .insert(session_id.to_string(), AgentTurnPhase::Stopping);
        }
    }

    /// Returns whether a new turn must wait for this session's current turn.
    pub fn is_session_busy(&self, session_id: &str) -> bool {
        self.turn_phases.contains_key(session_id)
    }

    pub fn queue_prompt(&mut self, session_id: impl Into<String>, prompt: QueuedAgentPrompt) {
        self.queued_prompts
            .entry(session_id.into())
            .or_default()
            .push_back(prompt);
    }

    /// Ends a session's turn and returns the next prompt that may now start.
    pub fn finish_turn(&mut self, session_id: &str) -> Option<QueuedAgentPrompt> {
        self.turn_phases.remove(session_id);
        self.active_turn_ids.remove(session_id);
        let queue = self.queued_prompts.get_mut(session_id)?;
        let prompt = queue.pop_front();
        if queue.is_empty() {
            self.queued_prompts.remove(session_id);
        }
        prompt
    }

    /// Drops busy state and queued prompts for a session that is being closed.
    pub fn discard_session_queue(&mut self, session_id: &str) -> usize {
        self.turn_phases.remove(session_id);
        self.queued_prompts
            .remove(session_id)
            .map_or(0, |queue| queue.len())
    }

    /// Takes every unsent prompt so a lost bridge can hand them back to the pane.
    pub fn take_all_queued_prompts(&mut self) -> Vec<(String, Vec<String>)> {
        let mut returned = self
            .queued_prompts
            .drain()
            .map(|(session_id, queue)| {
                (
                    session_id,
                    queue.into_iter().map(|prompt| prompt.text).collect(),
                )
            })
            .collect::<Vec<_>>();
        returned.sort_by(|left, right| left.0.cmp(&right.0));
        returned
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

    pub fn mark_session_request_pending(&mut self, request_id: i64) {
        self.pending_session_requests.insert(request_id);
    }

    pub fn finish_session_request(&mut self, request_id: i64) {
        self.pending_session_requests.remove(&request_id);
    }

    pub fn take_pending_session_requests(&mut self) -> Vec<i64> {
        self.pending_session_requests.drain().collect()
    }

    pub fn mark_session_preview_pending(&mut self, request_id: i64) {
        self.pending_session_preview_requests.insert(request_id);
    }

    pub fn finish_session_preview(&mut self, request_id: i64) {
        self.pending_session_preview_requests.remove(&request_id);
    }

    pub fn take_pending_session_previews(&mut self) -> Vec<i64> {
        self.pending_session_preview_requests.drain().collect()
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
        if let Some(conversation) = self
            .conversation_mut()
            .filter(|conversation| conversation.thread_id == session_id)
        {
            conversation.model_info = Some(model_info);
        }
    }

    pub fn begin_conversation(&mut self, thread_id: impl Into<String>, cwd: &Path) {
        let thread_id = thread_id.into();
        self.forgotten_conversations.remove(&thread_id);
        self.conversation_revision += 1;
        self.conversation = Some(AgentConversationSnapshot::new(
            thread_id,
            cwd.to_string_lossy(),
        ));
    }

    pub fn restore_conversation(&mut self, conversation: AgentConversationSnapshot) {
        self.root = Some(PathBuf::from(&conversation.cwd));
        self.conversation_revision += 1;
        self.conversation = Some(conversation);
    }

    pub fn reconcile_conversation(
        &mut self,
        thread_id: &str,
        cwd: &Path,
        thread: &serde_json::Value,
    ) -> Option<&AgentConversationSnapshot> {
        let cached = self
            .conversation
            .take()
            .filter(|conversation| conversation.thread_id == thread_id)
            .unwrap_or_else(|| AgentConversationSnapshot::new(thread_id, cwd.to_string_lossy()));
        self.conversation_revision += 1;
        self.conversation = Some(cached.reconciled_with_thread(thread));
        self.conversation.as_ref()
    }

    pub fn conversation_snapshot(&self) -> Option<AgentConversationSnapshot> {
        self.conversation.clone()
    }

    pub fn conversation(&self) -> Option<&AgentConversationSnapshot> {
        self.conversation.as_ref()
    }

    pub fn conversation_revision(&self) -> u64 {
        self.conversation_revision
    }

    fn conversation_mut(&mut self) -> Option<&mut AgentConversationSnapshot> {
        let conversation = self.conversation.as_mut()?;
        self.conversation_revision += 1;
        Some(conversation)
    }

    /// Returns the tool-contract version retained for a particular conversation.
    pub fn conversation_tool_contract_version(&self, session_id: &str) -> Option<u32> {
        self.conversation
            .as_ref()
            .filter(|conversation| conversation.thread_id == session_id)
            .map(|conversation| conversation.tool_contract_version)
    }

    pub fn replace_annotation_records(&mut self, annotations: Vec<AgentAnnotationRecord>) {
        if let Some(conversation) = self.conversation_mut() {
            conversation.annotations = annotations;
        }
    }

    pub fn forget_conversation(&mut self, session_id: &str) {
        self.next_model = None;
        self.forgotten_conversations.insert(session_id.to_string());
        if self
            .conversation
            .as_ref()
            .is_some_and(|conversation| conversation.thread_id == session_id)
        {
            self.conversation_revision += 1;
            self.conversation = None;
        }
    }

    /// Drops the conversation without forgetting its thread, for a recovered
    /// conversation that another editor already drives.
    pub fn discard_conversation(&mut self) {
        if self.conversation.take().is_some() {
            self.conversation_revision += 1;
        }
    }

    pub fn take_forgotten_conversation(&mut self, session_id: &str) -> bool {
        self.forgotten_conversations.remove(session_id)
    }

    pub fn record_user_message(&mut self, session_id: &str, turn_id: &str, text: &str) {
        if let Some(conversation) = self
            .conversation_mut()
            .filter(|conversation| conversation.thread_id == session_id)
        {
            conversation.append_user(turn_id, text);
        }
    }

    pub fn record_agent_delta(&mut self, session_id: &str, text: &str) {
        let Some(turn_id) = self.active_turn_ids.get(session_id).cloned() else {
            return;
        };
        if let Some(conversation) = self
            .conversation_mut()
            .filter(|conversation| conversation.thread_id == session_id)
        {
            conversation.append_agent_delta(&turn_id, text);
        }
    }

    pub fn complete_agent_message(&mut self, session_id: &str, text: &str) {
        let Some(turn_id) = self.active_turn_ids.get(session_id).cloned() else {
            return;
        };
        let Some(conversation) = self
            .conversation_mut()
            .filter(|conversation| conversation.thread_id == session_id)
        else {
            return;
        };
        if let Some(item) = conversation.items.iter_mut().rev().find(|item| {
            item.role == crate::agent_conversation::AgentTranscriptRole::Agent
                && item.turn_id.as_deref() == Some(turn_id.as_str())
        }) {
            item.text = text.to_string();
        } else {
            conversation.append_agent_delta(&turn_id, text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AgentManager;
    use crate::{
        agent_conversation::{AgentConversationSnapshot, AGENT_TOOL_CONTRACT_VERSION},
        agent_tools::PendingEditorToolResponse,
    };
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
    fn retained_conversations_expose_their_tool_contract_version() {
        let mut manager = AgentManager::new();
        manager.begin_conversation("current", Path::new("/workspace"));
        assert_eq!(
            manager.conversation_tool_contract_version("current"),
            Some(AGENT_TOOL_CONTRACT_VERSION)
        );

        let mut old = AgentConversationSnapshot::new("old", "/workspace");
        old.tool_contract_version = 0;
        manager.restore_conversation(old);
        assert_eq!(manager.conversation_tool_contract_version("old"), Some(0));
        assert_eq!(manager.conversation_tool_contract_version("other"), None);
    }

    #[test]
    fn a_lost_bridge_returns_prompts_held_behind_a_stopping_turn() {
        let mut manager = AgentManager::new();
        manager.mark_session_active("session-1");
        manager.begin_stopping("session-1");
        for text in ["first", "second"] {
            manager.queue_prompt(
                "session-1",
                super::QueuedAgentPrompt {
                    text: text.to_string(),
                    context: None,
                },
            );
        }

        manager.clear_active_sessions();

        assert_eq!(
            manager.take_all_queued_prompts(),
            [(
                "session-1".to_string(),
                vec!["first".to_string(), "second".to_string()]
            )]
        );
        assert!(manager.take_all_queued_prompts().is_empty());
        assert!(!manager.is_session_busy("session-1"));
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
