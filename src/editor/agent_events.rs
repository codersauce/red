//! Editor handling of backend events: replies, inline events, session and turn events.

use super::*;

impl Editor {
    /// Applies one backend event to the editor and forwards it to plugins.
    pub(super) async fn handle_agent_event(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
        event: CodexEvent,
    ) -> anyhow::Result<()> {
        if matches!(event, CodexEvent::TurnAccepted { .. }) {
            // The pane does not read acceptance yet.
            return Ok(());
        }
        let event = match event {
            CodexEvent::ModelRequestCompleted { request_id, result } => {
                return self.agent_model_reply(runtime, request_id, &result).await;
            }
            CodexEvent::SessionListCompleted { request_id, result } => {
                return self
                    .agent_session_list_reply(runtime, request_id, &result)
                    .await;
            }
            CodexEvent::SessionPreviewCompleted { request_id, result } => {
                return self
                    .agent_session_preview_reply(runtime, request_id, &result)
                    .await;
            }
            CodexEvent::CommitMessageGenerated { request_id, result } => {
                return self
                    .agent_commit_message_reply(runtime, request_id, &result)
                    .await;
            }
            CodexEvent::InlineContextRead {
                request_id,
                description,
            } => {
                self.agent_inline_context_read(request_id, description);
                return Ok(());
            }
            CodexEvent::InlineAnswerDelta { request_id, text } => {
                self.agent_inline_answer_delta(request_id, text);
                return Ok(());
            }
            CodexEvent::InlineSessionCreated {
                request_id,
                session_id,
            } => {
                self.agent_inline_session_created(request_id, session_id);
                return Ok(());
            }
            CodexEvent::InlineResult {
                request_id,
                session_id,
                result,
            } => {
                return self
                    .agent_inline_result(buffer, runtime, request_id, session_id, result)
                    .await;
            }
            CodexEvent::InlineFailed {
                request_id,
                session_id,
                message,
            } => {
                return self.agent_inline_failed(buffer, request_id, session_id, message);
            }
            event => event,
        };
        match &event {
            CodexEvent::SessionModelChanged {
                session_id,
                model_info,
            } => {
                self.agent_manager
                    .set_conversation_model(session_id, model_info.clone());
            }
            CodexEvent::SessionCreated { session_id } => {
                self.agent_manager.take_next_model();
                let root = self
                    .agent_manager
                    .root()
                    .map(Path::to_path_buf)
                    .unwrap_or_default();
                self.agent_manager.begin_conversation(session_id, &root);
            }
            CodexEvent::SessionRestored { session_id, thread } => {
                return self
                    .agent_session_restored(runtime, session_id, thread)
                    .await;
            }
            _ => {}
        }
        self.handle_agent_turn_event(buffer, runtime, event).await
    }

    async fn agent_model_reply(
        &mut self,
        runtime: &mut Runtime,
        request_id: i64,
        result: &Result<crate::agent_backend::types::ModelResult, String>,
    ) -> anyhow::Result<()> {
        self.agent_manager.finish_model_request(request_id);
        self.plugin_registry
            .resolve_request(
                runtime,
                RequestId::from_raw(request_id),
                agent_models::model_result_payload(result),
            )
            .await?;
        Ok(())
    }

    async fn agent_session_list_reply(
        &mut self,
        runtime: &mut Runtime,
        request_id: i64,
        result: &Result<crate::agent_backend::types::SessionPage, String>,
    ) -> anyhow::Result<()> {
        self.agent_manager.finish_session_request(request_id);
        self.plugin_registry
            .resolve_request(
                runtime,
                RequestId::from_raw(request_id),
                agent_sessions::session_result_payload(result),
            )
            .await?;
        Ok(())
    }

    async fn agent_session_preview_reply(
        &mut self,
        runtime: &mut Runtime,
        request_id: i64,
        result: &Result<crate::agent_backend::types::SessionPreview, String>,
    ) -> anyhow::Result<()> {
        self.agent_manager.finish_session_preview(request_id);
        self.plugin_registry
            .resolve_request(
                runtime,
                RequestId::from_raw(request_id),
                agent_sessions::session_preview_payload(result),
            )
            .await?;
        Ok(())
    }

