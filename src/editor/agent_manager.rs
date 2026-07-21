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
};

/// Encapsulates background AI agent task state, active turn metrics, and tool channels.
#[derive(Default)]
#[allow(dead_code)]
pub struct AgentManager {
    bridge: Option<CodexBridge>,
    task: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
    workspace: Option<Arc<Mutex<ProposalWorkspace>>>,
    tool_requests: Option<tokio::sync::mpsc::Receiver<PendingEditorTool>>,
    active_sessions: HashSet<String>,
    turn_started_at: HashMap<String, Instant>,
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

    /// Returns `true` if an AI agent task is actively executing.
    pub fn is_task_active(&self) -> bool {
        self.task.as_ref().is_some_and(|handle| !handle.is_finished())
    }

    /// Returns the set of active session IDs.
    pub fn active_sessions(&self) -> &HashSet<String> {
        &self.active_sessions
    }

    /// Marks a session as active.
    pub fn mark_session_active(&mut self, session_id: impl Into<String>) {
        self.active_sessions.insert(session_id.into());
    }

    /// Marks a session as inactive.
    pub fn mark_session_inactive(&mut self, session_id: &str) {
        self.active_sessions.remove(session_id);
    }

    /// Records turn start timestamp for turn duration metrics.
    pub fn record_turn_start(&mut self, turn_id: impl Into<String>) {
        self.turn_started_at.insert(turn_id.into(), Instant::now());
    }

    /// Takes turn start timestamp and returns elapsed duration if recorded.
    pub fn elapsed_turn_duration(&mut self, turn_id: &str) -> Option<std::time::Duration> {
        self.turn_started_at.remove(turn_id).map(|start| start.elapsed())
    }
}
