//! Red adapter for document-window Felix sessions.

mod prompt;

use super::*;
use crate::theme::SelectionForegroundPriority;
use anyhow::Context as _;
use felix_core::{History, HostAction, Range as FelixRange, Selections, Session};
use std::collections::HashMap;

const CLIPBOARD_SEPARATOR: &str = if cfg!(windows) { "\r\n" } else { "\n" };

static NATIVE_KEYS: std::sync::LazyLock<crate::config::Keys> = std::sync::LazyLock::new(|| {
    let mut keys: crate::config::Keys = toml::from_str(include_str!("felix_bindings.toml"))
        .expect("invalid built-in Felix host bindings");
    let windows = keys.normal["Ctrl-w"].clone();
    let KeyAction::Nested(space) = keys.normal.get_mut(" ").unwrap() else {
        unreachable!("space is a prefix");
    };
    space.insert("w".into(), windows);
    keys
});

pub(super) fn native_keys() -> &'static crate::config::Keys {
    &NATIVE_KEYS
}

#[derive(Clone, Copy)]
pub(super) enum Input<'a> {
    Key(&'a str),
    Replay(felix_core::ReplayStep<'a>),
    Bracketed(&'a str),
    ClipboardPaste(felix_core::Paste),
    ClipboardReplace,
    ClipboardYank { primary: bool },
}

impl Input<'static> {
    fn clipboard_action(action: &Action) -> Option<Self> {
        Some(match action {
            Action::FelixClipboardBefore => Self::ClipboardPaste(felix_core::Paste::Before),
            Action::FelixClipboardAfter => Self::ClipboardPaste(felix_core::Paste::After),
            Action::FelixClipboardReplace => Self::ClipboardReplace,
            Action::FelixClipboardYank => Self::ClipboardYank { primary: false },
            Action::FelixClipboardYankPrimary => Self::ClipboardYank { primary: true },
            _ => return None,
        })
    }
}

pub(super) type Owner = (BufferId, WindowId, u64, u64);

#[derive(Default)]
struct FelixLanguageEdit {
    indents: Vec<(usize, usize)>,
    tokens: &'static [&'static str],
}

#[derive(Default)]
pub(super) struct FelixHost {
    session: Session,
    native_prefix_owner: Option<Owner>,
    pub(super) preview: Option<CompletionPreview>,
    last_insert: felix_core::LastInsert,
    jumps: HashMap<WindowId, felix_core::Jumps<BufferId>>,
    indent: HashMap<BufferId, String>,
    selections: HashMap<(BufferId, WindowId), Selections>,
    generation: u64,
    // A Felix plan already specifies the active selections. Map other views and
    // jumps once against the committed text, using edits in application order.
    pending_rebase: Option<Vec<AppliedTextEdit>>,
    unnamed: Vec<String>,
    clipboard: Vec<String>,
    search: Vec<String>,
    before: Option<Selections>,
    history: HashMap<(BufferId, String), (Selections, Selections)>,
}

/// Filter keys edit the live document. An accepted item belongs to the original request.
/// Keep inverse edits in application order so acceptance can restore that exact version.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct FilterSavepoint {
    pub(super) owner: Owner,
    text: ropey::Rope,
    selections: Selections,
    recipe: usize,
    inverse: Vec<(usize, usize, String)>,
}

pub(super) struct CompletionPreview {
    owner: Owner,
    pub(super) text: ropey::Rope,
    original: ropey::Rope,
    replacements: Vec<std::ops::Range<usize>>,
    replacement_len: usize,
    pub(super) carets: Vec<usize>,
    pub(super) primary: usize,
}

impl CompletionPreview {
    pub(super) fn projected_line(&self, line: usize) -> Option<usize> {
        let pos = self.original.try_line_to_char(line).ok()?;
        let mut mapped = pos;
        for range in &self.replacements {
            if pos <= range.start {
                break;
            }
            mapped -= range.len().min(pos - range.start);
            mapped += self.replacement_len;
            if pos < range.end {
                break;
            }
        }
        Some(self.text.char_to_line(mapped))
    }
}

fn felix_display_col(
    text: ropey::RopeSlice<'_>,
    offset: usize,
    tab_width: usize,
) -> (usize, usize) {
    let line = text.char_to_line(offset);
    let prefix: std::borrow::Cow<'_, str> = text.slice(text.line_to_char(line)..offset).into();
    (
        line,
        crate::config::EditingModel::Helix.source_width(&prefix, tab_width),
    )
}

impl Editor {
    pub(super) fn felix_active_selection(&self) -> Option<&Selections> {
        let (doc, view, _, _) = self.felix_owner()?;
        self.felix.selections.get(&(doc, view))
    }

    pub(super) fn set_felix_active_selection(&mut self, state: Selections) {
        if let Some((doc, view, _, _)) = self.felix_owner() {
            self.place_felix_cursor(&state);
            self.felix.selections.insert((doc, view), state);
        }
    }

    pub(super) fn restore_felix_view(&mut self) {
        self.snippet_session = None;
        let text = self.current_buffer().contents_snapshot();
        let state = self.felix_active_selection().cloned().unwrap_or_else(|| {
            let offset = self
                .current_buffer()
                .position_to_char_idx(self.cursor_text_position());
            Selections::single(text.slice(..), FelixRange::at(text.slice(..), offset))
        });
        self.set_felix_active_selection(state);
        self.felix.generation = self.felix.generation.wrapping_add(1);
        self.felix.session.cancel_pending();
    }

    pub(super) fn felix_prune_views(&mut self) {
        self.felix
            .selections
            .retain(|(_, view), _| self.window_manager.window(*view).is_some());
        self.felix
            .jumps
            .retain(|view, _| self.window_manager.window(*view).is_some());
    }

    pub(super) fn felix_forget_document(&mut self, doc: BufferId) {
        self.felix.indent.remove(&doc);
        for jumps in self.felix.jumps.values_mut() {
            jumps.forget(doc);
        }
        self.felix.selections.retain(|(id, _), _| *id != doc);
        self.felix.history.retain(|(id, _), _| *id != doc);
    }

    pub(super) fn felix_pointer(&mut self, point: Point, add: bool, drag: bool) {
        let Some((doc, view, _, _)) = self.felix_owner() else {
            return;
        };
        let buffer = self.current_buffer();
        let pos = buffer.position_to_char_idx(TextPosition::new(
            point.y,
            self.grapheme_to_char_on_line(point.x, point.y),
        ));
        let rope = buffer.contents_snapshot();
        let text = rope.slice(..);
        let initial = self
            .felix
            .selections
            .get(&(doc, view))
            .cloned()
            .unwrap_or_else(|| Selections::single(text, FelixRange::at(text, pos)));
        let next = initial.pointer(
            text,
            pos,
            self.felix.session.mode == felix_core::Mode::Select,
            add,
            drag,
        );
        self.snippet_session = None;
        self.place_felix_cursor(&next);
        self.felix.selections.insert((doc, view), next);
        self.felix.session.cancel_pending();
        self.felix.generation = self.felix.generation.wrapping_add(1);
    }

    pub(super) fn rebase_felix_views(&mut self, edits: &[AppliedTextEdit]) {
        if let Some(pending) = &mut self.felix.pending_rebase {
            pending.extend_from_slice(edits);
            return;
        }
        let buffer = self.current_buffer();
        let id = buffer.id();
        let snapshot = buffer.contents_snapshot();
        let text = snapshot.slice(..);
        let changes = edits
            .iter()
            .map(|e| (e.start_char, e.end_char, e.new_char_len));
        for ((doc, _), selections) in &mut self.felix.selections {
            if *doc == id {
                *selections = selections.rebase(text, changes.clone());
            }
        }
        for jumps in self.felix.jumps.values_mut() {
            jumps.rebase(id, text, changes.clone());
        }
        // Workspace edits can temporarily activate a buffer without switching the window.
        if self
            .window_manager
            .active_window()
            .is_some_and(|window| window.buffer_index == self.buffer_manager.active_index())
        {
            self.felix.generation = self.felix.generation.wrapping_add(1);
            self.felix.session.cancel_pending();
        }
    }

    /// Keep selections on unchanged characters inside a rewritten line. Pure
    /// insertions and deletions can go directly through the shared transaction.
    pub(super) fn replace_felix_hunks(&mut self, hunks: &[crate::editing::LineHunk]) {
        let before = self.current_buffer().contents_snapshot();
        for hunk in hunks.iter().rev() {
            let start = hunk.old_chars.start;
            let old = before.slice(hunk.old_chars.clone());
            let apply = |editor: &mut Self, from, to, replacement: &str| {
                let doc = editor.current_buffer();
                let range =
                    TextRange::new(doc.char_idx_to_position(from), doc.char_idx_to_position(to));
                editor.replace_range(range, replacement);
            };
            if old.len_chars() == 0
                || hunk.new_text.is_empty()
                || hunk.old_lines.len() + hunk.new_lines.len() > 200
            {
                apply(self, start, hunk.old_chars.end, &hunk.new_text);
                continue;
            }
            let old = old.to_string();
            let diff = similar::TextDiff::configure()
                .timeout(std::time::Duration::from_millis(250))
                .diff_chars(old.as_str(), hunk.new_text.as_str());
            for op in diff
                .ops()
                .iter()
                .rev()
                .filter(|op| op.tag() != similar::DiffTag::Equal)
            {
                let range = op.old_range();
                let insert = diff.new_slices()[op.new_range()].concat();
                apply(self, start + range.start, start + range.end, &insert);
            }
        }
        if self
            .window_manager
            .active_window()
            .is_some_and(|w| w.buffer_index == self.buffer_manager.active_index())
        {
            self.sync_felix_cursor();
        }
    }

    pub(super) fn sync_felix_cursor(&mut self) {
        if let Some(selection) = self.felix_active_selection().cloned() {
            self.place_felix_cursor(&selection);
        }
    }

    pub(super) fn restore_felix_history(&mut self, transaction: Option<&str>, undo: bool) {
        let Some((doc, view, _, _)) = self.felix_owner() else {
            return;
        };
        let recorded = transaction
            .and_then(|key| self.felix.history.get(&(doc, key.to_owned())))
            .map(|(before, after)| if undo { before } else { after });
        let state = recorded
            .or_else(|| self.felix.selections.get(&(doc, view)))
            .cloned();
        if let Some(state) = state {
            self.set_felix_active_selection(state);
        }
    }

    pub(super) fn felix_owner(&self) -> Option<Owner> {
        Some((
            self.current_buffer().id(),
            self.window_manager.active_stable_window_id()?,
            self.current_buffer().revision(),
            self.felix.generation,
        ))
    }

