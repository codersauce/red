//! User-invoked, bounded shell jobs owned by the editor event loop.
//!
//! Ex shell commands are intentionally separate from Husk plugin subprocesses:
//! only explicit editor command-line input can grant the user's shell and full
//! environment. Workers stream sanitized output back to the editor without
//! taking ownership of terminal modes or detached-session IPC.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    process::{ExitStatus, Stdio},
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::{mpsc, oneshot},
};

use super::*;
use crate::notification::{MessageContent, ProgressOutcome, ProgressPriority};

const MAX_SHELL_OUTPUT_BYTES: usize = 60 * 1024;
const SHELL_EVENT_CAPACITY: usize = 32;
const SHELL_EVENTS_PER_TICK: usize = 32;
const SHELL_READ_BYTES: usize = 4 * 1024;
const SHELL_CANCEL_GRACE: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputStream {
    Stdout,
    Stderr,
}

enum ShellEvent {
    Output {
        job_id: u64,
        stream: OutputStream,
        bytes: Vec<u8>,
    },
    Finished {
        job_id: u64,
        status: Result<ExitStatus, String>,
        cancelled: bool,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum EscapeState {
    #[default]
    Ground,
    Escape,
    ControlSequence,
    OperatingSystemCommand,
    OperatingSystemCommandEscape,
}

#[derive(Debug, Default)]
struct OutputSanitizer {
    state: EscapeState,
    previous_carriage_return: bool,
}

impl OutputSanitizer {
    fn append(&mut self, input: &[u8], output: &mut Vec<u8>) {
        for byte in input.iter().copied() {
            let linefeed_after_carriage_return = self.previous_carriage_return && byte == b'\n';
            self.previous_carriage_return = false;
            if linefeed_after_carriage_return {
                continue;
            }
            match self.state {
                EscapeState::Ground => match byte {
                    b'\x1b' => self.state = EscapeState::Escape,
                    b'\n' | b'\t' => output.push(byte),
                    b'\r' => {
                        output.push(b'\n');
                        self.previous_carriage_return = true;
                    }
                    0x00..=0x1f | 0x7f => {}
                    _ => output.push(byte),
                },
                EscapeState::Escape => {
                    self.state = match byte {
                        b'[' => EscapeState::ControlSequence,
                        b']' => EscapeState::OperatingSystemCommand,
                        _ => EscapeState::Ground,
                    };
                }
                EscapeState::ControlSequence => {
                    if (0x40..=0x7e).contains(&byte) {
                        self.state = EscapeState::Ground;
                    }
                }
                EscapeState::OperatingSystemCommand => match byte {
                    b'\x07' => self.state = EscapeState::Ground,
                    b'\x1b' => self.state = EscapeState::OperatingSystemCommandEscape,
                    _ => {}
                },
                EscapeState::OperatingSystemCommandEscape => {
                    self.state = if byte == b'\\' {
                        EscapeState::Ground
                    } else {
                        EscapeState::OperatingSystemCommand
                    };
                }
            }
        }
    }
}

struct ShellJob {
    notification_id: NotificationId,
    command: String,
    output: Vec<u8>,
    truncated: bool,
    stdout: OutputSanitizer,
    stderr: OutputSanitizer,
    cancellation: Option<oneshot::Sender<()>>,
}

impl ShellJob {
    fn content(&self, summary: String) -> MessageContent {
        let mut details = format!("$ {}\n\n", self.command);
        if self.truncated {
            details.push_str("[Earlier output truncated by Red]\n");
        }
        details.push_str(&String::from_utf8_lossy(&self.output));
        MessageContent::new(summary).with_details(details)
    }

    fn append_output(&mut self, stream: OutputStream, bytes: &[u8]) {
        let sanitizer = match stream {
            OutputStream::Stdout => &mut self.stdout,
            OutputStream::Stderr => &mut self.stderr,
        };
        sanitizer.append(bytes, &mut self.output);
        if self.output.len() > MAX_SHELL_OUTPUT_BYTES {
            let excess = self.output.len() - MAX_SHELL_OUTPUT_BYTES;
            self.output.drain(..excess);
            while self
                .output
                .first()
                .is_some_and(|byte| byte & 0b1100_0000 == 0b1000_0000)
            {
                self.output.remove(0);
            }
            self.truncated = true;
        }
    }
}

pub(super) struct ShellCommandState {
    previous: Option<String>,
    next_id: u64,
    jobs: BTreeMap<u64, ShellJob>,
    sender: mpsc::Sender<ShellEvent>,
    receiver: mpsc::Receiver<ShellEvent>,
}

impl Default for ShellCommandState {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel(SHELL_EVENT_CAPACITY);
        Self {
            previous: None,
            next_id: 0,
            jobs: BTreeMap::new(),
            sender,
            receiver,
        }
    }
}

impl ShellCommandState {
    pub(super) fn has_notification(&self, notification_id: NotificationId) -> bool {
        self.jobs
            .values()
            .any(|job| job.notification_id == notification_id && job.cancellation.is_some())
    }

