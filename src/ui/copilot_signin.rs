//! Stateful GitHub Copilot device-flow dialog.

use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

use crossterm::event::{Event, KeyCode, KeyModifiers};
use serde_json::Value;

use crate::{
    config::KeyAction,
    editor::{Action, Editor, RenderBuffer},
    theme::{Style, Theme},
    unicode_utils::{display_width, truncate_display_width},
};

use super::{
    agent_composer::wrap_text,
    dialog::{BorderStyle, Dialog, SurfaceRole},
    spinner_frame, Component, SPINNER_FRAME_INTERVAL_MS,
};

const BUTTON_GAP: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CopilotSignInPhase {
    Ready,
    Waiting,
    Requesting,
    Failed(String),
}

#[derive(Debug, Clone)]
pub(crate) struct CopilotSignInModel {
    pub user_code: String,
    pub command: Value,
    pub phase: CopilotSignInPhase,
    pub clipboard_copied: bool,
}

pub(crate) struct CopilotSignInDialog {
    dialog: Dialog,
    model: Arc<Mutex<CopilotSignInModel>>,
    accept_selected: bool,
    spinner_started: Option<Instant>,
    spinner_frame: u64,
    style: Style,
    theme: Theme,
}

impl CopilotSignInDialog {
    pub(crate) fn new(editor: &Editor, model: Arc<Mutex<CopilotSignInModel>>) -> Self {
        let style = editor.theme.ui_style.dialog.clone();
        let (width, height) = dialog_size(editor.vwidth(), editor.vheight());
        let x = editor.vwidth().saturating_sub(width + 2) / 2;
        let y = editor.vheight().saturating_sub(height + 2) / 2;
        let spinner_started = model.lock().ok().and_then(|model| {
            matches!(
                model.phase,
                CopilotSignInPhase::Waiting | CopilotSignInPhase::Requesting
            )
            .then(Instant::now)
        });
        Self {
            dialog: Dialog::new(
                Some("Sign in to GitHub Copilot".into()),
                x,
                y,
                width,
                height,
                &style,
                BorderStyle::Single,
                &editor.theme,
            )
            .with_surface_theme(&editor.theme, SurfaceRole::Dialog),
            model,
            accept_selected: true,
            spinner_started,
            spinner_frame: 0,
            style,
            theme: editor.theme.clone(),
        }
    }

    fn snapshot(&self) -> CopilotSignInModel {
        self.model
            .lock()
            .map(|model| model.clone())
            .unwrap_or_else(|_| CopilotSignInModel {
                user_code: "Unavailable".into(),
                command: Value::Null,
                phase: CopilotSignInPhase::Failed("Sign-in state is unavailable".into()),
                clipboard_copied: false,
            })
    }

    fn message(model: &CopilotSignInModel, spinner: &str) -> String {
        match &model.phase {
            CopilotSignInPhase::Ready if model.clipboard_copied => format!(
                "Code {} was copied to the clipboard. Open GitHub and paste it to continue.",
                model.user_code
            ),
            CopilotSignInPhase::Ready => format!(
                "Enter code {} on GitHub's device activation page. Clipboard copy is unavailable.",
                model.user_code
            ),
            CopilotSignInPhase::Waiting => format!(
                "{spinner} Waiting for GitHub authorization...\n\nCode: {}",
                model.user_code
            ),
            CopilotSignInPhase::Requesting => {
                format!("{spinner} Requesting a new device code from GitHub Copilot...")
            }
            CopilotSignInPhase::Failed(error) => format!(
                "Sign-in failed: {error}\n\nThe previous code was {}. Try again for a new code.",
                model.user_code
            ),
        }
    }

    fn labels(model: &CopilotSignInModel) -> (&'static str, &'static str) {
        match model.phase {
            CopilotSignInPhase::Ready => ("Open GitHub", "Dismiss"),
            CopilotSignInPhase::Waiting => ("Copy again", "Dismiss"),
            CopilotSignInPhase::Requesting => ("Requesting...", "Dismiss"),
            CopilotSignInPhase::Failed(_) => ("Try again", "Dismiss"),
        }
    }

