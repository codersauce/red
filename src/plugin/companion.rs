//! Lazy, supervised native companion processes for trusted external plugins.
//!
//! A companion is started only after its owning Husk plugin issues its first RPC
//! request. Messages are newline-delimited JSON with bounded frame sizes. Red supplies
//! monotonically increasing request IDs, enforces timeouts, validates response
//! ownership, and tears children down with the editor.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
    time::timeout,
};

use super::package::{PluginId, PluginPackageManager, PluginPackageManifest};

const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// One successful companion response and any progress frames received before it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CompanionCallResult {
    pub result: Value,
    pub progress: Vec<Value>,
}

#[derive(Debug, Serialize)]
struct CompanionRequest<'a> {
    id: u64,
    method: &'a str,
    params: &'a Value,
}

#[derive(Debug, Deserialize)]
struct CompanionResponse {
    id: u64,
    #[serde(default)]
    result: Value,
    error: Option<CompanionResponseError>,
    progress: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct CompanionResponseError {
    code: Option<String>,
    message: String,
}

struct CompanionProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    diagnostics: Arc<Mutex<Vec<u8>>>,
    next_request_id: u64,
}

/// Owns lazily spawned companion children for one editor instance.
pub struct CompanionManager {
    packages: PluginPackageManager,
    processes: HashMap<PluginId, CompanionProcess>,
}

impl CompanionManager {
    /// Creates an idle manager. Construction performs no process or network work.
    #[must_use]
    pub fn new(config_dir: impl Into<PathBuf>) -> Self {
        Self {
            packages: PluginPackageManager::new(config_dir),
            processes: HashMap::new(),
        }
    }

    /// Calls one companion method and waits for its matching bounded response.
    pub async fn call(
        &mut self,
        owner: &str,
        method: &str,
        params: Value,
        timeout_duration: Option<Duration>,
    ) -> Result<CompanionCallResult> {
        anyhow::ensure!(!method.trim().is_empty(), "companion method is empty");
        let owner = PluginId::parse(owner)?;
        if method == "$cancel" {
            self.stop(&owner).await?;
            return Ok(CompanionCallResult {
                result: serde_json::json!({ "cancelled": true }),
                progress: Vec::new(),
            });
        }
        if !self.processes.contains_key(&owner) {
            let process = self.spawn(&owner).await?;
            self.processes.insert(owner.clone(), process);
        }
        let process = self
            .processes
            .get_mut(&owner)
            .ok_or_else(|| anyhow::anyhow!("companion process disappeared"))?;
        let request_id = process.next_request_id;
        process.next_request_id = process.next_request_id.saturating_add(1).max(1);
        let frame = serde_json::to_vec(&CompanionRequest {
            id: request_id,
            method,
            params: &params,
        })?;
        anyhow::ensure!(
            frame.len() <= MAX_FRAME_BYTES,
            "companion request exceeds {MAX_FRAME_BYTES} bytes"
        );
        process.stdin.write_all(&frame).await?;
        process.stdin.write_all(b"\n").await?;
        process.stdin.flush().await?;

        let duration = timeout_duration.unwrap_or(DEFAULT_TIMEOUT).min(MAX_TIMEOUT);
        let response = timeout(duration, read_response(process, request_id)).await;
        match response {
            Ok(result) => result,
            Err(_) => {
                self.stop(&owner).await?;
                anyhow::bail!(
                    "companion request `{method}` timed out after {} ms",
                    duration.as_millis()
                )
            }
        }
    }

    /// Stops all children owned by this editor instance.
    pub async fn shutdown(&mut self) {
        let owners = self.processes.keys().cloned().collect::<Vec<_>>();
        for owner in owners {
            let _ = self.stop(&owner).await;
        }
    }

