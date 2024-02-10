use std::{fs, io::stdout, panic};

use buffer::Buffer;
use config::Config;
use crossterm::{terminal, ExecutableCommand};
use editor::Editor;
use logger::Logger;
use once_cell::sync::OnceCell;

mod buffer;
mod config;
mod editor;
mod logger;
mod theme;

#[allow(unused)]
static LOGGER: OnceCell<Logger> = OnceCell::new();

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        {
            let log_message = format!($($arg)*);
            $crate::LOGGER.get_or_init(|| $crate::Logger::new("red.log")).log(&log_message);
        }
    };
}

fn main() -> anyhow::Result<()> {
    let toml = fs::read_to_string("config.toml")?;
    let config: Config = toml::from_str(&toml)?;

    let file = std::env::args().nth(1);
    let buffer = Buffer::from_file(file);

    let theme = theme::parse_vscode_theme(&config.theme)?;
    let mut editor = Editor::new(config, theme, buffer)?;

    panic::set_hook(Box::new(|info| {
        _ = stdout().execute(terminal::LeaveAlternateScreen);
        _ = terminal::disable_raw_mode();

        eprintln!("{}", info);
    }));

    editor.run()?;
    editor.cleanup()
}