    fn accept(&mut self) -> Option<KeyAction> {
        let mut model = self.model.lock().ok()?;
        match model.phase {
            CopilotSignInPhase::Ready => {
                model.phase = CopilotSignInPhase::Waiting;
                self.spinner_started = Some(Instant::now());
                self.spinner_frame = 0;
                Some(KeyAction::Single(Action::CopilotFinishSignIn(
                    model.command.clone(),
                )))
            }
            CopilotSignInPhase::Waiting => Some(KeyAction::Single(Action::CopilotCopySignInCode(
                model.user_code.clone(),
            ))),
            CopilotSignInPhase::Requesting => None,
            CopilotSignInPhase::Failed(_) => {
                model.phase = CopilotSignInPhase::Requesting;
                self.spinner_started = Some(Instant::now());
                self.spinner_frame = 0;
                Some(KeyAction::Single(Action::CopilotRetrySignIn))
            }
        }
    }

    fn dismiss_action() -> KeyAction {
        KeyAction::Multiple(vec![Action::CopilotDismissSignIn, Action::CloseDialog])
    }
}

impl Component for CopilotSignInDialog {
    fn set_theme(&mut self, theme: &Theme) {
        self.style = theme.ui_style.dialog.clone();
        self.dialog.apply_surface_theme(theme, SurfaceRole::Dialog);
        self.theme = theme.clone();
    }

    fn resize(&mut self, viewport_width: usize, viewport_height: usize) -> bool {
        (self.dialog.width, self.dialog.height) = dialog_size(viewport_width, viewport_height);
        self.dialog.x = viewport_width.saturating_sub(self.dialog.width + 2) / 2;
        self.dialog.y = viewport_height.saturating_sub(self.dialog.height + 2) / 2;
        true
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.dialog.draw(buffer)?;
        let model = self.snapshot();
        let spinner = self
            .spinner_started
            .map(|started| spinner_frame(started.elapsed().as_millis() as u64))
            .unwrap_or_default();
        let message = Self::message(&model, spinner);
        for (offset, row) in wrap_text(&message, self.dialog.width)
            .rows
            .iter()
            .take(self.dialog.height.saturating_sub(1))
            .enumerate()
        {
            buffer.set_text(
                self.dialog.x + 1,
                self.dialog.y + 1 + offset,
                &truncate_display_width(row, self.dialog.width),
                &self.style,
            );
        }

        let (accept, cancel) = Self::labels(&model);
        let accept = format!("[ {accept} ]");
        let cancel = format!("[ {cancel} ]");
        let buttons_width = display_width(&accept) + BUTTON_GAP + display_width(&cancel);
        let button_x = self.dialog.x + 1 + self.dialog.width.saturating_sub(buttons_width) / 2;
        let button_y = self.dialog.y + self.dialog.height;
        let selected = self.theme.selected_style(
            &self.style,
            &self.theme.ui_style.picker_selected_item,
            crate::theme::SelectionForegroundPriority::Selection,
        );
        buffer.set_text(
            button_x,
            button_y,
            &accept,
            if self.accept_selected {
                &selected
            } else {
                &self.style
            },
        );
        buffer.set_text(
            button_x + display_width(&accept) + BUTTON_GAP,
            button_y,
            &cancel,
            if self.accept_selected {
                &self.style
            } else {
                &selected
            },
        );
        Ok(())
    }

    fn handle_event(&mut self, event: &Event) -> Option<KeyAction> {
        let Event::Key(key) = event else {
            return None;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                Some(Self::dismiss_action())
            }
            (KeyCode::Left | KeyCode::BackTab | KeyCode::Char('h') | KeyCode::Char('k'), _) => {
                self.accept_selected = true;
                Some(KeyAction::Single(Action::Refresh))
            }
            (KeyCode::Right | KeyCode::Tab | KeyCode::Char('j') | KeyCode::Char('l'), _) => {
                self.accept_selected = false;
                Some(KeyAction::Single(Action::Refresh))
            }
            (KeyCode::Char('c'), KeyModifiers::NONE) => {
                let code = self.model.lock().ok()?.user_code.clone();
                Some(KeyAction::Single(Action::CopilotCopySignInCode(code)))
            }
            (KeyCode::Char('y' | 'Y'), _) => self.accept(),
            (KeyCode::Enter, _) if self.accept_selected => self.accept(),
            (KeyCode::Enter, _) => Some(Self::dismiss_action()),
            _ => None,
        }
    }

    fn tick(&mut self) -> anyhow::Result<bool> {
        let busy = self.model.lock().is_ok_and(|model| {
            matches!(
                model.phase,
                CopilotSignInPhase::Waiting | CopilotSignInPhase::Requesting
            )
        });
        if !busy {
            self.spinner_started = None;
            self.spinner_frame = 0;
            return Ok(false);
        }
        let started = self.spinner_started.get_or_insert_with(Instant::now);
        let frame = started.elapsed().as_millis() as u64 / SPINNER_FRAME_INTERVAL_MS;
        if frame == self.spinner_frame {
            return Ok(false);
        }
        self.spinner_frame = frame;
        Ok(true)
    }
}

