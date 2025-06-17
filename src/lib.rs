#![recursion_limit = "256"]

pub mod buffer;
pub mod cli;
pub mod color;
pub mod command;
pub mod config;
pub mod dispatcher;
pub mod editor;
pub mod highlighter;
pub mod logger;
pub mod lsp;
pub mod plugin;
pub mod sync;
pub mod theme;
pub mod ui;
pub mod unicode_utils;
pub mod utils;
pub mod window;

// Test utilities for integration tests
#[doc(hidden)]
pub mod test_utils;

use once_cell::sync::OnceCell;

pub use logger::Logger;
pub use lsp::RealLspClient;

#[allow(unused)]
pub static LOGGER: OnceCell<Option<Logger>> = OnceCell::new();

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        {
            let log_message = format!($($arg)*);
            if let Some(logger) = $crate::LOGGER.get_or_init(|| Some($crate::Logger::new("red.log"))) {
                logger.log(&log_message);
            }
        }
    };
}