    async fn spawn(&self, owner: &PluginId) -> Result<CompanionProcess> {
        let installed = self
            .packages
            .installed(owner)?
            .ok_or_else(|| anyhow::anyhow!("plugin `{owner}` is not installed"))?;
        anyhow::ensure!(installed.enabled, "plugin `{owner}` is disabled");
        let manifest = PluginPackageManifest::load(&installed.package_root)?;
        let command = manifest
            .companion_command(&installed.package_root)
            .ok_or_else(|| {
                anyhow::anyhow!("plugin `{owner}` has no companion for this platform")
            })?;
        ensure_executable_in_package(&command, &installed.package_root)?;
        let data_dir = self.packages.data_dir(owner);
        tokio::fs::create_dir_all(&data_dir)
            .await
            .with_context(|| format!("failed to create {}", data_dir.display()))?;

        let mut child = Command::new(&command)
            .current_dir(&installed.package_root)
            .env("RED_PLUGIN_ID", owner.as_str())
            .env("RED_PLUGIN_PROTOCOL", "1")
            .env("RED_PLUGIN_DATA_DIR", data_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| {
                format!(
                    "failed to start companion for `{owner}` from {}",
                    command.display()
                )
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("companion stdin was not captured"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("companion stdout was not captured"))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("companion stderr was not captured"))?;
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let diagnostic_sink = Arc::clone(&diagnostics);
        tokio::spawn(async move {
            let mut chunk = [0_u8; 1024];
            while let Ok(read) = stderr.read(&mut chunk).await {
                if read == 0 {
                    break;
                }
                let mut retained = diagnostic_sink.lock().await;
                retained.extend_from_slice(&chunk[..read]);
                if retained.len() > MAX_DIAGNOSTIC_BYTES {
                    let overflow = retained.len() - MAX_DIAGNOSTIC_BYTES;
                    retained.drain(..overflow);
                }
            }
        });
        Ok(CompanionProcess {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            diagnostics,
            next_request_id: 1,
        })
    }

    async fn stop(&mut self, owner: &PluginId) -> Result<()> {
        let Some(mut process) = self.processes.remove(owner) else {
            return Ok(());
        };
        if process.child.try_wait()?.is_none() {
            process.child.start_kill()?;
            let _ = timeout(Duration::from_secs(2), process.child.wait()).await;
        }
        Ok(())
    }
}

async fn read_response(
    process: &mut CompanionProcess,
    request_id: u64,
) -> Result<CompanionCallResult> {
    let mut progress = Vec::new();
    loop {
        let mut line = Vec::new();
        let read = process
            .stdout
            .read_until(b'\n', &mut line)
            .await
            .context("failed to read companion response")?;
        if read == 0 {
            let diagnostics = process.diagnostics.lock().await;
            let message = String::from_utf8_lossy(&diagnostics).trim().to_string();
            if message.is_empty() {
                anyhow::bail!("companion exited before responding");
            }
            anyhow::bail!("companion exited before responding: {message}");
        }
        anyhow::ensure!(
            line.len() <= MAX_FRAME_BYTES,
            "companion response exceeds {MAX_FRAME_BYTES} bytes"
        );
        let response: CompanionResponse =
            serde_json::from_slice(&line).context("companion returned invalid JSON")?;
        anyhow::ensure!(
            response.id == request_id,
            "companion response id {} does not match request {request_id}",
            response.id
        );
        if let Some(value) = response.progress {
            progress.push(value);
            continue;
        }
        if let Some(error) = response.error {
            let code = error.code.unwrap_or_else(|| "companion_error".to_string());
            anyhow::bail!("{code}: {}", error.message);
        }
        return Ok(CompanionCallResult {
            result: response.result,
            progress,
        });
    }
}

fn ensure_executable_in_package(command: &Path, package_root: &Path) -> Result<()> {
    let command = command
        .canonicalize()
        .with_context(|| format!("failed to resolve companion {}", command.display()))?;
    let root = package_root
        .canonicalize()
        .with_context(|| format!("failed to resolve package {}", package_root.display()))?;
    anyhow::ensure!(
        command.starts_with(&root),
        "companion executable escapes its plugin package"
    );
    anyhow::ensure!(command.is_file(), "companion executable is not a file");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn companion_paths_cannot_escape_the_package() {
        let package = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        assert!(ensure_executable_in_package(outside.path(), package.path()).is_err());
    }

    #[test]
    fn manager_construction_never_starts_or_scans_a_companion() {
        let config = tempfile::tempdir().unwrap();
        let manager = CompanionManager::new(config.path());

        assert!(manager.processes.is_empty());
        assert!(!config.path().join("plugin-data").exists());
    }
}