fn dialog_size(viewport_width: usize, viewport_height: usize) -> (usize, usize) {
    (
        60.min(viewport_width.saturating_sub(4)).max(1),
        7.min(viewport_height.saturating_sub(4)).max(2),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{buffer::Buffer, config::Config, lsp::LspManager};
    use crossterm::event::KeyEvent;

    fn editor() -> Editor {
        let config = Config::from_user_toml_with_overrides("", &[]).unwrap();
        Editor::with_size(
            Box::new(LspManager::new(config.lsp.clone())),
            80,
            20,
            config,
            Theme::default(),
            vec![Buffer::new(None, String::new())],
        )
        .unwrap()
    }

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn model(phase: CopilotSignInPhase) -> Arc<Mutex<CopilotSignInModel>> {
        Arc::new(Mutex::new(CopilotSignInModel {
            user_code: "ABCD-EFGH".into(),
            command: serde_json::json!({"command":"github.copilot.finishDeviceFlow"}),
            phase,
            clipboard_copied: true,
        }))
    }

    #[test]
    fn opening_github_keeps_the_dialog_open_and_switches_to_waiting() {
        let state = model(CopilotSignInPhase::Ready);
        let mut dialog = CopilotSignInDialog::new(&editor(), state.clone());

        assert!(matches!(
            dialog.handle_event(&key(KeyCode::Enter)),
            Some(KeyAction::Single(Action::CopilotFinishSignIn(_)))
        ));
        assert_eq!(state.lock().unwrap().phase, CopilotSignInPhase::Waiting);
    }

    #[test]
    fn waiting_dialog_keeps_the_code_visible_and_can_copy_it_again() {
        let state = model(CopilotSignInPhase::Waiting);
        let mut dialog = CopilotSignInDialog::new(&editor(), state);
        let mut buffer = RenderBuffer::new(80, 20, &Style::default());

        dialog.draw(&mut buffer).unwrap();
        let rendered = buffer
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>();
        assert!(rendered.contains("⠋ Waiting for GitHub authorization"));
        assert!(rendered.contains("ABCD-EFGH"), "{rendered:?}");
        assert_eq!(
            dialog.handle_event(&key(KeyCode::Char('c'))),
            Some(KeyAction::Single(Action::CopilotCopySignInCode(
                "ABCD-EFGH".into()
            )))
        );
    }

    #[test]
    fn waiting_spinner_advances_on_the_shared_interval() {
        let state = model(CopilotSignInPhase::Waiting);
        let mut dialog = CopilotSignInDialog::new(&editor(), state);
        dialog.spinner_started =
            Some(Instant::now() - std::time::Duration::from_millis(SPINNER_FRAME_INTERVAL_MS));

        assert!(dialog.tick().unwrap());
        let mut buffer = RenderBuffer::new(80, 20, &Style::default());
        dialog.draw(&mut buffer).unwrap();
        let rendered = buffer
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>();
        assert!(rendered.contains("⠙ Waiting for GitHub authorization"));
    }

    #[test]
    fn failed_sign_in_can_request_a_fresh_code() {
        let state = model(CopilotSignInPhase::Failed("expired".into()));
        let mut dialog = CopilotSignInDialog::new(&editor(), state.clone());

        assert_eq!(
            dialog.handle_event(&key(KeyCode::Enter)),
            Some(KeyAction::Single(Action::CopilotRetrySignIn))
        );
        assert_eq!(state.lock().unwrap().phase, CopilotSignInPhase::Requesting);
    }
}
