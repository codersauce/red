//! Modal terminal UI components hosted above the editor and plugin surfaces.
//!
//! [`Component`] defines drawing, event handling, resizing, theme updates, cursor
//! placement, and optional passthrough for one active dialog-like surface. Components
//! return editor [`KeyAction`] values instead of mutating the
//! editor directly. Sensitive components must report their input status so tracing and
//! logging do not serialize secrets.

mod action_bar;
mod agent_composer;
mod completion;
mod confirmation;
mod copilot_signin;
mod diagnostic_info;
mod dialog;
mod file_picker;
mod geometry;
mod hover_info;
mod icons;
mod info;
mod inline_assist;
mod inline_history;
mod input_prompt;
mod keyboard_shortcuts;
mod keymap_hints;
mod learn;
mod list;
mod messages;
mod picker;
mod prompt_buffer;
mod rich_text;
mod selection;
mod shortcut_catalog;
mod spinner;
mod statusline_layout;
mod whats_new;

pub(crate) use action_bar::ActionMenu;
pub use action_bar::{
    ActionBar, ActionBarLayout, ActionBarRole, ActionBarSpan, ActionMode, ActionPriority, UiAction,
};
pub(crate) use agent_composer::wrap_text;
pub use agent_composer::AgentComposer;
pub use completion::CompletionUI;
pub use confirmation::{Confirmation, ConfirmationOptions, ConfirmationSegment};
pub(crate) use copilot_signin::{CopilotSignInDialog, CopilotSignInModel, CopilotSignInPhase};
use crossterm::event::{Event, KeyCode, MouseEvent, MouseEventKind};
pub use diagnostic_info::DiagnosticInfo;
use dialog::Dialog;
pub use file_picker::FilePicker;
pub use geometry::OverlayLayout;
pub(crate) use geometry::ScreenRect;
pub use hover_info::{HoverInfo, HoverInfoFormat};
pub(crate) use icons::IconCatalog;
pub use info::Info;
pub use inline_assist::{InlineAssistPopup, InlineAssistPopupState};
pub(crate) use inline_history::InlineHistoryPanel;
pub use input_prompt::InputPrompt;
pub(crate) use keyboard_shortcuts::{
    is_keyboard_shortcuts_alias, KeyboardShortcuts, ShortcutEntry, ShortcutEvent,
    ShortcutHelpRegion, ShortcutTarget,
};
pub(crate) use keymap_hints::draw_keymap_hints;
pub(crate) use learn::{draw_learn_coach, CoachLayout, LearnHub};
use list::List;
pub(crate) use messages::{MessageRow, MessagesPanel, MessagesView};
pub(crate) use picker::MAX_UNFOCUSED_PREVIEW_BYTES;
pub use picker::{
    LegacyPickerOptions, Picker, PickerIcon, PickerItem, PickerOptions, PickerPresentation,
    PickerPreview, PickerUpdate,
};
pub(crate) use prompt_buffer::{
    first_prompt_line, normalize_prompt_newlines, PromptBuffer, PromptInput, PromptKeyPolicy,
    PROMPT_MAX_BYTES,
};
pub(crate) use rich_text::paint_rich_text;
pub(crate) use selection::{FollowTailViewport, SelectionViewport};
pub(crate) use shortcut_catalog::{
    common_shortcut_entries, picker_reference_actions, prompt_reference_actions, reference_actions,
    surface_reference_actions,
};
pub(crate) use spinner::{spinner_frame, SPINNER_FRAME_INTERVAL_MS};
pub use statusline_layout::StatuslineLayoutPanel;
pub use whats_new::WhatsNewPanel;

use crate::{
    config::KeyAction,
    editor::{Action, Mode, RenderBuffer},
    lsp::types::CompletionResponseItem,
    plugin::{ComposerHandle, PickerHandle},
    theme::Theme,
};

pub trait Component: Send {
    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()>;

    /// Uses the whole terminal's editor area rather than the active split.
    fn uses_full_editor_viewport(&self) -> bool {
        false
    }

    fn is_message_history(&self) -> bool {
        false
    }

    /// Whether this component currently owns a visible keyboard-help context.
    /// Hidden, pass-through popups can leave help with the underlying editor.
    fn has_shortcut_context(&self) -> bool {
        true
    }

    /// User-facing name used by contextual keyboard help.
    fn shortcut_context(&self) -> &str {
        "Dialog"
    }

    /// Actions for the surface-local F1 menu. The default keeps passive popups unchanged.
    fn surface_actions(&self) -> Vec<UiAction> {
        Vec::new()
    }

    fn activate_surface_action(&mut self, id: &str) -> Option<KeyAction> {
        let event = self
            .surface_actions()
            .iter()
            .find(|action| action.id == id && action.enabled)?
            .event()?;
        self.handle_event(&event)
    }

    fn tick(&mut self) -> anyhow::Result<bool> {
        Ok(false)
    }

    fn update_picker(&mut self, _id: i32, _update: PickerUpdate) -> bool {
        false
    }

    fn update_completion(&mut self, _items: Vec<CompletionResponseItem>, _filter: &str) -> bool {
        false
    }

    fn picker_id(&self) -> Option<i32> {
        None
    }

    fn picker_handle(&self) -> Option<PickerHandle> {
        None
    }

    fn composer_handle(&self) -> Option<ComposerHandle> {
        None
    }

    fn resize(&mut self, _viewport_width: usize, _viewport_height: usize) -> bool {
        false
    }

    fn update_overlay_layout(&mut self, _layout: OverlayLayout) -> bool {
        false
    }

    fn set_theme(&mut self, _theme: &Theme) {}

    fn handle_event(&mut self, ev: &Event) -> Option<crate::config::KeyAction> {
        match ev {
            Event::Key(event) => match event.code {
                KeyCode::Esc => Some(KeyAction::Single(Action::CloseDialog)),
                _ => None,
            },
            Event::Mouse(ev) => {
                let MouseEvent { kind, .. } = ev;
                match kind {
                    MouseEventKind::Down(_) => Some(KeyAction::Single(Action::CloseDialog)),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn allows_event_passthrough(&self) -> bool {
        false
    }

    fn is_sensitive_input(&self) -> bool {
        false
    }

    fn cursor_position(&self) -> Option<(usize, usize)> {
        None
    }

    fn cursor_mode(&self) -> Option<Mode> {
        None
    }
}
