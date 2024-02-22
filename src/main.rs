use std::{fs, io::stdout, panic, path::Path};

use buffer::Buffer;
use config::Config;
use crossterm::{terminal, ExecutableCommand};
use editor::Editor;
use lsp::LspClient;

mod buffer;
mod command;
mod config;
mod editor;
mod highlighter;
mod logger;
mod lsp;
mod theme;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let log_file = crate::logger::init()?;
    println!("Log file located at {log_file:?}");

    #[allow(deprecated)]
    let config_path = std::env::home_dir().unwrap().join(".config/red");

    let config_file = config_path.join("config.toml");
    let config_file = Path::new(&config_file);
    if !config_file.exists() {
        eprintln!("Config file {} not found", config_file.display());
        std::process::exit(1);
    }

    let toml = fs::read_to_string(config_file)?;
    let config: Config = toml::from_str(&toml)?;

    let mut lsp = LspClient::start().await?;
    lsp.initialize().await?;

    let files = std::env::args();
    let mut buffers = Vec::new();
    for file in files.skip(1) {
        let buffer = Buffer::from_file(&mut lsp, Some(file)).await?;
        buffers.push(buffer);
    }

    let theme_file = config_path.join("themes").join(&config.theme);
    if !theme_file.exists() {
        eprintln!("Theme file {} not found", config.theme);
        std::process::exit(1);
    }
    let theme = theme::parse_vscode_theme(&theme_file.to_string_lossy())?;
    let mut editor = Editor::new(lsp, config, theme, buffers)?;

    panic::set_hook(Box::new(|info| {
        _ = stdout().execute(terminal::LeaveAlternateScreen);
        _ = terminal::disable_raw_mode();

        eprintln!("{}", info);
    }));

    editor.run().await?;
    editor.cleanup()
}
