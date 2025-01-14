mod completion;
mod dialog;
mod file_picker;
mod info;
mod list;
mod picker;

pub use completion::CompletionUI;
use crossterm::event::{Event, KeyCode, MouseEvent, MouseEventKind};
use dialog::Dialog;
pub use file_picker::FilePicker;
pub use info::Info;
use list::List;
pub use picker::Picker;

use crate::{
    config::KeyAction,
    editor::{Action, RenderBuffer},
};

pub trait Component: Send {
    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()>;

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

    fn cursor_position(&self) -> Option<(usize, usize)> {
        None
    }
}
