//! Agent manager sub-controller for Codex AI app-server integration and tool channels.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Instant,
};

use crate::{
    agent_tools::PendingEditorTool,
    agent_workspace::ProposalWorkspace,
    codex::CodexBridge,
    replay::{GitObjectId, ReplayAgentScope},
};

/// Editor-verified provenance and authority for a dedicated Replay Codex turn.
#[derive(Debug, Clone)]
pub struct ReplayAgentSession {
    pub workspace_id: String,
    pub step_id: String,
    pub scope: ReplayAgentScope,
    pub prompt: String,
    pub target_commit: GitObjectId,
}

/// Encapsulates background AI agent task state, active turn metrics, and tool channels.
#[derive(Default)]
pub struct AgentManager {
    bridge: Option<CodexBridge>,
    task: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
    workspace: Option<Arc<Mutex<ProposalWorkspace>>>,
    tool_requests: Option<tokio::sync::mpsc::Receiver<PendingEditorTool>>,
    active_sessions: HashSet<String>,
    turn_started_at: HashMap<String, Instant>,
    pending_replay_session: Option<ReplayAgentSession>,
    replay_sessions: HashMap<String, ReplayAgentSession>,
    general_sessions: HashSet<String>,
    read_only_sessions: Arc<Mutex<HashSet<String>>>,
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

    pub fn workspace(&self) -> Option<&Arc<Mutex<ProposalWorkspace>>> {
        self.workspace.as_ref()
    }

    pub fn workspace_cloned(&self) -> Option<Arc<Mutex<ProposalWorkspace>>> {
        self.workspace.clone()
    }

    pub fn set_workspace(&mut self, workspace: Option<Arc<Mutex<ProposalWorkspace>>>) {
        self.workspace = workspace;
    }

    pub fn set_tool_requests(&mut self, requests: tokio::sync::mpsc::Receiver<PendingEditorTool>) {
        self.tool_requests = Some(requests);
    }

    pub fn clear_tool_requests(&mut self) {
        self.tool_requests = None;
    }

    pub fn try_recv_tool_request(&mut self) -> Option<PendingEditorTool> {
        self.tool_requests
            .as_mut()
            .and_then(|requests| requests.try_recv().ok())
    }

    /// Marks a session as active.
    pub fn mark_session_active(&mut self, session_id: impl Into<String>) {
        self.active_sessions.insert(session_id.into());
    }

    /// Marks a session as inactive.
    pub fn mark_session_inactive(&mut self, session_id: &str) {
        self.active_sessions.remove(session_id);
    }

    pub fn is_session_active(&self, session_id: &str) -> bool {
        self.active_sessions.contains(session_id)
    }

    pub fn clear_active_sessions(&mut self) {
        self.active_sessions.clear();
    }

