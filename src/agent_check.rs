//! Read-only diagnostics for Red's direct Codex integration.

use std::path::PathBuf;

use serde::Serialize;

use crate::{codex, config::Config};

pub const MINIMUM_CODEX_VERSION: &str = "0.144.1";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentCheckReport {
    pub enabled: bool,
    pub command: String,
    pub executable: Option<PathBuf>,
    pub installed_version: Option<String>,
    pub minimum_version: String,
    pub authentication: String,
    pub production_ready: bool,
    pub messages: Vec<String>,
}

impl AgentCheckReport {
    #[must_use]
    pub fn format(&self) -> String {
        let mut lines = vec![
            format!(
                "agent support: {}",
                if self.enabled { "enabled" } else { "disabled" }
            ),
            "backend: Codex app-server".to_string(),
            format!("command: {}", self.command),
            format!("minimum Codex version: {}", self.minimum_version),
            format!("authentication: {}", self.authentication),
            format!(
                "reviewable-edit readiness: {}",
                if self.production_ready {
                    "ready"
                } else {
                    "not ready"
                }
            ),
        ];
        if let Some(executable) = &self.executable {
            lines.push(format!("executable: {}", executable.display()));
        }
        if let Some(version) = &self.installed_version {
            lines.push(format!("installed version: {version}"));
        }
        lines.extend(self.messages.iter().map(|message| format!("- {message}")));
        lines.join("\n")
    }
}

#[must_use]
pub fn find_executable_on_path(command: &str) -> Option<PathBuf> {
    codex::find_executable(command)
}

#[must_use]
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
