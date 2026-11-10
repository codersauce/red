//! Editor handling of backend events: replies, inline events, session and turn events.

use super::agent_payloads::{session_event_payload, turn_event_payload};
use super::*;
use crate::agent_backend::{
    events::{
        Delivery, Event, Failure, FailureCause, InlineEvent, Lost, LostScope, QueryKind, ReplyBody,
        SessionEvent, TurnEnd, TurnEvent, TurnEventKind,
    },
    ids::{QueryId, SessionId},
};

impl Editor {
    /// Applies one backend event to the editor and forwards it to plugins.
    pub(super) async fn handle_agent_event(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
        event: Event,
    ) -> anyhow::Result<()> {
        match event {
            Event::Reply { id, kind, result } => self.agent_reply(runtime, id, kind, result).await,
            Event::Inline(event) => self.handle_agent_inline_event(buffer, runtime, event).await,
            Event::Session(event) => {
                self.handle_agent_session_event(buffer, runtime, event)
                    .await
            }
            Event::Turn(event) => self.handle_agent_turn_event(buffer, runtime, event).await,
            Event::Lost(lost) => self.handle_agent_loss(buffer, runtime, lost).await,
        }
    }

    /// Resolves the plugin callback a query was opened for.
    async fn agent_reply(
        &mut self,
        runtime: &mut Runtime,
        id: QueryId,
        kind: QueryKind,
        result: Result<ReplyBody, String>,
    ) -> anyhow::Result<()> {
        let payload = match result {
            Ok(ReplyBody::Model(model)) => agent_models::model_result_payload(&Ok(model)),
            Ok(ReplyBody::Sessions(page)) => agent_sessions::session_result_payload(&Ok(page)),
            Ok(ReplyBody::Preview(preview)) => {
                agent_sessions::session_preview_payload(&Ok(preview))
            }
            Ok(ReplyBody::CommitMessage(message)) => json!({ "message": message, "error": "" }),
            Err(error) => match kind {
                QueryKind::DefaultModel | QueryKind::ListModels | QueryKind::SetModel => {
                    agent_models::model_result_payload(&Err(error))
                }
                QueryKind::ListSessions => agent_sessions::session_result_payload(&Err(error)),
                QueryKind::PreviewSession => agent_sessions::session_preview_payload(&Err(error)),
                QueryKind::CommitMessage => json!({ "message": "", "error": error }),
            },
        };
        self.plugin_registry
            .resolve_request(runtime, RequestId::from_raw(id.0), payload)
            .await?;
        Ok(())
    }

    async fn handle_agent_inline_event(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
        event: InlineEvent,
    ) -> anyhow::Result<()> {
        match event {
            InlineEvent::ContextRead {
                request,
                description,
            } => self.agent_inline_context_read(request, description),
            InlineEvent::AnswerDelta { request, text } => {
                self.agent_inline_answer_delta(request, text);
            }
            InlineEvent::SessionCreated { request, session } => {
                self.agent_inline_session_created(request, session.to_string());
            }
            InlineEvent::Result {
                request,
                session,
                result,
            } => {
                return self
                    .agent_inline_result(buffer, runtime, request, session.to_string(), result)
                    .await;
            }
            InlineEvent::Failed {
                request,
                session,
                message,
            } => {
                return self.agent_inline_failed(
                    buffer,
                    request,
                    session.map(|session| session.to_string()),
                    message,
                );
            }
        }
        Ok(())
    }

