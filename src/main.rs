//! Process entry point and top-level lifecycle selection for Red.
//!
//! Startup validates mutually exclusive utility modes before constructing editor state.
//! Utility commands exit without entering the terminal, interactive runs own terminal
//! setup and cleanup, and Unix detach mode splits ownership between a persistent core
//! process and a replaceable terminal client. This module is responsible for choosing
//! those lifecycles, not for implementing editor behavior within them.

use std::{
    ffi::{OsStr, OsString},
    fs,
    io::{stdout, Write as _},
    panic,
    path::{Path, PathBuf},
    process::ExitCode,
};

#[cfg(unix)]
use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use clap::Parser as _;
use crossterm::{cursor, event, terminal, ExecutableCommand};
#[cfg(any(unix, test))]
use crossterm::{style, QueueableCommand};

use red::assets;
use red::buffer::Buffer;
use red::cli::{Args, LanguageCommand, PluginCommand, RootCommand};
use red::config::{
    Config, ConfigDiagnosticSeverity, ConfigRecovery, KeyAction, Keys, LoadedConfig,
};
use red::editor::{Action, Editor};
#[cfg(any(unix, test))]
use red::headless::{
    InputEvent as DetachedInput, KeyCode as DetachedKeyCode, KeyKind, KeyModifier,
};
use red::language::GrammarTrustStore;
use red::logger::Logger;
use red::lsp::{LspClient, LspManager};
use red::onboarding;
use red::preferences::PreferencesStore;
use red::session::SessionStore;
use red::theme::{parse_vscode_theme, parse_vscode_theme_contents, Theme};
use red::utils::{expand_user_path, same_file_path};
use red::{log, run_self_check, LOGGER};

#[cfg(unix)]
const DETACHED_PASTE_CHUNK_BYTES: usize = 128 * 1024;
#[cfg(unix)]
const DETACHED_POLL_INTERVAL: Duration = Duration::from_millis(50);
#[cfg(unix)]
const DETACHED_RENDER_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[tokio::main(flavor = "multi_thread")]
async fn main() -> ExitCode {
    if let Some(arguments) = forwarded_husk_arguments() {
        return husk_cli::run_from(arguments);
    }
    if let Err(error) = run().await {
        print_error(&error);
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn forwarded_husk_arguments() -> Option<Vec<OsString>> {
    forwarded_husk_arguments_from(std::env::args_os())
}

fn forwarded_husk_arguments_from(
    arguments: impl IntoIterator<Item = OsString>,
) -> Option<Vec<OsString>> {
    let mut arguments = arguments.into_iter();
    let _program = arguments.next()?;
    (arguments.next().as_deref() == Some(OsStr::new("husk"))).then(|| {
        std::iter::once(OsString::from("red husk"))
            .chain(arguments)
            .collect()
    })
}

async fn run() -> anyhow::Result<()> {
    let args = Args::parse();
    args.validate_utility_args()?;

    match &args.command {
        Some(RootCommand::Keys(keys)) => {
            return red::keyboard::inspect_keys(keys.protocol, keys.count);
        }
        Some(RootCommand::Plugin(plugin)) => return run_plugin_command(&plugin.command).await,
        Some(RootCommand::Language(language)) => {
            return run_language_command(&language.command, &args.config_overrides)
        }
        None => {}
    }

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
            "Codex agent-edit readiness check failed"
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
    loaded.config.startup_session_resumed = args.resume;

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
        buffers = load_startup_buffers(&args.files).await?;
    }

    let diagnostics = std::mem::take(&mut loaded.diagnostics);
    let recovery = loaded.recovery;
    let mut editor = Editor::new_with_preferences(lsp, loaded.config, theme, buffers, preferences)?;
    editor.set_language_reload_source(config_file, args.config_overrides.clone());
    editor.set_config_diagnostics(diagnostics, recovery);
    if let Some(snapshot) = &resumed_session {
        editor.suppress_startup_whats_new();
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
        _ = stdout.execute(terminal::EndSynchronizedUpdate);
        _ = stdout.execute(terminal::EnableLineWrap);
        red::keyboard::restore_after_panic(&mut stdout);
        _ = write!(stdout, "\x1b]112\x1b\\");
        _ = stdout.execute(event::DisableBracketedPaste);
        _ = stdout.execute(event::DisableFocusChange);
        _ = stdout.execute(terminal::LeaveAlternateScreen);
        _ = stdout.execute(cursor::Show);
        _ = terminal::disable_raw_mode();

        eprintln!("{}", info);
    }));

    let result = editor.run().await;

    let cleanup_result = editor.cleanup();

    log!(" ===> after run, shutting down LSP");
    if let Err(e) = editor.lsp_mut().shutdown().await {
        log!("Error shutting down LSP: {}", e);
    }

    cleanup_result?;
    result?;

    Ok(())
}

