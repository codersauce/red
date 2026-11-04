//! Red adapter for document-window Felix sessions.

use super::*;
use crate::theme::SelectionForegroundPriority;
use anyhow::Context as _;
use felix_core::{History, HostAction, Range as FelixRange, Selections, Session};
use std::collections::HashMap;

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

#[derive(Clone, Copy)]
pub(super) enum Input<'a> {
    Key(&'a str),
    Bracketed(&'a str),
    Clipboard(felix_core::Paste),
}

type Owner = (BufferId, WindowId, u64, u64);

#[derive(Default)]
pub(super) struct FelixHost {
    session: Session,
    last_insert: Vec<String>,
    indent: HashMap<BufferId, String>,
    selections: HashMap<(BufferId, WindowId), Selections>,
    generation: u64,
    unnamed: Vec<String>,
    search: Vec<String>,
    before: Option<Selections>,
    history: HashMap<(BufferId, String), (Selections, Selections)>,
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
    }

    pub(super) fn felix_forget_document(&mut self, doc: BufferId) {
        self.felix.indent.remove(&doc);
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
        self.place_felix_cursor(&next);
        self.felix.selections.insert((doc, view), next);
        self.felix.session.cancel_pending();
        self.felix.generation = self.felix.generation.wrapping_add(1);
    }

    pub(super) fn rebase_felix_views(&mut self, edit: AppliedTextEdit) {
        let buffer = self.current_buffer();
        let id = buffer.id();
        let snapshot = buffer.contents_snapshot();
        for ((doc, _), selections) in &mut self.felix.selections {
            if *doc == id {
                *selections = selections.rebase(
                    snapshot.slice(..),
                    edit.start_char,
                    edit.end_char,
                    edit.new_char_len,
                );
            }
        }
        self.felix.generation = self.felix.generation.wrapping_add(1);
        self.felix.session.cancel_pending();
    }

    pub(super) fn felix_owner(&self) -> Option<Owner> {
        Some((
            self.current_buffer().id(),
            self.window_manager.active_stable_window_id()?,
            self.current_buffer().revision(),
            self.felix.generation,
        ))
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
        let page_height = self.vheight();
        let indent = self.felix.indent.entry(id.0).or_insert_with(|| {
            felix_core::detect_indent(text)
                .unwrap_or_else(|| indentation.whitespace_for_columns(indentation.shift_width))
        });
        let context = felix_core::Context {
            search: self.felix.search.last().map(String::as_str),
            register: &self.felix.unnamed,
            tab_width: indentation.tab_width,
            indent,
            page_height,
        };
        let plan = match input {
            Input::Key(key) => self.felix.session.plan(text, &initial, key, &context),
            Input::Bracketed(value) => self.felix.session.paste(
                text,
                &initial,
                &[value.to_owned()],
                if self.felix.session.mode == felix_core::Mode::Normal {
                    felix_core::Paste::Before
                } else {
                    felix_core::Paste::Cursor
                },
            ),
            Input::Clipboard(placement) => {
                let Some(value) = self.clipboard.read_text().await? else {
                    return Ok(());
                };
                self.felix
                    .session
                    .paste(text, &initial, &[value], placement)
            }
        };
        if !plan.edits.is_empty() && self.current_buffer().directory.is_some() {
            self.set_legacy_message(Some("Directory buffers are read-only".into()));
            return self.render(frame);
        }
        if let Input::Key(key) = input {
            if self.felix.session.mode != felix_core::Mode::Insert
                && plan.next.mode == felix_core::Mode::Insert
            {
                self.felix.last_insert.clear();
                self.felix.last_insert.push(key.into());
            } else if self.felix.session.mode == felix_core::Mode::Insert {
                self.felix.last_insert.push(key.into());
            }
        }
        if matches!(plan.history, History::Begin | History::Atomic) {
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
        }
        for edit in plan.edits.iter().rev() {
            let buffer = self.current_buffer();
            let range = TextRange::new(
                buffer.char_idx_to_position(edit.start),
                buffer.char_idx_to_position(edit.end),
            );
            self.replace_range(range, &edit.insert);
        }
        let after = self.current_buffer().contents_snapshot();
        let selections = plan.selections(after.slice(..));
        self.felix.selections.insert(id, selections.clone());
        self.felix.generation = self.felix.generation.wrapping_add(1);
        self.mode = match plan.next.mode {
            felix_core::Mode::Insert => Mode::Insert,
            _ => Mode::Normal,
        };
        self.felix.session = plan.next;
        self.place_felix_cursor(&selections);
        if matches!(
            plan.history,
            History::Atomic | History::Commit | History::Checkpoint
        ) {
            let before = self.felix.before.take();
            if self.commit_transaction(self.cursor_snapshot()) {
                if let (Some(before), Some(tx)) = (
                    before,
                    self.current_buffer().undo_history.latest_transaction(),
                ) {
                    self.felix
                        .history
                        .insert((id.0, tx.id.clone()), (before, selections.clone()));
                }
            }
        }
        if plan.history == History::Checkpoint {
            self.begin_transaction("Helix edit");
        }
        if let Some(request) = plan.host {
            match request {
                HostAction::Undo | HostAction::Redo => {
                    let undo = request == HostAction::Undo;
                    let mut tx = self
                        .current_buffer()
                        .undo_history
                        .latest_transaction()
                        .map(|t| t.id.clone());
                    if undo {
                        self.undo_transaction(frame, runtime).await?;
                    } else {
                        self.redo_transaction(frame, runtime).await?;
                        tx = self
                            .current_buffer()
                            .undo_history
                            .latest_transaction()
                            .map(|t| t.id.clone());
                    }
                    if let Some((before, after)) =
                        tx.and_then(|key| self.felix.history.get(&(id.0, key)))
                    {
                        let state = if undo { before } else { after }.clone();
                        self.place_felix_cursor(&state);
                        self.felix.selections.insert(id, state);
                    }
                }
                HostAction::NextBuffer | HostAction::PreviousBuffer => {
                    let len = self.buffer_manager.len();
                    let current = self.buffer_manager.active_index();
                    let delta = if request == HostAction::NextBuffer {
                        1
                    } else {
                        len - 1
                    };
                    self.set_current_buffer(frame, (current + delta) % len)
                        .await?;
                    self.felix.session = Session::default();
                    self.mode = Mode::Normal;
                }
                HostAction::RepeatInsert(count) => {
                    let recipe = self.felix.last_insert.clone();
                    if !recipe.is_empty() {
                        for _ in 0..count {
                            for key in &recipe {
                                let owner =
                                    self.felix_owner().context("missing repeat destination")?;
                                Box::pin(self.execute_felix(
                                    Input::Key(key),
                                    owner,
                                    frame,
                                    runtime,
                                ))
                                .await?;
                            }
                        }
                    }
                }
                HostAction::SyntaxParentEnd => {
                    if let Some(lang) = self
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
        self.set_legacy_message(plan.message);
        if !plan.edits.is_empty() {
            self.notify_change(runtime).await?;
        }
        self.render(frame)
    }
}

impl Editor {
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
    ) {
        let buffer = &self.buffer_manager[window.buffer_index];
        let Some(sel) = self.felix.selections.get(&(buffer.id(), window.id)) else {
            return;
        };
        let rope = buffer.contents_snapshot();
        let text = rope.slice(..);
        let tab_width = self.tab_width_for_buffer_index(window.buffer_index);
        for range in sel.ranges() {
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
                let col = |idx| display_width_with_tabs(char_prefix(line, idx), tab_width);
                let left = if row == start.line {
                    col(start.character)
                } else {
                    0
                };
                let right = if row == end.line {
                    col(end.character)
                } else {
                    display_width_with_tabs(line, tab_width) + 1
                };
                if left < right {
                    let points = self.display_col_range_points_in_window(window, row, left, right);
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
                    for pt in self.display_col_range_points_in_window(window, row, x, x + width) {
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
        let mut config =
            Config::from_toml_with_overrides(crate::assets::DEFAULT_CONFIG, &[]).unwrap();
        config.editing_model = crate::config::EditingModel::Helix;
        config.lsp.enabled = false;
        let lsp = Box::new(crate::LspManager::new(config.lsp.clone()));
        let buffers = vec![
            Buffer::new(Some("example.txt".into()), text.into()),
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
        replay("change-two-matches", "foo foo\n", &fixture).await;
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
            )
            .await;
        }
    }

    async fn replay(name: &str, text: &str, fixture: &[Value]) {
        assert!(!fixture.is_empty());
        let (mut editor, mut frame, mut runtime) = fixture_editor(text);
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
                    KeyCode::Char(token.chars().nth(3).unwrap())
                }
                "<esc>" => KeyCode::Esc,
                "<enter>" => KeyCode::Enter,
                _ => KeyCode::Char(token.chars().next().unwrap()),
            };
            let event = Event::Key(KeyEvent::new(code, modifiers));
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
