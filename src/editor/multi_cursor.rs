use crate::{
    buffer::BufferId,
    editing::{CharRange, SelectionSet},
    undo::TextRange,
    window::WindowId,
};

use super::Editor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MultiCursorPhase {
    Selecting,
    Inserting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MultiCursorInsertAnchor {
    Start,
    End,
    Replace,
}

#[derive(Debug, Clone)]
pub(super) struct MultiCursorSession {
    buffer_id: BufferId,
    window_id: WindowId,
    revision: u64,
    phase: MultiCursorPhase,
    selections: SelectionSet,
}

impl MultiCursorSession {
    fn belongs_to(&self, editor: &Editor) -> bool {
        self.buffer_id == editor.current_buffer().id()
            && Some(self.window_id) == editor.window_manager.active_stable_window_id()
            && self.revision == editor.current_buffer().revision()
    }

    pub(super) fn ranges_for_buffer(
        &self,
        editor: &Editor,
        buffer_id: BufferId,
        window_id: WindowId,
    ) -> Vec<TextRange> {
        if self.buffer_id != buffer_id
            || self.window_id != window_id
            || self.revision != editor.current_buffer().revision()
        {
            return Vec::new();
        }
        self.selections.text_ranges(editor.current_buffer())
    }
}

impl Editor {
    pub(super) fn has_multi_cursor_session(&self) -> bool {
        self.multi_cursor
            .as_ref()
            .is_some_and(|session| session.belongs_to(self))
    }

    pub(super) fn multi_cursor_is_inserting(&self) -> bool {
        self.multi_cursor.as_ref().is_some_and(|session| {
            session.belongs_to(self) && session.phase == MultiCursorPhase::Inserting
        })
    }

    pub(super) fn clear_multi_cursor(&mut self) {
        self.multi_cursor = None;
    }

    pub(super) fn select_next_occurrence(&mut self) {
        let valid_selecting_session = self.multi_cursor.as_ref().is_some_and(|session| {
            session.belongs_to(self)
                && session.phase == MultiCursorPhase::Selecting
                && session
                    .selections
                    .ranges()
                    .iter()
                    .all(|range| !range.is_empty())
        });

        if valid_selecting_session {
            let session = self
                .multi_cursor
                .as_mut()
                .expect("session was checked above");
            session.selections.select_next();
        } else {
            let buffer_id = self.current_buffer().id();
            let Some(window_id) = self.window_manager.active_stable_window_id() else {
                return;
            };
            let revision = self.current_buffer().revision();
            let cursor = self.cursor_text_position();
            let Some(selections) = SelectionSet::from_cursor(
                self.current_buffer(),
                cursor,
                self.config.search.ignorecase,
                self.config.search.smartcase,
            ) else {
                self.multi_cursor = None;
                return;
            };
            self.multi_cursor = Some(MultiCursorSession {
                buffer_id,
                window_id,
                revision,
                phase: MultiCursorPhase::Selecting,
                selections,
            });
        }

        self.move_to_active_multi_cursor(false);
    }

    pub(super) fn begin_multi_cursor_insert(&mut self, anchor: MultiCursorInsertAnchor) -> bool {
        if !self.has_multi_cursor_session() {
            self.multi_cursor = None;
            return false;
        }
        let ranges = self
            .multi_cursor
            .as_ref()
            .expect("session was checked above")
            .selections
            .ranges()
            .to_vec();
        if ranges.is_empty() {
            return false;
        }

        let label = match anchor {
            MultiCursorInsertAnchor::Start => "multi-cursor insert",
            MultiCursorInsertAnchor::End => "multi-cursor append",
            MultiCursorInsertAnchor::Replace => "multi-cursor change",
        };
        self.begin_transaction(label);
        match anchor {
            MultiCursorInsertAnchor::Start => {
                self.collapse_multi_cursor_ranges(&ranges, |range| range.start);
            }
            MultiCursorInsertAnchor::End => {
                self.collapse_multi_cursor_ranges(&ranges, |range| range.end);
            }
            MultiCursorInsertAnchor::Replace => self.replace_multi_cursor_targets(ranges, ""),
        }
        if let Some(session) = self.multi_cursor.as_mut() {
            session.phase = MultiCursorPhase::Inserting;
        }
        self.mode = super::Mode::Insert;
        self.insert_entry_cursor = None;
        self.generated_indent = None;
        self.move_to_active_multi_cursor(true);
        true
    }

    fn collapse_multi_cursor_ranges(
        &mut self,
        ranges: &[CharRange],
        position: impl Fn(CharRange) -> usize,
    ) {
        let session = self
            .multi_cursor
            .as_mut()
            .expect("multi-cursor collapse requires a session");
        let active_range = session.selections.active_range();
        let active = ranges
            .iter()
            .position(|range| *range == active_range)
            .unwrap_or(0);
        let cursors = ranges
            .iter()
            .map(|range| {
                let position = position(*range);
                CharRange::new(position, position)
            })
            .collect();
        session.selections.replace_ranges(cursors, active);
    }

    pub(super) fn insert_at_multi_cursors(&mut self, text: &str) -> bool {
        if !self.multi_cursor_is_inserting() {
            return false;
        }
        let targets = self
            .multi_cursor
            .as_ref()
            .expect("session was checked above")
            .selections
            .ranges()
            .to_vec();
        self.replace_multi_cursor_targets(targets, text);
        self.move_to_active_multi_cursor(true);
        true
    }

    pub(super) fn delete_before_multi_cursors(&mut self) -> bool {
        if !self.multi_cursor_is_inserting() {
            return false;
        }
        let targets = self
            .multi_cursor
            .as_ref()
            .expect("session was checked above")
            .selections
            .ranges()
            .iter()
            .map(|cursor| CharRange::new(cursor.start.saturating_sub(1), cursor.start))
            .collect::<Vec<_>>();
        self.replace_multi_cursor_targets(targets, "");
        self.move_to_active_multi_cursor(true);
        true
    }

    pub(super) fn finish_multi_cursor_insert(&mut self) -> bool {
        if !self.multi_cursor_is_inserting() {
            return false;
        }
        self.mode = super::Mode::Normal;
        self.insert_entry_cursor = None;
        self.commit_transaction(self.cursor_snapshot());
        self.cancel_transaction_if_empty();
        if let Some(session) = self.multi_cursor.as_mut() {
            session.phase = MultiCursorPhase::Selecting;
        }
        self.move_to_active_multi_cursor(false);
        true
    }

    fn replace_multi_cursor_targets(&mut self, targets: Vec<CharRange>, replacement: &str) {
        let active_range = self
            .multi_cursor
            .as_ref()
            .expect("multi-cursor replacement requires a session")
            .selections
            .active_range();
        let active = self
            .multi_cursor
            .as_ref()
            .expect("multi-cursor replacement requires a session")
            .selections
            .ranges()
            .iter()
            .position(|range| *range == active_range)
            .unwrap_or(0);
        let text_ranges = targets
            .iter()
            .map(|range| {
                TextRange::new(
                    self.current_buffer().char_idx_to_position(range.start),
                    self.current_buffer().char_idx_to_position(range.end),
                )
            })
            .collect::<Vec<_>>();

        for range in text_ranges.into_iter().rev() {
            self.replace_range(range, replacement);
        }

        let replacement_len = replacement.chars().count();
        let mut shift = 0isize;
        let mut updated = Vec::with_capacity(targets.len());
        for target in targets {
            let start = target.start.saturating_add_signed(shift);
            let cursor = start + replacement_len;
            updated.push(CharRange::new(cursor, cursor));
            shift += replacement_len as isize - (target.end - target.start) as isize;
        }
        let revision = self.current_buffer().revision();
        let session = self
            .multi_cursor
            .as_mut()
            .expect("multi-cursor replacement requires a session");
        session.revision = revision;
        session.selections.replace_ranges(updated, active);
    }

    fn move_to_active_multi_cursor(&mut self, insert: bool) {
        let Some(range) = self
            .multi_cursor
            .as_ref()
            .filter(|session| session.belongs_to(self))
            .map(|session| session.selections.active_range())
        else {
            return;
        };
        let index = if insert || range.is_empty() {
            range.end
        } else {
            range.end.saturating_sub(1)
        };
        let position = self.current_buffer().char_idx_to_position(index);
        if insert {
            self.move_to_insert_text_position(position);
        } else {
            self.move_to_text_position(position);
        }
        self.refresh_cursor_goal();
    }

    pub(super) fn multi_cursor_render_ranges(
        &self,
        buffer_id: BufferId,
        window_id: WindowId,
    ) -> Vec<(TextRange, bool)> {
        self.multi_cursor
            .as_ref()
            .map(|session| {
                let collapsed = session.phase == MultiCursorPhase::Inserting
                    || session
                        .selections
                        .ranges()
                        .iter()
                        .all(|range| range.is_empty());
                session
                    .ranges_for_buffer(self, buffer_id, window_id)
                    .into_iter()
                    .map(|range| (range, collapsed))
                    .collect()
            })
            .unwrap_or_default()
    }
}
