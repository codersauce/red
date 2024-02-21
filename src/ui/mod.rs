mod dialog;
mod file_picker;
mod list;

use crossterm::event;
use dialog::Dialog;
pub use file_picker::FilePicker;
use list::List;

use crate::{config::KeyAction, editor::RenderBuffer};

pub trait Component: Send {
    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()>;

    fn handle_event(&mut self, _ev: &event::Event) -> Option<KeyAction> {
        None
    }

    fn current_position(&self) -> Option<(u16, u16)> {
        None
    }
}
