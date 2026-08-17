//! Signature-help lifecycle. The popup never owns editor input or `current_dialog`.

use super::*;
use crate::lsp::{SignatureHelp, SignatureHelpContext};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Snapshot {
    buffer: BufferId,
    revision: u64,
    cursor: TextPosition,
    mode: Mode,
    window: Option<crate::window::WindowId>,
}

struct Scheduled {
    deadline: Instant,
    snapshot: Snapshot,
    context: SignatureHelpContext,
}

struct Pending {
    id: i64,
    snapshot: Snapshot,
}

#[derive(Default)]
pub(super) struct SignatureHelpState {
    scheduled: Option<Scheduled>,
    pending: Option<Pending>,
    visible: Option<SignatureHelp>,
}

impl SignatureHelpState {
    pub(super) fn is_visible(&self) -> bool {
        self.visible.is_some()
    }
    fn is_active(&self) -> bool {
        self.visible.is_some() || self.pending.is_some() || self.scheduled.is_some()
    }
    fn clear(&mut self) -> bool {
        let visible = self.visible.take().is_some();
        self.pending = None;
        self.scheduled = None;
        visible
    }
}

impl Editor {
    pub(super) fn signature_snapshot(&self) -> Snapshot {
        Snapshot {
            buffer: self.current_buffer().id(),
            revision: self.current_buffer().revision(),
            cursor: self.cursor_text_position(),
            mode: self.mode,
            window: self.window_manager.active_stable_window_id(),
        }
    }

    fn signature_context_available(&self) -> bool {
        !self.has_term()
            && !self.panel_manager.has_focused_panel()
            && !self.workspace_manager.is_active()
            && self
                .current_dialog
                .as_ref()
                .is_none_or(|dialog| dialog.allows_event_passthrough())
    }

    fn signature_context(&self, trigger: Option<char>, manual: bool) -> SignatureHelpContext {
        SignatureHelpContext {
            trigger_kind: if manual {
                1
            } else if trigger.is_some() {
                2
            } else {
                3
            },
            trigger_character: trigger.map(|ch| ch.to_string()),
            is_retrigger: self.signature_help.is_visible(),
            active_signature_help: self.signature_help.visible.clone(),
        }
    }

    /// Called after the canonical edit and LSP didChange notification have completed.
    pub(super) fn observe_signature_help_action(
        &mut self,
        action: &Action,
        before: Snapshot,
    ) -> bool {
        let after = self.signature_snapshot();
        if !self.signature_context_available() {
            return self.signature_help.clear();
        }
        if before == after {
            return false;
        }
        if !self.is_insert() || before.buffer != after.buffer || before.window != after.window {
            return self.signature_help.clear();
        }
        let Some(options) = self
            .current_buffer()
            .file
            .as_deref()
            .and_then(|file| self.lsp.server_capabilities_for_file(file))
            .and_then(|capabilities| capabilities.signature_help_provider.as_ref())
        else {
            return self.signature_help.clear();
        };
        let active = self.signature_help.is_active();
        let typed = match action {
            Action::InsertCharAtCursorPos(ch) => Some(*ch),
            _ => None,
        };
        let trigger = typed.filter(|ch| {
            let text = ch.to_string();
            options
                .trigger_characters
                .as_ref()
                .is_some_and(|chars| chars.contains(&text))
                || active
                    && options
                        .retrigger_characters
                        .as_ref()
                        .is_some_and(|chars| chars.contains(&text))
        });
        let completion = matches!(action, Action::ApplyCompletion { .. });
        if !(active || self.config.signature_help.auto_trigger && (trigger.is_some() || completion))
        {
            return false;
        }
        let context = self.signature_context(trigger, completion && !active && trigger.is_none());
        self.signature_help.pending = None;
        self.signature_help.scheduled = Some(Scheduled {
            deadline: Instant::now()
                + Duration::from_millis(self.config.signature_help.debounce_ms.min(5_000)),
            snapshot: after,
            context,
        });
        // A closing delimiter may return to an outer call. Hide the inner signature
        // immediately, then let the server identify the enclosing callable.
        if typed.is_some_and(|ch| matches!(ch, ')' | ']' | '}')) {
            return self.signature_help.visible.take().is_some();
        }
        false
    }

