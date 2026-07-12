use std::{
    collections::{HashMap, VecDeque},
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, Sender, SyncSender},
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::PluginPermissions;

const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);
pub const MAX_PROCESSES_PER_PLUGIN: usize = 16;
const MAX_PENDING_PROCESS_EVENTS: usize = 16;
const MAX_PROCESS_LINE_BYTES: usize = 256 * 1024;
const MAX_PROCESS_RAW_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_PROCESS_STDIN_BYTES: usize = 16 * 1024 * 1024;
const INHERITED_ENVIRONMENT_KEYS: &[&str] = &[
    "HOME",
    "PATH",
    "USER",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    "TMP",
    "TEMP",
    "LANG",
    "LC_ALL",
    "TERM",
    "SSH_AUTH_SOCK",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "SYSTEMROOT",
    "SystemRoot",
    "WINDIR",
    "COMSPEC",
    "ComSpec",
    "PATHEXT",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
];

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
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
    rename_all = "snake_case",
    rename_all_fields = "snake_case"
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
    fn plugin_name(&self) -> &str {
        match self {
            Self::Stdout { plugin_name, .. }
            | Self::Stderr { plugin_name, .. }
            | Self::Exit { plugin_name, .. }
            | Self::Error { plugin_name, .. } => plugin_name,
        }
    }

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
    event_sender: SyncSender<ProcessEvent>,
    event_receiver: Receiver<ProcessEvent>,
    pending_events: VecDeque<ProcessEvent>,
}

