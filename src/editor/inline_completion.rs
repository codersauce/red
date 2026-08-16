//! Editor-owned scheduling, preview validation, and acceptance for inline AI.

use super::*;
use crate::{
    agent_tools::EditorPosition,
    copilot::{
        Bridge, CompletionItem, CompletionRequest, Control, Event as CopilotEvent, Snapshot,
    },
};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct InlineCompletionState {
    bridge: Option<Bridge>,
    enabled_override: Option<bool>,
    generation: u64,
    scheduled: Option<(Instant, Snapshot)>,
    requested: Option<Snapshot>,
    pub(super) suggestion: Option<Suggestion>,
    status: String,
    failed: bool,
    prompts: VecDeque<CopilotEvent>,
}

pub(super) struct Suggestion {
    pub snapshot: Snapshot,
    pub insertion: String,
    item: CompletionItem,
    shown: bool,
}

/// Reuses the normal picker while ensuring dismissal answers the server request.
struct CopilotMessagePicker {
    picker: Picker,
    id: Value,
}

impl CopilotMessagePicker {
    fn new(editor: &Editor, id: Value, message: String, actions: Vec<Value>) -> Self {
        let mut choices = HashMap::new();
        let mut labels = Vec::new();
        for (index, action) in actions.into_iter().enumerate() {
            let title = action["title"].as_str().unwrap_or("Continue");
            let title = title
                .chars()
                .filter(|ch| !ch.is_control())
                .take(120)
                .collect::<String>();
            let label = format!("{}. {title}", index + 1);
            labels.push(label.clone());
            choices.insert(label, action);
        }
        labels.push("Dismiss".into());
        let response_id = id.clone();
        let picker = Picker::builder()
            .title("GitHub Copilot")
            .items(labels)
            .status(message)
            .content_sized(80, 12)
            .select_action(move |label| Action::CopilotRespond {
                id: response_id.clone(),
                result: choices.get(&label).cloned().unwrap_or(Value::Null),
            })
            .build(editor);
        Self { picker, id }
    }
}

impl Component for CopilotMessagePicker {
    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.picker.draw(buffer)
    }
    fn resize(&mut self, width: usize, height: usize) -> bool {
        self.picker.resize(width, height)
    }
    fn set_theme(&mut self, theme: &Theme) {
        self.picker.set_theme(theme);
    }
    fn cursor_position(&self) -> Option<(usize, usize)> {
        self.picker.cursor_position()
    }
    fn cursor_mode(&self) -> Option<Mode> {
        self.picker.cursor_mode()
    }
    fn handle_event(&mut self, event: &Event) -> Option<KeyAction> {
        match self.picker.handle_event(event) {
            Some(KeyAction::Single(Action::CloseDialog)) => Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::CopilotRespond {
                    id: self.id.clone(),
                    result: Value::Null,
                },
            ])),
            action => action,
        }
    }
}