    pub(super) async fn invoke_signature_help(&mut self) -> anyhow::Result<bool> {
        if !self.signature_context_available() {
            return Ok(false);
        }
        if let Some(help) = self
            .signature_help
            .visible
            .as_mut()
            .filter(|help| help.signatures.len() > 1)
        {
            help.active_signature =
                Some((help.active_signature.unwrap_or(0) + 1) % help.signatures.len());
            return Ok(true);
        }
        let context = self.signature_context(None, true);
        self.signature_help.scheduled = None;
        self.send_signature_help(context).await
    }

    pub(super) async fn service_signature_help(&mut self) -> anyhow::Result<bool> {
        if self
            .signature_help
            .scheduled
            .as_ref()
            .is_none_or(|request| Instant::now() < request.deadline)
        {
            return Ok(false);
        }
        let scheduled = self.signature_help.scheduled.take().unwrap();
        if scheduled.snapshot != self.signature_snapshot()
            || !self.signature_context_available()
            || !self.is_insert()
        {
            return Ok(self.signature_help.clear());
        }
        self.send_signature_help(scheduled.context).await
    }

    async fn send_signature_help(&mut self, context: SignatureHelpContext) -> anyhow::Result<bool> {
        let Some(file) = self.current_buffer().file.clone() else {
            return Ok(false);
        };
        self.ensure_current_buffer_lsp_opened().await?;
        if self
            .lsp
            .server_capabilities_for_file(&file)
            .is_some_and(|cap| cap.signature_help_provider.is_none())
        {
            return Ok(self.signature_help.clear());
        }
        let position = self.cursor_lsp_position();
        let snapshot = self.signature_snapshot();
        match self
            .lsp
            .signature_help_with_context(&file, position.character, position.line, Some(context))
            .await
        {
            Ok(id) if id > 0 => self.signature_help.pending = Some(Pending { id, snapshot }),
            Ok(_) => return Ok(self.signature_help.clear()),
            Err(error) => {
                log!("signature help unavailable: {error}");
                return Ok(self.signature_help.clear());
            }
        }
        Ok(false)
    }

    pub(super) fn signature_help_action(&mut self, response: &ResponseMessage) -> Option<Action> {
        if self
            .signature_help
            .pending
            .as_ref()
            .is_none_or(|pending| pending.id != response.id)
        {
            return None;
        }
        let pending = self.signature_help.pending.take().unwrap();
        if pending.snapshot != self.signature_snapshot() || !self.signature_context_available() {
            return None;
        }
        let help = serde_json::from_value::<SignatureHelp>(response.result.clone())
            .ok()
            .filter(|help| !help.signatures.is_empty());
        self.signature_help.visible = help;
        Some(Action::Refresh)
    }

    pub(super) fn signature_help_error(&mut self, id: i64) -> Option<Action> {
        if self
            .signature_help
            .pending
            .as_ref()
            .is_some_and(|pending| pending.id == id)
        {
            self.signature_help.clear().then_some(Action::Refresh)
        } else {
            None
        }
    }

