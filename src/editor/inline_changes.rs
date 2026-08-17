//! Durable edit receipts projected from retained before/after text and transactions.

use super::inline_comments::InlineCommentOrigin;
use super::*;
use crate::inline_history::{InlineHistoryTurn, InlineSourceState};

#[cfg(test)]
mod tests;

impl Editor {
    /// Deleted targets have an empty post-image. Resolve their insertion point
    /// from retained surrounding text instead of matching an empty string.
    pub(super) fn resolve_inline_change_source(
        &self,
        turn: &InlineHistoryTurn,
    ) -> Option<(usize, TextRange, InlineSourceState)> {
        let expected = if turn.disposition == InlineDisposition::Undone {
            turn.before.as_str()
        } else {
            turn.reviewed()
        };
        if !expected.is_empty() {
            return self.resolve_history_source(&turn.location, expected, true);
        }
        let location = &turn.location;
        let (index, buffer) = self.buffer_manager.iter().enumerate().find(|(_, buffer)| {
            location.buffer_id.map_or_else(
                || buffer.file.as_deref() == Some(location.file.as_str()),
                |id| buffer.id() == id,
            )
        })?;
        if buffer.byte_len() > 4 * 1024 * 1024 {
            return None;
        }
        let contents = buffer.contents();
        let boundary_matches = |offset: usize| {
            (if location.context_before.is_empty() {
                offset == 0
            } else {
                contents[..offset].ends_with(&location.context_before)
            }) && (if location.context_after.is_empty() {
                offset == contents.len()
            } else {
                contents[offset..].starts_with(&location.context_after)
            })
        };
        let tracked = contents
            .char_indices()
            .nth(location.start_char)
            .map_or(contents.len(), |(offset, _)| offset);
        let offset = if !location.detached && boundary_matches(tracked) {
            tracked
        } else {
            let context = format!("{}{}", location.context_before, location.context_after);
            if context.is_empty() {
                if !contents.is_empty() {
                    return None;
                }
                0
            } else {
                let mut matches = contents
                    .match_indices(&context)
                    .map(|(offset, _)| offset + location.context_before.len())
                    .filter(|offset| boundary_matches(*offset));
                let offset = matches.next()?;
                if matches.next().is_some() {
                    return None;
                }
                offset
            }
        };
        let position = buffer.char_idx_to_position(contents[..offset].chars().count());
        Some((
            index,
            TextRange::insertion(position),
            InlineSourceState::Unchanged,
        ))
    }

    pub(super) fn inline_change_label(&self, turn: &InlineHistoryTurn) -> String {
        if turn.disposition == InlineDisposition::Undone {
            return "Undone".into();
        }
        let Some((index, _, state)) = self.resolve_inline_change_source(turn) else {
            return "Applied · source unavailable".into();
        };
        if state != InlineSourceState::Unchanged {
            return "Applied · source changed".into();
        }
        if self.buffer_manager[index].is_dirty() {
            "Applied · buffer unsaved".into()
        } else {
            "Applied · buffer saved".into()
        }
    }

    fn inline_change_can_undo(&self, turn: &InlineHistoryTurn) -> bool {
        turn.disposition != InlineDisposition::Undone
            && self.buffer_manager.iter().any(|buffer| {
                turn.location.buffer_id.map_or_else(
                    || buffer.file.as_deref() == Some(turn.location.file.as_str()),
                    |id| buffer.id() == id,
                ) && buffer
                    .undo_history
                    .latest_transaction()
                    .is_some_and(|transaction| {
                        Some(transaction.id.as_str()) == turn.transaction_id.as_deref()
                    })
            })
    }

