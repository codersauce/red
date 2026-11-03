//! Editor adapter for the experimental Helix grammar.
//!
//! The grammar and selection math live in [`crate::felix`], which is the code
//! that can later move into the `felix` crate. This module only connects that
//! kernel to Red's buffers, transactions, pickers, and LSP.

use std::{
    collections::{HashMap, HashSet},
    sync::OnceLock,
};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::{
    buffer::BufferId,
    config::{Config, EditingModel, KeyAction},
    editing::{MotionResolver, TextObjectScope, text_object_kind_for_key},
    felix::{
        self, Command, KeyNode, Range, Selection, SurroundOp, TextEdit, extend_command, motion_for,
        normal_keymap,
    },
    textobjects::SyntaxObjectKind,
    undo::{TextPosition, TextRange},
};

use super::{
    Action, Content, ContentKind, DEFAULT_REGISTER, Editor, Mode, RenderBuffer, Runtime,
    SearchDirection,
};

#[derive(Debug, Clone, Default)]
pub(super) struct State {
    prompt: Option<Prompt>,
    pending: Option<Pending>,
    history: HashMap<(BufferId, u64), Selection>,
    pub(super) insert_cursors: Vec<usize>,
    insert_primary: usize,
    insert_anchor: usize,
    insert_extend: bool,
    syntax_history: Vec<Range>,
    pub(super) reference_requests: HashSet<i64>,
    search_extend: bool,
}

#[derive(Debug, Clone)]
struct Prompt {
    command: Command,
    draft: String,
}

#[derive(Debug, Clone, Copy)]
struct Pending {
    command: Command,
    count: u16,
    first: Option<char>,
}

impl Editor {
    pub(super) fn is_helix(&self) -> bool {
        self.config.editing_model == EditingModel::Helix
    }

