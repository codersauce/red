use crate::{
    buffer::BufferId,
    editing::{CharRange, MotionResolver, SelectionSet},
    undo::{TextPosition, TextRange},
    unicode_utils::{
        column_to_grapheme_with_tabs, grapheme_len, grapheme_to_column_with_tabs, trim_line_ending,
    },
    window::WindowId,
};
use futures::future::BoxFuture;

use super::{Action, Content, ContentKind, Editor, RenderBuffer, Runtime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VerticalCursorDirection {
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MultiCursorMotion {
    Left,
    Right,
    WordForward,
    WordEnd,
    LineStart,
    LineEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MultiCursorSelection {
    anchor: usize,
    head: usize,
}

impl MultiCursorSelection {
    fn range(self, selection_end: Option<usize>) -> CharRange {
        let Some(selection_end) = selection_end else {
            return CharRange::new(self.head, self.head);
        };
        CharRange::new(self.anchor.min(self.head), selection_end)
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MultiCursorPasteAnchor {
    Before,
    After,
}

#[derive(Debug, Clone)]
pub(super) struct MultiCursorSession {
    buffer_id: BufferId,
    window_id: WindowId,
    revision: u64,
    phase: MultiCursorPhase,
    selections: SelectionSet,
    extend_selections: Option<Vec<MultiCursorSelection>>,
    occurrence_navigation: bool,
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
    // These async paths are reached through the recursive action dispatcher. Boxing them here
    // keeps multi-cursor support from increasing every nested dispatch frame.
    #[inline(never)]
    pub(super) fn publish_multi_cursor_edit<'a>(
        &'a mut self,
        buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            self.notify_change(runtime).await?;
            self.render(buffer)
        })
    }

    #[inline(never)]
    pub(super) fn execute_multi_cursor_action<'a>(
        &'a mut self,
        action: &'a Action,
        buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            match action {
                Action::SelectNextOccurrence => self.select_next_occurrence(),
                Action::AddCursorUp => {
                    self.add_vertical_cursor(VerticalCursorDirection::Up);
                }
                Action::AddCursorDown => {
                    self.add_vertical_cursor(VerticalCursorDirection::Down);
                }
                Action::ToggleMultiCursorExtendMode => self.toggle_multi_cursor_extend_mode(),
                Action::ExtendMultiSelectionLeft => {
                    self.extend_multi_cursor_selections(MultiCursorMotion::Left);
                }
                Action::ExtendMultiSelectionRight => {
                    self.extend_multi_cursor_selections(MultiCursorMotion::Right);
                }
                Action::ExtendMultiSelectionWordForward => {
                    self.extend_multi_cursor_selections(MultiCursorMotion::WordForward);
                }
                Action::ExtendMultiSelectionWordEnd => {
                    self.extend_multi_cursor_selections(MultiCursorMotion::WordEnd);
                }
                Action::ExtendMultiSelectionLineStart => {
                    self.extend_multi_cursor_selections(MultiCursorMotion::LineStart);
                }
                Action::ExtendMultiSelectionLineEnd => {
                    self.extend_multi_cursor_selections(MultiCursorMotion::LineEnd);
                }
                Action::InvertMultiSelection => self.invert_multi_cursor_selections(),
                Action::SelectPreviousOccurrence => self.select_previous_occurrence(),
                Action::SkipMultiSelection => self.skip_multi_cursor_occurrence(),
                Action::RemoveActiveMultiSelection => {
                    self.remove_active_multi_cursor_selection();
                }
                Action::ChangeMultiSelection => {
                    if self.begin_multi_cursor_insert(MultiCursorInsertAnchor::Replace) {
                        self.notify_change(runtime).await?;
                    }
                }
                Action::InsertAtMultiSelectionStart => {
                    self.begin_multi_cursor_insert(MultiCursorInsertAnchor::Start);
                }
                Action::AppendAtMultiSelectionEnd => {
                    self.begin_multi_cursor_insert(MultiCursorInsertAnchor::End);
                }
                Action::DeleteMultiSelection => {
                    if self.delete_multi_cursor_selections(/*preserve_register*/ false) {
                        self.notify_change(runtime).await?;
                    }
                }
                Action::DeleteMultiSelectionBlackHole => {
                    if self.delete_multi_cursor_selections(/*preserve_register*/ true) {
                        self.notify_change(runtime).await?;
                    }
                }
                Action::PasteAfterMultiSelection => {
                    if self.paste_at_multi_cursors(MultiCursorPasteAnchor::After) {
                        self.notify_change(runtime).await?;
                    }
                }
                Action::PasteBeforeMultiSelection => {
                    if self.paste_at_multi_cursors(MultiCursorPasteAnchor::Before) {
                        self.notify_change(runtime).await?;
                    }
                }
                Action::YankMultiSelection => {
                    self.yank_multi_cursor_selections();
                }
                Action::ClearMultiSelection => self.clear_multi_cursor(),
                _ => unreachable!("non-multi-cursor action reached multi-cursor dispatcher"),
            }
            self.render(buffer)?;
            Ok(())
        })
    }

    pub(super) fn has_multi_cursor_session(&self) -> bool {
        self.multi_cursor
            .as_ref()
            .is_some_and(|session| session.belongs_to(self))
    }

    pub(super) fn can_navigate_multi_cursor_occurrences(&self) -> bool {
        self.multi_cursor.as_ref().is_some_and(|session| {
            session.belongs_to(self)
                && session.occurrence_navigation
                && session.phase == MultiCursorPhase::Selecting
                && session
                    .selections
                    .ranges()
                    .iter()
                    .all(|range| !range.is_empty())
        })
    }

    pub(super) fn has_multi_cursor_selections(&self) -> bool {
        self.multi_cursor.as_ref().is_some_and(|session| {
            session.belongs_to(self)
                && session.phase == MultiCursorPhase::Selecting
                && session
                    .selections
                    .ranges()
                    .iter()
                    .all(|range| !range.is_empty())
        })
    }

    pub(super) fn multi_cursor_is_extending(&self) -> bool {
        self.multi_cursor.as_ref().is_some_and(|session| {
            session.belongs_to(self)
                && session.phase == MultiCursorPhase::Selecting
                && session.extend_selections.is_some()
        })
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
        if self.can_navigate_multi_cursor_occurrences() {
            let session = self
                .multi_cursor
                .as_mut()
                .expect("session was checked above");
            session.selections.select_next();
            self.refresh_multi_cursor_extend_selections();
        } else if self.has_collapsed_multi_cursor_session() {
            self.promote_multi_cursors_to_words();
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
            let extend_selections = selections
                .ranges()
                .iter()
                .copied()
                .map(|range| self.multi_cursor_selection_from_range(range))
                .collect();
            self.multi_cursor = Some(MultiCursorSession {
                buffer_id,
                window_id,
                revision,
                phase: MultiCursorPhase::Selecting,
                extend_selections: Some(extend_selections),
                occurrence_navigation: true,
                selections,
            });
        }

        self.move_to_active_multi_cursor(false);
    }

    fn has_collapsed_multi_cursor_session(&self) -> bool {
        self.multi_cursor.as_ref().is_some_and(|session| {
            session.belongs_to(self)
                && session.phase == MultiCursorPhase::Selecting
                && session
                    .selections
                    .ranges()
                    .iter()
                    .all(|range| range.is_empty())
        })
    }

    fn promote_multi_cursors_to_words(&mut self) {
        let session = self
            .multi_cursor
            .as_ref()
            .expect("collapsed session was checked above");
        let active_range = session.selections.active_range();
        let active = session
            .selections
            .ranges()
            .iter()
            .position(|range| *range == active_range)
            .unwrap_or(0);
        let ranges = session
            .selections
            .ranges()
            .iter()
            .map(|cursor| {
                let position = self.current_buffer().char_idx_to_position(cursor.start);
                SelectionSet::range_at_cursor(self.current_buffer(), position).unwrap_or(*cursor)
            })
            .collect::<Vec<_>>();
        let extend_selections = ranges
            .iter()
            .copied()
            .map(|range| self.multi_cursor_selection_from_range(range))
            .collect();
        let session = self
            .multi_cursor
            .as_mut()
            .expect("collapsed session was checked above");
        session.selections.replace_ranges(ranges, active);
        session.extend_selections = Some(extend_selections);
        session.occurrence_navigation = false;
    }

    pub(super) fn add_vertical_cursor(&mut self, direction: VerticalCursorDirection) {
        let compatible_session = self.multi_cursor.as_ref().is_some_and(|session| {
            session.belongs_to(self)
                && session.phase == MultiCursorPhase::Selecting
                && session.extend_selections.is_none()
                && session
                    .selections
                    .ranges()
                    .iter()
                    .all(|range| range.is_empty())
        });
        if self.has_multi_cursor_session() && !compatible_session {
            return;
        }

        let (origin, origin_line, display_column) = if compatible_session {
            let session = self
                .multi_cursor
                .as_ref()
                .expect("compatible session was checked above");
            let origin = session.selections.active_range();
            let origin_line = self
                .current_buffer()
                .char_idx_to_position(origin.start)
                .line;
            let display_column = self.current_cursor_display_col();
            (origin, origin_line, display_column)
        } else {
            let position = self.cursor_text_position();
            let index = self.current_buffer().position_to_char_idx(position);
            (
                CharRange::new(index, index),
                position.line,
                self.current_cursor_display_col(),
            )
        };

        let Some(target) = self.vertical_cursor_target(origin_line, display_column, direction)
        else {
            return;
        };

        if compatible_session {
            let session = self
                .multi_cursor
                .as_mut()
                .expect("compatible session was checked above");
            session.selections.add_or_activate(target);
        } else {
            let buffer_id = self.current_buffer().id();
            let Some(window_id) = self.window_manager.active_stable_window_id() else {
                return;
            };
            let revision = self.current_buffer().revision();
            let Some(selections) = SelectionSet::from_ranges(vec![origin, target], 1) else {
                return;
            };
            self.multi_cursor = Some(MultiCursorSession {
                buffer_id,
                window_id,
                revision,
                phase: MultiCursorPhase::Selecting,
                selections,
                extend_selections: None,
                occurrence_navigation: false,
            });
        }

        self.move_to_active_multi_cursor(false);
    }

    fn vertical_cursor_target(
        &self,
        origin_line: usize,
        display_column: usize,
        direction: VerticalCursorDirection,
    ) -> Option<CharRange> {
        let last_line = self.current_buffer().len();
        let tab_width = self.active_tab_width();
        let mut line_index = origin_line;

        loop {
            line_index = match direction {
                VerticalCursorDirection::Up => line_index.checked_sub(1)?,
                VerticalCursorDirection::Down => {
                    if line_index >= last_line {
                        return None;
                    }
                    line_index + 1
                }
            };
            let line = self.current_buffer().get(line_index)?;
            let line = trim_line_ending(&line);
            let grapheme = column_to_grapheme_with_tabs(line, display_column, tab_width);
            let exact_column =
                grapheme_to_column_with_tabs(line, grapheme, tab_width) == display_column;
            let cursor_exists = display_column == 0 || grapheme < grapheme_len(line);
            if !exact_column || !cursor_exists {
                continue;
            }

            let character = self.grapheme_to_char_on_line(grapheme, line_index);
            let index = self
                .current_buffer()
                .position_to_char_idx(TextPosition::new(line_index, character));
            return Some(CharRange::new(index, index));
        }
    }

    pub(super) fn toggle_multi_cursor_extend_mode(&mut self) {
        if !self.has_multi_cursor_session() {
            return;
        }
        if self.multi_cursor_is_extending() {
            self.collapse_multi_cursor_to_heads();
        } else {
            self.enter_multi_cursor_extend_mode();
        }
        self.move_to_active_multi_cursor(false);
    }

    pub(super) fn extend_multi_cursor_selections(&mut self, motion: MultiCursorMotion) {
        if !self.has_multi_cursor_session() {
            return;
        }
        if !self.multi_cursor_is_extending() {
            self.enter_multi_cursor_extend_mode();
        }

        let session = self
            .multi_cursor
            .as_ref()
            .expect("session was checked above");
        let active_range = session.selections.active_range();
        let active = session
            .selections
            .ranges()
            .iter()
            .position(|range| *range == active_range)
            .unwrap_or(0);
        let mut selections = session
            .extend_selections
            .clone()
            .expect("extend mode requires oriented selections");
        for selection in &mut selections {
            selection.head = self.multi_cursor_motion_target(selection.head, motion);
        }
        let ranges = selections
            .iter()
            .map(|selection| {
                selection
                    .range(self.multi_cursor_grapheme_end(selection.anchor.max(selection.head)))
            })
            .collect();
        let session = self
            .multi_cursor
            .as_mut()
            .expect("session was checked above");
        session.selections.replace_ranges(ranges, active);
        session.extend_selections = Some(selections);
        session.occurrence_navigation = false;
        self.move_to_active_multi_cursor(false);
    }

    pub(super) fn invert_multi_cursor_selections(&mut self) {
        if !self.multi_cursor_is_extending() {
            return;
        }
        let session = self
            .multi_cursor
            .as_mut()
            .expect("extend session was checked above");
        for selection in session
            .extend_selections
            .as_mut()
            .expect("extend mode requires oriented selections")
        {
            std::mem::swap(&mut selection.anchor, &mut selection.head);
        }
        self.move_to_active_multi_cursor(false);
    }

    fn enter_multi_cursor_extend_mode(&mut self) {
        let session = self
            .multi_cursor
            .as_ref()
            .expect("multi-cursor extend requires a session");
        let active_range = session.selections.active_range();
        let active = session
            .selections
            .ranges()
            .iter()
            .position(|range| *range == active_range)
            .unwrap_or(0);
        let ranges = session.selections.ranges().to_vec();
        let selections = ranges
            .iter()
            .map(|range| self.multi_cursor_selection_from_range(*range))
            .collect::<Vec<_>>();
        let ranges = selections
            .iter()
            .map(|selection| {
                selection
                    .range(self.multi_cursor_grapheme_end(selection.anchor.max(selection.head)))
            })
            .collect();
        let session = self
            .multi_cursor
            .as_mut()
            .expect("multi-cursor extend requires a session");
        session.selections.replace_ranges(ranges, active);
        session.extend_selections = Some(selections);
    }

    fn collapse_multi_cursor_to_heads(&mut self) {
        let session = self
            .multi_cursor
            .as_ref()
            .expect("multi-cursor collapse requires a session");
        let active_range = session.selections.active_range();
        let active = session
            .selections
            .ranges()
            .iter()
            .position(|range| *range == active_range)
            .unwrap_or(0);
        let cursors = session
            .extend_selections
            .as_ref()
            .expect("extend mode requires oriented selections")
            .iter()
            .map(|selection| CharRange::new(selection.head, selection.head))
            .collect();
        let session = self
            .multi_cursor
            .as_mut()
            .expect("multi-cursor collapse requires a session");
        session.selections.replace_ranges(cursors, active);
        session.extend_selections = None;
        session.occurrence_navigation = false;
    }

    fn multi_cursor_motion_target(&self, index: usize, motion: MultiCursorMotion) -> usize {
        let position = self.current_buffer().char_idx_to_position(index);
        let line = position.line;
        let line_contents = self.current_buffer().get(line).unwrap_or_default();
        let line_contents = trim_line_ending(&line_contents);
        let line_graphemes = grapheme_len(line_contents);
        let current_grapheme = self.char_to_grapheme_on_line(position.character, line);
        let last_grapheme = line_graphemes.saturating_sub(1);
        let target = match motion {
            MultiCursorMotion::Left => current_grapheme.saturating_sub(1),
            MultiCursorMotion::Right => current_grapheme.saturating_add(1).min(last_grapheme),
            MultiCursorMotion::LineStart => 0,
            MultiCursorMotion::LineEnd => last_grapheme,
            MultiCursorMotion::WordForward | MultiCursorMotion::WordEnd => {
                let end = motion == MultiCursorMotion::WordEnd;
                let target = MotionResolver::new(self.current_buffer(), position)
                    .word_target(
                        /*count*/ 1, /*backward*/ false, end, /*big_word*/ false,
                    )
                    .filter(|target| target.line == line);
                return target
                    .map(|target| self.current_buffer().position_to_char_idx(target))
                    .unwrap_or_else(|| {
                        self.current_buffer()
                            .position_to_char_idx(TextPosition::new(
                                line,
                                self.grapheme_to_char_on_line(last_grapheme, line),
                            ))
                    });
            }
        };
        self.current_buffer()
            .position_to_char_idx(TextPosition::new(
                line,
                self.grapheme_to_char_on_line(target, line),
            ))
    }

    fn multi_cursor_grapheme_end(&self, index: usize) -> Option<usize> {
        let position = self.current_buffer().char_idx_to_position(index);
        let line_length = self.line_character_len(position.line);
        if position.character >= line_length {
            return None;
        }
        let grapheme = self.char_to_grapheme_on_line(position.character, position.line);
        let end = self.grapheme_to_char_on_line(grapheme + 1, position.line);
        Some(
            self.current_buffer()
                .position_to_char_idx(TextPosition::new(position.line, end)),
        )
    }

    fn multi_cursor_selection_from_range(&self, range: CharRange) -> MultiCursorSelection {
        if range.is_empty() {
            return MultiCursorSelection {
                anchor: range.start,
                head: range.start,
            };
        }
        let last_character = range.end - 1;
        let position = self.current_buffer().char_idx_to_position(last_character);
        let grapheme = self.char_to_grapheme_on_line(position.character, position.line);
        let head = self
            .current_buffer()
            .position_to_char_idx(TextPosition::new(
                position.line,
                self.grapheme_to_char_on_line(grapheme, position.line),
            ));
        MultiCursorSelection {
            anchor: range.start,
            head,
        }
    }

    fn refresh_multi_cursor_extend_selections(&mut self) {
        let Some(ranges) = self
            .multi_cursor
            .as_ref()
            .map(|session| session.selections.ranges().to_vec())
        else {
            return;
        };
        let selections = ranges
            .into_iter()
            .map(|range| self.multi_cursor_selection_from_range(range))
            .collect();
        self.multi_cursor
            .as_mut()
            .expect("multi-cursor session was checked above")
            .extend_selections = Some(selections);
    }

    pub(super) fn select_previous_occurrence(&mut self) {
        if !self.can_navigate_multi_cursor_occurrences() {
            self.multi_cursor = None;
            return;
        }
        let session = self
            .multi_cursor
            .as_mut()
            .expect("session was checked above");
        session.selections.select_previous();
        self.refresh_multi_cursor_extend_selections();
        self.move_to_active_multi_cursor(false);
    }

    pub(super) fn skip_multi_cursor_occurrence(&mut self) {
        if !self.can_navigate_multi_cursor_occurrences() {
            self.multi_cursor = None;
            return;
        }
        let session = self
            .multi_cursor
            .as_mut()
            .expect("session was checked above");
        session.selections.skip_active();
        self.refresh_multi_cursor_extend_selections();
        self.move_to_active_multi_cursor(false);
    }

    pub(super) fn remove_active_multi_cursor_selection(&mut self) {
        if !self.can_navigate_multi_cursor_occurrences() {
            self.multi_cursor = None;
            return;
        }
        let retained_selection = self
            .multi_cursor
            .as_mut()
            .expect("session was checked above")
            .selections
            .remove_active();
        if !retained_selection {
            self.multi_cursor = None;
            return;
        }
        self.refresh_multi_cursor_extend_selections();
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
        session.extend_selections = None;
        session.occurrence_navigation = false;
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
            session.extend_selections = None;
        }
        self.move_to_active_multi_cursor(false);
        true
    }

    pub(super) fn delete_multi_cursor_selections(&mut self, preserve_register: bool) -> bool {
        if !self.has_multi_cursor_selections() {
            return false;
        }
        let targets = self
            .multi_cursor
            .as_ref()
            .expect("session was checked above")
            .selections
            .ranges()
            .to_vec();
        let deleted = targets
            .iter()
            .map(|range| {
                self.current_buffer().text_in_range(TextRange::new(
                    self.current_buffer().char_idx_to_position(range.start),
                    self.current_buffer().char_idx_to_position(range.end),
                ))
            })
            .collect::<Vec<_>>();

        self.begin_transaction("delete multi-cursor selections");
        if !preserve_register {
            self.set_default_register(Content::multi_cursor_blockwise(deleted));
        }
        self.replace_multi_cursor_targets(targets, "");
        self.move_to_active_multi_cursor(false);
        self.commit_transaction(self.cursor_snapshot());
        true
    }

    pub(super) fn yank_multi_cursor_selections(&mut self) -> bool {
        if !self.has_multi_cursor_selections() {
            return false;
        }
        let ranges = self
            .multi_cursor
            .as_ref()
            .expect("session was checked above")
            .selections
            .ranges()
            .to_vec();
        let yanked = ranges
            .iter()
            .map(|range| {
                self.current_buffer().text_in_range(TextRange::new(
                    self.current_buffer().char_idx_to_position(range.start),
                    self.current_buffer().char_idx_to_position(range.end),
                ))
            })
            .collect();

        self.set_default_register(Content::multi_cursor_blockwise(yanked));
        self.collapse_multi_cursor_ranges(&ranges, |range| range.start);
        self.move_to_active_multi_cursor(false);
        true
    }

    pub(super) fn paste_at_multi_cursors(&mut self, anchor: MultiCursorPasteAnchor) -> bool {
        if !self.has_multi_cursor_session() {
            return false;
        }
        let extending = self.multi_cursor_is_extending();
        self.refresh_default_register_from_system_clipboard();
        let Some(content) = self.registers.get(&super::DEFAULT_REGISTER).cloned() else {
            return false;
        };
        let targets = self
            .multi_cursor
            .as_ref()
            .expect("session was checked above")
            .selections
            .ranges()
            .to_vec();
        if targets.is_empty() || content.text.is_empty() {
            return false;
        }

        if content.kind == ContentKind::Linewise {
            return self.paste_linewise_at_multi_cursors(targets, &content);
        }

        let selecting = targets.iter().any(|range| !range.is_empty());
        let replaced = targets
            .iter()
            .map(|range| {
                self.current_buffer().text_in_range(TextRange::new(
                    self.current_buffer().char_idx_to_position(range.start),
                    self.current_buffer().char_idx_to_position(range.end),
                ))
            })
            .collect::<Vec<_>>();
        let replacements =
            self.multi_cursor_paste_replacements(&content, &replaced, targets.len(), selecting);

        self.begin_transaction("paste at multi-cursors");
        if selecting {
            self.apply_multi_cursor_replacements(targets, replacements, |start, replacement| {
                CharRange::new(start, start + replacement.chars().count())
            });
        } else {
            let insertion_targets = targets
                .iter()
                .map(|range| {
                    let cursor = self.normal_multi_cursor_index(range.start);
                    let position = self.current_buffer().char_idx_to_position(cursor);
                    let line_len = self.line_character_len(position.line);
                    let insertion = match anchor {
                        MultiCursorPasteAnchor::Before => cursor,
                        MultiCursorPasteAnchor::After if line_len == 0 => cursor,
                        MultiCursorPasteAnchor::After => cursor + 1,
                    };
                    CharRange::new(insertion, insertion)
                })
                .collect();
            self.apply_multi_cursor_replacements(
                insertion_targets,
                replacements,
                |start, replacement| {
                    let replacement_len = replacement.chars().count();
                    let cursor = match anchor {
                        MultiCursorPasteAnchor::Before => start,
                        MultiCursorPasteAnchor::After => start + replacement_len.saturating_sub(1),
                    };
                    CharRange::new(cursor, cursor)
                },
            );
        }
        if selecting && extending {
            self.refresh_multi_cursor_extend_selections();
        }
        self.move_to_active_multi_cursor(false);
        self.commit_transaction(self.cursor_snapshot());
        self.cancel_transaction_if_empty();
        true
    }

    fn multi_cursor_paste_replacements(
        &self,
        content: &Content,
        replaced: &[String],
        count: usize,
        selecting: bool,
    ) -> Vec<String> {
        if content.kind == ContentKind::Charwise {
            return vec![content.text.clone(); count];
        }

        let mut replacements = if content.multi_cursor_segments.is_empty() {
            content.text.lines().map(str::to_string).collect::<Vec<_>>()
        } else {
            content.multi_cursor_segments.clone()
        };
        replacements.truncate(count);
        if replacements.len() < count {
            if selecting {
                replacements.extend(replaced.iter().skip(replacements.len()).cloned());
            } else {
                replacements.resize(count, String::new());
            }
        }
        replacements
    }

    fn paste_linewise_at_multi_cursors(
        &mut self,
        targets: Vec<CharRange>,
        content: &Content,
    ) -> bool {
        let lines = content.text.lines().collect::<Vec<_>>().join("\n");
        if lines.is_empty() {
            return false;
        }
        let selecting = targets.iter().any(|range| !range.is_empty());
        let active_range = self
            .multi_cursor
            .as_ref()
            .expect("session was checked above")
            .selections
            .active_range();
        let active = targets
            .iter()
            .position(|range| *range == active_range)
            .unwrap_or(0);
        let source_lines = targets
            .iter()
            .map(|range| {
                let cursor = self.normal_multi_cursor_index(range.start);
                self.current_buffer().char_idx_to_position(cursor).line
            })
            .collect::<Vec<_>>();
        let active_source_line = source_lines.get(active).copied();
        let mut replacements = Vec::with_capacity(targets.len());
        let insertion_targets = targets
            .into_iter()
            .map(|range| {
                let cursor = self.normal_multi_cursor_index(range.start);
                let position = self.current_buffer().char_idx_to_position(cursor);
                let last_unterminated_line = position.line == self.current_buffer().len()
                    && self
                        .current_buffer()
                        .get(position.line)
                        .is_some_and(|line| !line.ends_with('\n'));
                if last_unterminated_line {
                    replacements.push(format!("\n{lines}"));
                    let end = self.current_buffer().contents().chars().count();
                    CharRange::new(end, end)
                } else {
                    replacements.push(format!("{lines}\n"));
                    let start = self
                        .current_buffer()
                        .position_to_char_idx(crate::undo::TextPosition::new(position.line + 1, 0));
                    CharRange::new(start, start)
                }
            })
            .collect();

        self.begin_transaction("paste lines at multi-cursors");
        self.apply_multi_cursor_replacements(
            insertion_targets,
            replacements,
            |start, replacement| {
                let cursor = start + usize::from(replacement.starts_with('\n'));
                CharRange::new(cursor, cursor)
            },
        );
        if selecting {
            let updated = self
                .multi_cursor
                .as_ref()
                .expect("multi-cursor replacement requires a session")
                .selections
                .ranges()
                .to_vec();
            let mut retained_lines = Vec::new();
            let mut retained = Vec::new();
            let mut active = 0;
            for (source_line, range) in source_lines.into_iter().zip(updated) {
                if retained_lines.contains(&source_line) {
                    continue;
                }
                if Some(source_line) == active_source_line {
                    active = retained.len();
                }
                retained_lines.push(source_line);
                retained.push(range);
            }
            self.multi_cursor
                .as_mut()
                .expect("multi-cursor replacement requires a session")
                .selections
                .replace_ranges(retained, active);
        }
        self.move_to_active_multi_cursor(false);
        self.commit_transaction(self.cursor_snapshot());
        true
    }

    fn normal_multi_cursor_index(&self, index: usize) -> usize {
        let position = self.current_buffer().char_idx_to_position(index);
        let line_len = self.line_character_len(position.line);
        let character = if line_len == 0 {
            0
        } else {
            position.character.min(line_len - 1)
        };
        self.current_buffer()
            .position_to_char_idx(crate::undo::TextPosition::new(position.line, character))
    }

    fn apply_multi_cursor_replacements(
        &mut self,
        targets: Vec<CharRange>,
        replacements: Vec<String>,
        result_range: impl Fn(usize, &str) -> CharRange,
    ) {
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
        let edits = targets
            .iter()
            .zip(&replacements)
            .map(|(range, replacement)| {
                (
                    TextRange::new(
                        self.current_buffer().char_idx_to_position(range.start),
                        self.current_buffer().char_idx_to_position(range.end),
                    ),
                    replacement,
                )
            })
            .collect::<Vec<_>>();
        for (range, replacement) in edits.into_iter().rev() {
            self.replace_range(range, replacement);
        }

        let mut shift = 0isize;
        let updated = targets
            .into_iter()
            .zip(replacements)
            .map(|(target, replacement)| {
                let start = target.start.saturating_add_signed(shift);
                shift +=
                    replacement.chars().count() as isize - (target.end - target.start) as isize;
                result_range(start, &replacement)
            })
            .collect();
        let revision = self.current_buffer().revision();
        let session = self
            .multi_cursor
            .as_mut()
            .expect("multi-cursor replacement requires a session");
        session.revision = revision;
        session.selections.replace_ranges(updated, active);
        session.extend_selections = None;
        session.occurrence_navigation = false;
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
        let active_range = updated.get(active).copied();
        updated.dedup();
        let active = active_range
            .and_then(|active_range| updated.iter().position(|range| *range == active_range))
            .unwrap_or(0);
        let revision = self.current_buffer().revision();
        let session = self
            .multi_cursor
            .as_mut()
            .expect("multi-cursor replacement requires a session");
        session.revision = revision;
        session.selections.replace_ranges(updated, active);
        session.extend_selections = None;
        session.occurrence_navigation = false;
    }

    fn move_to_active_multi_cursor(&mut self, insert: bool) {
        let Some((range, extend_head)) = self
            .multi_cursor
            .as_ref()
            .filter(|session| session.belongs_to(self))
            .map(|session| {
                let range = session.selections.active_range();
                let active = session.selections.active_selection().0.saturating_sub(1);
                let extend_head = session
                    .extend_selections
                    .as_ref()
                    .and_then(|selections| selections.get(active))
                    .map(|selection| selection.head);
                (range, extend_head)
            })
        else {
            return;
        };
        let index = if let Some(head) = extend_head {
            head
        } else if insert || range.is_empty() {
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

    pub(super) fn multi_cursor_status_label(&self) -> Option<String> {
        let session = self
            .multi_cursor
            .as_ref()
            .filter(|session| session.belongs_to(self))?;
        let (current, total) = session.selections.active_selection();
        let mode = match session.phase {
            MultiCursorPhase::Selecting => "MULTI",
            MultiCursorPhase::Inserting => "MULTI-I",
        };
        Some(format!("{mode} {current}/{total}"))
    }
}