    pub(super) fn render_signature_help(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        let Some(help) = &self.signature_help.visible else {
            return Ok(());
        };
        if !self.signature_context_available() {
            return Ok(());
        }
        let Some(window) = self.active_window_with_editor_view() else {
            return Ok(());
        };
        let Some(anchor) = self.render_cursor_position() else {
            return Ok(());
        };
        let viewport = ScreenRect {
            x: window.position.x,
            y: window.position.y + self.window_content_top(&window),
            width: window.inner_width(),
            height: self.window_content_height(&window),
        };
        let completion = self
            .current_dialog
            .as_ref()
            .and_then(|dialog| dialog.completion_popup_bounds())
            .map(|(x, y, width, height)| ScreenRect {
                x,
                y,
                width,
                height,
            });
        crate::ui::signature_help::render(
            buffer,
            &self.theme,
            help,
            OverlayLayout {
                viewport,
                anchor,
                avoid_rows: None,
                protected_rows: Some((anchor.1, anchor.1)),
            },
            completion,
            self.config.signature_help.show_documentation,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str) -> Editor {
        let mut config = Config::default();
        config.lsp.enabled = false;
        let mut editor = Editor::with_size(
            Box::new(crate::lsp::LspManager::new(config.lsp.clone())),
            60,
            12,
            config,
            Theme::default(),
            vec![Buffer::new(None, text.to_owned())],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        editor.mode = Mode::Insert;
        editor.cx = grapheme_len(text);
        editor
    }

    fn result(id: i64, label: &str) -> ResponseMessage {
        ResponseMessage {
            id,
            result: json!({"signatures":[{"label":label,"parameters":[{"label":"x: f32"}]}]}),
            request: None,
        }
    }

    fn pending(editor: &mut Editor, id: i64) {
        editor.signature_help.pending = Some(Pending {
            id,
            snapshot: editor.signature_snapshot(),
        });
    }

    #[test]
    fn response_is_non_modal_and_stale_ids_positions_and_modes_are_rejected() {
        let mut editor = editor("call(");
        pending(&mut editor, 1);
        assert!(editor.signature_help_action(&result(2, "wrong")).is_none());
        assert!(matches!(
            editor.signature_help_action(&result(1, "call(x: f32)")),
            Some(Action::Refresh)
        ));
        assert!(editor.signature_help.is_visible());
        assert!(editor.current_dialog.is_none());
        pending(&mut editor, 3);
        editor.cx -= 1;
        assert!(editor.signature_help_action(&result(3, "stale")).is_none());
        pending(&mut editor, 4);
        editor.mode = Mode::Normal;
        assert!(editor.signature_help_action(&result(4, "stale")).is_none());
        assert_eq!(
            editor.signature_help.visible.as_ref().unwrap().signatures[0].label,
            "call(x: f32)"
        );
    }

    #[test]
    fn null_response_and_matching_error_close_help_without_notifications() {
        let mut editor = editor("call(");
        pending(&mut editor, 1);
        editor.signature_help_action(&result(1, "call(x: f32)"));
        pending(&mut editor, 2);
        assert!(matches!(
            editor.signature_help_action(&ResponseMessage {
                id: 2,
                result: Value::Null,
                request: None
            }),
            Some(Action::Refresh)
        ));
        assert!(!editor.signature_help.is_visible());
        pending(&mut editor, 3);
        editor.signature_help_action(&result(3, "call(x: f32)"));
        pending(&mut editor, 4);
        assert!(editor.signature_help_error(3).is_none());
        assert!(matches!(
            editor.signature_help_error(4),
            Some(Action::Refresh)
        ));
        assert!(editor.last_error.is_none());
        assert!(!editor.signature_help.is_active());
    }

    #[tokio::test]
    async fn manual_invocation_cycles_overloads_without_stealing_input() {
        let mut editor = editor("call(");
        editor.signature_help.visible = Some(
            serde_json::from_value(
                json!({"signatures":[{"label":"first()"},{"label":"second()"}]}),
            )
            .unwrap(),
        );
        assert!(editor.invoke_signature_help().await.unwrap());
        assert_eq!(
            editor
                .signature_help
                .visible
                .as_ref()
                .unwrap()
                .active_signature,
            Some(1)
        );
        assert!(editor.invoke_signature_help().await.unwrap());
        assert_eq!(
            editor
                .signature_help
                .visible
                .as_ref()
                .unwrap()
                .active_signature,
            Some(0)
        );
        let action = editor
            .handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Char('a'),
                KeyModifiers::NONE,
            )))
            .unwrap();
        assert!(matches!(
            action,
            Some(KeyAction::Single(Action::InsertCharAtCursorPos('a')))
        ));
    }

    #[cfg(unix)]
    async fn pump_until(editor: &mut Editor, predicate: impl Fn(&Editor) -> bool) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !predicate(editor) {
                if let Some((message, method)) = editor.lsp.recv_response().await.unwrap() {
                    if let Some(action) = editor.handle_lsp_message(&message, method) {
                        editor.test_execute_production_action(action).await.unwrap();
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("signature-help server should respond");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn real_transport_tracks_arguments_nested_calls_and_document_changes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("example.sigtest");
        let script = root.path().join("server.py");
        let events = root.path().join("events.jsonl");
        fs::write(&path, "outer").unwrap();
        fs::write(
            &script,
            include_str!("../../tests/fixtures/signature_help_lsp.py"),
        )
        .unwrap();
        let file = path.to_string_lossy().into_owned();
        let mut config = Config::default();
        config.completion.auto_trigger = false;
        config.signature_help.debounce_ms = 0;
        config.lsp.servers = HashMap::from([("signature-test".to_owned(), serde_json::from_value(json!({
            "command":"python3", "args":[script, events], "language_id":"rust", "file_extensions":["sigtest"]
        })).unwrap())]);
        let mut editor = Editor::with_size(
            Box::new(crate::lsp::LspManager::new(config.lsp.clone())),
            60,
            12,
            config,
            Theme::default(),
            vec![Buffer::new(Some(file.clone()), "outer".to_owned())],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        editor.mode = Mode::Insert;
        editor.cx = 5;
        editor.ensure_current_buffer_lsp_opened().await.unwrap();
        pump_until(&mut editor, |editor| {
            editor.lsp.server_capabilities_for_file(&file).is_some()
        })
        .await;
        async fn type_and_request(editor: &mut Editor, text: &str) {
            for ch in text.chars() {
                editor
                    .test_execute_production_action(Action::InsertCharAtCursorPos(ch))
                    .await
                    .unwrap();
            }
            editor.service_signature_help().await.unwrap();
            pump_until(editor, |editor| editor.signature_help.pending.is_none()).await;
        }
        type_and_request(&mut editor, "(").await;
        assert_eq!(
            crate::ui::signature_help::active_parameter(
                editor.signature_help.visible.as_ref().unwrap()
            )
            .unwrap()
            .1,
            0
        );
        assert!(editor.current_dialog.is_none());
        type_and_request(&mut editor, "1, ").await;
        assert_eq!(
            crate::ui::signature_help::active_parameter(
                editor.signature_help.visible.as_ref().unwrap()
            )
            .unwrap()
            .1,
            1
        );
        type_and_request(&mut editor, "inner(").await;
        assert!(
            editor.signature_help.visible.as_ref().unwrap().signatures[0]
                .label
                .contains("inner(")
        );
        type_and_request(&mut editor, "2)").await;
        assert!(
            editor.signature_help.visible.as_ref().unwrap().signatures[0]
                .label
                .contains("outer(")
        );
        type_and_request(&mut editor, ")").await;
        assert!(!editor.signature_help.is_visible());
        let messages = fs::read_to_string(&events)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        let requests = messages
            .iter()
            .enumerate()
            .filter(|(_, message)| message["method"] == "textDocument/signatureHelp")
            .collect::<Vec<_>>();
        assert_eq!(requests.len(), 5);
        assert_eq!(requests[0].1["params"]["context"]["triggerKind"], 2);
        assert_eq!(requests[0].1["params"]["context"]["triggerCharacter"], "(");
        assert_eq!(requests[0].1["params"]["context"]["isRetrigger"], false);
        assert_eq!(requests[1].1["params"]["context"]["isRetrigger"], true);
        assert!(requests[1].1["params"]["context"]["activeSignatureHelp"].is_object());
        for (index, _) in requests {
            assert!(messages[..index]
                .iter()
                .rev()
                .any(|message| message["method"] == "textDocument/didChange"));
        }
        editor.config.signature_help.auto_trigger = false;
        type_and_request(&mut editor, " + outer(").await;
        assert!(!editor.signature_help.is_active());
        editor.invoke_signature_help().await.unwrap();
        pump_until(&mut editor, |editor| {
            editor.signature_help.pending.is_none()
        })
        .await;
        assert!(editor.signature_help.is_visible());
        editor
            .test_execute_production_action(Action::EnterMode(Mode::Normal))
            .await
            .unwrap();
        assert!(!editor.signature_help.is_active());

        editor.mode = Mode::Insert;
        editor.cx = grapheme_len(&editor.current_buffer().contents());
        editor.config.signature_help.auto_trigger = true;
        editor
            .test_execute_production_action(Action::ApplyCompletion {
                item: Box::new(
                    serde_json::from_value(json!({
                        "label":"inner", "insertText":"inner(${1:0})", "insertTextFormat":2
                    }))
                    .unwrap(),
                ),
                commit_character: None,
            })
            .await
            .unwrap();
        assert!(editor.signature_help.scheduled.is_some());
        editor.service_signature_help().await.unwrap();
        pump_until(&mut editor, |editor| {
            editor.signature_help.pending.is_none()
        })
        .await;
        assert!(
            editor.signature_help.visible.as_ref().unwrap().signatures[0]
                .label
                .contains("inner(")
        );
    }
}
