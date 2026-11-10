//! Test seam between editor tests and the agent backend.
//!
//! Editor tests drive the agent exclusively through [`AgentTestHandle`]: they
//! install it on an editor, script backend events with `emit_*`, observe what
//! the editor asked of the backend with `recv_command`, and simulate backend
//! loss with `lose`. Only this file knows which backend implements it.

use std::{num::NonZeroUsize, time::Duration};

use super::Editor;
use crate::{
    agent_backend::types::ModelQuery,
    codex::{CodexBridge, CodexBridgeWorker, CodexCommand, CodexEvent},
    inline_assist::InlineAssistResult,
};

/// What the editor asked the backend to do, in backend-neutral terms.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AgentCommand {
    Prompt { session: String, text: String },
    Stop { session: String },
    Close { session: String },
    ListModels,
    DefaultModel,
    Other,
}

/// How the browser decided to bring a saved session back.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SessionRestore {
    Rejoin { session: String },
    Fork { session: String },
    Other,
}

impl SessionRestore {
    pub(crate) fn of(command: CodexCommand) -> Self {
        match command {
            CodexCommand::ResumeSession { session_id, .. } => Self::Rejoin {
                session: session_id,
            },
            CodexCommand::ContinueSession { session_id, .. } => Self::Fork {
                session: session_id,
            },
            _ => Self::Other,
        }
    }
}

/// Test-side endpoint of the agent backend installed on an editor.
pub(crate) struct AgentTestHandle {
    worker: CodexBridgeWorker,
}

impl AgentTestHandle {
    /// Installs a scripted backend whose event queue holds `capacity` items.
    pub(crate) fn install(editor: &mut Editor, capacity: usize) -> Self {
        let (bridge, worker) = CodexBridge::channel(NonZeroUsize::new(capacity).unwrap());
        editor.agent_manager.set_bridge(bridge);
        Self { worker }
    }

    /// Installs a backend with a prompt already waiting in its command queue.
    pub(crate) async fn install_with_queued_prompt(
        editor: &mut Editor,
        session: &str,
        text: &str,
    ) -> Self {
        let (bridge, worker) = CodexBridge::channel(NonZeroUsize::new(1).unwrap());
        bridge
            .send(CodexCommand::Prompt {
                session_id: session.to_string(),
                turn: crate::agent_backend::ids::TurnId::mint(),
                text: text.to_string(),
            })
            .await
            .unwrap();
        editor.agent_manager.set_bridge(bridge);
        Self { worker }
    }

    /// Next command the editor sent, or `None` once the backend was disconnected.
    pub(crate) async fn recv_command(&mut self) -> Option<AgentCommand> {
        Some(match self.worker.recv().await? {
            CodexCommand::Prompt {
                session_id, text, ..
            }
            | CodexCommand::PromptWithContext {
                session_id, text, ..
            } => AgentCommand::Prompt {
                session: session_id,
                text,
            },
            CodexCommand::Cancel { session_id } => AgentCommand::Stop {
                session: session_id,
            },
            CodexCommand::CloseSession { session_id } => AgentCommand::Close {
                session: session_id,
            },
            CodexCommand::ModelRequest {
                request: ModelQuery::List,
                ..
            } => AgentCommand::ListModels,
            CodexCommand::ModelRequest {
                request: ModelQuery::ReadDefault { .. },
                ..
            } => AgentCommand::DefaultModel,
            _ => AgentCommand::Other,
        })
    }

    /// True when the editor sends no command within `window`.
    pub(crate) async fn no_command_within(&mut self, window: Duration) -> bool {
        tokio::time::timeout(window, self.worker.recv())
            .await
            .is_err()
    }

    pub(crate) async fn emit_update(&self, session: &str, text: &str) {
        self.emit(CodexEvent::Update {
            session_id: session.to_string(),
            text: text.to_string(),
        })
        .await;
    }
    /// Text of the next update the editor has not yet consumed from the backend.
    pub(crate) fn next_undelivered_update(editor: &mut Editor) -> Option<String> {
        match editor
            .agent_manager
            .bridge_mut()
            .and_then(CodexBridge::try_recv)
        {
            Some(CodexEvent::Update { text, .. }) => Some(text),
            _ => None,
        }
    }

    pub(crate) async fn emit_message_completed(&self, session: &str, text: &str) {
        self.emit(CodexEvent::MessageCompleted {
            session_id: session.to_string(),
            text: text.to_string(),
        })
        .await;
    }

    pub(crate) async fn emit_completed(&self, session: &str, status: &str) {
        self.emit(CodexEvent::Completed {
            session_id: session.to_string(),
            stop_reason: status.to_string(),
        })
        .await;
    }

    pub(crate) async fn emit_cancelled(&self, session: &str) {
        self.emit(CodexEvent::Cancelled {
            session_id: session.to_string(),
        })
        .await;
    }

    pub(crate) async fn emit_cancel_failed(&self, session: &str, message: &str) {
        self.emit(CodexEvent::CancelFailed {
            session_id: session.to_string(),
            message: message.to_string(),
        })
        .await;
    }

    /// A permission prompt for a tool call the editor no longer tracks.
    pub(crate) async fn emit_stale_permission(&self, request_id: &str, session: &str) {
        self.emit(CodexEvent::PermissionRequested {
            request_id: request_id.to_string(),
            session_id: session.to_string(),
            tool_call: serde_json::json!({ "tool_call_id": "stale-tool" }),
            options: serde_json::json!([]),
        })
        .await;
    }

    pub(crate) async fn emit_inline_context_read(&self, request: &str, description: &str) {
        self.emit(CodexEvent::InlineContextRead {
            request_id: request.to_string(),
            description: description.to_string(),
        })
        .await;
    }

    pub(crate) async fn emit_inline_result(
        &self,
        request: &str,
        session: &str,
        result: InlineAssistResult,
    ) {
        self.emit(CodexEvent::InlineResult {
            request_id: request.to_string(),
            session_id: session.to_string(),
            result,
        })
        .await;
    }

    /// Ends the backend with `reason` after everything already emitted is delivered.
    pub(crate) async fn lose(self, editor: &mut Editor, reason: &'static str) {
        drop(self.worker);
        editor
            .agent_manager
            .set_task(tokio::spawn(async move { anyhow::bail!(reason) }));
        while !editor.agent_manager.is_task_finished() {
            tokio::task::yield_now().await;
        }
    }

    /// Detaches the backend from the editor so the command stream closes.
    pub(crate) fn disconnect(editor: &mut Editor) {
        drop(editor.agent_manager.take_bridge());
    }

    async fn emit(&self, event: CodexEvent) {
        self.worker.send(event).await.unwrap();
    }
}