    /// Esc and `v` leave select mode from every state: a count, a pending
    /// character, or an open keymap menu. `v` is still the argument while a
    /// character prompt is waiting (`fv`), and Esc is what leaves that prompt.
    /// Ranges are left untouched.
    pub(super) fn helix_select_exit_action(&mut self, event: &Event) -> Option<KeyAction> {
        if !self.is_helix() || self.mode != Mode::Select {
            return None;
        }
        let Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) = event
        else {
            return None;
        };
        let exit = match code {
            KeyCode::Esc if modifiers.is_empty() => true,
            KeyCode::Char('v') if modifiers.is_empty() && self.felix.pending.is_none() => true,
            _ => false,
        };
        if !exit {
            return None;
        }
        self.repeater = None;
        self.felix.pending = None;
        self.waiting_command = None;
        self.waiting_key_action = None;
        self.clear_keymap_hints();
        Some(KeyAction::Single(Action::EnterMode(Mode::Normal)))
    }

    pub(super) fn handle_felix_event(&mut self, event: &Event) -> Option<KeyAction> {
        if self.felix.prompt.is_some() {
            return self.handle_felix_prompt(event);
        }
        if let Some(action) = self.helix_select_exit_action(event) {
            return Some(action);
        }
        if self.felix.pending.is_some() {
            return self.handle_felix_pending_key(event);
        }
        if let Some(action) = self.felix_directory_action(event) {
            return Some(KeyAction::Single(action));
        }
        if let Event::Key(KeyEvent {
            code: KeyCode::Char('%'),
            modifiers: KeyModifiers::NONE | KeyModifiers::SHIFT,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) = event
        {
            if self.repeater.take().is_some() {
                return Some(KeyAction::Single(Action::Felix(Command::SelectAll)));
            }
        }
        let mode = self.mode;
        let mappings = self.felix_keymap(mode);
        self.event_to_key_action(&mappings, event)
    }

    pub(super) fn handle_felix_multi_insert(&mut self, event: &Event) -> Option<KeyAction> {
        let Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) = event
        else {
            return None;
        };
        if *code == KeyCode::Esc {
            return Some(KeyAction::Single(Action::EnterMode(Mode::Normal)));
        }
        if !modifiers.is_empty() && *modifiers != KeyModifiers::SHIFT {
            return Some(KeyAction::None);
        }
        match code {
            KeyCode::Char(character) => {
                self.felix_insert_text(&character.to_string());
                Some(KeyAction::None)
            }
            KeyCode::Enter => {
                self.felix_insert_text("\n");
                Some(KeyAction::None)
            }
            KeyCode::Backspace => {
                self.felix_backspace_cursors();
                Some(KeyAction::None)
            }
            _ => Some(KeyAction::None),
        }
    }

    pub(super) async fn execute_felix(
        &mut self,
        command: Command,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        if command.needs_character() {
            self.felix.pending = Some(Pending {
                command,
                count: self.repeater.take().unwrap_or(1),
                first: None,
            });
            self.waiting_command = Some(command.label());
            self.draw_commandline(buffer);
            return Ok(());
        }
        if command.needs_prompt() {
            self.felix.prompt = Some(Prompt {
                command,
                draft: String::new(),
            });
            self.set_legacy_message(Some(format!("{}: ", command.label())));
            self.draw_commandline(buffer);
            return Ok(());
        }
        let count = self.repeater.take();
        self.run_felix_command(command, count, None, buffer, runtime)
            .await
    }

    pub(super) fn finish_felix_mode_transition(&mut self, old_mode: Mode, new_mode: Mode) {
        if !self.is_helix() {
            return;
        }
        if old_mode == Mode::Insert && new_mode == Mode::Normal {
            let cursors = std::mem::take(&mut self.felix.insert_cursors);
            if cursors.len() > 1 {
                let ranges = cursors.into_iter().map(Range::cursor).collect();
                let primary = self.felix.insert_primary;
                self.set_felix_selection(Selection::from_ranges(ranges, primary));
            }
            self.felix.insert_extend = false;
            self.remember_felix_selection();
        }
        if new_mode == Mode::Search {
            self.felix.search_extend = old_mode == Mode::Select;
        }
        if new_mode == Mode::Normal {
            self.felix.pending = None;
            self.felix.prompt = None;
        }
    }

    pub(super) fn remember_felix_selection(&mut self) {
        if !self.is_helix() {
            return;
        }
        let id = self.current_buffer().id();
        let revision = self.current_buffer().undo_history.history_revision();
        let selection = self.felix_selection();
        self.felix.history.insert((id, revision), selection);
    }

    pub(super) fn restore_felix_selection(&mut self) {
        if !self.is_helix() {
            return;
        }
        let id = self.current_buffer().id();
        let revision = self.current_buffer().undo_history.history_revision();
        if let Some(selection) = self.felix.history.get(&(id, revision)).cloned() {
            self.set_felix_selection(selection);
        } else {
            self.set_felix_selection(Selection::cursor(self.cursor_char_index()));
        }
    }

    pub(super) fn felix_select_search_match(&mut self, start: TextPosition, end: TextPosition) {
        if !self.is_helix() {
            return;
        }
        let buffer = self.current_buffer();
        let start = buffer.position_to_char_idx(start);
        let end = buffer.position_to_char_idx(end);
        if end <= start {
            return;
        }
        let range = Range::covering(start, end - 1, true);
        let mut selection = self.felix_selection();
        if self.felix.search_extend || self.mode == Mode::Select {
            let len = self.current_buffer().contents_snapshot().len_chars();
            selection.push_range(range, len);
        } else {
            selection = Selection::single(range);
        }
        self.set_felix_selection(selection);
    }

    pub(super) fn felix_wide_lines(
        &self,
        window: &crate::window::Window,
    ) -> Option<std::ops::Range<usize>> {
        if !self.is_helix() {
            return None;
        }
        let buffer = self.buffer_manager.get(window.buffer_index)?;
        let len = buffer.contents_snapshot().len_chars();
        let mut first = None;
        let mut last = None;
        for range in window.selection.ranges() {
            let (start, end) = range.span(len);
            if end.saturating_sub(start) <= 1 {
                continue;
            }
            let start_line = buffer.char_idx_to_position(start).line;
            let end_line = buffer.char_idx_to_position(end.saturating_sub(1)).line;
            first = Some(first.map_or(start_line, |line: usize| line.min(start_line)));
            last = Some(last.map_or(end_line, |line: usize| line.max(end_line)));
        }
        Some(first?..last? + 1)
    }

    pub(super) fn felix_references_from_locations(&mut self, locations: &[serde_json::Value]) {
        let Some(current) = self.current_buffer().file.clone() else {
            self.set_legacy_message(Some("references need a named file".to_string()));
            return;
        };
        let mut ranges = Vec::new();
        for location in locations {
            let Some(uri) = location.get("uri").and_then(|value| value.as_str()) else {
                continue;
            };
            if self.uri_to_file(uri) != current {
                continue;
            }
            let Some(range) = location.get("range") else {
                continue;
            };
            let Some(start) = lsp_char_position(range.get("start")) else {
                continue;
            };
            let Some(end) = lsp_char_position(range.get("end")) else {
                continue;
            };
            let buffer = self.current_buffer();
            let start = buffer.position_to_char_idx(TextPosition::new(start.0, start.1));
            let end = buffer.position_to_char_idx(TextPosition::new(end.0, end.1));
            if end > start {
                ranges.push(Range::covering(start, end - 1, true));
            }
        }
        if ranges.is_empty() {
            self.set_legacy_message(Some("no references in this file".to_string()));
            return;
        }
        let mut selection = Selection::from_ranges(ranges, 0);
        selection.normalize(self.current_buffer().contents_snapshot().len_chars());
        self.set_felix_selection(selection);
        self.set_legacy_message(Some(format!(
            "{} references selected",
            self.felix_selection().len()
        )));
    }

    #[doc(hidden)]
    pub fn test_felix_markup(&self) -> String {
        let text = self.current_buffer().contents();
        let selection = self
            .window_manager
            .active_window()
            .map(|window| window.selection.clone())
            .unwrap_or_default();
        markup(&text, &selection)
    }

    fn felix_selection(&self) -> Selection {
        self.window_manager
            .active_window()
            .map(|window| window.selection.clone())
            .unwrap_or_default()
    }

    fn set_felix_selection(&mut self, mut selection: Selection) {
        let len = self.current_buffer().contents_snapshot().len_chars();
        selection.normalize(len);
        if let Some(window) = self.window_manager.active_window_mut() {
            window.selection = selection;
        }
        self.place_felix_cursor();
    }

    pub(super) fn draw_felix_ranges(
        &self,
        buffer: &mut RenderBuffer,
        window: &crate::window::Window,
    ) {
        let source = self.buffer_manager.get(window.buffer_index);
        let Some(source) = source else {
            return;
        };
        let len = source.contents_snapshot().len_chars();
        let style = self.theme.editor_selection_style();
        let cursor_style = self.theme.synthetic_cursor_style(&self.theme.style);
        let primary = window.selection.primary_index();
        for (index, range) in window.selection.ranges().iter().enumerate() {
            let (start, end) = range.span(len);
            if end <= start {
                continue;
            }
            // Helix paints `ui.selection` on every scalar except the head.
            // The head is the cursor cell: the focused primary head is drawn
            // by the block cursor, an unfocused primary head is left as text
            // (the terminal cursor is an underscore), and every other head
            // uses the cursor style.
            let head = (range.head < len).then(|| source.char_idx_to_position(range.head));
            let start_position = source.char_idx_to_position(start);
            let end_position = source.char_idx_to_position(end);
            for line_index in
                start_position.line..=end_position.line.min(source.len().saturating_sub(1))
            {
                let Some(line) = source.get(line_index) else {
                    continue;
                };
                let line = crate::unicode_utils::trim_line_ending(&line);
                let start_char = if line_index == start_position.line {
                    start_position.character
                } else {
                    0
                };
                let end_char = if line_index == end_position.line {
                    end_position.character
                } else {
                    line.chars().count()
                };
                let head_char = head
                    .filter(|head| head.line == line_index)
                    .map(|head| head.character)
                    .filter(|character| *character >= start_char && *character < end_char);
                let mut pieces = Vec::new();
                if let Some(head_char) = head_char {
                    if head_char > start_char {
                        pieces.push((start_char, head_char, false));
                    }
                    if index != primary {
                        pieces.push((head_char, head_char + 1, true));
                    }
                    if end_char > head_char + 1 {
                        pieces.push((head_char + 1, end_char, false));
                    }
                } else if end_char > start_char {
                    pieces.push((start_char, end_char, false));
                }
                let tab_width = self.tab_width_for_buffer_index(window.buffer_index);
                for (piece_start, piece_end, is_cursor) in pieces {
                    let start_col = crate::unicode_utils::display_width_with_tabs(
                        crate::unicode_utils::char_prefix(line, piece_start),
                        tab_width,
                    );
                    let end_col = crate::unicode_utils::display_width_with_tabs(
                        crate::unicode_utils::char_prefix(line, piece_end),
                        tab_width,
                    );
                    let points = self
                        .display_col_range_points_in_window(window, line_index, start_col, end_col);
                    buffer.apply_selection_for_points(
                        points,
                        if is_cursor { &cursor_style } else { &style },
                        &self.theme,
                        crate::theme::SelectionForegroundPriority::Selection,
                    );
                }
            }
        }
    }

    fn place_felix_cursor(&mut self) {
        let head = self.felix_selection().primary().head;
        let position = self.current_buffer().char_idx_to_position(head);
        self.move_to_text_position(position);
        // The painted block follows cursor_goal. Keep it on the selection head
        // rather than the column where the motion started.
        self.refresh_cursor_goal();
        self.sync_to_window();
    }

    fn felix_text(&self) -> String {
        self.current_buffer().contents()
    }

    fn felix_keymap(&self, mode: Mode) -> HashMap<String, KeyAction> {
        let vim = self.vim_normal_keys();
        let mut map = HashMap::new();
        for (key, action) in &vim {
            if keep_red_key(key) {
                map.insert(key.clone(), action.clone());
            }
        }
        for (key, node) in normal_keymap() {
            map.insert(key.to_string(), key_node_action(node));
        }
        if let Some(space) = map.get(" ").cloned().or_else(|| map.get("Space").cloned()) {
            let KeyAction::Nested(mut space) = space else {
                return map;
            };
            for (key, action) in [
                ("/", Action::PluginCommand("ProjectSearch".to_string())),
                (
                    "S",
                    Action::PluginCommand("LspWorkspaceSymbols".to_string()),
                ),
                ("l", Action::Felix(Command::SelectReferences)),
                ("K", Action::Hover),
                ("y", Action::Felix(Command::Yank)),
            ] {
                space
                    .entry(key.to_string())
                    .or_insert(KeyAction::Single(action));
            }
            map.insert(" ".to_string(), KeyAction::Nested(space.clone()));
            map.insert("Space".to_string(), KeyAction::Nested(space));
        }
        let overrides = match mode {
            Mode::Select => &self.config.helix_keys.select,
            Mode::Insert => &self.config.helix_keys.insert,
            _ => &self.config.helix_keys.normal,
        };
        for (key, action) in overrides {
            map.insert(key.clone(), action.clone());
        }
        map
    }

    fn vim_normal_keys(&self) -> HashMap<String, KeyAction> {
        if self.config.keys.normal.is_empty() {
            embedded_normal_keys().clone()
        } else {
            self.config.keys.normal.clone()
        }
    }

    fn felix_directory_action(&mut self, event: &Event) -> Option<Action> {
        let (directory, event) = (&self.current_buffer().directory, event);
        let directory = directory.as_ref()?;
        let Event::Key(event) = event else {
            return None;
        };
        if !matches!(event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return None;
        }
        match (event.code, event.modifiers) {
            (KeyCode::Enter, KeyModifiers::NONE) => self.directory_open_action(),
            (KeyCode::Char('-'), KeyModifiers::NONE) => std::path::Path::new(&directory.path)
                .parent()
                .map(|path| Action::OpenFile(path.to_string_lossy().into_owned())),
            _ => None,
        }
    }

    fn handle_felix_prompt(&mut self, event: &Event) -> Option<KeyAction> {
        let Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) = event
        else {
            return Some(KeyAction::None);
        };
        let prompt = self.felix.prompt.as_mut()?;
        match code {
            KeyCode::Esc => {
                self.felix.prompt = None;
                self.set_legacy_message(None);
                Some(KeyAction::None)
            }
            KeyCode::Enter => {
                let prompt = self.felix.prompt.take()?;
                self.repeater = None;
                Some(KeyAction::Single(Action::FelixPrompt {
                    command: prompt.command,
                    text: prompt.draft,
                }))
            }
            KeyCode::Backspace => {
                prompt.draft.pop();
                let draft = prompt.draft.clone();
                let label = prompt.command.label();
                self.set_legacy_message(Some(format!("{label}: {draft}")));
                Some(KeyAction::None)
            }
            KeyCode::Char(character)
                if *modifiers == KeyModifiers::NONE || *modifiers == KeyModifiers::SHIFT =>
            {
                prompt.draft.push(*character);
                let draft = prompt.draft.clone();
                let label = prompt.command.label();
                self.set_legacy_message(Some(format!("{label}: {draft}")));
                Some(KeyAction::None)
            }
            _ => Some(KeyAction::None),
        }
    }

    fn handle_felix_pending_key(&mut self, event: &Event) -> Option<KeyAction> {
        let Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) = event
        else {
            return Some(KeyAction::None);
        };
        if *code == KeyCode::Esc {
            self.felix.pending = None;
            self.waiting_command = None;
            return Some(KeyAction::None);
        }
        let KeyCode::Char(character) = code else {
            self.felix.pending = None;
            self.set_legacy_message(Some("expected a character".to_string()));
            return Some(KeyAction::None);
        };
        if !matches!(*modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT) {
            self.felix.pending = None;
            return Some(KeyAction::None);
        }
        let mut pending = self.felix.pending.take()?;
        if pending.command == Command::SurroundReplace && pending.first.is_none() {
            pending.first = Some(*character);
            self.felix.pending = Some(pending);
            self.waiting_command = Some(format!("replace surround {}", character));
            return Some(KeyAction::None);
        }
        self.waiting_command = None;
        self.repeater = Some(pending.count);
        Some(KeyAction::Single(Action::FelixChar {
            command: pending.command,
            character: *character,
            extra: pending.first,
        }))
    }

    async fn run_felix_command(
        &mut self,
        command: Command,
        count: Option<u16>,
        argument: Option<(char, Option<char>)>,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let extend = extend_command(command, self.mode == Mode::Select);
        let times = count.unwrap_or(1) as usize;
        let text = self.felix_text();
        let selection = self.felix_selection();
        if let Some(mut motion) = motion_for(command) {
            if command == Command::GotoFileStart && count.is_some() {
                motion = felix::goto_line_motion(Some(times));
            }
            if command == Command::GotoLine {
                let Some(line) = count else {
                    self.render(buffer)?;
                    return Ok(());
                };
                motion = felix::goto_line_motion(Some(line as usize));
            }
            let target = argument.map(|(character, _)| character);
            match felix::apply_motion(&text, &selection, motion, times, target, extend) {
                Some(next) => self.set_felix_selection(next),
                None => self.set_legacy_message(Some("character not found".to_string())),
            }
            self.render(buffer)?;
            return Ok(());
        }

        match command {
            Command::ExtendLineBelow => {
                let next = selection.transform_each(|range| felix::extend_line_below(&text, range));
                self.set_felix_selection(next);
            }
            Command::ExtendToLineBounds => {
                let next = selection.transform_each(|range| felix::line_bounds_range(&text, range));
                self.set_felix_selection(next);
            }
            Command::ShrinkToLineBounds => {
                let next =
                    selection.transform_each(|range| felix::shrink_to_line_bounds(&text, range));
                self.set_felix_selection(next);
            }
            Command::SelectAll => {
                self.set_felix_selection(Selection::single(felix::select_all(&text)))
            }
            Command::CollapseSelection => self.set_felix_selection(selection.collapse()),
            Command::FlipSelections => self.set_felix_selection(selection.flip()),
            Command::KeepPrimarySelection => self.set_felix_selection(selection.keep_primary()),
            Command::RemovePrimarySelection => {
                if let Some(next) = selection.remove_primary() {
                    self.set_felix_selection(next);
                } else {
                    self.set_legacy_message(Some("only one selection".to_string()));
                }
            }
            Command::RotateSelectionsForward => self.set_felix_selection(selection.rotate(true)),
            Command::RotateSelectionsBackward => self.set_felix_selection(selection.rotate(false)),
            Command::CopySelectionOnNextLine | Command::CopySelectionOnPrevLine => {
                let delta = if command == Command::CopySelectionOnNextLine {
                    times as isize
                } else {
                    -(times as isize)
                };
                let mut next = selection.clone();
                let len = text.chars().count();
                for range in selection.ranges() {
                    if let Some(copied) = felix::copy_range_on_line(&text, *range, delta) {
                        next.push_range(copied, len);
                    }
                }
                self.set_felix_selection(next);
            }
            Command::SplitSelectionOnNewline => {
                self.set_felix_selection(felix::split_lines(&text, &selection))
            }
            Command::TrimSelections => {
                self.set_felix_selection(felix::trim_ranges(&text, &selection))
            }
            Command::AlignSelections => {
                let edits = felix::align_edits(&text, &selection);
                let ranges = selection
                    .ranges()
                    .iter()
                    .map(|range| {
                        let pad: usize = edits
                            .iter()
                            .filter(|edit| edit.start <= range.from())
                            .map(|edit| edit.text.chars().count())
                            .sum();
                        Range::new(range.anchor + pad, range.head + pad)
                    })
                    .collect();
                let primary = selection.primary_index();
                self.apply_felix_edits(edits, false);
                self.set_felix_selection(Selection::from_ranges(ranges, primary));
            }
            Command::DeleteSelection | Command::DeleteSelectionNoYank => {
                self.delete_felix_selection(command == Command::DeleteSelection, false)
            }
            Command::ChangeSelection | Command::ChangeSelectionNoYank => {
                self.delete_felix_selection(command == Command::ChangeSelection, true)
            }
            Command::Yank => self.yank_felix_selection(),
            Command::PasteAfter | Command::PasteBefore => {
                self.paste_felix(command == Command::PasteBefore)
            }
            Command::ReplaceWithYanked => self.replace_felix_with_yank(),
            Command::SwitchCase | Command::SwitchToLowercase | Command::SwitchToUppercase => {
                let change = match command {
                    Command::SwitchToLowercase => felix::CaseChange::Lower,
                    Command::SwitchToUppercase => felix::CaseChange::Upper,
                    _ => felix::CaseChange::Toggle,
                };
                self.transform_felix_case(change);
            }
            Command::SelectMode => {
                // Helix `v` toggles. Leaving select keeps every range.
                let next = if self.mode == Mode::Select {
                    Mode::Normal
                } else {
                    Mode::Select
                };
                self.execute(&Action::EnterMode(next), buffer, runtime)
                    .await?;
                return Ok(());
            }
            Command::NormalMode => {
                self.execute(&Action::EnterMode(Mode::Normal), buffer, runtime)
                    .await?;
                return Ok(());
            }
            Command::InsertMode
            | Command::AppendMode
            | Command::InsertAtLineStart
            | Command::InsertAtLineEnd
            | Command::OpenBelow
            | Command::OpenAbove => {
                self.enter_felix_insert(command, buffer, runtime).await?;
                return Ok(());
            }
            Command::CommandMode => {
                self.execute(&Action::EnterMode(Mode::Command), buffer, runtime)
                    .await?;
                return Ok(());
            }
            Command::Search | Command::Rsearch => {
                let direction = if command == Command::Search {
                    SearchDirection::Forward
                } else {
                    SearchDirection::Backward
                };
                self.execute(&Action::EnterSearch(direction), buffer, runtime)
                    .await?;
                return Ok(());
            }
            Command::SearchNext | Command::SearchPrev => {
                if command == Command::SearchPrev {
                    let start = self.felix_selection().primary().from();
                    if start > 0 {
                        let position = self.current_buffer().char_idx_to_position(start - 1);
                        self.move_to_text_position(position);
                    }
                }
                let action = if command == Command::SearchNext {
                    Action::RepeatSearch
                } else {
                    Action::RepeatSearchOpposite
                };
                self.execute(&action, buffer, runtime).await?;
                return Ok(());
            }
            Command::SearchSelection => self.search_felix_selection(buffer, runtime).await?,
            Command::Undo | Command::Redo | Command::Earlier | Command::Later => {
                let action = match command {
                    Command::Redo => Action::Redo,
                    Command::Earlier => Action::SelectPreviousUndoBranch,
                    Command::Later => Action::SelectNextUndoBranch,
                    _ => Action::Undo,
                };
                self.execute(&action, buffer, runtime).await?;
                return Ok(());
            }
            Command::PageUp
            | Command::PageDown
            | Command::HalfPageUp
            | Command::HalfPageDown
            | Command::JumpBackward
            | Command::JumpForward
            | Command::GotoWindowTop
            | Command::GotoWindowCenter
            | Command::GotoWindowBottom
            | Command::AlignViewCenter
            | Command::AlignViewTop
            | Command::AlignViewBottom
            | Command::ScrollDown
            | Command::ScrollUp
            | Command::GotoDefinition
            | Command::GotoDeclaration
            | Command::GotoTypeDefinition
            | Command::GotoImplementation
            | Command::GotoReference
            | Command::GotoLastAccessedFile
            | Command::GotoNextBuffer
            | Command::GotoPreviousBuffer
            | Command::GotoLastModification
            | Command::FilePicker
            | Command::Hover
            | Command::CodeAction => {
                self.delegate_felix_action(command, extend, buffer, runtime)
                    .await?;
                return Ok(());
            }
            Command::MatchBrackets => self.select_felix_match(),
            Command::SelectReferences => {
                self.request_felix_references(runtime).await?;
            }
            Command::ExpandSelection
            | Command::ShrinkSelection
            | Command::SelectNextSibling
            | Command::SelectPrevSibling => self.walk_felix_syntax(command),
            Command::Indent | Command::Unindent => {
                self.indent_felix_selection(command == Command::Unindent)
            }
            Command::JoinSelections => self.join_felix_selections(),
            Command::FormatSelections => self.format_felix_selection(),
            Command::ToggleComments => self.comment_felix_selection(),
            Command::SelectRegex
            | Command::SplitSelection
            | Command::KeepSelections
            | Command::RemoveSelections
            | Command::FindNextChar
            | Command::FindPrevChar
            | Command::TillNextChar
            | Command::TillPrevChar
            | Command::Replace
            | Command::SelectTextobjectAround
            | Command::SelectTextobjectInner
            | Command::SurroundAdd
            | Command::SurroundReplace
            | Command::SurroundDelete
            | Command::MoveCharLeft
            | Command::MoveCharRight
            | Command::MoveLineUp
            | Command::MoveLineDown
            | Command::MoveNextWordStart
            | Command::MovePrevWordStart
            | Command::MoveNextWordEnd
            | Command::MovePrevWordEnd
            | Command::MoveNextLongWordStart
            | Command::MovePrevLongWordStart
            | Command::MoveNextLongWordEnd
            | Command::MovePrevLongWordEnd
            | Command::GotoFileStart
            | Command::GotoLastLine
            | Command::GotoLine
            | Command::GotoLineStart
            | Command::GotoLineEnd
            | Command::GotoFirstNonwhitespace => {}
        }
        self.render(buffer)?;
        Ok(())
    }

    pub(super) async fn execute_felix_argument(
        &mut self,
        command: Command,
        character: char,
        extra: Option<char>,
        text: Option<String>,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        if let Some(pattern) = text {
            let source = self.felix_text();
            let selection = self.felix_selection();
            let result = match command {
                Command::SelectRegex => felix::regex_select(&source, &selection, &pattern),
                Command::SplitSelection => felix::regex_split(&source, &selection, &pattern),
                Command::KeepSelections => felix::regex_keep(&source, &selection, &pattern, false),
                Command::RemoveSelections => felix::regex_keep(&source, &selection, &pattern, true),
                _ => Err("unsupported prompt".to_string()),
            };
            match result {
                Ok(next) => self.set_felix_selection(next),
                Err(error) => self.set_legacy_message(Some(error)),
            }
            self.render(buffer)?;
            return Ok(());
        }
        if command == Command::Replace {
            self.replace_felix_chars(character);
            self.render(buffer)?;
            return Ok(());
        }
        if matches!(
            command,
            Command::SelectTextobjectInner | Command::SelectTextobjectAround
        ) {
            let scope = if command == Command::SelectTextobjectInner {
                TextObjectScope::Inner
            } else {
                TextObjectScope::Around
            };
            self.select_felix_textobject(scope, character);
            self.render(buffer)?;
            return Ok(());
        }
        if matches!(
            command,
            Command::SurroundAdd | Command::SurroundDelete | Command::SurroundReplace
        ) {
            self.surround_felix(command, character, extra);
            self.render(buffer)?;
            return Ok(());
        }
        let count = self.repeater.take();
        self.run_felix_command(command, count, Some((character, extra)), buffer, runtime)
            .await
    }

    fn delete_felix_selection(&mut self, yank: bool, change: bool) {
        let text = self.felix_text();
        let len = text.chars().count();
        let selection = self.felix_selection();
        let mut spans: Vec<(usize, usize)> = selection
            .ranges()
            .iter()
            .map(|range| {
                let (start, end) = range.span(len);
                if end > start {
                    (start, end)
                } else if start < len {
                    (start, start + 1)
                } else {
                    (start, start)
                }
            })
            .filter(|(start, end)| end > start)
            .collect();
        if yank {
            let yanked: Vec<String> = spans
                .iter()
                .map(|(start, end)| self.current_buffer().text_in_char_range(*start, *end))
                .collect();
            self.set_default_register(if yanked.len() > 1 {
                Content::multi_cursor_blockwise(yanked)
            } else {
                Content::charwise(yanked.into_iter().next().unwrap_or_default())
            });
        }
        spans.sort_by_key(|(start, _)| std::cmp::Reverse(*start));
        let mut cursors: Vec<usize> = spans.iter().map(|(start, _)| *start).collect();
        let mut primary_cursor = selection.primary().from();
        for (start, end) in &spans {
            let removed = end.saturating_sub(*start);
            for cursor in &mut cursors {
                if *cursor >= *end {
                    *cursor -= removed;
                } else if *cursor > *start {
                    *cursor = *start;
                }
            }
            if primary_cursor >= *end {
                primary_cursor -= removed;
            } else if primary_cursor > *start {
                primary_cursor = *start;
            }
        }
        self.remember_felix_selection();
        self.begin_transaction(if change {
            "change selection"
        } else {
            "delete selection"
        });
        for (start, end) in &spans {
            let start = self.current_buffer().char_idx_to_position(*start);
            let end = self.current_buffer().char_idx_to_position(*end);
            self.replace_range(TextRange::new(start, end), "");
        }
        self.commit_transaction(self.cursor_snapshot());
        let primary = cursors
            .iter()
            .position(|cursor| *cursor == primary_cursor)
            .unwrap_or(0);
        let mut next =
            Selection::from_ranges(cursors.into_iter().map(Range::cursor).collect(), primary);
        next.normalize(self.current_buffer().contents_snapshot().len_chars());
        if change {
            self.felix.insert_extend = false;
            self.felix.insert_cursors = next.ranges().iter().map(|range| range.head).collect();
            self.felix.insert_primary = next.primary_index();
            self.set_felix_selection(next);
            self.mode = Mode::Insert;
            self.insert_entry_cursor = Some(self.cursor_snapshot());
            self.begin_transaction("insert");
        } else {
            self.set_felix_selection(next);
        }
        self.remember_felix_selection();
    }

    fn yank_felix_selection(&mut self) {
        let text_len = self.current_buffer().contents_snapshot().len_chars();
        let yanked: Vec<String> = self
            .felix_selection()
            .ranges()
            .iter()
            .map(|range| {
                let (start, end) = range.span(text_len);
                self.current_buffer().text_in_char_range(start, end)
            })
            .collect();
        self.set_default_register(if yanked.len() > 1 {
            Content::multi_cursor_blockwise(yanked)
        } else {
            Content::charwise(yanked.into_iter().next().unwrap_or_default())
        });
        self.set_felix_selection(self.felix_selection().collapse());
    }

    fn paste_felix(&mut self, before: bool) {
        let Some(content) = self.registers.get(&DEFAULT_REGISTER).cloned() else {
            self.set_legacy_message(Some("nothing to paste".to_string()));
            return;
        };
        let len = self.current_buffer().contents_snapshot().len_chars();
        let selection = self.felix_selection();
        let segments = if content.multi_cursor_segments.is_empty() {
            vec![content.text.clone()]
        } else {
            content.multi_cursor_segments.clone()
        };
        let mut edits = Vec::new();
        for (index, range) in selection.ranges().iter().enumerate() {
            let (start, end) = range.span(len);
            let at = if before { start } else { end };
            let text = if segments.len() == selection.len() {
                segments[index].clone()
            } else {
                content.text.clone()
            };
            let text = if content.kind == ContentKind::Linewise && !text.ends_with('\n') {
                format!("\n{text}")
            } else {
                text
            };
            edits.push(TextEdit {
                start: at,
                end: at,
                text,
            });
        }
        self.apply_felix_edits(edits, true);
    }

    fn replace_felix_with_yank(&mut self) {
        let Some(content) = self.registers.get(&DEFAULT_REGISTER).cloned() else {
            return;
        };
        let len = self.current_buffer().contents_snapshot().len_chars();
        let edits = self
            .felix_selection()
            .ranges()
            .iter()
            .map(|range| {
                let (start, end) = range.span(len);
                TextEdit {
                    start,
                    end,
                    text: content.text.clone(),
                }
            })
            .collect();
        self.apply_felix_edits(edits, true);
    }

    fn replace_felix_chars(&mut self, character: char) {
        let len = self.current_buffer().contents_snapshot().len_chars();
        let edits = self
            .felix_selection()
            .ranges()
            .iter()
            .filter_map(|range| {
                let (start, end) = range.span(len);
                (end > start).then(|| TextEdit {
                    start,
                    end,
                    text: felix::replacement(
                        &self.current_buffer().text_in_char_range(start, end),
                        character,
                    ),
                })
            })
            .collect();
        self.apply_felix_edits(edits, false);
    }

    fn transform_felix_case(&mut self, change: felix::CaseChange) {
        let len = self.current_buffer().contents_snapshot().len_chars();
        let edits = self
            .felix_selection()
            .ranges()
            .iter()
            .filter_map(|range| {
                let (start, end) = range.span(len);
                let text = self.current_buffer().text_in_char_range(start, end);
                let replacement = felix::transform_case(&text, change);
                (text != replacement).then_some(TextEdit {
                    start,
                    end,
                    text: replacement,
                })
            })
            .collect();
        self.apply_felix_edits(edits, false);
    }

    fn apply_felix_edits(&mut self, edits: Vec<TextEdit>, select_inserted: bool) {
        if edits.is_empty() {
            return;
        }
        let mut edits = edits;
        edits.sort_by_key(|edit| std::cmp::Reverse(edit.start));
        let before = self.felix_selection();
        self.remember_felix_selection();
        self.begin_transaction("helix edit");
        for edit in &edits {
            let start = self.current_buffer().char_idx_to_position(edit.start);
            let end = self.current_buffer().char_idx_to_position(edit.end);
            self.replace_range(TextRange::new(start, end), &edit.text);
        }
        self.commit_transaction(self.cursor_snapshot());
        let len = self.current_buffer().contents_snapshot().len_chars();
        let mut selection = before;
        let mut ascending = edits;
        ascending.sort_by_key(|edit| edit.start);
        let mut inserted_ranges = Vec::new();
        let mut offset = 0usize;
        for edit in &ascending {
            let new_end = edit.start + edit.text.chars().count();
            selection.map_edit(edit.start, edit.end, new_end, len);
            let at = edit.start + offset;
            let inserted = edit.text.chars().count();
            if inserted > 0 {
                inserted_ranges.push(Range::covering(at, at + inserted - 1, true));
            }
            offset += inserted.saturating_sub(edit.end.saturating_sub(edit.start));
        }
        if select_inserted && !inserted_ranges.is_empty() {
            selection = Selection::from_ranges(inserted_ranges, 0);
            selection.normalize(len);
        }
        self.set_felix_selection(selection);
        self.remember_felix_selection();
    }

    async fn enter_felix_insert(
        &mut self,
        command: Command,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let len = self.current_buffer().contents_snapshot().len_chars();
        let selection = self.felix_selection();
        self.felix.insert_extend = command == Command::AppendMode;
        self.felix.insert_anchor = selection.primary().from();
        if matches!(command, Command::OpenBelow | Command::OpenAbove) {
            let head = selection.primary().head;
            let position = self.current_buffer().char_idx_to_position(head);
            self.move_to_text_position(position);
            let action = if command == Command::OpenBelow {
                Action::InsertLineBelowCursor
            } else {
                Action::InsertLineAtCursor
            };
            self.execute(&action, buffer, runtime).await?;
            return Ok(());
        }
        let cursors: Vec<usize> = selection
            .ranges()
            .iter()
            .map(|range| match command {
                Command::AppendMode => range.span(len).1.min(len),
                Command::InsertAtLineStart => {
                    let position = self.current_buffer().char_idx_to_position(range.from());
                    let line = self.current_buffer().get(position.line).unwrap_or_default();
                    let offset = line
                        .chars()
                        .take_while(|character| character.is_whitespace() && *character != '\n')
                        .count();
                    range.from() - position.character + offset
                }
                Command::InsertAtLineEnd => {
                    let position = self.current_buffer().char_idx_to_position(range.to());
                    let line = self.current_buffer().get(position.line).unwrap_or_default();
                    let content = line.trim_end_matches(['\n', '\r']).chars().count();
                    range.to() - position.character + content
                }
                _ => range.from(),
            })
            .collect();
        self.felix.insert_cursors = cursors;
        self.felix.insert_primary = selection.primary_index();
        let primary = self
            .felix
            .insert_cursors
            .get(self.felix.insert_primary)
            .copied()
            .unwrap_or(0);
        self.set_felix_selection(Selection::cursor(primary));
        if self.felix.insert_cursors.len() <= 1 {
            self.felix.insert_cursors.clear();
        }
        self.execute(&Action::EnterMode(Mode::Insert), buffer, runtime)
            .await?;
        Ok(())
    }

    fn felix_insert_text(&mut self, text: &str) {
        let added = text.chars().count();
        let mut cursors = self.felix.insert_cursors.clone();
        let mut order: Vec<usize> = (0..cursors.len()).collect();
        order.sort_by_key(|index| std::cmp::Reverse(cursors[*index]));
        if !self.transaction_active() {
            self.begin_transaction("insert");
        }
        for index in order {
            let at = cursors[index];
            let position = self.current_buffer().char_idx_to_position(at);
            self.replace_range(TextRange::insertion(position), text);
            for cursor in &mut cursors {
                if *cursor >= at {
                    *cursor += added;
                }
            }
        }
        self.felix.insert_cursors = cursors;
        let len = self.current_buffer().contents_snapshot().len_chars();
        let primary = self
            .felix
            .insert_primary
            .min(self.felix.insert_cursors.len().saturating_sub(1));
        let ranges = self
            .felix
            .insert_cursors
            .iter()
            .copied()
            .map(Range::cursor)
            .collect();
        let mut selection = Selection::from_ranges(ranges, primary);
        selection.normalize(len);
        self.set_felix_selection(selection);
    }

    pub(super) fn finish_helix_insert(&mut self) {
        self.sync_helix_insert_selection();
        let head = self.felix_selection().primary().head;
        if self.felix.insert_extend {
            let anchor = self.felix.insert_anchor;
            let selection = Selection::single(Range::new(anchor, head.max(anchor)));
            self.set_felix_selection(selection);
            self.set_special_mark_at_char('.', head.max(anchor), super::AnchorAffinity::Right);
            return;
        }
        let selection = self.felix_selection();
        self.set_felix_selection(selection);
        self.set_special_mark_at_char('.', head, super::AnchorAffinity::Right);
    }

    pub(super) fn sync_helix_insert_selection(&mut self) {
        if self.felix.insert_cursors.len() > 1 {
            return;
        }
        let gap = self.cursor_char_index();
        let selection = if self.felix.insert_extend {
            let head = gap.saturating_sub(1);
            Range::new(self.felix.insert_anchor.min(head), head)
        } else {
            Range::cursor(gap)
        };
        let len = self.current_buffer().contents_snapshot().len_chars();
        let mut selection = Selection::single(selection);
        selection.normalize(len);
        // Keep the insert cursor where the next character will go. Placing it
        // on the selection head would move an append cursor backward.
        if let Some(window) = self.window_manager.active_window_mut() {
            window.selection = selection;
        }
    }

    fn felix_backspace_cursors(&mut self) {
        let mut cursors = self.felix.insert_cursors.clone();
        let mut order: Vec<usize> = (0..cursors.len()).collect();
        order.sort_by_key(|index| std::cmp::Reverse(cursors[*index]));
        if !self.transaction_active() {
            self.begin_transaction("insert");
        }
        for index in order {
            if cursors[index] == 0 {
                continue;
            }
            let start = self
                .current_buffer()
                .char_idx_to_position(cursors[index] - 1);
            let end = self.current_buffer().char_idx_to_position(cursors[index]);
            self.replace_range(TextRange::new(start, end), "");
            cursors[index] -= 1;
        }
        self.felix.insert_cursors = cursors;
    }

    fn felix_line_start(&self, line: usize) -> usize {
        let mut seen = 0usize;
        for (index, character) in self.felix_text().chars().enumerate() {
            if seen == line {
                return index;
            }
            if character == '\n' {
                seen += 1;
            }
        }
        self.current_buffer().contents_snapshot().len_chars()
    }

    fn felix_place_on_line(&mut self, line: usize) {
        let len = self.current_buffer().contents_snapshot().len_chars();
        let index = self.felix_line_start(line).min(len);
        self.set_felix_selection(Selection::cursor(index));
        let position = self.current_buffer().char_idx_to_position(index);
        let height = self.vheight().max(1);
        if position.line < self.vtop {
            self.vtop = position.line;
        } else if position.line >= self.vtop.saturating_add(height) {
            self.vtop = position.line.saturating_sub(height - 1);
        }
        self.move_to_text_position(position);
    }

    /// Helix page and window motions move the primary cursor by the text
    /// height and `scrolloff`, which for a 24-row frame is 22 and 3.
    fn felix_view_command(&mut self, command: Command) {
        let height = self.vheight().max(1);
        let scrolloff = self
            .config
            .scrolloff
            .unwrap_or(0)
            .min(height.saturating_sub(1) / 2);
        let cursor_line = self
            .current_buffer()
            .char_idx_to_position(self.felix_selection().primary().head)
            .line;
        let target = match command {
            Command::GotoWindowTop => self.vtop + scrolloff,
            Command::GotoWindowCenter => self.vtop + height.saturating_sub(1) / 2,
            Command::GotoWindowBottom => self
                .vtop
                .saturating_add(height.saturating_sub(1).saturating_sub(scrolloff)),
            Command::ScrollDown => {
                self.vtop = self.vtop.saturating_add(1);
                cursor_line.max(self.vtop + scrolloff)
            }
            Command::ScrollUp => {
                self.vtop = self.vtop.saturating_sub(1);
                let bottom = self
                    .vtop
                    .saturating_add(height.saturating_sub(1).saturating_sub(scrolloff));
                cursor_line.min(bottom)
            }
            Command::PageDown => {
                self.vtop = self.vtop.saturating_add(height);
                cursor_line.max(self.vtop + scrolloff)
            }
            Command::PageUp => {
                self.vtop = self.vtop.saturating_sub(height);
                let bottom = self
                    .vtop
                    .saturating_add(height.saturating_sub(1).saturating_sub(scrolloff));
                cursor_line.min(bottom)
            }
            Command::HalfPageDown => cursor_line.saturating_add(height / 2),
            Command::HalfPageUp => cursor_line.saturating_sub(height / 2),
            _ => cursor_line,
        };
        self.felix_place_on_line(target);
    }

    async fn delegate_felix_action(
        &mut self,
        command: Command,
        extend: bool,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let before = self.felix_selection().primary();
        let action = match command {
            Command::PageUp
            | Command::PageDown
            | Command::HalfPageUp
            | Command::HalfPageDown
            | Command::GotoWindowTop
            | Command::GotoWindowCenter
            | Command::GotoWindowBottom
            | Command::ScrollDown
            | Command::ScrollUp => {
                self.felix_view_command(command);
                self.render(buffer)?;
                return Ok(());
            }
            Command::JumpBackward => Action::JumpBack,
            Command::JumpForward => Action::JumpForward,
            Command::AlignViewCenter => Action::MoveLineToViewportCenter,
            Command::AlignViewTop => Action::ScrollCursorToViewStart,
            Command::AlignViewBottom => Action::ScrollCursorToViewEnd,
            Command::GotoDefinition => Action::GoToDefinition,
            Command::GotoDeclaration => Action::FelixGoto("textDocument/declaration".to_string()),
            Command::GotoTypeDefinition => {
                Action::FelixGoto("textDocument/typeDefinition".to_string())
            }
            Command::GotoImplementation => {
                Action::FelixGoto("textDocument/implementation".to_string())
            }
            Command::GotoReference => Action::PluginCommand("LspReferences".to_string()),
            Command::GotoLastAccessedFile => Action::AlternateBuffer,
            Command::GotoNextBuffer => Action::NextBuffer,
            Command::GotoPreviousBuffer => Action::PreviousBuffer,
            Command::GotoLastModification => Action::JumpToMark {
                mark: '.',
                linewise: false,
            },
            Command::FilePicker => Action::FilePicker,
            Command::Hover => Action::Hover,
            Command::CodeAction => Action::CodeAction,
            _ => return Ok(()),
        };
        self.execute(&action, buffer, runtime).await?;
        if matches!(
            command,
            Command::PageUp
                | Command::PageDown
                | Command::HalfPageUp
                | Command::HalfPageDown
                | Command::GotoWindowTop
                | Command::GotoWindowCenter
                | Command::GotoWindowBottom
                | Command::GotoLastModification
                | Command::JumpBackward
                | Command::JumpForward
                | Command::ScrollDown
                | Command::ScrollUp
        ) {
            let head = self.cursor_char_index();
            if matches!(command, Command::JumpBackward | Command::JumpForward)
                && head == before.head
            {
                return Ok(());
            }
            let range = if extend {
                Range::new(before.anchor, head)
            } else {
                Range::cursor(head)
            };
            self.set_felix_selection(Selection::single(range));
        }
        Ok(())
    }

    fn select_felix_match(&mut self) {
        let origin = self.cursor_text_position();
        let Some(motion) = self.matchit_motion(crate::matchit::MatchDirection::Forward) else {
            self.set_legacy_message(Some("match not found".to_string()));
            return;
        };
        let start = self.current_buffer().position_to_char_idx(origin);
        let end = self.current_buffer().position_to_char_idx(motion.target);
        let range = if self.mode == Mode::Select {
            Range::new(start, end)
        } else {
            Range::cursor(end)
        };
        self.set_felix_selection(Selection::single(range));
    }

    fn select_felix_textobject(&mut self, scope: TextObjectScope, key: char) {
        if let Some(kind) = SyntaxObjectKind::for_text_object_key(key) {
            if let Some(object) = self.syntax_text_object(scope, kind) {
                self.set_felix_range_from_text_range(object.range);
                return;
            }
        }
        let Some(kind) = text_object_kind_for_key(key) else {
            self.set_legacy_message(Some("unknown text object".to_string()));
            return;
        };
        let resolver = MotionResolver::new(self.current_buffer(), self.cursor_text_position());
        let Some(range) = resolver.text_object(scope, kind) else {
            self.set_legacy_message(Some("text object not found".to_string()));
            return;
        };
        self.set_felix_range_from_text_range(range);
    }

    fn set_felix_range_from_text_range(&mut self, range: TextRange) {
        let buffer = self.current_buffer();
        let start = buffer.position_to_char_idx(range.start);
        let end = buffer.position_to_char_idx(range.end);
        if end <= start {
            self.set_felix_selection(Selection::cursor(start));
        } else {
            self.set_felix_selection(Selection::single(Range::covering(start, end - 1, true)));
        }
    }

    fn surround_felix(&mut self, command: Command, character: char, extra: Option<char>) {
        let op = match command {
            Command::SurroundDelete => SurroundOp::Delete,
            Command::SurroundReplace => SurroundOp::Replace,
            _ => SurroundOp::Add,
        };
        let text = self.felix_text();
        let selection = self.felix_selection();
        let mut edits = Vec::new();
        let mut chosen = Vec::new();
        for range in selection.ranges() {
            match felix::surround(&text, *range, op, character, extra) {
                Ok((more, next)) => {
                    edits.extend(more);
                    chosen.push(next);
                }
                Err(error) => {
                    self.set_legacy_message(Some(error));
                    return;
                }
            }
        }
        self.apply_felix_edits(edits, false);
        if op == SurroundOp::Delete {
            let cursor = chosen
                .first()
                .map(|range| range.from().saturating_sub(1))
                .unwrap_or(0);
            self.set_felix_selection(Selection::cursor(cursor));
        } else if !chosen.is_empty() {
            self.set_felix_selection(Selection::from_ranges(chosen, selection.primary_index()));
        }
    }

    fn walk_felix_syntax(&mut self, command: Command) {
        let Some(language) = self.current_language_id() else {
            self.set_legacy_message(Some("no grammar for this buffer".to_string()));
            return;
        };
        let len = self.current_buffer().contents_snapshot().len_chars();
        let range = self.felix_selection().primary();
        if command == Command::ShrinkSelection {
            if let Some(previous) = self.felix.syntax_history.pop() {
                self.set_felix_selection(Selection::single(previous));
                return;
            }
        } else if command == Command::ExpandSelection {
            self.felix.syntax_history.push(range);
        }
        let (start, end) = range.span(len);
        let start_byte = self
            .current_buffer()
            .position_to_byte_idx(self.current_buffer().char_idx_to_position(start));
        let end_byte = self
            .current_buffer()
            .position_to_byte_idx(self.current_buffer().char_idx_to_position(end));
        let walk = match command {
            Command::ShrinkSelection => crate::textobjects::SyntaxWalk::Shrink,
            Command::SelectNextSibling => crate::textobjects::SyntaxWalk::NextSibling,
            Command::SelectPrevSibling => crate::textobjects::SyntaxWalk::PrevSibling,
            _ => crate::textobjects::SyntaxWalk::Expand,
        };
        let contents = self.felix_text();
        match self
            .syntax_textobjects
            .walk_syntax_node(&contents, &language, start_byte, end_byte, walk)
        {
            Ok(Some((start_byte, end_byte))) => {
                let start = self
                    .current_buffer()
                    .byte_idx_to_position(start_byte)
                    .map(|position| self.current_buffer().position_to_char_idx(position))
                    .unwrap_or(start);
                let end = self
                    .current_buffer()
                    .byte_idx_to_position(end_byte)
                    .map(|position| self.current_buffer().position_to_char_idx(position))
                    .unwrap_or(end);
                if end > start {
                    self.set_felix_selection(Selection::single(Range::covering(
                        start,
                        end - 1,
                        true,
                    )));
                }
            }
            Ok(None) if command == Command::SelectPrevSibling && len > 0 => {
                // Helix's previous-sibling walk from the first token selects
                // back across the buffer to the final scalar.
                self.set_felix_selection(Selection::single(Range::new(len - 1, range.head)));
            }
            Ok(None) => self.set_legacy_message(Some("no syntax node".to_string())),
            Err(error) => self.set_legacy_message(Some(error.to_string())),
        }
    }

    fn indent_felix_selection(&mut self, unindent: bool) {
        let len = self.current_buffer().contents_snapshot().len_chars();
        let selection = self.felix_selection();
        let mut first = usize::MAX;
        let mut last = 0;
        for range in selection.ranges() {
            let (start, end) = range.span(len);
            let start_line = self.current_buffer().char_idx_to_position(start).line;
            let end_line = self
                .current_buffer()
                .char_idx_to_position(end.saturating_sub(1).max(start))
                .line;
            first = first.min(start_line);
            last = last.max(end_line);
        }
        if first > last {
            return;
        }
        if unindent {
            self.unindent_line_range(first, last, 1);
            self.set_felix_selection(Selection::cursor(self.cursor_char_index()));
            return;
        }
        // A buffer without a language indent uses Helix's plain-text tab.
        self.begin_transaction("indent selection");
        for line in (first..=last).rev() {
            self.replace_range(TextRange::insertion(TextPosition::new(line, 0)), "\t");
        }
        self.commit_transaction(self.cursor_snapshot());
        let cursor = self
            .current_buffer()
            .position_to_char_idx(TextPosition::new(first, 1));
        self.set_felix_selection(Selection::cursor(cursor));
    }

    fn join_felix_selections(&mut self) {
        let len = self.current_buffer().contents_snapshot().len_chars();
        let selection = self.felix_selection();
        let edits = selection
            .ranges()
            .iter()
            .filter_map(|range| {
                let (start, end) = range.span(len);
                let text = felix::join_span(&self.felix_text(), start, end);
                let original = self.current_buffer().text_in_char_range(start, end);
                (text != original).then_some(TextEdit { start, end, text })
            })
            .collect();
        self.apply_felix_edits(edits, false);
    }

    fn format_felix_selection(&mut self) {
        let len = self.current_buffer().contents_snapshot().len_chars();
        let (start, end) = self.felix_selection().primary().span(len);
        let start = self.current_buffer().char_idx_to_position(start);
        let end = self.current_buffer().char_idx_to_position(end);
        self.format_text_range(TextRange::new(start, end));
        self.set_felix_selection(Selection::cursor(self.cursor_char_index()));
    }

    fn comment_felix_selection(&mut self) {
        let len = self.current_buffer().contents_snapshot().len_chars();
        let (start, end) = self.felix_selection().primary().span(len);
        let start_line = self.current_buffer().char_idx_to_position(start).line;
        let end_line = self
            .current_buffer()
            .char_idx_to_position(end.saturating_sub(1).max(start))
            .line;
        if self.toggle_comment_lines(start_line, end_line) {
            let cursor = self
                .current_buffer()
                .position_to_char_idx(TextPosition::new(start_line, 0));
            self.set_felix_selection(Selection::cursor(cursor));
        }
    }

    async fn search_felix_selection(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let len = self.current_buffer().contents_snapshot().len_chars();
        let (start, end) = self.felix_selection().primary().span(len);
        let selected = self.current_buffer().text_in_char_range(start, end);
        if selected.is_empty() {
            self.execute(&Action::SearchWordUnderCursor, buffer, runtime)
                .await?;
            return Ok(());
        }
        // Helix `*` stores the selection as the search pattern and does not
        // jump. `n` moves to the next match.
        self.search_term = regex::escape(&selected);
        self.search_direction = SearchDirection::Forward;
        let _ = (buffer, runtime);
        Ok(())
    }

    async fn request_felix_references(&mut self, runtime: &mut Runtime) -> anyhow::Result<()> {
        let _ = runtime;
        let Some(file) = self.current_buffer().file.clone() else {
            self.set_legacy_message(Some("references need a named file".to_string()));
            return Ok(());
        };
        let position = self.cursor_lsp_position();
        self.ensure_current_buffer_lsp_opened().await?;
        let request_id = self
            .lsp
            .references(&file, position.character, position.line, true)
            .await?;
        self.felix.reference_requests.insert(request_id);
        Ok(())
    }

    pub(super) async fn felix_lsp_goto(
        &mut self,
        method: &str,
        _runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let Some(file) = self.current_buffer().file.clone() else {
            self.set_legacy_message(Some("no definition found".to_string()));
            return Ok(());
        };
        let position = self.cursor_lsp_position();
        self.ensure_current_buffer_lsp_opened().await?;
        let request_id = match method {
            "textDocument/declaration" => {
                self.lsp
                    .goto_declaration(&file, position.character, position.line)
                    .await?
            }
            "textDocument/typeDefinition" => {
                self.lsp
                    .goto_type_definition(&file, position.character, position.line)
                    .await?
            }
            _ => {
                self.lsp
                    .goto_implementation(&file, position.character, position.line)
                    .await?
            }
        };
        self.pending_definition_requests.insert(request_id);
        Ok(())
    }
}

