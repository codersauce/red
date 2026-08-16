//! History ownership, conservative source resolution, and browser coordination.

use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};

use super::inline_comments::InlineCommentOrigin;
use super::*;
use crate::inline_history::{
    HistoryAction, InlineConversation, InlineHistoryTurn, InlineLocation, InlineSourceState,
};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum HistoryKey {
    Turn(String),
    Draft(String),
}

impl HistoryKey {
    fn request(&self) -> Option<&str> {
        match self {
            Self::Turn(request) => Some(request),
            Self::Draft(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct HistoryRow {
    pub(super) group: String,
    pub(super) key: HistoryKey,
    pub(super) label: String,
    pub(super) running: bool,
}

#[derive(Debug)]
pub(super) struct HistoryBrowser {
    origin: JumpEntry,
    viewport: (usize, usize, usize),
    active_comment: Option<uuid::Uuid>,
    file: Option<String>,
    workspace: bool,
    query: String,
    searching: bool,
    selected: Option<HistoryKey>,
    expanded: HashSet<String>,
    view: usize,
    scroll: usize,
    confirm_forget: bool,
    dirty: bool,
    refreshed_at: Instant,
    animation_started: Instant,
}

fn history_location_label(location: &InlineLocation) -> String {
    format!(
        "{}:{}–{}",
        location.file,
        location.range.start.line + 1,
        location.range.end.line + usize::from(location.range.end.character > 0)
    )
}

fn history_match(matcher: &SkimMatcherV2, query: &str, text: &str) -> Option<i64> {
    if query.is_empty() {
        Some(0)
    } else {
        matcher.fuzzy_match(text, query)
    }
}

impl Editor {
    /// Recall submitted prompts from this workspace, newest first, without
    /// maintaining a second copy outside recoverable inline conversations.
    pub(crate) fn inline_prompt_history(&self) -> Vec<String> {
        let cwd = get_workspace_path();
        let mut turns = self
            .inline_history
            .conversations
            .iter()
            .rev()
            .filter(|conversation| Path::new(&conversation.cwd) == cwd)
            .flat_map(|conversation| conversation.turns.iter().rev())
            .collect::<Vec<_>>();
        turns.sort_by_key(|turn| std::cmp::Reverse(turn.created_at_ms));
        let mut seen = HashSet::new();
        turns
            .into_iter()
            .map(|turn| turn.prompt.as_str())
            .filter(|prompt| !prompt.trim().is_empty() && seen.insert(*prompt))
            .take(50)
            .map(str::to_owned)
            .collect()
    }

    fn history_draft_row(&self, group: &str, matcher: &SkimMatcherV2) -> Option<(HistoryRow, i64)> {
        let browser = self.inline_history_browser.as_ref()?;
        let job = self.inline_jobs.get(group)?;
        let InlineAssistPopupState::Prompt { initial, .. } = &job.state else {
            return None;
        };
        if initial.trim().is_empty() {
            return None;
        }
        if !browser.workspace && browser.file.as_deref() != Some(job.location.file.as_str()) {
            return None;
        }
        let location = history_location_label(&job.location);
        let snippet = crate::ui::first_prompt_line(&job.session.expected_text);
        let score = history_match(
            matcher,
            &browser.query,
            &format!("{location} {initial} {snippet}"),
        )?;
        let prompt = if initial.is_empty() {
            "Untitled inline draft".into()
        } else {
            crate::ui::first_prompt_line(initial)
        };
        Some((
            HistoryRow {
                group: group.into(),
                key: HistoryKey::Draft(group.into()),
                label: format!("[draft] {prompt}\n{location} · {snippet}"),
                running: false,
            },
            score,
        ))
    }

    pub(super) fn history_rows(&self) -> Vec<HistoryRow> {
        let Some(browser) = &self.inline_history_browser else {
            return Vec::new();
        };
        let matcher = SkimMatcherV2::default();
        let mut groups = Vec::new();
        for conversation in self.inline_history.conversations.iter().rev() {
            let Some(latest) = conversation.turns.last() else {
                continue;
            };
            let mut rows = Vec::new();
            let mut score = i64::MIN;
            if let Some((row, draft_score)) = self.history_draft_row(&conversation.id, &matcher) {
                score = draft_score;
                rows.push(row);
            }
            let expanded = browser.expanded.contains(&conversation.id) || !browser.query.is_empty();
            for (index, turn) in conversation.turns.iter().enumerate().rev() {
                if (!browser.workspace
                    && browser.file.as_deref() != Some(turn.location.file.as_str()))
                    || (!expanded && index + 1 != conversation.turns.len())
                {
                    continue;
                }
                let location = history_location_label(&turn.location);
                let snippet = crate::ui::first_prompt_line(&turn.before);
                let Some(turn_score) = history_match(
                    &matcher,
                    &browser.query,
                    &format!(
                        "{location} {} {} {snippet}",
                        turn.prompt,
                        turn.answer_text()
                    ),
                ) else {
                    continue;
                };
                score = score.max(turn_score);
                let marker = if index + 1 < conversation.turns.len() {
                    "  ↳ "
                } else if conversation.turns.len() > 1 {
                    "▸ "
                } else {
                    ""
                };
                let resolved = if conversation.resolved {
                    "[resolved] "
                } else {
                    ""
                };
                let source_state = self
                    .resolve_history_turn(turn)
                    .map_or(InlineSourceState::Detached, |(_, _, state)| state);
                let status = if turn.state == InlineTurnState::Pending {
                    "running"
                } else {
                    turn.status()
                };
                let name = Path::new(&turn.location.file)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(&turn.location.file);
                rows.push(HistoryRow {
                    group: conversation.id.clone(),
                    key: HistoryKey::Turn(turn.request_id.clone()),
                    label: format!(
                        "{marker}{resolved}{}\n{name}:{}–{} · {status} · {} · {snippet}",
                        crate::ui::first_prompt_line(&turn.prompt),
                        turn.location.range.start.line + 1,
                        turn.location.range.end.line
                            + usize::from(turn.location.range.end.character > 0),
                        source_state.label()
                    ),
                    running: turn.state == InlineTurnState::Pending,
                });
            }
            if rows.is_empty() {
                continue;
            }
            let rank = match latest.state {
                InlineTurnState::Pending => 0,
                InlineTurnState::Ready => 1,
                _ if self.has_parked_inline_draft(&conversation.id) => 2,
                _ => 3,
            };
            groups.push((
                rank,
                std::cmp::Reverse(score),
                std::cmp::Reverse(latest.created_at_ms),
                rows,
            ));
        }
        for group in self.inline_jobs.keys() {
            if self
                .inline_history
                .conversations
                .iter()
                .any(|conversation| &conversation.id == group)
            {
                continue;
            }
            if let Some((row, score)) = self.history_draft_row(group, &matcher) {
                groups.push((2, std::cmp::Reverse(score), std::cmp::Reverse(0), vec![row]));
            }
        }
        groups.sort_by_key(|(rank, score, time, _)| (*rank, *score, *time));
        groups
            .into_iter()
            .flat_map(|(_, _, _, rows)| rows)
            .collect()
    }

    pub(super) fn mark_inline_history_dirty(&mut self) {
        if let Some(browser) = &mut self.inline_history_browser {
            browser.dirty = true;
        }
    }

    pub(super) async fn refresh_live_inline_history(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let should_refresh = self.inline_history_browser.as_ref().is_some_and(|browser| {
            browser.dirty
                && browser.refreshed_at.elapsed()
                    >= Duration::from_millis(crate::ui::SPINNER_FRAME_INTERVAL_MS)
        }) && self
            .current_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.is_inline_history());
        if should_refresh {
            self.refresh_inline_history_browser(buffer, runtime, false)
                .await?;
        }
        Ok(())
    }

    pub(super) async fn open_inline_history(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        self.park_inline_assist();
        self.refresh_inline_history_paths();
        self.complete_ready_inline_answers();
        self.sync_inline_activity();
        if self.inline_history_browser.is_none() {
            let now = Instant::now();
            self.inline_history_browser = Some(HistoryBrowser {
                origin: self.current_jump_entry(),
                viewport: (self.vtop, self.vleft, self.skipcol),
                active_comment: self.active_inline_comment,
                file: self.current_buffer().file.clone(),
                workspace: true,
                query: String::new(),
                searching: false,
                selected: None,
                expanded: HashSet::new(),
                view: 0,
                scroll: 0,
                confirm_forget: false,
                dirty: false,
                refreshed_at: now,
                animation_started: now,
            });
        }
        self.refresh_inline_history_browser(buffer, runtime, true)
            .await
    }

    pub(super) async fn open_inline_history_request(
        &mut self,
        group: &str,
        request: &str,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        self.open_inline_history(buffer, runtime).await?;
        if let Some(browser) = &mut self.inline_history_browser {
            browser.workspace = true;
            browser.query.clear();
            browser.searching = false;
            browser.expanded.insert(group.to_owned());
            browser.selected = Some(HistoryKey::Turn(request.to_owned()));
            browser.scroll = 0;
        }
        self.refresh_inline_history_browser(buffer, runtime, true)
            .await
    }

    async fn refresh_inline_history_browser(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
        preview_source: bool,
    ) -> anyhow::Result<()> {
        self.clear_history_preview();
        let rows = self.history_rows();
        let selected = self
            .inline_history_browser
            .as_ref()
            .and_then(|browser| browser.selected.as_ref())
            .and_then(|key| rows.iter().position(|row| &row.key == key))
            .unwrap_or(0);
        let selected_key = rows.get(selected).map(|row| row.key.clone());
        if let Some(browser) = &mut self.inline_history_browser {
            browser.selected = selected_key.clone();
        }
        let turn = selected_key
            .as_ref()
            .and_then(HistoryKey::request)
            .and_then(|request| self.inline_history.turn(request))
            .cloned();
        let draft = selected_key.as_ref().and_then(|key| match key {
            HistoryKey::Draft(group) => self.inline_jobs.get(group).and_then(|job| {
                let InlineAssistPopupState::Prompt { initial, .. } = &job.state else {
                    return None;
                };
                Some((
                    job.location.clone(),
                    job.session.expected_text.clone(),
                    initial.clone(),
                ))
            }),
            HistoryKey::Turn(_) => None,
        });
        let file = turn
            .as_ref()
            .map(|turn| turn.location.file.as_str())
            .or_else(|| {
                draft
                    .as_ref()
                    .map(|(location, _, _)| location.file.as_str())
            });
        if preview_source {
            if let Some(file) = file.filter(|file| !file.is_empty() && Path::new(file).is_file()) {
                if !self
                    .buffer_manager
                    .iter()
                    .any(|buffer| buffer.file.as_deref() == Some(file))
                {
                    self.execute_with_tracking(
                        &Action::OpenFile(file.into()),
                        buffer,
                        runtime,
                        false,
                    )
                    .await?;
                }
            }
        }
        let mut detail =
            "No inline conversations here yet. Use Space i to ask a question.".to_string();
        let mut can_restore = false;
        if let Some(turn) = turn {
            let resolved = self.resolve_history_turn(&turn);
            let state = resolved
                .as_ref()
                .map_or(InlineSourceState::Detached, |(_, _, state)| *state);
            let preview_location = resolved
                .filter(|(_, _, state)| *state != InlineSourceState::Detached)
                .or_else(|| {
                    (0..turn.comment_locations.len()).find_map(|index| {
                        self.resolve_history_comment(&turn, index)
                            .filter(|(_, _, state)| *state != InlineSourceState::Detached)
                    })
                });
            if let Some((index, range, _)) = preview_location {
                if preview_source {
                    self.preview_inline_history_location(index, range, buffer)
                        .await?;
                }
                if index == self.buffer_manager.active_index() {
                    if let Some(result) = &turn.result {
                        let comments = result
                            .comments
                            .iter()
                            .enumerate()
                            .filter_map(|(comment_index, comment)| {
                                let (comment_buffer, range, state) =
                                    self.resolve_history_comment(&turn, comment_index)?;
                                if comment_buffer != index || state == InlineSourceState::Detached {
                                    return None;
                                }
                                let last = range.end.line.saturating_sub(usize::from(
                                    range.end.character == 0 && range.end.line > range.start.line,
                                ));
                                let mut value = Self::make_inline_comment_in_buffer(
                                    &self.buffer_manager[index],
                                    range.start.line,
                                    last,
                                    comment.message.clone(),
                                    InlineCommentOrigin::HistoryPreview {
                                        request_id: turn.request_id.clone(),
                                        comment_index,
                                    },
                                );
                                value.stale = state == InlineSourceState::Changed;
                                Some(value)
                            })
                            .collect::<Vec<_>>();
                        self.active_inline_comment = comments.first().map(|comment| comment.id);
                        self.inline_comments.extend(comments);
                    }
                }
            }
            can_restore = turn.state == InlineTurnState::Completed
                && (turn.change_summary.is_some()
                    || turn
                        .result
                        .as_ref()
                        .is_some_and(|result| !result.comments.is_empty()));
            let view = self
                .inline_history_browser
                .as_ref()
                .map_or(0, |browser| browser.view);
            let header = format!(
                "{} · {}\n{}\nSource: {}",
                state.label(),
                turn.status(),
                history_location_label(&turn.location),
                crate::ui::first_prompt_line(&turn.before)
            );
            let context = if turn.context_reads.is_empty() {
                String::new()
            } else {
                format!("\n\nContext read:\n- {}", turn.context_reads.join("\n- "))
            };
            detail = match view {
                4 if turn.has_code_change() => format!(
                    "{header}\n\n{}\n\n{}",
                    self.inline_change_label(&turn),
                    turn.change_diff()
                ),
                4 => format!("{header}\n\nNo applied code changes in this turn."),
                1 => format!(
                    "{header}\n\nREVIEWED SOURCE · read-only\n{}",
                    turn.reviewed()
                ),
                2 => format!("{header}\n\nBEFORE EDIT · read-only\n{}", turn.before),
                3 => {
                    let current = self
                        .resolve_history_turn(&turn)
                        .map(|(index, range, _)| self.buffer_manager[index].text_in_range(range))
                        .unwrap_or_else(|| "[source detached]".into());
                    format!(
                        "{header}\n\nREVIEWED\n{}\n\nCURRENT\n{current}",
                        turn.reviewed()
                    )
                }
                _ => format!(
                    "{header}\n\nYou: {}\n\nAssistant: {}{}{context}",
                    turn.prompt,
                    turn.answer_text(),
                    turn.error
                        .as_ref()
                        .map(|error| format!("\n\nOutcome: {error}"))
                        .unwrap_or_default()
                ),
            };
        } else if let Some((location, source, prompt)) = draft {
            let resolved = self.resolve_history_source(&location, &source, true);
            let state = resolved
                .as_ref()
                .map_or(InlineSourceState::Detached, |(_, _, state)| *state);
            if preview_source {
                if let Some((index, range, _)) =
                    resolved.filter(|(_, _, state)| *state != InlineSourceState::Detached)
                {
                    self.preview_inline_history_location(index, range, buffer)
                        .await?;
                }
            }
            detail = format!(
                "draft · {}\n{}\n\nUnsent prompt:\n{prompt}\n\nTARGET · read-only\n{source}",
                state.label(),
                history_location_label(&location)
            );
        }
        let Some(browser) = &mut self.inline_history_browser else {
            return Ok(());
        };
        browser.dirty = false;
        browser.refreshed_at = Instant::now();
        let title = format!(
            "Inline history · {} · {}",
            if browser.workspace {
                "workspace"
            } else {
                "current file"
            },
            ["conversation", "reviewed code", "before edit", "compare"][browser.view]
        );
        let (scroll, searching, query, confirm_forget, animation_started) = (
            browser.scroll,
            browser.searching,
            browser.query.clone(),
            browser.confirm_forget,
            browser.animation_started,
        );
        let panel = crate::ui::InlineHistoryPanel::new(
            self,
            rows.into_iter()
                .map(|row| crate::ui::InlineHistoryRow {
                    text: row.label,
                    running: row.running,
                })
                .collect(),
            selected,
            detail,
            scroll,
            searching,
            query,
            confirm_forget,
            title,
            can_restore,
            animation_started,
        );
        self.current_dialog = Some(Box::new(panel));
        self.layout_cache.borrow_mut().clear();
        self.render(buffer)
    }

    async fn preview_inline_history_location(
        &mut self,
        index: usize,
        range: TextRange,
        buffer: &mut RenderBuffer,
    ) -> anyhow::Result<()> {
        if self.buffer_manager.active_index() != index {
            self.set_current_buffer(buffer, index).await?;
        }
        self.move_to_text_position(range.start);
        self.vtop = range.start.line.saturating_sub(1);
        self.cy = range.start.line.saturating_sub(self.vtop);
        self.skipcol = 0;
        self.sync_to_window();
        Ok(())
    }

    pub(super) async fn close_inline_history(
        &mut self,
        jump: bool,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let Some(browser) = self.inline_history_browser.take() else {
            return Ok(());
        };
        self.clear_history_preview();
        self.current_dialog = None;
        self.active_inline_comment = browser.active_comment;
        if jump {
            self.save_to_history(browser.origin);
        } else {
            self.jump_to_entry(&browser.origin, buffer, runtime).await?;
            let line = self.buffer_line();
            self.vtop = browser.viewport.0.min(line);
            self.cy = line.saturating_sub(self.vtop);
            self.vleft = browser.viewport.1;
            self.skipcol = browser.viewport.2;
            self.sync_to_window();
        }
        self.render(buffer)
    }

    pub(super) fn recovered_inline_context(&self, group: &str) -> String {
        let Some(conversation) = self
            .inline_history
            .conversations
            .iter()
            .find(|conversation| conversation.id == group)
        else {
            return String::new();
        };
        let mut items = conversation
            .turns
            .iter()
            .rev()
            .filter(|turn| turn.state != InlineTurnState::Pending)
            .take(4)
            .collect::<Vec<_>>();
        items.reverse();
        let mut context = String::new();
        for turn in items {
            let text = format!(
                "\nYou: {}\nAssistant: {}\nOutcome: {}\n",
                turn.prompt,
                turn.answer_text(),
                turn.status()
            );
            if context.len() + text.len() > 16 * 1024 {
                continue;
            }
            context.push_str(&text);
        }
        if context.is_empty() {
            context
        } else {
            format!("\n\n<recovered_inline_history>\nEarlier discussion, not current source. Re-evaluate against the current target.\n{context}</recovered_inline_history>")
        }
    }

    pub(super) fn inline_handoff_prompt(&self, group: &str) -> Option<String> {
        let conversation = self
            .inline_history
            .conversations
            .iter()
            .find(|conversation| conversation.id == group)?;
        let latest = conversation.turns.last()?;
        let range = latest.location.range;
        Some(format!(
            "Continue this inline-assist discussion in the project. Carry out the latest user request below, using the earlier answers as context. Read current files through Red before editing; the discussion may describe older source.\n\nLocation: {}:{}–{}\n\nLatest user request:\n{}\n{}",
            latest.location.file,
            range.start.line + 1,
            range.end.line + usize::from(range.end.character > 0),
            latest.prompt,
            self.recovered_inline_context(group),
        ))
    }

    pub(super) async fn handle_inline_history_action(
        &mut self,
        action: &HistoryAction,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        if let HistoryAction::Export(path) = action {
            let contents = serde_json::to_vec_pretty(&self.inline_history)?;
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options
                .open(path)
                .and_then(|mut file| file.write_all(&contents))
            {
                Ok(()) => {
                    self.set_legacy_message(Some(format!("exported inline history to {path}")))
                }
                Err(error) => self
                    .set_legacy_message(Some(format!("could not export inline history: {error}"))),
            }
            return self.render(buffer);
        }
        let rows = self.history_rows();
        let Some(browser) = &self.inline_history_browser else {
            return Ok(());
        };
        let selected = browser
            .selected
            .as_ref()
            .and_then(|key| rows.iter().position(|row| &row.key == key))
            .unwrap_or(0);
        let selected_row = rows.get(selected).cloned();
        if matches!(action, HistoryAction::Open) {
            if let Some(row) = selected_row {
                if let Some(request) = row.key.request() {
                    if self
                        .inline_history
                        .turn(request)
                        .is_some_and(|turn| turn.has_code_change())
                    {
                        return self.view_inline_changes(request, 0, buffer, runtime).await;
                    }
                    let historical = self
                        .inline_history
                        .conversations
                        .iter()
                        .find(|conversation| conversation.id == row.group)
                        .and_then(|conversation| conversation.turns.last())
                        .is_some_and(|latest| latest.request_id != request);
                    if historical {
                        if let Some(turn) = self.inline_history.turn(request) {
                            self.current_dialog = Some(Box::new(
                                HoverInfo::new(
                                    self,
                                    turn.answer_text(),
                                    HoverInfoFormat::Plaintext,
                                    Vec::new(),
                                )
                                .with_label("Historical inline answer")
                                .with_close_action(Action::OpenInlineHistory),
                            ));
                            return self.render(buffer);
                        }
                    }
                }
                return self.open_inline_job(&row.group, buffer, runtime).await;
            }
            return Ok(());
        }
        if matches!(action, HistoryAction::ShowAnnotations) {
            if let Some(request) = selected_row
                .as_ref()
                .and_then(|row| row.key.request())
                .map(str::to_string)
            {
                if self
                    .show_inline_history_annotations(&request, buffer, runtime)
                    .await?
                {
                    return self.close_inline_history(true, buffer, runtime).await;
                }
            } else {
                self.set_legacy_message(Some("this item has no completed annotations".into()));
            }
            return self
                .refresh_inline_history_browser(buffer, runtime, false)
                .await;
        }
        if matches!(action, HistoryAction::Close | HistoryAction::Jump) {
            return self
                .close_inline_history(matches!(action, HistoryAction::Jump), buffer, runtime)
                .await;
        }
        if matches!(action, HistoryAction::Continue | HistoryAction::Recheck) {
            if let Some(row) = selected_row {
                let group = row.group;
                let Some(request) = row.key.request() else {
                    return self.open_inline_job(&group, buffer, runtime).await;
                };
                let latest_is_unfinished = self
                    .inline_history
                    .conversations
                    .iter()
                    .find(|conversation| conversation.id == group)
                    .and_then(|conversation| conversation.turns.last())
                    .is_some_and(|turn| {
                        matches!(
                            turn.state,
                            InlineTurnState::Pending | InlineTurnState::Ready
                        )
                    });
                if self.has_parked_inline_draft(&group) || latest_is_unfinished {
                    return self.open_inline_job(&group, buffer, runtime).await;
                }
                let turn = self.inline_history.turn(request).cloned();
                if let Some(turn) = turn {
                    if let Some((index, range, state)) = self.resolve_history_turn(&turn) {
                        if state != InlineSourceState::Detached {
                            self.close_inline_history(true, buffer, runtime).await?;
                            self.release_parked_inline_job(&group);
                            if self.buffer_manager.active_index() != index {
                                self.set_current_buffer(buffer, index).await?;
                            }
                            let Some(window_id) = self.window_manager.active_stable_window_id()
                            else {
                                return Ok(());
                            };
                            let scope = format!(
                                "lines {}–{} · continued",
                                range.start.line + 1,
                                range.end.line + usize::from(range.end.character > 0)
                            );
                            self.inline_assist = Some(InlineAssistSession {
                                allow_expansion: turn.allow_expansion,
                                buffer_id: self.current_buffer().id(),
                                window_id,
                                expected_revision: self.current_buffer().revision(),
                                range,
                                expected_text: self.current_buffer().text_in_range(range),
                                scope: scope.clone(),
                                request_id: None,
                                session_id: None,
                                transaction_id: None,
                                annotation_group_id: group,
                                has_result: false,
                                result_request_id: None,
                            });
                            let initial = if matches!(action, HistoryAction::Recheck) {
                                format!(
                                    "Recheck this earlier request against the current code: {}",
                                    turn.prompt
                                )
                            } else {
                                String::new()
                            };
                            self.current_dialog = Some(Box::new(self.inline_assist_popup(
                                scope,
                                InlineAssistPopupState::Prompt {
                                    initial,
                                    refining: false,
                                },
                            )));
                            return self.render(buffer);
                        }
                    }
                }
            }
            self.set_legacy_message(Some(
                "source is detached; select the intended code and start a new inline request"
                    .into(),
            ));
            return self.render(buffer);
        }
        let Some(browser) = &mut self.inline_history_browser else {
            return Ok(());
        };
        match action {
            HistoryAction::Next | HistoryAction::Previous if !rows.is_empty() => {
                let next = if matches!(action, HistoryAction::Next) {
                    (selected + 1) % rows.len()
                } else {
                    (selected + rows.len() - 1) % rows.len()
                };
                browser.selected = Some(rows[next].key.clone());
                browser.scroll = 0;
            }
            HistoryAction::Select(index) => {
                if let Some(row) = rows.get(*index) {
                    browser.selected = Some(row.key.clone());
                    browser.scroll = 0;
                }
            }
            HistoryAction::Expand | HistoryAction::Collapse => {
                if let Some(row) = &selected_row {
                    let group = &row.group;
                    if matches!(action, HistoryAction::Expand) {
                        browser.expanded.insert(group.clone());
                    } else {
                        browser.expanded.remove(group);
                    }
                }
            }
            HistoryAction::ToggleWorkspace => {
                browser.workspace = !browser.workspace;
                browser.selected = None;
            }
            HistoryAction::Search => browser.searching = true,
            HistoryAction::Query(text) => {
                if browser.query.len() + text.len() <= 1024 {
                    browser
                        .query
                        .extend(text.chars().filter(|ch| !ch.is_control()));
                }
                browser.selected = None;
            }
            HistoryAction::Backspace => {
                browser.query.pop();
                browser.selected = None;
            }
            HistoryAction::DeletePreviousWord => {
                crate::unicode_utils::delete_last_word(&mut browser.query);
                browser.selected = None;
            }
            HistoryAction::EndSearch => browser.searching = false,
            HistoryAction::ClearSearch => {
                browser.searching = false;
                browser.query.clear();
                browser.selected = None;
            }
            HistoryAction::ScrollDown => browser.scroll = browser.scroll.saturating_add(4),
            HistoryAction::ScrollUp => browser.scroll = browser.scroll.saturating_sub(4),
            HistoryAction::CycleView => {
                browser.view = (browser.view + 1) % 5;
                browser.scroll = 0;
            }
            HistoryAction::Forget => browser.confirm_forget = !browser.confirm_forget,
            HistoryAction::ConfirmForget => {
                browser.confirm_forget = false;
                if let Some(row) = &selected_row {
                    let group = &row.group;
                    self.inline_history
                        .conversations
                        .retain(|conversation| conversation.id != *group);
                    self.inline_history.remove_unused_sources();
                    self.remove_inline_comment_group(group);
                    self.release_parked_inline_job(group);
                }
            }
            HistoryAction::Resolve => {
                if let Some(row) = &selected_row {
                    let group = &row.group;
                    if let Some(conversation) = self
                        .inline_history
                        .conversations
                        .iter_mut()
                        .find(|conversation| conversation.id == *group)
                    {
                        conversation.resolved = !conversation.resolved;
                    }
                    self.remove_inline_comment_group(group);
                    self.restore_inline_history_comments();
                    self.sync_inline_activity();
                }
            }
            _ => {}
        }
        let preview_source = matches!(
            action,
            HistoryAction::Next
                | HistoryAction::Previous
                | HistoryAction::Select(_)
                | HistoryAction::ToggleWorkspace
                | HistoryAction::Query(_)
                | HistoryAction::Backspace
                | HistoryAction::ClearSearch
                | HistoryAction::Expand
                | HistoryAction::Collapse
        );
        self.refresh_inline_history_browser(buffer, runtime, preview_source)
            .await
    }

    pub(super) fn inline_annotations_request(
        &self,
        group: &str,
        preferred: Option<&str>,
    ) -> Option<String> {
        let conversation = self
            .inline_history
            .conversations
            .iter()
            .find(|conversation| conversation.id == group)?;
        let has_annotations = |turn: &&InlineHistoryTurn| {
            turn.state == InlineTurnState::Completed
                && (turn.change_summary.is_some()
                    || turn
                        .result
                        .as_ref()
                        .is_some_and(|result| !result.comments.is_empty()))
        };
        preferred
            .or(conversation.visible_request.as_deref())
            .and_then(|request| {
                conversation
                    .turns
                    .iter()
                    .find(|turn| turn.request_id == request)
                    .filter(has_annotations)
            })
            .or_else(|| conversation.turns.iter().rev().find(has_annotations))
            .map(|turn| turn.request_id.clone())
    }

    /// Make a retained turn visible without replaying its source edit or changing
    /// its historical outcome. Detached comments remain available in history.
    pub(super) async fn show_inline_history_annotations(
        &mut self,
        request: &str,
        frame: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<bool> {
        let Some((group, turn)) =
            self.inline_history
                .conversations
                .iter()
                .find_map(|conversation| {
                    conversation
                        .turns
                        .iter()
                        .find(|turn| {
                            turn.request_id == request && turn.state == InlineTurnState::Completed
                        })
                        .map(|turn| (conversation.id.clone(), turn.clone()))
                })
        else {
            self.set_legacy_message(Some("this item has no completed annotations".into()));
            return Ok(false);
        };
        if turn.change_summary.is_some() {
            if let Some(conversation) = self
                .inline_history
                .conversations
                .iter_mut()
                .find(|conversation| conversation.id == group)
            {
                conversation.resolved = false;
                conversation.visible_request = Some(request.into());
                if let Some(summary) = conversation
                    .turns
                    .iter_mut()
                    .find(|turn| turn.request_id == request)
                    .and_then(|turn| turn.change_summary.as_mut())
                {
                    summary.hidden = false;
                }
            }
            self.sync_inline_change_summaries();
            if turn
                .result
                .as_ref()
                .is_none_or(|result| result.comments.is_empty())
            {
                self.set_legacy_message(Some(
                    "change summary restored · source unchanged by this action".into(),
                ));
                return Ok(true);
            }
        }
        let Some(result) = turn
            .result
            .as_ref()
            .filter(|result| !result.comments.is_empty())
        else {
            self.set_legacy_message(Some("this result has no annotations".into()));
            return Ok(false);
        };
        if self.resolve_history_turn(&turn).is_none() && Path::new(&turn.location.file).is_file() {
            self.execute_with_tracking(
                &Action::OpenFile(turn.location.file.clone()),
                frame,
                runtime,
                false,
            )
            .await?;
        }
        let annotations = result
            .comments
            .iter()
            .enumerate()
            .filter_map(|(comment_index, comment)| {
                let (index, range, state) = self.resolve_history_comment(&turn, comment_index)?;
                if state == InlineSourceState::Detached {
                    return None;
                }
                let last = range.end.line.saturating_sub(usize::from(
                    range.end.character == 0 && range.end.line > range.start.line,
                ));
                let mut annotation = Self::make_inline_comment_in_buffer(
                    &self.buffer_manager[index],
                    range.start.line,
                    last,
                    comment.message.clone(),
                    InlineCommentOrigin::Assist {
                        group_id: group.clone(),
                        session_id: turn.session_id.clone().unwrap_or_default(),
                        request_id: request.into(),
                        comment_index,
                    },
                );
                if let Some(fingerprint) = turn.comment_fingerprints.get(comment_index) {
                    annotation.expected_fingerprint = *fingerprint;
                }
                annotation.stale = state != InlineSourceState::Unchanged;
                Some(annotation)
            })
            .collect::<Vec<_>>();
        if annotations.is_empty() {
            self.set_legacy_message(Some(
                "annotations retained, but their source is detached; recheck against current code"
                    .into(),
            ));
            return Ok(false);
        }
        let mut counts = HashMap::new();
        for annotation in &annotations {
            *counts.entry(annotation.anchor.buffer_id).or_insert(0) += 1;
        }
        for (buffer_id, count) in counts {
            if let Err(error) =
                self.check_inline_comment_capacity_for_buffer(buffer_id, &group, count)
            {
                self.set_legacy_message(Some(error.to_string()));
                return Ok(false);
            }
        }
        let restored = annotations.len();
        let selected = annotations.first().map(|annotation| annotation.id);
        if let Some(conversation) = self
            .inline_history
            .conversations
            .iter_mut()
            .find(|conversation| conversation.id == group)
        {
            conversation.resolved = false;
            conversation.visible_request = Some(request.into());
            if let Some(turn) = conversation
                .turns
                .iter_mut()
                .find(|turn| turn.request_id == request)
            {
                turn.hidden_comments.clear();
            }
        }
        self.remove_inline_comment_group(&group);
        self.inline_comments.extend(annotations);
        self.active_inline_comment = selected;
        if let Some(browser) = &mut self.inline_history_browser {
            browser.active_comment = selected;
        }
        self.sync_inline_activity();
        self.set_legacy_message(Some(format!(
            "showing {restored}/{} retained annotation(s) · source unchanged by this action",
            result.comments.len()
        )));
        Ok(true)
    }

    pub(super) fn history_location(&self, range: TextRange) -> InlineLocation {
        Self::history_location_in_buffer(self.current_buffer(), range)
    }

    pub(super) fn history_location_in_buffer(buffer: &Buffer, range: TextRange) -> InlineLocation {
        let start_char = buffer.position_to_char_idx(range.start);
        let end_char = buffer.position_to_char_idx(range.end);
        InlineLocation {
            file: buffer.file.clone().unwrap_or_default(),
            range,
            start_char,
            end_char,
            detached: false,
            context_before: buffer.text_in_range(TextRange::new(
                buffer.char_idx_to_position(start_char.saturating_sub(128)),
                range.start,
            )),
            context_after: buffer.text_in_range(TextRange::new(
                range.end,
                buffer.char_idx_to_position(end_char.saturating_add(128)),
            )),
            buffer_id: Some(buffer.id()),
        }
    }

    pub(super) fn refresh_inline_history_paths(&mut self) {
        let files = self
            .buffer_manager
            .iter()
            .filter_map(|buffer| buffer.file.as_ref().map(|file| (buffer.id(), file.clone())))
            .collect::<HashMap<_, _>>();
        for job in self.inline_jobs.values_mut() {
            if let Some(file) = job.location.buffer_id.and_then(|id| files.get(&id)) {
                job.location.file.clone_from(file);
            }
        }
        for conversation in &mut self.inline_history.conversations {
            for turn in &mut conversation.turns {
                for location in turn.locations_mut() {
                    if let Some(file) = location.buffer_id.and_then(|id| files.get(&id)) {
                        location.file.clone_from(file);
                    }
                }
            }
            if let Some(turn) = conversation.turns.last() {
                conversation.file.clone_from(&turn.location.file);
            }
        }
    }

    pub(super) fn set_inline_history_transaction_applied(
        &mut self,
        transaction: &str,
        applied: bool,
    ) {
        for conversation in &mut self.inline_history.conversations {
            let latest = conversation
                .turns
                .iter()
                .rposition(|turn| turn.state == InlineTurnState::Completed);
            for (index, turn) in conversation.turns.iter_mut().enumerate() {
                if turn.transaction_id.as_deref() == Some(transaction) {
                    turn.disposition = if !applied {
                        InlineDisposition::Undone
                    } else if latest == Some(index) {
                        InlineDisposition::Kept
                    } else {
                        InlineDisposition::Superseded
                    };
                }
            }
        }
        self.mark_inline_history_dirty();
    }

    pub(super) fn rebind_inline_history_file(&mut self, file: &str) {
        if self
            .inline_history
            .conversations
            .iter()
            .flat_map(|conversation| &conversation.turns)
            .any(|turn| turn.location.buffer_id.is_none() && turn.location.file == file)
        {
            self.restore_inline_history_comments();
        }
    }

    pub(super) fn begin_inline_history_turn(
        &mut self,
        request: &str,
        prompt: &str,
        range: TextRange,
    ) -> anyhow::Result<()> {
        let before = self.current_buffer().text_in_range(range);
        self.inline_history.check_capacity(prompt, &before)?;
        let location = self.history_location(range);
        let group = self
            .inline_assist
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("inline assist is no longer active"))?
            .annotation_group_id
            .clone();
        let turn = InlineHistoryTurn {
            expanded_location: None,
            allow_expansion: self
                .inline_assist
                .as_ref()
                .is_some_and(|session| session.allow_expansion),
            context_reads: Vec::new(),
            request_id: request.to_string(),
            created_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            prompt: prompt.to_string(),
            answer: String::new(),
            answer_truncated: false,
            before,
            original_range: range,
            location,
            state: InlineTurnState::Pending,
            disposition: InlineDisposition::Kept,
            result: None,
            error: None,
            transaction_id: None,
            change_summary: None,
            session_id: None,
            hidden_comments: Vec::new(),
            comment_fingerprints: Vec::new(),
            comment_locations: Vec::new(),
            comment_source_ids: Vec::new(),
        };
        if let Some(conversation) = self
            .inline_history
            .conversations
            .iter_mut()
            .find(|conversation| conversation.id == group)
        {
            conversation.turns.push(turn);
            conversation.resolved = false;
        } else {
            self.inline_history.conversations.push(InlineConversation {
                id: group,
                cwd: get_workspace_path().to_string_lossy().into_owned(),
                file: turn.location.file.clone(),
                turns: vec![turn],
                resolved: false,
                visible_request: None,
            });
        }
        Ok(())
    }

    pub(super) fn complete_inline_history_turn(
        &mut self,
        request: &str,
        session: &str,
        result: &InlineAssistResult,
        range: TextRange,
    ) {
        let location = self.history_location(range);
        let transaction = self
            .inline_assist
            .as_ref()
            .and_then(|assist| assist.transaction_id.clone());
        self.complete_inline_history_turn_at(request, session, result, location, transaction);
    }

    pub(super) fn complete_inline_history_turn_at(
        &mut self,
        request: &str,
        session: &str,
        result: &InlineAssistResult,
        location: InlineLocation,
        transaction: Option<String>,
    ) {
        let fingerprints = self
            .inline_comments
            .iter()
            .filter_map(|comment| match &comment.origin {
                InlineCommentOrigin::Assist { request_id, .. } if request_id == request => {
                    Some(comment.expected_fingerprint)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let comment_records = self
            .inline_comments
            .iter()
            .filter_map(|comment| match &comment.origin {
                InlineCommentOrigin::Assist { request_id, .. } if request_id == request => {
                    let buffer = self
                        .buffer_manager
                        .iter()
                        .find(|buffer| buffer.id() == comment.anchor.buffer_id)?;
                    let (start, end) = comment.lines(buffer);
                    let location = Self::history_location_in_buffer(
                        buffer,
                        TextRange::new(
                            TextPosition::new(start, 0),
                            TextPosition::new(end.saturating_add(1), 0),
                        ),
                    );
                    let source = (buffer.line_range_byte_len(start, end.saturating_add(1))
                        <= 256 * 1024)
                        .then(|| buffer.line_range_contents(start, end.saturating_add(1)));
                    Some((location, source))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let (comment_locations, comment_source_ids): (Vec<_>, Vec<_>) = comment_records
            .into_iter()
            .map(|(location, source)| {
                (
                    location,
                    source.and_then(|source| self.inline_history.retain_source(source)),
                )
            })
            .unzip();
        for conversation in &mut self.inline_history.conversations {
            if conversation
                .turns
                .iter()
                .any(|turn| turn.request_id == request)
            {
                if result.needs_agent.is_none() {
                    conversation.visible_request = None;
                }
                for turn in &mut conversation.turns {
                    if turn.request_id == request {
                        turn.location = location.clone();
                        turn.state = InlineTurnState::Completed;
                        turn.expanded_location = None;
                        turn.error = None;
                        turn.result = Some(result.clone());
                        turn.session_id = Some(session.to_string());
                        turn.transaction_id = if result
                            .replacement
                            .as_deref()
                            .is_some_and(|text| text != turn.before)
                        {
                            transaction.clone()
                        } else {
                            None
                        };
                        turn.comment_fingerprints.clone_from(&fingerprints);
                        turn.comment_locations.clone_from(&comment_locations);
                        turn.comment_source_ids.clone_from(&comment_source_ids);
                        turn.ensure_change_summary();
                    } else if result.needs_agent.is_none()
                        && turn.state == InlineTurnState::Completed
                        && turn.disposition == InlineDisposition::Kept
                    {
                        turn.disposition = InlineDisposition::Superseded;
                    }
                }
                break;
            }
        }
    }

    /// Rebind only to the same live buffer, or one unambiguous reopened file.
    /// Exact source relocation is permitted; ambiguous matches never attach.
    pub(super) fn resolve_history_turn(
        &self,
        turn: &InlineHistoryTurn,
    ) -> Option<(usize, TextRange, InlineSourceState)> {
        self.resolve_history_source(
            &turn.location,
            if turn.state != InlineTurnState::Completed {
                &turn.before
            } else {
                turn.reviewed()
            },
            matches!(
                turn.state,
                InlineTurnState::Completed | InlineTurnState::Ready
            ) || turn
                .result
                .as_ref()
                .is_none_or(|result| result.replacement.is_none()),
        )
    }

    fn resolve_history_comment(
        &self,
        turn: &InlineHistoryTurn,
        comment: usize,
    ) -> Option<(usize, TextRange, InlineSourceState)> {
        let location = turn.comment_locations.get(comment)?;
        let source = self
            .inline_history
            .sources
            .get(turn.comment_source_ids.get(comment)?.as_ref()?)?;
        self.resolve_history_source(location, source, true)
    }

    pub(super) fn resolve_history_source(
        &self,
        location: &InlineLocation,
        reviewed: &str,
        allow_changed: bool,
    ) -> Option<(usize, TextRange, InlineSourceState)> {
        let mut candidates = self
            .buffer_manager
            .iter()
            .enumerate()
            .filter(|(_, buffer)| {
                location.buffer_id.map_or_else(
                    || buffer.file.as_deref() == Some(location.file.as_str()),
                    |id| buffer.id() == id,
                )
            });
        let (index, buffer) = candidates.next()?;
        if candidates.next().is_some() {
            return None;
        }
        let tracked = TextRange::new(
            buffer.char_idx_to_position(location.start_char),
            buffer.char_idx_to_position(location.end_char),
        );
        if !location.detached && !reviewed.is_empty() && buffer.text_in_range(tracked) == reviewed {
            return Some((index, tracked, InlineSourceState::Unchanged));
        }
        if !reviewed.is_empty() && buffer.byte_len() <= 4 * 1024 * 1024 {
            let contents = buffer.contents();
            let mut matches = contents.match_indices(reviewed).filter(|(offset, _)| {
                !location.detached
                    || (contents[..*offset].ends_with(&location.context_before)
                        && contents[offset + reviewed.len()..].starts_with(&location.context_after))
            });
            if let Some((offset, _)) = matches.next() {
                if matches.next().is_some() {
                    return Some((index, tracked, InlineSourceState::Detached));
                }
                let start = contents[..offset].chars().count();
                return Some((
                    index,
                    TextRange::new(
                        buffer.char_idx_to_position(start),
                        buffer.char_idx_to_position(start + reviewed.chars().count()),
                    ),
                    InlineSourceState::Unchanged,
                ));
            }
        }
        let state = if location.detached || tracked.start == tracked.end || !allow_changed {
            InlineSourceState::Detached
        } else {
            InlineSourceState::Changed
        };
        Some((index, tracked, state))
    }

    pub(super) fn transform_inline_history_for_edit(&mut self, edit: AppliedTextEdit) {
        let buffer = self.current_buffer();
        let id = buffer.id();
        let file = buffer.file.clone();
        let buffer_index = self.buffer_manager.active_index();
        let buffer = &self.buffer_manager[buffer_index];
        let locations = self
            .inline_history
            .conversations
            .iter_mut()
            .flat_map(|conversation| &mut conversation.turns)
            .flat_map(|turn| turn.locations_mut())
            .chain(self.inline_jobs.values_mut().map(|job| &mut job.location));
        for location in locations {
            if location.buffer_id != Some(id) {
                continue;
            }
            if edit.new_char_len == 0
                && edit.start_char <= location.start_char
                && edit.end_char >= location.end_char
                && edit.end_char > edit.start_char
            {
                location.detached = true;
            }
            let mut start = EditAnchor {
                buffer_id: id,
                file: file.clone(),
                char_index: location.start_char,
                fallback: location.range.start,
                affinity: AnchorAffinity::Right,
            };
            let mut end = EditAnchor {
                buffer_id: id,
                file: file.clone(),
                char_index: location.end_char,
                fallback: location.range.end,
                affinity: AnchorAffinity::Left,
            };
            Self::transform_inline_comment_anchor(&mut start, edit, buffer);
            Self::transform_inline_comment_anchor(&mut end, edit, buffer);
            location.start_char = start.char_index;
            location.end_char = end.char_index.max(start.char_index);
            location.range = TextRange::new(
                buffer.char_idx_to_position(location.start_char),
                buffer.char_idx_to_position(location.end_char),
            );
            if let Some(file) = &file {
                location.file.clone_from(file);
            }
        }
        self.mark_inline_history_dirty();
    }

    pub(super) fn detach_inline_history_buffer(&mut self, id: BufferId) {
        for job in self.inline_jobs.values_mut() {
            if job.location.buffer_id == Some(id) {
                job.location.buffer_id = None;
            }
        }
        for turn in self
            .inline_history
            .conversations
            .iter_mut()
            .flat_map(|conversation| &mut conversation.turns)
        {
            for location in turn.locations_mut() {
                if location.buffer_id == Some(id) {
                    location.buffer_id = None;
                }
            }
        }
    }

    fn clear_history_preview(&mut self) {
        self.inline_comments.retain(|comment| {
            !matches!(comment.origin, InlineCommentOrigin::HistoryPreview { .. })
        });
        self.layout_cache.borrow_mut().clear();
    }

    /// Reconstruct visible annotations from retained outcomes after recovery.
    pub(super) fn refresh_history_annotation_states(&mut self) {
        let states = self
            .inline_comments
            .iter()
            .filter_map(|comment| {
                let (request, index) = match &comment.origin {
                    InlineCommentOrigin::Assist {
                        request_id,
                        comment_index,
                        ..
                    }
                    | InlineCommentOrigin::HistoryPreview {
                        request_id,
                        comment_index,
                    } => (request_id, *comment_index),
                    InlineCommentOrigin::Sample
                    | InlineCommentOrigin::Activity { .. }
                    | InlineCommentOrigin::ChangeSummary { .. } => return None,
                };
                let turn = self.inline_history.turn(request)?;
                let resolved = self.resolve_history_comment(turn, index);
                let state = resolved
                    .as_ref()
                    .map_or(InlineSourceState::Detached, |(_, _, state)| *state);
                let anchors = resolved
                    .filter(|(_, _, state)| *state != InlineSourceState::Detached)
                    .map(|(index, range, _)| {
                        let buffer = &self.buffer_manager[index];
                        let start = TextPosition::new(range.start.line, 0);
                        let last = range.end.line.saturating_sub(usize::from(
                            range.end.character == 0 && range.end.line > range.start.line,
                        ));
                        let end = TextPosition::new(last, 0);
                        (
                            buffer.id(),
                            buffer.position_to_char_idx(start),
                            buffer.position_to_char_idx(end),
                            start,
                            end,
                        )
                    });
                Some((comment.id, (state, anchors)))
            })
            .collect::<HashMap<_, _>>();
        for comment in &mut self.inline_comments {
            if let Some((state, anchors)) = states.get(&comment.id) {
                comment.detached = *state == InlineSourceState::Detached;
                comment.stale = *state != InlineSourceState::Unchanged;
                if let Some((buffer_id, start_char, end_char, start, end)) = anchors {
                    comment.anchor.buffer_id = *buffer_id;
                    comment.anchor.char_index = *start_char;
                    comment.anchor.fallback = *start;
                    comment.end_anchor.buffer_id = *buffer_id;
                    comment.end_anchor.char_index = *end_char;
                    comment.end_anchor.fallback = *end;
                }
            }
        }
    }

    /// Reconstruct visible annotations from retained outcomes after recovery.
    pub(super) fn restore_inline_history_comments(&mut self) {
        for turn in self
            .inline_history
            .conversations
            .iter_mut()
            .flat_map(|conversation| &mut conversation.turns)
        {
            turn.ensure_change_summary();
        }
        let expansions = self
            .inline_history
            .conversations
            .iter()
            .flat_map(|conversation| &conversation.turns)
            .filter_map(|turn| {
                let location = turn.expanded_location.as_ref()?;
                let scope = turn.result.as_ref()?.expanded_scope.as_ref()?;
                let (index, range, state) =
                    self.resolve_history_source(location, &scope.before, true)?;
                Some((turn.request_id.clone(), index, range, state))
            })
            .collect::<Vec<_>>();
        for (request, index, range, state) in expansions {
            let mut location = Self::history_location_in_buffer(&self.buffer_manager[index], range);
            location.detached = state == InlineSourceState::Detached;
            if let Some(turn) = self.inline_history.turn_mut(&request) {
                turn.expanded_location = Some(location);
            }
        }
        let mut bindings = self
            .inline_history
            .conversations
            .iter()
            .flat_map(|conversation| &conversation.turns)
            .filter_map(|turn| {
                self.resolve_history_turn(turn)
                    .map(|(index, range, state)| {
                        (turn.request_id.clone(), None, index, range, state)
                    })
            })
            .collect::<Vec<_>>();
        for turn in self
            .inline_history
            .conversations
            .iter()
            .flat_map(|conversation| &conversation.turns)
        {
            for comment in 0..turn.comment_locations.len() {
                if let Some((index, range, state)) = self.resolve_history_comment(turn, comment) {
                    bindings.push((turn.request_id.clone(), Some(comment), index, range, state));
                }
            }
        }
        for (request, comment, index, range, state) in bindings {
            let buffer = &self.buffer_manager[index];
            if let Some(turn) = self.inline_history.turn_mut(&request) {
                let location = comment.map_or(&mut turn.location, |comment| {
                    &mut turn.comment_locations[comment]
                });
                location.buffer_id = Some(buffer.id());
                location.range = range;
                location.start_char = buffer.position_to_char_idx(range.start);
                location.end_char = buffer.position_to_char_idx(range.end);
                location.detached = state == InlineSourceState::Detached;
            }
        }
        let active = self.buffer_manager.active_index();
        let records = self
            .inline_history
            .conversations
            .iter()
            .filter(|conversation| !conversation.resolved)
            .filter_map(|conversation| {
                conversation
                    .visible_request
                    .as_deref()
                    .and_then(|request| {
                        conversation.turns.iter().find(|turn| {
                            turn.request_id == request
                                && turn.state == InlineTurnState::Completed
                                && turn
                                    .result
                                    .as_ref()
                                    .is_some_and(|result| result.needs_agent.is_none())
                        })
                    })
                    .or_else(|| {
                        conversation.turns.iter().rev().find(|turn| {
                            turn.state == InlineTurnState::Completed
                                && turn.disposition == InlineDisposition::Kept
                                && turn
                                    .result
                                    .as_ref()
                                    .is_some_and(|result| result.needs_agent.is_none())
                        })
                    })
                    .map(|turn| (conversation.id.clone(), turn.clone()))
            })
            .collect::<Vec<_>>();
        for (group, turn) in records {
            if self.inline_comments.iter().any(|comment| matches!(&comment.origin, InlineCommentOrigin::Assist { group_id, .. } if group_id == &group)) { continue; }
            self.remove_inline_comment_group(&group);
            let Some(result) = &turn.result else {
                continue;
            };
            for (comment_index, comment) in result.comments.iter().enumerate() {
                if turn.hidden_comments.contains(&comment_index) {
                    continue;
                }
                let Some((index, range, state)) =
                    self.resolve_history_comment(&turn, comment_index)
                else {
                    continue;
                };
                if state == InlineSourceState::Detached {
                    continue;
                }
                self.buffer_manager.set_active_index(index);
                let last = range.end.line.saturating_sub(usize::from(
                    range.end.character == 0 && range.end.line > range.start.line,
                ));
                let mut value = self.make_inline_comment(
                    range.start.line,
                    last,
                    comment.message.clone(),
                    InlineCommentOrigin::Assist {
                        group_id: group.clone(),
                        session_id: turn.session_id.clone().unwrap_or_default(),
                        request_id: turn.request_id.clone(),
                        comment_index,
                    },
                );
                if let Some(fingerprint) = turn.comment_fingerprints.get(comment_index) {
                    value.expected_fingerprint = *fingerprint;
                }
                value.stale = state != InlineSourceState::Unchanged;
                self.inline_comments.push(value);
            }
        }
        self.buffer_manager.set_active_index(active);
        self.sync_inline_change_summaries();
        self.layout_cache.borrow_mut().clear();
    }
}