    async fn agent_commit_message_reply(
        &mut self,
        runtime: &mut Runtime,
        request_id: i64,
        result: &Result<String, String>,
    ) -> anyhow::Result<()> {
        self.agent_manager.finish_commit_message(request_id);
        let payload = match result {
            Ok(message) => json!({ "message": message, "error": "" }),
            Err(error) => json!({ "message": "", "error": error }),
        };
        self.plugin_registry
            .resolve_request(runtime, RequestId::from_raw(request_id), payload)
            .await?;
        Ok(())
    }

    fn agent_inline_context_read(&mut self, request_id: String, description: String) {
        if let Some(turn) = self.inline_history.turn_mut(&request_id) {
            if turn.context_reads.len() < 12 && !turn.context_reads.contains(&description) {
                turn.context_reads.push(description);
            }
        }
        self.mark_inline_history_dirty();
    }

    fn agent_inline_answer_delta(&mut self, request_id: String, text: String) {
        self.inline_history.append_answer(&request_id, &text);
        self.mark_inline_history_dirty();
    }

    fn agent_inline_session_created(&mut self, request_id: String, session_id: String) {
        let pending = self
            .inline_history
            .turn(&request_id)
            .is_some_and(|turn| turn.state == InlineTurnState::Pending);
        if let Some(assist) = self
            .inline_request_session_mut(&request_id)
            .filter(|_| pending)
        {
            assist.session_id = Some(session_id.clone());
            if let Some(turn) = self.inline_history.turn_mut(&request_id) {
                turn.session_id = Some(session_id);
            }
        } else if let Some(bridge) = self.agent_manager.bridge() {
            let _ = bridge.try_send(CodexCommand::CloseSession { session_id });
        }
    }

    async fn agent_inline_result(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
        request_id: String,
        session_id: String,
        result: InlineAssistResult,
    ) -> anyhow::Result<()> {
        // A duplicate or late provider event must never apply a
        // result after its request has already finished.
        if !self
            .inline_history
            .turn(&request_id)
            .is_some_and(|turn| turn.state == InlineTurnState::Pending)
        {
            return Ok(());
        }
        let foreground = self
            .inline_assist
            .as_ref()
            .is_some_and(|assist| assist.request_id.as_deref() == Some(&request_id))
            && self
                .current_dialog
                .as_ref()
                .and_then(|dialog| dialog.inline_assist_state())
                .is_some();
        let changes_text = self
            .inline_history
            .turn(&request_id)
            .is_some_and(|turn| result.changes_text(&turn.before));
        if foreground
            && changes_text
            && result.expanded_scope.is_none()
            && self.config.agent.auto_apply_inline_edits
            && self
                .apply_inline_result(&request_id, &session_id, &result, buffer, runtime)
                .await
                .is_ok()
        {
            return Ok(());
        }
        if result.expanded_scope.is_some() || changes_text {
            self.stage_background_inline_result(&request_id, &session_id, result);
            if foreground {
                if let Some(scope) = self
                    .inline_assist
                    .as_ref()
                    .map(|assist| assist.scope.clone())
                {
                    self.current_dialog = Some(Box::new(
                        self.inline_assist_popup(scope, self.inline_assist_result_state()),
                    ));
                }
            }
            self.render(buffer)?;
            return Ok(());
        }
        if !foreground {
            if self
                .inline_assist
                .as_ref()
                .is_some_and(|assist| assist.request_id.as_deref() == Some(&request_id))
            {
                self.park_inline_assist();
            }
            self.stage_background_inline_result(&request_id, &session_id, result);
            self.render(buffer)?;
            return Ok(());
        }
        if let Err(error) = self
            .apply_inline_result(&request_id, &session_id, &result, buffer, runtime)
            .await
        {
            if let Some(turn) = self.inline_history.turn_mut(&request_id).filter(|turn| {
                matches!(
                    turn.state,
                    InlineTurnState::Pending | InlineTurnState::Ready
                )
            }) {
                turn.result = Some(result);
                turn.session_id = Some(session_id);
            }
            self.inline_history.finish(
                &request_id,
                InlineTurnState::Rejected,
                Some(error.to_string()),
            );
            self.sync_inline_activity();
            if let Some(scope) = self.inline_assist.as_ref().and_then(|assist| {
                (assist.request_id.as_deref() == Some(request_id.as_str()))
                    .then(|| assist.scope.clone())
            }) {
                self.current_dialog = Some(Box::new(self.inline_assist_popup(
                    scope,
                    InlineAssistPopupState::Failed(error.to_string()),
                )));
                self.render(buffer)?;
            }
        }
        Ok(())
    }