    fn projected_felix_completion(&self) -> Option<CompletionPreview> {
        let item = self.current_dialog.as_ref()?.selected_completion()?;
        let snapshot = self.completion_snapshot.as_ref()?;
        if !self.completion_snapshot_is_current(snapshot) {
            return None;
        }
        let edit = self.completion_main_edit(item, Some(snapshot))?;
        let buffer = self.current_buffer();
        let (mut text, selections) = match snapshot.felix_filter.as_deref() {
            Some(savepoint) => (savepoint.text.clone(), &savepoint.selections),
            None => (buffer.contents_snapshot(), self.felix_active_selection()?),
        };
        let offset = |position: LspPosition| {
            let line = text.get_line(position.line)?;
            let ch = line.try_utf16_cu_to_char(position.character).ok()?;
            (line.char_to_utf16_cu(ch) == position.character
                && (ch == 0 || !matches!(line.char(ch - 1), '\r' | '\n')))
            .then(|| text.line_to_char(position.line) + ch)
        };
        let (start, end) = (offset(edit.range.start)?, offset(edit.range.end)?);
        if start > end {
            return None;
        }
        let (ranges, state) = selections.completion_ranges(text.slice(..), start..end);
        let parsed = matches!(item.insert_text_format, Some(InsertTextFormat::Snippet))
            .then(|| snippet::parse_felix_snippet(&edit.new_text));
        let value = parsed
            .as_ref()
            .map_or(edit.new_text.as_str(), |s| s.text.as_str());
        let length = value.chars().count();
        let cursor = parsed
            .as_ref()
            .and_then(|s| {
                s.placeholders
                    .first()
                    .map(|p| p.end.saturating_sub(1).max(p.start))
                    .or(s.final_cursor)
            })
            .unwrap_or(length);
        let mut removed = 0;
        let mut carets = Vec::with_capacity(ranges.len());
        for (index, range) in ranges.iter().enumerate() {
            carets.push(range.start - removed + index * length + cursor);
            removed += range.len();
        }
        let original = text.clone();
        for range in ranges.iter().rev() {
            let at = range.start;
            text.remove(range.clone());
            text.insert(at, value);
        }
        Some(CompletionPreview {
            owner: self.felix_owner()?,
            text,
            original,
            replacements: ranges,
            replacement_len: length,
            carets,
            primary: state.primary_index(),
        })
    }

    pub(super) fn refresh_felix_preview(&mut self) {
        self.felix.preview = self.projected_felix_completion();
    }

    pub(super) fn felix_preview(
        &self,
        window: &crate::window::Window,
    ) -> Option<&CompletionPreview> {
        let preview = self.felix.preview.as_ref()?;
        (self.buffer_manager[window.buffer_index].id() == preview.owner.0
            && self.felix_owner() == Some(preview.owner)
            && self
                .current_dialog
                .as_ref()?
                .selected_completion()
                .is_some())
        .then_some(preview)
    }

