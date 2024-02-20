use std::{fs, io::stdout, panic, path::{Path, PathBuf}};

use buffer::Buffer;
use config::Config;
use crossterm::{terminal, ExecutableCommand};
use editor::Editor;
use logger::Logger;
use lsp::LspClient;
use once_cell::sync::OnceCell;
use theme::Theme;

mod buffer;
mod config;
mod editor;
mod highlighter;
mod logger;
mod lsp;
mod theme;

#[allow(unused)]
static LOGGER: OnceCell<Option<Logger>> = OnceCell::new();

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

fn get_config(path: &PathBuf) -> anyhow::Result<Config> {
    let mut default_config = Config::default();
    let config_file = path.join("config.toml");
    let config_file = Path::new(&config_file);

    match config_file.exists() {
        false => {
            let config_contents = toml::to_string(&default_config)?;
            fs::write(config_file, &config_contents[..])?;

            Ok(default_config)
        },
        true => {
            let toml = fs::read_to_string(config_file)?;
            let config: Config = toml::from_str(&toml)?;
            
            default_config.extend(config);

            Ok(default_config)
        }
    }
}

fn get_theme(config_path: &PathBuf, config: &Config) -> anyhow::Result<Theme> {
    let theme_file = config_path.join("themes").join(&config.theme);
    if !theme_file.exists() {
        eprintln!("Theme file {} not found", config.theme);
        std::process::exit(1);
    }

    let theme = theme::parse_vscode_theme(&theme_file.to_string_lossy())?;
    Ok(theme)
}

fn init_logger(config: &Config) {
    if let Some(log_file) = &config.log_file {
        LOGGER.get_or_init(|| Some(Logger::new(log_file)));
    } else {
        LOGGER.get_or_init(|| None);
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    #[allow(deprecated)]
    let config_path = std::env::home_dir().unwrap().join(".config/red");
    let config = get_config(&config_path)?;
    let theme = get_theme(&config_path, &config)?;

    init_logger(&config);

    let mut lsp = LspClient::start().await?;
    lsp.initialize().await?;

    let file = std::env::args().nth(1);
    let buffer = Buffer::from_file(&mut lsp, file.clone()).await?;
    let mut editor = Editor::new(lsp, config, theme, buffer)?;

    panic::set_hook(Box::new(|info| {
        _ = stdout().execute(terminal::LeaveAlternateScreen);
        _ = terminal::disable_raw_mode();

        eprintln!("{}", info);
    }));

    editor.run().await?;
    editor.cleanup()
}
