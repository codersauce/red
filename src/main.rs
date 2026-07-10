use std::{
    fs,
    io::{stdout, Write as _},
    panic,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use clap::Parser as _;
use crossterm::{event, style, terminal, ExecutableCommand, QueueableCommand};

use red::assets;
use red::buffer::Buffer;
use red::cli::Args;
use red::config::Config;
use red::editor::Editor;
use red::headless::{InputEvent as DetachedInput, KeyCode as DetachedKeyCode, KeyModifier};
use red::logger::Logger;
use red::lsp::{LspClient, LspManager};
use red::onboarding;
use red::preferences::PreferencesStore;
use red::session::SessionStore;
use red::theme::{parse_vscode_theme, parse_vscode_theme_contents, Theme};
use red::{log, run_self_check, LOGGER};

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    if let Err(error) = run().await {
        print_error(&error);
        std::process::exit(1);
    }
}

async fn run() -> anyhow::Result<()> {
    let args = Args::parse();
    args.validate_utility_args()?;

    if let Some(session) = &args.attach {
        return attach_session(session).await;
    }
    if let Some(session) = &args.stop {
        return stop_session(session).await;
    }
    if let Some(session) = &args.detach {
        let owner_pid = start_detached_owner(&args, session)?;
        wait_for_detached_owner(session, owner_pid).await?;
        return attach_session(session).await;
    }

    if args.process_editor_replace {
        let contents = std::env::var("RED_PROCESS_EDITOR_CONTENT")
            .map_err(|_| anyhow::anyhow!("RED_PROCESS_EDITOR_CONTENT is not set"))?;
        fs::write(&args.files[0], contents)?;
        return Ok(());
    }

    if args.self_check {
        let report = run_self_check().await?;
        println!("{}", report.format());
        println!("red self-check ok");
        return Ok(());
    }

    if args.agent_check {
        let config_file = Config::path("config.toml");
        let toml = fs::read_to_string(config_file).unwrap_or_default();
        let config = Config::from_user_toml_with_overrides(&toml, &args.config_overrides)?;
        println!("{}", red::agent_check::run(&config).format());
        return Ok(());
    }

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
    config.disable_plugin_typecheck = args.no_typecheck;

    if let Some(log_file) = &config.log_file {
        LOGGER.get_or_init(|| Some(Logger::new(log_file)));
    } else {
        LOGGER.get_or_init(|| None);
    }
    let preferences = PreferencesStore::load(Config::path("preferences.json"));

    config.startup_file_count = args.files.len();

    if let Some(root) = &args.root {
        // change to root directory
        std::env::set_current_dir(root)?;
    }

    let session_store = SessionStore::new(Config::path("sessions"));
    let resumed_session = if args.resume {
        let snapshot = session_store.load()?;
        if !snapshot.cwd.is_empty() {
            std::env::set_current_dir(&snapshot.cwd)?;
        }
        Some(snapshot)
    } else {
        None
    };

    let lsp = Box::new(LspManager::new(config.lsp.clone())) as Box<dyn LspClient>;

    let mut buffers = Vec::new();
    if let Some(snapshot) = &resumed_session {
        buffers = Editor::buffers_from_session_snapshot(snapshot);
        anyhow::ensure!(!buffers.is_empty(), "session snapshot contains no buffers");
    } else if args.files.is_empty() {
        let buffer = Buffer::new(None, String::new());
        buffers.push(buffer);
    } else {
        for file in &args.files {
            let buffer = Buffer::from_file(Some(file.clone())).await?;
            buffers.push(buffer);
        }
    }

    let theme = load_theme(&config.theme)?;
    let mut editor = Editor::new_with_preferences(lsp, config, theme, buffers, preferences)?;
    if let Some(snapshot) = &resumed_session {
        for divergence in editor.restore_session_snapshot(snapshot)? {
            eprintln!(
                "Recovered {} with external disk changes:\n{}",
                divergence.path, divergence.diff
            );
        }
    }
    editor.set_session_store(session_store);

    if let Some(session) = &args.core_session {
        #[cfg(unix)]
        {
            let bound = red::headless::bind_session(&Config::path("run"), session)?;
            let core = red::editor::DetachedEditorCore::new(editor).await?;
            return red::headless::serve_editor_session(&bound, core).await;
        }
        #[cfg(not(unix))]
        anyhow::bail!("detach is currently available on Linux and macOS; use --resume on Windows");
    }

    panic::set_hook(Box::new(|info| {
        let mut stdout = stdout();
        _ = write!(stdout, "\x1b]112\x1b\\");
        _ = stdout.execute(event::DisableBracketedPaste);
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

fn start_detached_owner(args: &Args, session: &str) -> anyhow::Result<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;

        anyhow::ensure!(
            !red::headless::session_is_active(&Config::path("run"), session)?,
            "detach session `{session}` is already running; use `red --attach {session}`"
        );
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("--core-session")
            .arg(session)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: `pre_exec` only calls the async-signal-safe `setsid(2)` wrapper. A new
        // session prevents the owner from inheriting the SSH terminal's hangup lifecycle.
        unsafe {
            command.pre_exec(|| {
                nix::unistd::setsid()
                    .map(|_| ())
                    .map_err(std::io::Error::other)
            });
        }
        if let Some(root) = &args.root {
            command.arg("--root").arg(root);
        }
        for config_override in &args.config_overrides {
            command.arg("--config-override").arg(config_override);
        }
        if args.no_typecheck {
            command.arg("--no-typecheck");
        }
        command.args(&args.files);
        Ok(command.spawn()?.id())
    }
    #[cfg(not(unix))]
    {
        let _ = (args, session);
        anyhow::bail!("detach is currently available on Linux and macOS; use --resume on Windows")
    }
}

async fn wait_for_detached_owner(session: &str, owner_pid: u32) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let paths = red::headless::SessionPaths::new(&Config::path("run"), session)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let pid_matches = std::fs::read_to_string(&paths.pid)
                .ok()
                .and_then(|pid| pid.trim().parse::<u32>().ok())
                == Some(owner_pid);
            if paths.socket.exists() && paths.token.exists() && pid_matches {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        anyhow::bail!("detached owner did not create its socket; run red --self-check")
    }
    #[cfg(not(unix))]
    {
        let _ = (session, owner_pid);
        anyhow::bail!("detach is currently available on Linux and macOS; use --resume on Windows")
    }
}

async fn stop_session(session: &str) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        red::headless::stop_session(&Config::path("run"), session).await
    }
    #[cfg(not(unix))]
    {
        let _ = session;
        anyhow::bail!("detach is currently available on Linux and macOS; use --resume on Windows")
    }
}

