//! History ownership, conservative source resolution, and browser coordination.

use super::inline_comments::InlineCommentOrigin;
use super::*;
use crate::inline_history::{
    HistoryAction, InlineConversation, InlineHistoryTurn, InlineLocation, InlineSourceState,
};

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub(super) struct HistoryBrowser {
    origin: JumpEntry,
    viewport: (usize, usize, usize),
    active_comment: Option<uuid::Uuid>,
    file: Option<String>,
    workspace: bool,
    query: String,
    searching: bool,
    selected: Option<String>,
    expanded: HashSet<String>,
    view: usize,
    scroll: usize,
    confirm_forget: bool,
}

impl Editor {
    fn history_rows(&self) -> Vec<(String, String, String)> {
        let Some(browser) = &self.inline_history_browser else {
            return Vec::new();
        };
        let query = browser.query.to_lowercase();
        let mut rows = Vec::new();
        for conversation in self.inline_history.conversations.iter().rev() {
            if !browser.workspace && browser.file.as_deref() != Some(conversation.file.as_str()) {
                continue;
            }
            let expanded = browser.expanded.contains(&conversation.id) || !query.is_empty();
            for (index, turn) in conversation.turns.iter().enumerate().rev() {
                if !expanded && index + 1 != conversation.turns.len() {
                    continue;
                }
                if !query.is_empty()
                    && !format!(
                        "{} {} {}",
                        conversation.file,
                        turn.prompt,
                        turn.answer_text()
                    )
                    .to_lowercase()
                    .contains(&query)
                {
                    continue;
                }
                let marker = if index + 1 == conversation.turns.len() {
                    if conversation.turns.len() > 1 {
                        "▸ "
                    } else {
                        ""
                    }
                } else {
                    "  ↳ "
                };
                let resolved = if conversation.resolved {
                    "[resolved] "
                } else {
                    ""
                };
                let (source_line, source_state) = self.resolve_history_turn(turn).map_or(
                    (turn.location.range.start.line, InlineSourceState::Detached),
                    |(_, range, state)| (range.start.line, state),
                );
                let name = Path::new(&turn.location.file)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(&turn.location.file);
                rows.push((
                    conversation.id.clone(),
                    turn.request_id.clone(),
                    format!(
                        "{marker}{resolved}{}\n{name}:{} · {} · {}",
                        turn.prompt.lines().next().unwrap_or("Inline question"),
                        source_line + 1,
                        source_state.label(),
                        turn.status()
                    ),
                ));
            }
        }
        rows
    }

    pub(super) async fn open_inline_history(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        self.refresh_inline_history_paths();
        if self.inline_history_browser.is_none() {
            self.close_inline_assist_session();
            self.inline_history_browser = Some(HistoryBrowser {
                origin: self.current_jump_entry(),
                viewport: (self.vtop, self.vleft, self.skipcol),
                active_comment: self.active_inline_comment,
                file: self.current_buffer().file.clone(),
                workspace: false,
                query: String::new(),
                searching: false,
                selected: None,
                expanded: HashSet::new(),
                view: 0,
                scroll: 0,
                confirm_forget: false,
            });
        }
        self.refresh_inline_history_browser(buffer, runtime).await
    }

