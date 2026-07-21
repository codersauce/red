//! Process entry point and top-level lifecycle selection for Red.
//!
//! Startup validates mutually exclusive utility modes before constructing editor state.
//! Utility commands exit without entering the terminal, interactive runs own terminal
//! setup and cleanup, and Unix detach mode splits ownership between a persistent core
//! process and a replaceable terminal client. This module is responsible for choosing
//! those lifecycles, not for implementing editor behavior within them.

use std::{
    fs,
    io::{stdout, Write as _},
    panic,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use clap::Parser as _;
use crossterm::{event, terminal, ExecutableCommand};
#[cfg(any(unix, test))]
use crossterm::{style, QueueableCommand};

use red::assets;
use red::buffer::Buffer;
use red::cli::Args;
use red::config::{Config, ConfigDiagnosticSeverity, ConfigRecovery, LoadedConfig};
use red::editor::Editor;
#[cfg(any(unix, test))]
use red::headless::{InputEvent as DetachedInput, KeyCode as DetachedKeyCode, KeyModifier};
use red::logger::Logger;
use red::lsp::{LspClient, LspManager};
use red::onboarding;
use red::preferences::PreferencesStore;
use red::session::SessionStore;
use red::theme::{parse_vscode_theme, parse_vscode_theme_contents, Theme};
use red::utils::expand_user_path;
use red::{log, run_self_check, LOGGER};

#[cfg(unix)]
const DETACHED_PASTE_CHUNK_BYTES: usize = 128 * 1024;
#[cfg(unix)]
const DETACHED_POLL_INTERVAL: Duration = Duration::from_millis(50);
#[cfg(unix)]
const DETACHED_RENDER_POLL_INTERVAL: Duration = Duration::from_millis(100);

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

    if args.check_config {
        let config_file = Config::path("config.toml");
        let (mut loaded, _, _) = finalize_runtime_config(Config::load_user_file(
            &config_file,
            &args.config_overrides,
        )?)?;
        loaded.diagnostics.sort_by(|left, right| {
            left.source
                .to_string()
                .cmp(&right.source.to_string())
                .then_with(|| {
                    left.span
                        .as_ref()
                        .map(|span| span.start)
                        .cmp(&right.span.as_ref().map(|span| span.start))
                })
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.code.cmp(&right.code))
        });
        if loaded.diagnostics.is_empty() {
            println!("config ok");
            return Ok(());
        }
        for diagnostic in &loaded.diagnostics {
            println!("{}", diagnostic.format());
        }
        anyhow::bail!(
            "configuration validation failed with {} problem(s)",
            loaded.diagnostics.len()
        );
    }

    if args.agent_check {
        let config_file = Config::path("config.toml");
        let loaded = Config::load_user_file(&config_file, &args.config_overrides)?;
        anyhow::ensure!(
            loaded.is_clean(),
            "configuration validation failed:\n{}",
            loaded
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.format())
                .collect::<Vec<_>>()
                .join("\n")
        );
        let report = red::agent_check::run(&loaded.config);
        println!("{}", report.format());
        anyhow::ensure!(
            !args.strict || report.production_ready,
            "Codex reviewable-edit readiness check failed"
        );
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

    let (mut loaded, theme, logger) = finalize_runtime_config(Config::load_user_file(
        &config_file,
        &args.config_overrides,
    )?)?;
    loaded.config.disable_plugin_typecheck = args.no_typecheck;
    LOGGER.get_or_init(|| logger);
    let preferences = PreferencesStore::load(Config::path("preferences.json"));

    loaded.config.startup_file_count = args.files.len();

    if let Some(root) = &args.root {
        // change to root directory
        std::env::set_current_dir(root)?;
    }

    let session_root = Config::path("sessions");
    let (resumed_store, resumed_session) = if args.resume {
        let (store, snapshot) = SessionStore::load_latest_with_store(&session_root)?;
        if !snapshot.cwd.is_empty() {
            std::env::set_current_dir(&snapshot.cwd)?;
        }
        (Some(store), Some(snapshot))
    } else {
        (None, None)
    };
    let session_store = match (&args.core_session, resumed_store) {
        (Some(session), _) => {
            SessionStore::for_owner(&session_root, &format!("detached-{session}"))?
        }
        (None, Some(store)) => store,
        (None, None) => {
            SessionStore::for_owner(&session_root, &format!("editor-{}", uuid::Uuid::new_v4()))?
        }
    };

    let lsp = Box::new(LspManager::new(loaded.config.lsp.clone())) as Box<dyn LspClient>;

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

    let diagnostics = std::mem::take(&mut loaded.diagnostics);
    let recovery = loaded.recovery;
    let mut editor = Editor::new_with_preferences(lsp, loaded.config, theme, buffers, preferences)?;
    editor.set_config_diagnostics(diagnostics, recovery);
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
        {
            let _ = session;
            anyhow::bail!(
                "detach is currently available on Linux and macOS; use --resume on Windows"
            );
        }
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
            .execute(event::EnableMouseCapture)?
            .execute(terminal::EnterAlternateScreen)?
            .execute(event::PushKeyboardEnhancementFlags(
                event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
            ))?
            .execute(terminal::DisableLineWrap)?
            .execute(terminal::Clear(terminal::ClearType::All))?;
        let result = async {
            paint_detached_delta(&mut output, &mut rows, &client.initial_render)?;
            let mut last_heartbeat = Instant::now();
            loop {
                if event::poll(DETACHED_POLL_INTERVAL)? {
                    match event::read()? {
                        event::Event::Key(key) if is_detach_key(&key) => {
                            client.detach().await?;
                            return Ok(());
                        }
                        event::Event::Resize(columns, rows_count) => {
                            let delta = client.resize(columns, rows_count).await?;
                            paint_detached_resize(&mut output, &mut rows, &delta, rows_count)?;
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
                            let delta = send_detached_paste(&mut client, text).await?;
                            paint_detached_delta(&mut output, &mut rows, &delta)?;
                        }
                        event::Event::Mouse(event) => {
                            let delta = client.input(DetachedInput::Mouse { event }).await?;
                            paint_detached_delta(&mut output, &mut rows, &delta)?;
                        }
                        event::Event::Key(key) => {
                            if let Some(input) = detached_key_input(key) {
                                let delta = client.input(input).await?;
                                paint_detached_delta(&mut output, &mut rows, &delta)?;
                            }
                        }
                    }
                }
                if last_heartbeat.elapsed() >= DETACHED_RENDER_POLL_INTERVAL {
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

#[cfg(unix)]
struct DetachedTerminalGuard;

#[cfg(unix)]
impl Drop for DetachedTerminalGuard {
    fn drop(&mut self) {
        let mut output = stdout();
        _ = output.execute(event::DisableBracketedPaste);
        _ = output.execute(event::DisableFocusChange);
        _ = output.execute(event::DisableMouseCapture);
        _ = output.execute(terminal::EnableLineWrap);
        _ = output.execute(event::PopKeyboardEnhancementFlags);
        _ = output.execute(terminal::LeaveAlternateScreen);
        _ = terminal::disable_raw_mode();
    }
}

#[cfg(any(unix, test))]
fn is_detach_key(key: &event::KeyEvent) -> bool {
    key.modifiers.contains(event::KeyModifiers::CONTROL)
        && matches!(key.code, event::KeyCode::Char('\\' | '4'))
}

#[cfg(any(unix, test))]
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
        event::KeyCode::F(number) => DetachedKeyCode::Function(number),
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

#[cfg(unix)]
async fn send_detached_paste<S>(
    client: &mut red::headless::HeadlessClient<S>,
    text: String,
) -> anyhow::Result<red::headless::RenderDelta>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    if text.len() <= DETACHED_PASTE_CHUNK_BYTES {
        return client.input(DetachedInput::Paste { text }).await;
    }

    let mut start: usize = 0;
    loop {
        let mut end = start
            .saturating_add(DETACHED_PASTE_CHUNK_BYTES)
            .min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let final_chunk = end == text.len();
        let delta = client
            .input(DetachedInput::PasteChunk {
                text: text[start..end].to_string(),
                final_chunk,
            })
            .await?;
        if final_chunk {
            return Ok(delta);
        }
        start = end;
    }
}

#[cfg(any(unix, test))]
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
        paint_detached_row(output, patch)?;
    }
    finish_detached_paint(output, delta.cursor)
}

