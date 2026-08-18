//! Editor-owned scheduling, preview validation, and acceptance for inline AI.

use super::*;
use crate::{
    agent_tools::EditorPosition,
    config::InlineCompletionMode,
    copilot::{
        Bridge, CompletionItem, CompletionRequest, Control, Event as CopilotEvent,
        SelectedCompletionInfo, Snapshot,
    },
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct InlineCompletionState {
    bridge: Option<Bridge>,
    enabled_override: Option<bool>,
    generation: u64,
    scheduled: Option<(Instant, Snapshot)>,
    requested: Option<Snapshot>,
    observed_selection: Option<ObservedCompletion>,
    selected_completion: Option<CoordinatedCompletion>,
    pub(super) suggestion: Option<Suggestion>,
    status: String,
    failed: bool,
    prompts: VecDeque<CopilotEvent>,
    pub(super) sign_in: Option<Arc<Mutex<CopilotSignInModel>>>,
    setup_hint_checked: bool,
}

pub(super) struct Suggestion {
    pub snapshot: Snapshot,
    pub insertion: String,
    item: CompletionItem,
    shown: bool,
}

#[derive(Clone, PartialEq)]
struct ObservedCompletion {
    item: CompletionResponseItem,
    snapshot: CompletionSnapshot,
}

#[derive(Clone, PartialEq)]
struct CoordinatedCompletion {
    item: CompletionResponseItem,
    info: SelectedCompletionInfo,
    insertion: String,
}

pub(super) struct CoordinatedContinuation {
    buffer_id: BufferId,
    contents: String,
    cursor: TextPosition,
    insertion: String,
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
        // Opening ordinary completion must only hide a current AI suggestion,
        // not cancel it before the popup has a chance to take priority.
        if mapping == Some(KeyAction::Single(Action::RequestCompletion)) {
            return None;
        }
        self.visible_inline_suggestion()?;
        // The menu owns navigation, Tab, Enter, and dismissal. Only the explicit
        // AI binding accepts the combined preview while the menu is open.
        if self.visible_completion_menu() {
            return matches!(
                mapping,
                Some(KeyAction::Single(
                    Action::AcceptInlineCompletion | Action::DismissInlineCompletion
                ))
            )
            .then_some(mapping)
            .flatten();
        }
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
            selected_completion_info: self
                .inline_completion
                .selected_completion
                .as_ref()
                .map(|selected| selected.info.clone()),
        })
    }

    fn inline_snapshot_current(&self, snapshot: &Snapshot) -> bool {
        self.is_insert()
            && self.copilot_enabled()
            && self.completion_selection_is_observed()
            && self.inline_snapshot().as_ref() == Some(snapshot)
    }

    fn inline_editor_focused(&self) -> bool {
        self.is_insert()
            && !self.workspace_manager.is_active()
            && !self.panel_manager.has_focused_panel()
    }

    fn visible_completion_menu(&self) -> bool {
        self.current_dialog.as_ref().is_some_and(|dialog| {
            dialog.allows_event_passthrough() && dialog.has_shortcut_context()
        })
    }

    fn completion_selection(&self) -> Option<(&CompletionResponseItem, &CompletionSnapshot)> {
        if !self.copilot_enabled()
            || !self.config.completion.enabled
            || self.config.completion.inline_mode != InlineCompletionMode::Coordinated
            || !self.visible_completion_menu()
        {
            return None;
        }
        let item = self.current_dialog.as_ref()?.selected_completion()?;
        let snapshot = self.completion_snapshot.as_ref()?;
        self.completion_snapshot_is_current(snapshot)
            .then_some((item, snapshot))
    }

    fn completion_selection_is_observed(&self) -> bool {
        match (
            self.completion_selection(),
            &self.inline_completion.observed_selection,
        ) {
            (None, None) => true,
            (Some((item, snapshot)), Some(observed)) => {
                item == &observed.item && snapshot == &observed.snapshot
            }
            _ => false,
        }
    }

    /// This cheap check also hides stale previews before the next service tick.
    fn coordinated_completion(&self) -> Option<&CoordinatedCompletion> {
        self.inline_completion
            .selected_completion
            .as_ref()
            .filter(|_| self.completion_selection_is_observed())
    }

    /// Only insertion-equivalent, side-effect-free items can share a preview.
    /// Resolve against source text once per selection/document change, not per frame.
    fn prepare_coordinated_completion(
        &self,
        observed: &ObservedCompletion,
    ) -> Option<CoordinatedCompletion> {
        let item = &observed.item;
        let snapshot = &observed.snapshot;
        if !self.copilot_file_allowed()
            || item.insert_text_format == Some(InsertTextFormat::Snippet)
            || item.command.is_some()
            || item
                .additional_text_edits
                .as_ref()
                .is_some_and(|edits| !edits.is_empty())
        {
            return None;
        }
        let edit = self.completion_main_edit(item, Some(snapshot))?;
        if edit.range.end != self.cursor_lsp_position() {
            return None;
        }
        let contents = self.current_buffer().contents();
        let candidate = CompletionItem {
            insert_text: edit.new_text.clone(),
            range: Some(edit.range.clone()),
            command: None,
            extra: Default::default(),
        };
        let insertion =
            insertion_for_edit(&contents, self.cursor_lsp_position(), &candidate, true)?;
        // Ordinary completion does not normalize newlines. Require the preview
        // projection and the actual ordinary edit to produce identical bytes.
        let actual = crate::lsp::apply_text_edits(&contents, std::slice::from_ref(&edit)).ok()?;
        let insertion_edit = LspTextEdit {
            range: Range {
                start: edit.range.end,
                end: edit.range.end,
            },
            new_text: insertion.clone(),
        };
        if actual != crate::lsp::apply_text_edits(&contents, &[insertion_edit]).ok()? {
            return None;
        }
        Some(CoordinatedCompletion {
            item: item.clone(),
            info: SelectedCompletionInfo {
                range: edit.range,
                text: edit.new_text,
            },
            insertion,
        })
    }

    /// Selection changes do not change the buffer revision, so they need their
    /// own generation and cancellation boundary.
    fn refresh_coordinated_completion(&mut self) -> bool {
        if self.completion_selection_is_observed() {
            return false;
        }
        let observed = self
            .completion_selection()
            .map(|(item, snapshot)| ObservedCompletion {
                item: item.clone(),
                snapshot: snapshot.clone(),
            });
        let selected = observed
            .as_ref()
            .and_then(|observed| self.prepare_coordinated_completion(observed));
        let changed = selected.is_some() || self.inline_completion.selected_completion.is_some();
        self.inline_completion.observed_selection = observed;
        self.inline_completion.selected_completion = selected;
        if changed {
            self.schedule_inline_completion();
        }
        changed
    }

    /// An installed completion component may have no visible matches.
    /// Only an interactive popup (or another dialog) takes priority over ghost text.
    fn inline_completion_obscured(&self) -> bool {
        self.current_dialog.as_ref().is_some_and(|dialog| {
            !dialog.allows_event_passthrough()
                || (dialog.has_shortcut_context() && self.coordinated_completion().is_none())
        })
    }

    pub(super) fn visible_inline_suggestion(&self) -> Option<&Suggestion> {
        self.inline_completion
            .suggestion
            .as_ref()
            .filter(|suggestion| {
                self.inline_editor_focused()
                    && !self.inline_completion_obscured()
                    && self.inline_snapshot_current(&suggestion.snapshot)
            })
    }

    fn ensure_copilot(&mut self) -> bool {
        if self.config.disable_ai {
            self.set_legacy_message(Some("Copilot is disabled by disable_ai = true".into()));
            return false;
        }
        if !self.copilot_enabled() {
            self.set_legacy_message(Some(
                "Copilot is disabled; use :Copilot enable to allow source-code transmission".into(),
            ));
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

    fn copilot_executable_available(&self) -> bool {
        crate::codex::find_executable(&self.config.copilot.command).is_some()
    }

    fn mark_copilot_setup_hint_seen(&mut self) {
        self.inline_completion.setup_hint_checked = true;
        if let Err(error) = self.preferences.mark_copilot_setup_hint_seen() {
            log!("failed to persist Copilot setup hint: {error}");
        }
    }

    fn confirm_copilot_sign_in(&mut self) {
        if self.config.disable_ai {
            self.set_legacy_message(Some("Copilot is disabled by disable_ai = true".into()));
            return;
        }
        if !self.copilot_executable_available() {
            self.set_legacy_message(Some(format!(
                "Copilot language server not found: {}",
                self.config.copilot.command
            )));
            return;
        }
        self.mark_copilot_setup_hint_seen();
        self.current_dialog = Some(Box::new(Confirmation::new_actions(
            self,
            "Enable GitHub Copilot?",
            "Eligible source files may be sent to GitHub for inline suggestions. Red will remember that Copilot is enabled.",
            "Enable and sign in",
            "Cancel",
            Action::CopilotEnableAndSignIn,
            Action::CloseDialog,
        )));
    }

    pub(super) fn enable_and_sign_in_copilot(&mut self) {
        if self.config.disable_ai {
            self.set_legacy_message(Some("Copilot is disabled by disable_ai = true".into()));
            return;
        }
        let saved = self.set_copilot_enabled(true);
        self.inline_completion.failed = false;
        if self.ensure_copilot() {
            self.set_legacy_message(Some("Contacting GitHub Copilot...".into()));
            self.copilot_control(Control::SignIn);
        }
        self.report_copilot_save_error(true, saved);
    }

    fn set_copilot_enabled(&mut self, enabled: bool) -> anyhow::Result<()> {
        self.mark_copilot_setup_hint_seen();
        self.inline_completion.enabled_override = Some(enabled);
        anyhow::ensure!(
            self.preferences.is_persistent(),
            "no persistent user configuration"
        );
        Config::persist_copilot_enabled(&self.language_config_path, enabled)?;
        self.config.copilot.enabled = enabled;
        Ok(())
    }

    fn report_copilot_save_error(&mut self, enabled: bool, saved: anyhow::Result<()>) {
        if let Err(error) = saved {
            let state = if enabled { "enabled" } else { "disabled" };
            self.set_notification_message(
                Severity::Warning,
                Some(format!(
                    "Copilot {state} for this session only; couldn't save configuration: {error:#}"
                )),
            );
        }
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
            self.set_legacy_message(Some(error.to_string()));
        }
    }

    pub(super) fn handle_copilot_command(&mut self, command: &str) {
        use crate::copilot::CopilotCommand;
        match CopilotCommand::parse(command) {
            Some(CopilotCommand::Enable) => {
                if self.config.disable_ai {
                    self.set_legacy_message(Some(
                        "Copilot is disabled by disable_ai = true".into(),
                    ));
                    return;
                }
                let saved = self.set_copilot_enabled(true);
                self.inline_completion.failed = false;
                self.ensure_copilot();
                self.set_legacy_message(Some(
                    "Copilot enabled; eligible source files may be sent to GitHub".into(),
                ));
                self.report_copilot_save_error(true, saved);
            }
            Some(CopilotCommand::Disable) => {
                let saved = self.set_copilot_enabled(false);
                self.dismiss_inline_completion();
                if self.inline_completion.sign_in.take().is_some() {
                    self.current_dialog = None;
                }
                self.inline_completion.bridge = None;
                self.inline_completion.prompts.clear();
                self.inline_completion.status = "Disabled".into();
                self.set_legacy_message(Some("Copilot disabled".into()));
                self.report_copilot_save_error(false, saved);
            }
            Some(CopilotCommand::SignIn) => {
                self.inline_completion.failed = false;
                if self.copilot_enabled() {
                    self.enable_and_sign_in_copilot();
                } else {
                    self.confirm_copilot_sign_in();
                }
            }
            Some(CopilotCommand::Restart) => {
                self.inline_completion.failed = false;
                self.inline_completion.bridge = None;
                self.ensure_copilot();
            }
            Some(CopilotCommand::SignOut) => {
                self.dismiss_inline_completion();
                if self.inline_completion.sign_in.take().is_some() {
                    self.current_dialog = None;
                }
                self.copilot_control(Control::SignOut);
            }
            Some(CopilotCommand::Status) => self.set_legacy_message(Some(format!(
                "Copilot: {}",
                if !self.copilot_enabled() {
                    "Disabled"
                } else if self.inline_completion.status.is_empty() {
                    "Not started"
                } else {
                    &self.inline_completion.status
                }
            ))),
            _ => self.set_legacy_message(Some(CopilotCommand::usage())),
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

    fn write_copilot_sign_in_code(&mut self, code: &str) -> bool {
        if !self.config.clipboard.enabled || !self.clipboard.is_available() {
            return false;
        }
        match self.clipboard.set_text(code) {
            Ok(()) => true,
            Err(error) => {
                log!("failed to copy Copilot device code: {error}");
                false
            }
        }
    }

    fn show_copilot_sign_in(&mut self, user_code: String, command: Value) {
        let clipboard_copied = self.write_copilot_sign_in_code(&user_code);
        let model = Arc::new(Mutex::new(CopilotSignInModel {
            user_code,
            command,
            phase: CopilotSignInPhase::Ready,
            clipboard_copied,
        }));
        self.current_dialog = Some(Box::new(CopilotSignInDialog::new(self, model.clone())));
        self.inline_completion.sign_in = Some(model);
    }

    pub(super) fn copy_copilot_sign_in_code(&mut self, code: &str) {
        let copied = self.write_copilot_sign_in_code(code);
        if let Some(model) = &self.inline_completion.sign_in {
            if let Ok(mut model) = model.lock() {
                if model.user_code == code {
                    model.clipboard_copied = copied;
                }
            }
        }
        self.set_legacy_message(Some(if copied {
            "Copilot device code copied to clipboard".into()
        } else {
            "Unable to copy Copilot device code; it remains visible in the dialog".into()
        }));
    }

    pub(super) fn retry_copilot_sign_in(&mut self) {
        self.inline_completion.failed = false;
        self.copilot_control(Control::SignIn);
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
            self.inline_completion.observed_selection = None;
            self.inline_completion.selected_completion = None;
        }
        let Some(snapshot) = self.inline_snapshot() else {
            return;
        };
        if !self.copilot_file_allowed() {
            if !automatic {
                self.set_legacy_message(Some(
                    "Copilot: file excluded, outside workspace, or too large".into(),
                ));
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
        if !self.config.completion.enabled {
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
                changed = true;
            }
        }
        changed |= self.refresh_coordinated_completion();
        if !self.inline_completion.setup_hint_checked
            && !self.config.disable_ai
            && !self.config.copilot.enabled
            && self.preferences.is_persistent()
            && !self.preferences.copilot_setup_hint_seen()
            && self.current_dialog.is_none()
            && self.last_error.is_none()
            && self.copilot_file_allowed()
            && self.copilot_executable_available()
        {
            self.mark_copilot_setup_hint_seen();
            self.set_legacy_message(Some(
                "GitHub Copilot is available. Run :Copilot signin to enable inline suggestions."
                    .into(),
            ));
            changed = true;
        }
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
            if !self.inline_snapshot_current(&snapshot) {
                self.inline_completion.scheduled = None;
            } else if Instant::now() >= deadline && !self.inline_completion_obscured() {
                self.inline_completion.scheduled = None;
                self.request_inline_completion(true);
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
                CopilotEvent::SignInFinished { error } => {
                    if let Some(error) = error {
                        if let Some(model) = &self.inline_completion.sign_in {
                            if let Ok(mut model) = model.lock() {
                                model.phase = CopilotSignInPhase::Failed(error.clone());
                            }
                        }
                        self.inline_completion.status = format!("Sign-in failed: {error}");
                        self.set_legacy_message(Some(format!("Copilot sign-in failed: {error}")));
                    } else {
                        self.inline_completion.status = "Signed in".into();
                        if self.inline_completion.sign_in.take().is_some() {
                            self.current_dialog = None;
                        }
                        self.set_legacy_message(Some("Copilot signed in".into()));
                    }
                    changed = true;
                }
                CopilotEvent::Stopped(status) => {
                    self.inline_completion.status = status.clone();
                    self.inline_completion.failed = true;
                    self.inline_completion.bridge = None;
                    self.dismiss_inline_completion();
                    if let Some(model) = &self.inline_completion.sign_in {
                        if let Ok(mut model) = model.lock() {
                            model.phase = CopilotSignInPhase::Failed(status.clone());
                        }
                    }
                    self.set_legacy_message(Some(status));
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
                        if let Some(selected) = &snapshot.selected_completion_info {
                            if item.range.as_ref() != Some(&selected.range)
                                || item
                                    .insert_text
                                    .strip_prefix(&selected.text)
                                    .is_none_or(|suffix| suffix.is_empty())
                            {
                                return None;
                            }
                        }
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
                CopilotEvent::SignIn { user_code, command }
                    if self.inline_completion.sign_in.is_some() =>
                {
                    self.show_copilot_sign_in(user_code, command);
                    changed = true;
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
                match prompt {
                    CopilotEvent::SignIn { user_code, command } => {
                        self.show_copilot_sign_in(user_code, command);
                    }
                    CopilotEvent::Message {
                        id,
                        message,
                        actions,
                    } => {
                        self.current_dialog = Some(Box::new(CopilotMessagePicker::new(
                            self, id, message, actions,
                        )));
                    }
                    _ => unreachable!("only prompts are queued"),
                }
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
        if self.visible_inline_suggestion().is_none() {
            return Ok(());
        }
        let Some(suggestion) = self.inline_completion.suggestion.take() else {
            return Ok(());
        };
        if !self.inline_snapshot_current(&suggestion.snapshot) {
            self.dismiss_inline_completion();
            return Ok(());
        }
        // A filtered-out completion popup must not survive the accepted edit.
        if self.current_dialog.is_some() {
            self.current_dialog = None;
            self.completion_snapshot = None;
        }
        self.inline_completion.observed_selection = None;
        self.inline_completion.selected_completion = None;
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

    /// Capture the suffix before ordinary completion invalidates the snapshot.
    pub(super) fn coordinated_completion_continuation(
        &self,
        item: &CompletionResponseItem,
        commit_character: Option<char>,
    ) -> Option<CoordinatedContinuation> {
        if commit_character.is_some() {
            return None;
        }
        let suggestion = self.visible_inline_suggestion()?;
        let selected = self.coordinated_completion()?;
        if &selected.item != item {
            return None;
        }
        let insertion = suggestion.insertion.strip_prefix(&selected.insertion)?;
        if insertion.is_empty() {
            return None;
        }
        let contents = self.current_buffer().contents();
        let edit = LspTextEdit {
            range: selected.info.range.clone(),
            new_text: selected.info.text.clone(),
        };
        let prepared = completion_edit_from_lsp(&contents, &edit, None, true).ok()?;
        Some(CoordinatedContinuation {
            buffer_id: self.current_buffer().id(),
            contents: crate::lsp::apply_text_edits(&contents, &[edit]).ok()?,
            cursor: offset_text_position(
                prepared.range.start,
                &prepared.new_text,
                prepared.new_text.chars().count(),
            ),
            insertion: insertion.to_owned(),
            item: suggestion.item.clone(),
            shown: suggestion.shown,
        })
    }

    pub(super) fn restore_coordinated_completion(&mut self, continuation: CoordinatedContinuation) {
        if self.current_buffer().id() != continuation.buffer_id
            || self.current_buffer().contents() != continuation.contents
            || self.cursor_text_position() != continuation.cursor
            || !self.inline_editor_focused()
            || !self.copilot_enabled()
        {
            return;
        }
        self.current_dialog = None;
        self.completion_snapshot = None;
        self.scheduled_completion = None;
        for pending in self.pending_completions.values_mut() {
            pending.superseded = true;
        }
        self.inline_completion.observed_selection = None;
        self.inline_completion.selected_completion = None;
        self.dismiss_inline_completion();
        if let Some(snapshot) = self.inline_snapshot() {
            self.inline_completion.suggestion = Some(Suggestion {
                snapshot,
                insertion: continuation.insertion,
                item: continuation.item,
                shown: continuation.shown,
            });
            self.layout_cache.borrow_mut().clear();
            self.force_full_redraw = true;
        }
    }
}

fn insertion_for_item(
    contents: &str,
    cursor: LspPosition,
    item: &CompletionItem,
) -> Option<String> {
    insertion_for_edit(contents, cursor, item, false)
}

fn insertion_for_edit(
    contents: &str,
    cursor: LspPosition,
    item: &CompletionItem,
    allow_empty: bool,
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
    if (!allow_empty && inserted.is_empty())
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
    use crate::{clipboard::MemoryClipboardProvider, test_utils::EditorTestExt};

    fn editor(text: &str) -> Editor {
        let mut config = Config::from_user_toml_with_overrides("", &[]).unwrap();
        config.copilot.enabled = true;
        editor_with_config(text, config)
    }

    fn editor_with_config(text: &str, mut config: Config) -> Editor {
        config.lsp.enabled = false;
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

    fn use_temporary_config(editor: &mut Editor, directory: &Path, contents: &str) -> PathBuf {
        let path = directory.join("config.toml");
        std::fs::write(&path, contents).unwrap();
        editor.preferences = PreferencesStore::load(directory.join("preferences.json"));
        editor.set_language_reload_source(path.clone(), Vec::new());
        path
    }

    fn reopen_with_config(path: &Path) -> Editor {
        editor_with_config("foo", Config::load_user_file(path, &[]).unwrap().config)
    }

    fn show_popup(editor: &mut Editor, label: &str) {
        let item = serde_json::from_value(json!({"label": label})).unwrap();
        assert!(editor.show_completion_items(vec![item], editor.completion_snapshot()));
    }

    fn expire_inline_schedule(editor: &mut Editor) {
        editor.inline_completion.scheduled.as_mut().unwrap().0 = Instant::now();
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

    fn ordinary(label: &str) -> CompletionResponseItem {
        serde_json::from_value(json!({"label": label})).unwrap()
    }

    async fn coordinated_editor(
        text: &str,
        cursor: usize,
        items: Vec<CompletionResponseItem>,
    ) -> (
        Editor,
        tokio::sync::watch::Receiver<Option<CompletionRequest>>,
        tokio::sync::mpsc::Receiver<Control>,
        tokio::sync::mpsc::Sender<CopilotEvent>,
    ) {
        let mut editor = editor(text);
        editor.config.completion.inline_mode = InlineCompletionMode::Coordinated;
        show(&mut editor, "", cursor).await;
        editor.dismiss_inline_completion();
        let (bridge, requests, controls, events) = Bridge::test_channels();
        editor.inline_completion.bridge = Some(bridge);
        editor.inline_completion.failed = false;
        assert!(editor.show_completion_items(items, editor.completion_snapshot()));
        editor.service_inline_completion();
        if editor.coordinated_completion().is_some() {
            expire_inline_schedule(&mut editor);
            editor.service_inline_completion();
        }
        (editor, requests, controls, events)
    }

    async fn respond(
        editor: &mut Editor,
        events: &tokio::sync::mpsc::Sender<CopilotEvent>,
        snapshot: Snapshot,
        items: Vec<CompletionItem>,
    ) {
        events
            .send(CopilotEvent::Completion { snapshot, items })
            .await
            .unwrap();
        editor.service_inline_completion();
    }

    async fn execute_key(editor: &mut Editor, code: KeyCode, modifiers: KeyModifiers) {
        match editor
            .handle_event(&Event::Key(KeyEvent::new(code, modifiers)))
            .unwrap()
        {
            Some(KeyAction::Single(action)) => editor.test_execute_action(action).await.unwrap(),
            Some(KeyAction::Multiple(actions)) => {
                for action in actions {
                    editor.test_execute_action(action).await.unwrap();
                }
            }
            Some(KeyAction::None) | None => {}
            action => panic!("unexpected action: {action:?}"),
        }
    }

    #[tokio::test]
    async fn coordinated_preview_extends_selected_item_and_accepts_in_two_steps() {
        for accept in [KeyCode::Tab, KeyCode::Enter] {
            let (mut editor, requests, mut controls, events) =
                coordinated_editor("foo()", 3, vec![ordinary("foobar")]).await;
            let request = requests.borrow().clone().unwrap();
            assert_eq!(request.contents, "foo()");
            assert_eq!(
                request
                    .snapshot
                    .selected_completion_info
                    .as_ref()
                    .unwrap()
                    .text,
                "foobar"
            );
            let ai = item("foobar_extra", 0, 3);
            respond(&mut editor, &events, request.snapshot, vec![ai.clone()]).await;
            assert!(editor.visible_completion_menu());
            assert_eq!(
                editor.visible_inline_suggestion().unwrap().insertion,
                "bar_extra"
            );
            assert!(rendered_rows(&mut editor)
                .iter()
                .any(|row| row.contains("foobar_extra()")));
            assert_eq!(editor.current_buffer().contents(), "foo()");
            assert!(matches!(controls.try_recv(), Ok(Control::Shown(_))));

            execute_key(&mut editor, accept, KeyModifiers::NONE).await;
            assert_eq!(editor.current_buffer().contents(), "foobar()");
            assert!(editor.current_dialog.is_none());
            assert_eq!(
                editor.visible_inline_suggestion().unwrap().insertion,
                "_extra"
            );
            assert!(controls.try_recv().is_err());
            editor.service_inline_completion();
            assert!(controls.try_recv().is_err());
            execute_key(&mut editor, KeyCode::Tab, KeyModifiers::NONE).await;
            assert_eq!(editor.current_buffer().contents(), "foobar_extra()");
            assert!(
                matches!(controls.try_recv(), Ok(Control::Accepted(accepted)) if accepted == ai)
            );
            editor
                .test_execute_action(Action::EnterMode(Mode::Normal))
                .await
                .unwrap();
            editor.test_execute_action(Action::Undo).await.unwrap();
            assert_eq!(editor.current_buffer().contents(), "foobar()");
            editor.test_execute_action(Action::Undo).await.unwrap();
            assert_eq!(editor.current_buffer().contents(), "foo()");
        }
    }

    #[tokio::test]
    async fn coordinated_ctrl_l_accepts_the_whole_edit_in_one_undo_step() {
        let (mut editor, requests, mut controls, events) =
            coordinated_editor("foo()", 3, vec![ordinary("foobar")]).await;
        let snapshot = requests.borrow().as_ref().unwrap().snapshot.clone();
        let ai = item("foobar\nnext", 0, 3);
        respond(&mut editor, &events, snapshot, vec![ai.clone()]).await;
        assert!(matches!(controls.try_recv(), Ok(Control::Shown(_))));
        execute_key(&mut editor, KeyCode::Char('l'), KeyModifiers::CONTROL).await;
        assert_eq!(editor.current_buffer().contents(), "foobar\nnext()");
        assert!(editor.current_dialog.is_none());
        assert!(matches!(controls.try_recv(), Ok(Control::Accepted(accepted)) if accepted == ai));
        editor
            .test_execute_action(Action::EnterMode(Mode::Normal))
            .await
            .unwrap();
        editor.test_execute_action(Action::Undo).await.unwrap();
        assert_eq!(editor.current_buffer().contents(), "foo()");
    }

    #[tokio::test]
    async fn coordinated_selection_change_rejects_old_results_without_a_text_edit() {
        let (mut editor, requests, _controls, events) =
            coordinated_editor("foo", 3, vec![ordinary("foobar"), ordinary("foobaz")]).await;
        let first = requests.borrow().as_ref().unwrap().snapshot.clone();
        let selected = first
            .selected_completion_info
            .as_ref()
            .unwrap()
            .text
            .clone();
        respond(
            &mut editor,
            &events,
            first.clone(),
            vec![item(&format!("{selected}_old"), 0, 3)],
        )
        .await;
        assert!(editor.visible_inline_suggestion().is_some());
        execute_key(&mut editor, KeyCode::Down, KeyModifiers::NONE).await;
        assert!(editor.visible_inline_suggestion().is_none());
        editor
            .test_execute_action(Action::AcceptInlineCompletion)
            .await
            .unwrap();
        assert_eq!(editor.current_buffer().contents(), "foo");
        editor.service_inline_completion();
        expire_inline_schedule(&mut editor);
        editor.service_inline_completion();
        let second = requests.borrow().as_ref().unwrap().snapshot.clone();
        assert_eq!(first.revision, second.revision);
        assert_ne!(first.generation, second.generation);
        assert_ne!(
            first.selected_completion_info,
            second.selected_completion_info
        );
        respond(
            &mut editor,
            &events,
            first,
            vec![item(&format!("{selected}_late"), 0, 3)],
        )
        .await;
        assert!(editor.visible_inline_suggestion().is_none());
        let selected = second
            .selected_completion_info
            .as_ref()
            .unwrap()
            .text
            .clone();
        respond(
            &mut editor,
            &events,
            second,
            vec![item(&format!("{selected}_new"), 0, 3)],
        )
        .await;
        assert!(editor
            .visible_inline_suggestion()
            .unwrap()
            .insertion
            .ends_with("_new"));
    }

    #[tokio::test]
    async fn coordinated_rejects_incompatible_results_and_unsafe_ordinary_items() {
        let (mut editor, requests, _controls, events) =
            coordinated_editor("foo", 3, vec![ordinary("foobar")]).await;
        let snapshot = requests.borrow().as_ref().unwrap().snapshot.clone();
        respond(
            &mut editor,
            &events,
            snapshot,
            vec![
                item("foobaz_extra", 0, 3),
                item("foobar", 0, 3),
                item("foobar_extra", 1, 3),
            ],
        )
        .await;
        assert!(editor.visible_inline_suggestion().is_none());
        assert!(editor.visible_completion_menu());

        for value in [
            json!({"label":"foobar", "insertTextFormat":2}),
            json!({"label":"foobar", "command":{"title":"run","command":"run"}}),
            json!({"label":"foobar", "additionalTextEdits":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},"newText":"import\n"}]}),
            json!({"label":"foobar", "textEdit":{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":4}},"newText":"foobar"}}),
            json!({"label":"foobar", "insertText":"different"}),
        ] {
            let ordinary = serde_json::from_value(value).unwrap();
            let (editor, requests, _controls, _events) =
                coordinated_editor("foo()", 3, vec![ordinary]).await;
            assert!(editor.coordinated_completion().is_none());
            assert!(editor.inline_completion_obscured());
            assert!(requests.borrow().is_none());
        }
    }

    #[tokio::test]
    async fn coordinated_identity_includes_payload_and_explicit_request_is_standalone() {
        let (mut editor, requests, _controls, events) =
            coordinated_editor("foo", 3, vec![ordinary("foobar")]).await;
        let first = requests.borrow().as_ref().unwrap().snapshot.clone();
        respond(
            &mut editor,
            &events,
            first.clone(),
            vec![item("foobar_extra", 0, 3)],
        )
        .await;
        let mut changed = ordinary("foobar");
        changed.data = Some(json!({"identity":2}));
        assert!(editor.show_completion_items(vec![changed], editor.completion_snapshot()));
        assert!(editor.visible_inline_suggestion().is_none());
        editor.service_inline_completion();
        expire_inline_schedule(&mut editor);
        editor.service_inline_completion();
        let second = requests.borrow().as_ref().unwrap().snapshot.clone();
        assert_eq!(
            first.selected_completion_info,
            second.selected_completion_info
        );
        assert_ne!(first.generation, second.generation);
        execute_key(&mut editor, KeyCode::Char('\\'), KeyModifiers::ALT).await;
        assert!(editor.current_dialog.is_none());
        let request = requests.borrow().clone().unwrap();
        assert!(!request.automatic);
        assert!(request.snapshot.selected_completion_info.is_none());
        assert_eq!(request.contents, "foo");
    }

    #[tokio::test]
    async fn coordinated_plain_text_edits_use_rebased_utf16_ranges() {
        let ordinary = serde_json::from_value(json!({
            "label":"foobar",
            "textEdit":{"range":{"start":{"line":0,"character":2},"end":{"line":0,"character":5}},"newText":"foobar"}
        })).unwrap();
        let (mut editor, requests, _controls, events) =
            coordinated_editor("😀foo", 4, vec![ordinary]).await;
        execute_key(&mut editor, KeyCode::Char('b'), KeyModifiers::NONE).await;
        editor.service_inline_completion();
        expire_inline_schedule(&mut editor);
        editor.service_inline_completion();
        let request = requests.borrow().clone().unwrap();
        let selected = request.snapshot.selected_completion_info.as_ref().unwrap();
        assert_eq!(selected.range.start.character, 2);
        assert_eq!(selected.range.end.character, 6);
        respond(
            &mut editor,
            &events,
            request.snapshot,
            vec![item("foobar_extra", 2, 6)],
        )
        .await;
        execute_key(&mut editor, KeyCode::Tab, KeyModifiers::NONE).await;
        assert_eq!(editor.current_buffer().contents(), "😀foobar");
        assert_eq!(
            editor.visible_inline_suggestion().unwrap().insertion,
            "_extra"
        );
    }

    #[tokio::test]
    async fn completion_can_be_disabled_without_disabling_copilot() {
        let mut editor = editor("foo foobar");
        show(&mut editor, "_ai", 3).await;
        show_popup(&mut editor, "foobar");
        editor.config.completion.enabled = false;
        editor.service_inline_completion();
        assert!(editor.current_dialog.is_none());
        assert!(editor.copilot_enabled());
        editor.schedule_automatic_completion();
        assert!(editor.scheduled_completion.is_none());
        assert!(!editor.request_completion(None).await.unwrap());
        assert!(
            !editor.show_completion_items(vec![ordinary("foobar")], editor.completion_snapshot())
        );
        assert_eq!(editor.visible_inline_suggestion().unwrap().insertion, "_ai");
        execute_key(&mut editor, KeyCode::Tab, KeyModifiers::NONE).await;
        assert_eq!(editor.current_buffer().contents(), "foo_ai foobar");

        editor.config.completion.enabled = true;
        editor.config.completion.auto_trigger = false;
        editor.schedule_automatic_completion();
        assert!(editor.scheduled_completion.is_none());
        editor
            .test_execute_action(Action::SetCursor(3, 0))
            .await
            .unwrap();
        assert!(editor.request_completion(None).await.unwrap());
    }

    #[tokio::test]
    async fn coordinated_dismissal_returns_to_standalone_and_commit_drops_suffix() {
        let (mut editor, requests, _controls, events) =
            coordinated_editor("foo", 3, vec![ordinary("foobar")]).await;
        let old = requests.borrow().as_ref().unwrap().snapshot.clone();
        respond(
            &mut editor,
            &events,
            old.clone(),
            vec![item("foobar_extra", 0, 3)],
        )
        .await;
        execute_key(&mut editor, KeyCode::Char('e'), KeyModifiers::CONTROL).await;
        assert!(editor.current_dialog.is_none());
        assert!(editor.visible_inline_suggestion().is_none());
        editor.service_inline_completion();
        expire_inline_schedule(&mut editor);
        editor.service_inline_completion();
        let standalone = requests.borrow().as_ref().unwrap().snapshot.clone();
        assert!(standalone.selected_completion_info.is_none());
        respond(&mut editor, &events, old, vec![item("foobar_late", 0, 3)]).await;
        assert!(editor.visible_inline_suggestion().is_none());
        respond(
            &mut editor,
            &events,
            standalone,
            vec![item("foo_standalone", 0, 3)],
        )
        .await;
        assert_eq!(
            editor.visible_inline_suggestion().unwrap().insertion,
            "_standalone"
        );

        let mut ordinary = ordinary("foobar");
        ordinary.commit_characters = Some(vec![".".into()]);
        let (mut editor, requests, _controls, events) =
            coordinated_editor("foo", 3, vec![ordinary]).await;
        let snapshot = requests.borrow().as_ref().unwrap().snapshot.clone();
        respond(
            &mut editor,
            &events,
            snapshot,
            vec![item("foobar_extra", 0, 3)],
        )
        .await;
        execute_key(&mut editor, KeyCode::Char('.'), KeyModifiers::NONE).await;
        assert_eq!(editor.current_buffer().contents(), "foobar.");
        assert!(editor.visible_inline_suggestion().is_none());
    }

    #[tokio::test]
    async fn coordinated_already_typed_item_preserves_a_crlf_suffix() {
        let (mut editor, requests, _controls, events) =
            coordinated_editor("foo\r\n", 3, vec![ordinary("foo")]).await;
        let snapshot = requests.borrow().as_ref().unwrap().snapshot.clone();
        respond(
            &mut editor,
            &events,
            snapshot,
            vec![item("foo\nnext", 0, 3)],
        )
        .await;
        assert_eq!(
            editor.visible_inline_suggestion().unwrap().insertion,
            "\r\nnext"
        );
        execute_key(&mut editor, KeyCode::Tab, KeyModifiers::NONE).await;
        assert_eq!(editor.current_buffer().contents(), "foo\r\n");
        assert_eq!(
            editor.visible_inline_suggestion().unwrap().insertion,
            "\r\nnext"
        );
        execute_key(&mut editor, KeyCode::Tab, KeyModifiers::NONE).await;
        assert_eq!(editor.current_buffer().contents(), "foo\r\nnext\r\n");
    }

    #[tokio::test]
    async fn disabled_completion_rejects_late_lsp_responses() {
        let mut editor = editor("foo");
        show(&mut editor, "_ai", 3).await;
        editor.pending_completions.insert(
            71,
            PendingCompletion {
                buffer_items: vec![ordinary("foobar")],
                snapshot: editor.completion_snapshot(),
                displayed_immediately: false,
                superseded: false,
            },
        );
        editor.config.completion.enabled = false;
        let response = InboundMessage::Message(ResponseMessage {
            id: 71,
            result: json!([{"label":"foobar"}]),
            request: None,
        });
        assert!(editor
            .handle_lsp_message(&response, Some("textDocument/completion".into()))
            .is_none());
        assert!(editor.current_dialog.is_none());
        assert_eq!(editor.visible_inline_suggestion().unwrap().insertion, "_ai");
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

    #[test]
    fn available_copilot_is_suggested_once_without_starting_it() {
        let mut editor = editor("foo");
        let preferences_dir =
            std::env::temp_dir().join(format!("red-copilot-hint-editor-{}", uuid::Uuid::new_v4()));
        editor.preferences = PreferencesStore::load(preferences_dir.join("preferences.json"));
        editor.config.copilot.enabled = false;
        editor.config.copilot.command = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        editor.inline_completion.bridge = None;

        assert!(editor.service_inline_completion());

        assert_eq!(
            editor.last_error.as_deref(),
            Some("GitHub Copilot is available. Run :Copilot signin to enable inline suggestions.")
        );
        assert!(editor.preferences.copilot_setup_hint_seen());
        assert!(editor.inline_completion.bridge.is_none());

        editor.set_legacy_message(None);
        assert!(!editor.service_inline_completion());
        assert!(editor.last_error.is_none());
        std::fs::remove_dir_all(preferences_dir).ok();
    }

    #[test]
    fn copilot_hint_respects_global_ai_disable() {
        let mut editor = editor("foo");
        editor.config.copilot.enabled = false;
        editor.config.disable_ai = true;
        editor.config.copilot.command = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        editor.inline_completion.bridge = None;

        assert!(!editor.service_inline_completion());
        assert!(!editor.preferences.copilot_setup_hint_seen());
        assert!(editor.last_error.is_none());
    }

    #[tokio::test]
    async fn signin_confirms_consent_then_enables_and_starts_authentication() {
        let mut editor = editor("foo");
        let directory = tempfile::tempdir().unwrap();
        let path = use_temporary_config(
            &mut editor,
            directory.path(),
            "[copilot]\nenabled = false\n",
        );
        editor.config.copilot.enabled = false;
        editor.config.copilot.command = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        editor.inline_completion.bridge = None;

        editor.handle_copilot_command("signin");

        assert!(!editor.copilot_enabled());
        assert!(!reopen_with_config(&path).copilot_enabled());
        assert!(editor.inline_completion.bridge.is_none());
        assert!(editor.preferences.copilot_setup_hint_seen());
        let dialog = editor.current_dialog.as_mut().unwrap();
        dialog.handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Left,
            KeyModifiers::NONE,
        )));
        assert_eq!(
            dialog.handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::CopilotEnableAndSignIn,
            ]))
        );

        let (bridge, _requests, mut controls, events) = Bridge::test_channels();
        editor.inline_completion.bridge = Some(bridge);
        editor
            .test_execute_action(Action::CopilotEnableAndSignIn)
            .await
            .unwrap();

        assert!(editor.copilot_enabled());
        assert_eq!(
            editor.last_error.as_deref(),
            Some("Contacting GitHub Copilot...")
        );
        assert!(matches!(controls.recv().await, Some(Control::SignIn)));
        assert!(reopen_with_config(&path).copilot_enabled());

        events
            .send(CopilotEvent::SignInFinished {
                error: Some("not authorized".into()),
            })
            .await
            .unwrap();
        editor.service_inline_completion();
        assert!(reopen_with_config(&path).copilot_enabled());
    }

    #[tokio::test]
    async fn copilot_enable_and_disable_survive_restart() {
        let mut editor = editor("foo");
        let directory = tempfile::tempdir().unwrap();
        let path = use_temporary_config(&mut editor, directory.path(), "# keep me\n");
        editor.config.copilot.enabled = false;
        let (bridge, _requests, _controls, _events) = Bridge::test_channels();
        editor.inline_completion.bridge = Some(bridge);

        editor.handle_copilot_command("enable");
        assert!(editor.copilot_enabled());
        assert!(reopen_with_config(&path).copilot_enabled());
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("# keep me"));

        editor.handle_copilot_command("disable");
        assert!(!editor.copilot_enabled());
        assert!(editor.inline_completion.bridge.is_none());
        assert!(!reopen_with_config(&path).copilot_enabled());
    }

    #[tokio::test]
    async fn signin_persists_existing_session_enablement_and_signout_keeps_it() {
        let mut editor = editor("foo");
        let directory = tempfile::tempdir().unwrap();
        let path = use_temporary_config(
            &mut editor,
            directory.path(),
            "[copilot]\nenabled = false\n",
        );
        let (bridge, _requests, mut controls, _events) = Bridge::test_channels();
        editor.inline_completion.bridge = Some(bridge);

        editor.handle_copilot_command("signin");
        assert!(matches!(controls.recv().await, Some(Control::SignIn)));
        assert!(reopen_with_config(&path).copilot_enabled());
        let contents = std::fs::read_to_string(&path).unwrap();

        editor.handle_copilot_command("signout");
        assert!(matches!(controls.recv().await, Some(Control::SignOut)));
        assert!(editor.copilot_enabled());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
    }

    #[tokio::test]
    async fn failed_copilot_saves_keep_session_choice_and_warn() {
        let mut editor = editor("foo");
        let directory = tempfile::tempdir().unwrap();
        let contents = "[copilot\n";
        let path = use_temporary_config(&mut editor, directory.path(), contents);
        editor.config.copilot.enabled = false;
        let (bridge, _requests, mut controls, _events) = Bridge::test_channels();
        editor.inline_completion.bridge = Some(bridge);

        editor.enable_and_sign_in_copilot();
        assert!(editor.copilot_enabled());
        assert!(matches!(controls.recv().await, Some(Control::SignIn)));
        assert!(editor
            .last_error
            .as_deref()
            .unwrap()
            .starts_with("Copilot enabled for this session only; couldn't save configuration:"));
        assert!(editor
            .notifications
            .records()
            .any(|notice| notice.severity == Severity::Warning
                && notice
                    .content
                    .summary
                    .contains("couldn't save configuration")));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);

        editor.handle_copilot_command("disable");
        assert!(!editor.copilot_enabled());
        assert!(editor.inline_completion.bridge.is_none());
        assert!(editor
            .last_error
            .as_deref()
            .unwrap()
            .starts_with("Copilot disabled for this session only; couldn't save configuration:"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
    }

    #[test]
    fn global_ai_disable_prevents_persisting_copilot_consent() {
        let mut editor = editor("foo");
        let directory = tempfile::tempdir().unwrap();
        let contents = "disable_ai = true\n[copilot]\nenabled = false\n";
        let path = use_temporary_config(&mut editor, directory.path(), contents);
        editor.config.disable_ai = true;
        editor.config.copilot.enabled = false;
        for command in ["enable", "signin"] {
            editor.handle_copilot_command(command);
            assert!(!editor.copilot_enabled());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
        }
        editor.enable_and_sign_in_copilot();
        assert!(!editor.copilot_enabled());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
    }

    #[test]
    fn signin_reports_a_missing_language_server_without_enabling() {
        let mut editor = editor("foo");
        editor.config.copilot.enabled = false;
        editor.config.copilot.command = format!(
            "red-missing-copilot-language-server-{}",
            uuid::Uuid::new_v4()
        );
        editor.inline_completion.bridge = None;

        editor.handle_copilot_command("signin");

        assert!(!editor.copilot_enabled());
        assert!(editor.current_dialog.is_none());
        assert!(editor
            .last_error
            .as_deref()
            .is_some_and(|message| message.starts_with("Copilot language server not found:")));
    }

    #[tokio::test]
    async fn sign_in_copies_the_code_stays_open_and_tracks_the_result() {
        let mut editor = editor("foo");
        let clipboard = MemoryClipboardProvider::default();
        let clipboard_text = clipboard.shared_text();
        editor.test_set_clipboard(Box::new(clipboard));
        let (bridge, _requests, mut controls, events) = Bridge::test_channels();
        editor.inline_completion.bridge = Some(bridge);
        editor.inline_completion.failed = false;
        let command = json!({
            "command": "github.copilot.finishDeviceFlow",
            "arguments": []
        });

        events
            .send(CopilotEvent::SignIn {
                user_code: "ABCD-EFGH".into(),
                command: command.clone(),
            })
            .await
            .unwrap();
        editor.service_inline_completion();

        assert_eq!(clipboard_text.lock().unwrap().as_deref(), Some("ABCD-EFGH"));
        let action = editor
            .current_dialog
            .as_mut()
            .unwrap()
            .handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .unwrap();
        assert_eq!(
            action,
            KeyAction::Single(Action::CopilotFinishSignIn(command.clone()))
        );
        editor
            .test_execute_action(Action::CopilotFinishSignIn(command))
            .await
            .unwrap();
        assert!(editor.current_dialog.is_some());
        assert!(matches!(
            controls.recv().await,
            Some(Control::FinishSignIn(_))
        ));

        events
            .send(CopilotEvent::SignInFinished {
                error: Some("device code expired".into()),
            })
            .await
            .unwrap();
        editor.service_inline_completion();
        assert!(editor.current_dialog.is_some());
        assert!(matches!(
            editor
                .inline_completion
                .sign_in
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .phase,
            CopilotSignInPhase::Failed(_)
        ));

        events
            .send(CopilotEvent::SignInFinished { error: None })
            .await
            .unwrap();
        editor.service_inline_completion();
        assert!(editor.current_dialog.is_none());
        assert!(editor.inline_completion.sign_in.is_none());
        assert_eq!(editor.last_error.as_deref(), Some("Copilot signed in"));
        assert!(editor
            .notifications()
            .records()
            .any(|notice| notice.content.summary == "Copilot signed in"));
    }

    #[test]
    fn copilot_status_is_published_as_a_visible_notification() {
        let mut editor = editor("foo");
        editor.inline_completion.status = "Signed in".into();

        editor.handle_copilot_command("status");

        assert!(editor
            .notifications()
            .records()
            .any(|notice| notice.content.summary == "Copilot: Signed in"));
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
    async fn ctrl_l_accepts_inline_completion() {
        let mut editor = editor("foo");
        show(&mut editor, "bar", 3).await;

        assert_eq!(
            editor
                .handle_event(&Event::Key(KeyEvent::new(
                    KeyCode::Char('l'),
                    KeyModifiers::CONTROL,
                )))
                .unwrap(),
            Some(KeyAction::Single(Action::AcceptInlineCompletion)),
        );
        editor
            .test_execute_action(Action::AcceptInlineCompletion)
            .await
            .unwrap();
        assert_eq!(editor.current_buffer().contents(), "foobar");
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
        show_popup(&mut editor, "foobar");
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
        assert!(editor.inline_completion.requested.is_some());
    }

    #[tokio::test]
    async fn ordinary_completion_remains_automatically_available() {
        let mut editor = editor("foo foobar");
        let (bridge, requests, _controls, _events) = Bridge::test_channels();
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
        editor.schedule_inline_completion();
        editor.inline_completion.scheduled.as_mut().unwrap().0 =
            Instant::now() + Duration::from_secs(60);
        editor.schedule_automatic_completion();
        assert!(editor.scheduled_completion.is_some());
        editor.scheduled_completion.as_mut().unwrap().deadline = Instant::now();
        let mut output = RenderBuffer::new(60, 16, &editor.theme.style);
        let mut runtime = Runtime::new();
        editor
            .service_background(&mut output, &mut runtime)
            .await
            .unwrap();
        assert!(editor.current_dialog.is_some());
        assert!(requests.borrow().is_none());
        assert!(editor.inline_completion.scheduled.is_some());
    }

    #[tokio::test]
    async fn completion_popup_hides_ghost_text_and_owns_acceptance() {
        let mut editor = editor("foo foobar");
        show(&mut editor, "_ai", 3).await;
        let request = editor
            .handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Char(' '),
                KeyModifiers::CONTROL,
            )))
            .unwrap();
        assert_eq!(request, Some(KeyAction::Single(Action::RequestCompletion)));
        assert!(editor.inline_completion.suggestion.is_some());
        editor
            .test_execute_action(Action::RequestCompletion)
            .await
            .unwrap();
        assert!(editor.visible_inline_suggestion().is_none());
        assert!(editor.inline_completion.suggestion.is_some());

        let tab = Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let action = editor.handle_event(&tab).unwrap().unwrap();
        assert!(matches!(action, KeyAction::Multiple(ref actions)
            if matches!(actions.first(), Some(Action::ApplyCompletion { item, .. }) if item.label == "foobar")));
        editor
            .test_execute_action(Action::AcceptInlineCompletion)
            .await
            .unwrap();
        assert_eq!(editor.current_buffer().contents(), "foo foobar");
        assert!(editor.inline_completion.suggestion.is_some());

        let close = editor
            .handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Char('e'),
                KeyModifiers::CONTROL,
            )))
            .unwrap();
        assert_eq!(close, Some(KeyAction::Single(Action::CloseDialog)));
        editor
            .test_execute_action(Action::CloseDialog)
            .await
            .unwrap();
        assert_eq!(editor.visible_inline_suggestion().unwrap().insertion, "_ai");
        assert_eq!(
            editor.handle_event(&tab).unwrap(),
            Some(KeyAction::Single(Action::AcceptInlineCompletion))
        );
        editor
            .test_execute_action(Action::AcceptInlineCompletion)
            .await
            .unwrap();
        assert_eq!(editor.current_buffer().contents(), "foo_ai foobar");
    }

    #[tokio::test]
    async fn completion_popup_defers_copilot_until_it_closes() {
        let mut editor = editor("foo foobar");
        let (bridge, requests, _controls, _events) = Bridge::test_channels();
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
        editor.schedule_inline_completion();
        expire_inline_schedule(&mut editor);
        assert!(editor.request_completion(None).await.unwrap());

        editor.service_inline_completion();
        assert!(requests.borrow().is_none());
        assert!(editor.inline_completion.scheduled.is_some());
        let key = Event::Key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        assert_eq!(
            editor.handle_event(&key).unwrap(),
            Some(KeyAction::Single(Action::InsertCharAtCursorPos('b')))
        );
        editor
            .test_execute_action(Action::InsertCharAtCursorPos('b'))
            .await
            .unwrap();
        expire_inline_schedule(&mut editor);
        editor.service_inline_completion();
        assert!(requests.borrow().is_none());

        editor
            .test_execute_action(Action::CloseDialog)
            .await
            .unwrap();
        editor.service_inline_completion();
        let request = requests.borrow().clone().unwrap();
        assert!(request.automatic);
        assert_eq!(request.contents, "foob foobar");
        assert_eq!(request.snapshot, editor.inline_snapshot().unwrap());
        assert!(editor.inline_completion.scheduled.is_none());
    }

    #[tokio::test]
    async fn inflight_copilot_result_waits_for_completion_popup() {
        let mut editor = editor("foo foobar");
        let (bridge, requests, mut controls, events) = Bridge::test_channels();
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
        assert!(editor.request_completion(None).await.unwrap());
        assert_eq!(editor.inline_completion.requested.as_ref(), Some(&snapshot));

        events
            .send(CopilotEvent::Completion {
                snapshot,
                items: vec![item("foo_ai", 0, 3)],
            })
            .await
            .unwrap();
        editor.service_inline_completion();
        assert!(editor.inline_completion.suggestion.is_some());
        assert!(editor.visible_inline_suggestion().is_none());
        assert!(controls.try_recv().is_err());

        editor
            .test_execute_action(Action::CloseDialog)
            .await
            .unwrap();
        editor.service_inline_completion();
        assert_eq!(editor.visible_inline_suggestion().unwrap().insertion, "_ai");
        assert!(matches!(controls.try_recv(), Ok(Control::Shown(_))));
    }

    #[tokio::test]
    async fn empty_completion_popup_does_not_block_copilot() {
        let mut editor = editor("foo");
        let (bridge, requests, _controls, _events) = Bridge::test_channels();
        editor.inline_completion.bridge = Some(bridge);
        editor.inline_completion.failed = false;
        show(&mut editor, "_ai", 3).await;
        show_popup(&mut editor, "unrelated");
        assert!(editor
            .current_dialog
            .as_ref()
            .unwrap()
            .is_empty_completion());
        assert!(editor.visible_inline_suggestion().is_some());

        editor.schedule_inline_completion();
        expire_inline_schedule(&mut editor);
        editor.service_inline_completion();
        assert!(requests.borrow().is_some());
        show(&mut editor, "_ai", 3).await;
        editor
            .test_execute_action(Action::AcceptInlineCompletion)
            .await
            .unwrap();
        assert_eq!(editor.current_buffer().contents(), "foo_ai");
        assert!(editor.current_dialog.is_none());
        assert!(editor.completion_snapshot.is_none());
    }

    #[tokio::test]
    async fn empty_ordinary_completion_does_not_cancel_copilot() {
        let mut editor = editor("foo");
        let (bridge, requests, _controls, _events) = Bridge::test_channels();
        editor.inline_completion.bridge = Some(bridge);
        editor.inline_completion.failed = false;
        editor.config.completion.buffer_words = false;
        editor
            .test_execute_action(Action::EnterMode(Mode::Insert))
            .await
            .unwrap();
        editor
            .test_execute_action(Action::SetCursor(3, 0))
            .await
            .unwrap();
        editor.schedule_inline_completion();
        expire_inline_schedule(&mut editor);
        assert!(!editor.request_completion(None).await.unwrap());
        assert!(editor.inline_completion.scheduled.is_some());
        editor.service_inline_completion();
        assert!(requests.borrow().is_some());
        assert!(editor.current_dialog.is_none());
    }
}
