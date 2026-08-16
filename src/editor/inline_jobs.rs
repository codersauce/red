//! Recoverable inline work, independent of the one foreground popup.

use super::inline_comments::{InlineComment, InlineCommentOrigin};
use super::*;
use crate::inline_history::{InlineLocation, InlineSourceState};
use crate::ui::{spinner_frame, SPINNER_FRAME_INTERVAL_MS};
use crate::unicode_utils::truncate_display_width;

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub(super) struct ParkedInlineAssist {
    pub(super) session: InlineAssistSession,
    pub(super) state: InlineAssistPopupState,
    pub(super) location: InlineLocation,
}

/// Presentation-only animation state; never part of retained inline history.
#[derive(Debug)]
pub(super) struct InlineActivityAnimation {
    since: Instant,
    frame: u64,
    running: HashMap<String, String>,
}

impl Default for InlineActivityAnimation {
    fn default() -> Self {
        Self {
            since: Instant::now(),
            frame: 0,
            running: HashMap::new(),
        }
    }
}

impl InlineActivityAnimation {
    fn message(&self, prompt: &str) -> String {
        format!(
            "{} Working · {prompt} · Space H",
            spinner_frame(self.frame.saturating_mul(SPINNER_FRAME_INTERVAL_MS))
        )
    }
}

impl Editor {
    fn inline_session_location(&self, session: &InlineAssistSession) -> Option<InlineLocation> {
        if let Some(turn) = session
            .request_id
            .as_deref()
            .and_then(|id| self.inline_history.turn(id))
        {
            return Some(turn.location.clone());
        }
        let buffer = self
            .buffer_manager
            .iter()
            .find(|buffer| buffer.id() == session.buffer_id)?;
        Some(InlineLocation {
            file: buffer.file.clone().unwrap_or_default(),
            range: session.range,
            start_char: buffer.position_to_char_idx(session.range.start),
            end_char: buffer.position_to_char_idx(session.range.end),
            detached: false,
            context_before: String::new(),
            context_after: String::new(),
            buffer_id: Some(buffer.id()),
        })
    }

    pub(super) fn inline_target_matches(&self, session: &InlineAssistSession) -> bool {
        self.buffer_manager
            .iter()
            .find(|buffer| buffer.id() == session.buffer_id)
            .is_some_and(|buffer| {
                buffer.revision() == session.expected_revision
                    && buffer.text_in_range(session.range) == session.expected_text
            })
    }

