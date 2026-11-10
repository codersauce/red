//! Editor ownership and callback routing for persisted Agent sessions.

use super::*;
use crate::agent_backend::{
    ids::{QueryId, SessionId},
    types::{SessionPage, SessionPreview, SessionPreviewRequest, SessionQuery},
};

pub(crate) fn session_result_payload(result: &Result<SessionPage, String>) -> Value {
    match result {
        Ok(value) => {
            let mut value = serde_json::to_value(value).expect("agent payloads serialize");
            value["error"] = json!("");
            value
        }
        Err(error) => json!({"sessions": [], "error": error}),
    }
}

pub(crate) fn session_preview_payload(result: &Result<SessionPreview, String>) -> Value {
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
        let cwd = request.cwd.clone();
        let result = self.ensure_agent(&cwd).and_then(|()| {
            self.agent_manager
                .agents_mut()
                .list_sessions(QueryId(request_id.get()), request)
                .map_err(anyhow::Error::from)
        });
        if let Err(error) = result {
            self.plugin_registry
                .resolve_request(
                    runtime,
                    request_id,
                    json!({"sessions": [], "error": error.to_string()}),
                )
                .await?;
        }
        Ok(())
    }

    pub(super) async fn handle_agent_session_preview(
        &mut self,
        runtime: &mut Runtime,
        request_id: RequestId,
        request: SessionPreviewRequest,
    ) -> anyhow::Result<()> {
        let result = self.ensure_agent(&request.cwd).and_then(|()| {
            self.agent_manager
                .agents_mut()
                .preview_session(
                    QueryId(request_id.get()),
                    &request.cwd,
                    &SessionId::from_raw(request.session_id),
                )
                .map_err(anyhow::Error::from)
        });
        if let Err(error) = result {
            self.plugin_registry
                .resolve_request(
                    runtime,
                    request_id,
                    session_preview_payload(&Err(error.to_string())),
                )
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_conversation::{AgentConversationSnapshot, AGENT_TOOL_CONTRACT_VERSION};
    use crate::editor::test_agent::{AgentCommand, AgentTestHandle};

    fn editor() -> Editor {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            lsp,
            80,
            24,
            config,
            Theme::default(),
            vec![Buffer::new(None, String::new())],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        editor
    }

    #[tokio::test]
    async fn browser_resumes_only_sessions_with_a_known_compatible_tool_catalog() {
        let current = AGENT_TOOL_CONTRACT_VERSION;
        for (stamp, resume, expected) in [
            (None, true, None),
            (Some(0), true, Some(0)),
            (Some(current), true, Some(current)),
            (Some(current), false, None),
        ] {
            let mut editor = editor();
            let agent = AgentTestHandle::install(&mut editor);
            if let Some(stamp) = stamp {
                let mut snapshot = AgentConversationSnapshot::new("old", "/workspace");
                snapshot.tool_contract_version = stamp;
                editor.agent_manager.restore_conversation(snapshot);
            }
            let mut runtime = Runtime::new();
            editor
                .agent_switch_session(&mut runtime, "/workspace".into(), "old".into(), resume)
                .await
                .unwrap();
            assert_eq!(
                agent.commands(),
                [AgentCommand::Restore {
                    session: "old".into(),
                    contract: expected,
                }],
                "stamp {stamp:?}, resume {resume}"
            );
        }
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
