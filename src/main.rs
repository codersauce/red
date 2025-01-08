use std::{fs, io::stdout, panic};

use crossterm::{terminal, ExecutableCommand};

use red::buffer::Buffer;
use red::config::Config;
use red::editor::Editor;
use red::logger::Logger;
use red::lsp::{start_lsp, LspClient};
use red::theme::parse_vscode_theme;
use red::LOGGER;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_file = Config::path("config.toml");
    if !config_file.exists() {
        eprintln!("Config file {} not found", config_file.display());
        std::process::exit(1);
    }

    let toml = fs::read_to_string(config_file)?;
    let config: Config = toml::from_str(&toml)?;

    if let Some(log_file) = &config.log_file {
        LOGGER.get_or_init(|| Some(Logger::new(log_file)));
    } else {
        LOGGER.get_or_init(|| None);
    }

    let mut lsp = Box::new(start_lsp().await?) as Box<dyn LspClient>;
    lsp.initialize().await?;

    let files = std::env::args();
    let mut buffers = Vec::new();

    if files.len() < 2 {
        let buffer = Buffer::new(None, String::new());
        buffers.push(buffer);
    } else {
        for file in files.skip(1) {
            let buffer = Buffer::from_file(&mut lsp, Some(file)).await?;
            buffers.push(buffer);
        }
    }

    let theme_file = &Config::path("themes").join(&config.theme);
    if !theme_file.exists() {
        eprintln!("Theme file {} not found", config.theme);
        std::process::exit(1);
    }
    let theme = parse_vscode_theme(&theme_file.to_string_lossy())?;
    let mut editor = Editor::new(lsp, config, theme, buffers)?;

    panic::set_hook(Box::new(|info| {
        _ = stdout().execute(terminal::LeaveAlternateScreen);
        _ = terminal::disable_raw_mode();

        eprintln!("{}", info);
    }));

    editor.run().await?;
    editor.cleanup()
}