#[cfg(any(unix, test))]
fn finish_detached_paint(
    output: &mut impl std::io::Write,
    cursor: (usize, usize),
) -> anyhow::Result<()> {
    output
        .queue(style::ResetColor)?
        .queue(style::SetAttribute(style::Attribute::Reset))?;
    write!(
        output,
        "\x1b[{};{}H",
        cursor.1.saturating_add(1),
        cursor.0.saturating_add(1)
    )?;
    output.flush()?;
    Ok(())
}

#[cfg(any(unix, test))]
fn paint_detached_row(
    output: &mut impl std::io::Write,
    row: &red::headless::LinePatch,
) -> anyhow::Result<()> {
    write!(output, "\x1b[{};1H\x1b[2K", row.row.saturating_add(1))?;
    if row.spans.is_empty() {
        write!(output, "{}", row.text)?;
        return Ok(());
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
    Ok(())
}

#[cfg(any(unix, test))]
fn paint_detached_resize(
    output: &mut impl std::io::Write,
    rows: &mut Vec<red::headless::LinePatch>,
    delta: &red::headless::RenderDelta,
    rows_count: u16,
) -> anyhow::Result<()> {
    rows.truncate(rows_count as usize);
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
    for row in rows {
        paint_detached_row(output, row)?;
    }
    finish_detached_paint(output, delta.cursor)
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

fn finalize_runtime_config(
    mut loaded: LoadedConfig,
) -> anyhow::Result<(LoadedConfig, Theme, Option<Logger>)> {
    let config_dir = Config::config_dir();
    for plugin in loaded.config.missing_plugins(&config_dir) {
        loaded.config.plugins.remove(&plugin);
        loaded.add_runtime_diagnostic(
            "CFG301",
            ConfigDiagnosticSeverity::Error,
            &["plugins".to_string(), plugin],
            "configured plugin could not be found",
            "quarantined the affected plugin",
        );
    }

    let theme = match load_theme(&loaded.config.theme) {
        Ok(theme) => theme,
        Err(error) => {
            loaded.add_runtime_diagnostic(
                "CFG302",
                ConfigDiagnosticSeverity::Error,
                &["theme".to_string()],
                format!("configured theme could not be loaded: {error}"),
                "used the embedded default theme",
            );
            loaded.config.theme = "red.json".to_string();
            let contents = assets::bundled_theme("red.json")
                .ok_or_else(|| anyhow::anyhow!("embedded default theme is missing"))?;
            parse_vscode_theme_contents(contents)
                .map_err(|error| anyhow::anyhow!("embedded default theme is invalid: {error}"))?
        }
    };

    let logger = match loaded.config.log_file.clone() {
        Some(configured_path) => {
            match resolve_log_path(&config_dir, &configured_path).and_then(|path| {
                Logger::try_new(&path)
                    .map(|logger| (path, logger))
                    .map_err(anyhow::Error::from)
            }) {
                Ok((path, logger)) => {
                    loaded.config.log_file = Some(path.to_string_lossy().into_owned());
                    Some(logger)
                }
                Err(error) => {
                    loaded.add_runtime_diagnostic(
                        "CFG303",
                        ConfigDiagnosticSeverity::Error,
                        &["log_file".to_string()],
                        format!("configured log file could not be opened: {error}"),
                        "disabled logging",
                    );
                    loaded.config.log_file = None;
                    None
                }
            }
        }
        None => None,
    };

    if loaded.recovery == ConfigRecovery::WholeFileFallback {
        loaded.config.disable_ai = true;
        loaded.config.plugins.clear();
        loaded.config.plugin_permissions.clear();
        loaded.config.lsp.enabled = false;
        loaded.config.lsp.servers.clear();
        loaded.config.log_file = None;
    }

    Ok((loaded, theme, logger))
}

fn resolve_log_path(config_dir: &Path, configured_path: &str) -> anyhow::Result<PathBuf> {
    let path = expand_user_path(configured_path)?;
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(config_dir.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_config_falls_back_for_missing_theme_and_invalid_log_path() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        let contents = format!(
            "theme = \"missing-theme.json\"\nlog_file = {:?}\n",
            directory.path()
        );
        let loaded = Config::load_user_toml(&contents, &config_path, &[]).unwrap();

        let (loaded, _, logger) = finalize_runtime_config(loaded).unwrap();

        assert_eq!(loaded.config.theme, "red.json");
        assert!(loaded.config.log_file.is_none());
        assert!(logger.is_none());
        assert!(loaded
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "CFG302"));
        assert!(loaded
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "CFG303"));
    }

    #[test]
    fn relative_log_paths_resolve_from_the_config_directory() {
        let config_dir = Path::new("config-root");

        assert_eq!(
            resolve_log_path(config_dir, "logs/red.log").unwrap(),
            config_dir.join("logs").join("red.log")
        );
    }

    #[test]
    fn absolute_log_paths_are_preserved() {
        let absolute = std::env::current_dir().unwrap().join("red.log");

        assert_eq!(
            resolve_log_path(Path::new("ignored"), &absolute.to_string_lossy()).unwrap(),
            absolute
        );
    }

    #[test]
    fn detach_key_accepts_raw_control_backslash() {
        let control = event::KeyModifiers::CONTROL;

        assert!(is_detach_key(&event::KeyEvent::new(
            event::KeyCode::Char('\\'),
            control
        )));
        assert!(is_detach_key(&event::KeyEvent::new(
            event::KeyCode::Char('4'),
            control
        )));
        assert!(!is_detach_key(&event::KeyEvent::new(
            event::KeyCode::Char('4'),
            event::KeyModifiers::NONE
        )));
    }

    #[test]
    fn detached_key_input_preserves_function_keys_and_combined_modifiers() {
        assert_eq!(
            detached_key_input(event::KeyEvent::new(
                event::KeyCode::F(1),
                event::KeyModifiers::NONE,
            )),
            Some(DetachedInput::Key {
                code: DetachedKeyCode::Function(1),
                modifiers: Vec::new(),
            })
        );
        assert_eq!(
            detached_key_input(event::KeyEvent::new(
                event::KeyCode::Char('p'),
                event::KeyModifiers::CONTROL | event::KeyModifiers::SHIFT,
            )),
            Some(DetachedInput::Key {
                code: DetachedKeyCode::Character('p'),
                modifiers: vec![KeyModifier::Control, KeyModifier::Shift],
            })
        );
    }

    #[test]
    fn detached_resize_drops_rows_below_the_new_terminal_height() {
        let mut rows = (0..5)
            .map(|row| red::headless::LinePatch {
                row,
                text: format!("stale row {row}"),
                spans: Vec::new(),
            })
            .collect();
        let delta = red::headless::RenderDelta {
            revision: 1,
            lines: (0..3)
                .map(|row| red::headless::LinePatch {
                    row,
                    text: format!("fresh row {row}"),
                    spans: Vec::new(),
                })
                .collect(),
            cursor: (0, 0),
        };
        let mut output = Vec::new();

        paint_detached_resize(&mut output, &mut rows, &delta, 3).unwrap();

        assert_eq!(rows.len(), 3);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("fresh row 0"));
        assert!(output.contains("fresh row 1"));
        assert!(output.contains("fresh row 2"));
        assert!(!output.contains("stale row"));
    }

    #[test]
    fn detached_delta_only_repaints_changed_rows() {
        let mut rows = vec![
            red::headless::LinePatch {
                row: 0,
                text: "unchanged".to_string(),
                spans: Vec::new(),
            },
            red::headless::LinePatch {
                row: 1,
                text: "before".to_string(),
                spans: Vec::new(),
            },
        ];
        let delta = red::headless::RenderDelta {
            revision: 2,
            lines: vec![red::headless::LinePatch {
                row: 1,
                text: "changed".to_string(),
                spans: Vec::new(),
            }],
            cursor: (0, 1),
        };
        let mut output = Vec::new();

        paint_detached_delta(&mut output, &mut rows, &delta).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("changed"));
        assert!(!output.contains("unchanged"));
        assert!(!output.contains("\u{1b}[H\u{1b}[2J"));
    }

    #[test]
    fn detached_resize_repaints_cached_unchanged_rows_after_clear() {
        let mut rows = vec![
            red::headless::LinePatch {
                row: 0,
                text: "cached".to_string(),
                spans: Vec::new(),
            },
            red::headless::LinePatch {
                row: 1,
                text: "before".to_string(),
                spans: Vec::new(),
            },
        ];
        let delta = red::headless::RenderDelta {
            revision: 3,
            lines: vec![red::headless::LinePatch {
                row: 1,
                text: "changed".to_string(),
                spans: Vec::new(),
            }],
            cursor: (0, 0),
        };
        let mut output = Vec::new();

        paint_detached_resize(&mut output, &mut rows, &delta, 2).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("cached"));
        assert!(output.contains("changed"));
        assert!(!output.contains("before"));
    }

    #[test]
    fn structured_husk_errors_do_not_get_a_rust_error_prefix() {
        let error = husk_runtime::CompiledProgram::parse("broken", "fn activate( {").unwrap_err();

        let rendered = format_error(&error);

        assert!(rendered.starts_with("error[HUSK-P0001]:"));
        assert!(!rendered.starts_with("Error:"));
    }
}