    /// A summary is editor-owned and survives the ephemeral provider session.
    pub(super) fn sync_inline_change_summaries(&mut self) {
        self.sync_inline_agent_markers();
        if !self
            .inline_history
            .conversations
            .iter()
            .any(|conversation| {
                conversation
                    .turns
                    .iter()
                    .any(|turn| turn.change_summary.is_some())
            })
            && !self
                .inline_comments
                .iter()
                .any(|comment| matches!(comment.origin, InlineCommentOrigin::ChangeSummary { .. }))
        {
            return;
        }
        let records =
            self.inline_history
                .conversations
                .iter()
                .filter(|conversation| !conversation.resolved)
                .filter_map(|conversation| {
                    conversation
                        .visible_request
                        .as_deref()
                        .and_then(|request| {
                            conversation.turns.iter().find(|turn| {
                                turn.request_id == request && turn.change_summary.is_some()
                            })
                        })
                        .or_else(|| {
                            conversation
                                .turns
                                .iter()
                                .rev()
                                .find(|turn| turn.change_summary.is_some())
                        })
                })
                .filter(|turn| {
                    turn.has_code_change()
                        && turn
                            .change_summary
                            .as_ref()
                            .is_some_and(|summary| !summary.hidden)
                })
                .filter_map(|turn| {
                    let summary = turn.change_summary.as_ref()?;
                    let (index, range, state) = self.resolve_inline_change_source(turn)?;
                    if state == InlineSourceState::Detached {
                        return None;
                    }
                    let buffer = &self.buffer_manager[index];
                    let start = buffer.position_to_char_idx(range.start);
                    let position = if state == InlineSourceState::Unchanged
                        && turn.disposition != InlineDisposition::Undone
                    {
                        buffer.char_idx_to_position(start.saturating_add(
                            summary.hunks.first().map_or(0, |hunk| hunk.start_char),
                        ))
                    } else {
                        range.start
                    };
                    let message = format!(
                        "✓ {} · {} location(s)\n{} · Space v changes · Space H",
                        self.inline_change_label(turn),
                        summary.hunks.len(),
                        crate::ui::first_prompt_line(&turn.prompt)
                    );
                    Some((turn.request_id.clone(), index, position.line, message))
                })
                .collect::<Vec<_>>();
        let requests = records
            .iter()
            .map(|(request, ..)| request.as_str())
            .collect::<HashSet<_>>();
        let mut changed = HashSet::new();
        self.inline_comments
            .retain(|comment| match &comment.origin {
                InlineCommentOrigin::ChangeSummary { request_id } => {
                    let keep = requests.contains(request_id.as_str());
                    if !keep {
                        changed.insert(comment.anchor.buffer_id);
                    }
                    keep
                }
                _ => true,
            });
        for (request_id, index, line, message) in records {
            let mut value = Self::make_inline_comment_in_buffer(
                &self.buffer_manager[index],
                line,
                line,
                message,
                InlineCommentOrigin::ChangeSummary {
                    request_id: request_id.clone(),
                },
            );
            if let Some(existing) = self.inline_comments.iter_mut().find(|comment|
                matches!(&comment.origin, InlineCommentOrigin::ChangeSummary { request_id: owner } if owner == &request_id)) {
                if existing.message == value.message && existing.anchor.buffer_id == value.anchor.buffer_id
                    && existing.anchor.char_index == value.anchor.char_index && existing.end_anchor.char_index == value.end_anchor.char_index
                    && !existing.detached { continue; }
                changed.insert(existing.anchor.buffer_id);
                changed.insert(value.anchor.buffer_id);
                value.id = existing.id;
                *existing = value;
            } else {
                changed.insert(value.anchor.buffer_id);
                self.inline_comments.push(value);
            }
        }
        if !changed.is_empty() {
            self.layout_cache
                .borrow_mut()
                .retain(|key, _| !changed.contains(&key.buffer_id));
        }
    }

