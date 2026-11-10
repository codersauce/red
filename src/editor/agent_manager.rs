//! Agent manager sub-controller for the agent backend and the editor's tool channel.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    time::Instant,
};

use crate::{
    agent_backend::{
        backend::AgentBackend,
        ids::{SessionId, TurnId},
    },
    agent_conversation::{AgentAnnotationRecord, AgentConversationSnapshot},
    agent_tools::{PendingEditorTool, PendingEditorToolResponse},
};

/// What the editor was asking for when a command found the agent gone. The loss
/// that follows reports it the way the failed command would have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentLossContext {
    /// A prompt that never reached the agent; the plugin gets it back so it can
    /// retry the message.
    Prompt { session_id: String, prompt: String },
    /// A new session that could not be requested.
    NewSession,
    /// A saved session that could not be rejoined or forked.
    Restore { session_id: String },
}

/// Encapsulates the agent backend, active turn metrics, and the tool channel.
pub struct AgentManager {
    agents: Box<dyn AgentBackend>,
    root: Option<PathBuf>,
    /// Tool calls from the agent, for the editor's whole lifetime. A lost agent
    /// leaves it open: the next one reuses it through the same tool host.
    tool_requests: tokio::sync::mpsc::Receiver<PendingEditorTool>,
    playback_tool: Option<PendingEditorTool>,
    playback_ready_at: Option<Instant>,
    playback_response: Option<PendingEditorToolResponse>,
    playback_response_ready_at: Option<Instant>,
    /// Sessions with a turn in flight; absent sessions are idle.
    turn_phases: HashMap<String, AgentTurnPhase>,
    active_turn_ids: HashMap<String, String>,
    turn_started_at: HashMap<String, Instant>,
    loss_context: Option<AgentLossContext>,
    next_model: Option<crate::agent_backend::types::ModelSelection>,
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
    /// Creates a manager over `agents`, whose tool calls arrive on `tool_requests`.
    pub fn new(
        agents: Box<dyn AgentBackend>,
        tool_requests: tokio::sync::mpsc::Receiver<PendingEditorTool>,
    ) -> Self {
        Self {
            agents,
            root: None,
            tool_requests,
            playback_tool: None,
            playback_ready_at: None,
            playback_response: None,
            playback_response_ready_at: None,
            turn_phases: HashMap::new(),
            active_turn_ids: HashMap::new(),
            turn_started_at: HashMap::new(),
            loss_context: None,
            next_model: None,
            conversation: None,
            conversation_revision: 0,
            forgotten_conversations: HashSet::new(),
            queued_prompts: HashMap::new(),
            pane: super::agent_pane::AgentPaneLifecycle::default(),
            pane_effects: Vec::new(),
            published_pane: None,
        }
    }

    /// A manager over a scripted backend that nothing drives, for unit tests.
    #[cfg(test)]
    pub fn for_test() -> Self {
        let (agents, _script) = crate::agent_backend::testing::open_scripted();
        Self::new(agents, tokio::sync::mpsc::channel(1).1)
    }

    /// Swaps the backend, so tests can script it.
    #[cfg(test)]
    pub fn replace_agents(&mut self, agents: Box<dyn AgentBackend>) {
        self.agents = agents;
    }

