//! Validation and staging for proposals that widen an automatic inline target.

use super::*;

#[cfg(test)]
mod tests;

impl Editor {
    fn validate_inline_expansion(
        &self,
        request: &str,
        result: &InlineAssistResult,
    ) -> anyhow::Result<crate::inline_history::InlineLocation> {
        let scope = result
            .expanded_scope
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("missing wider edit scope"))?;
        result.validate_for_target(result.replacement.as_deref().unwrap_or_default())?;
        let session = self
            .inline_assist
            .iter()
            .chain(self.inline_jobs.values().map(|job| &job.session))
            .find(|session| session.request_id.as_deref() == Some(request))
            .ok_or_else(|| anyhow::anyhow!("inline request is no longer active"))?;
        anyhow::ensure!(
            session.allow_expansion
                && self
                    .inline_history
                    .turn(request)
                    .is_some_and(|turn| turn.allow_expansion),
            "explicit selections cannot be expanded; continue in Agent or start from a cursor"
        );
        anyhow::ensure!(
            scope.expected_revision == session.expected_revision
                && self.inline_target_matches(session),
            "source changed; recheck the wider proposal against current code"
        );
        let buffer = self
            .buffer_manager
            .iter()
            .find(|buffer| buffer.id() == session.buffer_id)
            .ok_or_else(|| anyhow::anyhow!("source buffer is no longer available"))?;
        anyhow::ensure!(
            scope.end_line <= buffer.navigable_line_count(),
            "wider edit extends beyond the source file"
        );
        let range = TextRange::new(
            TextPosition::new(scope.start_line - 1, 0),
            if scope.end_line <= buffer.len() {
                TextPosition::new(scope.end_line, 0)
            } else {
                buffer.char_idx_to_position(usize::MAX)
            },
        );
        let key = |position: TextPosition| (position.line, position.character);
        anyhow::ensure!(
            key(range.start) <= key(session.range.start)
                && key(range.end) >= key(session.range.end)
                && range != session.range,
            "wider edit must contain and extend the original target"
        );
        anyhow::ensure!(
            buffer.text_in_range(range) == scope.before,
            "wider edit original text does not match the source"
        );
        Ok(Self::history_location_in_buffer(buffer, range))
    }

    pub(super) fn stage_expanded_inline_result(
        &mut self,
        request: &str,
        provider: &str,
        result: InlineAssistResult,
    ) {
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
        let validation = self.validate_inline_expansion(request, &result);
        if let Ok(location) = &validation {
            if let Some(turn) = self.inline_history.turn_mut(request) {
                turn.expanded_location = Some(location.clone());
            }
        }
        if let Some(session) = self.inline_request_session_mut(request) {
            session.session_id = Some(provider.into());
            session.result_request_id = Some(request.into());
            session.has_result = true;
        }
        if let Some(turn) = self.inline_history.turn_mut(request) {
            turn.session_id = Some(provider.into());
            turn.result = Some(result);
            match validation {
                Ok(_) => {
                    turn.state = InlineTurnState::Ready;
                    turn.error = None;
                }
                Err(error) => {
                    turn.state = InlineTurnState::Rejected;
                    turn.error = Some(format!("Wider edit not applied: {error}"));
                }
            }
        }
        self.sync_inline_activity();
        self.notify_inline_completion(request);
    }

    pub(super) fn pending_inline_expansion_range(
        &self,
        session: &InlineAssistSession,
    ) -> anyhow::Result<TextRange> {
        let turn = session
            .request_id
            .as_deref()
            .and_then(|request| self.inline_history.turn(request))
            .ok_or_else(|| anyhow::anyhow!("wider proposal is no longer available"))?;
        anyhow::ensure!(
            session.allow_expansion && turn.allow_expansion && turn.state == InlineTurnState::Ready,
            "wider proposal is no longer awaiting review"
        );
        anyhow::ensure!(
            self.inline_target_matches(session),
            "source changed; recheck the wider proposal"
        );
        let scope = turn
            .result
            .as_ref()
            .and_then(|result| result.expanded_scope.as_ref())
            .ok_or_else(|| anyhow::anyhow!("missing wider proposal"))?;
        let location = turn
            .expanded_location
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("wider proposal was not validated"))?;
        let (index, range, state) = self
            .resolve_history_source(location, &scope.before, false)
            .ok_or_else(|| anyhow::anyhow!("wider proposal source is unavailable"))?;
        anyhow::ensure!(
            self.buffer_manager[index].id() == session.buffer_id
                && state == crate::inline_history::InlineSourceState::Unchanged,
            "wider proposal source changed"
        );
        let key = |position: TextPosition| (position.line, position.character);
        anyhow::ensure!(
            key(range.start) <= key(session.range.start)
                && key(range.end) >= key(session.range.end)
                && range != session.range,
            "wider proposal no longer contains the original target"
        );
        Ok(range)
    }

    /// Called only after the request-bound diff review has been approved.
    pub(super) fn prepare_reviewed_inline_expansion(
        &mut self,
        request: &str,
    ) -> anyhow::Result<()> {
        let session = self
            .inline_assist
            .as_ref()
            .filter(|session| session.request_id.as_deref() == Some(request))
            .ok_or_else(|| anyhow::anyhow!("wider edit review is no longer current"))?;
        let range = self.pending_inline_expansion_range(session)?;
        let turn = self
            .inline_history
            .turn(request)
            .expect("validated retained proposal");
        let before = turn
            .result
            .as_ref()
            .and_then(|result| result.expanded_scope.as_ref())
            .expect("validated wider scope")
            .before
            .clone();
        anyhow::ensure!(
            self.current_buffer().id() == session.buffer_id,
            "active buffer changed during wider edit review"
        );
        let location = Self::history_location_in_buffer(self.current_buffer(), range);
        if let Some(session) = self.inline_assist.as_mut() {
            session.range = range;
            session.expected_text.clone_from(&before);
            session.scope = format!(
                "wider edit · lines {}–{}",
                range.start.line + 1,
                range.end.line + usize::from(range.end.character > 0)
            );
        }
        if let Some(turn) = self.inline_history.turn_mut(request) {
            turn.before = before;
            turn.location = location;
        }
        Ok(())
    }
}
