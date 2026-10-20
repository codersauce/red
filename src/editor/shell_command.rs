//! User-invoked, bounded shell jobs owned by the editor event loop.
//!
//! Ex shell commands are intentionally separate from Husk plugin subprocesses:
//! only explicit editor command-line input can grant the user's shell and full
//! environment. Workers stream sanitized output back to the editor without
//! taking ownership of terminal modes or detached-session IPC.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::SeekFrom,
    process::{ExitStatus, Stdio},
    time::Duration,
};

use tokio::{
    fs::File,
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
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
        filter_output: Option<Result<Vec<u8>, String>>,
    },
}

#[derive(Clone, Debug)]
struct ShellFilterTarget {
    buffer_id: BufferId,
    revision: u64,
    range: TextRange,
    line_ending: &'static str,
    followed_by_content: bool,
    source_ends_with_newline: bool,
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
    filter: Option<ShellFilterTarget>,
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
    mut capture: Option<File>,
) -> Result<Option<Vec<u8>>, String> {
    let mut buffer = [0; SHELL_READ_BYTES];
    loop {
        let count = match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) => return Err(format!("could not read shell output: {error}")),
        };
        if let Some(capture) = capture.as_mut() {
            capture
                .write_all(&buffer[..count])
                .await
                .map_err(|error| format!("could not retain shell filter output: {error}"))?;
        }
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

    let Some(mut capture) = capture else {
        return Ok(None);
    };
    capture
        .seek(SeekFrom::Start(0))
        .await
        .map_err(|error| format!("could not rewind shell filter output: {error}"))?;
    let mut output = Vec::new();
    capture
        .read_to_end(&mut output)
        .await
        .map_err(|error| format!("could not load shell filter output: {error}"))?;
    Ok(Some(output))
}

