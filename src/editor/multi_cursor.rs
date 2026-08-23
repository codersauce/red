use crate::{
    buffer::BufferId,
    editing::{CharRange, SelectionSet},
    undo::TextRange,
    window::WindowId,
};

use super::{Content, ContentKind, Editor};

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

    pub(super) fn can_navigate_multi_cursor_occurrences(&self) -> bool {
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

    pub(super) fn delete_multi_cursor_selections(&mut self, preserve_register: bool) -> bool {
        if !self.can_navigate_multi_cursor_occurrences() {
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

    pub(super) fn paste_at_multi_cursors(&mut self, anchor: MultiCursorPasteAnchor) -> bool {
        if !self.has_multi_cursor_session() {
            return false;
        }
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