fn key_node_action(node: KeyNode) -> KeyAction {
    match node {
        KeyNode::Command(command) => KeyAction::Single(Action::Felix(command)),
        KeyNode::Group(children) => {
            let mut map = HashMap::new();
            for (key, child) in children {
                map.insert(key.to_string(), key_node_action(child));
            }
            KeyAction::Nested(map)
        }
    }
}

fn keep_red_key(key: &str) -> bool {
    matches!(key, " " | "Space" | "[" | "]")
        || key == "Ctrl-w"
        || matches!(
            key,
            "Ctrl-e"
                | "Ctrl-t"
                | "Ctrl-z"
                | "Ctrl-j"
                | "Ctrl-y"
                | "Ctrl-Shift-p"
                | "F1"
                | "Alt-e"
                | "Alt-a"
                | "Alt-m"
                | "Alt-w"
        )
}

fn embedded_normal_keys() -> &'static HashMap<String, KeyAction> {
    static KEYS: OnceLock<HashMap<String, KeyAction>> = OnceLock::new();
    KEYS.get_or_init(|| {
        toml::from_str::<Config>(include_str!("../../default_config.toml"))
            .map(|config| config.keys.normal)
            .unwrap_or_default()
    })
}

fn lsp_char_position(value: Option<&serde_json::Value>) -> Option<(usize, usize)> {
    let value = value?;
    let line = value.get("line")?.as_u64()? as usize;
    let character = value.get("character")?.as_u64()? as usize;
    Some((line, character))
}

fn markup(text: &str, selection: &Selection) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut marks: Vec<Vec<&str>> = vec![Vec::new(); chars.len() + 1];
    for (index, range) in selection.ranges().iter().enumerate() {
        let primary = index == selection.primary_index();
        let (open, close) = if primary { ("#[", "]#") } else { ("#(", ")#") };
        let start = range.from().min(chars.len());
        let end = range.to().saturating_add(1).min(chars.len());
        marks[start].push(open);
        if range.head < marks.len() {
            marks[range.head.min(chars.len())].push("|");
        }
        marks[end].push(close);
    }
    let mut rendered = String::new();
    for (index, character) in chars.iter().enumerate() {
        for mark in &marks[index] {
            rendered.push_str(mark);
        }
        rendered.push(*character);
    }
    for mark in &marks[chars.len()] {
        rendered.push_str(mark);
    }
    rendered
}