impl ProcessManager {
    pub fn new(permissions: HashMap<String, PluginPermissions>) -> Self {
        let (event_sender, event_receiver) = mpsc::sync_channel(MAX_PENDING_PROCESS_EVENTS);
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
        if let Some(stdin) = &options.stdin {
            anyhow::ensure!(
                stdin.len() <= MAX_PROCESS_STDIN_BYTES,
                "plugin `{plugin_name}` process stdin exceeds {MAX_PROCESS_STDIN_BYTES} bytes"
            );
        }

        if self.active_process_count(plugin_name) >= MAX_PROCESSES_PER_PLUGIN {
            anyhow::bail!(
                "plugin `{plugin_name}` already has the maximum of {MAX_PROCESSES_PER_PLUGIN} active processes"
            );
        }

        let mut command = Command::new(&options.command);
        command
            .env_clear()
            .args(&options.args)
            .stdin(if options.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for key in INHERITED_ENVIRONMENT_KEYS {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
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
        let stdin = options
            .stdin
            .map(|input| {
                child
                    .stdin
                    .take()
                    .map(|stdin| (stdin, input))
                    .ok_or_else(|| anyhow::anyhow!("spawned process did not provide stdin"))
            })
            .transpose()?;
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
        if let Some((mut stdin, input)) = stdin {
            let event_sender = self.event_sender.clone();
            let plugin_name = plugin_name.to_string();
            let process_id = process_id.clone();
            thread::spawn(move || {
                if let Err(error) = stdin.write_all(input.as_bytes()) {
                    let _ = event_sender.send(ProcessEvent::Error {
                        plugin_name,
                        process_id,
                        message: format!("failed to write process stdin: {error}"),
                    });
                }
            });
        }
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

    pub fn shutdown_plugin(&mut self, plugin_name: &str) {
        self.refresh_events();
        self.processes.retain(|_, process| {
            if process.plugin_name == plugin_name {
                let _ = process.kill_sender.send(());
                false
            } else {
                true
            }
        });
        self.pending_events
            .retain(|event| event.plugin_name() != plugin_name);
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
        while self.pending_events.len() < MAX_PENDING_PROCESS_EVENTS {
            let Ok(event) = self.event_receiver.try_recv() else {
                break;
            };
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
    event_sender: SyncSender<ProcessEvent>,
    raw_output: bool,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = BufReader::new(output);
        if raw_output {
            let mut bytes = Vec::new();
            let result = reader
                .by_ref()
                .take((MAX_PROCESS_RAW_OUTPUT_BYTES + 1) as u64)
                .read_to_end(&mut bytes);
            match result {
                Ok(_) if bytes.len() > MAX_PROCESS_RAW_OUTPUT_BYTES => {
                    let _ = std::io::copy(&mut reader, &mut std::io::sink());
                    let _ = event_sender.send(ProcessEvent::Error {
                        plugin_name,
                        process_id,
                        message: format!(
                            "raw process output exceeds {MAX_PROCESS_RAW_OUTPUT_BYTES} bytes"
                        ),
                    });
                }
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
            let result = reader
                .by_ref()
                .take((MAX_PROCESS_LINE_BYTES + 1) as u64)
                .read_until(b'\n', &mut bytes);
            match result {
                Ok(0) => break,
                Ok(_) if bytes.len() > MAX_PROCESS_LINE_BYTES => {
                    if bytes.last() != Some(&b'\n') && discard_line(&mut reader).is_err() {
                        break;
                    }
                    let _ = event_sender.send(ProcessEvent::Error {
                        plugin_name: plugin_name.clone(),
                        process_id: process_id.clone(),
                        message: format!(
                            "process output line exceeds {MAX_PROCESS_LINE_BYTES} bytes"
                        ),
                    });
                }
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

fn discard_line(reader: &mut impl BufRead) -> std::io::Result<()> {
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(());
        }
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        let complete = available[consumed - 1] == b'\n';
        reader.consume(consumed);
        if complete {
            return Ok(());
        }
    }
}

fn spawn_process_supervisor(
    mut child: std::process::Child,
    plugin_name: String,
    process_id: String,
    kill_receiver: Receiver<()>,
    event_sender: SyncSender<ProcessEvent>,
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
            command: "cmd.exe".to_string(),
            args: vec![
                "/D".to_string(),
                "/C".to_string(),
                "echo first&echo second&1>&2 echo problem&exit /b 7".to_string(),
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

    #[test]
    fn preserves_streamed_output_when_both_event_queues_fill() {
        let mut manager = manager_with_commands(&[]);
        let process_id = "burst-process".to_string();
        let stdout = |line: usize| ProcessEvent::Stdout {
            plugin_name: "test".to_string(),
            process_id: process_id.clone(),
            line: line.to_string(),
        };
        let total_lines = MAX_PENDING_PROCESS_EVENTS * 4;

        manager
            .pending_events
            .extend((0..MAX_PENDING_PROCESS_EVENTS).map(stdout));
        for line in MAX_PENDING_PROCESS_EVENTS..MAX_PENDING_PROCESS_EVENTS * 2 {
            manager.event_sender.send(stdout(line)).unwrap();
        }

        let output = (MAX_PENDING_PROCESS_EVENTS * 2..total_lines)
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        let reader = spawn_output_reader(
            std::io::Cursor::new(output),
            OutputStream::Stdout,
            "test".to_string(),
            process_id,
            manager.event_sender.clone(),
            false,
        );
        let (done_sender, done_receiver) = mpsc::channel();
        thread::spawn(move || {
            reader.join().unwrap();
            done_sender.send(()).unwrap();
        });

        assert!(matches!(
            done_receiver.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut events = Vec::new();
        while events.len() < total_lines {
            events.extend(manager.poll_events());
            assert!(
                Instant::now() < deadline,
                "streamed output remained blocked"
            );
            thread::yield_now();
        }

        done_receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        let lines = events
            .into_iter()
            .map(|event| match event {
                ProcessEvent::Stdout { line, .. } => line,
                other => panic!("unexpected process event: {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            lines,
            (0..total_lines)
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
        );
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
    fn does_not_inherit_unrelated_environment_variables() {
        const SECRET_KEY: &str = "RED_PROCESS_TEST_PRIVATE_TOKEN";
        std::env::set_var(SECRET_KEY, "must-not-be-inherited");
        let options = ProcessSpawnOptions {
            command: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                format!("printf '%s\\n' \"${{{SECRET_KEY}:-missing}}\""),
            ],
            ..ProcessSpawnOptions::default()
        };
        let mut manager = manager_with_commands(&[options.command.as_str()]);
        let process_id = manager.spawn("test", options).unwrap();
        let events = collect_until_exit(&mut manager);
        std::env::remove_var(SECRET_KEY);

        assert!(events.contains(&ProcessEvent::Stdout {
            plugin_name: "test".to_string(),
            process_id,
            line: "missing".to_string(),
        }));
    }

    #[cfg(not(windows))]
    #[test]
    fn reports_an_oversized_output_line_and_continues_streaming() {
        let options = ProcessSpawnOptions {
            command: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                format!(
                    "dd if=/dev/zero bs={} count=1 2>/dev/null | tr '\\000' x; printf '\\nnext\\n'",
                    MAX_PROCESS_LINE_BYTES + 1
                ),
            ],
            ..ProcessSpawnOptions::default()
        };
        let mut manager = manager_with_commands(&[options.command.as_str()]);
        let process_id = manager.spawn("test", options).unwrap();
        let events = collect_until_exit(&mut manager);

        assert!(events.iter().any(|event| matches!(
            event,
            ProcessEvent::Error { message, .. }
                if message.contains("process output line exceeds")
        )));
        assert!(events.contains(&ProcessEvent::Stdout {
            plugin_name: "test".to_string(),
            process_id,
            line: "next".to_string(),
        }));
    }

    #[cfg(not(windows))]
    #[test]
    fn reports_bounded_raw_output_without_stalling_the_process() {
        let options = ProcessSpawnOptions {
            command: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                format!(
                    "dd if=/dev/zero bs={} count=1 2>/dev/null",
                    MAX_PROCESS_RAW_OUTPUT_BYTES + 1
                ),
            ],
            raw_output: true,
            ..ProcessSpawnOptions::default()
        };
        let mut manager = manager_with_commands(&[options.command.as_str()]);
        manager.spawn("test", options).unwrap();
        let events = collect_until_exit(&mut manager);

        assert!(events.iter().any(|event| matches!(
            event,
            ProcessEvent::Error { message, .. }
                if message.contains("raw process output exceeds")
        )));
        assert!(events
            .iter()
            .any(|event| matches!(event, ProcessEvent::Exit { .. })));
    }

    #[cfg(not(windows))]
    #[test]
    fn drains_output_while_a_child_is_waiting_to_consume_large_stdin() {
        let options = ProcessSpawnOptions {
            command: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                "dd if=/dev/zero bs=524288 count=1 2>/dev/null; cat >/dev/null; printf 'done\\n' >&2"
                    .to_string(),
            ],
            stdin: Some("x".repeat(512 * 1024)),
            ..ProcessSpawnOptions::default()
        };
        let mut manager = manager_with_commands(&[options.command.as_str()]);
        let process_id = manager.spawn("test", options).unwrap();
        let events = collect_until_exit(&mut manager);

        assert!(events.contains(&ProcessEvent::Stderr {
            plugin_name: "test".to_string(),
            process_id,
            line: "done".to_string(),
        }));
    }

    #[test]
    fn rejects_oversized_process_stdin_before_spawn() {
        let options = ProcessSpawnOptions {
            command: "git".to_string(),
            stdin: Some("x".repeat(MAX_PROCESS_STDIN_BYTES + 1)),
            ..ProcessSpawnOptions::default()
        };
        let mut manager = manager_with_commands(&["git"]);
        let error = manager.spawn("test", options).unwrap_err();

        assert!(error.to_string().contains("process stdin exceeds"));
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

    #[test]
    fn shutdown_plugin_releases_only_the_target_plugins_processes() {
        let options = long_running_options();
        let permissions = PluginPermissions {
            process: vec![options.command.clone()],
        };
        let mut manager = ProcessManager::new(HashMap::from([
            ("target".to_string(), permissions.clone()),
            ("other".to_string(), permissions),
        ]));
        manager.spawn("target", options.clone()).unwrap();
        manager.spawn("target", options.clone()).unwrap();
        manager.spawn("other", options).unwrap();
        assert_eq!(manager.active_process_count("target"), 2);
        assert_eq!(manager.active_process_count("other"), 1);

        manager.shutdown_plugin("target");

        assert_eq!(manager.active_process_count("target"), 0);
        assert_eq!(manager.active_process_count("other"), 1);
        manager.shutdown();
    }
}