    async fn handle_agent_session_event(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
        event: SessionEvent,
    ) -> anyhow::Result<()> {
        match &event {
            SessionEvent::ModelChanged { session, model } => {
                self.agent_manager
                    .set_conversation_model(session.as_str(), model.clone());
            }
            SessionEvent::Created { session } => {
                self.agent_manager.take_next_model();
                let root = self
                    .agent_manager
                    .root()
                    .map(Path::to_path_buf)
                    .unwrap_or_default();
                self.agent_manager
                    .begin_conversation(session.as_str(), &root);
            }
            SessionEvent::Restored { session, history } => {
                return self
                    .agent_session_restored(runtime, session.as_str(), history)
                    .await;
            }
            // An error for a session with no turn in flight ends that
            // session's turn bookkeeping the way a failed turn does.
            SessionEvent::Failed { session, message } => {
                let failed = TurnEvent {
                    session: session.clone(),
                    turn: TurnId::mint(),
                    kind: TurnEventKind::End(TurnEnd::Failed(Failure {
                        message: message.clone(),
                        cause: FailureCause::Agent,
                        delivery: Delivery::Unconfirmed,
                    })),
                };
                return self.handle_agent_turn_event(buffer, runtime, failed).await;
            }
            SessionEvent::CreateFailed { .. }
            | SessionEvent::RestoreFailed { .. }
            | SessionEvent::ModelRerouted { .. } => {}
        }
        if let Some((name, payload)) = session_event_payload(&event) {
            self.notify_agent(runtime, name, payload).await?;
        }
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
        } else {
            self.agent_manager.close_session(session_id);
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
            self.agent_manager.close_session(session_id);
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
        event: TurnEvent,
    ) -> anyhow::Result<()> {
        let session_id = event.session.as_str();
        match &event.kind {
            // The pane does not read acceptance yet.
            TurnEventKind::Accepted => return Ok(()),
            // The loss that ended this turn reports it; the turn's own failure
            // would only repeat that to the plugin and dispatch queued prompts
            // into an agent that is gone.
            TurnEventKind::End(TurnEnd::Failed(Failure {
                cause: FailureCause::BackendLost,
                ..
            })) => return Ok(()),
            TurnEventKind::Delta(text) if self.agent_manager.is_session_active(session_id) => {
                self.agent_manager.record_agent_delta(session_id, text);
            }
            TurnEventKind::MessageDone(text)
                if self.agent_manager.is_session_active(session_id) =>
            {
                self.agent_manager.complete_agent_message(session_id, text);
            }
            _ => {}
        }
        match &event.kind {
            TurnEventKind::End(TurnEnd::Completed(_) | TurnEnd::Stopped) => self
                .finish_inline_agent_outcome(
                    session_id,
                    crate::inline_history::InlineAgentState::Completed,
                    None,
                ),
            TurnEventKind::StopRequested => self.finish_inline_agent_outcome(
                session_id,
                crate::inline_history::InlineAgentState::Cancelled,
                Some("Agent turn cancelled; any completed writes remain applied."),
            ),
            TurnEventKind::End(TurnEnd::Failed(failure)) => self.finish_inline_agent_outcome(
                session_id,
                crate::inline_history::InlineAgentState::Failed,
                Some(&failure.message),
            ),
            _ => {}
        }
        let ended_turn_id = match &event.kind {
            TurnEventKind::End(_) | TurnEventKind::StopRequested => {
                self.agent_manager.turn_id(session_id).map(str::to_string)
            }
            _ => None,
        };
        let mut next_prompt = None;
        match &event.kind {
            TurnEventKind::End(_) => {
                next_prompt = self
                    .agent_manager
                    .finish_turn(session_id)
                    .map(|prompt| (session_id.to_string(), prompt));
            }
            TurnEventKind::StopRequested => {
                self.agent_manager.mark_session_inactive(session_id);
            }
            _ => {}
        }
        if matches!(
            event.kind,
            TurnEventKind::Delta(_) | TurnEventKind::MessageDone(_) | TurnEventKind::Activity(_)
        ) && !self.agent_manager.is_session_active(session_id)
        {
            return Ok(());
        }
        let turn_elapsed_ms = match &event.kind {
            TurnEventKind::End(TurnEnd::Completed(_) | TurnEnd::Stopped) => self
                .agent_manager
                .elapsed_turn_duration(session_id)
                .map(|elapsed| elapsed.as_millis() as u64),
            TurnEventKind::End(TurnEnd::Failed(_)) => {
                self.agent_manager.discard_turn(session_id);
                None
            }
            _ => None,
        };
        let Some((name, mut payload)) = turn_event_payload(&event) else {
            return Ok(());
        };
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

    /// The agent, or one of its sessions, ended without the editor asking. The
    /// backend already closed everything that was open in that scope.
    async fn handle_agent_loss(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
        lost: Lost,
    ) -> anyhow::Result<()> {
        match lost.scope {
            LostScope::Backend => {
                self.handle_agent_backend_loss(buffer, runtime, lost.message, lost.had_sessions)
                    .await
            }
            LostScope::Session(session) => {
                self.handle_agent_session_loss(runtime, &session, lost.message)
                    .await
            }
        }
    }

    async fn handle_agent_session_loss(
        &mut self,
        runtime: &mut Runtime,
        session: &SessionId,
        message: String,
    ) -> anyhow::Result<()> {
        let session_id = session.as_str();
        self.finish_inline_agent_outcome(
            session_id,
            crate::inline_history::InlineAgentState::Failed,
            Some(&message),
        );
        self.abandon_agent_edit_decision(Some(session_id), "the agent session stopped");
        let prompts = self.agent_manager.end_session_turns(session_id);
        if !prompts.is_empty() {
            self.notify_agent(
                runtime,
                "agent:prompts_returned",
                json!({ "session_id": session_id, "prompts": prompts }),
            )
            .await?;
        }
        self.notify_agent(
            runtime,
            "agent:session_lost",
            json!({ "session_id": session_id, "message": message }),
        )
        .await
    }

    async fn handle_agent_backend_loss(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
        message: String,
        had_sessions: bool,
    ) -> anyhow::Result<()> {
        let context = self.agent_manager.take_loss_context();
        let inline_scope = self
            .current_dialog
            .as_ref()
            .and_then(|dialog| dialog.inline_assist_state())
            .and_then(|_| {
                self.inline_assist
                    .as_ref()
                    .map(|assist| assist.scope.clone())
            });
        let name = self.agent_manager.agents().kind().name();
        self.stop_inline_agent_outcomes(&format!("{name} app-server stopped unexpectedly"));
        self.abandon_agent_edit_decision(None, &format!("the {name} app-server stopped"));
        self.agent_manager.clear_active_sessions();
        // Prompts held behind a stopping turn were never sent; return them to the
        // pane instead of letting a dead agent swallow them.
        for (session_id, prompts) in self.agent_manager.take_all_queued_prompts() {
            self.notify_agent(
                runtime,
                "agent:prompts_returned",
                json!({ "session_id": session_id, "prompts": prompts }),
            )
            .await?;
        }
        self.agent_manager.clear_turns();
        self.agent_manager.clear_tool_requests();
        self.agent_manager.set_root(None);
        self.fail_running_inline_jobs(&message);
        match context {
            Some(agent_manager::AgentLossContext::Prompt { session_id, prompt }) => {
                self.notify_agent(
                    runtime,
                    "agent:session_lost",
                    json!({
                        "session_id": session_id,
                        "prompt": prompt,
                        "message": message
                    }),
                )
                .await?;
            }
            Some(agent_manager::AgentLossContext::NewSession) => {
                self.notify_agent(runtime, "agent:session_lost", json!({ "message": message }))
                    .await?;
            }
            Some(agent_manager::AgentLossContext::Restore { session_id }) => {
                self.notify_agent(
                    runtime,
                    "agent:session_restore_failed",
                    json!({ "session_id": session_id, "message": message }),
                )
                .await?;
            }
            None => {
                if let Some(scope) = inline_scope {
                    if let Some(assist) = self.inline_assist.as_mut() {
                        assist.session_id = None;
                    }
                    self.current_dialog = Some(Box::new(self.inline_assist_popup(
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
                } else if had_sessions {
                    self.notify_agent(runtime, "agent:session_lost", json!({ "message": message }))
                        .await?;
                }
            }
        }
        Ok(())
    }
}
