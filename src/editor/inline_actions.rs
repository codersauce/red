//! Inline-assist dispatch kept out of the recursive editing action frame.

use super::*;

impl Editor {
    /// Keep the common post-action bookkeeping in the caller. `Break` preserves
    /// an action's early return; `Continue` runs that bookkeeping normally.
    pub(super) async fn execute_inline_action(
        &mut self,
        action: &Action,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<std::ops::ControlFlow<bool>> {
        match action {
            Action::AddSampleInlineComment => {
                if matches!(self.mode, Mode::Normal | Mode::Visual | Mode::VisualLine) {
                    let (start_line, end_line) = self.inline_comment_target_lines();
                    let was_visual = self.is_visual();
                    self.add_sample_inline_comment();
                    if was_visual {
                        self.execute(&Action::EnterMode(Mode::Normal), buffer, runtime)
                            .await?;
                        self.move_to_text_position(TextPosition::new(start_line, 0));
                        self.refresh_cursor_goal();
                    }
                    let scope = if start_line == end_line {
                        format!("line {}", start_line + 1)
                    } else {
                        format!("lines {}–{}", start_line + 1, end_line + 1)
                    };
                    self.set_legacy_message(Some(format!(
                        "sample comment · {scope} · Space C replace · Space X clear"
                    )));
                    self.render(buffer)?;
                }
            }
            Action::ClearInlineComments => {
                self.clear_inline_comments();
                self.render(buffer)?;
            }
            Action::ShowInlineComment => {
                if let Some(id) = self.current_inline_comment_id() {
                    self.open_inline_comment_by_id(id, buffer, runtime).await?;
                } else {
                    self.show_inline_comment();
                }
                self.render(buffer)?;
            }
            Action::DismissInlineComment => {
                self.dismiss_inline_comment();
                self.render(buffer)?;
            }
            Action::NextInlineComment | Action::PreviousInlineComment => {
                self.navigate_inline_comment(matches!(action, Action::PreviousInlineComment));
                self.render(buffer)?;
            }
            Action::NextOverlappingInlineComment | Action::PreviousOverlappingInlineComment => {
                if let Some(id) = self.current_inline_comment_id() {
                    self.cycle_overlapping_inline_comment(
                        id,
                        matches!(action, Action::PreviousOverlappingInlineComment),
                    );
                } else {
                    self.set_legacy_message(Some("no inline item at the cursor".into()));
                }
                self.render(buffer)?;
            }
            Action::OpenInlineComment(id) => {
                self.open_inline_comment_by_id(*id, buffer, runtime).await?;
            }
            Action::FocusInlineComment(id) => {
                self.focus_inline_comment(*id, buffer, runtime).await?;
            }
            Action::ChooseInlineComment(id) => {
                self.choose_inline_comment(*id);
                self.render(buffer)?;
            }
            Action::NavigateInlineCommentCard { id, backwards } => {
                if let Some(next) = self.cycle_overlapping_inline_comment(*id, *backwards) {
                    self.focus_inline_comment(next, buffer, runtime).await?;
                }
            }
            Action::RefineInlineComment(id) => {
                self.refine_inline_comment(*id, buffer, runtime).await?;
            }
            Action::ResolveInlineComment(id) => {
                self.resolve_inline_comment(*id);
                self.render(buffer)?;
            }
            Action::DismissInlineCommentById(id) => {
                if self.select_inline_comment_by_id(*id) {
                    self.dismiss_inline_comment();
                    self.current_dialog = None;
                }
                self.render(buffer)?;
            }
            Action::NavigateOverlappingInlineComment {
                id,
                backwards,
                open,
            } => {
                if *open {
                    self.park_inline_assist();
                }
                if let Some(selected) = self.cycle_overlapping_inline_comment(*id, *backwards) {
                    if *open {
                        self.open_inline_comment_by_id(selected, buffer, runtime)
                            .await?;
                    } else {
                        self.render(buffer)?;
                    }
                }
            }
            Action::AskInlineComment { id, in_agent } => {
                self.ask_inline_comment(*id, *in_agent, buffer, runtime)
                    .await?;
            }
            Action::InlineAssist => match self.inline_assist_target() {
                Ok((range, scope)) => {
                    let Some(window_id) = self.window_manager.active_stable_window_id() else {
                        self.set_legacy_message(Some(
                            "inline assist requires an active editor window".to_string(),
                        ));
                        self.draw_commandline(buffer);
                        return Ok(std::ops::ControlFlow::Break(false));
                    };
                    let expected_text = self.current_buffer().text_in_range(range);
                    self.park_inline_assist();
                    self.inline_assist = Some(InlineAssistSession {
                        parent_comment: None,
                        allow_expansion: !matches!(
                            self.mode,
                            Mode::Visual | Mode::VisualLine | Mode::VisualBlock
                        ),
                        buffer_id: self.current_buffer().id(),
                        window_id,
                        expected_revision: self.current_buffer().revision(),
                        range,
                        expected_text,
                        scope: scope.clone(),
                        request_id: None,
                        session_id: None,
                        transaction_id: None,
                        annotation_group_id: uuid::Uuid::new_v4().to_string(),
                        has_result: false,
                        result_request_id: None,
                    });
                    self.current_dialog = Some(Box::new(self.inline_assist_popup(
                        scope,
                        InlineAssistPopupState::Prompt {
                            initial: String::new(),
                            refining: false,
                        },
                    )));
                    self.render(buffer)?;
                }
                Err(error) => {
                    self.set_legacy_message(Some(error.to_string()));
                    self.draw_commandline(buffer);
                }
            },
            Action::SubmitInlineAssist(prompt) => {
                let range = match self.inline_submission_target() {
                    Ok(range) => range,
                    Err(error) => {
                        self.set_legacy_message(Some(error.to_string()));
                        self.render(buffer)?;
                        return Ok(std::ops::ControlFlow::Break(false));
                    }
                };
                let Some((scope, existing_session, retired_session)) =
                    self.inline_assist.as_ref().map(|assist| {
                        let reuse = range == assist.range && self.inline_target_matches(assist);
                        (
                            assist.scope.clone(),
                            reuse.then(|| assist.session_id.clone()).flatten(),
                            (!reuse).then(|| assist.session_id.clone()).flatten(),
                        )
                    })
                else {
                    self.set_legacy_message(Some("inline assist is no longer active".to_string()));
                    return Ok(std::ops::ControlFlow::Break(false));
                };
                let mut context = match self.inline_assist_context(range) {
                    Ok(context) => context,
                    Err(error) => {
                        self.set_legacy_message(Some(error.to_string()));
                        return Ok(std::ops::ControlFlow::Break(false));
                    }
                };
                if existing_session.is_none() {
                    if let Some(assist) = &self.inline_assist {
                        context
                            .push_str(&self.recovered_inline_context(&assist.annotation_group_id));
                    }
                }
                let request_id = uuid::Uuid::new_v4().to_string();
                if let Err(error) = self.begin_inline_history_turn(&request_id, prompt, range) {
                    self.set_legacy_message(Some(error.to_string()));
                    self.render(buffer)?;
                    return Ok(std::ops::ControlFlow::Break(false));
                }
                if let Some(session_id) = retired_session {
                    if let Some(bridge) = self.agent_manager.bridge() {
                        let _ = bridge.try_send(CodexCommand::CloseSession { session_id });
                    }
                }
                let revision = self.current_buffer().revision();
                let expected_text = self.current_buffer().text_in_range(range);
                let buffer_id = self.current_buffer().id();
                if let Some(assist) = self.inline_assist.as_mut() {
                    assist.buffer_id = buffer_id;
                    assist.range = range;
                    assist.expected_revision = revision;
                    assist.expected_text = expected_text;
                    assist.request_id = Some(request_id.clone());
                    assist.session_id.clone_from(&existing_session);
                }
                let cwd = get_workspace_path();
                if let Err(error) = self.ensure_agent_bridge(&cwd) {
                    self.inline_history.finish(
                        &request_id,
                        InlineTurnState::Failed,
                        Some(error.to_string()),
                    );
                    self.current_dialog = Some(Box::new(self.inline_assist_popup(
                        scope,
                        InlineAssistPopupState::Failed(error.to_string()),
                    )));
                    self.sync_inline_activity();
                    self.render(buffer)?;
                    return Ok(std::ops::ControlFlow::Break(false));
                }
                let command = existing_session.map_or_else(
                    || CodexCommand::InlineAssist {
                        request_id: request_id.clone(),
                        cwd,
                        prompt: prompt.clone(),
                        context: context.clone(),
                    },
                    |session_id| CodexCommand::InlineAssistFollowup {
                        request_id: request_id.clone(),
                        session_id,
                        prompt: prompt.clone(),
                        context: context.clone(),
                    },
                );
                let send_result = if let Some(bridge) = self.agent_manager.bridge() {
                    bridge.send(command).await
                } else {
                    Err(anyhow::anyhow!("Codex bridge is unavailable"))
                };
                if let Err(error) = send_result {
                    self.inline_history.finish(
                        &request_id,
                        InlineTurnState::Failed,
                        Some(error.to_string()),
                    );
                    if let Some(assist) = self.inline_assist.as_mut() {
                        assist.session_id = None;
                    }
                    self.current_dialog = Some(Box::new(self.inline_assist_popup(
                        scope,
                        InlineAssistPopupState::Failed(error.to_string()),
                    )));
                } else {
                    self.current_dialog = Some(Box::new(
                        self.inline_assist_popup(scope, InlineAssistPopupState::Working),
                    ));
                }
                self.sync_inline_activity();
                self.render(buffer)?;
            }
            Action::HideInlineAssist => {
                let close = self
                    .current_dialog
                    .as_mut()
                    .and_then(|dialog| dialog.request_inline_assist_close());
                if let Some(close) = close {
                    self.execute_with_tracking(&close, buffer, runtime, false)
                        .await?;
                } else {
                    self.park_inline_assist();
                    self.render(buffer)?;
                }
            }
            Action::SaveInlineAssistDraft => {
                if self.current_dialog.as_ref().is_some_and(|dialog| {
                    matches!(
                        dialog.inline_assist_state(),
                        Some(InlineAssistPopupState::Prompt { .. })
                    )
                }) {
                    self.park_inline_assist();
                }
                self.render(buffer)?;
            }
            Action::DiscardInlineAssistDraft => {
                if self.current_dialog.as_ref().is_some_and(|dialog| {
                    matches!(
                        dialog.inline_assist_state(),
                        Some(InlineAssistPopupState::Prompt { .. })
                    )
                }) {
                    self.close_inline_assist_session();
                }
                self.render(buffer)?;
            }
            Action::OpenInlineJob(group) => {
                self.open_inline_job(group, buffer, runtime).await?;
            }
            Action::OpenLatestInlineCompletion => {
                self.open_latest_inline_completion(buffer, runtime).await?;
            }
            Action::OpenInlineCompletion(request) => {
                self.open_inline_completion(request, buffer, runtime)
                    .await?;
            }
            Action::ApplyPendingInlineAssist | Action::ApplyReviewedInlineAssist(_) => {
                let pending = self
                    .inline_assist
                    .as_ref()
                    .and_then(|assist| assist.request_id.as_deref())
                    .and_then(|request| self.inline_history.turn(request))
                    .filter(|turn| turn.state == InlineTurnState::Ready)
                    .and_then(|turn| {
                        Some((
                            turn.request_id.clone(),
                            turn.session_id.clone()?,
                            turn.result.clone()?,
                        ))
                    });
                if let Some((request, session, result)) = pending {
                    if let Action::ApplyReviewedInlineAssist(reviewed) = action {
                        if reviewed != &request {
                            self.set_legacy_message(Some(
                                "inline edit review is no longer current".into(),
                            ));
                            self.render(buffer)?;
                            return Ok(std::ops::ControlFlow::Break(false));
                        }
                    } else if result.expanded_scope.is_some() {
                        self.execute_with_tracking(
                            &Action::ViewInlineAssistAnswer,
                            buffer,
                            runtime,
                            false,
                        )
                        .await?;
                        return Ok(std::ops::ControlFlow::Break(false));
                    }
                    let preparation = if result.expanded_scope.is_some() {
                        self.prepare_reviewed_inline_expansion(&request)
                    } else {
                        Ok(())
                    };
                    let applied = match preparation {
                        Ok(()) => {
                            self.apply_inline_result(&request, &session, &result, buffer, runtime)
                                .await
                        }
                        Err(error) => Err(error),
                    };
                    if let Err(error) = applied {
                        self.inline_history.finish(
                            &request,
                            InlineTurnState::Rejected,
                            Some(error.to_string()),
                        );
                        self.sync_inline_activity();
                        let scope = self
                            .inline_assist
                            .as_ref()
                            .map_or("selection", |assist| assist.scope.as_str())
                            .to_string();
                        self.current_dialog = Some(Box::new(self.inline_assist_popup(
                            scope,
                            InlineAssistPopupState::Failed(error.to_string()),
                        )));
                        self.render(buffer)?;
                    }
                }
            }
            Action::ViewInlineChanges { request_id, hunk } => {
                self.view_inline_changes(request_id, *hunk, buffer, runtime)
                    .await?;
            }
            Action::ViewInlineAgentChanges {
                request_id,
                outcome,
                change,
            } => {
                self.view_inline_agent_changes(request_id, *outcome, *change, buffer, runtime)
                    .await?;
            }
            Action::UndoInlineChange(request) => {
                self.undo_inline_change(request, buffer, runtime).await?;
            }
            Action::RejectPendingInlineAssist => {
                if let Some(request) = self
                    .inline_assist
                    .as_ref()
                    .and_then(|assist| assist.request_id.clone())
                    .filter(|request| {
                        self.inline_history.turn(request).is_some_and(|turn| {
                            turn.state == InlineTurnState::Ready
                                && turn
                                    .result
                                    .as_ref()
                                    .is_some_and(|result| result.changes_text(&turn.before))
                        })
                    })
                {
                    self.inline_history.finish(
                        &request,
                        InlineTurnState::Declined,
                        Some("Inline edit declined; source unchanged.".into()),
                    );
                    self.close_inline_assist_session();
                    self.notify_inline_outcome(&request);
                    self.set_legacy_message(Some(
                        "inline edit declined · source unchanged · retained in InlineHistory"
                            .into(),
                    ));
                    self.render(buffer)?;
                }
            }
            Action::CancelInlineAssist => {
                self.close_inline_assist_session();
                self.render(buffer)?;
            }
            Action::CancelInlineAssistRefine => {
                if let Some(scope) = self
                    .inline_assist
                    .as_ref()
                    .map(|assist| assist.scope.clone())
                {
                    self.current_dialog = Some(Box::new(
                        self.inline_assist_popup(scope, self.inline_assist_result_state()),
                    ));
                } else {
                    self.current_dialog = None;
                }
                self.render(buffer)?;
            }
            Action::KeepInlineAssist => {
                let result = self.inline_assist_result_state();
                let request = self
                    .inline_assist
                    .as_ref()
                    .and_then(|assist| assist.result_request_id.clone());
                self.close_inline_assist_session();
                if matches!(result, InlineAssistPopupState::Applied { edited: true, .. }) {
                    self.set_legacy_message(None);
                    if let Some(request) = request {
                        self.notify_inline_outcome(&request);
                    }
                } else {
                    self.set_legacy_message(Some(match result {
                        InlineAssistPopupState::Applied { comments, .. } => format!(
                            "kept {comments} inline comment(s) · Space v view · Space x dismiss"
                        ),
                        InlineAssistPopupState::NeedsAgent(_) => {
                            "discussion kept · Space H to continue from history".to_string()
                        }
                        _ => "inline assist closed".to_string(),
                    }));
                }
                self.render(buffer)?;
            }
            Action::UndoInlineAssist => {
                let transaction_id = self
                    .inline_assist
                    .as_ref()
                    .and_then(|assist| assist.transaction_id.clone());
                let is_latest = transaction_id.as_deref().is_some_and(|transaction_id| {
                    self.current_buffer()
                        .undo_history
                        .latest_transaction()
                        .is_some_and(|latest| latest.id == transaction_id)
                });
                if let Some(request) = self
                    .inline_assist
                    .as_ref()
                    .and_then(|assist| assist.result_request_id.as_deref())
                {
                    if let Some(turn) = self.inline_history.turn_mut(request) {
                        if is_latest || transaction_id.is_none() {
                            turn.disposition = InlineDisposition::Undone;
                        }
                        if let Some(result) = &turn.result {
                            turn.hidden_comments = (0..result.comments.len()).collect();
                        }
                    }
                }
                if is_latest {
                    for turn in self
                        .inline_history
                        .conversations
                        .iter_mut()
                        .flat_map(|conversation| &mut conversation.turns)
                    {
                        if turn.transaction_id.as_deref() == transaction_id.as_deref() {
                            turn.disposition = InlineDisposition::Undone;
                        }
                    }
                }
                if let Some(group_id) = self
                    .inline_assist
                    .as_ref()
                    .map(|assist| assist.annotation_group_id.clone())
                {
                    self.dismiss_inline_comment_group(&group_id);
                }
                self.close_inline_assist_session();
                if is_latest {
                    self.undo_transaction(buffer, runtime).await?;
                } else if transaction_id.is_some() {
                    self.set_legacy_message(Some(
                        "inline edit is no longer the latest change; use transaction history to revert it"
                            .to_string(),
                    ));
                    self.render(buffer)?;
                } else {
                    self.set_legacy_message(Some("dismissed inline comments".to_string()));
                    self.render(buffer)?;
                }
            }
            Action::RefineInlineAssist => {
                if let Err(error) = self.inline_submission_target() {
                    self.set_legacy_message(Some(error.to_string()));
                    self.render(buffer)?;
                    return Ok(std::ops::ControlFlow::Break(false));
                }
                let initial = self
                    .inline_assist
                    .as_ref()
                    .and_then(|assist| assist.request_id.as_deref())
                    .and_then(|request| self.inline_history.turn(request))
                    .filter(|turn| turn.state == InlineTurnState::Ready)
                    .map_or_else(String::new, |turn| {
                        format!(
                            "Recheck this request against the current code: {}",
                            turn.prompt
                        )
                    });
                if let Some((scope, refining)) = self
                    .inline_assist
                    .as_ref()
                    .map(|assist| (assist.scope.clone(), assist.has_result))
                {
                    self.current_dialog = Some(Box::new(self.inline_assist_popup(
                        scope,
                        InlineAssistPopupState::Prompt { initial, refining },
                    )));
                    self.render(buffer)?;
                }
            }
            Action::ViewInlineAssistAnswer => {
                if let Some(request) = self
                    .inline_assist
                    .as_ref()
                    .and_then(|assist| assist.request_id.as_deref())
                    .and_then(|request| self.inline_history.turn(request))
                    .filter(|turn| turn.has_code_change())
                    .map(|turn| turn.request_id.clone())
                {
                    self.view_inline_changes(&request, 0, buffer, runtime)
                        .await?;
                    return Ok(std::ops::ControlFlow::Break(false));
                }
                if let Some(turn) = self
                    .inline_assist
                    .as_ref()
                    .and_then(|assist| {
                        assist
                            .request_id
                            .as_deref()
                            .or(assist.result_request_id.as_deref())
                    })
                    .and_then(|request| self.inline_history.turn(request))
                {
                    let expanded_range = self
                        .inline_assist
                        .as_ref()
                        .and_then(|session| self.pending_inline_expansion_range(session).ok());
                    let proposed = turn.proposed_edit();
                    let description = if proposed.is_some() {
                        turn.proposal_description()
                    } else {
                        turn.answer_text()
                    };
                    let answer = expanded_range.map_or_else(
                        || description.clone(),
                        |range| {
                            format!(
                                "Current proposed range: {}:{}–{}\n\n{}",
                                turn.location.file,
                                range.start.line + 1,
                                range.end.line + usize::from(range.end.character > 0),
                                description
                            )
                        },
                    );
                    let mut hover =
                        HoverInfo::new(self, answer, HoverInfoFormat::Markdown, Vec::new())
                            .with_label("Inline answer")
                            .with_close_action(Action::CancelInlineAssistRefine);
                    if let Some((before, after)) = proposed {
                        hover = hover.with_diff(&turn.location.file, before, after, "proposed");
                    }
                    if turn.state == InlineTurnState::Ready
                        && turn
                            .result
                            .as_ref()
                            .is_some_and(|result| result.expanded_scope.is_some())
                    {
                        hover = hover.with_label("Review wider edit · not applied");
                        if expanded_range.is_some() {
                            hover = hover.with_shortcut(
                                'a',
                                "apply wider edit",
                                Action::ApplyReviewedInlineAssist(turn.request_id.clone()),
                            );
                        }
                        hover =
                            hover.with_shortcut('d', "decline", Action::RejectPendingInlineAssist);
                    } else if turn.state == InlineTurnState::Ready
                        && turn
                            .result
                            .as_ref()
                            .is_some_and(|result| result.changes_text(&turn.before))
                    {
                        hover = hover
                            .with_label("Review inline edit · not applied")
                            .with_shortcut('d', "decline", Action::RejectPendingInlineAssist);
                        if self
                            .inline_assist
                            .as_ref()
                            .is_some_and(|session| self.inline_target_matches(session))
                        {
                            hover = hover.with_shortcut(
                                'a',
                                "apply edit",
                                Action::ApplyReviewedInlineAssist(turn.request_id.clone()),
                            );
                        }
                    } else if let Some((id, ordinal, count)) = self.current_inline_navigation() {
                        hover = hover
                            .with_label(format!("Inline answer · inline {ordinal} of {count}"))
                            .with_inline_navigation(id);
                    }
                    self.current_dialog = Some(Box::new(hover));
                }
                self.render(buffer)?;
            }
            Action::EscalateInlineAssist => {
                let Some(assist) = self.inline_assist.as_ref() else {
                    return Ok(std::ops::ControlFlow::Break(false));
                };
                let Some(prompt) = self.inline_handoff_prompt(&assist.annotation_group_id) else {
                    self.set_legacy_message(Some(
                        "inline discussion is no longer available".into(),
                    ));
                    return Ok(std::ops::ControlFlow::Break(false));
                };
                let request_id = assist.request_id.clone();
                self.plugin_registry
                    .ensure_command_registered(runtime, "AgentOpen")
                    .await;
                if runtime.command_plugin("AgentOpen").is_none() {
                    self.set_legacy_message(Some(
                        "Agent is unavailable; the inline discussion remains in history".into(),
                    ));
                    self.render(buffer)?;
                    return Ok(std::ops::ControlFlow::Break(false));
                }
                self.close_inline_assist_session();
                // The handoff carries its own location and source context. Do not
                // leave an artificial visual selection behind in the editor.
                if !self.is_normal() {
                    self.execute_with_tracking(
                        &Action::EnterMode(Mode::Normal),
                        buffer,
                        runtime,
                        false,
                    )
                    .await?;
                }
                self.plugin_registry.execute(runtime, "AgentOpen").await?;
                // AgentOpen queues pane creation/restoration first. Stage the draft
                // only after those requests have been processed.
                runtime.send_request(PluginRequest::Action(Action::StageInlineAssistHandoff {
                    request_id,
                    comment_followup: None,
                    prompt,
                    expected_draft: None,
                }));
            }
            Action::StageInlineAssistHandoff {
                request_id,
                comment_followup,
                prompt,
                expected_draft,
            } => {
                // Session restoration can hide a newly recreated pane, and an
                // editor zoom can obscure an otherwise visible one. An explicit
                // handoff must reveal the actual pane before loading its draft.
                if self
                    .panel_manager
                    .set_panel_visible("agent-conversation", true)
                {
                    self.clear_pane_zoom();
                    self.apply_panel_layout();
                }
                match self.panel_manager.load_text_panel_draft(
                    "agent-conversation",
                    prompt,
                    *expected_draft,
                ) {
                    Ok(plugin::panel::TextPanelReuseOutcome::Loaded) => {
                        self.staged_inline_agent_handoff = request_id
                            .as_ref()
                            .filter(|request| {
                                comment_followup.is_some()
                                    || self.inline_history.turn(request).is_some()
                            })
                            .map(|request| inline_agent_outcomes::StagedHandoff {
                                request_id: request.clone(),
                                comment_followup: comment_followup.clone(),
                            });
                        self.set_legacy_message(Some(
                            "inline discussion loaded in Agent; review and send when ready".into(),
                        ));
                    }
                    Ok(plugin::panel::TextPanelReuseOutcome::Confirm(revision)) => {
                        self.current_dialog = Some(Box::new(Confirmation::new_actions(
                            self,
                            "Replace unsent Agent draft?",
                            "The inline discussion will replace the draft as one undoable edit. Nothing will be sent.",
                            "Load discussion", "Keep draft",
                            Action::StageInlineAssistHandoff { request_id: request_id.clone(), comment_followup: comment_followup.clone(), prompt: prompt.clone(), expected_draft: Some(revision) },
                            Action::Print("current draft kept; inline discussion remains in history".into()),
                        )));
                    }
                    Err(message) => self.set_legacy_message(Some(message.to_string())),
                }
                self.render(buffer)?;
            }
            _ => unreachable!("non-inline action routed to inline dispatcher"),
        }
        Ok(std::ops::ControlFlow::Continue(()))
    }
}
