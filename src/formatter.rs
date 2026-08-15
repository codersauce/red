//! External document formatter discovery and execution.
//!
//! Language packs describe stdin-to-stdout tools. Red resolves project-local
//! executables before `PATH`, launches them without a shell, and bounds each run.

use std::{
    env,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{anyhow, Context};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    time::timeout,
};

use crate::config::LanguageFormatterConfig;

const FORMAT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_FORMATTED_BYTES: usize = 16 * 1024 * 1024;
const MAX_ERROR_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub struct FormattedDocument {
    pub name: String,
    pub contents: String,
}

/// Returns the workspace used for formatter discovery and execution.
#[must_use]
pub fn workspace_root(file: &Path, root_markers: &[String]) -> PathBuf {
    let parent = file.parent().unwrap_or_else(|| Path::new("."));
    if !root_markers.is_empty() {
        for ancestor in parent.ancestors() {
            if root_markers
                .iter()
                .any(|marker| ancestor.join(marker).exists())
            {
                return ancestor.to_path_buf();
            }
        }
    }
    parent.to_path_buf()
}

/// Resolves an executable, preferring conventional project-local tool directories.
#[must_use]
pub fn resolve_command(command: &str, workspace: &Path) -> Option<PathBuf> {
    let command_path = Path::new(command);
    if command_path.components().count() > 1 {
        let candidate = if command_path.is_absolute() {
            command_path.to_path_buf()
        } else {
            workspace.join(command_path)
        };
        return executable_candidate(&candidate);
    }

    for directory in [
        workspace.join("node_modules/.bin"),
        workspace.join(".venv/bin"),
        workspace.join("venv/bin"),
        workspace.join("vendor/bin"),
    ] {
        if let Some(candidate) = executable_candidate(&directory.join(command)) {
            return Some(candidate);
        }
    }

    env::var_os("PATH")
        .into_iter()
        .flat_map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .find_map(|directory| executable_candidate(&directory.join(command)))
}

fn executable_candidate(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if path.metadata().ok()?.permissions().mode() & 0o111 == 0 {
                return None;
            }
        }
        return Some(path.to_path_buf());
    }
    #[cfg(windows)]
    for extension in ["exe", "cmd", "bat"] {
        let candidate = path.with_extension(extension);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

async fn read_bounded(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut result = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0_u8; 8192];
    let mut overflowed = false;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            return Ok((result, overflowed));
        }
        let remaining = limit.saturating_sub(result.len());
        result.extend_from_slice(&buffer[..read.min(remaining)]);
        overflowed |= read > remaining;
    }
}

#[must_use]
pub fn is_available(config: &LanguageFormatterConfig, file: &Path) -> bool {
    let workspace = workspace_root(file, &config.root_markers);
    resolve_command(&config.command, &workspace).is_some()
}

/// Formats `contents`, returning `None` when the configured executable is unavailable.
pub async fn format_document(
    config: &LanguageFormatterConfig,
    file: &Path,
    contents: &str,
) -> anyhow::Result<Option<FormattedDocument>> {
    let workspace = workspace_root(file, &config.root_markers);
    let Some(executable) = resolve_command(&config.command, &workspace) else {
        return Ok(None);
    };
    let file = file.to_string_lossy();
    let workspace_text = workspace.to_string_lossy();
    let arguments = config.args.iter().map(|argument| {
        argument
            .replace("{file}", &file)
            .replace("{workspace}", &workspace_text)
    });
    let environment = config.env.iter().map(|(key, value)| {
        (
            key,
            value
                .replace("{file}", &file)
                .replace("{workspace}", &workspace_text),
        )
    });
    let mut command = Command::new(&executable);
    command
        .args(arguments)
        .current_dir(&workspace)
        .envs(environment)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to launch formatter {}", config.name))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("formatter {} has no stdin", config.name))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("formatter {} has no stdout", config.name))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("formatter {} has no stderr", config.name))?;
    let input = contents.as_bytes();
    let run = async move {
        let write_input = async move {
            stdin.write_all(input).await?;
            drop(stdin);
            Ok::<_, std::io::Error>(())
        };
        tokio::join!(
            write_input,
            read_bounded(stdout, MAX_FORMATTED_BYTES),
            read_bounded(stderr, MAX_ERROR_BYTES),
            child.wait()
        )
    };
    let (write_result, stdout_result, stderr_result, status_result) = timeout(FORMAT_TIMEOUT, run)
        .await
        .map_err(|_| anyhow!("formatter {} timed out after 30 seconds", config.name))?;
    let status = status_result?;
    let (stdout, stdout_overflowed) = stdout_result?;
    let (stderr, _) = stderr_result?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        let detail = stderr.trim();
        return Err(if detail.is_empty() {
            anyhow!("formatter {} exited with {}", config.name, status)
        } else {
            anyhow!("formatter {} exited with {}: {detail}", config.name, status)
        });
    }
    write_result
        .with_context(|| format!("failed to write document to formatter {}", config.name))?;
    anyhow::ensure!(
        !stdout_overflowed,
        "formatter {} returned more than 16 MiB",
        config.name
    );
    let contents = String::from_utf8(stdout)
        .with_context(|| format!("formatter {} returned non-UTF-8 output", config.name))?;
    Ok(Some(FormattedDocument {
        name: if config.name.trim().is_empty() {
            config.command.clone()
        } else {
            config.name.clone()
        },
        contents,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_root_uses_nearest_marker() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("src/deep");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(temp.path().join("project.toml"), "").unwrap();
        assert_eq!(
            workspace_root(
                &nested.join("main.rs"),
                &["project.toml".to_string(), ".git".to_string()]
            ),
            temp.path()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_local_formatter_with_placeholders() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("node_modules/.bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(temp.path().join("project.toml"), "").unwrap();
        let script = bin.join("test-format");
        std::fs::write(&script, "#!/bin/sh\ncat | tr 'a-z' 'A-Z'\n").unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).unwrap();
        let file = temp.path().join("src/main.test");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let config = LanguageFormatterConfig {
            name: "Test Format".to_string(),
            command: "test-format".to_string(),
            args: vec!["{file}".to_string(), "{workspace}".to_string()],
            root_markers: vec!["project.toml".to_string()],
            env: Default::default(),
        };

        let formatted = format_document(&config, &file, "hello\n")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(formatted.name, "Test Format");
        assert_eq!(formatted.contents, "HELLO\n");
    }
}