async fn attach_session(session: &str) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let size = terminal::size().unwrap_or((80, 24));
        let mut client =
            red::headless::connect_session(&Config::path("run"), session, None, size).await?;
        let mut rows = Vec::new();
        terminal::enable_raw_mode()?;
        let terminal_guard = DetachedTerminalGuard;
        let mut output = stdout();
        output
            .execute(event::EnableBracketedPaste)?
            .execute(event::EnableFocusChange)?
            .execute(terminal::EnterAlternateScreen)?
            .execute(terminal::Clear(terminal::ClearType::All))?;
        let result = async {
            paint_detached_delta(&mut output, &mut rows, &client.initial_render)?;
            let mut last_heartbeat = Instant::now();
            loop {
                if event::poll(Duration::from_millis(250))? {
                    match event::read()? {
                        event::Event::Key(key)
                            if key.code == event::KeyCode::Char('\\')
                                && key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                        {
                            client.detach().await?;
                            return Ok(());
                        }
                        event::Event::Resize(columns, rows_count) => {
                            let delta = client.resize(columns, rows_count).await?;
                            paint_detached_delta(&mut output, &mut rows, &delta)?;
                        }
                        event::Event::FocusGained => {
                            let delta = client.focus(/*focused*/ true).await?;
                            paint_detached_delta(&mut output, &mut rows, &delta)?;
                        }
                        event::Event::FocusLost => {
                            let delta = client.focus(/*focused*/ false).await?;
                            paint_detached_delta(&mut output, &mut rows, &delta)?;
                        }
                        event::Event::Paste(text) => {
                            let delta = client.input(DetachedInput::Paste { text }).await?;
                            paint_detached_delta(&mut output, &mut rows, &delta)?;
                        }
                        event::Event::Key(key) => {
                            if let Some(input) = detached_key_input(key) {
                                let delta = client.input(input).await?;
                                paint_detached_delta(&mut output, &mut rows, &delta)?;
                            }
                        }
                        _ => {}
                    }
                }
                if last_heartbeat.elapsed() >= Duration::from_secs(5) {
                    let delta = client.heartbeat().await?;
                    paint_detached_delta(&mut output, &mut rows, &delta)?;
                    last_heartbeat = Instant::now();
                }
            }
        }
        .await;
        drop(terminal_guard);
        result
    }
    #[cfg(not(unix))]
    {
        let _ = session;
        anyhow::bail!("detach is currently available on Linux and macOS; use --resume on Windows")
    }
}

