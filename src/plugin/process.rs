use std::{
    collections::{HashMap, VecDeque},
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::PluginPermissions;

const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);
pub const MAX_PROCESSES_PER_PLUGIN: usize = 16;

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProcessSpawnOptions {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub stdin: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub raw_output: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ProcessEvent {
    Stdout {
        plugin_name: String,
        process_id: String,
        line: String,
    },
    Stderr {
        plugin_name: String,
        process_id: String,
        line: String,
    },
    Exit {
        plugin_name: String,
        process_id: String,
        code: Option<i32>,
    },
    Error {
        plugin_name: String,
        process_id: String,
        message: String,
    },
}

impl ProcessEvent {
    fn process_id(&self) -> &str {
        match self {
            Self::Stdout { process_id, .. }
            | Self::Stderr { process_id, .. }
            | Self::Exit { process_id, .. }
            | Self::Error { process_id, .. } => process_id,
        }
    }

    fn is_exit(&self) -> bool {
        matches!(self, Self::Exit { .. })
    }
}

#[derive(Debug)]
struct ManagedProcess {
    plugin_name: String,
    kill_sender: Sender<()>,
}

pub struct ProcessManager {
    permissions: HashMap<String, PluginPermissions>,
    processes: HashMap<String, ManagedProcess>,
    event_sender: Sender<ProcessEvent>,
    event_receiver: Receiver<ProcessEvent>,
    pending_events: VecDeque<ProcessEvent>,
}

impl ProcessManager {
    pub fn new(permissions: HashMap<String, PluginPermissions>) -> Self {
        let (event_sender, event_receiver) = mpsc::channel();
        Self {
            permissions,
            processes: HashMap::new(),
            event_sender,
            event_receiver,
            pending_events: VecDeque::new(),
        }
    }

    pub fn spawn(
        &mut self,
        plugin_name: &str,
        options: ProcessSpawnOptions,
    ) -> anyhow::Result<String> {
        self.refresh_events();
        self.require_command_permission(plugin_name, &options.command)?;

        if self.active_process_count(plugin_name) >= MAX_PROCESSES_PER_PLUGIN {
            anyhow::bail!(
                "plugin `{plugin_name}` already has the maximum of {MAX_PROCESSES_PER_PLUGIN} active processes"
            );
        }

        let mut command = Command::new(&options.command);
        command
            .args(&options.args)
            .stdin(if options.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in &options.env {
            anyhow::ensure!(
                is_allowed_environment_key(key),
                "plugin process environment variable `{key}` is not allowed"
            );
            command.env(key, value);
        }
        if let Some(cwd) = options.cwd {
            command.current_dir(cwd);
        }

        let mut child = command.spawn().map_err(|error| {
            anyhow::anyhow!(
                "plugin `{plugin_name}` failed to spawn `{}`: {error}",
                options.command
            )
        })?;
        if let Some(input) = options.stdin {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| anyhow::anyhow!("spawned process did not provide stdin"))?;
            stdin.write_all(input.as_bytes()).map_err(|error| {
                anyhow::anyhow!("plugin `{plugin_name}` failed to write process stdin: {error}")
            })?;
        }
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("spawned process did not provide stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("spawned process did not provide stderr"))?;
        let process_id = Uuid::new_v4().to_string();
        let (kill_sender, kill_receiver) = mpsc::channel();

        let stdout_reader = spawn_output_reader(
            stdout,
            OutputStream::Stdout,
            plugin_name.to_string(),
            process_id.clone(),
            self.event_sender.clone(),
            options.raw_output,
        );
        let stderr_reader = spawn_output_reader(
            stderr,
            OutputStream::Stderr,
            plugin_name.to_string(),
            process_id.clone(),
            self.event_sender.clone(),
            options.raw_output,
        );
        spawn_process_supervisor(
            child,
            plugin_name.to_string(),
            process_id.clone(),
            kill_receiver,
            self.event_sender.clone(),
            [stdout_reader, stderr_reader],
        );

        self.processes.insert(
            process_id.clone(),
            ManagedProcess {
                plugin_name: plugin_name.to_string(),
                kill_sender,
            },
        );
        Ok(process_id)
    }

    pub fn kill(&mut self, plugin_name: &str, process_id: &str) -> anyhow::Result<()> {
        self.require_process_permission(plugin_name)?;
        self.refresh_events();
        let Some(process) = self.processes.get(process_id) else {
            return Ok(());
        };
        if process.plugin_name != plugin_name {
            return Ok(());
        }
        let _ = process.kill_sender.send(());
        Ok(())
    }

    pub fn poll_events(&mut self) -> Vec<ProcessEvent> {
        self.refresh_events();
        self.pending_events.drain(..).collect()
    }

    pub fn active_process_count(&self, plugin_name: &str) -> usize {
        self.processes
            .values()
            .filter(|process| process.plugin_name == plugin_name)
            .count()
    }

    pub fn shutdown(&mut self) {
        for process in self.processes.values() {
            let _ = process.kill_sender.send(());
        }
        self.processes.clear();
    }

    fn require_command_permission(&self, plugin_name: &str, command: &str) -> anyhow::Result<()> {
        let permissions = self.require_process_permission(plugin_name)?;
        if permissions.process.iter().any(|allowed| allowed == command) {
            return Ok(());
        }
        anyhow::bail!("plugin `{plugin_name}` is not allowed to run `{command}`")
    }

    fn require_process_permission(&self, plugin_name: &str) -> anyhow::Result<&PluginPermissions> {
        let Some(permissions) = self.permissions.get(plugin_name) else {
            anyhow::bail!("plugin `{plugin_name}` does not have process permissions configured");
        };
        if permissions.process.is_empty() {
            anyhow::bail!("plugin `{plugin_name}` does not have process permissions configured");
        }
        Ok(permissions)
    }

    fn refresh_events(&mut self) {
        while let Ok(event) = self.event_receiver.try_recv() {
            if event.is_exit() {
                self.processes.remove(event.process_id());
            }
            self.pending_events.push_back(event);
        }
    }
}

fn is_allowed_environment_key(key: &str) -> bool {
    matches!(
        key,
        "GIT_PAGER"
            | "GIT_EDITOR"
            | "GIT_SEQUENCE_EDITOR"
            | "GIT_TERMINAL_PROMPT"
            | "GIT_OPTIONAL_LOCKS"
            | "LC_ALL"
            | "LANG"
            | "NO_COLOR"
            | "RED_PROCESS_EDITOR_CONTENT"
    )
}

impl Drop for ProcessManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Clone, Copy)]
enum OutputStream {
    Stdout,
    Stderr,
}

