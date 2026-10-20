//! Session-local annotations, provenance, overlap projection, and source anchors.

use sha2::{Digest as _, Sha256};
use std::collections::HashSet;
use uuid::Uuid;

use super::{
    display_layout::{InlineCommentRow, LineSegment},
    Action, AnchorAffinity, EditAnchor, Editor, Mode, RenderBuffer, Runtime,
};
use crate::{
    buffer::{Buffer, BufferId},
    inline_assist::InlineCommentInput,
    ui::{HoverInfo, HoverInfoFormat},
    undo::{AppliedTextEdit, TextPosition, TextRange},
    unicode_utils::{display_width, display_width_with_tabs, trim_line_ending},
};

pub(super) const MAX_BUFFER_COMMENTS: usize = 256;
const MAX_FINGERPRINT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone)]
pub(super) enum InlineCommentOrigin {
    Sample,
    Activity {
        group_id: String,
    },
    ChangeSummary {
        request_id: String,
    },
    AgentOutcome {
        request_id: String,
        file: String,
    },
    AgentAnnotation {
        session_id: String,
        turn_id: String,
    },
    HistoryPreview {
        request_id: String,
        comment_index: usize,
    },
    Assist {
        group_id: String,
        session_id: String,
        request_id: String,
        comment_index: usize,
    },
}

#[derive(Debug)]
pub(super) struct InlineComment {
    pub id: Uuid,
    pub anchor: EditAnchor,
    pub end_anchor: EditAnchor,
    pub message: String,
    pub origin: InlineCommentOrigin,
    pub stale: bool,
    pub detached: bool,
    pub(super) expected_fingerprint: Option<[u8; 32]>,
}

impl InlineComment {
    pub(super) fn lines(&self, buffer: &Buffer) -> (usize, usize) {
        let last = buffer.navigable_line_count().saturating_sub(1);
        let start = buffer
            .char_idx_to_position(self.anchor.char_index)
            .line
            .min(last);
        let end = buffer
            .char_idx_to_position(self.end_anchor.char_index)
            .line
            .min(last);
        (start, end.max(start))
    }

    fn belongs_to(&self, group_id: &str) -> bool {
        matches!(&self.origin, InlineCommentOrigin::Assist { group_id: owner, .. } if owner == group_id)
    }

    pub(super) fn refresh_staleness(&mut self, buffer: &Buffer) {
        if matches!(
            self.origin,
            InlineCommentOrigin::Activity { .. }
                | InlineCommentOrigin::ChangeSummary { .. }
                | InlineCommentOrigin::AgentOutcome { .. }
        ) {
            return;
        }
        let (start, end) = self.lines(buffer);
        self.stale = self.expected_fingerprint.is_none()
            || fingerprint(buffer, start, end) != self.expected_fingerprint;
    }

    pub(super) fn expected_fingerprint(&self) -> Option<[u8; 32]> {
        self.expected_fingerprint
    }

    pub(super) fn set_expected_fingerprint(&mut self, fingerprint: Option<[u8; 32]>) {
        self.expected_fingerprint = fingerprint;
    }
}

fn fingerprint(buffer: &Buffer, start: usize, end: usize) -> Option<[u8; 32]> {
    (buffer.line_range_byte_len(start, end.saturating_add(1)) <= MAX_FINGERPRINT_BYTES).then(|| {
        Sha256::digest(
            buffer
                .line_range_contents(start, end.saturating_add(1))
                .as_bytes(),
        )
        .into()
    })
}

struct CommentProjection {
    index: usize,
    ordinal: usize,
    count: usize,
    members: Vec<usize>,
}

/// The same pager text supplies both painting and terminal-column hit targets.
struct InlinePager {
    text: String,
    next_start: usize,
}

impl InlinePager {
    fn new(ordinal: usize, count: usize, ascii: bool) -> Self {
        let (previous, next) = if ascii { ('<', '>') } else { ('‹', '›') };
        let text = format!("{previous} {ordinal}/{count} {next}");
        let next_start = display_width(&text).saturating_sub(2);
        Self { text, next_start }
    }

