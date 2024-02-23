mod dialog;
mod file_picker;
mod info;
mod list;

use crossterm::event::{Event, KeyCode, MouseEvent, MouseEventKind};
use dialog::Dialog;
pub use file_picker::FilePicker;
pub use info::Info;
use list::List;

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
            Event::Mouse(ev) => match ev {
                MouseEvent { kind, .. } => match kind {
                    MouseEventKind::Down(_) => Some(KeyAction::Single(Action::CloseDialog)),
                    _ => None,
                },
            },
            _ => None,
        }
    }

    fn cursor_position(&self) -> Option<(u16, u16)> {
        None
    }
}
