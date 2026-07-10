//! Read-only ACP adapter prerequisite diagnostics.

use std::path::PathBuf;

use serde::Serialize;

use crate::{acp, config::Config};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterDescriptor {
    pub id: &'static str,
    pub program: &'static str,
    pub tested_version: &'static str,
    pub production_supported: bool,
    pub authentication: &'static str,
}

/// Adapters Red can discover without a configuration file.
///
/// The conformance fixture is intentionally not a production-supported agent. A real
/// adapter is promoted only after its edit path passes the client-filesystem conformance
/// test; conversation-only adapters must not be presented as reviewable-edit support.
pub const ADAPTER_REGISTRY: &[AdapterDescriptor] = &[AdapterDescriptor {
    id: "conformance",
    program: "acp_conformance_fixture",
    tested_version: env!("CARGO_PKG_VERSION"),
    production_supported: false,
    authentication: "not required (development fixture)",
}];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentCheckReport {
    pub enabled: bool,
    pub adapter_id: String,
    pub command: Option<String>,
    pub executable: Option<PathBuf>,
    pub schema_version: String,
    pub wire_protocol: String,
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
            format!("adapter: {}", self.adapter_id),
            format!("ACP schema artifact: {}", self.schema_version),
            format!("ACP wire protocol: {}", self.wire_protocol),
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
        if let Some(command) = &self.command {
            lines.push(format!("command: {command}"));
        }
        if let Some(executable) = &self.executable {
            lines.push(format!("executable: {}", executable.display()));
        }
        lines.extend(self.messages.iter().map(|message| format!("- {message}")));
        lines.join("\n")
    }
}

#[must_use]
pub fn registry_adapter(id: &str) -> Option<&'static AdapterDescriptor> {
    ADAPTER_REGISTRY.iter().find(|adapter| adapter.id == id)
}

pub fn run(config: &Config) -> AgentCheckReport {
    if config.disable_ai {
        return AgentCheckReport {
            enabled: false,
            adapter_id: "disabled".to_string(),
            command: None,
            executable: None,
            schema_version: acp::SCHEMA_ARTIFACT_VERSION.to_string(),
            wire_protocol: acp::WIRE_PROTOCOL_VERSION.to_string(),
            authentication: "not checked while disable_ai = true".to_string(),
            production_ready: false,
            messages: vec![
                "No adapter process was spawned and no authentication or network check was performed."
                    .to_string(),
            ],
        };
    }

    let descriptor = config.agent.adapter.as_deref().and_then(registry_adapter);
    let command = config
        .agent
        .command
        .clone()
        .or_else(|| descriptor.map(|adapter| adapter.program.to_string()));
    let executable = command.as_deref().and_then(find_executable);
    let adapter_id = descriptor
        .map(|adapter| adapter.id.to_string())
        .unwrap_or_else(|| {
            if command.is_some() {
                "custom".to_string()
            } else {
                "unconfigured".to_string()
            }
        });
    let authentication = descriptor
        .map(|adapter| adapter.authentication.to_string())
        .unwrap_or_else(|| {
            "custom adapter authentication must be completed externally".to_string()
        });
    let production_ready =
        descriptor.is_some_and(|adapter| adapter.production_supported) && executable.is_some();
    let mut messages = Vec::new();
    if config.agent.adapter.is_some() && descriptor.is_none() {
        messages.push(format!(
            "Unknown built-in adapter {:?}; choose a registry id or set agent.command.",
            config.agent.adapter
        ));
    }
    match (&command, &executable) {
        (None, _) => messages.push(
            "No production ACP adapter is configured. Red will not install or download one."
                .to_string(),
        ),
        (Some(command), None) => messages.push(format!(
            "Adapter executable {command:?} was not found on PATH or at the configured path."
        )),
        (Some(_), Some(_)) if descriptor.is_none() => messages.push(
            "Custom adapter found; reviewable-edit readiness requires the Red filesystem conformance suite."
                .to_string(),
        ),
        (Some(_), Some(_)) if !production_ready => messages.push(
            "Development fixture found; it is not a production agent or launch prerequisite."
                .to_string(),
        ),
        _ => {}
    }

    AgentCheckReport {
        enabled: true,
        adapter_id,
        command,
        executable,
        schema_version: acp::SCHEMA_ARTIFACT_VERSION.to_string(),
        wire_protocol: acp::WIRE_PROTOCOL_VERSION.to_string(),
        authentication,
        production_ready,
        messages,
    }
}

fn find_executable(command: &str) -> Option<PathBuf> {
    let candidate = PathBuf::from(command);
    if candidate.components().count() > 1 {
        return candidate.is_file().then_some(candidate);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(command))
        .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_check_is_inert_and_explicit() {
        let config = Config {
            disable_ai: true,
            agent: crate::config::AgentConfig {
                command: Some("must-not-run".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };

        let report = run(&config);

        assert!(!report.enabled);
        assert!(report.executable.is_none());
        assert!(report.format().contains("No adapter process was spawned"));
    }

    #[test]
    fn missing_custom_adapter_has_actionable_output() {
        let config = Config {
            agent: crate::config::AgentConfig {
                command: Some("red-definitely-missing-adapter".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };

        let report = run(&config);

        assert_eq!(report.adapter_id, "custom");
        assert!(!report.production_ready);
        assert!(report.format().contains("was not found on PATH"));
    }
}
