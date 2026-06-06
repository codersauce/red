use std::{
    fs,
    io::{stdout, Write as _},
    panic,
};

use clap::Parser as _;
use crossterm::{terminal, ExecutableCommand};

use red::buffer::Buffer;
use red::cli::Args;
use red::config::Config;
use red::editor::Editor;
use red::logger::Logger;
use red::lsp::{LspClient, LspManager};
use red::theme::parse_vscode_theme;
use red::{log, LOGGER};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let config_file = Config::path("config.toml");
    if !config_file.exists() {
        eprintln!("Config file {} not found", config_file.display());
        std::process::exit(1);
    }

    let toml = fs::read_to_string(config_file)?;
    let mut config: Config = toml::from_str(&toml)?;

    if let Some(log_file) = &config.log_file {
        LOGGER.get_or_init(|| Some(Logger::new(log_file)));
    } else {
        LOGGER.get_or_init(|| None);
    }

    let args = Args::parse();
    config.startup_file_count = args.files.len();

    if let Some(root) = args.root {
        // change to root directory
        std::env::set_current_dir(root)?;
    }

    let lsp = Box::new(LspManager::new(config.lsp.clone())) as Box<dyn LspClient>;

    let mut buffers = Vec::new();
    if args.files.is_empty() {
        let buffer = Buffer::new(None, String::new());
        buffers.push(buffer);
    } else {
        for file in args.files {
            let buffer = Buffer::from_file(Some(file)).await?;
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
        let mut stdout = stdout();
        _ = write!(stdout, "\x1b]112\x1b\\");
        _ = stdout.execute(terminal::LeaveAlternateScreen);
        _ = terminal::disable_raw_mode();

        eprintln!("{}", info);
    }));

    let result = editor.run().await;

    log!(" ===> after run, shutting down LSP");
    if let Err(e) = editor.lsp_mut().shutdown().await {
        log!("Error shutting down LSP: {}", e);
    }

    editor.cleanup()?;
    result?;

    Ok(())
}
