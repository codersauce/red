//! Editor ownership and callback routing for the Agent model picker.

use super::*;
use crate::codex::ModelRequest;

/// The catalog intentionally keeps Codex's camelCase field names. Other agent
/// notifications use `plugin_json`, which would change this public API shape.
pub(super) fn model_result_payload(result: &Result<Value, String>) -> Value {
    match result {
        Ok(value) => {
            let mut value = value.clone();
            value["error"] = json!("");
            value
        }
        Err(error) => json!({"error": error}),
    }
}

impl Editor {
    pub(super) async fn handle_agent_model_request(
        &mut self,
        runtime: &mut Runtime,
        request_id: RequestId,
        request: ModelRequest,
    ) -> anyhow::Result<()> {
        if let ModelRequest::Set {
            session_id,
            selection,
        } = &request
        {
            if selection.model.trim().is_empty() {
                self.plugin_registry
                    .resolve_request(runtime, request_id, json!({"error": "Choose a model"}))
                    .await?;
                return Ok(());
            }
            let current = self.agent_manager.conversation_snapshot();
            if session_id.is_empty() && current.is_none() {
                self.agent_manager.set_next_model(selection.clone());
                self.plugin_registry
                    .resolve_request(runtime, request_id, json!({"accepted": true, "error": ""}))
                    .await?;
                return Ok(());
            }
            if current
                .as_ref()
                .is_none_or(|conversation| conversation.thread_id != *session_id)
            {
                self.plugin_registry
                    .resolve_request(
                        runtime,
                        request_id,
                        json!({"error": "The agent conversation changed; choose its model again"}),
                    )
                    .await?;
                return Ok(());
            }
        }
        if self.agent_manager.is_task_finished() {
            let _ = self
                .finish_agent_bridge(runtime, "Codex app-server stopped unexpectedly")
                .await?;
        }
        let had_bridge = self.agent_manager.has_bridge();
        let result = self
            .ensure_agent_bridge(&get_workspace_path())
            .and_then(|()| {
                if !had_bridge {
                    self.agent_manager.mark_model_only_bridge();
                }
                self.agent_manager
                    .bridge()
                    .ok_or_else(|| anyhow::anyhow!("Codex app-server did not start"))?
                    .try_send(CodexCommand::ModelRequest {
                        request_id: request_id.get(),
                        request,
                    })
            });
        if let Err(error) = result {
            self.plugin_registry
                .resolve_request(runtime, request_id, json!({"error": error.to_string()}))
                .await?;
        } else {
            self.agent_manager
                .mark_model_request_pending(request_id.get());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_catalog_callback_preserves_protocol_field_names() {
        let payload = model_result_payload(&Ok(
            json!({"models":[{"model":"test","displayName":"Test","supportedReasoningEfforts":[{"reasoningEffort":"high"}]}]}),
        ));
        assert_eq!(payload["models"][0]["displayName"], "Test");
        assert_eq!(
            payload["models"][0]["supportedReasoningEfforts"][0]["reasoningEffort"],
            "high"
        );
        assert_eq!(payload["error"], "");
    }

    #[tokio::test]
    async fn metadata_only_bridge_failure_resolves_callback_without_session_loss() {
        while ACTION_DISPATCHER.try_recv_request().is_some() {}
        let plugin_root = tempfile::tempdir().unwrap();
        let plugin_path = plugin_root.path().join("model-preview.hk");
        std::fs::write(&plugin_path, r#"
            pub fn activate() {
                red::add_command("PreviewModel", preview);
                red::on("agent:session_lost", lost);
            }
            fn preview() { red::request("AgentReadDefaultModel", loaded); }
            fn loaded(result: Json) { red::execute("Print", "preview:" + red::string(result.error, "")); }
            fn lost(event: Json) { red::execute("Print", "unexpected conversation loss"); }
        "#).unwrap();
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
        let mut runtime = Runtime::new();
        editor
            .plugin_registry
            .add("model_preview", plugin_path.to_string_lossy().as_ref());
        editor
            .plugin_registry
            .initialize(&mut runtime)
            .await
            .unwrap();
        runtime.execute_command("PreviewModel").await.unwrap();
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::AgentModelRequest { request_id, .. } => request_id,
            _ => panic!("expected model preview request"),
        };
        let (bridge, worker) = CodexBridge::channel(NonZeroUsize::new(1).unwrap());
        drop(worker);
        editor.agent_manager.set_bridge(bridge);
        editor.agent_manager.mark_model_only_bridge();
        editor
            .agent_manager
            .mark_model_request_pending(request_id.get());
        editor
            .agent_manager
            .set_task(tokio::spawn(async { anyhow::bail!("preview unavailable") }));
        while !editor.agent_manager.is_task_finished() {
            tokio::task::yield_now().await;
        }
        let mut buffer = RenderBuffer::new(80, 24, &Style::default());
        editor
            .service_background(&mut buffer, &mut runtime)
            .await
            .unwrap();
        assert!(editor
            .last_error
            .as_deref()
            .is_some_and(|message| message.starts_with("preview:")
                && message.contains("preview unavailable")));
        assert!(!editor.agent_manager.has_bridge());
        assert!(editor
            .agent_manager
            .take_pending_model_requests()
            .is_empty());
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[test]
    fn model_shortcut_precedes_composer_input_and_preserves_focus_and_draft() {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            lsp,
            100,
            30,
            config,
            Theme::default(),
            vec![Buffer::new(None, String::new())],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        editor.panel_manager.create_text_panel(
            "agent-conversation".into(),
            plugin::PanelConfig {
                title: Some("Agent".into()),
                composer: Some(plugin::TextPanelComposerConfig {
                    placeholder: "Ask".into(),
                    rows: 3,
                }),
                ..Default::default()
            },
        );
        editor.panel_manager.set_text_panel_header_detail(
            "agent-conversation",
            Some(plugin::TextPanelHeaderDetail {
                text: "Codex".into(),
                secondary: String::new(),
                compact_text: String::new(),
                action: Some("model".into()),
                shortcut: Some("Alt-m".into()),
            }),
        );
        editor
            .panel_manager
            .focus_text_panel_composer("agent-conversation");
        editor
            .panel_manager
            .handle_focused_text_input(&Event::Paste("unfinished draft".into()), 100);
        let key = Event::Key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT));
        for composer in [true, false] {
            if !composer {
                editor.panel_manager.focus_focused_text_scrollback(100);
            }
            let before = editor.panel_manager.snapshot(100);
            let action = editor.handle_panel_event(&key, None).unwrap();
            assert!(
                matches!(action, KeyAction::Multiple(actions) if actions.iter().any(|action| matches!(action, Action::NotifyPlugins(name, payload) if name == "panel:event:agent-conversation" && payload["action"] == "model")))
            );
            assert_eq!(editor.panel_manager.snapshot(100), before);
            assert!(editor
                .panel_manager
                .surface_actions()
                .iter()
                .any(|action| action.key == "Alt-m"));
        }
        editor
            .panel_manager
            .create_text_panel("other".into(), plugin::PanelConfig::default());
        editor.panel_manager.focus_panel("other");
        assert!(editor
            .panel_manager
            .header_shortcut_event("Alt-m")
            .is_none());
    }
}
