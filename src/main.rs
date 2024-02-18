use std::{fs, io::stdout, panic, path::Path};

use buffer::Buffer;
use config::Config;
use crossterm::{terminal, ExecutableCommand};
pub use editor::Editor;
use logger::Logger;
use lsp::LspClient;

mod buffer;
mod config;
mod editor;
mod editor_builder;
mod highlighter;
mod logger;
mod lsp;
mod theme;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    #[allow(deprecated)]
    let config_file = std::env::home_dir()
        .unwrap()
        .join(".config/red/config.toml");
    let config_file = Path::new(&config_file);
    if !config_file.exists() {
        eprintln!("Config file {} not found", config_file.display());
        std::process::exit(1);
    }

    let toml = fs::read_to_string(config_file)?;
    let config: Config = toml::from_str(&toml)?;

    let mut lsp = LspClient::start().await?;
    lsp.initialize().await?;

    let file = std::env::args().nth(1);
    let buffer = Buffer::from_file(&mut lsp, file.clone()).await?;

    let theme_file = Path::new(&config.theme);
    if !theme_file.exists() {
        eprintln!("Theme file {} not found", config.theme);
        std::process::exit(1);
    }
    let theme = theme::parse_vscode_theme(&config.theme)?;
    let mut editor = Editor::new(Some(lsp), config, theme, buffer)?;

    panic::set_hook(Box::new(|info| {
        _ = stdout().execute(terminal::LeaveAlternateScreen);
        _ = terminal::disable_raw_mode();

        eprintln!("{}", info);
    }));

    editor.run().await?;
    editor.cleanup()
}