    fn agent_inline_failed(
        &mut self,
        buffer: &mut RenderBuffer,
        request_id: Option<String>,
        session_id: Option<String>,
        message: String,
    ) -> anyhow::Result<()> {
        let request_id = request_id.or_else(|| {
            session_id
                .as_deref()
                .and_then(|provider| self.inline_request_for_provider(provider))
        });
        if !request_id
            .as_deref()
            .and_then(|request| self.inline_history.turn(request))
            .is_some_and(|turn| turn.state == InlineTurnState::Pending)
        {
            return Ok(());
        }
        let scope = self.inline_assist.as_ref().and_then(|assist| {
            (assist.request_id == request_id
                && self
                    .current_dialog
                    .as_ref()
                    .and_then(|dialog| dialog.inline_assist_state())
                    .is_some())
            .then(|| assist.scope.clone())
        });
        if let Some(request) = request_id {
            self.record_inline_failure(&request, &message);
        }
        if let Some(scope) = scope {
            self.current_dialog = Some(Box::new(
                self.inline_assist_popup(scope, InlineAssistPopupState::Failed(message)),
            ));
            self.render(buffer)?;
        } else {
            self.render(buffer)?;
        }
        Ok(())
    }

    async fn agent_session_restored(
        &mut self,
        runtime: &mut Runtime,
        session_id: &str,
        thread: &crate::agent_backend::types::RestoredHistory,
    ) -> anyhow::Result<()> {
        if self.agent_manager.take_forgotten_conversation(session_id) {
            if let Some(bridge) = self.agent_manager.bridge() {
                let _ = bridge.try_send(CodexCommand::CloseSession {
                    session_id: session_id.to_string(),
                });
            }
            return Ok(());
        }
        let root = self
            .agent_manager
            .root()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let conversation = self
            .agent_manager
            .reconcile_conversation(session_id, &root, thread)
            .cloned();
        if let Some(conversation) = conversation {
            self.restore_agent_annotations();
            self.notify_agent(
                runtime,
                "agent:session_restored",
                plugin_json(serde_json::to_value(conversation)?),
            )
            .await?;
        }
        Ok(())
    }