    pub(super) fn felix_completion_key(&self, event: &Event) -> Option<KeyAction> {
        let Event::Key(key) = event else { return None };
        if self.config.editing_model != crate::config::EditingModel::Helix
            || !self.is_insert()
            || self.panel_manager.focused_panel_id().is_some()
            || !self.current_dialog.as_ref()?.accepts_completion_updates()
        {
            return None;
        }
        let selected = self.current_dialog.as_ref()?.selected_completion();
        if self
            .felix
            .preview
            .as_ref()
            .is_some_and(|p| Some(p.owner) != self.felix_owner())
        {
            return Some(KeyAction::Single(Action::CloseDialog));
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                Some(KeyAction::Single(Action::CloseDialog))
            }
            (KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::BackTab, _)
            | (KeyCode::Char('n' | 'p'), KeyModifiers::CONTROL) => None,
            (KeyCode::Enter, KeyModifiers::NONE) if selected.is_some() => None,
            (KeyCode::Esc | KeyCode::Enter, KeyModifiers::NONE) if selected.is_none() => {
                Some(KeyAction::Multiple(vec![
                    Action::CloseDialog,
                    Action::FelixInput {
                        key: if key.code == KeyCode::Esc {
                            "<esc>"
                        } else {
                            "<enter>"
                        }
                        .into(),
                        owner: self.felix_owner()?,
                    },
                ]))
            }
            _ => Some(KeyAction::Single(Action::FelixCompletionKey {
                item: Box::new(selected?.clone()),
                key: *key,
                owner: self.felix_owner()?,
                resolve: true,
            })),
        }
    }

    pub(super) async fn execute_felix_clipboard(
        &mut self,
        action: &Action,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let (Some(input), Some(owner)) = (Input::clipboard_action(action), self.felix_owner())
        else {
            return Ok(());
        };
        let result = self.execute_felix(input, owner, buffer, runtime).await;
        // A failed or empty clipboard operation also ends its key sequence.
        self.felix.session.cancel_pending();
        result
    }

    pub(super) fn cancel_unfocused_felix_input(&mut self) {
        if !self.felix.session.awaiting_input() && self.felix.session.count().is_none() {
            return;
        }
        if self.current_dialog.is_none()
            && self.panel_manager.focused_panel_id().is_none()
            && !self.workspace_manager.is_active()
        {
            return;
        }
        if let Some((selections, top)) = self.felix.session.leave_document_input() {
            let text = self.current_buffer().contents_snapshot();
            let (line, col) = felix_display_col(
                text.slice(..),
                top.min(text.len_chars()),
                self.indentation().tab_width,
            );
            self.vtop = line;
            self.skipcol = if self.wrap { col } else { 0 };
            self.set_felix_active_selection(selections);
        }
    }

    /// Route the tail of a Red-owned document binding without starting a Vim count.
    pub(super) fn felix_native_prefix_event(&mut self, event: &Event) -> Option<KeyAction> {
        let owner = self.felix.native_prefix_owner?;
        let same_document = self.felix_owner() == Some(owner)
            && self.current_dialog.is_none()
            && self.panel_manager.focused_panel_id().is_none()
            && !self.workspace_manager.is_active();
        let pointer_press =
            matches!(event, Event::Mouse(mouse) if matches!(mouse.kind, MouseEventKind::Down(_)));
        if !same_document || pointer_press || self.waiting_key_action.is_none() {
            self.felix.native_prefix_owner = None;
            self.felix.session.cancel_pending();
            self.waiting_key_action = None;
            self.waiting_command = None;
            self.clear_keymap_hints();
            return None;
        }
        if matches!(event, Event::Paste(_)) {
            // The document paste consumes the count and dismisses the shared keymap.
            self.felix.native_prefix_owner = None;
            return None;
        }
        let Event::Key(key) = event else { return None };
        let name = Self::key_string_for_event(event);
        let Some(KeyAction::Nested(bindings)) = &self.waiting_key_action else {
            return None;
        };
        let binding = name.as_ref().and_then(|name| bindings.get(name));
        if (binding.is_none() || self.felix.session.count().is_some())
            && key.modifiers.is_empty()
            && matches!(key.code, KeyCode::Char(ch) if self.felix.session.push_count_digit(ch))
        {
            return Some(KeyAction::None);
        }
        let action = binding.cloned().unwrap_or(KeyAction::None);
        self.waiting_key_action = None;
        self.waiting_command = None;
        if !matches!(action, KeyAction::Nested(_)) {
            self.clear_keymap_hints();
            self.felix.native_prefix_owner = None;
            // Clipboard plans need the count while producing their edits.
            if !matches!(&action, KeyAction::Single(leaf) if Input::clipboard_action(leaf).is_some())
            {
                self.felix.session.cancel_pending();
            }
        }
        Some(action)
    }

    pub(super) fn felix_input_action(&mut self, key: &KeyEvent) -> Option<KeyAction> {
        if !self.felix.session.awaiting_input() {
            let bindings = if self.felix.session.mode == felix_core::Mode::Insert {
                &NATIVE_KEYS.insert
            } else {
                &NATIVE_KEYS.normal
            };
            let name = Self::key_string_for_event(&Event::Key(*key))?;
            let lookup = if name == "Space" { " " } else { &name };
            if let Some(action) = bindings.get(lookup) {
                if matches!(action, KeyAction::Nested(_)) {
                    self.felix.native_prefix_owner = self.felix_owner();
                } else {
                    self.felix.session.cancel_pending();
                }
                return Some(action.clone());
            }
        }
        let token = match key.code {
            KeyCode::Char(ch)
                if key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let prefix = if key.modifiers.contains(KeyModifiers::CONTROL) {
                    "c"
                } else {
                    "a"
                };
                format!("<{prefix}-{ch}>")
            }
            KeyCode::Char(ch) => ch.to_string(),
            KeyCode::Esc => "<esc>".into(),
            KeyCode::Enter => "<enter>".into(),
            KeyCode::Tab if key.modifiers == KeyModifiers::SHIFT => "<backtab>".into(),
            code if key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let prefix = match (
                    key.modifiers.contains(KeyModifiers::CONTROL),
                    key.modifiers.contains(KeyModifiers::ALT),
                ) {
                    (true, true) => "c-a",
                    (true, false) => "c",
                    _ => "a",
                };
                format!("<{prefix}-{code:?}>").to_lowercase()
            }
            KeyCode::Backspace => "<backspace>".into(),
            KeyCode::Left => "<left>".into(),
            KeyCode::Right => "<right>".into(),
            _ => format!("<{:?}>", key.code).to_lowercase(),
        };
        Some(KeyAction::Single(Action::FelixInput {
            key: token,
            owner: self.felix_owner()?,
        }))
    }

    pub(super) fn felix_code_action_range(&self) -> Option<crate::lsp::Range> {
        if self.config.editing_model != crate::config::EditingModel::Helix {
            return None;
        }
        let (doc, view, _, _) = self.felix_owner()?;
        let buf = self.current_buffer();
        let text = buf.contents_snapshot();
        let range = self.felix.selections.get(&(doc, view)).map_or_else(
            || {
                FelixRange::at(
                    text.slice(..),
                    buf.position_to_char_idx(self.cursor_text_position()),
                )
            },
            Selections::primary,
        );
        let pos = |offset| buf.position_to_lsp(buf.char_idx_to_position(offset));
        Some(crate::lsp::Range {
            start: pos(range.start()),
            end: pos(range.end()),
        })
    }

    fn place_felix_cursor(&mut self, selections: &Selections) {
        let buffer = self.current_buffer();
        let text = buffer.contents_snapshot();
        let offset = selections.primary().cursor(text.slice(..));
        let pos = buffer.char_idx_to_position(offset);
        self.move_to_insert_text_position(pos);
    }

    /// Evaluate Red's indentation queries on the text with every proposed break in place.
    /// The syntax cache checks its source as well as its revision. The document and its
    /// undo history are untouched until the host accepts the resulting edit plan.
    fn felix_break_indents(
        &mut self,
        rope: &ropey::Rope,
        initial: &Selections,
        key: &str,
        language: &str,
        indentation: Indentation,
    ) -> Vec<(usize, usize)> {
        if !matches!(key, "<enter>" | "<c-j>" | "o" | "O")
            || rope.len_bytes() > crate::syntax_indent::MAX_BYTES
        {
            return Vec::new();
        }
        let text = rope.slice(..);
        let mut sites: Vec<_> = initial
            .ranges()
            .iter()
            .map(|&range| match key {
                "o" | "O" => felix_core::open_line_site(text, range, key == "o").0,
                _ => range.cursor(text),
            })
            .collect();
        sites.sort_unstable();
        sites.dedup();
        let mut source = rope.clone();
        for &at in sites.iter().rev() {
            source.insert(at, "\n");
        }
        let lines: Vec<_> = sites
            .iter()
            .enumerate()
            .map(|(i, at)| source.char_to_line(at + i + 1))
            .collect();
        let source = source.to_string();
        let id = self.current_buffer().id();
        let revision = self.current_buffer().revision();
        let unit = &self.felix.indent[&id];
        let shift_width = if unit == "\t" {
            indentation.tab_width
        } else {
            unit.len()
        };
        sites
            .into_iter()
            .zip(lines)
            .filter_map(|(at, line)| {
                let syntax = self
                    .syntax_indentation
                    .indent(crate::syntax_indent::IndentRequest {
                        id,
                        revision,
                        language,
                        source: &source,
                        line,
                        shift_width,
                        tab_width: indentation.tab_width,
                        reason: crate::syntax_indent::IndentReason::NewLine,
                    });
                let decision = if syntax == IndentDecision::Keep {
                    indent::indent_for_line(
                        Some(language),
                        &source,
                        line,
                        shift_width,
                        indentation.tab_width,
                    )
                } else {
                    syntax
                };
                match decision {
                    IndentDecision::Columns(columns) => Some((at, columns)),
                    IndentDecision::Keep => None,
                }
            })
            .collect()
    }

    fn plan_felix_key(
        &self,
        text: ropey::RopeSlice<'_>,
        initial: &Selections,
        key: &str,
        register: &[String],
        tab_width: usize,
        language: &FelixLanguageEdit,
    ) -> felix_core::Plan {
        let viewport_line = self.vtop.min(text.len_lines() - 1);
        let display_offset = |line, col| {
            if col == 0 {
                return text.line_to_char(line);
            }
            let source = text.line(line);
            let source = std::borrow::Cow::from(source);
            let grapheme = crate::config::EditingModel::Helix.source_grapheme(
                trim_line_ending(&source),
                col,
                tab_width,
            );
            text.line_to_char(line) + self.grapheme_to_char_on_line(grapheme, line)
        };
        let visual_motion = |offset, delta, goal| {
            let (line, col) = felix_display_col(text, offset, tab_width);
            self.screen_line_target(
                line,
                col,
                delta,
                self.active_content_width().max(1),
                goal,
                true,
            )
            .map(|target| {
                (
                    display_offset(target.line, target.display_col),
                    target.screen_col,
                )
            })
            .unwrap_or((offset, goal.unwrap_or(col)))
        };
        let visual_rows = self.wrap
            || (matches!(
                key,
                "j" | "k"
                    | "<down>"
                    | "<up>"
                    | "<pagedown>"
                    | "<pageup>"
                    | "<c-u>"
                    | "<c-d>"
                    | "<c-f>"
                    | "<c-b>"
                    | "n"
                    | "N"
            ) || self.felix.session.prompt().is_some())
                && self.active_has_markdown_tables();
        let context = felix_core::Context {
            search_history: &self.felix.search,
            register,
            tab_width,
            indent: &self.felix.indent[&self.current_buffer().id()],
            newline_indents: &language.indents,
            comment_tokens: language.tokens,
            page_height: self.vheight(),
            viewport_start: display_offset(viewport_line, self.skipcol),
            scrolloff: self.config.scrolloff.unwrap_or(5),
            visual_motion: visual_rows.then_some(&visual_motion),
        };
        self.felix.session.plan(text, initial, key, &context)
    }

    pub(super) async fn execute_felix(
        &mut self,
        input: Input<'_>,
        owner: Owner,
        frame: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.felix_owner() == Some(owner),
            "document or selection changed before Helix input"
        );
        let id = (owner.0, owner.1);
        let removed_placeholders = match input {
            Input::Key(key) => self.prepare_felix_snippet_input(key),
            _ => false,
        };
        let rope = self.current_buffer().contents_snapshot();
        let text = rope.slice(..);
        let initial = self.felix.selections.get(&id).cloned().unwrap_or_else(|| {
            Selections::single(
                text,
                FelixRange::at(
                    text,
                    self.current_buffer()
                        .position_to_char_idx(self.cursor_text_position()),
                ),
            )
        });
        let indentation = self.indentation_for_buffer_index(self.buffer_manager.active_index());
        self.felix.indent.entry(id.0).or_insert_with(|| {
            felix_core::detect_indent(text)
                .unwrap_or_else(|| indentation.whitespace_for_columns(indentation.shift_width))
        });
        let mut language = FelixLanguageEdit::default();
        let key = match input {
            Input::Key(key) => Some(key),
            Input::Replay(step) => match step.event {
                felix_core::InsertEvent::Key(key) => Some(key.as_str()),
                _ => None,
            },
            _ => None,
        };
        if let Some((key, id)) = key
            .filter(|key| match self.felix.session.mode {
                felix_core::Mode::Insert => matches!(*key, "<enter>" | "<c-j>"),
                _ => matches!(*key, "o" | "O" | "J" | "<a-J>"),
            })
            .filter(|_| !self.felix.session.awaiting_input())
            .and_then(|key| self.current_language_id().map(|id| (key, id)))
        {
            language.tokens = match id.as_str() {
                "rust" => &["///", "//!", "//"],
                "javascript" | "typescript" | "jsx" | "tsx" => &["//"],
                "bash" | "sh" | "fish" | "toml" | "yaml" | "python" | "py" | "powershell" => &["#"],
                "lua" => &["--"],
                _ => &[],
            };
            language.indents = self.felix_break_indents(&rope, &initial, key, &id, indentation);
        }
        let plan_span = perf::PerfSpan::start("felix:plan");
        let mut plan = match input {
            Input::Key(key) => self.plan_felix_key(
                text,
                &initial,
                key,
                &self.felix.unnamed,
                indentation.tab_width,
                &language,
            ),
            Input::Replay(step) => match step.event {
                felix_core::InsertEvent::Key(key) => self.plan_felix_key(
                    text,
                    &initial,
                    key,
                    &self.felix.unnamed,
                    indentation.tab_width,
                    &language,
                ),
                felix_core::InsertEvent::Completion(change) => {
                    self.felix.session.replay_completion(text, &initial, change)
                }
            },
            Input::Bracketed(value) => self.felix.session.bracketed_paste(text, &initial, value),
            Input::ClipboardYank { primary } => {
                let mut plan = self.plan_felix_key(
                    text,
                    &initial,
                    "y",
                    &self.felix.unnamed,
                    indentation.tab_width,
                    &language,
                );
                let values = if primary {
                    vec![text
                        .slice(initial.primary().start()..initial.primary().end())
                        .to_string()]
                } else {
                    plan.register.take().unwrap_or_default()
                };
                plan.register = None;
                if !self.config.clipboard.enabled {
                    self.set_legacy_message(Some("Clipboard is disabled".into()));
                    return self.render(frame);
                }
                if let Err(err) = self
                    .clipboard
                    .copy(&values.join(CLIPBOARD_SEPARATOR), &mut self.stdout)
                {
                    self.set_legacy_message(Some(format!("Clipboard write failed: {err}")));
                    return self.render(frame);
                }
                self.felix.clipboard = values;
                plan
            }
            Input::ClipboardPaste(_) | Input::ClipboardReplace => {
                if !self.config.clipboard.enabled {
                    self.set_legacy_message(Some("Clipboard is disabled".into()));
                    return self.render(frame);
                }
                let value = match self.clipboard.read_text().await {
                    Ok(value @ Some(_)) => value,
                    Ok(None)
                        if self.clipboard.uses_saved_yank() && !self.felix.clipboard.is_empty() =>
                    {
                        None
                    }
                    Ok(None) => return Ok(()),
                    Err(err) => {
                        self.set_legacy_message(Some(format!("Clipboard read failed: {err}")));
                        return self.render(frame);
                    }
                };
                let values = match &value {
                    Some(external)
                        if self.felix.clipboard.is_empty()
                            || self.felix.clipboard.join(CLIPBOARD_SEPARATOR) != *external =>
                    {
                        std::slice::from_ref(external)
                    }
                    _ => &self.felix.clipboard,
                };
                if let Input::ClipboardPaste(placement) = input {
                    self.felix.session.paste(text, &initial, values, placement)
                } else {
                    self.plan_felix_key(
                        text,
                        &initial,
                        "R",
                        values,
                        indentation.tab_width,
                        &language,
                    )
                }
            }
        };
        drop(plan_span);
        let local_key = matches!(input, Input::Key(_))
            && plan.host.is_none()
            && self.felix.session.mode == plan.next.mode
            && !self.felix.session.awaiting_input()
            && !plan.next.awaiting_input()
            && plan.message.is_none()
            && self.felix.preview.is_none()
            && self.current_dialog.is_none();
        let viewport = plan
            .viewport_start
            .map(|offset| felix_display_col(text, offset, indentation.tab_width));
        if !plan.edits.is_empty() && self.current_buffer().directory.is_some() {
            self.set_legacy_message(Some("Directory buffers are read-only".into()));
            return self.render(frame);
        }
        if let Some(saved) = &plan.save_jump {
            self.felix.jumps.entry(id.1).or_default().save(id.0, saved);
        }
        let filtering = !removed_placeholders
            && !plan.edits.is_empty()
            && matches!(input, Input::Key(key) if key.chars().count() == 1 || key == "<backspace>")
            && self.current_dialog.as_ref().is_some_and(|d| {
                d.accepts_completion_updates() && d.selected_completion().is_none()
            })
            && self
                .completion_snapshot
                .as_ref()
                .is_some_and(|s| self.completion_snapshot_is_current(s));
        if filtering {
            let savepoint = self
                .completion_snapshot
                .as_mut()
                .unwrap()
                .felix_filter
                .get_or_insert_with(|| {
                    std::sync::Arc::new(FilterSavepoint {
                        owner,
                        text: rope.clone(),
                        selections: initial.clone(),
                        recipe: self.felix.last_insert.checkpoint(),
                        inverse: Vec::new(),
                    })
                });
            std::sync::Arc::make_mut(savepoint)
                .inverse
                .extend(plan.edits.iter().rev().map(|edit| {
                    (
                        edit.start,
                        edit.start + edit.insert.chars().count(),
                        text.slice(edit.start..edit.end).to_string(),
                    )
                }));
        }
        match input {
            Input::Key(key) => {
                self.felix
                    .last_insert
                    .record(key, self.felix.session.mode, plan.next.mode)
            }
            Input::Replay(step) => plan.history = step.history(plan.history),
            _ => {}
        }
        if plan.history == History::InsertPaste {
            self.commit_felix_transaction(id.0, &initial);
        }
        if matches!(
            plan.history,
            History::Begin | History::Atomic | History::InsertPaste
        ) {
            self.begin_transaction("Helix edit");
            self.felix.before = None;
        }
        if !plan.edits.is_empty() && self.felix.before.is_none() {
            self.felix.before = Some(initial.clone());
        }
        if let Some(query) = &plan.search {
            self.felix.search.push(query.clone());
        }
        if let Some(values) = plan.register.as_ref() {
            self.felix.unnamed.clone_from(values);
        }
        if !plan.edits.is_empty() {
            self.felix.selections.remove(&id);
            if plan.edits.len() > 1 {
                self.felix.pending_rebase = Some(Vec::with_capacity(plan.edits.len()));
            }
        }
        let apply_span = perf::PerfSpan::start("felix:apply");
        for edit in plan.edits.iter().rev() {
            let buffer = self.current_buffer();
            let range = TextRange::new(
                buffer.char_idx_to_position(edit.start),
                buffer.char_idx_to_position(edit.end),
            );
            self.replace_range(range, &edit.insert);
        }
        if let Some(edits) = self.felix.pending_rebase.take() {
            self.rebase_felix_views(&edits);
        }
        drop(apply_span);
        let selection_span = perf::PerfSpan::start("felix:select");
        let after = self.current_buffer().contents_snapshot();
        let selections = plan.selections(after.slice(..));
        self.felix.selections.insert(id, selections.clone());
        self.felix.generation = self.felix.generation.wrapping_add(1);
        self.mode = match plan.next.mode {
            felix_core::Mode::Insert => Mode::Insert,
            _ => Mode::Normal,
        };
        if plan.next.mode != felix_core::Mode::Insert {
            self.snippet_session = None;
        }
        self.felix.session = plan.next;
        if let Some((line, col)) = viewport {
            let pos = self
                .current_buffer()
                .char_idx_to_position(selections.primary().cursor(after.slice(..)));
            self.vtop = line.min(pos.line);
            self.skipcol = if self.wrap { col } else { 0 };
            self.cy = pos.line - self.vtop;
            self.cx = self.char_to_grapheme_on_line(pos.character, pos.line);
        } else {
            self.place_felix_cursor(&selections);
        }
        drop(selection_span);
        if matches!(
            plan.history,
            History::Atomic | History::Commit | History::Checkpoint | History::InsertPaste
        ) {
            self.commit_felix_transaction(id.0, &selections);
        }
        if matches!(plan.history, History::Checkpoint | History::InsertPaste) {
            self.begin_transaction("Helix edit");
        }
        if let Some(request) = plan.host {
            match request {
                HostAction::Undo(count) | HostAction::Redo(count) => {
                    for _ in 0..count {
                        let revision = self.current_buffer().revision();
                        if matches!(request, HostAction::Undo(_)) {
                            self.undo_transaction(frame, runtime).await?;
                        } else {
                            self.redo_transaction(frame, runtime).await?;
                        }
                        if self.current_buffer().revision() == revision {
                            break;
                        }
                    }
                }
                HostAction::SwitchBuffer { forward, count } => {
                    let len = self.buffer_manager.len();
                    let current = self.buffer_manager.active_index();
                    let step = count % len;
                    let delta = if forward { step } else { len - step };
                    self.set_current_buffer(frame, (current + delta) % len)
                        .await?;
                    self.felix.session = Session::default();
                    self.mode = Mode::Normal;
                }
                HostAction::RepeatInsert(count) => {
                    let recipe = self.felix.last_insert.clone();
                    for step in recipe.replay(count) {
                        let owner = self.felix_owner().context("missing repeat destination")?;
                        Box::pin(self.execute_felix(Input::Replay(step), owner, frame, runtime))
                            .await?;
                    }
                }
                HostAction::Jump { forward, count } => {
                    let dest = self.felix.jumps.entry(id.1).or_default().navigate(
                        id.0,
                        &selections,
                        forward,
                        count,
                    );
                    if let Some((doc, saved)) = dest {
                        if let Some(index) = self.buffer_manager.iter().position(|b| b.id() == doc)
                        {
                            if index != self.buffer_manager.active_index() {
                                self.set_current_buffer(frame, index).await?;
                            }
                            let text = self.current_buffer().contents_snapshot();
                            if let Some(current) = Selections::new(
                                text.slice(..),
                                saved.ranges().to_vec(),
                                saved.primary_index(),
                            ) {
                                self.set_felix_active_selection(current);
                            }
                        }
                    }
                }
                HostAction::SyntaxParentEnd => {
                    if self.snippet_session.is_some() {
                        self.navigate_snippet_placeholder(false);
                    } else if let Some(lang) = self
                        .highlight_language_id_for_buffer_index(self.buffer_manager.active_index())
                    {
                        let buffer = self
                            .buffer_manager
                            .active_buffer()
                            .context("missing syntax document")?;
                        if let Some(state) =
                            self.syntax_textobjects
                                .parent_ends(buffer, &lang, &selections)?
                        {
                            self.set_felix_active_selection(state);
                        }
                    }
                }
                HostAction::References => {
                    self.execute(
                        &Action::PluginCommand("LspReferences".into()),
                        frame,
                        runtime,
                    )
                    .await?;
                }
                HostAction::CommandLine => {
                    self.execute_enter_mode(Mode::Command, frame, runtime)
                        .await?;
                }
            }
        }
        if plan.host.is_none() || plan.message.is_some() {
            self.set_legacy_message(plan.message);
        }
        if filtering {
            let owner = self
                .felix_owner()
                .context("missing completion filter destination")?;
            let cursor = self.cursor_text_position();
            let snapshot = self
                .completion_snapshot
                .as_mut()
                .expect("filter retains menu");
            snapshot.cursor = Some(cursor);
            snapshot.revision = owner.2;
            // Keep current_range in request coordinates: resolution, preview, and
            // acceptance all use the original server item after rolling back the filter.
            std::sync::Arc::make_mut(snapshot.felix_filter.as_mut().unwrap()).owner = owner;
        }
        if removed_placeholders || !plan.edits.is_empty() {
            self.notify_change(runtime).await?;
        }
        if matches!(input, Input::Replay(_)) {
            Ok(())
        } else if local_key && !plan.edits.is_empty() {
            // An edit can be visible in several windows on the same buffer.
            self.render_edited_window_rows(frame)
        } else if local_key && initial != selections {
            // Wider selections can change rows outside both caret positions.
            // The row delta is safe only when both overlays fit their caret cell.
            let one_caret = |s: &Selections| {
                s.ranges().len() == 1 && s.primary().end() - s.primary().start() <= 1
            };
            self.check_bounds();
            self.sync_to_window();
            let old_line = text.char_to_line(initial.primary().start());
            let new_line = text.char_to_line(selections.primary().start());
            if one_caret(&initial)
                && one_caret(&selections)
                && self.can_render_cursor_motion_delta()
                && !self.has_multi_cursor_session()
                && !self.has_inline_comments(id.0)
                && !self.cursor_line_wraps(old_line, new_line)
            {
                self.render_cursor_motion_delta(frame)
            } else {
                self.render_motion_frame(frame)
            }
        } else {
            self.render(frame)
        }
    }
}

