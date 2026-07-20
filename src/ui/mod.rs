mod agent_composer;
mod completion;
mod dialog;
mod file_picker;
mod info;
mod input_prompt;
mod keymap_hints;
mod list;
mod picker;

pub use agent_composer::AgentComposer;
pub(crate) use agent_composer::{normalize_newlines, wrap_text};
pub use completion::CompletionUI;
use crossterm::event::{Event, KeyCode, MouseEvent, MouseEventKind};
use dialog::Dialog;
pub use file_picker::FilePicker;
pub use info::Info;
pub use input_prompt::InputPrompt;
pub(crate) use keymap_hints::draw_keymap_hints;
use list::List;
pub use picker::{
    LegacyPickerOptions, Picker, PickerIcon, PickerItem, PickerOptions, PickerPresentation,
    PickerPreview, PickerUpdate,
};

use crate::{
    config::KeyAction,
    editor::{Action, RenderBuffer},
    theme::Theme,
};

pub trait Component: Send {
    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()>;

    fn tick(&mut self) -> anyhow::Result<bool> {
        Ok(false)
    }

    fn update_picker(&mut self, _id: i32, _update: PickerUpdate) -> bool {
        false
    }

    fn picker_id(&self) -> Option<i32> {
        None
    }

    fn resize(&mut self, _viewport_width: usize, _viewport_height: usize) -> bool {
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
}