async fn run_plugin_command(command: &PluginCommand) -> anyhow::Result<()> {
    use red::plugin::catalog::{catalog_url, PluginCatalog};
    use red::plugin::package::{PluginId, PluginPackageManager};

    let manager = PluginPackageManager::new(Config::config_dir());
    match command {
        PluginCommand::Install(arguments) => {
            let installed = if let Some(path) = &arguments.path {
                manager
                    .install_path_with_trust(path, arguments.trust_native_grammars)
                    .await?
            } else if let Some(id) = &arguments.catalog {
                let id = PluginId::parse(id)?;
                let url = arguments.catalog_url.clone().unwrap_or_else(catalog_url);
                manager
                    .install_catalog(&url, &id, arguments.trust_native_grammars)
                    .await?
            } else {
                let source = arguments
                    .source
                    .as_deref()
                    .expect("clap requires a source or --path");
                let (repository, version) = source
                    .rsplit_once('@')
                    .map_or((source, None), |(repository, version)| {
                        (repository, Some(version))
                    });
                manager
                    .install_github_with_trust(repository, version, arguments.trust_native_grammars)
                    .await?
            };
            println!(
                "Installed {} {} ({})",
                installed.id,
                installed.version,
                installed.package_root.display()
            );
        }
        PluginCommand::Catalog(arguments) => {
            let url = arguments.catalog_url.clone().unwrap_or_else(catalog_url);
            let catalog = PluginCatalog::fetch(&url).await?;
            for package in catalog.packages {
                let compatibility = if package.supports_current_red_release()? {
                    "compatible"
                } else {
                    "incompatible"
                };
                let target = if package.artifact(red::language::host_target()).is_some() {
                    red::language::host_target()
                } else {
                    "unavailable"
                };
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    package.id,
                    package.version,
                    package.tier.label(),
                    compatibility,
                    target
                );
            }
        }
        PluginCommand::List => {
            let plugins = manager.list()?;
            if plugins.is_empty() {
                println!("No external plugins installed.");
            }
            for plugin in plugins {
                let state = if plugin.enabled {
                    "enabled"
                } else {
                    "disabled"
                };
                let compatibility = if plugin.compatible {
                    "compatible"
                } else {
                    "incompatible"
                };
                let companion = if plugin.has_companion {
                    "companion"
                } else if plugin.has_languages {
                    "language"
                } else {
                    "husk"
                };
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    plugin.id, plugin.version, state, compatibility, companion
                );
            }
        }
        PluginCommand::Update(arguments) if arguments.all => {
            let results = if arguments.trust_native_grammars {
                let mut results = Vec::new();
                for plugin in manager.list()?.into_iter().filter(|plugin| plugin.enabled) {
                    let id = plugin.id;
                    let result = manager.update_with_trust(&id, true).await;
                    results.push((id, result));
                }
                results
            } else {
                manager.update_all().await
            };
            for (id, result) in results {
                match result {
                    Ok(plugin) => println!("Updated {} to {}", id, plugin.version),
                    Err(error) => eprintln!("Failed to update {id}: {error:#}"),
                }
            }
        }
        PluginCommand::Update(arguments) => {
            let id = PluginId::parse(
                arguments
                    .id
                    .as_deref()
                    .expect("clap requires an id or --all"),
            )?;
            let plugin = manager
                .update_with_trust(&id, arguments.trust_native_grammars)
                .await?;
            println!("Updated {} to {}", plugin.id, plugin.version);
        }
        PluginCommand::Disable(arguments) => {
            let id = PluginId::parse(&arguments.id)?;
            manager.set_enabled(&id, false)?;
            println!("Disabled {id}");
        }
        PluginCommand::Enable(arguments) => {
            let id = PluginId::parse(&arguments.id)?;
            manager.set_enabled(&id, true)?;
            println!("Enabled {id}");
        }
        PluginCommand::Remove(arguments) => {
            let id = PluginId::parse(&arguments.id)?;
            manager.remove(&id, arguments.purge)?;
            if arguments.purge {
                println!("Removed {id} and purged its saved data");
            } else {
                println!("Removed {id}; saved data was preserved");
            }
        }
    }
    Ok(())
}