fn spawn_output_reader(
    output: impl Read + Send + 'static,
    stream: OutputStream,
    plugin_name: String,
    process_id: String,
    event_sender: Sender<ProcessEvent>,
    raw_output: bool,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = BufReader::new(output);
        if raw_output {
            let mut bytes = Vec::new();
            match reader.read_to_end(&mut bytes) {
                Ok(_) if !bytes.is_empty() => {
                    let line = String::from_utf8_lossy(&bytes).into_owned();
                    let event = match stream {
                        OutputStream::Stdout => ProcessEvent::Stdout {
                            plugin_name,
                            process_id,
                            line,
                        },
                        OutputStream::Stderr => ProcessEvent::Stderr {
                            plugin_name,
                            process_id,
                            line,
                        },
                    };
                    let _ = event_sender.send(event);
                }
                Ok(_) => {}
                Err(error) => {
                    let _ = event_sender.send(ProcessEvent::Error {
                        plugin_name,
                        process_id,
                        message: format!("failed to read process output: {error}"),
                    });
                }
            }
            return;
        }
        let mut bytes = Vec::new();
        loop {
            bytes.clear();
            match reader.read_until(b'\n', &mut bytes) {
                Ok(0) => break,
                Ok(_) => {
                    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
                        bytes.pop();
                    }
                    let line = String::from_utf8_lossy(&bytes).into_owned();
                    let event = match stream {
                        OutputStream::Stdout => ProcessEvent::Stdout {
                            plugin_name: plugin_name.clone(),
                            process_id: process_id.clone(),
                            line,
                        },
                        OutputStream::Stderr => ProcessEvent::Stderr {
                            plugin_name: plugin_name.clone(),
                            process_id: process_id.clone(),
                            line,
                        },
                    };
                    if event_sender.send(event).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = event_sender.send(ProcessEvent::Error {
                        plugin_name,
                        process_id,
                        message: format!("failed to read process output: {error}"),
                    });
                    break;
                }
            }
        }
    })
}

