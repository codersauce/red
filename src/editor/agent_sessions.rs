//! Editor ownership and callback routing for persisted Agent sessions.

use super::*;
use crate::codex::{AgentSessionListRequest, AgentSessionPreviewRequest};

pub(super) fn session_result_payload(result: &Result<Value, String>) -> Value {
    match result {
        Ok(value) => {
            let mut value = value.clone();
            value["error"] = json!("");
            value
        }
        Err(error) => json!({"sessions": [], "error": error}),
    }
}

pub(super) fn session_preview_payload(result: &Result<Value, String>) -> Value {
    match result {
        Ok(value) => {
            let mut value = value.clone();
            value["error"] = json!("");
            value
        }
        Err(error) => json!({"session_id":"", "prompt":"", "response":"", "error":error}),
    }
}

impl Editor {
    pub(super) async fn handle_agent_session_request(
        &mut self,
        runtime: &mut Runtime,
        request_id: RequestId,
        request: AgentSessionListRequest,
    ) -> anyhow::Result<()> {
        if self.agent_manager.is_task_finished() {
            let _ = self
                .finish_agent_bridge(runtime, "Codex app-server stopped unexpectedly")
                .await?;
        }
        let had_bridge = self.agent_manager.has_bridge();
        let cwd = request.cwd.clone();
        let result = self.ensure_agent_bridge(&cwd).and_then(|()| {
            if !had_bridge {
                self.agent_manager.mark_model_only_bridge();
            }
            self.agent_manager
                .bridge()
                .ok_or_else(|| anyhow::anyhow!("Codex app-server did not start"))?
                .try_send(CodexCommand::SessionList {
                    request_id: request_id.get(),
                    request,
                })
        });
        if let Err(error) = result {
            self.plugin_registry
                .resolve_request(
                    runtime,
                    request_id,
                    json!({"sessions": [], "error": error.to_string()}),
                )
                .await?;
        } else {
            self.agent_manager
                .mark_session_request_pending(request_id.get());
        }
        Ok(())
    }

    pub(super) async fn handle_agent_session_preview(
        &mut self,
        runtime: &mut Runtime,
        request_id: RequestId,
        request: AgentSessionPreviewRequest,
    ) -> anyhow::Result<()> {
        if self.agent_manager.is_task_finished() {
            let _ = self
                .finish_agent_bridge(runtime, "Codex app-server stopped unexpectedly")
                .await?;
        }
        let had_bridge = self.agent_manager.has_bridge();
        let result = self.ensure_agent_bridge(&request.cwd).and_then(|()| {
            if !had_bridge {
                self.agent_manager.mark_model_only_bridge();
            }
            self.agent_manager
                .bridge()
                .ok_or_else(|| anyhow::anyhow!("Codex app-server did not start"))?
                .try_send(CodexCommand::SessionPreview {
                    request_id: request_id.get(),
                    session_id: request.session_id,
                })
        });
        if let Err(error) = result {
            self.plugin_registry
                .resolve_request(
                    runtime,
                    request_id,
                    session_preview_payload(&Err(error.to_string())),
                )
                .await?;
        } else {
            self.agent_manager
                .mark_session_preview_pending(request_id.get());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_callback_adds_empty_error_without_renaming_fields() {
        let payload = session_result_payload(&Ok(json!({
            "sessions": [{"id": "one", "updated_at": 42, "is_red": true}]
        })));
        assert_eq!(payload["sessions"][0]["updated_at"], 42);
        assert_eq!(payload["sessions"][0]["is_red"], true);
        assert_eq!(payload["error"], "");
    }

    #[test]
    fn preview_callback_preserves_the_latest_exchange() {
        let payload = session_preview_payload(&Ok(json!({
            "session_id":"one", "prompt":"Question", "response":"Answer"
        })));
        assert_eq!(payload["prompt"], "Question");
        assert_eq!(payload["response"], "Answer");
        assert_eq!(payload["error"], "");
    }
}
