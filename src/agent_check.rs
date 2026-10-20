//! Read-only diagnostics for Red's direct Codex integration.

use std::path::PathBuf;

use serde::Serialize;

use crate::{
    cli_style::{paint, BOLD_GREEN, BOLD_RED, DIM, GREEN, YELLOW},
    codex,
    config::Config,
};

/// Minimum Codex CLI version tested with Red's dynamic-tool app-server contract.
pub const MINIMUM_CODEX_VERSION: &str = "0.144.1";

/// Offline prerequisite report for the direct Codex integration.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentCheckReport {
    /// Whether agent features are enabled by configuration.
    pub enabled: bool,
    /// Configured executable name or path.
    pub command: String,
    /// Executable resolved from the configured command.
    pub executable: Option<PathBuf>,
    /// Raw version output reported by the installed CLI.
    pub installed_version: Option<String>,
    /// Minimum accepted semantic version.
    pub minimum_version: String,
    /// Human-readable authentication expectation.
    pub authentication: String,
    /// Whether executable discovery and version checks passed.
    pub production_ready: bool,
    /// Actionable findings in display order.
    pub messages: Vec<String>,
}

impl AgentCheckReport {
    #[must_use]
    /// Formats a stable multiline report for terminal output.
    pub fn format(&self) -> String {
        self.render(false)
    }

    #[must_use]
    /// Formats the report, dimming labels and coloring the verdicts when
    /// `use_color` is set. The visible text matches [`Self::format`].
    pub fn render(&self, use_color: bool) -> String {
        let field = |label: &str, value: String| {
            format!("{} {value}", paint(DIM, &format!("{label}:"), use_color))
        };
        let mut lines = vec![
            field(
                "agent support",
                if self.enabled {
                    paint(GREEN, "enabled", use_color)
                } else {
                    paint(YELLOW, "disabled", use_color)
                },
            ),
            field("backend", "Codex app-server".to_string()),
            field("command", self.command.clone()),
            field("minimum Codex version", self.minimum_version.clone()),
            field("authentication", self.authentication.clone()),
            field(
                "agent-edit readiness",
                if self.production_ready {
                    paint(BOLD_GREEN, "ready", use_color)
                } else {
                    paint(BOLD_RED, "not ready", use_color)
                },
            ),
        ];
        if let Some(executable) = &self.executable {
            lines.push(field("executable", executable.display().to_string()));
        }
        if let Some(version) = &self.installed_version {
            lines.push(field("installed version", version.clone()));
        }
        lines.extend(
            self.messages
                .iter()
                .map(|message| paint(DIM, &format!("- {message}"), use_color)),
        );
        lines.join("\n")
    }
}

#[must_use]
/// Resolves a configured executable without launching or installing it.
pub fn find_executable_on_path(command: &str) -> Option<PathBuf> {
    codex::find_executable(command)
}

#[must_use]
/// Performs the offline executable and version readiness check.
///
/// Authentication is intentionally not attempted; the first live app-server session
/// verifies the account.
pub fn run(config: &Config) -> AgentCheckReport {
    let command = config
        .agent
        .command
        .clone()
        .unwrap_or_else(|| "codex".to_string());
    if config.disable_ai {
        return AgentCheckReport {
            enabled: false,
            command,
            executable: None,
            installed_version: None,
            minimum_version: MINIMUM_CODEX_VERSION.to_string(),
            authentication: "not checked while `disable_ai = true`".to_string(),
            production_ready: false,
            messages: vec!["Red will not launch Codex.".to_string()],
        };
    }

    let executable = codex::find_executable(&command);
    let installed_version = executable.as_ref().and_then(|executable| {
        std::process::Command::new(executable)
            .arg("--version")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
    });
    let parsed_version = installed_version
        .as_deref()
        .and_then(|version| version.split_whitespace().last())
        .and_then(|version| semver::Version::parse(version).ok());
    let minimum =
        semver::Version::parse(MINIMUM_CODEX_VERSION).expect("minimum Codex version must be valid");
    let version_ready = parsed_version
        .as_ref()
        .is_some_and(|version| version >= &minimum);
    let production_ready = executable.is_some() && version_ready;
    let mut messages = Vec::new();
    if executable.is_none() {
        messages.push(
            "Codex CLI was not found; install Codex, run `codex login`, and try again.".to_string(),
        );
    } else if !version_ready {
        messages.push(format!(
            "Codex {MINIMUM_CODEX_VERSION} or newer is required for the app-server dynamic-tool contract."
        ));
    } else {
        messages.push(
            "The check is offline; authentication is verified when the first session starts."
                .to_string(),
        );
    }
    AgentCheckReport {
        enabled: true,
        command,
        executable,
        installed_version,
        minimum_version: MINIMUM_CODEX_VERSION.to_string(),
        authentication: "installed Codex CLI (`codex login`)".to_string(),
        production_ready,
        messages,
    }
}