    pub(super) async fn view_inline_changes(
        &mut self,
        request: &str,
        hunk: usize,
        frame: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let Some(turn) = self
            .inline_history
            .turn(request)
            .filter(|turn| turn.has_code_change())
            .cloned()
        else {
            self.set_legacy_message(Some("inline edit is no longer available".into()));
            return self.render(frame);
        };
        self.park_inline_assist();
        if self.inline_history_browser.is_some() {
            self.close_inline_history(true, frame, runtime).await?;
        }
        if !self.buffer_manager.iter().any(|buffer| {
            turn.location.buffer_id == Some(buffer.id())
                || buffer.file.as_deref() == Some(turn.location.file.as_str())
        }) && Path::new(&turn.location.file).is_file()
        {
            self.execute_with_tracking(
                &Action::OpenFile(turn.location.file.clone()),
                frame,
                runtime,
                false,
            )
            .await?;
        }
        let summary = turn.change_summary.clone().unwrap_or_else(|| {
            crate::inline_history::InlineChangeSummary::new(&turn.before, turn.reviewed())
        });
        let count = summary.hunks.len();
        let selected = hunk.min(count.saturating_sub(1));
        let resolved = self.resolve_inline_change_source(&turn);
        let mut location = format!(
            "{}:{}",
            turn.location.file,
            turn.location.range.start.line + 1
        );
        let can_navigate = turn.disposition != InlineDisposition::Undone
            && resolved
                .as_ref()
                .is_some_and(|(_, _, state)| *state == InlineSourceState::Unchanged);
        if let Some((index, range, _)) =
            resolved.filter(|(_, _, state)| *state != InlineSourceState::Detached)
        {
            let source = &self.buffer_manager[index];
            let start = source.position_to_char_idx(range.start);
            let position = if can_navigate {
                source.char_idx_to_position(
                    start.saturating_add(
                        summary
                            .hunks
                            .get(selected)
                            .map_or(0, |hunk| hunk.start_char),
                    ),
                )
            } else {
                range.start
            };
            location = format!("{}:{}", turn.location.file, position.line + 1);
            if self.buffer_manager.active_index() != index {
                self.set_current_buffer(frame, index).await?;
            }
            self.mode = Mode::Normal;
            self.selection = None;
            self.selection_start = None;
            self.panel_manager.focus_editor();
            self.move_to_text_position(position);
            self.refresh_cursor_goal();
        }
        let status = self.inline_change_label(&turn);
        let navigation = if can_navigate {
            format!("Change {} of {count} · {location}", selected + 1)
        } else {
            format!("{location}\nHistorical diff · changed-location navigation unavailable")
        };
        let text = format!("{status}\n\n{navigation}\n\nRequest: {}", turn.prompt);
        let mut hover = HoverInfo::new(self, text, HoverInfoFormat::Markdown, Vec::new())
            .with_diff(
                &turn.location.file,
                &turn.before,
                turn.reviewed(),
                "after inline edit",
            )
            .with_label("Inline changes")
            .with_shortcut('H', "history", Action::OpenInlineHistory);
        if can_navigate && count > 1 {
            hover = hover
                .with_shortcut(
                    '[',
                    "previous change",
                    Action::ViewInlineChanges {
                        request_id: request.into(),
                        hunk: (selected + count - 1) % count,
                    },
                )
                .with_shortcut(
                    ']',
                    "next change",
                    Action::ViewInlineChanges {
                        request_id: request.into(),
                        hunk: (selected + 1) % count,
                    },
                );
        }
        if self.inline_change_can_undo(&turn) {
            hover = hover.with_shortcut('u', "undo edit", Action::UndoInlineChange(request.into()));
        }
        self.current_dialog = Some(Box::new(hover));
        self.render(frame)
    }

    pub(super) async fn undo_inline_change(
        &mut self,
        request: &str,
        frame: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let Some(turn) = self
            .inline_history
            .turn(request)
            .filter(|turn| turn.has_code_change())
            .cloned()
        else {
            return Ok(());
        };
        if !self.inline_change_can_undo(&turn) {
            self.set_legacy_message(Some(
                "inline edit is no longer the latest change; use transaction history to revert it"
                    .into(),
            ));
            return self.render(frame);
        }
        let Some(index) = self.buffer_manager.iter().position(|buffer| {
            buffer
                .undo_history
                .latest_transaction()
                .is_some_and(|transaction| {
                    Some(transaction.id.as_str()) == turn.transaction_id.as_deref()
                })
        }) else {
            return Ok(());
        };
        self.park_inline_assist();
        if self.buffer_manager.active_index() != index {
            self.set_current_buffer(frame, index).await?;
        }
        self.current_dialog = None;
        self.undo_transaction(frame, runtime).await?;
        self.notify_inline_outcome(request);
        self.view_inline_changes(request, 0, frame, runtime).await
    }
}
