//! Core library for Red's terminal editor, embedded plugin runtime, and persistent services.
//!
//! The interactive binary in `main.rs` assembles these modules, while this crate owns
//! the reusable state machines behind editing, rendering, language servers, plugins,
//! agent proposals, recovery, and detachable sessions. Most state is coordinated by
//! [`editor::Editor`] on one async task. Background processes and blocking persistence
//! work communicate with that owner through bounded channels or explicit join handles.
//!
//! Red currently exports a broad implementation-facing surface so the binary and
//! integration harnesses can share the same code. Visibility does not by itself promise
//! that every module is a stable third-party API; versioned compatibility commitments
//! are called out explicitly at boundaries such as the plugin host protocol.

#![recursion_limit = "256"]

pub mod agent_check;
pub mod agent_tools;
pub mod agent_workspace;
pub mod assets;
pub mod buffer;
pub mod cli;
pub mod clipboard;
pub mod codex;
pub mod color;
pub mod command;
pub mod command_palette;
pub mod config;
pub mod dispatcher;
pub mod editor;
pub mod headless;
pub mod highlighter;
pub mod logger;
pub mod lsp;
pub mod matchit;
pub mod onboarding;
pub mod plugin;
pub mod preferences;
mod self_check;
pub mod session;
pub mod splash;
pub mod sync;
pub mod theme;
pub mod ui;
pub mod undo;
pub mod unicode_utils;
pub mod utils;
pub mod window;

// Test utilities for integration tests
#[doc(hidden)]
pub mod test_utils;

use once_cell::sync::OnceCell;

pub use logger::Logger;
pub use lsp::{LspManager, RealLspClient};
#[doc(hidden)]
pub use self_check::{run as run_self_check, SelfCheckReport};

#[allow(unused)]
pub static LOGGER: OnceCell<Option<Logger>> = OnceCell::new();

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        {
            if let Some(logger) = $crate::LOGGER
                .get_or_init(|| $crate::Logger::try_new("red.log").ok())
                .as_ref()
            {
                let log_message = format!($($arg)*);
                logger.log(&log_message);
            }
        }
    };
}
