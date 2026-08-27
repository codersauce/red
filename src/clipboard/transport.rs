//! Clipboard selection follows the active terminal, not the detached owner's environment.

use std::{
    io::{IsTerminal, Write},
    process::Stdio,
    time::Duration,
};

use anyhow::{bail, ensure, Context};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::{ClipboardProvider, CopyOutcome, NativeClipboardProvider, MAX_CLIPBOARD_BYTES};
use crate::config::ClipboardConfig;

pub const fn default_max_bytes() -> usize {
    64 * 1024
}

pub fn deserialize_max_bytes<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<usize, D::Error> {
    let value = usize::deserialize(deserializer)?;
    if (1..=MAX_CLIPBOARD_BYTES).contains(&value) {
        Ok(value)
    } else {
        Err(serde::de::Error::custom(
            "clipboard.max_bytes must be between 1 and 262144",
        ))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardBackend {
    #[default]
    Auto,
    Native,
    Osc52,
    Tmux,
    Command,
    None,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Passthrough {
    #[default]
    None,
    Tmux,
    Screen,
}

#[derive(Debug, Clone, Default)]
struct Environment {
    terminal: bool,
    remote: bool,
    term: String,
    tmux_pane: Option<String>,
    screen: bool,
}

impl Environment {
    fn detect() -> Self {
        let present = |key: &str| std::env::var(key).ok().filter(|value| !value.is_empty());
        Self {
            terminal: std::io::stdout().is_terminal(),
            remote: ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
                .iter()
                .any(|key| present(key).is_some()),
            term: present("TERM").unwrap_or_default(),
            tmux_pane: present("TMUX").and_then(|_| present("TMUX_PANE")),
            screen: present("STY").is_some(),
        }
    }

    fn preferred(&self) -> ClipboardBackend {
        if !self.terminal || self.term == "dumb" {
            ClipboardBackend::None
        } else if self.tmux_pane.is_some() {
            ClipboardBackend::Tmux
        } else if self.remote || self.screen {
            ClipboardBackend::Osc52
        } else {
            ClipboardBackend::Native
        }
    }
}

/// One frontend's transport. Never constructs a native provider in an SSH auto session.
pub struct SessionClipboard {
    config: ClipboardConfig,
    backend: ClipboardBackend,
    native: Option<NativeClipboardProvider>,
    tmux_client: Option<String>,
    tmux_pane: Option<String>,
    reason: String,
    terminal: bool,
}

impl SessionClipboard {
    pub fn new(config: &ClipboardConfig) -> Self {
        Self::with_environment(config, Environment::detect())
    }

    pub fn test_text(&self) -> String {
        format!("red clipboard test ({:?}) — café 👻", self.backend)
    }

    fn with_environment(config: &ClipboardConfig, env: Environment) -> Self {
        let backend = if !config.enabled {
            ClipboardBackend::None
        } else if config.backend == ClipboardBackend::Auto {
            env.preferred()
        } else {
            config.backend
        };
        let mut result = Self {
            config: config.clone(),
            backend,
            native: None,
            tmux_client: None,
            tmux_pane: env.tmux_pane.clone(),
            reason: if config.backend == ClipboardBackend::Auto {
                format!(
                    "auto: tty={}, ssh={}, tmux={}, screen={}",
                    env.terminal,
                    env.remote,
                    env.tmux_pane.is_some(),
                    env.screen
                )
            } else {
                "explicit configuration".into()
            },
            terminal: env.terminal,
        };
        if !config.enabled {
            result.reason = "clipboard.enabled = false".into();
        }
        if config.max_bytes == 0 || config.max_bytes > MAX_CLIPBOARD_BYTES {
            result.disable("clipboard.max_bytes must be between 1 and 262144");
        } else if backend == ClipboardBackend::Native {
            match NativeClipboardProvider::new() {
                Ok(native) => result.native = Some(native),
                Err(_)
                    if config.backend == ClipboardBackend::Auto
                        && env.terminal
                        && !env.term.is_empty()
                        && env.term != "dumb" =>
                {
                    result.backend = ClipboardBackend::Osc52;
                    result
                        .reason
                        .push_str("; native clipboard unavailable, attempting terminal delivery");
                }
                Err(_) => result.disable("native clipboard unavailable"),
            }
        } else if backend == ClipboardBackend::Tmux {
            match tmux_target(env.tmux_pane.as_deref(), config.tmux_client.as_deref()) {
                Ok(client) => result.tmux_client = Some(client),
                // A pane can regain a client or permission without restarting Red.
                // Keep the route so the next explicit copy retries the current policy.
                Err(error) => result
                    .reason
                    .push_str(&format!("; currently unavailable: {error}")),
            }
        } else if backend == ClipboardBackend::Osc52 && !env.terminal {
            result.disable("OSC 52 requires an attached terminal; stdout is not a tty");
        } else if backend == ClipboardBackend::Command && config.copy_command.is_empty() {
            result.disable(
                "clipboard.copy_command must contain an executable and optional arguments",
            );
        }
        if env.screen
            && config.passthrough == Passthrough::None
            && result.backend == ClipboardBackend::Osc52
        {
            result.config.passthrough = Passthrough::Screen;
            result
                .reason
                .push_str("; GNU Screen DCS wrapping (small payloads only)");
        }
        result
    }

    fn disable(&mut self, reason: &str) {
        self.backend = ClipboardBackend::None;
        self.reason = reason.into();
    }

    /// Does not read, write, or display clipboard contents.
    pub fn diagnostic(&self) -> String {
        format!("Clipboard backend: {:?}\nSelection: {}\nRead support: {}\nCopy limit: {} UTF-8 bytes\nTerminal wrapping: {:?}\n{}",
            self.backend, self.reason, if self.can_read() { "native or explicitly configured command" } else { "internal register; use the terminal paste shortcut for external text" },
            self.config.max_bytes, self.config.passthrough,
            "OSC 52 delivery is unconfirmed until pasted on the client. No clipboard reads are used for detection.")
    }

    pub fn can_read(&self) -> bool {
        self.config.enabled
            && self.config.sync_on_paste
            && (self.backend == ClipboardBackend::Native
                || self.backend == ClipboardBackend::Command
                    && !self.config.paste_command.is_empty())
    }

    pub fn report(outcome: anyhow::Result<CopyOutcome>) -> String {
        match outcome {
            Ok(CopyOutcome::Written) => "copied to clipboard".into(),
            Ok(CopyOutcome::Sent) => "clipboard copy sent (delivery unconfirmed)".into(),
            Ok(CopyOutcome::Deferred) => "clipboard delivery pending".into(),
            Err(error) => format!("clipboard: {error}; text kept in Red's register"),
        }
    }
}

impl ClipboardProvider for SessionClipboard {
    fn is_available(&self) -> bool {
        self.backend != ClipboardBackend::None
    }

    fn get_text(&mut self) -> anyhow::Result<Option<String>> {
        if !self.can_read() {
            return Ok(None);
        }
        let text = if let Some(native) = &mut self.native {
            native.get_text()?
        } else {
            Some(
                String::from_utf8(run_command(&self.config.paste_command, None)?)
                    .context("clipboard command returned non-UTF-8 text")?,
            )
        };
        ensure!(
            text.as_ref().is_none_or(
                |text| text.len() <= self.config.max_bytes && text.len() <= MAX_CLIPBOARD_BYTES
            ),
            "clipboard read exceeds configured byte limit"
        );
        Ok(text)
    }

    fn set_text(&mut self, _text: &str) -> anyhow::Result<()> {
        bail!("clipboard delivery requires the frontend output writer")
    }

    fn copy(&mut self, text: &str, output: &mut dyn Write) -> anyhow::Result<CopyOutcome> {
        ensure!(
            text.len() <= self.config.max_bytes && text.len() <= MAX_CLIPBOARD_BYTES,
            "copy exceeds clipboard.max_bytes ({} bytes)",
            self.config.max_bytes
        );
        match self.backend {
            ClipboardBackend::Native => {
                self.native
                    .as_mut()
                    .context("native clipboard unavailable")?
                    .set_text(text)?;
                Ok(CopyOutcome::Written)
            }
            ClipboardBackend::Osc52 => {
                ensure!(self.terminal, "OSC 52 requires an attached terminal");
                output.write_all(&osc52(text, self.config.passthrough)?)?;
                output.flush()?;
                Ok(CopyOutcome::Sent)
            }
            ClipboardBackend::Tmux => {
                // Respect an explicit denial even if the tmux command itself would allow it.
                // tmux can acquire a different client without restarting this pane.
                self.tmux_client = Some(tmux_target(
                    self.tmux_pane.as_deref(),
                    self.config.tmux_client.as_deref(),
                )?);
                let client = self
                    .tmux_client
                    .as_ref()
                    .context("tmux client unavailable")?;
                run_command(
                    &["tmux", "load-buffer", "-w", "-t", client, "-"].map(str::to_owned),
                    Some(text),
                )?;
                Ok(CopyOutcome::Sent)
            }
            ClipboardBackend::Command => {
                run_command(&self.config.copy_command, Some(text))?;
                Ok(CopyOutcome::Sent)
            }
            ClipboardBackend::None | ClipboardBackend::Auto => bail!("{}", self.reason),
        }
    }
}

fn osc52(text: &str, wrapping: Passthrough) -> anyhow::Result<Vec<u8>> {
    let sequence = format!("\x1b]52;c;{}\x07", STANDARD.encode(text));
    Ok(match wrapping {
        Passthrough::None => sequence.into_bytes(),
        Passthrough::Tmux => {
            format!("\x1bPtmux;{}\x1b\\", sequence.replace('\x1b', "\x1b\x1b")).into_bytes()
        }
        Passthrough::Screen => {
            // Older Screen versions have a ~768-byte DCS limit. Never truncate or split writes.
            ensure!(sequence.len() <= 740, "GNU Screen clipboard payload exceeds the conservative DCS limit; use a custom clipboard command");
            format!("\x1bP{sequence}\x1b\\").into_bytes()
        }
    })
}

fn check_tmux_policy() -> anyhow::Result<()> {
    let setting = run_command(
        &["tmux", "show-options", "-s", "-v", "set-clipboard"].map(str::to_owned),
        None,
    )?;
    ensure!(
        matches!(String::from_utf8_lossy(&setting).trim(), "on" | "external"),
        "tmux clipboard disabled; check set-clipboard and the outer terminal's Ms capability"
    );
    Ok(())
}

fn tmux_target(pane: Option<&str>, requested: Option<&str>) -> anyhow::Result<String> {
    check_tmux_policy()?;
    let pane = pane.context("tmux backend requires TMUX and TMUX_PANE")?;
    let session = run_command(
        &["tmux", "display-message", "-p", "-t", pane, "#{session_id}"].map(str::to_owned),
        None,
    )?;
    let clients = run_command(
        &[
            "tmux",
            "list-clients",
            "-F",
            "#{client_tty}\t#{session_id}\t#{client_termfeatures}",
        ]
        .map(str::to_owned),
        None,
    )?;
    let clients = String::from_utf8(clients)?;
    let client = select_tmux_client(
        &clients,
        String::from_utf8_lossy(&session).trim(),
        requested,
    )?;
    let advertised = clients.lines().any(|line| {
        let mut fields = line.split('\t');
        fields.next() == Some(client.as_str())
            && fields
                .nth(1)
                .is_some_and(|features| features.split(',').any(|feature| feature == "clipboard"))
    });
    if !advertised {
        // Older tmux versions and hand-written terminal-overrides may have Ms
        // without the newer client_termfeatures flag.
        let info = run_command(&["tmux", "info", "-t", &client].map(str::to_owned), None)?;
        ensure!(tmux_info_supports_clipboard(&String::from_utf8_lossy(&info)), "tmux does not advertise clipboard support for this client; check its Ms capability / terminal-features");
    }
    Ok(client)
}

fn tmux_info_supports_clipboard(info: &str) -> bool {
    info.lines().any(|line| {
        line.split_once("Ms:").is_some_and(|(_, value)| {
            value
                .trim()
                .strip_prefix("(string)")
                .is_some_and(|sequence| !sequence.trim().is_empty())
        })
    })
}
fn select_tmux_client(
    clients: &str,
    session: &str,
    requested: Option<&str>,
) -> anyhow::Result<String> {
    let candidates: Vec<_> = clients
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let tty = fields.next()?;
            let candidate_session = fields.next()?;
            (candidate_session == session && requested.is_none_or(|requested| tty == requested))
                .then_some(tty)
        })
        .collect();
    ensure!(candidates.len() == 1, "tmux clipboard requires one attached client for this session; select clipboard.tmux_client explicitly when sharing a session");
    Ok(candidates[0].to_owned())
}

/// Run only user-configured argv or our fixed tmux commands. No shell, payload argv, or stderr logs.
fn run_command(argv: &[String], text: Option<&str>) -> anyhow::Result<Vec<u8>> {
    let argv = argv.to_vec();
    let input = text.map(str::to_owned);
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let program = argv.first().context("clipboard command is empty")?;
                let mut child = tokio::process::Command::new(program)
                    .args(&argv[1..])
                    .stdin(if input.is_some() {
                        Stdio::piped()
                    } else {
                        Stdio::null()
                    })
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .kill_on_drop(true)
                    .spawn()
                    .context("could not start clipboard command")?;
                let mut stdin = child.stdin.take();
                let mut stdout = child
                    .stdout
                    .take()
                    .context("clipboard command has no stdout")?
                    .take((MAX_CLIPBOARD_BYTES + 1) as u64);
                let result = tokio::time::timeout(Duration::from_millis(750), async {
                    let write = async {
                        if let (Some(stdin), Some(input)) = (&mut stdin, &input) {
                            stdin.write_all(input.as_bytes()).await?;
                        }
                        drop(stdin.take());
                        Ok::<_, std::io::Error>(())
                    };
                    let read = async {
                        let mut bytes = Vec::new();
                        stdout.read_to_end(&mut bytes).await?;
                        ensure!(
                            bytes.len() <= MAX_CLIPBOARD_BYTES,
                            "clipboard command output exceeds 256 KiB"
                        );
                        Ok::<_, anyhow::Error>(bytes)
                    };
                    let (_, bytes) =
                        tokio::try_join!(async { write.await.map_err(anyhow::Error::from) }, read)?;
                    ensure!(child.wait().await?.success(), "clipboard command failed");
                    Ok::<_, anyhow::Error>(bytes)
                })
                .await
                .context("clipboard command timed out after 750 ms")?;
                if result.is_err() {
                    let _ = child.kill().await;
                }
                result
            })
    })
    .join()
    .map_err(|_| anyhow::anyhow!("clipboard command worker failed"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_transport_precedes_native_clipboard() {
        let mut env = Environment {
            terminal: true,
            term: "xterm-256color".into(),
            ..Environment::default()
        };
        assert_eq!(env.preferred(), ClipboardBackend::Native);
        env.remote = true;
        assert_eq!(env.preferred(), ClipboardBackend::Osc52);
        env.tmux_pane = Some("%1".into());
        assert_eq!(env.preferred(), ClipboardBackend::Tmux);
        env.terminal = false;
        assert_eq!(env.preferred(), ClipboardBackend::None);
    }

    #[test]
    fn osc52_preserves_unicode_newlines_and_encodes_control_characters() {
        let text = "café 👻\nsecond\r\n\x1b]52;c;bad\x07";
        let bytes = osc52(text, Passthrough::None).unwrap();
        assert_eq!(&bytes[..7], b"\x1b]52;c;");
        assert_eq!(
            STANDARD.decode(&bytes[7..bytes.len() - 1]).unwrap(),
            text.as_bytes()
        );
        assert_eq!(bytes.last(), Some(&7));
    }

    #[test]
    fn explicit_wrapping_is_bounded_and_does_not_split_clipboard_writes() {
        let plain = osc52("test", Passthrough::None).unwrap();
        assert_eq!(
            osc52("test", Passthrough::Tmux).unwrap(),
            format!(
                "\x1bPtmux;{}\x1b\\",
                String::from_utf8(plain)
                    .unwrap()
                    .replace('\x1b', "\x1b\x1b")
            )
            .as_bytes()
        );
        assert!(osc52(&"x".repeat(600), Passthrough::Screen).is_err());
    }

    #[test]
    fn denied_and_oversized_copies_emit_nothing_and_remote_reads_are_inert() {
        let env = Environment {
            terminal: true,
            remote: true,
            term: "xterm-256color".into(),
            ..Environment::default()
        };
        let mut clipboard = SessionClipboard::with_environment(
            &ClipboardConfig {
                max_bytes: 3,
                ..ClipboardConfig::default()
            },
            env,
        );
        let mut output = Vec::new();
        assert_eq!(clipboard.get_text().unwrap(), None);
        assert!(clipboard.copy("large", &mut output).is_err());
        assert!(output.is_empty());
        assert_eq!(
            clipboard.copy("ok", &mut output).unwrap(),
            CopyOutcome::Sent
        );
        let mut disabled = SessionClipboard::with_environment(
            &ClipboardConfig {
                enabled: false,
                ..ClipboardConfig::default()
            },
            Environment::default(),
        );
        assert!(disabled.copy("private", &mut Vec::new()).is_err());
    }

    #[test]
    fn tmux_clients_are_selected_by_session_without_guessing_shared_clients() {
        let clients = "/dev/pts/1\t$1\tclipboard,RGB\n/dev/pts/2\t$2\tclipboard\n";
        assert_eq!(
            select_tmux_client(clients, "$1", None).unwrap(),
            "/dev/pts/1"
        );
        assert!(select_tmux_client(clients, "$1", Some("/dev/pts/2")).is_err());
        let shared = format!("{clients}/dev/pts/3\t$1\tclipboard\n");
        assert!(select_tmux_client(&shared, "$1", None).is_err());
        assert_eq!(
            select_tmux_client(&shared, "$1", Some("/dev/pts/3")).unwrap(),
            "/dev/pts/3"
        );
        assert_eq!(
            select_tmux_client("tty\t$1\tRGB", "$1", None).unwrap(),
            "tty"
        );
        assert!(tmux_info_supports_clipboard(
            "193: Ms: (string) \\033]52;%p1%s;%p2%s\\a"
        ));
        assert!(!tmux_info_supports_clipboard("193: Ms: [missing]"));
        assert!(!tmux_info_supports_clipboard("193: Ms: (string)"));
    }

    #[cfg(unix)]
    #[test]
    fn command_transport_preserves_payload_and_bounds_failures() {
        let argv = |command: &str| ["sh", "-c", command].map(str::to_owned);
        let payload = "café 👻\n$(not a command)\n";
        assert_eq!(
            run_command(&argv("cat"), Some(payload)).unwrap(),
            payload.as_bytes()
        );
        let error = run_command(&argv("printf private-content >&2; exit 7"), None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("clipboard command failed"));
        assert!(!error.contains("private-content"));
        let started = std::time::Instant::now();
        assert!(run_command(&argv("exec sleep 5"), None)
            .unwrap_err()
            .to_string()
            .contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn configured_backends_and_invalid_limits_are_validated() {
        let config = crate::config::Config::from_user_toml_with_overrides(
            "",
            &[
                "clipboard.backend = 'osc52'".into(),
                "clipboard.max_bytes = 1234".into(),
            ],
        )
        .unwrap();
        assert_eq!(config.clipboard.backend, ClipboardBackend::Osc52);
        assert_eq!(config.clipboard.max_bytes, 1234);
        for invalid in [
            "clipboard.max_bytes = 0",
            "clipboard.max_bytes = 262145",
            "clipboard.backend = 'unknown'",
        ] {
            assert!(
                crate::config::Config::from_user_toml_with_overrides("", &[invalid.into()])
                    .is_err(),
                "{invalid}"
            );
        }
    }
}
