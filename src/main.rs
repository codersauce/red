use std::{
    fs,
    io::{stdout, Write as _},
    panic,
};

use clap::Parser as _;
use crossterm::{event, terminal, ExecutableCommand};

use red::assets;
use red::buffer::Buffer;
use red::cli::Args;
use red::config::Config;
use red::editor::Editor;
use red::logger::Logger;
use red::lsp::{LspClient, LspManager};
use red::onboarding;
use red::preferences::PreferencesStore;
use red::theme::{parse_vscode_theme, parse_vscode_theme_contents, Theme};
use red::{log, LOGGER};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    args.validate_utility_args()?;

    if args.runtime_files {
        print!("{}", assets::format_runtime_files(&Config::config_dir())?);
        return Ok(());
    }

    if let Some(asset) = args.eject.as_deref().or(args.eject_force.as_deref()) {
        let target =
            assets::eject_runtime_asset(asset, &Config::config_dir(), args.eject_force.is_some())?;
        println!("Ejected {}", target.display());
        return Ok(());
    }

    let config_file = Config::path("config.toml");
    if !config_file.exists() {
        let config_dir = config_file
            .parent()
            .expect("config path always has a parent directory");
        onboarding::run(config_dir)?;
    }

    let toml = fs::read_to_string(&config_file).unwrap_or_default();
    let mut config = Config::from_user_toml_with_overrides(&toml, &args.config_overrides)?;

    if let Some(log_file) = &config.log_file {
        LOGGER.get_or_init(|| Some(Logger::new(log_file)));
    } else {
        LOGGER.get_or_init(|| None);
    }
    let preferences = PreferencesStore::load(Config::path("preferences.json"));

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

    let theme = load_theme(&config.theme)?;
    let mut editor = Editor::new_with_preferences(lsp, config, theme, buffers, preferences)?;

    panic::set_hook(Box::new(|info| {
        let mut stdout = stdout();
        _ = write!(stdout, "\x1b]112\x1b\\");
        _ = stdout.execute(event::DisableFocusChange);
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

fn load_theme(theme_name: &str) -> anyhow::Result<Theme> {
    let Some(theme_asset) = assets::resolve_theme(theme_name, &Config::config_dir()) else {
        anyhow::bail!("Theme file {} not found", theme_name);
    };

    if let Some(path) = theme_asset.path() {
        parse_vscode_theme(&path.to_string_lossy())
    } else {
        parse_vscode_theme_contents(&theme_asset.read_to_string()?)
    }
}