    /// A new turn may recheck changed source, but only at an unambiguous,
    /// edit-tracked location. It must never refresh a ready result's guard.
    pub(super) fn inline_submission_target(&self) -> anyhow::Result<TextRange> {
        let session = self
            .inline_assist
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("inline assist is no longer active"))?;
        let turn = session
            .request_id
            .as_deref()
            .and_then(|request| self.inline_history.turn(request));
        if let Some(turn) = turn {
            anyhow::ensure!(
                turn.state != InlineTurnState::Pending,
                "inline request is still running"
            );
            let (index, range, state) = self
                .resolve_history_source(&turn.location, &session.expected_text, true)
                .ok_or_else(|| anyhow::anyhow!("inline source is no longer available"))?;
            anyhow::ensure!(
                index == self.buffer_manager.active_index() && state != InlineSourceState::Detached,
                "source is detached; select the intended code and start a new inline request"
            );
            Ok(range)
        } else {
            anyhow::ensure!(
                self.current_buffer().id() == session.buffer_id
                    && self.inline_target_matches(session),
                "inline target changed; reopen the saved draft from Space H"
            );
            Ok(session.range)
        }
    }

    pub(super) fn inline_session_state(
        &self,
        session: &InlineAssistSession,
    ) -> InlineAssistPopupState {
        if let Some(turn) = session
            .request_id
            .as_deref()
            .and_then(|id| self.inline_history.turn(id))
        {
            match turn.state {
                InlineTurnState::Pending => return InlineAssistPopupState::Working,
                InlineTurnState::Ready => {
                    if let Some(scope) = turn
                        .result
                        .as_ref()
                        .and_then(|result| result.expanded_scope.as_ref())
                    {
                        let current = self.pending_inline_expansion_range(session);
                        let (start, end) =
                            current
                                .as_ref()
                                .map_or((scope.start_line, scope.end_line), |range| {
                                    (
                                        range.start.line + 1,
                                        range.end.line + usize::from(range.end.character > 0),
                                    )
                                });
                        return InlineAssistPopupState::WiderReady {
                            stale: current.is_err(),
                            summary: format!(
                                "Wider edit · lines {}–{}\n{}",
                                start, end, scope.reason
                            ),
                        };
                    }
                    if let Some(reason) = turn
                        .result
                        .as_ref()
                        .and_then(|result| result.needs_agent.as_ref())
                    {
                        return InlineAssistPopupState::NeedsAgent(reason.clone());
                    }
                    if turn
                        .result
                        .as_ref()
                        .is_some_and(|result| !result.changes_text(&turn.before))
                    {
                        return InlineAssistPopupState::AnswerRetained(turn.error.clone().unwrap_or_else(||
                            "Answer received · source changed; read the retained answer or recheck".into()));
                    }
                    return InlineAssistPopupState::Ready {
                        stale: !self.inline_target_matches(session),
                    };
                }
                InlineTurnState::Declined => {
                    return InlineAssistPopupState::Declined(
                        "The proposal remains in InlineHistory.".into(),
                    )
                }
                InlineTurnState::Failed
                | InlineTurnState::Rejected
                | InlineTurnState::Cancelled => {
                    return InlineAssistPopupState::Failed(
                        turn.error
                            .clone()
                            .unwrap_or_else(|| "Request cancelled".into()),
                    );
                }
                InlineTurnState::Completed => {
                    if let Some(reason) = turn
                        .result
                        .as_ref()
                        .and_then(|result| result.needs_agent.as_ref())
                    {
                        return InlineAssistPopupState::NeedsAgent(reason.clone());
                    }
                }
            }
        }
        if session.has_result {
            InlineAssistPopupState::Applied {
                edited: session.transaction_id.is_some(),
                comments: self.inline_comment_group_count(&session.annotation_group_id),
            }
        } else {
            InlineAssistPopupState::Prompt {
                initial: String::new(),
                refining: false,
            }
        }
    }

    pub(super) fn park_inline_assist(&mut self) {
        let Some(session) = self.inline_assist.take() else {
            return;
        };
        let mut state = self
            .current_dialog
            .as_ref()
            .and_then(|dialog| dialog.inline_assist_state())
            .unwrap_or_else(|| self.inline_session_state(&session));
        if matches!(&state, InlineAssistPopupState::Prompt { initial, .. } if initial.trim().is_empty())
        {
            state = self.inline_session_state(&session);
            if matches!(state, InlineAssistPopupState::Prompt { .. }) {
                if let (Some(session_id), Some(bridge)) =
                    (session.session_id, self.agent_manager.bridge())
                {
                    let _ = bridge.try_send(CodexCommand::CloseSession { session_id });
                }
                if self
                    .current_dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.inline_assist_state().is_some())
                {
                    self.current_dialog = None;
                }
                self.sync_inline_activity();
                return;
            }
        }
        if let Some(location) = self.inline_session_location(&session) {
            self.inline_jobs.insert(
                session.annotation_group_id.clone(),
                ParkedInlineAssist {
                    session,
                    state,
                    location,
                },
            );
        } else {
            self.inline_assist = Some(session);
            return;
        }
        if self
            .current_dialog
            .as_ref()
            .and_then(|dialog| dialog.inline_assist_state())
            .is_some()
        {
            self.current_dialog = None;
        }
        self.sync_inline_activity();
    }

    pub(super) fn inline_request_session_mut(
        &mut self,
        request: &str,
    ) -> Option<&mut InlineAssistSession> {
        if self
            .inline_assist
            .as_ref()
            .is_some_and(|session| session.request_id.as_deref() == Some(request))
        {
            return self.inline_assist.as_mut();
        }
        self.inline_jobs
            .values_mut()
            .map(|job| &mut job.session)
            .find(|session| session.request_id.as_deref() == Some(request))
    }

    /// Store an off-screen result without moving the cursor or mutating source.
    pub(super) fn stage_background_inline_result(
        &mut self,
        request: &str,
        provider: &str,
        result: InlineAssistResult,
    ) {
        if result.expanded_scope.is_some() {
            self.stage_expanded_inline_result(request, provider, result);
            return;
        }
        if !self
            .inline_history
            .turn(request)
            .is_some_and(|turn| turn.state == InlineTurnState::Pending)
        {
            return;
        }
        let Some(session) = self.inline_request_session_mut(request) else {
            return;
        };
        if session
            .session_id
            .as_deref()
            .is_some_and(|id| id != provider)
        {
            return;
        }
        let validation = result.validate_for_target(
            result
                .replacement
                .as_deref()
                .unwrap_or(&session.expected_text),
        );
        session.session_id = Some(provider.to_string());
        session.result_request_id = Some(request.to_string());
        session.has_result = true;
        if let Some(turn) = self
            .inline_history
            .turn_mut(request)
            .filter(|turn| turn.state == InlineTurnState::Pending)
        {
            turn.session_id = Some(provider.to_string());
            match validation {
                Ok(()) => {
                    turn.result = Some(result);
                    turn.state = InlineTurnState::Ready;
                }
                Err(error) => {
                    turn.state = InlineTurnState::Failed;
                    turn.error = Some(error.to_string());
                }
            }
        }
        self.complete_ready_inline_answer(request);
        self.sync_inline_activity();
        self.notify_inline_completion(request);
    }

    /// Comment-only answers need no edit approval. Publish only against an
    /// exact source match, without switching buffers, selections, or dialogs.
    fn complete_ready_inline_answer(&mut self, request: &str) {
        let Some((group, turn)) =
            self.inline_history
                .conversations
                .iter()
                .find_map(|conversation| {
                    let turn = conversation.turns.last()?;
                    (turn.request_id == request && turn.state == InlineTurnState::Ready)
                        .then(|| (conversation.id.clone(), turn.clone()))
                })
        else {
            return;
        };
        let Some(result) = turn
            .result
            .as_ref()
            .filter(|result| result.expanded_scope.is_none() && !result.changes_text(&turn.before))
        else {
            return;
        };
        if result.validate_for_target(&turn.before).is_err() {
            return;
        }
        let exact = self
            .resolve_history_source(&turn.location, &turn.before, false)
            .filter(|(_, _, state)| *state == InlineSourceState::Unchanged);
        let mut location = turn.location.clone();
        let provider = turn.session_id.as_deref().unwrap_or_default();
        if result.needs_agent.is_none() {
            if let Some((index, range, _)) = exact {
                let buffer = &self.buffer_manager[index];
                if let Err(error) = self.check_inline_comment_capacity_for_buffer(
                    buffer.id(),
                    &group,
                    result.comments.len(),
                ) {
                    if let Some(turn) = self.inline_history.turn_mut(request) {
                        turn.error = Some(format!("Answer retained · {error}"));
                    }
                    return;
                }
                location = Self::history_location_in_buffer(buffer, range);
                let select_answer = self.active_inline_comment.is_none() || self.inline_comments.iter().any(|comment|
                    Some(comment.id) == self.active_inline_comment && matches!(&comment.origin,
                        InlineCommentOrigin::Activity { group_id } | InlineCommentOrigin::Assist { group_id, .. } if group_id == &group));
                let first = self.replace_inline_comment_group_in_buffer(
                    index,
                    &group,
                    provider,
                    request,
                    range.start.line,
                    &result.comments,
                );
                if select_answer {
                    self.active_inline_comment = first;
                }
            } else if !result.comments.is_empty() {
                // Retain the original result and anchors for a later exact
                // reattachment; never silently bind comments to changed text.
                return;
            } else {
                self.remove_inline_comment_group(&group);
            }
        }
        self.complete_inline_history_turn_at(request, provider, result, location, None);
        let current_target = exact.map(|(index, range, _)| {
            let buffer = &self.buffer_manager[index];
            (buffer.id(), buffer.revision(), range)
        });
        if let Some(session) = self.inline_request_session_mut(request) {
            session.has_result = true;
            session.result_request_id = Some(request.to_string());
            if let Some((buffer_id, revision, range)) = current_target {
                session.buffer_id = buffer_id;
                session.expected_revision = revision;
                session.range = range;
                session.expected_text.clone_from(&turn.before);
            }
        }
    }

    pub(super) fn complete_ready_inline_answers(&mut self) {
        let requests = self
            .inline_history
            .conversations
            .iter()
            .filter_map(|conversation| conversation.turns.last())
            .filter(|turn| {
                turn.state == InlineTurnState::Ready
                    && turn
                        .result
                        .as_ref()
                        .is_some_and(|result| !result.changes_text(&turn.before))
            })
            .map(|turn| turn.request_id.clone())
            .collect::<Vec<_>>();
        for request in requests {
            self.complete_ready_inline_answer(&request);
        }
    }

    pub(super) fn record_inline_failure(&mut self, request: &str, message: &str) {
        if !self
            .inline_history
            .turn(request)
            .is_some_and(|turn| turn.state == InlineTurnState::Pending)
        {
            return;
        }
        self.inline_history
            .finish(request, InlineTurnState::Failed, Some(message.to_string()));
        if let Some(session) = self.inline_request_session_mut(request) {
            session.session_id = None;
        }
        self.sync_inline_activity();
        self.notify_inline_completion(request);
    }

    pub(super) fn inline_request_for_provider(&self, provider: &str) -> Option<String> {
        self.inline_assist
            .iter()
            .chain(self.inline_jobs.values().map(|job| &job.session))
            .find(|session| session.session_id.as_deref() == Some(provider))
            .and_then(|session| session.request_id.clone())
    }

    pub(super) fn fail_running_inline_jobs(&mut self, message: &str) {
        let requests = self
            .inline_assist
            .iter()
            .chain(self.inline_jobs.values().map(|job| &job.session))
            .filter_map(|session| session.request_id.as_deref())
            .filter(|request| {
                self.inline_history
                    .turn(request)
                    .is_some_and(|turn| turn.state == InlineTurnState::Pending)
            })
            .map(str::to_string)
            .collect::<Vec<_>>();
        for request in requests {
            self.record_inline_failure(&request, message);
        }
        for session in self
            .inline_assist
            .iter_mut()
            .chain(self.inline_jobs.values_mut().map(|job| &mut job.session))
        {
            session.session_id = None;
        }
    }

    /// Status annotations use the existing edit-tracked rail, never source text.
    pub(super) fn sync_inline_activity(&mut self) {
        if self.inline_activity_animation.running.is_empty() {
            self.inline_activity_animation.since = Instant::now();
            self.inline_activity_animation.frame = 0;
        }
        let mut specs = Vec::new();
        let mut running = HashMap::new();
        for (session, parked_state) in self
            .inline_assist
            .iter()
            .map(|session| (session, None))
            .chain(
                self.inline_jobs
                    .values()
                    .map(|job| (&job.session, Some(&job.state))),
            )
        {
            let turn = session
                .request_id
                .as_deref()
                .and_then(|id| self.inline_history.turn(id));
            let state = match parked_state {
                _ if turn.is_some_and(|turn| !turn.agent_outcomes.is_empty()) => continue,
                Some(state @ InlineAssistPopupState::Prompt { .. }) => state.clone(),
                _ => self.inline_session_state(session),
            };
            if turn.is_some_and(|turn| turn.has_code_change())
                && matches!(state, InlineAssistPopupState::Applied { .. })
            {
                continue;
            }
            let resolved = self
                .inline_history
                .conversations
                .iter()
                .any(|conversation| {
                    conversation.id == session.annotation_group_id && conversation.resolved
                });
            if resolved
                && matches!(
                    state,
                    InlineAssistPopupState::Applied { .. }
                        | InlineAssistPopupState::NeedsAgent(_)
                        | InlineAssistPopupState::Declined(_)
                        | InlineAssistPopupState::Failed(_)
                )
            {
                continue;
            }
            let status = match &state {
                InlineAssistPopupState::Working => "Working",
                InlineAssistPopupState::Ready { .. } => "● Ready",
                InlineAssistPopupState::WiderReady { .. } => "◈ Review wider edit",
                InlineAssistPopupState::AnswerRetained(_) => "✓ Answer retained",
                InlineAssistPopupState::NeedsAgent(_) => "↗ Needs Agent",
                InlineAssistPopupState::Failed(_) => "! Stopped",
                InlineAssistPopupState::Declined(_) => "– Declined",
                InlineAssistPopupState::Prompt { .. } if parked_state.is_some() => "✎ Draft",
                InlineAssistPopupState::Applied { comments: 0, .. }
                    if turn.is_none_or(|turn| {
                        turn.result
                            .as_ref()
                            .is_none_or(|result| result.comments.is_empty())
                    }) =>
                {
                    "✓ Done"
                }
                _ => continue,
            };
            let prompt = match &state {
                InlineAssistPopupState::Prompt { initial, .. } => initial.as_str(),
                _ => turn.map_or("Inline assist", |turn| turn.prompt.as_str()),
            };
            let display_prompt = truncate_display_width(&crate::ui::first_prompt_line(prompt), 64);
            let message = if matches!(state, InlineAssistPopupState::Working) {
                self.inline_activity_animation.message(&display_prompt)
            } else if matches!(
                state,
                InlineAssistPopupState::AnswerRetained(_) | InlineAssistPopupState::Applied { .. }
            ) {
                turn.map_or_else(
                    || format!("{status} · {prompt} · Space H"),
                    |turn| {
                        format!(
                            "{status} · Space H\n{}",
                            truncate_chars(&turn.answer_text(), 4096)
                        )
                    },
                )
            } else {
                format!("{status} · {} · Space H", display_prompt)
            };
            let expanded = turn
                .filter(|_| matches!(state, InlineAssistPopupState::WiderReady { .. }))
                .and_then(|turn| {
                    Some((
                        turn.expanded_location.as_ref()?,
                        turn.result.as_ref()?.expanded_scope.as_ref()?,
                    ))
                });
            let location = expanded.map(|(location, _)| location.clone()).or_else(|| {
                turn.map(|turn| turn.location.clone())
                    .or_else(|| {
                        self.inline_jobs
                            .get(&session.annotation_group_id)
                            .map(|job| job.location.clone())
                    })
                    .or_else(|| self.inline_session_location(session))
            });
            let expected = expanded.map_or(session.expected_text.as_str(), |(_, scope)| {
                scope.before.as_str()
            });
            let Some((index, range, source_state)) = location
                .as_ref()
                .and_then(|location| self.resolve_history_source(location, expected, true))
            else {
                continue;
            };
            if source_state == InlineSourceState::Detached {
                continue;
            }
            if matches!(state, InlineAssistPopupState::Working) {
                running.insert(session.annotation_group_id.clone(), display_prompt);
            }
            specs.push((
                session.annotation_group_id.clone(),
                self.buffer_manager[index].id(),
                range,
                message,
            ));
        }
        let groups = specs
            .iter()
            .map(|(group, ..)| group.as_str())
            .collect::<HashSet<_>>();
        self.inline_comments
            .retain(|comment| match &comment.origin {
                InlineCommentOrigin::Activity { group_id } => groups.contains(group_id.as_str()),
                _ => true,
            });
        for (group, buffer_id, range, message) in specs {
            let Some(buffer) = self
                .buffer_manager
                .iter()
                .find(|buffer| buffer.id() == buffer_id)
            else {
                continue;
            };
            let last = buffer.navigable_line_count().saturating_sub(1);
            let start = TextPosition::new(range.start.line.min(last), 0);
            let end_line = range.end.line.saturating_sub(usize::from(
                range.end.character == 0 && range.end.line > range.start.line,
            ));
            let end = TextPosition::new(end_line.max(start.line).min(last), 0);
            let anchor = |position| EditAnchor {
                buffer_id,
                file: buffer.file.clone(),
                char_index: buffer.position_to_char_idx(position),
                fallback: position,
                affinity: AnchorAffinity::Right,
            };
            if let Some(comment) = self.inline_comments.iter_mut().find(|comment|
                matches!(&comment.origin, InlineCommentOrigin::Activity { group_id } if group_id == &group))
            {
                comment.message = message;
                comment.anchor = anchor(start);
                comment.end_anchor = anchor(end);
                comment.detached = false;
                continue;
            }
            let id = uuid::Uuid::new_v4();
            self.inline_comments.push(InlineComment {
                id,
                anchor: anchor(start),
                end_anchor: anchor(end),
                message,
                origin: InlineCommentOrigin::Activity {
                    group_id: group.clone(),
                },
                stale: false,
                detached: false,
                expected_fingerprint: None,
            });
            if self
                .inline_assist
                .as_ref()
                .is_some_and(|session| session.annotation_group_id == group)
            {
                self.active_inline_comment = Some(id);
            }
        }
        self.inline_activity_animation.running = running;
        self.sync_inline_change_summaries();
        self.layout_cache.borrow_mut().clear();
        self.mark_inline_history_dirty();
    }

    /// Advance only running markers. Reuse the normal decoration repaint path
    /// and leave InlineHistory, source, and selection untouched.
    pub(super) fn poll_inline_activity_animation(&mut self, now: Instant) -> bool {
        let animation = &mut self.inline_activity_animation;
        if animation.running.is_empty() {
            return false;
        }
        let frame = now.saturating_duration_since(animation.since).as_millis() as u64
            / SPINNER_FRAME_INTERVAL_MS;
        if frame == animation.frame {
            return false;
        }
        animation.frame = frame;
        let mut changed = HashSet::new();
        let mut buffers = HashSet::new();
        for comment in &mut self.inline_comments {
            let InlineCommentOrigin::Activity { group_id } = &comment.origin else {
                continue;
            };
            let Some(prompt) = animation.running.get(group_id) else {
                continue;
            };
            let message = animation.message(prompt);
            if comment.message != message {
                comment.message = message;
                changed.insert(comment.id);
                buffers.insert(comment.anchor.buffer_id);
            }
        }
        if changed.is_empty() {
            return false;
        }
        self.layout_cache
            .borrow_mut()
            .retain(|key, _| !buffers.contains(&key.buffer_id));
        // Modal surfaces have their own animation. Refresh the stored glyphs,
        // but do not redraw a covered editor just for its background spinner.
        if self.current_dialog.is_some() || self.workspace_manager.is_active() {
            return false;
        }
        let active = self.active_window_with_editor_view();
        self.window_manager.windows().into_iter().any(|window| {
            let window = if window.active {
                active.as_ref().unwrap_or(window)
            } else {
                window
            };
            self.window_manager.is_presented(window.id)
                && self.inline_activity_visible_in_window(window, &changed)
        })
    }

    pub(super) fn has_parked_inline_draft(&self, group: &str) -> bool {
        self.inline_jobs
            .get(group)
            .is_some_and(|job| matches!(&job.state, InlineAssistPopupState::Prompt { initial, .. } if !initial.trim().is_empty()))
    }

    pub(super) fn release_parked_inline_job(&mut self, group: &str) {
        if let Some(job) = self.inline_jobs.remove(group) {
            if let (Some(session_id), Some(bridge)) =
                (job.session.session_id, self.agent_manager.bridge())
            {
                let _ = bridge.try_send(CodexCommand::CloseSession { session_id });
            }
        }
        self.sync_inline_activity();
    }

    pub(super) async fn open_inline_job(
        &mut self,
        group: &str,
        frame: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        self.park_inline_assist();
        if self.inline_history_browser.is_some() {
            self.close_inline_history(true, frame, runtime).await?;
        }
        self.complete_ready_inline_answers();
        self.sync_inline_activity();
        let mut turn = self
            .inline_history
            .conversations
            .iter()
            .find(|conversation| conversation.id == group)
            .and_then(|conversation| conversation.turns.last())
            .cloned();
        let location = self
            .inline_history
            .conversations
            .iter()
            .find(|conversation| conversation.id == group)
            .and_then(|conversation| conversation.turns.last())
            .map(|turn| turn.location.clone())
            .or_else(|| self.inline_jobs.get(group).map(|job| job.location.clone()));
        let Some(location) = location else {
            self.set_legacy_message(Some("inline item is no longer available".into()));
            return Ok(());
        };
        if !self.buffer_manager.iter().any(|buffer| {
            location.buffer_id == Some(buffer.id())
                || (!location.file.is_empty() && buffer.file.as_deref() == Some(&location.file))
        }) && Path::new(&location.file).is_file()
        {
            self.execute_with_tracking(
                &Action::OpenFile(location.file.clone()),
                frame,
                runtime,
                false,
            )
            .await?;
        }
        if let Some(request) = turn.as_ref().map(|turn| turn.request_id.clone()) {
            self.complete_ready_inline_answer(&request);
            turn = self.inline_history.turn(&request).cloned();
        }
        let resolved = if let Some(job) = self.inline_jobs.get(group) {
            self.resolve_history_source(&location, &job.session.expected_text, true)
        } else {
            turn.as_ref()
                .and_then(|turn| self.resolve_history_turn(turn))
        };
        let Some((index, range, source_state)) = resolved else {
            self.current_dialog = Some(Box::new(HoverInfo::new(self,
                turn.as_ref().map_or_else(|| "The source buffer is no longer available. The draft remains in InlineHistory.".into(), |turn| turn.answer_text()),
                HoverInfoFormat::Plaintext, Vec::new()).with_label("Inline result · source unavailable").with_close_action(Action::OpenInlineHistory)));
            return self.render(frame);
        };
        if self.buffer_manager.active_index() != index {
            self.set_current_buffer(frame, index).await?;
        }
        let Some(window_id) = self.window_manager.active_stable_window_id() else {
            return Ok(());
        };
        let (mut session, saved_state) = if let Some(job) = self.inline_jobs.remove(group) {
            (job.session, Some(job.state))
        } else {
            let turn = turn.as_ref().expect("retained inline item has a turn");
            let expected_text = if turn.state != InlineTurnState::Completed {
                turn.before.clone()
            } else {
                turn.reviewed().to_string()
            };
            let exact = source_state == InlineSourceState::Unchanged
                && self.current_buffer().text_in_range(range) == expected_text;
            (
                InlineAssistSession {
                    allow_expansion: turn.allow_expansion,
                    buffer_id: self.current_buffer().id(),
                    window_id,
                    expected_revision: if exact {
                        self.current_buffer().revision()
                    } else {
                        u64::MAX
                    },
                    range,
                    expected_text,
                    scope: format!(
                        "lines {}–{} · reopened",
                        range.start.line + 1,
                        range.end.line + usize::from(range.end.character > 0)
                    ),
                    request_id: Some(turn.request_id.clone()),
                    session_id: None,
                    transaction_id: turn.transaction_id.clone(),
                    annotation_group_id: group.to_string(),
                    has_result: turn.result.is_some(),
                    result_request_id: turn.result.as_ref().map(|_| turn.request_id.clone()),
                },
                None,
            )
        };
        session.window_id = window_id;
        // Drafts may follow an exact, unambiguous source relocation. Running and
        // ready edits retain their original revision guard.
        if matches!(saved_state, Some(InlineAssistPopupState::Prompt { .. }))
            && source_state == InlineSourceState::Unchanged
        {
            session.range = range;
            session.buffer_id = self.current_buffer().id();
            session.expected_revision = self.current_buffer().revision();
        }
        self.move_to_text_position(range.start);
        self.refresh_cursor_goal();
        let state = match saved_state {
            Some(state @ InlineAssistPopupState::Prompt { .. })
                if self.inline_target_matches(&session) =>
            {
                state
            }
            Some(InlineAssistPopupState::Prompt { initial, refining }) => {
                self.inline_jobs.insert(
                    group.to_string(),
                    ParkedInlineAssist {
                        session,
                        state: InlineAssistPopupState::Prompt {
                            initial: initial.clone(),
                            refining,
                        },
                        location,
                    },
                );
                self.current_dialog = Some(Box::new(HoverInfo::new(self, format!("The draft target changed. Select the intended source before starting another request.\n\nSaved draft:\n{initial}"), HoverInfoFormat::Plaintext, Vec::new()).with_label("Inline draft retained").with_close_action(Action::OpenInlineHistory)));
                return self.render(frame);
            }
            _ => self.inline_session_state(&session),
        };
        let scope = session.scope.clone();
        self.inline_assist = Some(session);
        self.sync_inline_activity();
        self.select_inline_comment_for_group(group);
        self.current_dialog = Some(Box::new(self.inline_assist_popup(scope, state)));
        self.render(frame)
    }
}