    fn direction_at(&self, text: &str, column: usize) -> Option<bool> {
        if column < 2 && text.starts_with(self.text.chars().next()?) {
            Some(true)
        } else if text.starts_with(&self.text)
            && (self.next_start..self.next_start + 3).contains(&column)
        {
            Some(false)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum InlineCommentConnector {
    Single,
    Start,
    Middle,
    End,
}

const SAMPLES: &[&str] = &[
    "Consider handling the empty case here.",
    "This is a good place to explain why this value is needed.",
    "The happy path looks clear. What should happen if this operation fails?",
    "This longer sample comment is here to exercise wrapping in narrow editor splits. It should stay aligned with the source column without changing the file or its line numbers.",
    "Could this be expressed more directly? A small helper might make the intent easier to see.",
];

impl Editor {
    pub(super) fn inline_comment_selection(
        &self,
        window: crate::window::WindowId,
        buffer: BufferId,
    ) -> Option<Uuid> {
        self.inline_comment_selections
            .get(&(window, buffer))
            .copied()
    }

    pub(super) fn inline_comment_is_current_at(
        &self,
        window: &crate::window::Window,
        line: usize,
        include_range: bool,
    ) -> bool {
        let Some(buffer) = self.buffer_manager.get(window.buffer_index) else {
            return false;
        };
        let Some(id) = self.inline_comment_selection(window.id, buffer.id()) else {
            return false;
        };
        self.inline_comments
            .iter()
            .find(|comment| comment.id == id && self.inline_comment_visible(comment))
            .is_some_and(|comment| {
                let (start, end) = comment.lines(buffer);
                start == line || (include_range && start <= line && line <= end)
            })
    }

    pub(super) fn active_inline_comment(&self) -> Option<Uuid> {
        self.inline_comment_selection(
            self.window_manager.active_stable_window_id()?,
            self.current_buffer().id(),
        )
    }

    pub(super) fn set_active_inline_comment(&mut self, id: Option<Uuid>) {
        if let Some(window) = self.window_manager.active_stable_window_id() {
            self.set_inline_comment_selection(window, self.current_buffer().id(), id);
        }
    }

    pub(super) fn set_inline_comment_selection(
        &mut self,
        window: crate::window::WindowId,
        buffer: BufferId,
        id: Option<Uuid>,
    ) {
        if let Some(id) = id {
            self.inline_comment_selections.insert((window, buffer), id);
        } else {
            self.inline_comment_selections.remove(&(window, buffer));
        }
        self.layout_cache.borrow_mut().clear();
    }

    /// Retarget every pane reading a removed item, without changing other panes.
    fn replace_inline_selection(&mut self, removed: Uuid, next: Option<Uuid>) {
        self.inline_comment_selections.retain(|_, selected| {
            if *selected != removed {
                return true;
            }
            if let Some(next) = next {
                *selected = next;
                true
            } else {
                false
            }
        });
    }

    /// Whole-target replacements must not send every covered annotation to the
    /// replacement's end. Preserve its relative source line as an approximate
    /// location; the fingerprint decides whether the annotation is still valid.
    pub(super) fn transform_inline_comment_anchor(
        anchor: &mut EditAnchor,
        edit: AppliedTextEdit,
        buffer: &Buffer,
    ) {
        if edit.start_char < edit.end_char
            && (edit.start_char..edit.end_char).contains(&anchor.char_index)
        {
            let first = buffer.char_idx_to_position(edit.start_char).line;
            let end =
                buffer.char_idx_to_position(edit.start_char.saturating_add(edit.new_char_len));
            let last = end
                .line
                .saturating_sub(usize::from(end.character == 0 && end.line > first));
            let line = first.saturating_add(
                anchor
                    .fallback
                    .line
                    .saturating_sub(first)
                    .min(last.saturating_sub(first)),
            );
            anchor.char_index = buffer.position_to_char_idx(TextPosition::new(line, 0));
        } else {
            Self::transform_anchor_for_edit(
                anchor,
                edit.start_char,
                edit.end_char,
                edit.new_char_len,
            );
        }
        anchor.fallback = buffer.char_idx_to_position(anchor.char_index);
    }

    pub(super) fn inline_comment_target_lines(&self) -> (usize, usize) {
        if matches!(self.mode, Mode::Visual | Mode::VisualLine) {
            if let Some(selection) = self.selection {
                let (_, start, _, end): (usize, usize, usize, usize) = selection.into();
                return (start.min(end), start.max(end));
            }
        }
        let line = self.buffer_line();
        if let Some(index) = self.comment_index_on_line(line) {
            return self.inline_comments[index].lines(self.current_buffer());
        }
        (line, line)
    }

    pub(super) fn add_sample_inline_comment(&mut self) {
        let sample = (uuid::Uuid::new_v4().as_u128() % SAMPLES.len() as u128) as usize;
        let previous = self.comment_index_on_line(self.inline_comment_target_lines().0);
        let sample = if previous
            .is_some_and(|index| self.inline_comments[index].message == SAMPLES[sample])
        {
            (sample + 1) % SAMPLES.len()
        } else {
            sample
        };
        self.set_inline_comment(SAMPLES[sample]);
    }

    fn comment_index_on_line(&self, line: usize) -> Option<usize> {
        let buffer = self.current_buffer();
        self.comment_projections(buffer)
            .into_iter()
            .map(|view| view.index)
            .find(|&index| self.inline_comments[index].lines(buffer).0 == line)
    }

    fn set_inline_comment(&mut self, message: &str) {
        let (start, end) = self.inline_comment_target_lines();
        let buffer_id = self.current_buffer().id();
        let buffer = &self.buffer_manager[self.buffer_manager.active_index()];
        self.inline_comments.retain(|existing| {
            existing.anchor.buffer_id != buffer_id
                || !matches!(existing.origin, InlineCommentOrigin::Sample)
                || existing.lines(buffer).0 != start
        });
        if self
            .inline_comments
            .iter()
            .filter(|comment| comment.anchor.buffer_id == buffer_id)
            .count()
            >= MAX_BUFFER_COMMENTS
        {
            self.set_legacy_message(Some(
                "inline comment limit reached; dismiss existing comments first".into(),
            ));
            return;
        }
        let comment =
            self.make_inline_comment(start, end, message.to_string(), InlineCommentOrigin::Sample);
        self.set_active_inline_comment(Some(comment.id));
        self.inline_comments.push(comment);
        self.layout_cache.borrow_mut().clear();
    }

    pub(super) fn make_inline_comment(
        &self,
        start: usize,
        end: usize,
        message: String,
        origin: InlineCommentOrigin,
    ) -> InlineComment {
        Self::make_inline_comment_in_buffer(self.current_buffer(), start, end, message, origin)
    }

    pub(super) fn make_inline_comment_in_buffer(
        buffer: &Buffer,
        start: usize,
        end: usize,
        message: String,
        origin: InlineCommentOrigin,
    ) -> InlineComment {
        let last = buffer.navigable_line_count().saturating_sub(1);
        let start = start.min(last);
        let end = end.max(start).min(last);
        let anchor = |line| {
            let position = TextPosition::new(line, 0);
            EditAnchor {
                buffer_id: buffer.id(),
                file: buffer.file.clone(),
                char_index: buffer.position_to_char_idx(position),
                fallback: position,
                affinity: AnchorAffinity::Right,
            }
        };
        InlineComment {
            id: Uuid::new_v4(),
            anchor: anchor(start),
            end_anchor: anchor(end),
            message,
            origin,
            stale: false,
            detached: false,
            expected_fingerprint: fingerprint(buffer, start, end),
        }
    }

    pub(super) fn check_inline_comment_capacity(
        &self,
        group_id: &str,
        count: usize,
    ) -> anyhow::Result<()> {
        self.check_inline_comment_capacity_for_buffer(self.current_buffer().id(), group_id, count)
    }

    pub(super) fn check_inline_comment_capacity_for_buffer(
        &self,
        buffer_id: BufferId,
        group_id: &str,
        count: usize,
    ) -> anyhow::Result<()> {
        let retained = self
            .inline_comments
            .iter()
            .filter(|comment| {
                comment.anchor.buffer_id == buffer_id
                    && !comment.belongs_to(group_id)
                    && !matches!(
                        comment.origin,
                        InlineCommentOrigin::Activity { .. }
                            | InlineCommentOrigin::ChangeSummary { .. }
                            | InlineCommentOrigin::AgentOutcome { .. }
                            | InlineCommentOrigin::HistoryPreview { .. }
                    )
            })
            .count();
        anyhow::ensure!(
            retained.saturating_add(count) <= MAX_BUFFER_COMMENTS,
            "inline comment limit reached; dismiss existing comments first"
        );
        Ok(())
    }

    pub(super) fn replace_inline_comment_group(
        &mut self,
        group_id: &str,
        session_id: &str,
        request_id: &str,
        start_line: usize,
        comments: &[InlineCommentInput],
    ) {
        if let Some(id) = self.replace_inline_comment_group_in_buffer(
            self.buffer_manager.active_index(),
            group_id,
            session_id,
            request_id,
            start_line,
            comments,
        ) {
            self.set_active_inline_comment(Some(id));
        }
    }

    /// Publish annotations without changing the active buffer or current item.
    pub(super) fn replace_inline_comment_group_in_buffer(
        &mut self,
        buffer_index: usize,
        group_id: &str,
        session_id: &str,
        request_id: &str,
        start_line: usize,
        comments: &[InlineCommentInput],
    ) -> Option<Uuid> {
        let buffer = &self.buffer_manager[buffer_index];
        let added = comments
            .iter()
            .enumerate()
            .map(|(comment_index, comment)| {
                Self::make_inline_comment_in_buffer(
                    buffer,
                    start_line + comment.start_line - 1,
                    start_line + comment.last_line() - 1,
                    comment.message.clone(),
                    InlineCommentOrigin::Assist {
                        group_id: group_id.to_string(),
                        session_id: session_id.to_string(),
                        request_id: request_id.to_string(),
                        comment_index,
                    },
                )
            })
            .collect::<Vec<_>>();
        let previous = self
            .inline_comments
            .iter()
            .filter_map(|comment| {
                let index = match &comment.origin {
                    InlineCommentOrigin::Assist {
                        group_id: owner,
                        comment_index,
                        ..
                    } if owner == group_id => *comment_index,
                    InlineCommentOrigin::Activity { group_id: owner } if owner == group_id => {
                        return Some((
                            comment.id,
                            Some(added.first().map_or(comment.id, |next| next.id)),
                        ));
                    }
                    _ => return None,
                };
                Some((
                    comment.id,
                    added
                        .get(index)
                        .or_else(|| added.first())
                        .map(|next| next.id),
                ))
            })
            .collect::<Vec<_>>();
        for (removed, next) in previous {
            self.replace_inline_selection(removed, next);
        }
        self.remove_inline_comment_group(group_id);
        let first = added.first().map(|comment| comment.id);
        self.inline_comments.extend(added);
        self.layout_cache.borrow_mut().clear();
        first
    }

    pub(super) fn inline_comment_group_count(&self, group_id: &str) -> usize {
        self.inline_comments
            .iter()
            .filter(|comment| comment.belongs_to(group_id))
            .count()
    }

    pub(super) fn remove_inline_comment_group(&mut self, group_id: &str) {
        let removed = self
            .inline_comments
            .iter()
            .filter(|comment| comment.belongs_to(group_id))
            .map(|comment| comment.id)
            .collect::<HashSet<_>>();
        self.inline_comments
            .retain(|comment| !comment.belongs_to(group_id));
        self.inline_comment_selections
            .retain(|_, id| !removed.contains(id));
        self.layout_cache.borrow_mut().clear();
    }

    pub(super) fn dismiss_inline_comment_group(&mut self, group_id: &str) {
        for comment in &self.inline_comments {
            if let InlineCommentOrigin::Assist {
                group_id: owner,
                request_id,
                comment_index,
                ..
            } = &comment.origin
            {
                if owner == group_id {
                    if let Some(turn) = self.inline_history.turn_mut(request_id) {
                        if !turn.hidden_comments.contains(comment_index) {
                            turn.hidden_comments.push(*comment_index);
                        }
                    }
                }
            }
        }
        self.remove_inline_comment_group(group_id);
    }

    // Collapse connected overlap groups, retaining every annotation in storage.
    // Cycling selects which range and box the one available gutter lane shows.
    fn comment_projections(&self, buffer: &Buffer) -> Vec<CommentProjection> {
        let selected = self
            .window_manager
            .active_stable_window_id()
            .and_then(|window| self.inline_comment_selection(window, buffer.id()));
        self.comment_projections_with_selection(buffer, selected)
    }

    fn comment_projections_for_window(
        &self,
        window: crate::window::WindowId,
        buffer: &Buffer,
    ) -> Vec<CommentProjection> {
        self.comment_projections_with_selection(
            buffer,
            self.inline_comment_selection(window, buffer.id()),
        )
    }

    fn comment_projections_with_selection(
        &self,
        buffer: &Buffer,
        current: Option<Uuid>,
    ) -> Vec<CommentProjection> {
        let mut indices = self
            .inline_comments
            .iter()
            .enumerate()
            .filter(|(_, comment)| {
                comment.anchor.buffer_id == buffer.id() && self.inline_comment_visible(comment)
            })
            .map(|(index, comment)| (index, comment.lines(buffer)))
            .collect::<Vec<_>>();
        indices.sort_by_key(|&(index, (start, _))| (start, index));
        let mut projected = Vec::new();
        let mut offset = 0;
        while offset < indices.len() {
            let mut end = indices[offset].1 .1;
            let mut next = offset + 1;
            while next < indices.len() && indices[next].1 .0 <= end {
                end = end.max(indices[next].1 .1);
                next += 1;
            }
            let group = &indices[offset..next];
            let selected = group
                .iter()
                .position(|&(index, _)| Some(self.inline_comments[index].id) == current)
                .unwrap_or_else(|| {
                    group
                        .iter()
                        .enumerate()
                        .max_by_key(|(_, (index, _))| *index)
                        .map_or(0, |(position, _)| position)
                });
            projected.push(CommentProjection {
                index: group[selected].0,
                ordinal: selected + 1,
                count: group.len(),
                members: group.iter().map(|(index, _)| *index).collect(),
            });
            offset = next;
        }
        projected
    }

    pub(super) fn inline_comment_display_messages(&self, buffer: &Buffer) -> Vec<(usize, String)> {
        self.inline_comment_messages(self.comment_projections(buffer), buffer)
    }

    pub(super) fn inline_comment_display_messages_for_window(
        &self,
        window: crate::window::WindowId,
        buffer: &Buffer,
    ) -> Vec<(usize, String)> {
        self.inline_comment_messages(self.comment_projections_for_window(window, buffer), buffer)
    }

    fn inline_comment_messages(
        &self,
        projections: Vec<CommentProjection>,
        buffer: &Buffer,
    ) -> Vec<(usize, String)> {
        projections
            .into_iter()
            .map(|view| {
                let comment = &self.inline_comments[view.index];
                let mut message = String::new();
                if view.count > 1 {
                    let pager = InlinePager::new(
                        view.ordinal,
                        view.count,
                        self.config.window_borders_ascii,
                    );
                    message.push_str(&format!("{} · Space v\n", pager.text));
                }
                if comment.stale {
                    message.push_str("[outdated] ");
                }
                message.push_str(&comment.message);
                (comment.lines(buffer).0, message)
            })
            .collect()
    }

    pub(super) fn inline_activity_visible_in_window(
        &self,
        window: &crate::window::Window,
        changed: &HashSet<Uuid>,
    ) -> bool {
        let Some(buffer) = self.buffer_manager.get(window.buffer_index) else {
            return false;
        };
        let lines = self
            .comment_projections_for_window(window.id, buffer)
            .into_iter()
            .filter_map(|view| {
                let comment = &self.inline_comments[view.index];
                changed
                    .contains(&comment.id)
                    .then(|| comment.lines(buffer).0)
            })
            .collect::<HashSet<_>>();
        !lines.is_empty()
            && self
                .layout_for_window(window)
                .inline_comments
                .iter()
                .any(|row| {
                    lines.contains(&row.line)
                        && matches!(
                            row.content,
                            super::display_layout::InlineCommentContent::Text(_)
                        )
                })
    }

    fn current_inline_comment_index(&self) -> Option<usize> {
        let buffer = self.current_buffer();
        let line = self.buffer_line();
        let at_line = |comment: &InlineComment| {
            let (start, end) = comment.lines(buffer);
            comment.anchor.buffer_id == buffer.id()
                && self.inline_comment_visible(comment)
                && start <= line
                && line <= end
        };
        self.inline_comments
            .iter()
            .position(|comment| {
                Some(comment.id) == self.active_inline_comment() && at_line(comment)
            })
            .or_else(|| {
                self.comment_projections(buffer)
                    .into_iter()
                    .find(|view| {
                        view.members
                            .iter()
                            .any(|&index| at_line(&self.inline_comments[index]))
                    })
                    .map(|view| view.index)
            })
    }

    pub(crate) fn current_inline_navigation(&self) -> Option<(Uuid, usize, usize)> {
        let index = self.current_inline_comment_index()?;
        let view = self
            .comment_projections(self.current_buffer())
            .into_iter()
            .find(|view| view.members.contains(&index))?;
        let ordinal = view.members.iter().position(|&member| member == index)? + 1;
        (view.count > 1).then_some((self.inline_comments[index].id, ordinal, view.count))
    }

    pub(super) fn current_inline_comment_id(&self) -> Option<Uuid> {
        self.current_inline_comment_index()
            .map(|index| self.inline_comments[index].id)
    }

    pub(super) fn select_inline_comment_for_group(&mut self, group: &str) {
        let buffer_id = self.current_buffer().id();
        let current_request = self
            .inline_assist
            .as_ref()
            .filter(|assist| assist.annotation_group_id == group)
            .and_then(|assist| {
                assist
                    .result_request_id
                    .as_deref()
                    .or(assist.request_id.as_deref())
            });
        let prefer_changes = current_request
            .and_then(|request| self.inline_history.turn(request))
            .is_some_and(|turn| turn.has_code_change());
        let belongs = |comment: &InlineComment| {
            comment.anchor.buffer_id == buffer_id
                && self.inline_comment_visible(comment)
                && (matches!(&comment.origin, InlineCommentOrigin::Assist { group_id, .. } | InlineCommentOrigin::Activity { group_id } if group_id == group)
                    || matches!(&comment.origin, InlineCommentOrigin::ChangeSummary { request_id } if self.inline_history.conversations.iter().any(|conversation| conversation.id == group && conversation.turns.iter().any(|turn| &turn.request_id == request_id))))
        };
        let selected = self
            .inline_comments
            .iter()
            .filter(|comment| belongs(comment))
            .find(|comment| match &comment.origin {
                InlineCommentOrigin::ChangeSummary { request_id } if prefer_changes => {
                    Some(request_id.as_str()) == current_request
                }
                InlineCommentOrigin::Assist { request_id, .. } if !prefer_changes => {
                    Some(request_id.as_str()) == current_request
                }
                _ => false,
            })
            .or_else(|| {
                self.inline_comments.iter().find(|comment| {
                    belongs(comment) && Some(comment.id) == self.active_inline_comment()
                })
            })
            .or_else(|| {
                self.inline_comments
                    .iter()
                    .filter(|comment| belongs(comment))
                    .find(|comment| matches!(comment.origin, InlineCommentOrigin::Assist { .. }))
            })
            .or_else(|| self.inline_comments.iter().find(|comment| belongs(comment)))
            .map(|comment| (comment.id, comment.lines(self.current_buffer()).0));
        if let Some((id, line)) = selected {
            self.set_active_inline_comment(Some(id));
            self.layout_cache.borrow_mut().clear();
            self.move_to_text_position(TextPosition::new(line, 0));
            self.refresh_cursor_goal();
        }
    }

    /// Cycle only the connected overlap group containing the specified item.
    pub(super) fn cycle_overlapping_inline_comment(
        &mut self,
        id: Uuid,
        backwards: bool,
    ) -> Option<Uuid> {
        let view = self
            .comment_projections(self.current_buffer())
            .into_iter()
            .find(|view| {
                view.members
                    .iter()
                    .any(|&index| self.inline_comments[index].id == id)
            })?;
        let position = view
            .members
            .iter()
            .position(|&index| self.inline_comments[index].id == id)?;
        let next = if backwards {
            (position + view.count - 1) % view.count
        } else {
            (position + 1) % view.count
        };
        let comment = &self.inline_comments[view.members[next]];
        let id = comment.id;
        let line = comment.lines(self.current_buffer()).0;
        self.set_active_inline_comment(Some(id));
        self.layout_cache.borrow_mut().clear();
        self.move_to_text_position(TextPosition::new(line, 0));
        self.refresh_cursor_goal();
        self.set_legacy_message(Some(format!(
            "inline {} of {} here · [i previous · ]i next · Space v open",
            next + 1,
            view.count
        )));
        Some(id)
    }

    pub(super) fn inline_comment_click_action(
        &self,
        row: &InlineCommentRow,
        content_x: usize,
    ) -> Option<Action> {
        let text = row.content.text();
        let column = content_x.checked_sub(row.text_offset)?;
        if column >= display_width(text) {
            return None;
        }
        let view = self
            .comment_projections(self.current_buffer())
            .into_iter()
            .find(|view| {
                self.inline_comments[view.index]
                    .lines(self.current_buffer())
                    .0
                    == row.line
            })?;
        let id = self.inline_comments[view.index].id;
        if row.starts_connection && view.count > 1 {
            let pager =
                InlinePager::new(view.ordinal, view.count, self.config.window_borders_ascii);
            if let Some(backwards) = pager.direction_at(text, column) {
                return Some(Action::NavigateOverlappingInlineComment {
                    id,
                    backwards,
                    open: false,
                });
            }
            if column < display_width(&pager.text) {
                return Some(Action::ChooseInlineComment(id));
            }
        }
        Some(Action::FocusInlineComment(id))
    }

    pub(super) fn select_inline_comment_by_id(&mut self, id: Uuid) -> bool {
        let Some(line) = self
            .inline_comments
            .iter()
            .find(|comment| {
                comment.id == id
                    && comment.anchor.buffer_id == self.current_buffer().id()
                    && self.inline_comment_visible(comment)
            })
            .map(|comment| comment.lines(self.current_buffer()).0)
        else {
            self.set_legacy_message(Some("inline item is no longer visible".into()));
            return false;
        };
        self.set_active_inline_comment(Some(id));
        self.move_to_text_position(TextPosition::new(line, 0));
        self.refresh_cursor_goal();
        true
    }

    pub(super) async fn focus_inline_comment(
        &mut self,
        id: Uuid,
        frame: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        if self.inline_comments.iter().any(|comment| {
            comment.id == id
                && matches!(
                    comment.origin,
                    InlineCommentOrigin::Activity { .. }
                        | InlineCommentOrigin::ChangeSummary { .. }
                        | InlineCommentOrigin::AgentOutcome { .. }
                )
        }) {
            return self.open_inline_comment_by_id(id, frame, runtime).await;
        }
        self.park_inline_assist();
        if self.select_inline_comment_by_id(id) {
            self.show_inline_comment_view(true);
        }
        self.render(frame)
    }

    pub(super) fn choose_inline_comment(&mut self, id: Uuid) {
        use crate::ui::{Picker, PickerItem};
        let Some(view) = self
            .comment_projections(self.current_buffer())
            .into_iter()
            .find(|view| {
                view.members
                    .iter()
                    .any(|index| self.inline_comments[*index].id == id)
            })
        else {
            return;
        };
        let items = view
            .members
            .into_iter()
            .map(|index| {
                let comment = &self.inline_comments[index];
                let (start, end) = comment.lines(self.current_buffer());
                PickerItem {
                    id: comment.id.to_string(),
                    icon: None,
                    label: comment
                        .message
                        .lines()
                        .find(|line| !line.trim().is_empty())
                        .unwrap_or("Inline item")
                        .chars()
                        .take(180)
                        .collect(),
                    kind: None,
                    annotation: Some(format!("{}–{}", start + 1, end + 1)),
                    detail: None,
                    data: serde_json::Value::Null,
                    matches: Vec::new(),
                    detail_matches: Vec::new(),
                    preview: None,
                }
            })
            .collect();
        let mut picker = Picker::builder()
            .title("Inline items here")
            .structured_items(items)
            .content_sized(76, 8)
            .placeholder("Find an inline item")
            .select_action(|id| {
                Uuid::parse_str(&id).map_or(Action::CloseDialog, Action::FocusInlineComment)
            })
            .build(self);
        picker.select_dynamic_id(&id.to_string());
        self.current_dialog = Some(Box::new(picker));
    }

    fn inline_comment_discussion(&self, id: Uuid) -> Option<(String, String)> {
        let comment = self
            .inline_comments
            .iter()
            .find(|comment| comment.id == id)?;
        let request = match &comment.origin {
            InlineCommentOrigin::Assist { request_id, .. }
            | InlineCommentOrigin::HistoryPreview { request_id, .. } => request_id,
            _ => return None,
        };
        self.inline_history
            .conversations
            .iter()
            .find(|conversation| {
                conversation
                    .turns
                    .iter()
                    .any(|turn| &turn.request_id == request)
            })
            .map(|conversation| (conversation.id.clone(), request.clone()))
    }

    pub(super) async fn refine_inline_comment(
        &mut self,
        id: Uuid,
        frame: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        if let Some((group, request)) = self.inline_comment_discussion(id) {
            let context = match self.selected_comment_context(id) {
                Ok((context, _)) => context,
                Err(error) => {
                    self.set_legacy_message(Some(error.to_string()));
                    return self.render(frame);
                }
            };
            self.open_inline_history_request(&group, &request, frame, runtime)
                .await?;
            let reuse_existing = self.has_parked_inline_draft(&group)
                || self
                    .inline_history
                    .conversations
                    .iter()
                    .find(|conversation| conversation.id == group)
                    .and_then(|conversation| conversation.turns.last())
                    .is_some_and(|turn| {
                        matches!(
                            turn.state,
                            crate::inline_history::InlineTurnState::Pending
                                | crate::inline_history::InlineTurnState::Ready
                        )
                    });
            self.handle_inline_history_action(
                &crate::inline_history::HistoryAction::Continue,
                frame,
                runtime,
            )
            .await?;
            if !reuse_existing {
                if let Some(session) = self.inline_assist.as_mut().filter(|session| {
                    session.annotation_group_id == group && session.request_id.is_none()
                }) {
                    session.parent_comment = Some(context);
                    let scope = session.scope.clone();
                    self.current_dialog = Some(Box::new(self.inline_assist_popup(
                        scope,
                        crate::ui::InlineAssistPopupState::Prompt {
                            initial: String::new(),
                            refining: false,
                        },
                    )));
                    self.render(frame)?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn resolve_inline_comment(&mut self, id: Uuid) {
        let Some((group, _)) = self.inline_comment_discussion(id) else {
            return;
        };
        if !self.select_inline_comment_by_id(id) {
            return;
        }
        let next = self
            .comment_projections(self.current_buffer())
            .into_iter()
            .find(|view| {
                view.members
                    .iter()
                    .any(|index| self.inline_comments[*index].id == id)
            })
            .and_then(|view| {
                view.members
                    .into_iter()
                    .map(|index| &self.inline_comments[index])
                    .find(|comment| !comment.belongs_to(&group))
                    .map(|comment| comment.id)
            });
        if let Some(conversation) = self
            .inline_history
            .conversations
            .iter_mut()
            .find(|conversation| conversation.id == group)
        {
            conversation.resolved = true;
        }
        self.remove_inline_comment_group(&group);
        self.restore_inline_history_comments();
        self.sync_inline_activity();
        self.set_active_inline_comment(next);
        if let Some(next) = next {
            self.select_inline_comment_by_id(next);
        }
        self.current_dialog = None;
        self.mark_inline_history_dirty();
        self.set_legacy_message(Some(
            "inline discussion resolved · Space H to restore".into(),
        ));
    }

    pub(super) async fn open_inline_comment_by_id(
        &mut self,
        id: Uuid,
        frame: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        self.park_inline_assist();
        if !self.select_inline_comment_by_id(id) {
            return self.render(frame);
        }
        let Some(comment) = self.inline_comments.iter().find(|comment| {
            comment.id == id
                && comment.anchor.buffer_id == self.current_buffer().id()
                && self.inline_comment_visible(comment)
        }) else {
            self.set_legacy_message(Some("inline item is no longer visible".into()));
            return self.render(frame);
        };
        if let InlineCommentOrigin::Activity { group_id } = &comment.origin {
            let group = group_id.clone();
            return self.open_inline_job(&group, frame, runtime).await;
        }
        if let InlineCommentOrigin::ChangeSummary { request_id } = &comment.origin {
            let request = request_id.clone();
            return self.view_inline_changes(&request, 0, frame, runtime).await;
        }
        if let InlineCommentOrigin::AgentOutcome { request_id, file } = &comment.origin {
            let request = request_id.clone();
            let file = file.clone();
            if let Some((outcome, change)) = self.inline_agent_file_review_target(&request, &file) {
                return self
                    .view_inline_agent_changes(&request, outcome, change, frame, runtime)
                    .await;
            }
        }
        self.show_inline_comment();
        self.render(frame)
    }

    pub(super) fn navigate_inline_comment(&mut self, backwards: bool) {
        let buffer = self.current_buffer();
        let mut indices = self
            .inline_comments
            .iter()
            .enumerate()
            .filter(|(_, comment)| {
                comment.anchor.buffer_id == buffer.id() && self.inline_comment_visible(comment)
            })
            .map(|(index, comment)| (index, comment.lines(buffer).0))
            .collect::<Vec<_>>();
        indices.sort_by_key(|&(index, start)| (start, index));
        if indices.is_empty() {
            self.set_routine_warning(Some("no inline comments in this buffer".into()));
            return;
        }
        let current = self.current_inline_comment_index().and_then(|index| {
            indices
                .iter()
                .position(|&(candidate, _)| candidate == index)
        });
        let position = match current {
            Some(position) if backwards => (position + indices.len() - 1) % indices.len(),
            Some(position) => (position + 1) % indices.len(),
            None if backwards => indices
                .iter()
                .rposition(|&(_, start)| start < self.buffer_line())
                .unwrap_or(indices.len() - 1),
            None => indices
                .iter()
                .position(|&(_, start)| start >= self.buffer_line())
                .unwrap_or(0),
        };
        let (index, line) = indices[position];
        self.set_active_inline_comment(Some(self.inline_comments[index].id));
        self.layout_cache.borrow_mut().clear();
        self.move_to_text_position(TextPosition::new(line, 0));
        self.refresh_cursor_goal();
        self.set_legacy_message(Some(format!(
            "comment {}/{} · Space v view · Space x dismiss",
            position + 1,
            indices.len()
        )));
    }

    pub(super) fn dismiss_inline_comment(&mut self) {
        if let Some(index) = self.current_inline_comment_index() {
            let removed = self.inline_comments[index].id;
            let next = self
                .comment_projections(self.current_buffer())
                .into_iter()
                .find(|view| view.members.contains(&index))
                .and_then(|view| {
                    let position = view.members.iter().position(|member| *member == index)?;
                    (view.count > 1)
                        .then(|| self.inline_comments[view.members[(position + 1) % view.count]].id)
                });
            if let InlineCommentOrigin::AgentOutcome { request_id, file } =
                &self.inline_comments[index].origin
            {
                let request = request_id.clone();
                let file = file.clone();
                self.hide_inline_agent_file(&request, &file);
                self.set_legacy_message(Some(
                    "Agent change summary hidden · Space H to restore".into(),
                ));
            }
            if let InlineCommentOrigin::ChangeSummary { request_id } =
                &self.inline_comments[index].origin
            {
                if let Some(summary) = self
                    .inline_history
                    .turn_mut(request_id)
                    .and_then(|turn| turn.change_summary.as_mut())
                {
                    summary.hidden = true;
                }
                self.set_legacy_message(Some("change summary hidden · Space H to restore".into()));
            }
            if matches!(
                self.inline_comments[index].origin,
                InlineCommentOrigin::Activity { .. }
            ) {
                self.set_legacy_message(Some(
                    "inline request retained · Space v to open · Ctrl-c to cancel".into(),
                ));
                return;
            }
            if let InlineCommentOrigin::Assist {
                request_id,
                comment_index,
                ..
            } = &self.inline_comments[index].origin
            {
                if let Some(turn) = self.inline_history.turn_mut(request_id) {
                    if !turn.hidden_comments.contains(comment_index) {
                        turn.hidden_comments.push(*comment_index);
                    }
                }
            }
            self.inline_comments.remove(index);
            self.replace_inline_selection(removed, next);
            // The cursor may have reached this card without an explicit selection.
            self.set_active_inline_comment(next);
            if let Some(line) = next
                .and_then(|id| self.inline_comments.iter().find(|comment| comment.id == id))
                .map(|comment| comment.lines(self.current_buffer()).0)
            {
                self.move_to_text_position(TextPosition::new(line, 0));
                self.refresh_cursor_goal();
            }
            self.layout_cache.borrow_mut().clear();
        } else {
            self.set_routine_warning(Some("no inline comment at the cursor".into()));
        }
    }

    pub(super) fn show_inline_comment(&mut self) {
        self.show_inline_comment_view(false);
    }

    fn show_inline_comment_view(&mut self, compact: bool) {
        // Source layout must be settled before deriving terminal coordinates.
        // An existing modal intentionally hides the terminal cursor, so it is
        // not a valid anchor when cycling between comments.
        self.check_bounds();
        self.sync_to_window();
        let Some(index) = self.current_inline_comment_index() else {
            self.set_routine_warning(Some("no inline comment at the cursor".into()));
            return;
        };
        let comment = &self.inline_comments[index];
        let (start, end) = comment.lines(self.current_buffer());
        let provenance = match &comment.origin {
            InlineCommentOrigin::Sample => "sample".to_string(),
            InlineCommentOrigin::Activity { .. } => "inline activity".to_string(),
            InlineCommentOrigin::ChangeSummary { .. } => "inline changes".to_string(),
            InlineCommentOrigin::AgentOutcome { .. } => "Agent changes".to_string(),
            InlineCommentOrigin::AgentAnnotation { turn_id, .. } => {
                format!("Agent annotation · turn {turn_id}")
            }
            InlineCommentOrigin::HistoryPreview { .. } => "history preview".to_string(),
            InlineCommentOrigin::Assist {
                session_id,
                request_id,
                ..
            } => self.inline_history.turn(request_id).map_or_else(
                || format!("inline assist · session {session_id} · request {request_id}"),
                |turn| format!("You: {}", turn.prompt),
            ),
        };
        let provenance = if compact && provenance.chars().count() > 120 {
            format!("{}…", provenance.chars().take(120).collect::<String>())
        } else {
            provenance
        };
        let message = if compact {
            let snippet = comment.message.chars().take(240).collect::<String>();
            if snippet.len() < comment.message.len() {
                format!("{snippet}…")
            } else {
                snippet
            }
        } else {
            comment.message.clone()
        };
        let text = format!(
            "Lines {}–{}{}\n{}\n\n{}",
            start + 1,
            end + 1,
            if comment.stale { " · outdated" } else { "" },
            provenance,
            message
        );
        let navigation = self.current_inline_navigation();
        let label = navigation.map_or_else(
            || "Inline comment".into(),
            |(_, ordinal, count)| format!("Inline {ordinal} of {count}"),
        );
        let mut hover =
            HoverInfo::new(self, text, HoverInfoFormat::Plaintext, Vec::new()).with_label(label);
        if let Some(layout) = self.inline_comment_overlay_layout(comment.id) {
            hover = hover.with_inline_source(comment.id, layout);
        }
        if compact {
            hover = hover.with_inline_card(comment.id).with_shortcut(
                'v',
                "full comment",
                Action::OpenInlineComment(comment.id),
            );
        }
        hover = hover.with_shortcut('x', "dismiss", Action::DismissInlineCommentById(comment.id));
        if matches!(
            comment.origin,
            InlineCommentOrigin::Assist { .. } | InlineCommentOrigin::HistoryPreview { .. }
        ) {
            hover = hover
                .with_shortcut(
                    'i',
                    "ask inline",
                    Action::AskInlineComment {
                        id: comment.id,
                        in_agent: false,
                    },
                )
                .with_shortcut(
                    'A',
                    "ask Agent",
                    Action::AskInlineComment {
                        id: comment.id,
                        in_agent: true,
                    },
                )
                .with_shortcut(
                    'r',
                    "refine discussion",
                    Action::RefineInlineComment(comment.id),
                )
                .with_shortcut(
                    'd',
                    "resolve discussion",
                    Action::ResolveInlineComment(comment.id),
                );
        }
        if let Some((id, _, _)) = navigation {
            hover = hover.with_inline_navigation(id).with_shortcut(
                'c',
                "choose inline",
                Action::ChooseInlineComment(id),
            );
        }
        self.current_dialog = Some(Box::new(hover));
    }

    pub(super) fn inline_comment_overlay_layout(
        &self,
        id: Uuid,
    ) -> Option<crate::ui::OverlayLayout> {
        let comment = self
            .inline_comments
            .iter()
            .find(|comment| comment.id == id)?;
        let window_id = self.window_manager.active_stable_window_id()?;
        let window = self.window_manager.window(window_id)?;
        let buffer = self.buffer_manager.get(window.buffer_index)?;
        if buffer.id() != comment.anchor.buffer_id || comment.detached {
            return None;
        }
        let (start, end) = comment.lines(buffer);
        let mut overlay = self.text_range_overlay_layout(
            window_id,
            buffer.id(),
            TextRange::new(
                TextPosition::new(start, 0),
                TextPosition::new(end.saturating_add(1), 0),
            ),
        )?;
        let layout = self.layout_for_window(window);
        let mut card_rows = layout
            .inline_comments
            .iter()
            .filter(|row| row.line == start);
        if let Some(first) = card_rows.next() {
            let last = card_rows.next_back().unwrap_or(first);
            let card = (
                self.window_to_terminal_y(window, first.row),
                self.window_to_terminal_y(window, last.row),
            );
            overlay.protected_rows = Some(card);
            overlay.avoid_rows = Some(
                overlay
                    .avoid_rows
                    .map_or(card, |source| (card.0.min(source.0), card.1.max(source.1))),
            );
        }
        Some(overlay)
    }

    pub(super) fn clear_inline_comments(&mut self) {
        let buffer_id = self.current_buffer().id();
        let agent_files = self
            .inline_comments
            .iter()
            .filter(|comment| comment.anchor.buffer_id == buffer_id)
            .filter_map(|comment| match &comment.origin {
                InlineCommentOrigin::AgentOutcome { request_id, file } => {
                    Some((request_id.clone(), file.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for (request, file) in agent_files {
            self.hide_inline_agent_file(&request, &file);
        }
        for comment in &self.inline_comments {
            if comment.anchor.buffer_id == buffer_id {
                if let InlineCommentOrigin::ChangeSummary { request_id } = &comment.origin {
                    if let Some(summary) = self
                        .inline_history
                        .turn_mut(request_id)
                        .and_then(|turn| turn.change_summary.as_mut())
                    {
                        summary.hidden = true;
                    }
                }
                if let InlineCommentOrigin::Assist {
                    request_id,
                    comment_index,
                    ..
                } = &comment.origin
                {
                    if let Some(turn) = self.inline_history.turn_mut(request_id) {
                        if !turn.hidden_comments.contains(comment_index) {
                            turn.hidden_comments.push(*comment_index);
                        }
                    }
                }
            }
        }
        self.inline_comments.retain(|comment| {
            comment.anchor.buffer_id != buffer_id
                || matches!(comment.origin, InlineCommentOrigin::Activity { .. })
        });
        self.inline_comment_selections
            .retain(|(_, buffer), _| *buffer != buffer_id);
        self.layout_cache.borrow_mut().clear();
    }

    pub(super) fn has_inline_comments(&self, buffer_id: BufferId) -> bool {
        self.inline_comments.iter().any(|comment| {
            comment.anchor.buffer_id == buffer_id && self.inline_comment_visible(comment)
        })
    }

    pub(super) fn inline_comment_visible(&self, comment: &InlineComment) -> bool {
        !comment.detached
            && matches!(comment.origin, InlineCommentOrigin::HistoryPreview { .. })
                == self.inline_history_browser.is_some()
    }

    pub(super) fn inline_job_on_comment_line(&self, line: usize) -> Option<String> {
        self.comment_projections(self.current_buffer())
            .into_iter()
            .find_map(|view| {
                let comment = &self.inline_comments[view.index];
                match &comment.origin {
                    InlineCommentOrigin::Activity { group_id }
                        if comment.lines(self.current_buffer()).0 == line =>
                    {
                        Some(group_id.clone())
                    }
                    _ => None,
                }
            })
    }

    /// Reserves a connector and a trailing space without consuming source cells.
    pub(super) fn inline_comment_lane_width_for_buffer(
        &self,
        buffer_index: usize,
        window_width: usize,
    ) -> usize {
        if !self
            .buffer_manager
            .get(buffer_index)
            .is_some_and(|buffer| self.has_inline_comments(buffer.id()))
        {
            return 0;
        }
        let available =
            window_width.saturating_sub(self.gutter_width_for_buffer_index(buffer_index) + 1);
        if available >= 24 {
            4
        } else if available >= 3 {
            2
        } else {
            0
        }
    }

    pub(super) fn inline_comment_lane_width(&self, window: &crate::window::Window) -> usize {
        self.inline_comment_lane_width_for_buffer(window.buffer_index, window.inner_width())
    }

    pub(super) fn inline_comment_connector_for_segment(
        &self,
        window: &crate::window::Window,
        segment: &LineSegment,
    ) -> Option<InlineCommentConnector> {
        let buffer = self.buffer_manager.get(window.buffer_index)?;
        let (start, end) = self
            .comment_projections_for_window(window.id, buffer)
            .into_iter()
            .find_map(|view| {
                let comment = &self.inline_comments[view.index];
                let range = comment.lines(buffer);
                (range.0 <= segment.line && segment.line <= range.1).then_some(range)
            })?;
        let first = segment.line == start && segment.first_segment;
        let last_segment = !window.wrap
            || buffer.get(segment.line).is_some_and(|line| {
                segment.end_col
                    >= display_width_with_tabs(
                        trim_line_ending(&line),
                        self.tab_width_for_buffer_index(window.buffer_index),
                    )
            });
        let last = segment.line == end && last_segment;
        Some(match (first, last) {
            (true, true) => InlineCommentConnector::Single,
            (true, false) => InlineCommentConnector::Start,
            (false, true) => InlineCommentConnector::End,
            (false, false) => InlineCommentConnector::Middle,
        })
    }

    /// Keep the real cursor visible when annotations consume viewport rows.
    pub(super) fn ensure_inline_comment_cursor_visible(&mut self) {
        let line = self.buffer_line();
        let display_col = self.current_cursor_display_col();
        let comment_height = self
            .inline_comment_display_messages(self.current_buffer())
            .into_iter()
            .find(|(start, _)| *start == line)
            .map_or(0, |(_, message)| {
                super::display_layout::inline_comment_block(
                    &message,
                    self.active_content_width(),
                    self.vheight().saturating_sub(1),
                )
                .rows
                .len()
            });
        while self.vtop < line {
            let Some(window) = self.active_window_with_editor_view() else {
                break;
            };
            let layout = self.layout_for_window(&window);
            let cursor_visible = if self.wrap {
                self.visible_cursor_segment(line, display_col)
            } else {
                layout.segment_for_cursor(line, display_col).is_some()
            };
            let visible_comment_height = layout
                .inline_comments
                .iter()
                .filter(|comment| comment.line == line)
                .count();
            if cursor_visible && visible_comment_height >= comment_height {
                break;
            }
            self.vtop += 1;
            self.skipcol = 0;
            self.cy = line - self.vtop;
        }
        if self.wrap && !self.visible_cursor_segment(line, display_col) {
            if let Some(segment) = self
                .wrapped_line_segments_for_width(line, self.active_content_width())
                .into_iter()
                .find(|segment| segment.contains_cursor_col(display_col))
            {
                self.vtop = line;
                self.cy = 0;
                self.skipcol = segment.start_col;
            }
        }
    }
}

#[cfg(test)]
mod tests;