    /// Marks the next created Codex session as owned exclusively by Replay.
    pub fn begin_replay_session(&mut self, session: ReplayAgentSession) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.pending_replay_session.is_none(),
            "another Replay Codex request is already starting"
        );
        self.pending_replay_session = Some(session);
        Ok(())
    }

    /// Takes a pending Replay request when the app-server creates its session.
    pub fn take_pending_replay_session(&mut self) -> Option<ReplayAgentSession> {
        self.pending_replay_session.take()
    }

    /// Returns whether an app-server setup failure belongs to Replay.
    pub fn has_pending_replay_session(&self) -> bool {
        self.pending_replay_session.is_some()
    }

    /// Registers an isolated Replay session and its enforced source-write policy.
    pub fn register_replay_session(
        &mut self,
        session_id: String,
        session: ReplayAgentSession,
    ) -> anyhow::Result<()> {
        if !session.scope.permits_source_proposals() {
            self.read_only_sessions
                .lock()
                .map_err(|_| anyhow::anyhow!("agent session policy lock is poisoned"))?
                .insert(session_id.clone());
        }
        self.replay_sessions.insert(session_id, session);
        Ok(())
    }

    /// Registers an ordinary conversation without granting Replay ownership.
    pub fn register_general_session(&mut self, session_id: String) {
        self.general_sessions.insert(session_id);
    }

    /// Returns the verified Replay identity associated with a Codex session.
    pub fn replay_session(&self, session_id: &str) -> Option<&ReplayAgentSession> {
        self.replay_sessions.get(session_id)
    }

    /// Refreshes one live PR-wide conversation without changing its write authority.
    pub fn update_replay_session(
        &mut self,
        session_id: &str,
        session: ReplayAgentSession,
    ) -> anyhow::Result<()> {
        let existing = self
            .replay_sessions
            .get_mut(session_id)
            .ok_or_else(|| anyhow::anyhow!("the Replay Codex session is no longer available"))?;
        anyhow::ensure!(
            existing.workspace_id == session.workspace_id
                && existing.target_commit == session.target_commit,
            "the Replay Codex session no longer matches the pinned review"
        );
        anyhow::ensure!(
            existing.scope.permits_source_proposals() == session.scope.permits_source_proposals(),
            "the Replay Codex session cannot change its source-write authority"
        );
        *existing = session;
        Ok(())
    }

    /// Returns isolated Replay sessions that must hear about worker shutdown.
    pub fn replay_sessions(&self) -> Vec<(String, ReplayAgentSession)> {
        self.replay_sessions
            .iter()
            .map(|(session_id, session)| (session_id.clone(), session.clone()))
            .collect()
    }

    /// Returns whether changing proposal roots would displace an ordinary agent.
    pub fn has_general_sessions(&self) -> bool {
        !self.general_sessions.is_empty()
    }

    /// Shares the enforced reviewer-session policy with the Codex tool host.
    pub fn read_only_sessions(&self) -> Arc<Mutex<HashSet<String>>> {
        Arc::clone(&self.read_only_sessions)
    }

    /// Drops ownership and access policy after the underlying worker stops.
    pub fn clear_session_ownership(&mut self) {
        self.pending_replay_session = None;
        self.replay_sessions.clear();
        self.general_sessions.clear();
        if let Ok(mut sessions) = self.read_only_sessions.lock() {
            sessions.clear();
        }
    }

    /// Stops tracking one explicitly closed Codex session.
    pub fn forget_session(&mut self, session_id: &str) {
        self.replay_sessions.remove(session_id);
        self.general_sessions.remove(session_id);
        if let Ok(mut sessions) = self.read_only_sessions.lock() {
            sessions.remove(session_id);
        }
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
}

#[cfg(test)]
mod tests {
    use super::{AgentManager, ReplayAgentSession};
    use crate::replay::{GitObjectId, ReplayAgentScope};

    #[test]
    fn owns_session_and_turn_lifecycle() {
        let mut manager = AgentManager::new();
        manager.mark_session_active("session-1");
        manager.record_turn_start("session-1");

        assert!(manager.is_session_active("session-1"));
        assert!(manager.elapsed_turn_duration("session-1").is_some());

        manager.mark_session_inactive("session-1");
        assert!(!manager.is_session_active("session-1"));
    }

    #[test]
    fn replay_reviewer_ownership_enforces_read_only_session_policy() {
        let mut manager = AgentManager::new();
        let session = ReplayAgentSession {
            workspace_id: "review-1".to_string(),
            step_id: "step-1".to_string(),
            scope: ReplayAgentScope::CurrentChange,
            prompt: "Check the boundary.".to_string(),
            target_commit: GitObjectId::parse(&"a".repeat(40)).unwrap(),
        };
        manager.begin_replay_session(session.clone()).unwrap();
        assert!(manager.has_pending_replay_session());
        assert!(manager.begin_replay_session(session).is_err());

        let session = manager.take_pending_replay_session().unwrap();
        manager
            .register_replay_session("codex-review-1".to_string(), session)
            .unwrap();
        assert_eq!(
            manager
                .replay_session("codex-review-1")
                .unwrap()
                .workspace_id,
            "review-1",
        );
        assert!(manager
            .read_only_sessions()
            .lock()
            .unwrap()
            .contains("codex-review-1"));

        manager.forget_session("codex-review-1");
        assert!(!manager
            .read_only_sessions()
            .lock()
            .unwrap()
            .contains("codex-review-1"));
    }