    pub(super) fn cancel_all(&mut self) {
        for job in self.jobs.values_mut() {
            if let Some(cancellation) = job.cancellation.take() {
                let _ = cancellation.send(());
            }
        }
    }
}

fn expand_shell_command(
    raw: &str,
    current_file: Option<&str>,
    alternate_file: Option<&str>,
    previous: Option<&str>,
) -> anyhow::Result<String> {
    anyhow::ensure!(!raw.trim().is_empty(), "usage: !{{shell command}}");

    let mut expanded = String::with_capacity(raw.len());
    let mut characters = raw.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\\' if characters
                .peek()
                .is_some_and(|next| matches!(next, '%' | '#' | '!')) =>
            {
                if let Some(literal) = characters.next() {
                    expanded.push(literal);
                }
            }
            '%' => expanded.push_str(
                current_file.ok_or_else(|| anyhow::anyhow!("No current file name for %"))?,
            ),
            '#' => expanded.push_str(
                alternate_file.ok_or_else(|| anyhow::anyhow!("No alternate file name for #"))?,
            ),
            '!' => expanded
                .push_str(previous.ok_or_else(|| anyhow::anyhow!("No previous shell command"))?),
            _ => expanded.push(character),
        }
    }
    Ok(expanded)
}

#[cfg(unix)]
fn shell_invocation(shell: Option<OsString>) -> (OsString, &'static str) {
    (
        shell
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| OsString::from("/bin/sh")),
        "-c",
    )
}

#[cfg(windows)]
fn shell_invocation(shell: Option<OsString>) -> (OsString, &'static str) {
    (
        shell
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| OsString::from("cmd.exe")),
        "/C",
    )
}

#[cfg(not(any(unix, windows)))]
fn shell_invocation(shell: Option<OsString>) -> (OsString, &'static str) {
    (
        shell
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| OsString::from("sh")),
        "-c",
    )
}

async fn forward_output(
    mut reader: impl AsyncRead + Unpin,
    job_id: u64,
    stream: OutputStream,
    sender: mpsc::Sender<ShellEvent>,
) {
    let mut buffer = [0; SHELL_READ_BYTES];
    loop {
        let count = match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(count) => count,
        };
        if sender
            .send(ShellEvent::Output {
                job_id,
                stream,
                bytes: buffer[..count].to_vec(),
            })
            .await
            .is_err()
        {
            break;
        }
    }
}