fn spawn_process_supervisor(
    mut child: std::process::Child,
    plugin_name: String,
    process_id: String,
    kill_receiver: Receiver<()>,
    event_sender: Sender<ProcessEvent>,
    output_readers: [thread::JoinHandle<()>; 2],
) {
    thread::spawn(move || loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                join_output_readers(output_readers);
                let _ = event_sender.send(ProcessEvent::Exit {
                    plugin_name,
                    process_id,
                    code: status.code(),
                });
                return;
            }
            Ok(None) => {}
            Err(error) => {
                let _ = event_sender.send(ProcessEvent::Error {
                    plugin_name: plugin_name.clone(),
                    process_id: process_id.clone(),
                    message: format!("failed to query process status: {error}"),
                });
                let _ = child.kill();
                let status = child.wait().ok();
                join_output_readers(output_readers);
                let _ = event_sender.send(ProcessEvent::Exit {
                    plugin_name,
                    process_id,
                    code: status.and_then(|status| status.code()),
                });
                return;
            }
        }

        match kill_receiver.recv_timeout(PROCESS_POLL_INTERVAL) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = child.kill();
                let status = child.wait().ok();
                join_output_readers(output_readers);
                let _ = event_sender.send(ProcessEvent::Exit {
                    plugin_name,
                    process_id,
                    code: status.and_then(|status| status.code()),
                });
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    });
}