    /// Swaps the channel editor tool calls arrive on, so tests can send some.
    #[cfg(test)]
    pub fn set_tool_requests(&mut self, requests: tokio::sync::mpsc::Receiver<PendingEditorTool>) {
        self.tool_requests = requests;
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

    /// The agent backend, for questions that do not change it.
    pub fn agents(&self) -> &dyn AgentBackend {
        &*self.agents
    }

    /// The agent backend, for commands.
    pub fn agents_mut(&mut self) -> &mut dyn AgentBackend {
        &mut *self.agents
    }

    /// Returns `true` while the agent backend has a live agent.
    pub fn is_running(&self) -> bool {
        self.agents.is_running()
    }

    /// Closes a session the editor is done with. Nothing to close when no agent
    /// is running, and a failure to reach it is not the user's to hear about.
    pub fn close_session(&mut self, session_id: impl Into<String>) {
        if self.agents.is_running() {
            let _ = self.agents.close(&SessionId::from_raw(session_id.into()));
        }
    }

    /// The turn the editor started for a session and has not seen end.
    pub fn live_turn(&self, session_id: &str) -> Option<TurnId> {
        let turn = self.active_turn_ids.get(session_id)?;
        uuid::Uuid::parse_str(turn).ok().map(TurnId::from_uuid)
    }

    /// Remembers what a failed command was doing, for the loss that follows.
    pub fn set_loss_context(&mut self, context: AgentLossContext) {
        self.loss_context = Some(context);
    }

    pub fn take_loss_context(&mut self) -> Option<AgentLossContext> {
        self.loss_context.take()
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    pub fn set_root(&mut self, root: Option<PathBuf>) {
        self.root = root;
    }

    /// Answers every tool call still waiting and drops the staged playback, for
    /// an agent that is gone: nothing is left to use the results.
    pub fn clear_tool_requests(&mut self) {
        while let Ok(pending) = self.tool_requests.try_recv() {
            let _ = pending.response.send(Err("the agent stopped".to_string()));
        }
        self.playback_tool = None;
        self.playback_ready_at = None;
        self.playback_response = None;
        self.playback_response_ready_at = None;
    }

    pub fn try_recv_tool_request(&mut self) -> Option<PendingEditorTool> {
        self.tool_requests.try_recv().ok()
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

    /// Ends everything a lost session had in flight and returns the prompts it
    /// never received, so the pane can hand them back.
    pub fn end_session_turns(&mut self, session_id: &str) -> Vec<String> {
        self.turn_phases.remove(session_id);
        self.active_turn_ids.remove(session_id);
        self.turn_started_at.remove(session_id);
        self.queued_prompts
            .remove(session_id)
            .map(|queue| queue.into_iter().map(|prompt| prompt.text).collect())
            .unwrap_or_default()
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

    pub fn set_next_model(&mut self, selection: crate::agent_backend::types::ModelSelection) {
        self.next_model = Some(selection);
    }

    pub fn next_model(&self) -> Option<&crate::agent_backend::types::ModelSelection> {
        self.next_model.as_ref()
    }

    pub fn take_next_model(&mut self) -> Option<crate::agent_backend::types::ModelSelection> {
        self.next_model.take()
    }

    pub fn set_conversation_model(
        &mut self,
        session_id: &str,
        model_info: crate::agent_backend::types::ModelInfo,
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
        thread: &crate::agent_backend::types::RestoredHistory,
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
        let mut manager = AgentManager::for_test();
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
        let mut manager = AgentManager::for_test();
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
    fn a_lost_agent_returns_prompts_held_behind_a_stopping_turn() {
        let mut manager = AgentManager::for_test();
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
        let mut manager = AgentManager::for_test();
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

#[cfg(test)]
mod turn_and_tool_tests {
    use super::AgentManager;
    use crate::{
        agent_backend::ids::{SessionId, TurnId},
        agent_tools::{EditorToolCall, EditorToolRequest, PendingEditorTool},
    };

    #[test]
    fn only_a_turn_the_editor_minted_can_be_stopped() {
        let mut manager = AgentManager::for_test();
        assert_eq!(manager.live_turn("session-1"), None);
        manager.set_turn_id("session-1", "turn-1");
        assert_eq!(manager.live_turn("session-1"), None);
        let turn = TurnId::mint();
        manager.set_turn_id("session-1", turn.to_string());
        assert_eq!(manager.live_turn("session-1"), Some(turn));
        manager.finish_turn("session-1");
        assert_eq!(manager.live_turn("session-1"), None);
    }

    #[tokio::test]
    async fn a_lost_agent_gets_its_waiting_tool_calls_answered() {
        let (agents, _script) = crate::agent_backend::testing::open_scripted();
        let (sender, receiver) = tokio::sync::mpsc::channel(4);
        let mut manager = AgentManager::new(agents, receiver);
        let (response, answer) = tokio::sync::oneshot::channel();
        sender
            .send(PendingEditorTool {
                request: EditorToolRequest {
                    session_id: SessionId::from_raw("session-1".to_string()),
                    turn: None,
                    call: EditorToolCall::ReadFile {
                        path: "a.rs".to_string(),
                        start_line: 1,
                        line_count: 1,
                    },
                },
                response,
            })
            .await
            .unwrap();

        manager.clear_tool_requests();

        assert_eq!(answer.await.unwrap(), Err("the agent stopped".to_string()));
        assert!(manager.try_recv_tool_request().is_none());
    }
}