async fn cancel_child(child: &mut tokio::process::Child) -> std::io::Result<ExitStatus> {
    #[cfg(unix)]
    if let Some(process_group) = child.id().and_then(|id| i32::try_from(id).ok()) {
        let process_group = Pid::from_raw(-process_group);
        let _ = signal::kill(process_group, Signal::SIGINT);
        if let Ok(status) = tokio::time::timeout(SHELL_CANCEL_GRACE, child.wait()).await {
            return status;
        }
        let _ = signal::kill(process_group, Signal::SIGKILL);
        return child.wait().await;
    }

    #[cfg(not(unix))]
    let _ = SHELL_CANCEL_GRACE;

    child.kill().await?;
    child.wait().await
}

impl Editor {
    pub(super) fn parse_shell_command(&mut self, raw: &str) -> anyhow::Result<String> {
        let current = self.current_buffer().file.as_deref();
        let alternate = self
            .buffer_manager
            .alternate_index()
            .and_then(|index| self.buffer_manager.get(index))
            .and_then(|buffer| buffer.file.as_deref());
        let command = expand_shell_command(
            raw,
            current,
            alternate,
            self.shell_commands.previous.as_deref(),
        )?;
        self.shell_commands.previous = Some(command.clone());
        Ok(command)
    }

    pub(super) fn start_shell_command(&mut self, command: &str) -> anyhow::Result<()> {
        #[cfg(windows)]
        let environment_shell = std::env::var_os("COMSPEC");
        #[cfg(not(windows))]
        let environment_shell = std::env::var_os("SHELL");

        let (shell, flag) = shell_invocation(environment_shell);
        let mut process = Command::new(&shell);
        process
            .arg(flag)
            .arg(command)
            .current_dir(&self.statusline_git_cache.working_directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        process.process_group(/*pgroup*/ 0);

        let mut child = process.spawn().map_err(|error| {
            anyhow::anyhow!("could not start shell {}: {error}", shell.to_string_lossy())
        })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("shell did not provide stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("shell did not provide stderr"))?;

        self.shell_commands.next_id = self.shell_commands.next_id.saturating_add(1);
        let job_id = self.shell_commands.next_id;
        let summary = format!("Shell running: {command}");
        let notification_id = self.notifications.begin_progress(
            NotificationSource::Editor,
            format!("shell-command-{job_id}"),
            MessageContent::new(summary).with_details(format!("$ {command}\n")),
            ProgressPriority::UserInitiated,
            NotificationTime::now(),
        )?;
        let (cancellation, mut cancellation_requested) = oneshot::channel();
        self.shell_commands.jobs.insert(
            job_id,
            ShellJob {
                notification_id,
                command: command.to_string(),
                output: Vec::new(),
                truncated: false,
                stdout: OutputSanitizer::default(),
                stderr: OutputSanitizer::default(),
                cancellation: Some(cancellation),
            },
        );

        let sender = self.shell_commands.sender.clone();
        tokio::spawn(async move {
            let stdout_task = tokio::spawn(forward_output(
                stdout,
                job_id,
                OutputStream::Stdout,
                sender.clone(),
            ));
            let stderr_task = tokio::spawn(forward_output(
                stderr,
                job_id,
                OutputStream::Stderr,
                sender.clone(),
            ));
            let (status, cancelled) = tokio::select! {
                status = child.wait() => (status, false),
                _ = &mut cancellation_requested => (cancel_child(&mut child).await, true),
            };
            let _ = tokio::join!(stdout_task, stderr_task);
            let _ = sender
                .send(ShellEvent::Finished {
                    job_id,
                    status: status.map_err(|error| error.to_string()),
                    cancelled,
                })
                .await;
        });

        self.open_message_notification(notification_id);
        Ok(())
    }

    pub(super) fn cancel_selected_shell_command(&mut self) -> bool {
        let Some(notification_id) = self.selected_message_notification() else {
            return false;
        };
        let Some(job) = self
            .shell_commands
            .jobs
            .values_mut()
            .find(|job| job.notification_id == notification_id)
        else {
            return false;
        };
        job.cancellation
            .take()
            .is_some_and(|cancellation| cancellation.send(()).is_ok())
    }

    pub(super) fn service_shell_commands(&mut self) -> bool {
        let mut changed = false;
        for _ in 0..SHELL_EVENTS_PER_TICK {
            let Ok(event) = self.shell_commands.receiver.try_recv() else {
                break;
            };
            match event {
                ShellEvent::Output {
                    job_id,
                    stream,
                    bytes,
                } => {
                    let Some(job) = self.shell_commands.jobs.get_mut(&job_id) else {
                        continue;
                    };
                    job.append_output(stream, &bytes);
                    let content = job.content(format!("Shell running: {}", job.command));
                    let _ = self.notifications.update_progress(
                        job.notification_id,
                        content,
                        /*percentage*/ None,
                        NotificationTime::now(),
                    );
                    changed = true;
                }
                ShellEvent::Finished {
                    job_id,
                    status,
                    cancelled,
                } => {
                    let Some(job) = self.shell_commands.jobs.remove(&job_id) else {
                        continue;
                    };
                    let (outcome, summary) = if cancelled {
                        (
                            ProgressOutcome::Cancelled,
                            format!("Shell cancelled: {}", job.command),
                        )
                    } else {
                        match status {
                            Ok(status) if status.success() => (
                                ProgressOutcome::Succeeded,
                                format!("Shell finished: {}", job.command),
                            ),
                            Ok(status) => (
                                ProgressOutcome::Failed,
                                format!("Shell failed ({status}): {}", job.command),
                            ),
                            Err(error) => (
                                ProgressOutcome::Failed,
                                format!("Shell failed ({error}): {}", job.command),
                            ),
                        }
                    };
                    let content = job.content(summary.clone());
                    let _ = self.notifications.finish_progress(
                        job.notification_id,
                        outcome,
                        content,
                        NotificationTime::now(),
                    );
                    self.last_error = matches!(outcome, ProgressOutcome::Failed).then_some(summary);
                    changed = true;
                }
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_expansion_preserves_quoting_and_expands_vim_placeholders() {
        assert_eq!(
            expand_shell_command(
                r"echo \% \# \! '%' '#' '!'",
                Some("current file.rs"),
                Some("alternate.rs"),
                Some("git status"),
            )
            .unwrap(),
            "echo % # ! 'current file.rs' 'alternate.rs' 'git status'"
        );
    }

    #[test]
    fn shell_expansion_reports_missing_required_context() {
        for (raw, expected) in [
            (" ", "usage: !{shell command}"),
            ("echo %", "No current file name"),
            ("echo #", "No alternate file name"),
            ("!", "No previous shell command"),
        ] {
            let error = expand_shell_command(raw, None, None, None).unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn output_sanitizer_removes_split_ansi_and_operating_system_sequences() {
        let mut sanitizer = OutputSanitizer::default();
        let mut output = Vec::new();
        sanitizer.append(b"before\x1b[31", &mut output);
        sanitizer.append(b"mred\x1b[0m\x1b]52;c;secret", &mut output);
        sanitizer.append(b"\x07after\x00\n", &mut output);
        assert_eq!(output, b"beforeredafter\n");
    }

    #[test]
    fn output_sanitizer_normalizes_split_windows_line_endings() {
        let mut sanitizer = OutputSanitizer::default();
        let mut output = Vec::new();
        sanitizer.append(b"first\r", &mut output);
        sanitizer.append(b"\nsecond\rthird\n", &mut output);
        assert_eq!(output, b"first\nsecond\nthird\n");
    }

    #[test]
    fn shell_selection_uses_environment_and_platform_fallback() {
        let (configured, _) = shell_invocation(Some(OsString::from("custom-shell")));
        assert_eq!(configured, OsString::from("custom-shell"));
        let (fallback, _) = shell_invocation(Some(OsString::new()));
        #[cfg(unix)]
        assert_eq!(fallback, OsString::from("/bin/sh"));
        #[cfg(windows)]
        assert_eq!(fallback, OsString::from("cmd.exe"));
    }
}