impl Editor {
    pub(super) fn handle_inline_completion_event(&mut self, event: &Event) -> Option<KeyAction> {
        let mapping = Self::key_string_for_event(event)
            .and_then(|key| self.config.keys.insert.get(&key))
            .cloned();
        if self.inline_editor_focused()
            && self.copilot_enabled()
            && mapping == Some(KeyAction::Single(Action::RequestInlineCompletion))
            && self
                .current_dialog
                .as_ref()
                .is_none_or(|dialog| dialog.allows_event_passthrough())
        {
            return Some(KeyAction::Single(Action::RequestInlineCompletion));
        }
        self.visible_inline_suggestion()?;
        if matches!(
            mapping,
            Some(KeyAction::Single(
                Action::AcceptInlineCompletion | Action::DismissInlineCompletion
            ))
        ) {
            return mapping;
        }
        match event {
            Event::Key(KeyEvent {
                code: KeyCode::Tab,
                modifiers: KeyModifiers::NONE,
                ..
            }) if mapping == Some(KeyAction::Single(Action::InsertTab)) => {
                Some(KeyAction::Single(Action::AcceptInlineCompletion))
            }
            Event::Key(KeyEvent {
                code: KeyCode::Esc, ..
            }) if mapping == Some(KeyAction::Single(Action::EnterMode(Mode::Normal))) => {
                Some(KeyAction::Multiple(vec![
                    Action::DismissInlineCompletion,
                    Action::EnterMode(Mode::Normal),
                ]))
            }
            Event::Key(_)
            | Event::Paste(_)
            | Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(_),
                ..
            }) => {
                self.dismiss_inline_completion();
                None
            }
            _ => None,
        }
    }
    pub(super) fn copilot_enabled(&self) -> bool {
        !self.config.disable_ai
            && self
                .inline_completion
                .enabled_override
                .unwrap_or(self.config.copilot.enabled)
    }

    pub(super) fn prefers_inline_completion(&self) -> bool {
        self.copilot_enabled() && !self.inline_completion.failed && self.copilot_file_allowed()
    }

    fn copilot_file_allowed(&self) -> bool {
        self.current_buffer()
            .uri()
            .ok()
            .flatten()
            .and_then(|uri| lsp_normalized_file_path(&uri).ok())
            .is_some_and(|path| {
                self.config.copilot.allows(
                    &get_workspace_path(),
                    Path::new(&path),
                    self.current_buffer().byte_len(),
                )
            })
    }

    fn inline_snapshot(&self) -> Option<Snapshot> {
        Some(Snapshot {
            generation: self.inline_completion.generation,
            buffer_id: self.current_buffer().id(),
            revision: self.current_buffer().revision(),
            cursor: self.cursor_text_position(),
            uri: self.current_buffer().uri().ok()??,
        })
    }

    fn inline_snapshot_current(&self, snapshot: &Snapshot) -> bool {
        self.is_insert()
            && self.copilot_enabled()
            && self.inline_snapshot().as_ref() == Some(snapshot)
    }

    fn inline_editor_focused(&self) -> bool {
        self.is_insert()
            && !self.workspace_manager.is_active()
            && !self.panel_manager.has_focused_panel()
    }

    pub(super) fn visible_inline_suggestion(&self) -> Option<&Suggestion> {
        self.inline_completion
            .suggestion
            .as_ref()
            .filter(|suggestion| {
                self.inline_editor_focused()
                    && self.current_dialog.is_none()
                    && self.inline_snapshot_current(&suggestion.snapshot)
            })
    }

    fn ensure_copilot(&mut self) -> bool {
        if self.config.disable_ai {
            self.last_error = Some("Copilot is disabled by disable_ai = true".into());
            return false;
        }
        if !self.copilot_enabled() {
            self.last_error = Some(
                "Copilot is disabled; use :Copilot enable to allow source-code transmission".into(),
            );
            return false;
        }
        if self.inline_completion.failed {
            return false;
        }
        if self.inline_completion.bridge.is_none() {
            self.inline_completion.bridge = Some(Bridge::start(
                self.config.copilot.clone(),
                get_workspace_path(),
            ));
            self.inline_completion.status = "Starting".into();
        }
        true
    }

    pub(super) fn copilot_control(&mut self, control: Control) {
        if !self.ensure_copilot() {
            return;
        }
        if let Some(Err(error)) = self
            .inline_completion
            .bridge
            .as_ref()
            .map(|bridge| bridge.control(control))
        {
            self.last_error = Some(error.to_string());
        }
    }

    pub(super) fn handle_copilot_command(&mut self, command: &str) {
        match command {
            "enable" => {
                if self.config.disable_ai {
                    self.last_error = Some("Copilot is disabled by disable_ai = true".into());
                    return;
                }
                self.inline_completion.enabled_override = Some(true);
                self.inline_completion.failed = false;
                self.ensure_copilot();
                self.last_error = Some(
                    "Copilot enabled for this session; eligible source files may be sent to GitHub"
                        .into(),
                );
            }
            "disable" => {
                self.dismiss_inline_completion();
                self.inline_completion.enabled_override = Some(false);
                self.inline_completion.bridge = None;
                self.inline_completion.prompts.clear();
                self.inline_completion.status = "Disabled".into();
                self.last_error = Some("Copilot disabled".into());
            }
            "signin" | "restart" => {
                self.inline_completion.failed = false;
                if command == "restart" {
                    self.inline_completion.bridge = None;
                }
                if self.ensure_copilot() && command == "signin" {
                    self.copilot_control(Control::SignIn);
                }
            }
            "signout" => {
                self.dismiss_inline_completion();
                self.copilot_control(Control::SignOut);
            }
            "" | "status" => {
                self.last_error = Some(format!(
                    "Copilot: {}",
                    if !self.copilot_enabled() {
                        "Disabled"
                    } else if self.inline_completion.status.is_empty() {
                        "Not started"
                    } else {
                        &self.inline_completion.status
                    }
                ))
            }
            _ => {
                self.last_error = Some(
                    "Usage: Copilot enable|disable|signin|signout|status|restart|complete".into(),
                )
            }
        }
    }

    pub(super) fn dismiss_inline_completion(&mut self) {
        self.inline_completion.generation = self.inline_completion.generation.wrapping_add(1);
        self.inline_completion.scheduled = None;
        self.inline_completion.requested = None;
        if self.inline_completion.suggestion.take().is_some() {
            self.layout_cache.borrow_mut().clear();
            self.force_full_redraw = true;
        }
        if let Some(bridge) = &self.inline_completion.bridge {
            bridge.cancel();
        }
    }

    pub(super) fn schedule_inline_completion(&mut self) {
        self.dismiss_inline_completion();
        if self.copilot_enabled() && self.inline_editor_focused() {
            if let Some(snapshot) = self.inline_snapshot() {
                self.inline_completion.scheduled = Some((
                    Instant::now() + Duration::from_millis(self.config.copilot.debounce_ms),
                    snapshot,
                ));
            }
        }
    }

    pub(super) fn request_inline_completion(&mut self, automatic: bool) {
        if !self.inline_editor_focused() {
            return;
        }
        if !automatic {
            self.dismiss_inline_completion();
            self.scheduled_completion = None;
            for pending in self.pending_completions.values_mut() {
                pending.superseded = true;
            }
            if self
                .current_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.allows_event_passthrough())
            {
                self.current_dialog = None;
                self.completion_snapshot = None;
            }
        }
        let Some(snapshot) = self.inline_snapshot() else {
            return;
        };
        if !self.copilot_file_allowed() {
            if !automatic {
                self.last_error =
                    Some("Copilot: file excluded, outside workspace, or too large".into());
            }
            return;
        }
        let request = CompletionRequest {
            snapshot: snapshot.clone(),
            language_id: self
                .current_language_id()
                .unwrap_or_else(|| "plaintext".into()),
            contents: self.current_buffer().contents(),
            position: self.cursor_lsp_position(),
            tab_size: self.indentation().shift_width,
            insert_spaces: self.indentation().expand_tab,
            automatic,
        };
        if !self.ensure_copilot() {
            return;
        }
        self.inline_completion.requested = Some(snapshot);
        self.inline_completion
            .bridge
            .as_ref()
            .expect("Copilot started")
            .request(request);
    }

    pub(super) fn service_inline_completion(&mut self) -> bool {
        let mut changed = false;
        if !self.copilot_enabled() && self.inline_completion.bridge.is_some() {
            self.dismiss_inline_completion();
            self.inline_completion.bridge = None;
            self.inline_completion.prompts.clear();
            changed = true;
        }
        if self
            .inline_completion
            .requested
            .as_ref()
            .is_some_and(|snapshot| !self.inline_snapshot_current(snapshot))
            || self
                .inline_completion
                .suggestion
                .as_ref()
                .is_some_and(|suggestion| !self.inline_snapshot_current(&suggestion.snapshot))
        {
            self.dismiss_inline_completion();
            changed = true;
        }
        if let Some((deadline, snapshot)) = self.inline_completion.scheduled.clone() {
            if Instant::now() >= deadline {
                self.inline_completion.scheduled = None;
                if self.inline_snapshot_current(&snapshot) {
                    self.request_inline_completion(true);
                }
            }
        }
        for _ in 0..32 {
            let Some(event) = self
                .inline_completion
                .bridge
                .as_mut()
                .and_then(Bridge::poll)
            else {
                break;
            };
            match event {
                CopilotEvent::Status(status) => self.inline_completion.status = status,
                CopilotEvent::Stopped(status) => {
                    self.inline_completion.status = status.clone();
                    self.inline_completion.failed = true;
                    self.inline_completion.bridge = None;
                    self.dismiss_inline_completion();
                    self.last_error = Some(status);
                    changed = true;
                    break;
                }
                CopilotEvent::Completion { snapshot, items } => {
                    if self.inline_completion.requested.as_ref() != Some(&snapshot)
                        || !self.inline_snapshot_current(&snapshot)
                    {
                        continue;
                    }
                    self.inline_completion.requested = None;
                    let contents = self.current_buffer().contents();
                    let position = self.cursor_lsp_position();
                    if let Some((item, insertion)) = items.into_iter().find_map(|item| {
                        insertion_for_item(&contents, position, &item)
                            .map(|insertion| (item, insertion))
                    }) {
                        self.inline_completion.suggestion = Some(Suggestion {
                            snapshot,
                            insertion,
                            item,
                            shown: false,
                        });
                        self.layout_cache.borrow_mut().clear();
                        self.force_full_redraw = true;
                        changed = true;
                    }
                }
                prompt @ (CopilotEvent::SignIn { .. } | CopilotEvent::Message { .. }) => {
                    if self.inline_completion.prompts.len() < 32 {
                        self.inline_completion.prompts.push_back(prompt);
                    } else if let CopilotEvent::Message { id, .. } = prompt {
                        self.copilot_control(Control::Respond {
                            id,
                            result: Value::Null,
                        });
                    }
                }
            }
        }
        if self.current_dialog.is_none() {
            if let Some(prompt) = self.inline_completion.prompts.pop_front() {
                self.current_dialog = Some(match prompt {
                    CopilotEvent::SignIn { user_code, command } => {
                        Box::new(Confirmation::new_actions(
                            self,
                            "Sign in to GitHub Copilot",
                            format!("Enter code {user_code} in GitHub’s device activation page."),
                            "Open browser",
                            "Dismiss",
                            Action::CopilotFinishSignIn(command),
                            Action::Refresh,
                        ))
                    }
                    CopilotEvent::Message {
                        id,
                        message,
                        actions,
                    } => Box::new(CopilotMessagePicker::new(self, id, message, actions)),
                    _ => unreachable!("only prompts are queued"),
                });
                changed = true;
            }
        }
        if self
            .visible_inline_suggestion()
            .is_some_and(|suggestion| !suggestion.shown)
        {
            if let Some(suggestion) = self.inline_completion.suggestion.as_mut() {
                suggestion.shown = true;
                let item = suggestion.item.clone();
                self.copilot_control(Control::Shown(item));
            }
        }
        changed
    }

    pub(super) async fn accept_inline_completion(
        &mut self,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let Some(suggestion) = self.inline_completion.suggestion.take() else {
            return Ok(());
        };
        if !self.inline_snapshot_current(&suggestion.snapshot) {
            self.dismiss_inline_completion();
            return Ok(());
        }
        let resume_insert = self.transaction_active();
        if resume_insert {
            self.commit_transaction(self.cursor_snapshot());
        }
        self.begin_transaction("accept Copilot completion");
        let start = suggestion.snapshot.cursor;
        let end = self
            .current_buffer()
            .range_for_text(start, &suggestion.insertion)
            .end;
        self.replace_range(TextRange::insertion(start), &suggestion.insertion);
        self.move_to_insert_text_position(end);
        self.commit_transaction(self.cursor_snapshot());
        self.notify_change(runtime).await?;
        if resume_insert && self.is_insert() {
            self.begin_transaction("insert");
        }
        self.copilot_control(Control::Accepted(suggestion.item));
        Ok(())
    }
}

