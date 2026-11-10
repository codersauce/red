//! The editor-side tool host every backend calls from its worker.

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use super::ids::{SessionId, TurnId};
use crate::agent_tools::EditorToolCall;

/// Which agent call is asking for a tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCtx {
    pub session: SessionId,
    /// The turn that issued the call. `None` for inline and commit-message
    /// conversations, which are not editor turns.
    pub turn: Option<TurnId>,
}

/// Editor operations exposed to an agent's tools. A backend clones the host
/// for each worker it starts, so a restarted worker keeps working.
#[async_trait]
pub trait ToolHost: Clone + Send + 'static {
    /// Reads the authoritative, visible contents of a file.
    async fn read_file(
        &mut self,
        ctx: &ToolCtx,
        path: &str,
        start_line: usize,
        line_count: usize,
    ) -> Result<Value>;
    /// Replaces a file's complete contents through the editor and persists them.
    async fn write_file(
        &mut self,
        ctx: &ToolCtx,
        path: &str,
        expected_revision: u64,
        content: String,
    ) -> Result<Value>;
    /// Dispatches an editor-owned semantic tool call.
    async fn editor_tool(&mut self, ctx: &ToolCtx, call: EditorToolCall) -> Result<Value>;
}
