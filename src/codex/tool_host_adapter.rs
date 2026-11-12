//! Bridges an editor `ToolHost` onto the wire-level `CodexToolHost` the worker calls.

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use super::{CodexToolCtx, CodexToolHost};
use crate::agent_backend::{
    ids::SessionId,
    tool_host::{ToolCtx, ToolHost},
};
use crate::agent_tools::EditorToolCall;

/// Lets a backend-neutral `ToolHost` serve the Codex worker.
pub struct ToolHostAdapter<H: ToolHost>(pub H);

fn tool_ctx(ctx: &CodexToolCtx<'_>) -> ToolCtx {
    ToolCtx {
        session: SessionId::from_raw(ctx.session_id.to_string()),
        turn: ctx.turn,
    }
}

#[async_trait]
impl<H: ToolHost> CodexToolHost for ToolHostAdapter<H> {
    async fn read_file(
        &mut self,
        ctx: &CodexToolCtx<'_>,
        path: &str,
        start_line: usize,
        line_count: usize,
    ) -> Result<Value> {
        self.0
            .read_file(&tool_ctx(ctx), path, start_line, line_count)
            .await
    }

    async fn write_file(
        &mut self,
        ctx: &CodexToolCtx<'_>,
        path: &str,
        expected_revision: u64,
        content: String,
    ) -> Result<Value> {
        self.0
            .write_file(&tool_ctx(ctx), path, expected_revision, content)
            .await
    }

    async fn editor_tool(&mut self, ctx: &CodexToolCtx<'_>, call: EditorToolCall) -> Result<Value> {
        self.0.editor_tool(&tool_ctx(ctx), call).await
    }
}