fn filter_replacement(
    output: Vec<u8>,
    line_ending: &str,
    followed_by_content: bool,
) -> anyhow::Result<String> {
    let output = String::from_utf8(output)
        .map_err(|_| anyhow::anyhow!("shell filter output is not valid UTF-8"))?;
    let mut replacement = output.replace("\r\n", "\n");
    if line_ending == "\r\n" {
        replacement = replacement.replace('\n', "\r\n");
    }
    if followed_by_content && !replacement.is_empty() && !replacement.ends_with('\n') {
        replacement.push_str(line_ending);
    }
    Ok(replacement)
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
        self.start_shell_job(command, None)
    }

    pub(super) fn start_shell_filter(
        &mut self,
        command: &str,
        start_line: usize,
        end_line: usize,
    ) -> anyhow::Result<()> {
        let buffer = self.current_buffer();
        let input = buffer.line_range_contents(start_line, end_line.saturating_add(1));
        let line_ending = if buffer.get(0).is_some_and(|line| line.ends_with("\r\n")) {
            "\r\n"
        } else {
            "\n"
        };
        let range_end = if end_line < buffer.len() {
            TextPosition::new(end_line + 1, 0)
        } else {
            TextPosition::new(end_line, self.line_character_len(end_line))
        };
        let target = ShellFilterTarget {
            buffer_id: buffer.id(),
            revision: buffer.revision(),
            range: TextRange::new(TextPosition::new(start_line, 0), range_end),
            line_ending,
            followed_by_content: end_line < buffer.last_navigable_line(),
            source_ends_with_newline: input.ends_with('\n'),
        };
        self.start_shell_job(command, Some((target, input.into_bytes())))
    }

    fn start_shell_job(
        &mut self,
        command: &str,
        filter: Option<(ShellFilterTarget, Vec<u8>)>,
    ) -> anyhow::Result<()> {
        #[cfg(windows)]
        let environment_shell = std::env::var_os("COMSPEC");
        #[cfg(not(windows))]
        let environment_shell = std::env::var_os("SHELL");

        let capture = if filter.is_some() {
            Some(File::from_std(tempfile::tempfile().map_err(|error| {
                anyhow::anyhow!("could not create shell filter output file: {error}")
            })?))
        } else {
            None
        };
        let (shell, flag) = shell_invocation(environment_shell);
        let mut process = Command::new(&shell);
        process
            .arg(flag)
            .arg(command)
            .current_dir(&self.statusline_git_cache.working_directory)
            .stdin(if filter.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
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
        let stdin = if filter.is_some() {
            Some(
                child
                    .stdin
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("shell did not provide stdin"))?,
            )
        } else {
            None
        };
        let (filter, input) = match filter {
            Some((target, input)) => (Some(target), Some(input)),
            None => (None, None),
        };

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
                filter,
            },
        );

        let sender = self.shell_commands.sender.clone();
        tokio::spawn(async move {
            let stdout_task = tokio::spawn(forward_output(
                stdout,
                job_id,
                OutputStream::Stdout,
                sender.clone(),
                capture,
            ));
            let stderr_task = tokio::spawn(forward_output(
                stderr,
                job_id,
                OutputStream::Stderr,
                sender.clone(),
                None,
            ));
            let stdin_task = stdin.zip(input).map(|(mut stdin, input)| {
                tokio::spawn(async move {
                    match stdin.write_all(&input).await {
                        Ok(()) => Ok(()),
                        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
                        Err(error) => Err(format!("could not write shell filter input: {error}")),
                    }
                })
            });
            let (status, cancelled) = tokio::select! {
                status = child.wait() => (status, false),
                _ = &mut cancellation_requested => (cancel_child(&mut child).await, true),
            };
            let (stdout_result, _, stdin_result) = tokio::join!(stdout_task, stderr_task, async {
                match stdin_task {
                    Some(task) => task
                        .await
                        .map_err(|error| format!("shell filter input task failed: {error}"))?,
                    None => Ok(()),
                }
            });
            let filter_output = match stdout_result {
                Ok(Ok(Some(output))) => Some(stdin_result.map(|()| output)),
                Ok(Ok(None)) => None,
                Ok(Err(error)) => Some(Err(error)),
                Err(error) => Some(Err(format!("shell output task failed: {error}"))),
            };
            let _ = sender
                .send(ShellEvent::Finished {
                    job_id,
                    status: status.map_err(|error| error.to_string()),
                    cancelled,
                    filter_output,
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

    async fn apply_shell_filter(
        &mut self,
        target: ShellFilterTarget,
        output: Vec<u8>,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let index = self
            .buffer_manager
            .iter()
            .position(|buffer| buffer.id() == target.buffer_id)
            .ok_or_else(|| anyhow::anyhow!("shell filter target buffer is no longer open"))?;
        anyhow::ensure!(
            self.buffer_manager[index].revision() == target.revision,
            "shell filter result is stale; buffer changed while the command was running"
        );
        let replacement =
            filter_replacement(output, target.line_ending, target.followed_by_content)?;

        let original_index = self.buffer_manager.active_index();
        let original_view = (self.cx, self.cy, self.vtop, self.vleft, self.skipcol);
        if index != original_index {
            self.select_buffer_for_lsp_edit(index);
        }

        let mut range = target.range;
        if replacement.is_empty()
            && !target.followed_by_content
            && !target.source_ends_with_newline
            && range.start.line > 0
        {
            let previous_line = range.start.line - 1;
            range.start = TextPosition::new(previous_line, self.line_character_len(previous_line));
        }

        if self.transaction_active() {
            self.commit_transaction(self.cursor_snapshot());
        }
        self.begin_transaction("shell filter");
        self.replace_range(range, &replacement);
        self.move_to_text_position(range.start);
        let changed = self.commit_transaction(self.cursor_snapshot());

        if index != original_index {
            self.select_buffer_for_lsp_edit(original_index);
            (self.cx, self.cy, self.vtop, self.vleft, self.skipcol) = original_view;
            self.check_bounds();
        }
        if changed {
            self.notify_buffer_change(index, runtime).await?;
        }
        Ok(())
    }

    pub(super) async fn service_shell_commands(&mut self, runtime: &mut Runtime) -> bool {
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
                    filter_output,
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
                            Ok(status) if status.success() => {
                                let result = if let Some(target) = job.filter.clone() {
                                    match filter_output {
                                        Some(Ok(output)) => {
                                            self.apply_shell_filter(target, output, runtime).await
                                        }
                                        Some(Err(error)) => Err(anyhow::anyhow!(error)),
                                        None => Err(anyhow::anyhow!(
                                            "shell filter did not capture command output"
                                        )),
                                    }
                                } else {
                                    Ok(())
                                };
                                match result {
                                    Ok(()) => (
                                        ProgressOutcome::Succeeded,
                                        format!("Shell finished: {}", job.command),
                                    ),
                                    Err(error) => (
                                        ProgressOutcome::Failed,
                                        format!("Shell failed ({error}): {}", job.command),
                                    ),
                                }
                            }
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
    fn filter_output_preserves_document_line_endings_and_line_boundaries() {
        assert_eq!(
            filter_replacement(b"first\r\nsecond".to_vec(), "\n", true).unwrap(),
            "first\nsecond\n"
        );
        assert_eq!(
            filter_replacement(b"first\nsecond\r\n".to_vec(), "\r\n", false).unwrap(),
            "first\r\nsecond\r\n"
        );
        assert_eq!(filter_replacement(Vec::new(), "\n", true).unwrap(), "");
    }

    #[test]
    fn filter_output_rejects_invalid_utf8_without_lossy_document_changes() {
        let error = filter_replacement(vec![0xff], "\n", false).unwrap_err();
        assert!(error.to_string().contains("not valid UTF-8"));
    }

    #[test]
    fn shell_command_detection_includes_explicit_ex_filter_ranges() {
        for command in ["!echo hello", "%!sort", "2,5!sort", "'<,'>!sort"] {
            assert!(Editor::is_shell_ex_command(command), "{command}");
        }
        for command in ["w!", "%s/a/b/", "bufdo !echo nope"] {
            assert!(!Editor::is_shell_ex_command(command), "{command}");
        }
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
