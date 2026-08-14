//! Agent manager sub-controller for Codex AI app-server integration and tool channels.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Instant,
};

use crate::{
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
    active_sessions: HashSet<String>,
    active_turn_ids: HashMap<String, String>,
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
        self.active_sessions.insert(session_id.into());
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
}

#[cfg(test)]
mod tests {
    use super::AgentManager;
    use crate::agent_tools::PendingEditorToolResponse;
    use serde_json::json;
    use std::time::{Duration, Instant};

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
