//! Read-only ACP adapter prerequisite diagnostics.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use serde::Serialize;

use crate::{acp, config::Config};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterDescriptor {
    pub id: &'static str,
    pub program: &'static str,
    pub tested_version: &'static str,
    pub production_supported: bool,
    pub authentication: &'static str,
    pub required_env: Option<&'static str>,
    pub required_program: Option<&'static str>,
}

/// Adapters Red can discover without a configuration file.
///
/// The conformance fixture is intentionally not a production-supported agent. A real
/// adapter is promoted only after its edit path passes the client-filesystem conformance
/// test; conversation-only adapters must not be presented as reviewable-edit support.
pub const ADAPTER_REGISTRY: &[AdapterDescriptor] = &[
    AdapterDescriptor {
        id: "openai",
        program: "red_openai_acp",
        tested_version: env!("CARGO_PKG_VERSION"),
        production_supported: true,
        authentication: "OPENAI_API_KEY (environment or [agent.env])",
        required_env: Some("OPENAI_API_KEY"),
        required_program: None,
    },
    AdapterDescriptor {
        id: "codex",
        program: "red_codex_acp",
        tested_version: "codex-cli 0.144.1",
        production_supported: true,
        authentication: "installed Codex CLI (`codex login`)",
        required_env: None,
        required_program: Some("codex"),
    },
    AdapterDescriptor {
        id: "conformance",
        program: "acp_conformance_fixture",
        tested_version: env!("CARGO_PKG_VERSION"),
        production_supported: false,
        authentication: "not required (development fixture)",
        required_env: None,
        required_program: None,
    },
];

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

/// Resolve the configured ACP command, preferring a bundled companion beside Red.
///
/// Explicit custom commands keep their precedence. A missing command is returned
/// unchanged so process startup can report its normal spawn error.
#[must_use]
pub fn resolve_adapter_command(config: &Config) -> Option<PathBuf> {
    let path = std::env::var_os("PATH");
    let current_exe = std::env::current_exe().ok();
    resolve_adapter_command_with_environment(config, path.as_deref(), current_exe.as_deref())
}

/// Find a runnable ACP executable on the current `PATH` or at an explicit path.
#[must_use]
pub fn find_executable_on_path(command: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH");
    find_executable(command, path.as_deref())
}

pub fn run(config: &Config) -> AgentCheckReport {
    let path = std::env::var_os("PATH");
    let api_key = std::env::var_os("OPENAI_API_KEY");
    let current_exe = std::env::current_exe().ok();
    run_with_environment(
        config,
        path.as_deref(),
        api_key.as_deref(),
        current_exe.as_deref(),
    )
}

