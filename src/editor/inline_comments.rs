//! Session-local sample annotations for iterating on the inline-assist presentation.

use super::{display_layout::LineSegment, AnchorAffinity, EditAnchor, Editor, Mode};
use crate::{
    buffer::{Buffer, BufferId},
    undo::TextPosition,
    unicode_utils::{display_width_with_tabs, trim_line_ending},
};

#[derive(Debug)]
pub(super) struct InlineComment {
    pub anchor: EditAnchor,
    pub end_anchor: EditAnchor,
    pub message: String,
}

impl InlineComment {
    fn lines(&self, buffer: &Buffer) -> (usize, usize) {
        let start = buffer.char_idx_to_position(self.anchor.char_index).line;
        let end = buffer.char_idx_to_position(self.end_anchor.char_index).line;
        (start, end.max(start))
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

    fn comment_index_on_current_line(&self) -> Option<usize> {
        self.comment_index_on_line(self.buffer_line())
    }

    fn comment_index_on_line(&self, line: usize) -> Option<usize> {
        let buffer = self.current_buffer();
        self.inline_comments.iter().position(|comment| {
            comment.anchor.buffer_id == buffer.id()
                && buffer.char_idx_to_position(comment.anchor.char_index).line == line
        })
    }

    fn set_inline_comment(&mut self, message: &str) {
        let (start, end) = self.inline_comment_target_lines();
        let buffer = self.current_buffer();
        let buffer_id = buffer.id();
        let start_char = buffer.position_to_char_idx(TextPosition::new(start, 0));
        let end_char = buffer.position_to_char_idx(TextPosition::new(end, 0));
        let comment = InlineComment {
            anchor: self.anchor_at_char(start_char, AnchorAffinity::Right),
            end_anchor: self.anchor_at_char(end_char, AnchorAffinity::Right),
            message: message.to_string(),
        };
        // One lane cannot unambiguously show overlapping brackets. The newest
        // sample replaces overlapping annotations in this presentation prototype.
        let buffer = &self.buffer_manager[self.buffer_manager.active_index()];
        self.inline_comments.retain(|existing| {
            if existing.anchor.buffer_id != buffer_id {
                return true;
            }
            let (existing_start, existing_end) = existing.lines(buffer);
            existing_end < start || existing_start > end
        });
        self.inline_comments.push(comment);
        self.layout_cache.borrow_mut().clear();
    }

    pub(super) fn clear_inline_comments(&mut self) {
        let buffer_id = self.current_buffer().id();
        self.inline_comments
            .retain(|comment| comment.anchor.buffer_id != buffer_id);
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
        let (start, end) = self.inline_comments.iter().find_map(|comment| {
            if comment.anchor.buffer_id != buffer.id() {
                return None;
            }
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
        let comment_height = self.comment_index_on_current_line().map_or(0, |index| {
            super::display_layout::inline_comment_block(
                &self.inline_comments[index].message,
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