impl Editor {
    pub(super) async fn apply_felix_completion(
        &mut self,
        savepoint: Option<&FilterSavepoint>,
        main: &LspTextEdit,
        additional: &[LspTextEdit],
        format: Option<&InsertTextFormat>,
        commit_character: Option<char>,
        runtime: &mut Runtime,
    ) -> anyhow::Result<bool> {
        let (rope, before) = match savepoint {
            Some(filter) if Some(filter.owner) == self.felix_owner() => {
                (filter.text.clone(), filter.selections.clone())
            }
            Some(_) => return Ok(false),
            None => (
                self.current_buffer().contents_snapshot(),
                self.felix_active_selection()
                    .expect("active Felix completion")
                    .clone(),
            ),
        };
        let doc = self.current_buffer().id();
        let contents = rope.to_string();
        let contents = contents.as_str();
        let (start, end) = match text_edit_char_range(contents, &main.range) {
            Ok(range) => range,
            Err(err) => {
                self.set_legacy_message(Some(format!("invalid LSP completion edit: {err}")));
                return Ok(false);
            }
        };
        let (ranges, selection) = before.completion_ranges(rope.slice(..), start..end);
        let mut snippet = matches!(format, Some(InsertTextFormat::Snippet))
            .then(|| snippet::parse_felix_snippet(&main.new_text));
        if let (Some(snippet), Some(ch)) = (&mut snippet, commit_character) {
            snippet.insert_commit_character(ch);
        }
        let mut replacement = std::borrow::Cow::Borrowed(
            snippet
                .as_ref()
                .map_or(main.new_text.as_str(), |parsed| parsed.text.as_str()),
        );
        if let (None, Some(ch)) = (&snippet, commit_character) {
            replacement.to_mut().push(ch);
        }
        // Keep projected ranges in original character coordinates. Only edits
        // received from the server require UTF-16 conversion.
        let mut edits: Vec<_> = ranges
            .iter()
            .cloned()
            .map(|range| (range, replacement.as_ref(), true))
            .collect();
        for edit in additional {
            match text_edit_char_range(contents, &edit.range) {
                Ok((start, end)) => edits.push((start..end, &edit.new_text, false)),
                Err(err) => {
                    self.set_legacy_message(Some(format!("invalid LSP completion edit: {err}")));
                    return Ok(false);
                }
            }
        }
        edits.sort_by_key(|(range, _, _)| (range.start, range.end));
        if edits.windows(2).any(|pair| pair[0].0.end > pair[1].0.start) {
            self.set_legacy_message(Some(
                "invalid LSP completion edit: overlapping ranges".into(),
            ));
            return Ok(false);
        }
        if let Some(filter) = savepoint {
            for (start, end, value) in filter.inverse.iter().rev() {
                let buffer = self.current_buffer();
                let range = TextRange::new(
                    buffer.char_idx_to_position(*start),
                    buffer.char_idx_to_position(*end),
                );
                self.replace_range(range, value);
            }
            self.set_felix_active_selection(before.clone());
            self.felix.last_insert.restore(filter.recipe);
        }
        self.felix.last_insert.record_completion(
            &ranges,
            before.primary().cursor(rope.slice(..)),
            &replacement,
        );
        self.commit_felix_transaction(doc, &before);
        self.begin_transaction("Helix completion");
        self.felix.before = Some(before);
        let primary = selection.primary_index();
        self.set_felix_active_selection(selection);
        let mut bases = Vec::new();
        if snippet.is_some() {
            let (mut removed, mut inserted) = (0, 0);
            for (range, value, is_main) in &edits {
                if *is_main {
                    bases.push(range.start - removed + inserted);
                }
                removed += range.len();
                inserted += value.chars().count();
            }
        }
        for (range, value, _) in edits.into_iter().rev() {
            let buffer = self.current_buffer();
            let range = TextRange::new(
                buffer.char_idx_to_position(range.start),
                buffer.char_idx_to_position(range.end),
            );
            self.replace_range(range, value);
        }
        if let Some(snippet) = snippet {
            self.activate_snippet_instances(&bases, primary, &snippet, true);
            if snippet.placeholders.is_empty() {
                let text = self.current_buffer().contents_snapshot();
                let final_offset = snippet
                    .final_cursor
                    .unwrap_or_else(|| snippet.text.chars().count());
                let cursors = bases
                    .iter()
                    .map(|base| FelixRange::at(text.slice(..), base + final_offset))
                    .collect();
                if let Some(state) = Selections::new(text.slice(..), cursors, primary) {
                    self.set_felix_active_selection(state);
                }
            } else {
                self.select_felix_snippet_stop(false);
            }
        } else if let Some(selection) = self.felix_active_selection().cloned() {
            self.place_felix_cursor(&selection);
        }
        self.notify_change(runtime).await?;
        Ok(true)
    }