fn run_with_environment(
    config: &Config,
    search_path: Option<&OsStr>,
    inherited_api_key: Option<&OsStr>,
    current_exe: Option<&Path>,
) -> AgentCheckReport {
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
    let executable = resolve_adapter_command_with_environment(config, search_path, current_exe)
        .filter(|path| is_executable(path));
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
    let required_env_ready = descriptor.is_none_or(|adapter| {
        adapter.required_env.is_none_or(|name| {
            if let Some(value) = config.agent.env.get(name) {
                !value.trim().is_empty()
            } else if name == "OPENAI_API_KEY" {
                inherited_api_key.is_some_and(|value| !value.to_string_lossy().trim().is_empty())
            } else {
                std::env::var_os(name)
                    .is_some_and(|value| !value.to_string_lossy().trim().is_empty())
            }
        })
    });
    let trusted_command = descriptor.is_some() && config.agent.command.is_none();
    let required_program_ready = descriptor.is_none_or(|adapter| {
        adapter
            .required_program
            .is_none_or(|program| find_executable(program, search_path).is_some())
    });
    let production_ready = descriptor.is_some_and(|adapter| adapter.production_supported)
        && executable.is_some()
        && trusted_command
        && required_env_ready
        && required_program_ready;
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
        (Some(command), None) if config.agent.command.is_none() => messages.push(format!(
            "Bundled adapter executable {command:?} was not found beside Red or on PATH; reinstall Red with its companion executable."
        )),
        (Some(command), None) => messages.push(format!(
            "Adapter executable {command:?} was not found on PATH or at the configured path."
        )),
        (Some(_), Some(_)) if descriptor.is_none() => messages.push(
            "Custom adapter found; reviewable-edit readiness requires the Red filesystem conformance suite."
                .to_string(),
        ),
        (Some(_), Some(_)) if !trusted_command => messages.push(
            "The configured command overrides the built-in adapter; reviewable-edit readiness requires the Red filesystem conformance suite."
                .to_string(),
        ),
        (Some(_), Some(_)) if !required_env_ready => messages.push(format!(
            "Required adapter credential {} is not set; Red will not expose or persist its value.",
            descriptor
                .and_then(|adapter| adapter.required_env)
                .unwrap_or("<unknown>")
        )),
        (Some(_), Some(_)) if !required_program_ready => messages.push(format!(
            "Required adapter program {} was not found on PATH; install Codex, run `codex login`, and try again.",
            descriptor
                .and_then(|adapter| adapter.required_program)
                .unwrap_or("<unknown>")
        )),
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

fn resolve_adapter_command_with_environment(
    config: &Config,
    search_path: Option<&OsStr>,
    current_exe: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(command) = config.agent.command.as_deref() {
        return Some(
            find_executable(command, search_path).unwrap_or_else(|| PathBuf::from(command)),
        );
    }

    let descriptor = config.agent.adapter.as_deref().and_then(registry_adapter)?;
    let sibling = current_exe
        .and_then(Path::parent)
        .and_then(|directory| find_in_directory(directory, descriptor.program));
    sibling
        .or_else(|| find_executable(descriptor.program, search_path))
        .or_else(|| Some(PathBuf::from(descriptor.program)))
}

fn find_executable(command: &str, search_path: Option<&OsStr>) -> Option<PathBuf> {
    let candidate = PathBuf::from(command);
    if candidate.components().count() > 1 {
        return is_executable(&candidate).then_some(candidate);
    }
    let path = search_path?;
    std::env::split_paths(path).find_map(|directory| find_in_directory(&directory, command))
}

#[cfg(not(windows))]
fn find_in_directory(directory: &std::path::Path, command: &str) -> Option<PathBuf> {
    let path = directory.join(command);
    is_executable(&path).then_some(path)
}

#[cfg(windows)]
fn find_in_directory(directory: &std::path::Path, command: &str) -> Option<PathBuf> {
    let path = directory.join(command);
    if is_executable(&path) {
        return Some(path);
    }
    if path.extension().is_some() {
        return None;
    }
    let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    extensions
        .split(';')
        .filter(|extension| !extension.is_empty())
        .map(|extension| directory.join(format!("{command}{extension}")))
        .find(|path| is_executable(path))
}

fn is_executable(path: &std::path::Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

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

    #[test]
    fn openai_adapter_reports_ready_without_exposing_configured_key() {
        let binary = tempfile::tempdir().unwrap();
        let program = binary.path().join("red_openai_acp");
        std::fs::write(&program, "test").unwrap();
        make_executable(&program);
        let secret = "test-secret-that-must-not-be-rendered";
        let config = Config {
            agent: crate::config::AgentConfig {
                adapter: Some("openai".to_string()),
                env: HashMap::from([("OPENAI_API_KEY".to_string(), secret.to_string())]),
                ..Default::default()
            },
            ..Default::default()
        };

        let report = run_with_environment(&config, Some(binary.path().as_os_str()), None, None);

        assert!(report.production_ready);
        assert_eq!(report.adapter_id, "openai");
        assert!(report.format().contains("OPENAI_API_KEY"));
        assert!(!report.format().contains(secret));
    }

    #[test]
    fn overriding_openai_command_never_claims_reviewable_readiness() {
        let binary = tempfile::tempdir().unwrap();
        let program = binary.path().join("untrusted-adapter");
        std::fs::write(&program, "test").unwrap();
        make_executable(&program);
        let config = Config {
            agent: crate::config::AgentConfig {
                adapter: Some("openai".to_string()),
                command: Some(program.to_string_lossy().into_owned()),
                env: HashMap::from([("OPENAI_API_KEY".to_string(), "present".to_string())]),
                ..Default::default()
            },
            ..Default::default()
        };

        let report = run(&config);

        assert!(!report.production_ready);
        assert!(report.format().contains("overrides the built-in adapter"));
    }

    #[test]
    fn whitespace_agent_env_key_overrides_an_inherited_key_and_is_not_ready() {
        let binary = tempfile::tempdir().unwrap();
        let program = binary.path().join("red_openai_acp");
        std::fs::write(&program, "test").unwrap();
        make_executable(&program);
        let config = Config {
            agent: crate::config::AgentConfig {
                adapter: Some("openai".to_string()),
                env: HashMap::from([("OPENAI_API_KEY".to_string(), "   ".to_string())]),
                ..Default::default()
            },
            ..Default::default()
        };

        let report = run_with_environment(
            &config,
            Some(binary.path().as_os_str()),
            Some(OsStr::new("inherited-secret")),
            None,
        );

        assert!(!report.production_ready);
        assert!(report
            .format()
            .contains("credential OPENAI_API_KEY is not set"));
        assert!(!report.format().contains("inherited-secret"));
    }

    #[test]
    fn bundled_adapter_prefers_companion_beside_red_over_path() {
        let bundle = tempfile::tempdir().unwrap();
        let path = tempfile::tempdir().unwrap();
        let red = bundle.path().join("red");
        let companion = bundle.path().join("red_openai_acp");
        let other = path.path().join("red_openai_acp");
        std::fs::write(&companion, "bundled").unwrap();
        std::fs::write(&other, "path").unwrap();
        make_executable(&companion);
        make_executable(&other);
        let config = Config {
            agent: crate::config::AgentConfig {
                adapter: Some("openai".to_string()),
                env: HashMap::from([("OPENAI_API_KEY".to_string(), "present".to_string())]),
                ..Default::default()
            },
            ..Default::default()
        };

        let resolved = resolve_adapter_command_with_environment(
            &config,
            Some(path.path().as_os_str()),
            Some(&red),
        );
        let report = run_with_environment(&config, Some(path.path().as_os_str()), None, Some(&red));

        assert_eq!(resolved.as_deref(), Some(companion.as_path()));
        assert_eq!(report.executable.as_deref(), Some(companion.as_path()));
        assert!(report.production_ready);
    }

    #[test]
    fn bundled_codex_adapter_is_ready_when_the_installed_cli_is_available() {
        let bundle = tempfile::tempdir().unwrap();
        let path = tempfile::tempdir().unwrap();
        let red = bundle.path().join("red");
        let companion = bundle.path().join("red_codex_acp");
        let codex = path.path().join("codex");
        std::fs::write(&companion, "bundled").unwrap();
        std::fs::write(&codex, "installed").unwrap();
        make_executable(&companion);
        make_executable(&codex);
        let config = Config {
            agent: crate::config::AgentConfig {
                adapter: Some("codex".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };

        let report = run_with_environment(&config, Some(path.path().as_os_str()), None, Some(&red));

        assert!(report.production_ready);
        assert_eq!(report.adapter_id, "codex");
        assert_eq!(report.executable.as_deref(), Some(companion.as_path()));
        assert!(report.format().contains("codex login"));
    }

    #[test]
    fn bundled_codex_adapter_reports_a_missing_cli_without_claiming_readiness() {
        let bundle = tempfile::tempdir().unwrap();
        let red = bundle.path().join("red");
        let companion = bundle.path().join("red_codex_acp");
        std::fs::write(&companion, "bundled").unwrap();
        make_executable(&companion);
        let config = Config {
            agent: crate::config::AgentConfig {
                adapter: Some("codex".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };

        let report = run_with_environment(&config, None, None, Some(&red));

        assert!(!report.production_ready);
        assert!(report
            .format()
            .contains("Required adapter program codex was not found on PATH"));
        assert!(report.format().contains("codex login"));
    }

    #[test]
    fn bundled_adapter_falls_back_to_path_when_companion_is_absent() {
        let bundle = tempfile::tempdir().unwrap();
        let path = tempfile::tempdir().unwrap();
        let red = bundle.path().join("red");
        let program = path.path().join("red_openai_acp");
        std::fs::write(&program, "path").unwrap();
        make_executable(&program);
        let config = Config {
            agent: crate::config::AgentConfig {
                adapter: Some("openai".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };

        let resolved = resolve_adapter_command_with_environment(
            &config,
            Some(path.path().as_os_str()),
            Some(&red),
        );

        assert_eq!(resolved.as_deref(), Some(program.as_path()));
    }

    #[test]
    fn missing_bundled_adapter_has_reinstall_remediation() {
        let bundle = tempfile::tempdir().unwrap();
        let red = bundle.path().join("red");
        let config = Config {
            agent: crate::config::AgentConfig {
                adapter: Some("openai".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };

        let report = run_with_environment(&config, None, None, Some(&red));

        assert!(!report.production_ready);
        assert!(report.executable.is_none());
        assert!(report.format().contains("not found beside Red or on PATH"));
        assert!(report.format().contains("reinstall Red"));
    }

    #[test]
    fn custom_command_never_uses_a_bundled_companion() {
        let bundle = tempfile::tempdir().unwrap();
        let red = bundle.path().join("red");
        let companion = bundle.path().join("red_openai_acp");
        std::fs::write(&companion, "bundled").unwrap();
        make_executable(&companion);
        let config = Config {
            agent: crate::config::AgentConfig {
                adapter: Some("openai".to_string()),
                command: Some("custom-acp".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };

        let resolved = resolve_adapter_command_with_environment(&config, None, Some(&red));
        let report = run_with_environment(&config, None, None, Some(&red));

        assert_eq!(resolved.as_deref(), Some(Path::new("custom-acp")));
        assert!(report.executable.is_none());
        assert!(!report.production_ready);
    }

    #[cfg(unix)]
    fn make_executable(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(not(unix))]
    fn make_executable(_: &std::path::Path) {}
}