fn run_language_command(command: &LanguageCommand, overrides: &[String]) -> anyhow::Result<()> {
    let value = match command {
        LanguageCommand::Trust(arguments) | LanguageCommand::Untrust(arguments) => {
            arguments.language_or_path.as_str()
        }
    };
    let config_dir = Config::config_dir();
    let loaded = Config::load_user_file(&Config::path("config.toml"), overrides)?;
    let mut path = loaded
        .config
        .languages
        .get(value)
        .and_then(|language| language.grammar.as_ref())
        .and_then(|grammar| grammar.path.clone())
        .map(|path| {
            let expanded = expand_user_path(&path.to_string_lossy())?;
            Ok::<_, anyhow::Error>(if expanded.is_absolute() {
                expanded
            } else {
                config_dir.join(expanded)
            })
        })
        .transpose()?;
    if path.is_none() {
        let manager = red::plugin::package::PluginPackageManager::new(&config_dir);
        for installed in manager
            .list()?
            .into_iter()
            .filter(|plugin| plugin.enabled && plugin.compatible)
        {
            let manifest =
                red::plugin::package::PluginPackageManifest::load(&installed.package_root)?;
            if let Some(language) = manifest.languages.get(value) {
                path = manifest.grammar_path(&installed.package_root, value, language);
                if path.is_some() {
                    break;
                }
            }
        }
    }
    let path = match path {
        Some(path) => path,
        None => expand_user_path(value)?,
    };
    let trust = GrammarTrustStore::new(config_dir);
    match command {
        LanguageCommand::Trust(_) => {
            let digest = trust.trust_path(&path)?;
            println!("Trusted native grammar {} ({digest})", path.display());
        }
        LanguageCommand::Untrust(_) => {
            trust.revoke_path(&path)?;
            println!("Revoked native grammar trust for {}", path.display());
        }
    }
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
        let mut terminal_guard = DetachedTerminalGuard::default();
        let mut output = stdout();
        output
            .execute(event::EnableBracketedPaste)?
            .execute(event::EnableFocusChange)?
            .execute(event::EnableMouseCapture)?
            .execute(terminal::EnterAlternateScreen)?;
        terminal_guard.keyboard_protocol = red::keyboard::KeyboardProtocol::start(
            &mut output,
            red::keyboard::KeyboardPreference::Auto,
        )?;
        output
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
#[derive(Default)]
struct DetachedTerminalGuard {
    keyboard_protocol: red::keyboard::KeyboardProtocol,
}

#[cfg(unix)]
impl Drop for DetachedTerminalGuard {
    fn drop(&mut self) {
        let mut output = stdout();
        _ = output.execute(terminal::EndSynchronizedUpdate);
        _ = output.execute(event::DisableBracketedPaste);
        _ = output.execute(event::DisableFocusChange);
        _ = output.execute(event::DisableMouseCapture);
        _ = output.execute(terminal::EnableLineWrap);
        _ = self.keyboard_protocol.stop(&mut output);
        _ = output.execute(terminal::LeaveAlternateScreen);
        _ = output.execute(cursor::Show);
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
    let key_kind = if key.kind == event::KeyEventKind::Repeat {
        KeyKind::Repeat
    } else {
        KeyKind::Press
    };
    Some(DetachedInput::Key {
        code,
        modifiers,
        key_kind,
    })
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
    let package_manager = red::plugin::package::PluginPackageManager::new(&config_dir);
    for installed in package_manager
        .list()?
        .into_iter()
        .filter(|plugin| plugin.enabled && plugin.compatible)
    {
        let manifest = red::plugin::package::PluginPackageManifest::load(&installed.package_root)?;
        apply_plugin_default_keymaps(&mut loaded.config.keys, &manifest.keymaps);
        red::language::merge_package_languages(
            &mut loaded.config,
            &manifest,
            &installed.package_root,
        );
        let Some(entrypoint) = manifest.husk_entry(&installed.package_root) else {
            continue;
        };
        loaded
            .config
            .plugins
            .entry(installed.id.to_string())
            .or_insert_with(|| entrypoint.to_string_lossy().into_owned());
    }
    red::language::finalize_language_configuration(&mut loaded, &config_dir)?;
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

fn apply_plugin_default_keymaps(
    keys: &mut Keys,
    keymaps: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
) {
    for (mode, bindings) in keymaps {
        let target = match mode.as_str() {
            "normal" => &mut keys.normal,
            "insert" => &mut keys.insert,
            "command" => &mut keys.command,
            "visual" => &mut keys.visual,
            "visual_line" => &mut keys.visual_line,
            "visual_block" => &mut keys.visual_block,
            _ => continue,
        };
        for (sequence, command) in bindings {
            let sequence = sequence
                .split_whitespace()
                .map(|key| if key == "Space" { " " } else { key })
                .collect::<Vec<_>>();
            insert_plugin_default_binding(target, &sequence, command);
        }
    }
}

fn insert_plugin_default_binding(
    bindings: &mut std::collections::HashMap<String, KeyAction>,
    sequence: &[&str],
    command: &str,
) {
    let Some((key, remainder)) = sequence.split_first() else {
        return;
    };
    if remainder.is_empty() {
        bindings
            .entry((*key).to_string())
            .or_insert_with(|| KeyAction::Single(Action::PluginCommand(command.to_string())));
        return;
    }
    match bindings.entry((*key).to_string()) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            let mut nested = std::collections::HashMap::new();
            insert_plugin_default_binding(&mut nested, remainder, command);
            entry.insert(KeyAction::Nested(nested));
        }
        std::collections::hash_map::Entry::Occupied(mut entry) => {
            if let KeyAction::Nested(nested) = entry.get_mut() {
                insert_plugin_default_binding(nested, remainder, command);
            }
        }
    }
}

fn resolve_log_path(config_dir: &Path, configured_path: &str) -> anyhow::Result<PathBuf> {
    let path = expand_user_path(configured_path)?;
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(config_dir.join(path))
    }
}