    #[test]
    fn replay_follow_ups_reuse_the_pinned_thread_without_escalating_authority() {
        let mut manager = AgentManager::new();
        let pinned_commit = GitObjectId::parse(&"a".repeat(40)).unwrap();
        manager
            .register_replay_session(
                "codex-review-1".to_string(),
                ReplayAgentSession {
                    workspace_id: "review-1".to_string(),
                    step_id: "step-1".to_string(),
                    scope: ReplayAgentScope::CurrentChange,
                    prompt: "Explain this change.".to_string(),
                    target_commit: pinned_commit.clone(),
                },
            )
            .unwrap();

        manager
            .update_replay_session(
                "codex-review-1",
                ReplayAgentSession {
                    workspace_id: "review-1".to_string(),
                    step_id: "step-2".to_string(),
                    scope: ReplayAgentScope::PullRequest,
                    prompt: "How does the next change depend on it?".to_string(),
                    target_commit: pinned_commit.clone(),
                },
            )
            .unwrap();

        let reused = manager.replay_session("codex-review-1").unwrap();
        assert_eq!(reused.step_id, "step-2");
        assert_eq!(reused.scope, ReplayAgentScope::PullRequest);
        assert!(manager
            .read_only_sessions()
            .lock()
            .unwrap()
            .contains("codex-review-1"));

        let escalation = manager.update_replay_session(
            "codex-review-1",
            ReplayAgentSession {
                workspace_id: "review-1".to_string(),
                step_id: "step-2".to_string(),
                scope: ReplayAgentScope::AuthorFix,
                prompt: "Try to stage a fix.".to_string(),
                target_commit: pinned_commit,
            },
        );
        assert!(escalation.is_err());
        assert_eq!(
            manager.replay_session("codex-review-1").unwrap().scope,
            ReplayAgentScope::PullRequest
        );
    }

    #[test]
    fn original_author_sessions_can_stage_source_proposals() {
        let mut manager = AgentManager::new();
        manager
            .register_replay_session(
                "codex-author-1".to_string(),
                ReplayAgentSession {
                    workspace_id: "review-1".to_string(),
                    step_id: "step-1".to_string(),
                    scope: ReplayAgentScope::AuthorFix,
                    prompt: "Fix this across the repository.".to_string(),
                    target_commit: GitObjectId::parse(&"a".repeat(40)).unwrap(),
                },
            )
            .unwrap();

        assert!(!manager
            .read_only_sessions()
            .lock()
            .unwrap()
            .contains("codex-author-1"));
    }

    #[test]
    fn explicit_reviewer_draft_sessions_remain_strictly_read_only() {
        for (index, scope) in [
            ReplayAgentScope::InlineComment,
            ReplayAgentScope::ReviewSummary,
        ]
        .into_iter()
        .enumerate()
        {
            let mut manager = AgentManager::new();
            let session_id = format!("codex-review-draft-{index}");
            manager
                .register_replay_session(
                    session_id.clone(),
                    ReplayAgentSession {
                        workspace_id: "review-1".to_string(),
                        step_id: "step-1".to_string(),
                        scope,
                        prompt: "Draft a review observation.".to_string(),
                        target_commit: GitObjectId::parse(&"a".repeat(40)).unwrap(),
                    },
                )
                .unwrap();

            assert!(!scope.answers_question());
            assert!(!scope.permits_source_proposals());
            assert!(manager
                .read_only_sessions()
                .lock()
                .unwrap()
                .contains(&session_id));
        }
    }
}
