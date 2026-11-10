//! The interface the editor uses to talk to an agent.

use std::{future::Future, path::Path, pin::Pin};

use super::{
    events::Event,
    ids::{BackendKind, Continuity, ContractStamp, QueryId, SavedSession, SessionId, TurnId},
    types::{ModelSelection, Prompt, SessionQuery},
};

/// Why a command did not reach the agent. Nothing was sent.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// The agent cannot be started: its executable is missing, or the
    /// workspace root conflicts with the one it already runs in. The message
    /// is meant for the user.
    #[error("{0}")]
    Unavailable(String),
    /// No live worker can take this command: it never started, died, or was
    /// shut down. The message says so for the user. `new_session`, `restore`
    /// and `ensure` start a worker; the other commands do not.
    #[error("{0}")]
    Gone(String),
}

/// A feature the active agent does not offer.
#[derive(Debug, thiserror::Error)]
#[error("{feature} requires Codex (active agent: {active}). Install Codex and run `codex login`.")]
pub struct Missing {
    pub feature: &'static str,
    pub active: &'static str,
}

impl From<Missing> for AgentError {
    fn from(missing: Missing) -> Self {
        Self::Unavailable(missing.to_string())
    }
}

/// What an agent implements. Object safe, synchronous and non-blocking: every
/// command either enqueues work for the backend's worker and returns, or fails
/// without side effects. Results come back through [`AgentBackend::poll`].
/// Constructing a backend never starts a process.
pub trait AgentBackend: Send {
    /// Which agent this is.
    fn kind(&self) -> BackendKind;
    /// The stamp to record in new snapshots.
    fn contract(&self) -> ContractStamp;
    /// Whether `restore` rejoins, forks, or cannot restore a saved session.
    /// Pure.
    fn continuity(&self, saved: &SavedSession) -> Continuity;

    /// Whether a worker exists that has not reported `Lost`.
    fn is_running(&self) -> bool;
    /// Starts the worker in `root` unless it already runs there. Idempotent.
    /// `Unavailable` when the agent cannot start or runs in another root.
    fn ensure(&mut self, root: &Path) -> Result<(), AgentError>;
    /// The root the worker runs in, if one is running.
    fn root(&self) -> Option<&Path>;

    /// Creates a conversation. Result: `SessionEvent::Created` or `CreateFailed`.
    fn new_session(&mut self, cwd: &Path, model: Option<ModelSelection>) -> Result<(), AgentError>;
    /// Brings a saved conversation back, rejoining or forking as `continuity`
    /// says. Result: `SessionEvent::Restored` or `RestoreFailed`.
    fn restore(&mut self, cwd: &Path, saved: &SavedSession) -> Result<(), AgentError>;
    /// Starts `turn`. Result: `TurnEvent`s ending in exactly one `End`. The
    /// editor keeps at most one live turn per session by queueing prompts.
    fn prompt(
        &mut self,
        session: &SessionId,
        turn: TurnId,
        prompt: Prompt,
    ) -> Result<(), AgentError>;
    /// Asks the agent to stop exactly `turn`. A stop for a turn that already
    /// ended is dropped, never retargeted.
    fn stop(&mut self, session: &SessionId, turn: TurnId) -> Result<(), AgentError>;
    /// Forgets a conversation, including one running an inline request.
    fn close(&mut self, session: &SessionId) -> Result<(), AgentError>;

    // Queries. Each yields exactly one `Event::Reply` carrying the same id.

    /// Resolves a workspace's defaults for a new conversation.
    fn default_model(&mut self, id: QueryId, cwd: &Path) -> Result<(), AgentError>;
    /// Lists the selectable models.
    fn list_models(&mut self, id: QueryId) -> Result<(), AgentError>;
    /// Changes the model of a running conversation.
    fn set_model(
        &mut self,
        id: QueryId,
        session: &SessionId,
        selection: ModelSelection,
    ) -> Result<(), AgentError>;
    /// Lists saved conversations.
    fn list_sessions(&mut self, id: QueryId, query: SessionQuery) -> Result<(), AgentError>;
    /// Reads the latest exchange of a saved conversation.
    fn preview_session(
        &mut self,
        id: QueryId,
        cwd: &Path,
        session: &SessionId,
    ) -> Result<(), AgentError>;

    /// The next ready event; never blocks. After the worker ends, yields its
    /// buffered events first and then `Lost`.
    fn poll(&mut self) -> Option<Event>;
    /// Kills the worker and forgets everything without emitting events. The
    /// next command that starts a worker starts a fresh one.
    fn shutdown(&mut self);
    /// Closes the command channel for a graceful exit and returns the future
    /// that completes when the worker has exited. Emits no events.
    fn detach_for_exit(&mut self) -> Option<Pin<Box<dyn Future<Output = ()> + Send>>>;

    /// Inline assist, when the agent offers it.
    fn inline_assist(&mut self) -> Result<&mut dyn InlineAssist, Missing> {
        Err(Missing {
            feature: "Inline assist",
            active: self.kind().name(),
        })
    }
    /// Commit message generation, when the agent offers it.
    fn commit_messages(&mut self) -> Result<&mut dyn CommitMessages, Missing> {
        Err(Missing {
            feature: "Commit message generation",
            active: self.kind().name(),
        })
    }
}

/// Isolated, ephemeral, tool-limited inline-edit conversations. Progress
/// arrives as `Event::Inline`; closing one is the generic
/// [`AgentBackend::close`].
pub trait InlineAssist {
    /// Starts a conversation in `cwd` and submits the first request.
    fn start(
        &mut self,
        request: String,
        cwd: &Path,
        prompt: String,
        context: String,
    ) -> Result<(), AgentError>;
    /// Continues an inline conversation with updated bounded context.
    fn followup(
        &mut self,
        request: String,
        session: &SessionId,
        prompt: String,
        context: String,
    ) -> Result<(), AgentError>;
}

/// Hidden, tool-free text generation. The result arrives as
/// `Reply { kind: CommitMessage }`; the backend enforces its own expiry.
pub trait CommitMessages {
    /// Generates text for `prompt` in a repository at `cwd`.
    fn generate(&mut self, id: QueryId, cwd: &Path, prompt: String) -> Result<(), AgentError>;
}