    async fn refresh_inline_history_browser(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        self.clear_history_preview();
        let rows = self.history_rows();
        let selected = self
            .inline_history_browser
            .as_ref()
            .and_then(|browser| browser.selected.as_deref())
            .and_then(|request| rows.iter().position(|(_, id, _)| id == request))
            .unwrap_or(0);
        let selected_id = rows.get(selected).map(|(_, request, _)| request.clone());
        if let Some(browser) = &mut self.inline_history_browser {
            browser.selected = selected_id.clone();
        }
        let turn = selected_id
            .as_deref()
            .and_then(|request| self.inline_history.turn(request))
            .cloned();
        let mut detail = "No inline conversations here yet. Use Space i to ask a question; w shows the whole workspace.".to_string();
        if let Some(turn) = turn {
            if self.resolve_history_turn(&turn).is_none()
                && Path::new(&turn.location.file).is_file()
            {
                self.execute_with_tracking(
                    &Action::OpenFile(turn.location.file.clone()),
                    buffer,
                    runtime,
                    false,
                )
                .await?;
            }
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
                if self.buffer_manager.active_index() != index {
                    self.set_current_buffer(buffer, index).await?;
                }
                self.move_to_text_position(range.start);
                self.vtop = range.start.line.saturating_sub(1);
                self.cy = range.start.line.saturating_sub(self.vtop);
                self.skipcol = 0;
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
                            let mut value = self.make_inline_comment(
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
                self.sync_to_window();
            }
            let view = self
                .inline_history_browser
                .as_ref()
                .map_or(0, |browser| browser.view);
            let header = format!(
                "{} · {}\n{}:{}–{}",
                state.label(),
                turn.status(),
                turn.location.file,
                turn.original_range.start.line + 1,
                turn.original_range.end.line + usize::from(turn.original_range.end.character > 0)
            );
            detail = match view {
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
                    "{header}\n\nYou: {}\n\nAssistant: {}{}",
                    turn.prompt,
                    turn.answer_text(),
                    turn.error
                        .as_ref()
                        .map(|error| format!("\n\nOutcome: {error}"))
                        .unwrap_or_default()
                ),
            };
        }
        let Some(browser) = &self.inline_history_browser else {
            return Ok(());
        };
        let title = format!(
            "Inline history · {} · {}",
            if browser.workspace {
                "workspace"
            } else {
                "current file"
            },
            ["conversation", "reviewed code", "before edit", "compare"][browser.view]
        );
        self.current_dialog = Some(Box::new(crate::ui::InlineHistoryPanel::new(
            self,
            rows.into_iter().map(|(_, _, label)| label).collect(),
            selected,
            detail,
            browser.scroll,
            browser.searching,
            browser.query.clone(),
            browser.confirm_forget,
            title,
        )));
        self.layout_cache.borrow_mut().clear();
        self.render(buffer)
    }

    async fn close_inline_history(
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
            .as_deref()
            .and_then(|request| rows.iter().position(|(_, id, _)| id == request))
            .unwrap_or(0);
        let selected_row = rows.get(selected).cloned();
        if matches!(action, HistoryAction::Close | HistoryAction::Jump) {
            return self
                .close_inline_history(matches!(action, HistoryAction::Jump), buffer, runtime)
                .await;
        }
        if matches!(action, HistoryAction::Continue | HistoryAction::Recheck) {
            if let Some((group, request, _)) = selected_row {
                let turn = self.inline_history.turn(&request).cloned();
                if let Some(turn) = turn {
                    if let Some((index, range, state)) = self.resolve_history_turn(&turn) {
                        if state != InlineSourceState::Detached {
                            self.close_inline_history(true, buffer, runtime).await?;
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
                browser.selected = Some(rows[next].1.clone());
                browser.scroll = 0;
            }
            HistoryAction::Expand | HistoryAction::Collapse => {
                if let Some((group, _, _)) = &selected_row {
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
            HistoryAction::EndSearch => browser.searching = false,
            HistoryAction::ClearSearch => {
                browser.searching = false;
                browser.query.clear();
                browser.selected = None;
            }
            HistoryAction::ScrollDown => browser.scroll = browser.scroll.saturating_add(4),
            HistoryAction::ScrollUp => browser.scroll = browser.scroll.saturating_sub(4),
            HistoryAction::CycleView => {
                browser.view = (browser.view + 1) % 4;
                browser.scroll = 0;
            }
            HistoryAction::Forget => browser.confirm_forget = !browser.confirm_forget,
            HistoryAction::ConfirmForget => {
                browser.confirm_forget = false;
                if let Some((group, _, _)) = &selected_row {
                    self.inline_history
                        .conversations
                        .retain(|conversation| conversation.id != *group);
                    self.inline_history.remove_unused_sources();
                    self.remove_inline_comment_group(group);
                }
            }
            HistoryAction::Resolve => {
                if let Some((group, _, _)) = &selected_row {
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
                }
            }
            _ => {}
        }
        self.refresh_inline_history_browser(buffer, runtime).await
    }

    fn history_location(&self, range: TextRange) -> InlineLocation {
        let buffer = self.current_buffer();
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
        for conversation in &mut self.inline_history.conversations {
            for turn in &mut conversation.turns {
                for location in
                    std::iter::once(&mut turn.location).chain(turn.comment_locations.iter_mut())
                {
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
                    let (start, end) = comment.lines(self.current_buffer());
                    let location = self.history_location(TextRange::new(
                        TextPosition::new(start, 0),
                        TextPosition::new(end.saturating_add(1), 0),
                    ));
                    let source = (self
                        .current_buffer()
                        .line_range_byte_len(start, end.saturating_add(1))
                        <= 256 * 1024)
                        .then(|| {
                            self.current_buffer()
                                .line_range_contents(start, end.saturating_add(1))
                        });
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
                for turn in &mut conversation.turns {
                    if turn.request_id == request {
                        turn.location = location.clone();
                        turn.state = InlineTurnState::Completed;
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
                    } else if turn.state == InlineTurnState::Completed
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
    fn resolve_history_turn(
        &self,
        turn: &InlineHistoryTurn,
    ) -> Option<(usize, TextRange, InlineSourceState)> {
        self.resolve_history_source(
            &turn.location,
            turn.reviewed(),
            turn.state == InlineTurnState::Completed
                || turn
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

    fn resolve_history_source(
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
        for turn in self
            .inline_history
            .conversations
            .iter_mut()
            .flat_map(|conversation| &mut conversation.turns)
        {
            for location in
                std::iter::once(&mut turn.location).chain(turn.comment_locations.iter_mut())
            {
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
        }
    }

    pub(super) fn detach_inline_history_buffer(&mut self, id: BufferId) {
        for turn in self
            .inline_history
            .conversations
            .iter_mut()
            .flat_map(|conversation| &mut conversation.turns)
        {
            for location in
                std::iter::once(&mut turn.location).chain(turn.comment_locations.iter_mut())
            {
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
                    InlineCommentOrigin::Sample => return None,
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
                    .turns
                    .iter()
                    .rev()
                    .find(|turn| {
                        turn.state == InlineTurnState::Completed
                            && turn.disposition == InlineDisposition::Kept
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
        self.layout_cache.borrow_mut().clear();
    }
}
