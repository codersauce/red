//! Session-local annotations, provenance, overlap projection, and source anchors.

use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use super::{display_layout::LineSegment, AnchorAffinity, EditAnchor, Editor, Mode};
use crate::{
    buffer::{Buffer, BufferId},
    inline_assist::InlineCommentInput,
    ui::{HoverInfo, HoverInfoFormat},
    undo::{AppliedTextEdit, TextPosition},
    unicode_utils::{display_width_with_tabs, trim_line_ending},
};

const MAX_BUFFER_COMMENTS: usize = 256;
const MAX_FINGERPRINT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone)]
pub(super) enum InlineCommentOrigin {
    Sample,
    Assist {
        group_id: String,
        session_id: String,
        request_id: String,
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
    expected_fingerprint: Option<[u8; 32]>,
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
        let (start, end) = self.lines(buffer);
        self.stale = self.expected_fingerprint.is_none()
            || fingerprint(buffer, start, end) != self.expected_fingerprint;
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
            self.last_error =
                Some("inline comment limit reached; dismiss existing comments first".into());
            return;
        }
        let comment =
            self.make_inline_comment(start, end, message.to_string(), InlineCommentOrigin::Sample);
        self.active_inline_comment = Some(comment.id);
        self.inline_comments.push(comment);
        self.layout_cache.borrow_mut().clear();
    }

    fn make_inline_comment(
        &self,
        start: usize,
        end: usize,
        message: String,
        origin: InlineCommentOrigin,
    ) -> InlineComment {
        let buffer = self.current_buffer();
        let last = buffer.navigable_line_count().saturating_sub(1);
        let start = start.min(last);
        let end = end.max(start).min(last);
        InlineComment {
            id: Uuid::new_v4(),
            anchor: self.anchor_at_char(
                buffer.position_to_char_idx(TextPosition::new(start, 0)),
                AnchorAffinity::Right,
            ),
            end_anchor: self.anchor_at_char(
                buffer.position_to_char_idx(TextPosition::new(end, 0)),
                AnchorAffinity::Right,
            ),
            message,
            origin,
            stale: false,
            expected_fingerprint: fingerprint(buffer, start, end),
        }
    }

    pub(super) fn check_inline_comment_capacity(
        &self,
        group_id: &str,
        count: usize,
    ) -> anyhow::Result<()> {
        let buffer_id = self.current_buffer().id();
        let retained = self
            .inline_comments
            .iter()
            .filter(|comment| {
                comment.anchor.buffer_id == buffer_id && !comment.belongs_to(group_id)
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
        let added = comments
            .iter()
            .map(|comment| {
                self.make_inline_comment(
                    start_line + comment.start_line - 1,
                    start_line + comment.last_line() - 1,
                    comment.message.clone(),
                    InlineCommentOrigin::Assist {
                        group_id: group_id.to_string(),
                        session_id: session_id.to_string(),
                        request_id: request_id.to_string(),
                    },
                )
            })
            .collect::<Vec<_>>();
        self.remove_inline_comment_group(group_id);
        if let Some(comment) = added.first() {
            self.active_inline_comment = Some(comment.id);
        }
        self.inline_comments.extend(added);
        self.layout_cache.borrow_mut().clear();
    }

    pub(super) fn inline_comment_group_count(&self, group_id: &str) -> usize {
        self.inline_comments
            .iter()
            .filter(|comment| comment.belongs_to(group_id))
            .count()
    }

    pub(super) fn remove_inline_comment_group(&mut self, group_id: &str) {
        self.inline_comments
            .retain(|comment| !comment.belongs_to(group_id));
        self.layout_cache.borrow_mut().clear();
    }

    // Collapse connected overlap groups, retaining every annotation in storage.
    // Cycling selects which range and box the one available gutter lane shows.
    fn comment_projections(&self, buffer: &Buffer) -> Vec<CommentProjection> {
        let mut indices = self
            .inline_comments
            .iter()
            .enumerate()
            .filter(|(_, comment)| comment.anchor.buffer_id == buffer.id())
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
                .position(|&(index, _)| {
                    Some(self.inline_comments[index].id) == self.active_inline_comment
                })
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
            });
            offset = next;
        }
        projected
    }

    pub(super) fn inline_comment_display_messages(&self, buffer: &Buffer) -> Vec<(usize, String)> {
        self.comment_projections(buffer)
            .into_iter()
            .map(|view| {
                let comment = &self.inline_comments[view.index];
                let mut message = String::new();
                if view.count > 1 {
                    message.push_str(&format!("[{}/{}] ", view.ordinal, view.count));
                }
                if comment.stale {
                    message.push_str("[outdated] ");
                }
                message.push_str(&comment.message);
                (comment.lines(buffer).0, message)
            })
            .collect()
    }

    fn current_inline_comment_index(&self) -> Option<usize> {
        let buffer = self.current_buffer();
        let line = self.buffer_line();
        let at_line = |comment: &InlineComment| {
            let (start, end) = comment.lines(buffer);
            comment.anchor.buffer_id == buffer.id() && start <= line && line <= end
        };
        self.inline_comments
            .iter()
            .position(|comment| Some(comment.id) == self.active_inline_comment && at_line(comment))
            .or_else(|| {
                self.comment_projections(buffer)
                    .into_iter()
                    .map(|view| view.index)
                    .find(|&index| at_line(&self.inline_comments[index]))
            })
    }

    pub(super) fn navigate_inline_comment(&mut self, backwards: bool) {
        let buffer = self.current_buffer();
        let mut indices = self
            .inline_comments
            .iter()
            .enumerate()
            .filter(|(_, comment)| comment.anchor.buffer_id == buffer.id())
            .map(|(index, comment)| (index, comment.lines(buffer).0))
            .collect::<Vec<_>>();
        indices.sort_by_key(|&(index, start)| (start, index));
        if indices.is_empty() {
            self.last_error = Some("no inline comments in this buffer".into());
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
        self.active_inline_comment = Some(self.inline_comments[index].id);
        self.layout_cache.borrow_mut().clear();
        self.move_to_text_position(TextPosition::new(line, 0));
        self.refresh_cursor_goal();
        self.last_error = Some(format!(
            "comment {}/{} · Space v view · Space x dismiss",
            position + 1,
            indices.len()
        ));
    }

    pub(super) fn dismiss_inline_comment(&mut self) {
        if let Some(index) = self.current_inline_comment_index() {
            self.inline_comments.remove(index);
            self.active_inline_comment = None;
            self.layout_cache.borrow_mut().clear();
        } else {
            self.last_error = Some("no inline comment at the cursor".into());
        }
    }

    pub(super) fn show_inline_comment(&mut self) {
        let Some(index) = self.current_inline_comment_index() else {
            self.last_error = Some("no inline comment at the cursor".into());
            return;
        };
        let comment = &self.inline_comments[index];
        let (start, end) = comment.lines(self.current_buffer());
        let provenance = match &comment.origin {
            InlineCommentOrigin::Sample => "sample".to_string(),
            InlineCommentOrigin::Assist {
                session_id,
                request_id,
                ..
            } => format!("inline assist · session {session_id} · request {request_id}"),
        };
        let text = format!(
            "Lines {}–{}{}\n{}\n\n{}",
            start + 1,
            end + 1,
            if comment.stale { " · outdated" } else { "" },
            provenance,
            comment.message
        );
        self.current_dialog = Some(Box::new(
            HoverInfo::new(self, text, HoverInfoFormat::Plaintext, Vec::new())
                .with_label("Inline comment"),
        ));
    }

    pub(super) fn clear_inline_comments(&mut self) {
        let buffer_id = self.current_buffer().id();
        self.inline_comments
            .retain(|comment| comment.anchor.buffer_id != buffer_id);
        self.active_inline_comment = None;
        self.layout_cache.borrow_mut().clear();
    }

    pub(super) fn has_inline_comments(&self, buffer_id: BufferId) -> bool {
        self.inline_comments
            .iter()
            .any(|comment| comment.anchor.buffer_id == buffer_id)
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
            .comment_projections(buffer)
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
                .find(|segment| segment.contains_display_col(display_col))
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