fn join_output_readers(readers: [thread::JoinHandle<()>; 2]) {
    for reader in readers {
        let _ = reader.join();
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, thread, time::Instant};

    use super::*;

    fn manager_with_commands(commands: &[&str]) -> ProcessManager {
        ProcessManager::new(HashMap::from([(
            "test".to_string(),
            PluginPermissions {
                process: commands.iter().map(|command| command.to_string()).collect(),
            },
        )]))
    }

    fn collect_until_exit(manager: &mut ProcessManager) -> Vec<ProcessEvent> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut events = Vec::new();
        while Instant::now() < deadline {
            let polled = manager.poll_events();
            let exited = polled
                .iter()
                .any(|event| matches!(event, ProcessEvent::Exit { .. }));
            events.extend(polled);
            if exited {
                return events;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("process did not exit before timeout");
    }

    #[cfg(not(windows))]
    fn stdout_stderr_exit_options() -> ProcessSpawnOptions {
        ProcessSpawnOptions {
            command: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                "printf 'first\\nsecond\\n'; printf 'problem\\n' >&2; exit 7".to_string(),
            ],
            cwd: None,
            ..ProcessSpawnOptions::default()
        }
    }

    #[cfg(windows)]
    fn stdout_stderr_exit_options() -> ProcessSpawnOptions {
        ProcessSpawnOptions {
            command: "powershell".to_string(),
            args: vec![
                "-NoLogo".to_string(),
                "-NoProfile".to_string(),
                "-Command".to_string(),
                concat!(
                    "Write-Output 'first'; ",
                    "Write-Output 'second'; ",
                    "[Console]::Error.WriteLine('problem'); ",
                    "exit 7"
                )
                .to_string(),
            ],
            cwd: None,
            ..ProcessSpawnOptions::default()
        }
    }

    #[cfg(not(windows))]
    fn long_running_options() -> ProcessSpawnOptions {
        ProcessSpawnOptions {
            command: "/bin/sleep".to_string(),
            args: vec!["30".to_string()],
            cwd: None,
            ..ProcessSpawnOptions::default()
        }
    }

    #[cfg(windows)]
    fn long_running_options() -> ProcessSpawnOptions {
        ProcessSpawnOptions {
            command: "powershell".to_string(),
            args: vec![
                "-NoLogo".to_string(),
                "-NoProfile".to_string(),
                "-Command".to_string(),
                "Start-Sleep -Seconds 30".to_string(),
            ],
            cwd: None,
            ..ProcessSpawnOptions::default()
        }
    }

    #[test]
    fn denies_commands_without_an_exact_permission() {
        let mut manager = manager_with_commands(&["printf"]);
        let error = manager
            .spawn(
                "test",
                ProcessSpawnOptions {
                    command: "/usr/bin/printf".to_string(),
                    args: vec![],
                    cwd: None,
                    ..ProcessSpawnOptions::default()
                },
            )
            .unwrap_err();

        assert!(error.to_string().contains("is not allowed to run"));
    }

    #[test]
    fn streams_stdout_stderr_and_exit() {
        let options = stdout_stderr_exit_options();
        let mut manager = manager_with_commands(&[options.command.as_str()]);
        let process_id = manager.spawn("test", options).unwrap();

        let events = collect_until_exit(&mut manager);
        assert!(events.contains(&ProcessEvent::Stdout {
            plugin_name: "test".to_string(),
            process_id: process_id.clone(),
            line: "first".to_string(),
        }));
        assert!(events.contains(&ProcessEvent::Stdout {
            plugin_name: "test".to_string(),
            process_id: process_id.clone(),
            line: "second".to_string(),
        }));
        assert!(events.contains(&ProcessEvent::Stderr {
            plugin_name: "test".to_string(),
            process_id: process_id.clone(),
            line: "problem".to_string(),
        }));
        assert!(events.contains(&ProcessEvent::Exit {
            plugin_name: "test".to_string(),
            process_id,
            code: Some(7),
        }));
        assert_eq!(manager.active_process_count("test"), 0);
    }

    #[cfg(not(windows))]
    #[test]
    fn writes_stdin_and_allows_restricted_environment() {
        let options = ProcessSpawnOptions {
            command: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                "read value; printf '%s:%s\\n' \"$value\" \"$GIT_PAGER\"".to_string(),
            ],
            stdin: Some("patch-data\n".to_string()),
            env: HashMap::from([("GIT_PAGER".to_string(), "cat".to_string())]),
            ..ProcessSpawnOptions::default()
        };
        let mut manager = manager_with_commands(&[options.command.as_str()]);
        let process_id = manager.spawn("test", options).unwrap();
        let events = collect_until_exit(&mut manager);
        assert!(events.contains(&ProcessEvent::Stdout {
            plugin_name: "test".to_string(),
            process_id,
            line: "patch-data:cat".to_string(),
        }));
    }

    #[cfg(not(windows))]
    #[test]
    fn raw_output_preserves_nul_and_newline_bytes() {
        let options = ProcessSpawnOptions {
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "printf 'one\\000two\\nthree'".to_string()],
            raw_output: true,
            ..ProcessSpawnOptions::default()
        };
        let mut manager = manager_with_commands(&[options.command.as_str()]);
        let process_id = manager.spawn("test", options).unwrap();
        let events = collect_until_exit(&mut manager);
        assert!(events.contains(&ProcessEvent::Stdout {
            plugin_name: "test".to_string(),
            process_id,
            line: "one\0two\nthree".to_string(),
        }));
    }

    #[test]
    fn rejects_unrestricted_environment_variables() {
        let options = ProcessSpawnOptions {
            command: "git".to_string(),
            env: HashMap::from([("PATH".to_string(), "/tmp".to_string())]),
            ..ProcessSpawnOptions::default()
        };
        let mut manager = manager_with_commands(&["git"]);
        let error = manager.spawn("test", options).unwrap_err();
        assert!(error
            .to_string()
            .contains("environment variable `PATH` is not allowed"));
    }

    #[test]
    fn enforces_per_plugin_process_limit_and_kill_is_idempotent() {
        let options = long_running_options();
        let mut manager = manager_with_commands(&[options.command.as_str()]);
        let mut process_ids = Vec::new();
        for _ in 0..MAX_PROCESSES_PER_PLUGIN {
            process_ids.push(manager.spawn("test", options.clone()).unwrap());
        }

        let error = manager.spawn("test", options).unwrap_err();
        assert!(error.to_string().contains(&format!(
            "maximum of {MAX_PROCESSES_PER_PLUGIN} active processes"
        )));

        for process_id in process_ids {
            manager.kill("test", &process_id).unwrap();
            manager.kill("test", &process_id).unwrap();
        }
        manager.kill("test", "already-finished").unwrap();
    }
}