    fn commit_felix_transaction(&mut self, doc: BufferId, selections: &Selections) {
        let before = self.felix.before.take();
        if !self.commit_transaction(self.cursor_snapshot()) {
            return;
        }
        if let (Some(before), Some(tx)) = (
            before,
            self.current_buffer().undo_history.latest_transaction(),
        ) {
            self.felix
                .history
                .insert((doc, tx.id.clone()), (before, selections.clone()));
        }
    }

    // A cursor-line highlight can cover several screen rows on either side of a move.
    fn cursor_line_wraps(&self, old_line: usize, new_line: usize) -> bool {
        self.wrap
            && self.window_manager.active_window().is_some_and(|window| {
                self.layout_for_window(window)
                    .rows
                    .iter()
                    .any(|row| !row.first_segment && (row.line == old_line || row.line == new_line))
            })
    }

    pub(super) fn felix_status(&self) -> Option<String> {
        let (doc, view, _, _) = self.felix_owner()?;
        let (primary, total) = self
            .felix
            .selections
            .get(&(doc, view))
            .map_or((1, 1), |s| (s.primary_index() + 1, s.ranges().len()));
        Some(format!(
            "{} {primary}/{total}",
            match self.felix.session.mode {
                felix_core::Mode::Insert => "INSERT",
                felix_core::Mode::Select => "SELECT",
                felix_core::Mode::Normal => "NORMAL",
            }
        ))
    }

