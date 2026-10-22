//! Clipboard delivery belongs to the active frontend, including over SSH and Herdr.

use std::io::{IsTerminal, Write};

use anyhow::{bail, ensure};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};

use super::{ClipboardProvider, CopyOutcome, NativeClipboardProvider};
use crate::config::ClipboardConfig;

const MAX_OSC52_BYTES: usize = 256 * 1024;

pub const fn default_max_bytes() -> usize {
    64 * 1024
}

pub fn deserialize_max_bytes<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<usize, D::Error> {
    let value = usize::deserialize(deserializer)?;
    if (1..=MAX_OSC52_BYTES).contains(&value) {
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
    None,
}

#[derive(Debug, Clone, Default)]
struct Environment {
    terminal: bool,
    remote: bool,
    herdr: bool,
    tmux: bool,
    term: String,
}

impl Environment {
    fn detect() -> Self {
        Self::from_env(std::io::stdout().is_terminal(), |key| {
            std::env::var(key).ok()
        })
    }

    fn from_env(terminal: bool, value: impl Fn(&str) -> Option<String>) -> Self {
        let present = |key| value(key).is_some_and(|value| !value.is_empty());
        Self {
            terminal,
            remote: ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
                .iter()
                .any(|key| present(key)),
            // Persistent Herdr panes may outlive the SSH connection that created them.
            herdr: value("HERDR_ENV").as_deref() == Some("1")
                || value("TERM_PROGRAM").as_deref() == Some("herdr"),
            tmux: present("TMUX"),
            term: value("TERM").unwrap_or_default(),
        }
    }

    fn preferred(&self) -> ClipboardBackend {
        if !self.terminal || self.term == "dumb" {
            ClipboardBackend::None
        } else if self.remote || self.herdr || self.tmux {
            ClipboardBackend::Osc52
        } else {
            ClipboardBackend::Native
        }
    }
}

/// Selects delivery using this frontend's environment, never the detached owner's.
/// Remote copies do not access the server's native clipboard or issue OSC 52 reads.
pub struct SessionClipboard {
    config: ClipboardConfig,
    backend: ClipboardBackend,
    native: Option<NativeClipboardProvider>,
    reason: String,
}

impl SessionClipboard {
    pub fn new(config: &ClipboardConfig) -> Self {
        Self::with_environment(config, Environment::detect())
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
            reason: if config.backend == ClipboardBackend::Auto {
                format!(
                    "auto: tty={}, ssh={}, herdr={}, tmux={}",
                    env.terminal, env.remote, env.herdr, env.tmux
                )
            } else {
                "explicit configuration".into()
            },
        };
        if !config.enabled {
            result.disable("clipboard.enabled = false");
        } else if backend == ClipboardBackend::Native {
            match NativeClipboardProvider::new() {
                Ok(native) => result.native = Some(native),
                Err(_)
                    if config.backend == ClipboardBackend::Auto
                        && env.terminal
                        && env.term != "dumb" =>
                {
                    result.backend = ClipboardBackend::Osc52;
                    result
                        .reason
                        .push_str("; native clipboard unavailable, attempting terminal delivery");
                }
                Err(_) => result.disable("native clipboard unavailable"),
            }
        } else if backend == ClipboardBackend::Osc52 && !env.terminal {
            result.disable("OSC 52 requires an attached terminal; stdout is not a tty");
        }
        result
    }

    fn disable(&mut self, reason: &str) {
        self.backend = ClipboardBackend::None;
        self.reason = reason.into();
    }

    pub fn test_text(&self) -> String {
        format!("red clipboard test ({:?}) — café 👻", self.backend)
    }

    /// Reports routing without reading or displaying clipboard contents.
    pub fn diagnostic(&self) -> String {
        format!("Clipboard backend: {:?}\nSelection: {}\nRead support: {}\nOSC 52 copy limit: {} UTF-8 bytes\nOSC 52 delivery is unconfirmed until pasted on the client.",
            self.backend, self.reason,
            if self.backend == ClipboardBackend::Native && self.config.sync_on_paste {
                "native clipboard"
            } else { "internal register; use the terminal paste shortcut for external text" },
            self.config.max_bytes)
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
        if !self.config.sync_on_paste {
            return Ok(None);
        }
        match &mut self.native {
            Some(native) => native.get_text(),
            None => Ok(None),
        }
    }

    fn set_text(&mut self, text: &str) -> anyhow::Result<()> {
        match &mut self.native {
            Some(native) => native.set_text(text),
            None => bail!("terminal clipboard delivery requires the frontend output writer"),
        }
    }

    fn copy(&mut self, text: &str, output: &mut dyn Write) -> anyhow::Result<CopyOutcome> {
        match self.backend {
            ClipboardBackend::Native => {
                self.set_text(text)?;
                Ok(CopyOutcome::Written)
            }
            ClipboardBackend::Osc52 => {
                ensure!(
                    (1..=MAX_OSC52_BYTES).contains(&self.config.max_bytes),
                    "clipboard.max_bytes must be between 1 and 262144"
                );
                ensure!(
                    text.len() <= self.config.max_bytes,
                    "copy exceeds clipboard.max_bytes ({} bytes)",
                    self.config.max_bytes
                );
                output.write_all(format!("\x1b]52;c;{}\x07", STANDARD.encode(text)).as_bytes())?;
                output.flush()?;
                Ok(CopyOutcome::Sent)
            }
            ClipboardBackend::None | ClipboardBackend::Auto => bail!("{}", self.reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn herdr_without_ssh_variables_uses_the_terminal_clipboard() {
        for marker in ["HERDR_ENV", "TERM_PROGRAM"] {
            let env = Environment::from_env(true, |key| match key {
                "TERM" => Some("xterm-256color".into()),
                key if key == marker => {
                    Some(if marker == "HERDR_ENV" { "1" } else { "herdr" }.into())
                }
                _ => None,
            });
            assert!(!env.remote);
            assert_eq!(env.preferred(), ClipboardBackend::Osc52);
            let mut clipboard =
                SessionClipboard::with_environment(&ClipboardConfig::default(), env);
            let mut output = Vec::new();
            let text = "café 👻\nsecond\r\n\x1b]52;c;bad\x07";
            assert_eq!(
                clipboard.copy(text, &mut output).unwrap(),
                CopyOutcome::Sent
            );
            assert_eq!(&output[..7], b"\x1b]52;c;");
            assert_eq!(
                STANDARD.decode(&output[7..output.len() - 1]).unwrap(),
                text.as_bytes()
            );
            assert_eq!(output.last(), Some(&7));
            assert_eq!(clipboard.get_text().unwrap(), None);
        }
    }

    #[test]
    fn automatic_routing_preserves_native_and_headless_behavior() {
        let mut env = Environment {
            terminal: true,
            term: "xterm-256color".into(),
            ..Environment::default()
        };
        assert_eq!(env.preferred(), ClipboardBackend::Native);
        env.remote = true;
        assert_eq!(env.preferred(), ClipboardBackend::Osc52);
        env.remote = false;
        env.tmux = true;
        assert_eq!(env.preferred(), ClipboardBackend::Osc52);
        env.terminal = false;
        assert_eq!(env.preferred(), ClipboardBackend::None);
        env.terminal = true;
        env.term = "dumb".into();
        assert_eq!(env.preferred(), ClipboardBackend::None);
    }

    #[test]
    fn denied_and_oversized_copies_emit_nothing() {
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
            env.clone(),
        );
        let mut output = Vec::new();
        assert!(clipboard.copy("large", &mut output).is_err());
        assert!(output.is_empty());
        clipboard.copy("ok", &mut output).unwrap();
        assert!(!output.is_empty());
        let mut disabled = SessionClipboard::with_environment(
            &ClipboardConfig {
                enabled: false,
                ..ClipboardConfig::default()
            },
            env,
        );
        assert!(disabled.copy("private", &mut Vec::new()).is_err());
        let mut no_tty = SessionClipboard::with_environment(
            &ClipboardConfig {
                backend: ClipboardBackend::Osc52,
                ..ClipboardConfig::default()
            },
            Environment::default(),
        );
        assert!(no_tty.copy("private", &mut Vec::new()).is_err());
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
