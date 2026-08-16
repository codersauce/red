//! Attribution and read-only review for explicit inline-to-Agent continuations.

use super::*;
use crate::inline_history::{
    HistoryView, InlineAgentEdit, InlineAgentFile, InlineAgentOutcome, InlineAgentState,
    InlineHistoryTurn, MAX_AGENT_IMAGE_BYTES, MAX_ANSWER_BYTES, MAX_HISTORY_BYTES,
};
use crate::ui::{HistoryBlock, HistoryStatus, HistoryTone};

#[cfg(test)]
mod tests;

pub(super) struct StagedHandoff {
    pub request_id: String,
}

pub(super) fn handoff_marker(request: &str) -> String {
    format!("Red inline history reference: {request}")
}

fn bounded_text(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

impl Editor {
    pub(super) fn hide_inline_agent_file(&mut self, request: &str, path: &str) {
        if let Some(turn) = self.inline_history.turn_mut(request) {
            for file in turn
                .agent_outcomes
                .iter_mut()
                .flat_map(|outcome| &mut outcome.files)
                .filter(|file| file.path == path)
            {
                file.hidden = true;
            }
        }
        self.mark_inline_history_dirty();
    }

    pub(super) fn inline_agent_file_review_target(
        &self,
        request: &str,
        path: &str,
    ) -> Option<(usize, usize)> {
        let turn = self.inline_history.turn(request)?;
        let outcome_index = turn.agent_outcomes.len().checked_sub(1)?;
        let outcome = &turn.agent_outcomes[outcome_index];
        let mut change = 0;
        for file in &outcome.files {
            if file.path == path {
                return Some((outcome_index, change));
            }
            change += file
                .edits
                .iter()
                .map(|edit| edit.changed_lines().len().max(1))
                .sum::<usize>();
        }
        None
    }

    pub(super) fn sync_inline_agent_markers(&mut self) {
        use super::inline_comments::InlineCommentOrigin;
        let mut records = Vec::new();
        for conversation in self
            .inline_history
            .conversations
            .iter()
            .filter(|conversation| !conversation.resolved)
        {
            for turn in &conversation.turns {
                let Some(outcome) = turn.agent_outcomes.last() else {
                    continue;
                };
                for file in outcome.files.iter().filter(|file| !file.hidden) {
                    let Some(index) = self.file_buffer_index(Path::new(&file.path)) else {
                        continue;
                    };
                    let Some(edit) = file.edits.last() else {
                        continue;
                    };
                    let buffer = &self.buffer_manager[index];
                    let existing = self.inline_comments.iter().find(|comment| matches!(&comment.origin, InlineCommentOrigin::AgentOutcome { request_id, file: path } if request_id == &turn.request_id && path == &file.path));
                    let line = if buffer.contents_snapshot() == edit.after.as_str() {
                        edit.changed_lines()
                            .first()
                            .copied()
                            .unwrap_or(0)
                            .min(buffer.last_navigable_line())
                    } else if let Some(existing) = existing.filter(|comment| {
                        comment.anchor.buffer_id == buffer.id() && !comment.detached
                    }) {
                        existing.lines(buffer).0
                    } else {
                        continue;
                    };
                    let (status, _) = self.inline_agent_file_status(file);
                    let icon = match outcome.state {
                        InlineAgentState::Running => "↗",
                        InlineAgentState::Completed => "✓",
                        InlineAgentState::Failed | InlineAgentState::Cancelled => "!",
                    };
                    let message = format!(
                        "{icon} {} · {status} · {} file(s)\n{} · Space v changes · Space H",
                        outcome.state.label(),
                        outcome.files.len(),
                        crate::ui::first_prompt_line(&turn.prompt)
                    );
                    records.push((
                        turn.request_id.clone(),
                        file.path.clone(),
                        index,
                        line,
                        message,
                    ));
                }
            }
        }
        let wanted = records
            .iter()
            .map(|(request, file, ..)| (request.as_str(), file.as_str()))
            .collect::<HashSet<_>>();
        let mut changed = HashSet::new();
        self.inline_comments
            .retain(|comment| match &comment.origin {
                InlineCommentOrigin::AgentOutcome { request_id, file }
                    if !wanted.contains(&(request_id.as_str(), file.as_str())) =>
                {
                    changed.insert(comment.anchor.buffer_id);
                    false
                }
                _ => true,
            });
        for (request_id, file, index, line, message) in records {
            let mut value = Self::make_inline_comment_in_buffer(
                &self.buffer_manager[index],
                line,
                line,
                message,
                InlineCommentOrigin::AgentOutcome {
                    request_id: request_id.clone(),
                    file: file.clone(),
                },
            );
            if let Some(existing) = self.inline_comments.iter_mut().find(|comment| matches!(&comment.origin, InlineCommentOrigin::AgentOutcome { request_id: owner, file: path } if owner == &request_id && path == &file)) {
                if existing.message == value.message && existing.anchor.buffer_id == value.anchor.buffer_id && existing.anchor.char_index == value.anchor.char_index && !existing.detached { continue; }
                changed.insert(existing.anchor.buffer_id);
                value.id = existing.id;
                *existing = value;
            } else { self.inline_comments.push(value); }
            changed.insert(self.buffer_manager[index].id());
        }
        if !changed.is_empty() {
            self.layout_cache
                .borrow_mut()
                .retain(|key, _| !changed.contains(&key.buffer_id));
        }
    }

    pub(super) fn begin_inline_agent_outcome(
        &mut self,
        session: &str,
        agent_turn: &str,
        prompt: &str,
    ) -> anyhow::Result<()> {
        let Some(staged) = self.staged_inline_agent_handoff.take() else {
            return Ok(());
        };
        if !prompt
            .lines()
            .any(|line| line == handoff_marker(&staged.request_id))
        {
            return Ok(());
        }
        let same_workspace = self
            .inline_history
            .conversations
            .iter()
            .any(|conversation| {
                conversation
                    .turns
                    .iter()
                    .any(|turn| turn.request_id == staged.request_id)
                    && self
                        .agent_manager
                        .root()
                        .is_some_and(|root| same_file_path(Path::new(&conversation.cwd), root))
            });
        if !same_workspace {
            return Ok(());
        }
        anyhow::ensure!(
            serde_json::to_vec(&self.inline_history)?
                .len()
                .saturating_add(MAX_ANSWER_BYTES)
                < MAX_HISTORY_BYTES,
            "inline history is full; export or forget old conversations before continuing in Agent"
        );
        if let Some(turn) = self.inline_history.turn_mut(&staged.request_id) {
            turn.agent_outcomes
                .push(InlineAgentOutcome::new(session.into(), agent_turn.into()));
        }
        self.sync_inline_activity();
        Ok(())
    }

    fn active_inline_agent_request(&self, session: &str) -> Option<(String, usize)> {
        let agent_turn = self.agent_manager.turn_id(session)?;
        self.inline_history
            .conversations
            .iter()
            .flat_map(|conversation| &conversation.turns)
            .find_map(|turn| {
                turn.agent_outcomes
                    .iter()
                    .position(|outcome| {
                        outcome.session_id == session
                            && outcome.turn_id == agent_turn
                            && outcome.state == InlineAgentState::Running
                    })
                    .map(|index| (turn.request_id.clone(), index))
            })
    }

    pub(super) fn check_inline_agent_receipt_capacity(
        &self,
        session: &str,
        before: &str,
        after: &str,
    ) -> anyhow::Result<()> {
        if before == after || self.active_inline_agent_request(session).is_none() {
            return Ok(());
        }
        anyhow::ensure!(
            before.len() <= MAX_AGENT_IMAGE_BYTES && after.len() <= MAX_AGENT_IMAGE_BYTES,
            "file is too large for a retained inline-to-Agent review"
        );
        let used = serde_json::to_vec(&self.inline_history)?.len();
        let added = serde_json::to_vec(&(before, after))?.len();
        anyhow::ensure!(
            used.saturating_add(added)
                .saturating_add(MAX_ANSWER_BYTES + 4096)
                <= MAX_HISTORY_BYTES,
            "inline history is full; export or forget old conversations before editing more files"
        );
        Ok(())
    }

    pub(super) fn record_inline_agent_edit(
        &mut self,
        session: &str,
        path: &Path,
        created: bool,
        edit: InlineAgentEdit,
    ) {
        let Some((request, index)) = self.active_inline_agent_request(session) else {
            return;
        };
        if let Some(outcome) = self
            .inline_history
            .turn_mut(&request)
            .and_then(|turn| turn.agent_outcomes.get_mut(index))
        {
            outcome.record(path.to_string_lossy().into_owned(), created, edit);
        }
        self.mark_inline_history_dirty();
    }

    pub(super) fn finish_inline_agent_outcome(
        &mut self,
        session: &str,
        state: InlineAgentState,
        error: Option<&str>,
    ) {
        let Some((request, index)) = self.active_inline_agent_request(session) else {
            return;
        };
        let agent_turn = self.agent_manager.turn_id(session).unwrap_or_default();
        let answer = self
            .agent_manager
            .conversation_snapshot()
            .map(|conversation| {
                conversation
                    .items
                    .iter()
                    .filter(|item| {
                        item.turn_id.as_deref() == Some(agent_turn)
                            && item.role == crate::agent_conversation::AgentTranscriptRole::Agent
                    })
                    .map(|item| item.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n")
            })
            .unwrap_or_default();
        if let Some(outcome) = self
            .inline_history
            .turn_mut(&request)
            .and_then(|turn| turn.agent_outcomes.get_mut(index))
        {
            outcome.state = state;
            outcome.answer = bounded_text(&answer, MAX_ANSWER_BYTES);
            outcome.error = error.map(|error| bounded_text(error, 4096));
        }
        self.sync_inline_activity();
        self.notify_inline_outcome(&request);
    }

    pub(super) fn stop_inline_agent_outcomes(&mut self, message: &str) {
        let sessions = self
            .inline_history
            .conversations
            .iter()
            .flat_map(|conversation| &conversation.turns)
            .flat_map(|turn| &turn.agent_outcomes)
            .filter(|outcome| outcome.state == InlineAgentState::Running)
            .map(|outcome| outcome.session_id.clone())
            .collect::<Vec<_>>();
        for session in sessions {
            self.finish_inline_agent_outcome(&session, InlineAgentState::Failed, Some(message));
        }
    }

    pub(super) fn inline_agent_file_status(
        &self,
        file: &InlineAgentFile,
    ) -> (&'static str, HistoryTone) {
        let Some(edit) = file.edits.last() else {
            return ("No edits", HistoryTone::Muted);
        };
        if let Some(index) = self.file_buffer_index(Path::new(&file.path)) {
            let buffer = &self.buffer_manager[index];
            if buffer.contents_snapshot() != edit.after.as_str() {
                return (
                    if buffer.is_dirty() {
                        "Changed since Agent · unsaved"
                    } else {
                        "Changed since Agent · saved"
                    },
                    HistoryTone::Warning,
                );
            }
            return if buffer.is_dirty() {
                ("Unsaved", HistoryTone::Warning)
            } else {
                ("Saved", HistoryTone::Success)
            };
        }
        match fs::metadata(&file.path) {
            Ok(metadata)
                if metadata.is_file() && metadata.len() <= MAX_AGENT_IMAGE_BYTES as u64 =>
            {
                if fs::read_to_string(&file.path).is_ok_and(|text| text == edit.after) {
                    ("Saved", HistoryTone::Success)
                } else {
                    ("Changed on disk since Agent", HistoryTone::Warning)
                }
            }
            Ok(_) => ("Source unavailable", HistoryTone::Warning),
            Err(_) => ("Source unavailable", HistoryTone::Warning),
        }
    }

    pub(super) fn inline_agent_history_blocks(
        &self,
        turn: &InlineHistoryTurn,
        view: HistoryView,
        cwd: &Path,
    ) -> Vec<HistoryBlock> {
        let mut blocks = Vec::new();
        for outcome in &turn.agent_outcomes {
            blocks.push(HistoryBlock::Plain(format!(
                "{} · {} file(s) · {} changed location(s)",
                outcome.state.label(),
                outcome.files.len(),
                outcome.change_count()
            )));
            if let Some(error) = &outcome.error {
                blocks.push(HistoryBlock::Plain(error.clone()));
            }
            if outcome.files.is_empty() {
                blocks.push(HistoryBlock::Plain(
                    if outcome.state == InlineAgentState::Running {
                        "No editor writes yet."
                    } else {
                        "No editor writes were applied."
                    }
                    .into(),
                ));
            }
            for file in &outcome.files {
                let line = file
                    .edits
                    .last()
                    .and_then(|edit| edit.changed_lines().first().copied())
                    .unwrap_or(0)
                    + 1;
                let label = Path::new(&file.path)
                    .strip_prefix(cwd)
                    .unwrap_or(Path::new(&file.path))
                    .display()
                    .to_string();
                blocks.push(HistoryBlock::FileLink {
                    text: format!("{label}:{line}"),
                    path: file.path.clone(),
                    line,
                });
                let (status, tone) = self.inline_agent_file_status(file);
                blocks.push(HistoryBlock::Status(HistoryStatus::new(
                    format!(
                        "{} · {status} · {} edit step(s)",
                        if file.created { "Created" } else { "Modified" },
                        file.edits.len()
                    ),
                    tone,
                )));
                if view == HistoryView::Changes {
                    for edit in &file.edits {
                        blocks.push(HistoryBlock::Diff {
                            file: file.path.clone(),
                            before: edit.before.clone(),
                            after: edit.after.clone(),
                            label: "after Agent edit".into(),
                        });
                    }
                }
            }
            if view == HistoryView::Conversation && !outcome.answer.is_empty() {
                blocks.push(HistoryBlock::Markdown(outcome.answer.clone()));
            }
        }
        blocks
    }

    pub(super) fn inline_agent_status(outcome: &InlineAgentOutcome) -> HistoryStatus {
        HistoryStatus::new(
            outcome.state.label(),
            match outcome.state {
                InlineAgentState::Running => HistoryTone::Info,
                InlineAgentState::Completed => HistoryTone::Success,
                InlineAgentState::Failed => HistoryTone::Error,
                InlineAgentState::Cancelled => HistoryTone::Warning,
            },
        )
    }

    pub(super) async fn view_inline_agent_changes(
        &mut self,
        request: &str,
        outcome_index: usize,
        change: usize,
        frame: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let Some(turn) = self.inline_history.turn(request).cloned() else {
            return Ok(());
        };
        let Some(outcome) = turn.agent_outcomes.get(outcome_index) else {
            return Ok(());
        };
        self.park_inline_assist();
        if self.inline_history_browser.is_some() {
            self.close_inline_history(true, frame, runtime).await?;
        }
        self.mode = Mode::Normal;
        self.selection = None;
        self.selection_start = None;
        self.panel_manager.focus_editor();
        let locations = outcome
            .files
            .iter()
            .enumerate()
            .flat_map(|(file_index, file)| {
                file.edits
                    .iter()
                    .enumerate()
                    .flat_map(move |(edit_index, edit)| {
                        let lines = edit.changed_lines();
                        if lines.is_empty() {
                            vec![(file_index, edit_index, 0)]
                        } else {
                            lines
                                .iter()
                                .copied()
                                .map(|line| (file_index, edit_index, line))
                                .collect()
                        }
                    })
            })
            .collect::<Vec<_>>();
        let selected = change.min(locations.len().saturating_sub(1));
        let action = |change| Action::ViewInlineAgentChanges {
            request_id: request.into(),
            outcome: outcome_index,
            change,
        };
        let mut text = format!(
            "{} · {} file(s) · {} changed location(s)\n\nRequest: {}",
            outcome.state.label(),
            outcome.files.len(),
            outcome.change_count(),
            turn.prompt
        );
        if let Some(error) = &outcome.error {
            text.push_str(&format!("\n\n{error}"));
        }
        let mut diff = None;
        if let Some(&(file_index, edit_index, line)) = locations.get(selected) {
            let file = &outcome.files[file_index];
            let edit = &file.edits[edit_index];
            let (status, _) = self.inline_agent_file_status(file);
            let exact = self
                .file_buffer_index(Path::new(&file.path))
                .map(|index| self.buffer_manager[index].contents_snapshot() == edit.after.as_str())
                .unwrap_or_else(|| {
                    fs::metadata(&file.path)
                        .is_ok_and(|metadata| metadata.len() <= MAX_AGENT_IMAGE_BYTES as u64)
                        && fs::read_to_string(&file.path).is_ok_and(|text| text == edit.after)
                });
            if Path::new(&file.path).is_file()
                || self.file_buffer_index(Path::new(&file.path)).is_some()
            {
                self.execute_with_tracking(
                    &Action::OpenLocation(
                        plugin::PluginLocation {
                            path: file.path.clone(),
                            line: if exact { line } else { 0 },
                            column: 0,
                            column_encoding: plugin::LocationColumnEncoding::Utf8Byte,
                        },
                        plugin::OpenLocationTarget::Current,
                    ),
                    frame,
                    runtime,
                    false,
                )
                .await?;
                self.panel_manager.focus_editor();
            }
            text.push_str(&format!(
                "\n\nFile {} of {} · {status}\n{}:{}\nChange {} of {}{}",
                file_index + 1,
                outcome.files.len(),
                file.path,
                line + 1,
                selected + 1,
                locations.len(),
                if exact {
                    ""
                } else {
                    "\nHistorical diff; the current file differs or is unavailable."
                }
            ));
            diff = Some((file, edit));
        } else {
            text.push_str("\n\nNo editor writes were applied.");
        }
        if !outcome.answer.is_empty() {
            text.push_str(&format!("\n\n{}", outcome.answer));
        }
        let mut hover = HoverInfo::new(self, text, HoverInfoFormat::Markdown, Vec::new())
            .with_label("Agent changes")
            .with_shortcut('H', "history", Action::OpenInlineHistory);
        if let Some((file, edit)) = diff {
            hover = hover.with_diff(&file.path, &edit.before, &edit.after, "after Agent edit");
        }
        if locations.len() > 1 {
            hover = hover
                .with_shortcut(
                    '[',
                    "previous change",
                    action((selected + locations.len() - 1) % locations.len()),
                )
                .with_shortcut(']', "next change", action((selected + 1) % locations.len()));
        }
        if outcome.files.len() > 1 {
            let current_file = locations[selected].0;
            let previous_file = (current_file + outcome.files.len() - 1) % outcome.files.len();
            let next_file = (current_file + 1) % outcome.files.len();
            if let (Some(previous), Some(next)) = (
                locations
                    .iter()
                    .position(|location| location.0 == previous_file),
                locations
                    .iter()
                    .position(|location| location.0 == next_file),
            ) {
                hover = hover
                    .with_shortcut('F', "previous file", action(previous))
                    .with_shortcut('f', "next file", action(next));
            }
        }
        self.current_dialog = Some(Box::new(hover));
        self.render(frame)
    }
}