    async fn handle_agent_turn_event(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
        event: CodexEvent,
    ) -> anyhow::Result<()> {
        match &event {
            CodexEvent::Update { session_id, text }
                if self.agent_manager.is_session_active(session_id) =>
            {
                self.agent_manager.record_agent_delta(session_id, text);
            }
            CodexEvent::MessageCompleted { session_id, text }
                if self.agent_manager.is_session_active(session_id) =>
            {
                self.agent_manager.complete_agent_message(session_id, text);
            }
            _ => {}
        }
        match &event {
            CodexEvent::Completed { session_id, .. } => self.finish_inline_agent_outcome(
                session_id,
                crate::inline_history::InlineAgentState::Completed,
                None,
            ),
            CodexEvent::Cancelled { session_id } => self.finish_inline_agent_outcome(
                session_id,
                crate::inline_history::InlineAgentState::Cancelled,
                Some("Agent turn cancelled; any completed writes remain applied."),
            ),
            CodexEvent::Failed {
                session_id: Some(session_id),
                message,
            } => self.finish_inline_agent_outcome(
                session_id,
                crate::inline_history::InlineAgentState::Failed,
                Some(message),
            ),
            _ => {}
        }
        let ended_turn_id = match &event {
            CodexEvent::Completed { session_id, .. }
            | CodexEvent::Cancelled { session_id }
            | CodexEvent::Failed {
                session_id: Some(session_id),
                ..
            } => self.agent_manager.turn_id(session_id).map(str::to_string),
            _ => None,
        };
        let mut next_prompt = None;
        match &event {
            CodexEvent::Completed { session_id, .. }
            | CodexEvent::Failed {
                session_id: Some(session_id),
                ..
            } => {
                next_prompt = self
                    .agent_manager
                    .finish_turn(session_id)
                    .map(|prompt| (session_id.clone(), prompt));
            }
            CodexEvent::Cancelled { session_id } => {
                self.agent_manager.mark_session_inactive(session_id);
            }
            _ => {}
        }
        match &event {
            CodexEvent::Update { session_id, .. }
            | CodexEvent::MessageCompleted { session_id, .. }
            | CodexEvent::Activity { session_id, .. }
                if !self.agent_manager.is_session_active(session_id) =>
            {
                return Ok(());
            }
            CodexEvent::PermissionRequested {
                request_id,
                session_id,
                ..
            } if !self.agent_manager.is_session_active(session_id) => {
                if let Some(bridge) = self.agent_manager.bridge() {
                    let _ = bridge.try_send(CodexCommand::PermissionResponse {
                        request_id: request_id.clone(),
                        option_id: None,
                    });
                }
                return Ok(());
            }
            _ => {}
        }
        let turn_elapsed_ms = match &event {
            CodexEvent::Completed { session_id, .. } => self
                .agent_manager
                .elapsed_turn_duration(session_id)
                .map(|elapsed| elapsed.as_millis() as u64),
            CodexEvent::Failed {
                session_id: Some(session_id),
                ..
            } => {
                self.agent_manager.discard_turn(session_id);
                None
            }
            _ => None,
        };
        let (name, mut payload) = agent_event_payload(event);
        if let (Some(elapsed_ms), Some(object)) = (turn_elapsed_ms, payload.as_object_mut()) {
            object.insert("elapsed_ms".to_string(), json!(elapsed_ms));
        }
        if let (Some(turn_id), Some(object)) = (ended_turn_id, payload.as_object_mut()) {
            object.insert("turn_id".to_string(), json!(turn_id));
        }
        self.notify_agent(runtime, name, payload).await?;
        if let Some((session_id, prompt)) = next_prompt {
            if self
                .dispatch_agent_prompt(runtime, session_id, prompt.text, prompt.context)
                .await?
            {
                self.render(buffer)?;
            }
        }
        Ok(())
    }

    /// Reports a backend worker that exited while no events remain to drain.
    pub(super) async fn handle_agent_worker_loss(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        if self.agent_manager.is_task_finished()
            && self
                .agent_manager
                .bridge()
                .is_none_or(|bridge| !bridge.has_pending_events())
        {
            let pending_commit_messages = self.agent_manager.take_pending_commit_messages();
            let model_only_bridge = self.agent_manager.is_model_only_bridge();
            let inline_scope = self
                .current_dialog
                .as_ref()
                .and_then(|dialog| dialog.inline_assist_state())
                .and_then(|_| {
                    self.inline_assist
                        .as_ref()
                        .map(|assist| assist.scope.clone())
                });
            let message = self
                .finish_agent_bridge(runtime, "Codex app-server stopped unexpectedly")
                .await?;
            for request_id in pending_commit_messages {
                self.plugin_registry
                    .resolve_request(
                        runtime,
                        RequestId::from_raw(request_id),
                        json!({ "message": "", "error": message.clone() }),
                    )
                    .await?;
            }
            if let Some(scope) = inline_scope {
                if let Some(assist) = self.inline_assist.as_mut() {
                    assist.session_id = None;
                }
                self.current_dialog =
                    Some(Box::new(self.inline_assist_popup(
                        scope,
                        InlineAssistPopupState::Failed(message.clone()),
                    )));
                self.render(buffer)?;
            }
            if let Some(conversation) = self.agent_manager.conversation_snapshot() {
                self.notify_agent(
                    runtime,
                    "agent:conversation_restore_pending",
                    plugin_json(serde_json::to_value(conversation)?),
                )
                .await?;
                self.set_legacy_message(Some(format!(
                    "{message}; restoring the persisted agent session"
                )));
            } else if !model_only_bridge {
                self.notify_agent(runtime, "agent:session_lost", json!({ "message": message }))
                    .await?;
            }
        }
        Ok(())
    }
}