struct DetachedTerminalGuard;

impl Drop for DetachedTerminalGuard {
    fn drop(&mut self) {
        let mut output = stdout();
        _ = output.execute(event::DisableBracketedPaste);
        _ = output.execute(event::DisableFocusChange);
        _ = output.execute(terminal::LeaveAlternateScreen);
        _ = terminal::disable_raw_mode();
    }
}

fn detached_key_input(key: event::KeyEvent) -> Option<DetachedInput> {
    if !matches!(
        key.kind,
        event::KeyEventKind::Press | event::KeyEventKind::Repeat
    ) {
        return None;
    }
    let code = match key.code {
        event::KeyCode::Char(character) => DetachedKeyCode::Character(character),
        event::KeyCode::Enter => DetachedKeyCode::Enter,
        event::KeyCode::Backspace => DetachedKeyCode::Backspace,
        event::KeyCode::Esc => DetachedKeyCode::Escape,
        event::KeyCode::Tab => DetachedKeyCode::Tab,
        event::KeyCode::BackTab => DetachedKeyCode::BackTab,
        event::KeyCode::Delete => DetachedKeyCode::Delete,
        event::KeyCode::Left => DetachedKeyCode::Left,
        event::KeyCode::Right => DetachedKeyCode::Right,
        event::KeyCode::Up => DetachedKeyCode::Up,
        event::KeyCode::Down => DetachedKeyCode::Down,
        event::KeyCode::Home => DetachedKeyCode::Home,
        event::KeyCode::End => DetachedKeyCode::End,
        event::KeyCode::PageUp => DetachedKeyCode::PageUp,
        event::KeyCode::PageDown => DetachedKeyCode::PageDown,
        _ => return None,
    };
    let mut modifiers = Vec::new();
    if key.modifiers.contains(event::KeyModifiers::CONTROL) {
        modifiers.push(KeyModifier::Control);
    }
    if key.modifiers.contains(event::KeyModifiers::ALT) {
        modifiers.push(KeyModifier::Alt);
    }
    if key.modifiers.contains(event::KeyModifiers::SHIFT) {
        modifiers.push(KeyModifier::Shift);
    }
    Some(DetachedInput::Key { code, modifiers })
}

fn paint_detached_delta(
    output: &mut impl std::io::Write,
    rows: &mut Vec<red::headless::LinePatch>,
    delta: &red::headless::RenderDelta,
) -> anyhow::Result<()> {
    for patch in &delta.lines {
        if rows.len() <= patch.row {
            rows.resize_with(patch.row + 1, || red::headless::LinePatch {
                row: 0,
                text: String::new(),
                spans: Vec::new(),
            });
        }
        rows[patch.row] = patch.clone();
    }
    write!(output, "\x1b[H\x1b[2J")?;
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            write!(output, "\r\n")?;
        }
        if row.spans.is_empty() {
            write!(output, "{}", row.text)?;
            continue;
        }
        for span in &row.spans {
            output
                .queue(style::ResetColor)?
                .queue(style::SetAttribute(style::Attribute::Reset))?;
            if let Some(foreground) = span.style.fg {
                output.queue(style::SetForegroundColor(foreground.into()))?;
            }
            if let Some(background) = span.style.bg {
                output.queue(style::SetBackgroundColor(background.into()))?;
            }
            if span.style.bold {
                output.queue(style::SetAttribute(style::Attribute::Bold))?;
            }
            if span.style.italic {
                output.queue(style::SetAttribute(style::Attribute::Italic))?;
            }
            write!(output, "{}", span.text)?;
        }
    }
    output
        .queue(style::ResetColor)?
        .queue(style::SetAttribute(style::Attribute::Reset))?;
    write!(
        output,
        "\x1b[{};{}H",
        delta.cursor.1.saturating_add(1),
        delta.cursor.0.saturating_add(1)
    )?;
    output.flush()?;
    Ok(())
}

fn print_error(error: &anyhow::Error) {
    eprintln!("{}", format_error(error));
}

fn format_error(error: &anyhow::Error) -> String {
    if let Some(report) = error.downcast_ref::<husk_diagnostics::Report>() {
        report.to_string()
    } else {
        format!("Error: {error:#}")
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_husk_errors_do_not_get_a_rust_error_prefix() {
        let error = husk::Program::parse("broken", "fn activate( {").unwrap_err();

        let rendered = format_error(&error);

        assert!(rendered.starts_with("error[HUSK-P0001]:"));
        assert!(!rendered.starts_with("Error:"));
    }
}
