//! Session-local sample annotations for iterating on the inline-assist presentation.

use super::{AnchorAffinity, EditAnchor, Editor};
use crate::{buffer::BufferId, undo::TextPosition};

#[derive(Debug)]
pub(super) struct InlineComment {
    pub anchor: EditAnchor,
    pub message: String,
}

const SAMPLES: &[&str] = &[
    "Consider handling the empty case here.",
    "This is a good place to explain why this value is needed.",
    "The happy path looks clear. What should happen if this operation fails?",
    "This longer sample comment is here to exercise wrapping in narrow editor splits. It should stay aligned with the source column without changing the file or its line numbers.",
    "Could this be expressed more directly? A small helper might make the intent easier to see.",
];

impl Editor {
    pub(super) fn add_sample_inline_comment(&mut self) {
        let sample = (uuid::Uuid::new_v4().as_u128() % SAMPLES.len() as u128) as usize;
        let previous = self.comment_index_on_current_line();
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
        let buffer = self.current_buffer();
        let line = self.buffer_line();
        self.inline_comments.iter().position(|comment| {
            comment.anchor.buffer_id == buffer.id()
                && buffer.char_idx_to_position(comment.anchor.char_index).line == line
        })
    }

    fn set_inline_comment(&mut self, message: &str) {
        let previous = self.comment_index_on_current_line();
        let buffer = self.current_buffer();
        let buffer_id = buffer.id();
        let line = self.buffer_line();
        let char_index = buffer.position_to_char_idx(TextPosition::new(line, 0));
        let file = buffer.file.clone();
        let comment = InlineComment {
            anchor: EditAnchor {
                buffer_id,
                file,
                char_index,
                fallback: TextPosition::new(line, 0),
                affinity: AnchorAffinity::Right,
            },
            message: message.to_string(),
        };
        if let Some(index) = previous {
            self.inline_comments[index] = comment;
        } else {
            self.inline_comments.push(comment);
        }
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