    pub(super) fn render_felix_selections(
        &self,
        frame: &mut RenderBuffer,
        window: &crate::window::Window,
        terminal_rows: Option<&[usize]>,
    ) {
        let buffer = &self.buffer_manager[window.buffer_index];
        let Some(sel) = self.felix.selections.get(&(buffer.id(), window.id)) else {
            return;
        };
        let rope = buffer.contents_snapshot();
        let text = rope.slice(..);
        let tab_width = self.tab_width_for_buffer_index(window.buffer_index);
        let layout = self.layout_for_window(window);
        let (Some(first), Some(last)) = (layout.rows.first(), layout.rows.last()) else {
            return;
        };
        let visible = rope.line_to_char(first.line)..rope.line_to_char(last.line + 1);
        let ranges = sel.ranges();
        let first_range = ranges.partition_point(|r| r.end() < visible.start);
        for range in ranges[first_range..]
            .iter()
            .take_while(|r| r.start() <= visible.end)
        {
            if range.end() > rope.len_chars() {
                continue;
            }
            let cursor = range.cursor(text);
            let caret = buffer.char_idx_to_position(cursor);
            let start = buffer.char_idx_to_position(range.start());
            let end = buffer.char_idx_to_position(range.end());
            let first = start.line.max(window.vtop);
            let last = end.line.min(window.vtop.saturating_add(window.size.1));
            for row in first..=last {
                let Some(line) = buffer.get(row) else {
                    continue;
                };
                let line = trim_line_ending(&line);
                let col = |idx| {
                    crate::config::EditingModel::Helix
                        .source_width(char_prefix(line, idx), tab_width)
                };
                let left = if row == start.line {
                    col(start.character)
                } else {
                    0
                };
                let right = if row == end.line {
                    col(end.character)
                } else {
                    crate::config::EditingModel::Helix.source_width(line, tab_width) + 1
                };
                // Source glyph spans exclude the cell after the last glyph.
                // Helix can select a newline and can place a cursor at EOF.
                let points_for = |left, right| {
                    let mut points =
                        self.display_col_range_points_in_layout(window, &layout, row, left, right);
                    let eol = crate::config::EditingModel::Helix.source_width(line, tab_width);
                    if let Some((x, y)) = (left <= eol && right > eol)
                        .then(|| self.buffer_to_window_coords(window, grapheme_len(line), row))
                        .flatten()
                    {
                        let point = Point::new(
                            self.window_to_terminal_x(window, x),
                            self.window_to_terminal_y(window, y),
                        );
                        if !points.contains(&point) {
                            points.push(point);
                        }
                    }
                    if let Some(rows) = terminal_rows {
                        points.retain(|point| rows.binary_search(&point.y).is_ok());
                    }
                    points
                };
                if left < right {
                    let points = points_for(left, right);
                    frame.apply_selection_for_points(
                        points,
                        &self.theme.editor_selection_style(),
                        &self.theme,
                        SelectionForegroundPriority::Selection,
                    );
                }
                if row == caret.line {
                    let x = col(caret.character);
                    let next = felix_core::boundary(text, cursor, true);
                    let to = buffer.char_idx_to_position(next);
                    let width = if to.line == row {
                        col(to.character).saturating_sub(x).max(1)
                    } else {
                        1
                    };
                    for pt in points_for(x, x + width) {
                        if let Some(cell) = frame.cells.get_mut(pt.y * frame.width + pt.x) {
                            cell.style = self.theme.synthetic_cursor_style(&cell.style);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn fixture_editor(text: &str) -> (Editor, RenderBuffer, Runtime) {
        named_fixture_editor("example.txt", text)
    }

    fn named_fixture_editor(name: &str, text: &str) -> (Editor, RenderBuffer, Runtime) {
        let mut config =
            Config::from_user_toml_with_overrides("editing_model = 'helix'", &[]).unwrap();
        config.lsp.enabled = false;
        let lsp = Box::new(crate::LspManager::new(config.lsp.clone()));
        let buffers = vec![
            Buffer::new(Some(name.into()), text.into()),
            Buffer::new(Some("other.txt".into()), "other buffer\n".into()),
        ];
        let mut editor = Editor::with_size(lsp, 80, 24, config, Theme::default(), buffers).unwrap();
        editor.test_disable_terminal_output();
        editor.splash_dismissed = true;
        let frame = RenderBuffer::new(80, 24, &Style::default());
        let runtime = Runtime::new();
        (editor, frame, runtime)
    }

    #[tokio::test]
    async fn cursor_row_repaint_matches_full_frame_including_the_previous_selection() {
        let (mut editor, mut frame, mut runtime) = fixture_editor("ab界c\nsecond\n");
        editor.render(&mut frame).unwrap();
        for key in ["l", "l", "j", "k", "h"] {
            let before = frame.cells.clone();
            let old_cursor = editor.last_rendered_cursor_position;
            let owner = editor.felix_owner().unwrap();
            editor
                .execute_felix(Input::Key(key), owner, &mut frame, &mut runtime)
                .await
                .unwrap();
            editor.render(&mut frame).unwrap();
            let expected = frame.cells.clone();
            frame.cells = before;
            editor.last_rendered_cursor_position = old_cursor;
            editor.render_cursor_motion_delta(&mut frame).unwrap();
            assert_eq!(frame.cells, expected, "after {key}");
        }
    }

    #[tokio::test]
    async fn mouse_selects_each_zero_width_space_on_a_continuation_row() {
        let text = format!("{}\u{200b}\u{200b}z\n", "a".repeat(20));
        let (mut editor, mut frame, mut runtime) = fixture_editor(&text);
        editor.wrap = true;
        let chrome = usize::from(editor.size.0) - editor.active_content_width();
        editor.test_set_size((20 + chrome) as u16, editor.size.1);
        let window = editor.active_window_with_editor_view().unwrap();
        let left = editor.gutter_width_for_window(&window) + 1;
        for cell in 0..3 {
            let column = left + cell;
            assert_eq!(
                editor.buffer_to_window_coords(&window, 20 + cell, 0),
                Some((column, 1))
            );
            editor
                .execute_mouse_press(
                    editor.window_to_terminal_x(&window, column) as u16,
                    editor.window_to_terminal_y(&window, 1) as u16,
                    KeyModifiers::NONE,
                    &mut frame,
                    &mut runtime,
                )
                .await
                .unwrap();
            assert_eq!(editor.cx, 20 + cell);
        }
    }

    #[tokio::test]
    async fn clicking_a_partly_visible_joined_emoji_edits_the_whole_grapheme() {
        let (mut editor, mut frame, mut runtime) = fixture_editor("a👩‍💻B\t界C0123456789\n");
        editor.wrap = false;
        editor.vleft = 3; // The emoji spans unwrapped columns 1..5.
        editor.sync_to_window();
        let window = editor.window_manager.active_window().unwrap();
        let x = editor.window_to_terminal_x(window, editor.gutter_width_for_window(window) + 1);
        let y = editor.window_to_terminal_y(window, 0);
        editor
            .execute_mouse_press(
                x as u16,
                y as u16,
                KeyModifiers::NONE,
                &mut frame,
                &mut runtime,
            )
            .await
            .unwrap();
        for key in ["r", "X"] {
            let owner = editor.felix_owner().unwrap();
            editor
                .execute_felix(Input::Key(key), owner, &mut frame, &mut runtime)
                .await
                .unwrap();
        }
        assert_eq!(
            editor.current_buffer().contents_snapshot().to_string(),
            "aXB\t界C0123456789\n"
        );
    }

    #[tokio::test]
    async fn preview_leaves_unicode_document_and_undo_untouched_and_rejects_stale_selections() {
        let (mut editor, mut frame, mut runtime) = fixture_editor("😀foo foo\n");
        editor
            .execute(&Action::AddSampleInlineComment, &mut frame, &mut runtime)
            .await
            .unwrap();
        for key in ["%", "s", "f", "o", "o", "<enter>", "c", "F"] {
            let owner = editor.felix_owner().unwrap();
            editor
                .execute_felix(Input::Key(key), owner, &mut frame, &mut runtime)
                .await
                .unwrap();
        }
        editor.render(&mut frame).unwrap();
        let window = editor.active_window_with_editor_view().unwrap();
        let comment = editor
            .layout_for_window(&window)
            .inline_comments
            .last()
            .unwrap()
            .row;
        let owner = editor.felix_owner();
        let selection = editor.felix_active_selection().cloned();
        let item = serde_json::from_value(json!({
            "label": "Felix", "textEdit": { "range": {
                "start": {"line": 0, "character": 2}, "end": {"line": 0, "character": 3}
            }, "newText": "a\nλ" }
        }))
        .unwrap();
        assert!(editor.show_completion_items(vec![item], editor.completion_snapshot()));
        assert!(editor
            .current_dialog
            .as_ref()
            .unwrap()
            .selected_completion()
            .is_none());
        let event = Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let action = editor
            .handle_event_with_runtime(&event, None)
            .unwrap()
            .unwrap();
        editor
            .handle_key_action(&event, &action, &mut frame, &mut runtime)
            .await
            .unwrap();
        assert_eq!(
            editor.felix.preview.as_ref().unwrap().text.to_string(),
            "😀a\nλ a\nλ\n"
        );
        let (_, y) = editor.render_cursor_position().unwrap();
        assert!(y > editor.window_to_terminal_y(&window, comment));
        assert_eq!(editor.felix_owner(), owner);
        assert_eq!(editor.felix_active_selection(), selection.as_ref());
        assert_eq!(editor.current_buffer().contents(), "😀F F\n");
        editor
            .execute_felix(
                Input::Key("<left>"),
                owner.unwrap(),
                &mut frame,
                &mut runtime,
            )
            .await
            .unwrap();
        let window = editor.window_manager.active_window().unwrap();
        assert!(editor.felix_preview(window).is_none());
        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            editor.felix_completion_key(&enter),
            Some(KeyAction::Single(Action::CloseDialog))
        ));
    }

    #[tokio::test]
    async fn search_and_move_through_projected_table_with_inline_comment() {
        let source =
            "before\n| A | Name |\n| --- | --- |\n| 界 | target |\n| x | second |\nafter\n";
        let (mut editor, mut frame, mut runtime) = named_fixture_editor("example.md", source);
        for wrap in [false, true] {
            editor.wrap = wrap;
            for key in ["g", "g", "/", "t", "a", "r", "g", "e", "t", "<enter>"] {
                let owner = editor.felix_owner().unwrap();
                editor
                    .execute_felix(Input::Key(key), owner, &mut frame, &mut runtime)
                    .await
                    .unwrap();
            }
            let window = editor.active_window_with_editor_view().unwrap();
            let selection = editor.felix_active_selection().unwrap().primary();
            assert_eq!(
                editor
                    .current_buffer()
                    .contents_snapshot()
                    .slice(selection.start()..selection.end()),
                "target"
            );
            editor
                .execute(&Action::AddSampleInlineComment, &mut frame, &mut runtime)
                .await
                .unwrap();
            editor.render(&mut frame).unwrap();
            let layout = editor.layout_for_window(&window);
            assert!(layout
                .rows
                .iter()
                .any(|row| row.line == 3 && row.projection.is_some()));
            assert!(layout
                .inline_comments
                .iter()
                .any(|comment| comment.line == 3));
            let (x, y) = editor.render_cursor_position().unwrap();
            assert_eq!(frame.cells[y * frame.width + x].c, 't');
            for key in ["j", "k"] {
                let owner = editor.felix_owner().unwrap();
                editor
                    .execute_felix(Input::Key(key), owner, &mut frame, &mut runtime)
                    .await
                    .unwrap();
                assert_eq!(
                    editor.cursor_text_position().line,
                    if key == "j" { 4 } else { 3 }
                );
            }
            assert_eq!(editor.current_buffer().contents(), source);
        }
    }

    #[tokio::test]
    async fn split_selection_and_external_edit_follow_document_and_view() {
        let (mut editor, mut frame, mut runtime) = fixture_editor("éfoo foo\n");
        let original = editor.felix_owner().unwrap();
        for key in ["%", "s", "f", "o", "o", "<enter>"] {
            let owner = editor.felix_owner().unwrap();
            editor
                .execute_felix(Input::Key(key), owner, &mut frame, &mut runtime)
                .await
                .unwrap();
        }
        let state = editor.felix_active_selection().unwrap().clone();
        assert_eq!(state.ranges().len(), 2);
        editor
            .execute(&Action::SplitVertical, &mut frame, &mut runtime)
            .await
            .unwrap();
        let other = editor.felix_owner().unwrap();
        assert_ne!(other.1, original.1);
        assert_eq!(editor.felix_active_selection().unwrap(), &state);
        editor
            .execute_felix(Input::Key(","), other, &mut frame, &mut runtime)
            .await
            .unwrap();
        let stale = editor.felix_owner().unwrap();
        editor.begin_transaction("external replacement");
        editor.replace_range(
            TextRange::new(TextPosition::new(0, 0), TextPosition::new(0, 1)),
            "😀xx",
        );
        editor.commit_transaction(editor.cursor_snapshot());
        assert_eq!(
            editor.felix_active_selection().unwrap().ranges(),
            &[FelixRange { anchor: 3, head: 6 }]
        );
        assert!(editor
            .execute_felix(Input::Key("d"), stale, &mut frame, &mut runtime)
            .await
            .is_err());
        assert_eq!(editor.current_buffer().contents(), "😀xxfoo foo\n");
        let index = editor.window_manager.window_index(original.1).unwrap();
        editor.set_active_window(index);
        let ranges = editor.felix_active_selection().unwrap().ranges();
        assert_eq!(
            ranges,
            &[
                FelixRange { anchor: 3, head: 6 },
                FelixRange {
                    anchor: 7,
                    head: 10
                }
            ]
        );
        let request = editor.felix_code_action_range().unwrap();
        assert_eq!((request.start.character, request.end.character), (4, 7));
    }

    #[tokio::test]
    async fn language_server_edit_preserves_view_and_rejects_stale_response() {
        let (mut editor, mut frame, mut runtime) = fixture_editor("😀foo foo\n");
        let root = tempfile::tempdir().unwrap();
        for (index, file) in ["active.txt", "background.txt"].into_iter().enumerate() {
            editor.buffer_manager[index].file =
                Some(root.path().join(file).to_str().unwrap().into());
        }
        for key in ["%", "s", "f", "o", "o", "<enter>"] {
            let owner = editor.felix_owner().unwrap();
            editor
                .execute_felix(Input::Key(key), owner, &mut frame, &mut runtime)
                .await
                .unwrap();
        }
        let selection = editor.felix_active_selection().unwrap().clone();
        let owner = editor.felix_owner().unwrap();
        let make_action = |buffer: &Buffer, replacement: &str| {
            let uri = buffer.uri().unwrap().unwrap();
            let start = crate::lsp::Position {
                line: 0,
                character: 0,
            };
            Action::ApplyLspWorkspaceEdit {
                documents: vec![LspDocumentEdit {
                    uri: uri.clone(),
                    version: None,
                    edits: vec![LspTextEdit {
                        range: crate::lsp::Range { start, end: start },
                        new_text: replacement.into(),
                    }],
                }],
                expected_revisions: vec![(uri, buffer.revision())],
                command: None,
                source: None,
                label: "server insertion".into(),
            }
        };
        let background = make_action(&editor.buffer_manager[1], "other ");
        editor
            .execute(&background, &mut frame, &mut runtime)
            .await
            .unwrap();
        assert_eq!(editor.felix_owner(), Some(owner));
        assert_eq!(editor.felix_active_selection(), Some(&selection));
        assert_eq!(editor.buffer_manager[1].contents(), "other other buffer\n");
        let edit = make_action(editor.current_buffer(), "éé");
        editor
            .execute(&edit, &mut frame, &mut runtime)
            .await
            .unwrap();
        let moved = editor.felix_active_selection().unwrap().clone();
        assert_eq!(
            moved.ranges(),
            &[
                FelixRange { anchor: 3, head: 6 },
                FelixRange {
                    anchor: 7,
                    head: 10
                }
            ]
        );
        assert_eq!(editor.cursor_text_position(), TextPosition::new(0, 5));
        let revision = editor.current_buffer().revision();
        editor
            .execute(&edit, &mut frame, &mut runtime)
            .await
            .unwrap();
        assert_eq!(editor.current_buffer().revision(), revision);
        assert_eq!(editor.felix_active_selection(), Some(&moved));
        assert_eq!(editor.current_buffer().contents(), "éé😀foo foo\n");
    }

    #[tokio::test]
    async fn file_reload_preserves_selections_amid_separate_edits_on_one_line() {
        let (mut editor, mut frame, mut runtime) = fixture_editor("A foo B foo C\r\n");
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("example.txt");
        editor.current_buffer_mut().file = Some(path.to_str().unwrap().into());
        for key in ["%", "s", "f", "o", "o", "<enter>"] {
            let owner = editor.felix_owner().unwrap();
            editor
                .execute_felix(Input::Key(key), owner, &mut frame, &mut runtime)
                .await
                .unwrap();
        }
        let updated = "🎈A foo ééB foo C\r\n";
        std::fs::write(path, updated).unwrap();
        editor
            .execute(&Action::ReloadFile(false), &mut frame, &mut runtime)
            .await
            .unwrap();
        assert_eq!(editor.current_buffer().contents(), updated);
        assert_eq!(
            editor.felix_active_selection().unwrap().ranges(),
            &[
                FelixRange { anchor: 3, head: 6 },
                FelixRange {
                    anchor: 11,
                    head: 14
                }
            ]
        );
        assert_eq!(editor.cursor_text_position(), TextPosition::new(0, 5));
        assert!(!editor.current_buffer().is_dirty());
    }

    #[tokio::test]
    async fn completion_validates_additional_edits_against_every_cursor_before_changing_text() {
        let (mut editor, mut frame, mut runtime) = fixture_editor("😀\nfoo foo\n");
        for key in ["%", "s", "f", "o", "o", "<enter>", "c", "F"] {
            let owner = editor.felix_owner().unwrap();
            editor
                .execute_felix(Input::Key(key), owner, &mut frame, &mut runtime)
                .await
                .unwrap();
        }
        let edit = |line, start, end, value: &str| LspTextEdit {
            range: Range {
                start: LspPosition {
                    line,
                    character: start,
                },
                end: LspPosition {
                    line,
                    character: end,
                },
            },
            new_text: value.into(),
        };
        let request_selections = editor.felix_active_selection().unwrap().clone();
        let candidate = serde_json::from_value(json!({"label": "Filled"})).unwrap();
        assert!(editor.show_completion_items(vec![candidate], editor.completion_snapshot()));
        let owner = editor.felix_owner().unwrap();
        editor
            .execute_felix(Input::Key("i"), owner, &mut frame, &mut runtime)
            .await
            .unwrap();
        let filter = editor
            .completion_snapshot
            .as_ref()
            .unwrap()
            .felix_filter
            .clone()
            .unwrap();
        let state = editor.felix_active_selection().unwrap().clone();
        let owner = editor.felix_owner();
        let checkpoint = editor.felix.last_insert.checkpoint();
        let primary = edit(1, 0, 1, "Filled");
        let overlap = edit(1, 2, 3, "conflict");
        assert!(!editor
            .apply_felix_completion(
                Some(&filter),
                &primary,
                &[overlap],
                None,
                None,
                &mut runtime
            )
            .await
            .unwrap());
        assert_eq!(editor.felix_owner(), owner);
        assert_eq!(editor.felix_active_selection(), Some(&state));
        assert_eq!(editor.current_buffer().contents(), "😀\nFi Fi\n");
        assert_eq!(editor.felix.last_insert.checkpoint(), checkpoint);
        let extra = edit(0, 0, 2, "λ");
        assert!(editor
            .apply_felix_completion(Some(&filter), &primary, &[extra], None, None, &mut runtime)
            .await
            .unwrap());
        assert_eq!(editor.current_buffer().contents(), "λ\nFilled Filled\n");
        for key in ["!", "<esc>", "u"] {
            let owner = editor.felix_owner().unwrap();
            editor
                .execute_felix(Input::Key(key), owner, &mut frame, &mut runtime)
                .await
                .unwrap();
        }
        assert_eq!(editor.current_buffer().contents(), "😀\nF F\n");
        assert_eq!(editor.felix_active_selection(), Some(&request_selections));
    }

    #[tokio::test]
    async fn snippet_final_stop_preserves_both_insert_cursors_and_undo() {
        for separator in ["=", ""] {
            let (mut editor, mut frame, mut runtime) = fixture_editor("foo foo\n");
            for key in ["%", "s", "f", "o", "o", "<enter>", "c", "S"] {
                let owner = editor.felix_owner().unwrap();
                editor
                    .execute_felix(Input::Key(key), owner, &mut frame, &mut runtime)
                    .await
                    .unwrap();
            }
            let edit = LspTextEdit {
                range: Range {
                    start: LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: LspPosition {
                        line: 0,
                        character: 1,
                    },
                },
                new_text: format!("${{1:one}}{separator}${{2:two}}$0"),
            };
            assert!(editor
                .apply_felix_completion(
                    None,
                    &edit,
                    &[],
                    Some(&InsertTextFormat::Snippet),
                    None,
                    &mut runtime
                )
                .await
                .unwrap());
            for (key, expected) in [
                ("X", "X=two X=two"),
                ("<tab>", "X=two X=two"),
                ("Y", "X=Y X=Y"),
                ("<tab>", "X=Y X=Y"),
                ("!", "X=Y! X=Y!"),
                ("<esc>", "X=Y! X=Y!"),
                ("u", "S S"),
                ("u", "foo foo"),
            ] {
                let owner = editor.felix_owner().unwrap();
                editor
                    .execute_felix(Input::Key(key), owner, &mut frame, &mut runtime)
                    .await
                    .unwrap();
                assert_eq!(
                    editor.current_buffer().contents().trim_end(),
                    expected.replace('=', separator),
                    "{key}"
                );
                assert_eq!(editor.felix_active_selection().unwrap().ranges().len(), 2);
            }
        }
    }

    #[tokio::test]
    async fn clipboard_provider_failures_leave_the_document_and_registers_intact() {
        struct Unavailable;
        impl crate::clipboard::ClipboardProvider for Unavailable {
            fn get_text(&mut self) -> anyhow::Result<Option<String>> {
                anyhow::bail!("test read failure")
            }
            fn set_text(&mut self, _: &str) -> anyhow::Result<()> {
                anyhow::bail!("test write failure")
            }
        }
        let (mut editor, mut frame, mut runtime) = fixture_editor("aa bb\n");
        editor.test_set_clipboard(Box::new(Unavailable));
        editor.config.clipboard.enabled = true;
        for key in ["e", "y", "w", "v", "l"] {
            let owner = editor.felix_owner().unwrap();
            editor
                .execute_felix(Input::Key(key), owner, &mut frame, &mut runtime)
                .await
                .unwrap();
        }
        let owner = editor.felix_owner().unwrap();
        let original = editor.felix_active_selection().unwrap().clone();
        let unnamed = editor.felix.unnamed.clone();
        let history = editor.current_buffer().undo_history.current_index();
        editor.felix.clipboard = vec!["prior".into()];
        for input in [
            Input::ClipboardYank { primary: false },
            Input::ClipboardPaste(felix_core::Paste::After),
            Input::ClipboardReplace,
        ] {
            editor
                .execute_felix(input, owner, &mut frame, &mut runtime)
                .await
                .unwrap();
            assert_eq!(editor.felix_owner(), Some(owner));
            assert_eq!(editor.felix_active_selection(), Some(&original));
            assert_eq!(editor.felix.session.mode, felix_core::Mode::Select);
            assert_eq!(editor.felix.unnamed, unnamed);
            assert_eq!(editor.felix.clipboard, ["prior"]);
            assert_eq!(editor.current_buffer().contents(), "aa bb\n");
            assert_eq!(
                editor.current_buffer().undo_history.current_index(),
                history
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn attached_clipboard_keeps_matching_fragments_and_reads_external_changes() {
        let (mut editor, _, _) = fixture_editor("a bb\n");
        let clipboard = crate::clipboard::ClientClipboard::default();
        clipboard.set_attached(true);
        editor.test_set_clipboard(Box::new(clipboard.clone()));
        editor.config.clipboard.enabled = true;
        let key = |ch| Event::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        for event in "%s[a-z]+".chars().map(key).chain([
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            key('y'),
            key(' '),
            key('y'),
        ]) {
            editor.test_execute_event(event).await.unwrap();
        }
        assert_eq!(clipboard.take_write().as_deref(), Some("a\nbb"));
        clipboard.finish_input();
        for (response, expected) in [
            (Ok(None), "a bb\n"),
            (Err("client failed".into()), "a bb\n"),
            (Ok(Some("a\nbb".into())), "aa bbbb\n"),
            (Ok(Some("ZZ".into())), "aZZ bbZZ\n"),
            (Ok(Some("a\nbbXYZ".into())), "aa\nbbXYZ bba\nbbXYZ\n"),
        ] {
            clipboard.begin_input(None);
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            clipboard.set_reader(tx);
            let reply =
                tokio::spawn(async move { rx.recv().await.unwrap().send(response).unwrap() });
            for ch in [' ', 'p'] {
                editor.test_execute_event(key(ch)).await.unwrap();
            }
            reply.await.unwrap();
            clipboard.finish_input();
            assert_eq!(editor.current_buffer().contents(), expected);
            if expected != "a bb\n" {
                editor.test_execute_event(key('u')).await.unwrap();
            }
            assert_eq!(editor.felix.unnamed, ["a", "bb"]);
        }
    }

    #[tokio::test]
    async fn clipboard_replace_repeats_external_text_with_document_line_endings() {
        let (mut editor, mut frame, mut runtime) = fixture_editor("aa bb\r\n");
        editor.test_set_clipboard(Box::new(
            crate::clipboard::MemoryClipboardProvider::with_text("x\ry\nz\r\n"),
        ));
        editor.config.clipboard.enabled = true;
        for key in ["%", "s", "[", "a", "b", "]", "+", "<enter>", "2"] {
            let owner = editor.felix_owner().unwrap();
            editor
                .execute_felix(Input::Key(key), owner, &mut frame, &mut runtime)
                .await
                .unwrap();
        }
        let owner = editor.felix_owner().unwrap();
        editor
            .execute_felix(Input::ClipboardReplace, owner, &mut frame, &mut runtime)
            .await
            .unwrap();
        let replacement = "x\r\ny\r\nz\r\n".repeat(2);
        assert_eq!(
            editor.current_buffer().contents(),
            format!("{replacement} {replacement}\r\n")
        );
        let owner = editor.felix_owner().unwrap();
        editor
            .execute_felix(Input::Key("u"), owner, &mut frame, &mut runtime)
            .await
            .unwrap();
        assert_eq!(editor.current_buffer().contents(), "aa bb\r\n");
        assert_eq!(editor.felix_active_selection().unwrap().ranges().len(), 2);
    }

    #[tokio::test]
    async fn explicit_clipboard_preserves_fragments_and_bracketed_paste_edits_all_ranges() {
        let (mut editor, mut frame, mut runtime) = fixture_editor("a bb\n");
        let provider = crate::clipboard::MemoryClipboardProvider::with_text("system");
        let storage = provider.shared_text();
        editor.test_set_clipboard(Box::new(provider));
        editor.config.clipboard.enabled = true;
        for key in ["%", "s", "[", "a", "b", "]", "+", "<enter>", "y"] {
            let owner = editor.felix_owner().unwrap();
            editor
                .execute_felix(Input::Key(key), owner, &mut frame, &mut runtime)
                .await
                .unwrap();
        }
        assert_eq!(storage.lock().unwrap().as_deref(), Some("system"));
        let owner = editor.felix_owner().unwrap();
        editor
            .execute_felix(
                Input::ClipboardYank { primary: false },
                owner,
                &mut frame,
                &mut runtime,
            )
            .await
            .unwrap();
        assert_eq!(
            storage.lock().unwrap().as_deref(),
            Some(["a", "bb"].join(CLIPBOARD_SEPARATOR).as_str())
        );
        let owner = editor.felix_owner().unwrap();
        editor
            .execute_felix(
                Input::ClipboardPaste(felix_core::Paste::After),
                owner,
                &mut frame,
                &mut runtime,
            )
            .await
            .unwrap();
        assert_eq!(editor.current_buffer().contents(), "aa bbbb\n");
        let event = Event::Paste("é".into());
        let action = editor
            .handle_event_with_runtime(&event, Some(&runtime))
            .unwrap()
            .unwrap();
        editor
            .handle_key_action(&event, &action, &mut frame, &mut runtime)
            .await
            .unwrap();
        assert_eq!(editor.current_buffer().contents(), "aéa bbébb\n");
    }

    #[tokio::test]
    async fn search_completion_pages_keep_the_selected_candidate_visible() {
        let (mut editor, mut frame, mut runtime) = fixture_editor("doc\n");
        editor.felix.search = (0..23).map(|i| format!("item{i:02}")).collect();
        for key in ["/", "<backtab>"] {
            let owner = editor.felix_owner().unwrap();
            editor
                .execute_felix(Input::Key(key), owner, &mut frame, &mut runtime)
                .await
                .unwrap();
        }
        let row = |y: usize| {
            frame.cells[y * 80..y * 80 + 6]
                .iter()
                .map(|c| c.c)
                .collect::<String>()
        };
        assert_eq!(row(13), "item20");
        assert_eq!(row(15), "item22");
        let selected = editor
            .theme
            .list_selection_style()
            .fallback_bg(&editor.theme.ui_style.popup.fallback_bg(&editor.theme.style));
        assert_eq!(frame.cells[15 * 80].style, selected);
        assert_ne!(frame.cells[13 * 80].style, selected);
        let owner = editor.felix_owner().unwrap();
        editor
            .execute_felix(Input::Key("<tab>"), owner, &mut frame, &mut runtime)
            .await
            .unwrap();
        assert_eq!(frame.cells[13 * 80].style, selected);
        assert_eq!(
            frame.cells[13 * 80 + 40..13 * 80 + 46]
                .iter()
                .map(|c| c.c)
                .collect::<String>(),
            "item10"
        );
    }

    #[tokio::test]
    async fn render_distinguishes_secondary_cursor_and_trailing_eof() {
        let (mut editor, _, _) = fixture_editor("ab\n");
        let text = editor.current_buffer().contents_snapshot();
        let state = Selections::new(
            text.slice(..),
            vec![
                FelixRange { anchor: 0, head: 2 },
                FelixRange { anchor: 3, head: 3 },
            ],
            1,
        )
        .unwrap();
        editor.set_felix_active_selection(state);
        let window = editor.active_window_with_editor_view().unwrap();
        let mut frame = RenderBuffer::new(80, 24, &Style::default());
        editor.render_felix_selections(&mut frame, &window, None);
        let row = window.position.y + editor.window_content_top(&window);
        let col = window.position.x + editor.gutter_width_for_window(&window) + 1;
        let selected = &frame.cells[row * frame.width + col].style;
        let secondary = &frame.cells[row * frame.width + col + 1].style;
        let eof = &frame.cells[(row + 1) * frame.width + col].style;
        assert_ne!(selected, secondary);
        assert_eq!(eof, &editor.theme.synthetic_cursor_style(&Style::default()));
        assert_eq!(editor.cursor_text_position(), TextPosition::new(1, 0));
    }

    #[tokio::test]
    async fn native_prefixes_consume_counts_on_completion_cancel_and_paste() {
        let (mut editor, _, _) = fixture_editor("0123456789\n");
        let key = |ch| Event::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        let escape = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let window = Event::Key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));

        for prefix in [key(' '), window] {
            for event in [key('2'), prefix, key('3')] {
                editor.test_execute_event(event).await.unwrap();
            }
            assert_eq!(editor.felix.session.count(), Some(23));
            assert!(editor.waiting_key_action.is_some());
            editor.test_execute_event(escape.clone()).await.unwrap();
            assert_eq!(editor.felix.session.count(), None);
            assert!(editor.waiting_key_action.is_none());
        }
        for event in [key(' '), key('w'), key('n'), key('2'), escape.clone()] {
            editor.test_execute_event(event).await.unwrap();
        }
        assert_eq!(editor.felix.session.count(), None);
        assert!(editor.waiting_key_action.is_none());

        editor.config.clipboard.enabled = false;
        for event in [key('2'), key(' '), key('p')] {
            editor.test_execute_event(event).await.unwrap();
        }
        assert_eq!(editor.felix.session.count(), None);

        for event in [key('2'), key(' '), key('f')] {
            editor.test_execute_event(event).await.unwrap();
        }
        assert!(editor.current_dialog.is_some());
        editor.test_execute_event(escape).await.unwrap();
        editor.test_execute_event(key('l')).await.unwrap();
        assert_eq!(editor.cx, 1);

        for event in [key(' '), key('2'), Event::Paste("X".into()), key('l')] {
            editor.test_execute_event(event).await.unwrap();
        }
        assert_eq!(
            editor.current_buffer().contents_snapshot().to_string(),
            "0XX123456789\n"
        );
        assert_eq!(editor.cx, 3);
    }

    #[tokio::test]
    async fn asynchronous_picker_cancels_document_arguments_counts_and_search_previews() {
        let (mut editor, mut frame, mut runtime) = fixture_editor("start and target\n");
        let key = |ch| Event::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        for prefix in ["2", "2f", "mi", "/target"] {
            for ch in prefix.chars() {
                editor.test_execute_event(key(ch)).await.unwrap();
            }
            editor
                .execute(&Action::FilePicker, &mut frame, &mut runtime)
                .await
                .unwrap();
            assert!(editor.current_dialog.is_some());
            editor
                .test_execute_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
                .await
                .unwrap();
            assert!(editor.current_dialog.is_none());
            assert!(!editor.felix.session.awaiting_input());
            assert_eq!(editor.felix.session.count(), None);
            editor.test_execute_event(key('l')).await.unwrap();
            assert_eq!(editor.cx, 1, "{prefix}");
            editor.test_execute_event(key('h')).await.unwrap();
        }
    }

    #[tokio::test]
    async fn native_prefix_cannot_open_a_picker_after_a_document_switch() {
        let (mut editor, mut frame, _) = fixture_editor("first document\n");
        let key = |ch| Event::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        for event in [key('2'), key(' ')] {
            editor.test_execute_event(event).await.unwrap();
        }
        editor.set_current_buffer(&mut frame, 1).await.unwrap();
        // f is a fresh find in the second document, rather than Space f in the first.
        editor.test_execute_event(key('f')).await.unwrap();
        assert!(editor.current_dialog.is_none());
        editor.test_execute_event(key('b')).await.unwrap();
        assert_eq!(editor.cx, 6);
        assert_eq!(editor.felix.session.count(), None);
    }

    #[tokio::test]
    async fn agreed_leader_actions_resolve_through_document_input() {
        let (mut editor, mut frame, mut runtime) = fixture_editor("abc");
        for (key, action) in [
            ('f', Action::FilePicker),
            ('a', Action::CodeAction),
            ('e', Action::PluginCommand("NeoTree".into())),
            ('=', Action::FormatDocument),
            ('d', Action::FelixDocumentDiagnostics),
        ] {
            let press = |c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            let event = press(' ');
            let leader = editor
                .handle_event_with_runtime(&event, Some(&runtime))
                .unwrap()
                .unwrap();
            editor
                .handle_key_action(&event, &leader, &mut frame, &mut runtime)
                .await
                .unwrap();
            assert_eq!(
                editor
                    .handle_event_with_runtime(&press(key), Some(&runtime))
                    .unwrap(),
                Some(KeyAction::Single(action))
            );
        }
    }

    #[tokio::test]
    async fn change_two_matches_matches_reference_at_every_key() {
        let fixture: Vec<Value> = serde_json::from_str(include_str!(
            "../../scripts/felix/fixtures/change-two-matches.json"
        ))
        .unwrap();
        replay("change-two-matches", "foo foo\n", &fixture, None).await;
    }

    #[tokio::test]
    async fn motions_match_reference_at_every_key() {
        // CI supplies a fresh capture. The checked fixture also runs offline.
        let capture = match std::env::var_os("FELIX_SCENARIOS") {
            Some(path) => std::fs::read_to_string(path).expect("read live reference"),
            None => include_str!("../../scripts/felix/fixtures/motions.json").into(),
        };
        let suite: Value = serde_json::from_str(&capture).unwrap();
        let cases = suite["cases"].as_array().unwrap();
        assert!(!cases.is_empty());
        for case in cases {
            replay(
                case["name"].as_str().unwrap(),
                case["text"].as_str().unwrap(),
                case["events"].as_array().unwrap(),
                case["wrap_width"].as_u64().map(|width| width as usize),
            )
            .await;
        }
    }

    async fn replay(name: &str, text: &str, fixture: &[Value], wrap_width: Option<usize>) {
        assert!(!fixture.is_empty());
        let (mut editor, mut frame, mut runtime) = fixture_editor(text);
        if let Some(width) = wrap_width {
            editor.wrap = true;
            let chrome = usize::from(editor.size.0) - editor.active_content_width();
            editor.test_set_size((width + chrome) as u16, editor.size.1);
            assert_eq!(editor.active_content_width(), width);
        }
        for (index, expected) in fixture.iter().enumerate() {
            let token = expected["key"].as_str().unwrap();
            let mut modifiers = KeyModifiers::NONE;
            let code = match token {
                token if token.starts_with("<a-") || token.starts_with("<c-") => {
                    modifiers = if token.starts_with("<a-") {
                        KeyModifiers::ALT
                    } else {
                        KeyModifiers::CONTROL
                    };
                    match &token[3..token.len() - 1] {
                        "backspace" => KeyCode::Backspace,
                        "delete" => KeyCode::Delete,
                        _ => KeyCode::Char(token.chars().nth(3).unwrap()),
                    }
                }
                "<backspace>" => KeyCode::Backspace,
                "<up>" => KeyCode::Up,
                "<down>" => KeyCode::Down,
                "<left>" => KeyCode::Left,
                "<right>" => KeyCode::Right,
                "<home>" => KeyCode::Home,
                "<end>" => KeyCode::End,
                "<delete>" => KeyCode::Delete,
                "<pageup>" => KeyCode::PageUp,
                "<pagedown>" => KeyCode::PageDown,
                "<esc>" => KeyCode::Esc,
                "<enter>" => KeyCode::Enter,
                "<tab>" => KeyCode::Tab,
                "<backtab>" => KeyCode::BackTab,
                _ => KeyCode::Char(token.chars().next().unwrap()),
            };
            let event = match expected.get("paste") {
                Some(value) => Event::Paste(value.as_str().unwrap().to_owned()),
                None => Event::Key(KeyEvent::new(code, modifiers)),
            };
            let action = editor
                .handle_event_with_runtime(&event, Some(&runtime))
                .unwrap()
                .expect("document key");
            editor
                .handle_key_action(&event, &action, &mut frame, &mut runtime)
                .await
                .unwrap();
            let (doc, view, _, _) = editor.felix_owner().unwrap();
            let s = &editor.felix.selections[&(doc, view)];
            let mut actual = json!({"key":token,"document":format!("~/{}",editor.current_file_name().unwrap()),
                "text":editor.current_buffer().contents(),"ranges":s.ranges().iter().map(|r|[r.anchor,r.head]).collect::<Vec<_>>(),
                "primary":s.primary_index(),"mode":format!("{:?}",editor.felix.session.mode),
                "search":(!editor.felix.search.is_empty()).then(|| editor.felix.search.iter().rev().collect::<Vec<_>>()),
                "unnamed":(!editor.felix.unnamed.is_empty()).then_some(&editor.felix.unnamed),
                "pending":editor.felix.session.pending_keys()});
            if let Event::Paste(value) = &event {
                actual["paste"] = json!(value);
            }
            if expected.get("prompt").is_some() {
                actual["prompt"] = json!(editor.felix.session.prompt().map(|(_, line, cursor)| {
                    let (candidates, selected) = editor.felix.session.prompt_completions().unwrap();
                    json!({"line":line,"cursor":cursor,"candidates":candidates,"selected":selected})
                }));
                if let Some((label, line, cursor)) = editor.felix.session.prompt() {
                    assert_eq!(
                        editor.render_cursor_position(),
                        Some((
                            label.len() + 1 + display_width(&line[..cursor]),
                            usize::from(editor.size.1) - 1
                        )),
                        "{name} input {} caret",
                        index + 1
                    );
                }
            }
            if expected.get("last_insert").is_some() {
                actual["last_insert"] = json!(editor.felix.last_insert.keys());
            }
            if expected.get("viewport").is_some() {
                let window = editor.active_window_with_editor_view().unwrap();
                let layout = editor.layout_for_window(&window);
                let first = layout.rows.first().unwrap();
                let text = editor.current_buffer().contents_snapshot();
                let anchor = text.line_to_char(first.line)
                    + text.line(first.line).byte_to_char(first.start_byte);
                actual["viewport"] = json!({"height":editor.vheight(),"anchor":anchor});
            }
            if expected.get("count").is_some() {
                actual["count"] = json!(editor.felix.session.count());
            }
            assert_eq!(
                &actual,
                expected,
                "{name}: divergence at input {} ({token})",
                index + 1
            );
        }
    }
}