fn insertion_for_item(
    contents: &str,
    cursor: LspPosition,
    item: &CompletionItem,
) -> Option<String> {
    let offset = |position: LspPosition| {
        utf16_byte_offset(
            contents,
            EditorPosition {
                line: position.line,
                character: position.character,
            },
        )
        .ok()
    };
    let cursor = offset(cursor)?;
    let (start, end) = item
        .range
        .as_ref()
        .map(|range| Some((offset(range.start)?, offset(range.end)?)))
        .unwrap_or(Some((cursor, cursor)))?;
    if start > cursor || cursor > end {
        return None;
    }
    let before = contents.get(start..cursor)?;
    let after = contents.get(cursor..end)?;
    let inserted = item.insert_text.strip_prefix(before)?.strip_suffix(after)?;
    if inserted.is_empty()
        || inserted
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
    {
        return None;
    }
    let inserted = inserted.replace("\r\n", "\n");
    if inserted.contains('\r') {
        return None;
    }
    Some(if contents.contains("\r\n") {
        inserted.replace('\n', "\r\n")
    } else {
        inserted
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::EditorTestExt;

    fn editor(text: &str) -> Editor {
        let mut config = Config::from_user_toml_with_overrides("", &[]).unwrap();
        config.lsp.enabled = false;
        config.copilot.enabled = true;
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let file = get_workspace_path()
            .join("src/main.rs")
            .to_string_lossy()
            .into_owned();
        let mut editor = Editor::with_size(
            lsp,
            60,
            16,
            config,
            Theme::default(),
            vec![Buffer::new(Some(file), text.into())],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        editor.inline_completion.failed = true;
        editor
    }

    async fn show(editor: &mut Editor, text: &str, cursor: usize) {
        editor
            .test_execute_action(Action::EnterMode(Mode::Insert))
            .await
            .unwrap();
        editor
            .test_execute_action(Action::SetCursor(cursor, 0))
            .await
            .unwrap();
        let snapshot = editor.inline_snapshot().unwrap();
        editor.inline_completion.suggestion = Some(Suggestion {
            snapshot,
            insertion: text.into(),
            item: CompletionItem {
                insert_text: text.into(),
                range: None,
                command: None,
                extra: Default::default(),
            },
            shown: true,
        });
        editor.layout_cache.borrow_mut().clear();
    }

    fn rendered_rows(editor: &mut Editor) -> Vec<String> {
        let mut output = RenderBuffer::new(60, 16, &editor.theme.style);
        editor.render(&mut output).unwrap();
        output
            .cells
            .chunks(output.width)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
            })
            .collect()
    }
    fn item(text: &str, start: usize, end: usize) -> CompletionItem {
        CompletionItem {
            insert_text: text.into(),
            range: Some(Range {
                start: LspPosition {
                    line: 0,
                    character: start,
                },
                end: LspPosition {
                    line: 0,
                    character: end,
                },
            }),
            command: None,
            extra: Default::default(),
        }
    }
    #[test]
    fn only_insertion_equivalent_edits_are_accepted() {
        let cursor = LspPosition {
            line: 0,
            character: 3,
        };
        assert_eq!(
            insertion_for_item("foo()", cursor, &item("foobar()", 0, 5)),
            Some("bar".into())
        );
        assert_eq!(
            insertion_for_item("foo()", cursor, &item("other()", 0, 5)),
            None
        );
        assert_eq!(
            insertion_for_item("foo()", cursor, &item("foo\u{1b}[31m()", 0, 5)),
            None
        );
    }
    #[test]
    fn utf16_boundaries_and_crlf_are_preserved() {
        let cursor = LspPosition {
            line: 0,
            character: 2,
        };
        assert_eq!(
            insertion_for_item("😀\r\n", cursor, &item("😀x\ny", 0, 2)),
            Some("x\r\ny".into())
        );
        assert_eq!(insertion_for_item("😀", cursor, &item("x", 1, 2)), None);
    }

    #[test]
    fn account_message_picker_preserves_all_actions_and_answers_dismissal() {
        let editor = editor("foo");
        let id = json!("request-id");
        let second = json!({"title":"Second","extra":"preserved"});
        let mut picker = CopilotMessagePicker::new(
            &editor,
            id.clone(),
            "Account issue".into(),
            vec![json!({"title":"First"}), second.clone()],
        );
        picker.handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Down,
            KeyModifiers::NONE,
        )));
        let Some(KeyAction::Multiple(actions)) = picker.handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))) else {
            panic!("expected selection");
        };
        assert!(actions.contains(&Action::CopilotRespond {
            id: id.clone(),
            result: second
        }));
        assert_eq!(
            picker.handle_event(&Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::CopilotRespond {
                    id,
                    result: Value::Null
                }
            ]))
        );
    }

    #[tokio::test]
    async fn multiline_preview_preserves_source_and_accepts_as_one_undo_step() {
        let mut editor = editor("foo()\nafter\n");
        show(&mut editor, "bar\nnext", 3).await;
        let before = editor.current_buffer().revision();
        let rows = rendered_rows(&mut editor);
        let first = rows.iter().position(|row| row.contains("foobar")).unwrap();
        assert!(rows[first + 1].contains("next()"), "{rows:?}");
        assert!(rows[first + 2].contains("after"), "{rows:?}");
        assert_eq!(editor.current_buffer().contents(), "foo()\nafter\n");
        assert_eq!(editor.current_buffer().revision(), before);
        assert_eq!(
            editor
                .handle_event(&Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)))
                .unwrap(),
            Some(KeyAction::Single(Action::AcceptInlineCompletion))
        );
        editor
            .test_execute_action(Action::AcceptInlineCompletion)
            .await
            .unwrap();
        assert_eq!(
            editor.current_buffer().contents(),
            "foobar\nnext()\nafter\n"
        );
        editor
            .test_execute_action(Action::EnterMode(Mode::Normal))
            .await
            .unwrap();
        editor.test_execute_action(Action::Undo).await.unwrap();
        assert_eq!(editor.current_buffer().contents(), "foo()\nafter\n");
    }

    #[tokio::test]
    async fn stale_preview_cannot_mutate_and_escape_keeps_vim_behavior() {
        let mut editor = editor("foo");
        show(&mut editor, "bar", 3).await;
        assert_eq!(
            editor
                .handle_event(&Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
                .unwrap(),
            Some(KeyAction::Multiple(vec![
                Action::DismissInlineCompletion,
                Action::EnterMode(Mode::Normal)
            ]))
        );
        editor
            .current_buffer_mut()
            .replace_range_raw(TextRange::insertion(TextPosition::new(0, 0)), "x");
        editor
            .test_execute_action(Action::AcceptInlineCompletion)
            .await
            .unwrap();
        assert_eq!(editor.current_buffer().contents(), "xfoo");
    }

    #[tokio::test]
    async fn late_responses_are_discarded_and_global_disable_wins() {
        let mut editor = editor("foo");
        let (bridge, requests, _controls, events) = Bridge::test_channels();
        editor.inline_completion.bridge = Some(bridge);
        editor.inline_completion.failed = false;
        editor
            .test_execute_action(Action::EnterMode(Mode::Insert))
            .await
            .unwrap();
        editor
            .test_execute_action(Action::SetCursor(3, 0))
            .await
            .unwrap();
        editor.request_inline_completion(false);
        let snapshot = requests.borrow().as_ref().unwrap().snapshot.clone();
        editor.dismiss_inline_completion();
        events
            .send(CopilotEvent::Completion {
                snapshot,
                items: vec![item("foobar", 0, 3)],
            })
            .await
            .unwrap();
        editor.service_inline_completion();
        assert!(editor.inline_completion.suggestion.is_none());
        editor.config.disable_ai = true;
        editor.handle_copilot_command("enable");
        editor.service_inline_completion();
        assert!(!editor.copilot_enabled());
        assert!(editor.inline_completion.bridge.is_none());
    }

    #[tokio::test]
    async fn palette_request_enters_insert_mode() {
        let mut editor = editor("foo");
        let (bridge, requests, _controls, _events) = Bridge::test_channels();
        editor.inline_completion.bridge = Some(bridge);
        editor.inline_completion.failed = false;
        let action = command_palette::entries(&editor.config.keys, &[])
            .into_iter()
            .find(|entry| entry.id == "ai.inline_completion")
            .unwrap()
            .action;
        editor.test_execute_action(action).await.unwrap();
        assert!(editor.is_insert());
        assert!(requests.borrow().is_some());
    }

    #[tokio::test]
    async fn inline_completion_respects_remapped_insert_keys() {
        let mut editor = editor("foo");
        show(&mut editor, "bar", 3).await;
        editor
            .config
            .keys
            .insert
            .insert("Tab".into(), KeyAction::Single(Action::MoveRight));
        assert_eq!(
            editor.handle_inline_completion_event(&Event::Key(KeyEvent::new(
                KeyCode::Tab,
                KeyModifiers::NONE,
            ))),
            None,
        );
        assert!(editor.inline_completion.suggestion.is_none());
        show(&mut editor, "bar", 3).await;
        editor.config.keys.insert.insert(
            "Ctrl-y".into(),
            KeyAction::Single(Action::AcceptInlineCompletion),
        );
        assert_eq!(
            editor.handle_inline_completion_event(&Event::Key(KeyEvent::new(
                KeyCode::Char('y'),
                KeyModifiers::CONTROL,
            ))),
            Some(KeyAction::Single(Action::AcceptInlineCompletion)),
        );
    }

    #[tokio::test]
    async fn explicit_inline_request_supersedes_old_popup_responses_only() {
        let mut editor = editor("foo foobar");
        let (bridge, _requests, _controls, _events) = Bridge::test_channels();
        editor.inline_completion.bridge = Some(bridge);
        editor.inline_completion.failed = false;
        editor
            .test_execute_action(Action::EnterMode(Mode::Insert))
            .await
            .unwrap();
        editor
            .test_execute_action(Action::SetCursor(3, 0))
            .await
            .unwrap();
        let snapshot = editor.completion_snapshot();
        editor.pending_completions.insert(
            41,
            PendingCompletion {
                buffer_items: Vec::new(),
                snapshot,
                displayed_immediately: false,
                superseded: false,
            },
        );
        editor.request_inline_completion(false);
        let response = InboundMessage::Message(ResponseMessage {
            id: 41,
            result: json!([{"label":"foobar"}]),
            request: None,
        });
        assert!(editor
            .handle_lsp_message(&response, Some("textDocument/completion".into()))
            .is_none());
        assert!(editor.current_dialog.is_none());
        assert!(editor.inline_completion.requested.is_some());
        editor
            .test_execute_action(Action::RequestCompletion)
            .await
            .unwrap();
        assert!(editor.current_dialog.is_some());
        assert!(editor.inline_completion.requested.is_none());
    }

    #[tokio::test]
    async fn ordinary_completion_remains_explicitly_available() {
        let mut editor = editor("foo foobar");
        editor.inline_completion.failed = false;
        editor
            .test_execute_action(Action::EnterMode(Mode::Insert))
            .await
            .unwrap();
        editor
            .test_execute_action(Action::SetCursor(3, 0))
            .await
            .unwrap();
        editor.schedule_automatic_completion();
        assert!(editor.scheduled_completion.is_none());
        editor
            .test_execute_action(Action::RequestCompletion)
            .await
            .unwrap();
        assert!(editor.current_dialog.is_some());
    }
}
