use crate::editor::Action;

mod registry;
mod runtime;

pub use registry::PluginRegistry;
pub use runtime::Runtime;

pub enum PluginMessage {
    Action(Action),
}
