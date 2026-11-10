//! Editor ownership and callback routing for persisted Agent sessions.

use super::*;
use crate::agent_backend::types::{
    SessionPage, SessionPreview, SessionPreviewRequest, SessionQuery,
};

/// Unknown or obsolete tool catalogs must be replaced by a fresh Red thread.
pub(super) fn session_switch_command(
    cwd: PathBuf,
    session_id: String,
    tool_contract_version: Option<u32>,
) -> CodexCommand {
    if tool_contract_version == Some(crate::agent_conversation::AGENT_TOOL_CONTRACT_VERSION) {
        CodexCommand::ResumeSession { cwd, session_id }
    } else {
        CodexCommand::ContinueSession { cwd, session_id }
    }
}

pub(super) fn session_result_payload(result: &Result<SessionPage, String>) -> Value {
    match result {
        Ok(value) => {
            let mut value = serde_json::to_value(value).expect("agent payloads serialize");
            value["error"] = json!("");
            value
        }
        Err(error) => json!({"sessions": [], "error": error}),
    }
}

pub(super) fn session_preview_payload(result: &Result<SessionPreview, String>) -> Value {
    match result {
        Ok(value) => {
            let mut value = serde_json::to_value(value).expect("agent payloads serialize");
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
        request: SessionQuery,
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
        request: SessionPreviewRequest,
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
    use crate::editor::test_agent::SessionRestore;

    #[test]
    fn browser_resumes_only_sessions_with_a_known_compatible_tool_catalog() {
        for version in [None, Some(0)] {
            assert_eq!(
                SessionRestore::of(session_switch_command(
                    PathBuf::from("/workspace"),
                    "old".into(),
                    version
                )),
                SessionRestore::Fork {
                    session: "old".into()
                }
            );
        }
        assert_eq!(
            SessionRestore::of(session_switch_command(
                PathBuf::from("/workspace"),
                "compatible".into(),
                Some(crate::agent_conversation::AGENT_TOOL_CONTRACT_VERSION),
            )),
            SessionRestore::Rejoin {
                session: "compatible".into()
            }
        );
    }

    #[test]
    fn session_callback_adds_empty_error_without_renaming_fields() {
        let payload = session_result_payload(&Ok(SessionPage {
            sessions: vec![crate::agent_backend::types::SessionSummary {
                id: "one".into(),
                cwd: "/workspace".into(),
                label: "One".into(),
                detail: String::new(),
                snippet: String::new(),
                source: "Red".into(),
                time_group: "Recent".into(),
                is_red: true,
                resumable: true,
                updated_at: 42,
            }],
            more: false,
        }));
        assert_eq!(payload["sessions"][0]["updated_at"], 42);
        assert_eq!(payload["sessions"][0]["is_red"], true);
        assert_eq!(payload["error"], "");
    }

    #[test]
    fn preview_callback_preserves_the_latest_exchange() {
        let payload = session_preview_payload(&Ok(SessionPreview {
            session_id: "one".into(),
            prompt: "Question".into(),
            response: "Answer".into(),
        }));
        assert_eq!(payload["prompt"], "Question");
        assert_eq!(payload["response"], "Answer");
        assert_eq!(payload["error"], "");
    }
}