async fn load_startup_buffers(files: &[String]) -> anyhow::Result<Vec<Buffer>> {
    let mut buffers = Vec::with_capacity(files.len());
    for file in files {
        let buffer = Buffer::load_or_create(Some(file.clone())).await?;
        let duplicate = buffer.file.as_deref().is_some_and(|candidate| {
            buffers.iter().any(|open: &Buffer| {
                open.file
                    .as_deref()
                    .is_some_and(|open| same_file_path(Path::new(open), Path::new(candidate)))
            })
        });
        if !duplicate {
            buffers.push(buffer);
        }
    }
    Ok(buffers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_default_keymaps_install_shared_leader_siblings() {
        let mut keys = Keys::default();
        let keymaps = std::collections::BTreeMap::from([(
            "normal".to_string(),
            std::collections::BTreeMap::from([
                ("Space R g".to_string(), "Replay".to_string()),
                ("Space R n".to_string(), "ReplayNext".to_string()),
            ]),
        )]);

        apply_plugin_default_keymaps(&mut keys, &keymaps);

        let Some(KeyAction::Nested(space)) = keys.normal.get(" ") else {
            panic!("expected Space to become a leader");
        };
        let Some(KeyAction::Nested(replay)) = space.get("R") else {
            panic!("expected Space R to become the Replay leader");
        };
        assert_eq!(
            replay.get("g"),
            Some(&KeyAction::Single(Action::PluginCommand(
                "Replay".to_string()
            )))
        );
        assert_eq!(
            replay.get("n"),
            Some(&KeyAction::Single(Action::PluginCommand(
                "ReplayNext".to_string()
            )))
        );
    }

    #[tokio::test]
    async fn startup_opens_missing_file_without_creating_it_until_save() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("new.rs");
        let file = path.to_string_lossy().into_owned();

        let mut buffers = load_startup_buffers(std::slice::from_ref(&file))
            .await
            .unwrap();

        assert_eq!(buffers.len(), 1);
        assert_eq!(buffers[0].file.as_deref(), Some(file.as_str()));
        assert_eq!(buffers[0].contents(), "\n");
        assert!(!buffers[0].is_dirty());
        assert!(!path.exists());

        buffers[0].save().unwrap();

        assert_eq!(fs::read_to_string(path).unwrap(), "\n");
        assert!(!buffers[0].is_dirty());
    }

    #[tokio::test]
    async fn startup_opens_existing_and_missing_files_in_argument_order() {
        let directory = tempfile::tempdir().unwrap();
        let existing_path = directory.path().join("existing.rs");
        let missing_path = directory.path().join("new.rs");
        fs::write(&existing_path, "fn main() {}\n").unwrap();
        let existing = existing_path.to_string_lossy().into_owned();
        let missing = missing_path.to_string_lossy().into_owned();

        let buffers = load_startup_buffers(&[existing.clone(), missing.clone()])
            .await
            .unwrap();

        assert_eq!(buffers.len(), 2);
        assert_eq!(buffers[0].file.as_deref(), Some(existing.as_str()));
        assert_eq!(buffers[0].contents(), "fn main() {}\n");
        assert_eq!(buffers[1].file.as_deref(), Some(missing.as_str()));
        assert_eq!(buffers[1].contents(), "\n");
        assert!(!missing_path.exists());
    }

    #[tokio::test]
    async fn startup_collapses_relative_and_absolute_aliases() {
        let cwd = std::env::current_dir().unwrap();
        let directory = tempfile::Builder::new()
            .prefix("red-startup-alias-")
            .tempdir_in(&cwd)
            .unwrap();
        let absolute = directory.path().join("main.c");
        fs::write(&absolute, "int main(void) { return 0; }\n").unwrap();
        let relative = absolute
            .strip_prefix(&cwd)
            .unwrap()
            .to_string_lossy()
            .into_owned();

        let buffers = load_startup_buffers(&[absolute.to_string_lossy().into_owned(), relative])
            .await
            .unwrap();

        assert_eq!(buffers.len(), 1);
        assert_eq!(buffers[0].file.as_deref(), absolute.to_str());
    }

    #[tokio::test]
    async fn startup_opens_missing_parent_but_reports_error_on_save() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("missing");
        let path = parent.join("new.rs");
        let file = path.to_string_lossy().into_owned();

        let mut buffers = load_startup_buffers(std::slice::from_ref(&file))
            .await
            .unwrap();

        let error = buffers[0].save().unwrap_err();

        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .map(|error| error.kind()),
            Some(std::io::ErrorKind::NotFound)
        );
        assert!(!parent.exists());
        assert!(!path.exists());
        assert_eq!(buffers[0].file.as_deref(), Some(file.as_str()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn startup_rejects_broken_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("broken.rs");
        std::os::unix::fs::symlink(directory.path().join("missing.rs"), &path).unwrap();
        let file = path.to_string_lossy().into_owned();

        let error = load_startup_buffers(std::slice::from_ref(&file))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("not found"));
        assert!(fs::symlink_metadata(path).unwrap().file_type().is_symlink());
    }

    #[test]
    fn forwards_only_the_husk_subcommand() {
        assert_eq!(
            forwarded_husk_arguments_from(
                ["red", "husk", "check", "script.hk"].map(OsString::from)
            ),
            Some(
                ["red husk", "check", "script.hk"]
                    .map(OsString::from)
                    .to_vec()
            )
        );
        assert_eq!(
            forwarded_husk_arguments_from(["red", "file.txt"].map(OsString::from)),
            None
        );
    }

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
                key_kind: KeyKind::Press,
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
                key_kind: KeyKind::Press,
            })
        );
    }

    #[test]
    fn detached_key_input_preserves_enter_repeats_and_discards_releases() {
        assert_eq!(
            detached_key_input(event::KeyEvent::new_with_kind(
                event::KeyCode::Enter,
                event::KeyModifiers::ALT,
                event::KeyEventKind::Repeat
            )),
            Some(DetachedInput::Key {
                code: DetachedKeyCode::Enter,
                modifiers: vec![KeyModifier::Alt],
                key_kind: KeyKind::Repeat
            })
        );
        assert_eq!(
            detached_key_input(event::KeyEvent::new_with_kind(
                event::KeyCode::Enter,
                event::KeyModifiers::NONE,
                event::KeyEventKind::Release
            )),
            None
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
