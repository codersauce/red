//! Run one submitted shell command against a snapshot. Publish all edits together
//! only while the originating document, revision, view and selection are current.
use super::*;
use felix_core::Shell;
use std::{path::Path, process::Stdio};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

pub(super) struct ShellJob {
    pub(super) owner: Owner,
    selections: Selections,
    pub(super) tail: Option<felix_core::MacroReplay>,
    kind: Shell,
    task: tokio::task::JoinHandle<anyhow::Result<Vec<Option<String>>>>,
}
impl Drop for ShellJob {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn read_output(stream: impl AsyncRead + Unpin) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    stream
        .take(OUTPUT_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    anyhow::ensure!(bytes.len() <= OUTPUT_LIMIT, "Shell output exceeds 8 MiB");
    Ok(bytes)
}

// A pipeline can outlive the shell process. On cancellation or an output-limit
// error, release every child that still owns one of our pipes.
#[cfg(unix)]
struct ChildGroup(Option<i32>);
#[cfg(unix)]
impl Drop for ChildGroup {
    fn drop(&mut self) {
        if let Some(id) = self.0 {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(id),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
}

async fn invoke(command: &str, cwd: &Path, input: Option<&str>) -> anyhow::Result<(bool, String)> {
    // Helix defaults to a POSIX shell on Unix even when the interactive shell is Fish.
    let (shell, flag) = crate::editor::shell_command::shell_invocation(None);
    let mut process = tokio::process::Command::new(shell);
    #[cfg(unix)]
    process.process_group(0);
    let mut child = process
        .arg(flag)
        .arg(command)
        .current_dir(cwd)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    #[cfg(unix)]
    let mut group = ChildGroup(child.id().and_then(|id| i32::try_from(id).ok()));
    let stdout = child.stdout.take().context("missing shell stdout")?;
    let stderr = child.stderr.take().context("missing shell stderr")?;
    let stdin = child.stdin.take();
    let write = async {
        if let (Some(mut stdin), Some(text)) = (stdin, input) {
            if let Err(e) = stdin.write_all(text.as_bytes()).await {
                if e.kind() != std::io::ErrorKind::BrokenPipe {
                    return Err(e.into());
                }
            }
        }
        anyhow::Ok(())
    };
    let (_, out, err, status) =
        tokio::try_join!(write, read_output(stdout), read_output(stderr), async {
            Ok(child.wait().await?)
        })?;
    #[cfg(unix)]
    {
        group.0 = None;
    }
    // The reference treats stderr as output, including on nonzero exit.
    let has_output = status.success() || !err.is_empty();
    let message = if !err.is_empty() {
        String::from_utf8_lossy(&err).into_owned()
    } else if status.success() {
        String::from_utf8_lossy(&out).into_owned()
    } else {
        status.code().map_or_else(
            || "Shell command failed".into(),
            |code| format!("Shell command failed: status {code}"),
        )
    };
    Ok((has_output, message))
}

async fn run(
    kind: Shell,
    command: String,
    cwd: std::path::PathBuf,
    rope: ropey::Rope,
    selections: Selections,
) -> anyhow::Result<Vec<Option<String>>> {
    let mut outputs = Vec::new();
    let mut cached = None;
    let mut bytes = 0;
    for r in selections.ranges() {
        let slice = rope.slice(r.start()..r.end());
        let shared = matches!(kind, Shell::Insert | Shell::Append);
        let output = if let Some(value) = &cached {
            Some(String::clone(value))
        } else {
            let input = (!shared).then(|| slice.to_string());
            let (success, mut value) = invoke(&command, &cwd, input.as_deref()).await?;
            if kind == Shell::Keep {
                success.then(String::new)
            } else {
                anyhow::ensure!(success, "{value}");
                if slice.get_char(slice.len_chars().saturating_sub(1)) != Some('\n')
                    && value.ends_with('\n')
                {
                    value.pop();
                    if value.ends_with('\r') {
                        value.pop();
                    }
                }
                if shared {
                    cached = Some(value.clone());
                }
                Some(if kind == Shell::PipeTo {
                    String::new()
                } else {
                    value
                })
            }
        };
        bytes += output.as_ref().map_or(0, String::len);
        anyhow::ensure!(bytes <= OUTPUT_LIMIT, "Combined shell output exceeds 8 MiB");
        outputs.push(output);
    }
    Ok(outputs)
}

impl Editor {
    pub(super) fn start_felix_shell(
        &mut self,
        kind: Shell,
        command: String,
        selections: &Selections,
    ) -> Result<(), String> {
        if self.felix.shell_job.is_some() {
            return Err("A shell command is already running".into());
        }
        let owner = self
            .felix_owner()
            .ok_or("Shell command requires a document window")?;
        let task = tokio::spawn(run(
            kind,
            command,
            self.statusline_git_cache.working_directory.clone(),
            self.current_buffer().contents_snapshot(),
            selections.clone(),
        ));
        self.felix.shell_job = Some(ShellJob {
            owner,
            selections: selections.clone(),
            kind,
            task,
            tail: None,
        });
        Ok(())
    }
    pub(in crate::editor) fn interrupt_felix_shell(&mut self, event: &Event) -> bool {
        let Some(job) = &mut self.felix.shell_job else {
            return false;
        };
        if crate::editor::rpc_commands::interrupts_replay(event) {
            job.tail = None;
        }
        let (source_doc, source_view, _, _) = job.owner;
        if matches!(event,Event::Key(key) if key.code==KeyCode::Char('c') && key.modifiers==KeyModifiers::CONTROL)
            && self.felix_shell_surface()
            && self.current_buffer().id() == source_doc
            && self.window_manager.active_stable_window_id() == Some(source_view)
            && !self.felix.session.awaiting_input()
        {
            self.felix.shell_job = None;
            self.set_legacy_message(Some("Shell command cancelled".into()));
            return true;
        }
        false
    }
    fn felix_shell_surface(&self) -> bool {
        self.current_dialog.is_none()
            && !self.panel_manager.has_focused_panel()
            && !self.workspace_manager.is_active()
            && !self.has_term()
    }

    pub(in crate::editor) fn felix_shell_ready(&self) -> bool {
        self.felix
            .shell_job
            .as_ref()
            .is_some_and(|job| job.task.is_finished())
    }

    pub(in crate::editor) async fn service_felix_shell(
        &mut self,
        frame: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<bool> {
        let Some(mut job) = self.felix.shell_job.take_if(|job| job.task.is_finished()) else {
            return Ok(false);
        };
        let result = (&mut job.task)
            .await
            .map_err(anyhow::Error::from)
            .and_then(|result| result);
        if self.felix_owner() != Some(job.owner)
            || self.felix_active_selection() != Some(&job.selections)
            || !self.felix_shell_surface()
        {
            self.set_legacy_message(Some(
                "Discarded shell result because the document or selections changed".into(),
            ));
        } else {
            match result {
                Ok(outputs) => {
                    self.execute_felix(
                        Input::ShellResult(job.kind, &outputs),
                        job.owner,
                        frame,
                        runtime,
                    )
                    .await?
                }
                Err(error) => self.set_legacy_message(Some(error.to_string())),
            }
            if let Some(tail) = job.tail.take() {
                self.felix.playback = tail;
                Box::pin(self.drain_felix_macro(frame, runtime)).await?;
            }
        }
        Ok(true)
    }
}

/// Decode just the active word, including partially entered quotes. The suffix is
/// replaced as a unit so accepting a filename cannot turn it into shell syntax.
fn active_word(line: &str) -> (usize, String, bool) {
    let mut start = 0;
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut program = true;
    for (i, ch) in line.char_indices() {
        if escaped {
            word.push(ch);
            escaped = false;
        } else if cfg!(unix) && ch == '\\' && quote != Some('\'') {
            escaped = true;
        } else if quote == Some(ch) {
            quote = None;
        } else if quote.is_none() && (ch == '"' || (cfg!(unix) && ch == '\'')) {
            quote = Some(ch);
        } else if quote.is_none() && (ch.is_whitespace() || "|;&".contains(ch)) {
            if !ch.is_whitespace() {
                program = true;
            } else if start < i {
                program = false;
            }
            start = i + ch.len_utf8();
            word.clear();
        } else {
            word.push(ch);
        }
    }
    (start, word, program)
}

fn literal_candidate(name: &str) -> Option<String> {
    if name
        .chars()
        .all(|c| c.is_alphanumeric() || "_-./".contains(c) || c == std::path::MAIN_SEPARATOR)
    {
        return Some(name.into());
    }
    // The prompt recognizes a directory by its final separator. Keep that
    // separator outside any quotes so Enter can descend to the next component.
    let (word, suffix) = name
        .strip_suffix(std::path::MAIN_SEPARATOR)
        .map_or((name, ""), |word| (word, std::path::MAIN_SEPARATOR_STR));
    #[cfg(unix)]
    {
        Some(format!("'{}'{suffix}", word.replace('\'', "'\\''")))
    }
    #[cfg(not(unix))]
    {
        // cmd.exe expands percent variables even inside quotes.
        (!word.contains(['"', '%', '!', '\r', '\n'])).then(|| format!("\"{word}\"{suffix}"))
    }
}

/// Executable names are stable across prompts. Resolve relative filename
/// candidates against this editor's workspace, including in detached sessions.
pub(super) fn completions(line: &str, cwd: &Path) -> (usize, Vec<String>) {
    use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
    static PROGRAMS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    let (start, word, program) = active_word(line);
    let matcher = SkimMatcherV2::default();
    let mut matches = Vec::new();
    if program && !word.contains(std::path::MAIN_SEPARATOR) {
        let programs = PROGRAMS.get_or_init(|| {
            let mut programs = std::collections::BTreeSet::new();
            if let Some(paths) = std::env::var_os("PATH") {
                for directory in std::env::split_paths(&paths) {
                    let Ok(entries) = std::fs::read_dir(directory) else {
                        continue;
                    };
                    for entry in entries.flatten() {
                        if entry
                            .file_type()
                            .is_ok_and(|t| t.is_file() || t.is_symlink())
                        {
                            if let Ok(name) = entry.file_name().into_string() {
                                programs.insert(name);
                            }
                        }
                    }
                }
            }
            programs.into_iter().collect()
        });
        for name in programs {
            if let Some(score) = matcher.fuzzy_match(name, &word) {
                matches.push((std::cmp::Reverse(score), name.clone()));
            }
        }
    } else {
        let from = word
            .rfind(std::path::MAIN_SEPARATOR)
            .map_or(0, |pos| pos + 1);
        let (parent, query) = word.split_at(from);
        if let Ok(entries) = std::fs::read_dir(cwd.join(parent)) {
            for entry in entries.flatten() {
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                if name.starts_with('.') && !query.starts_with('.') {
                    continue;
                }
                if let Some(score) = matcher.fuzzy_match(&name, query) {
                    let mut path = format!("{parent}{name}");
                    if entry.path().is_dir() {
                        path.push(std::path::MAIN_SEPARATOR);
                    }
                    matches.push((std::cmp::Reverse(score), path));
                }
            }
        }
    }
    matches.sort_unstable();
    (
        start,
        matches
            .into_iter()
            .filter_map(|(_, name)| literal_candidate(&name))
            .collect(),
    )
}

#[cfg(test)]
mod directory_tests {
    use super::*;

    #[test]
    fn quoted_directory_candidates_can_be_traversed() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("two words");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("child"), "").unwrap();
        let (_, candidates) = completions("echo two", temp.path());
        let [candidate] = candidates.as_slice() else {
            panic!("{candidates:?}")
        };
        assert!(candidate.ends_with(std::path::MAIN_SEPARATOR));
        let (_, children) = completions(&format!("echo {candidate}"), temp.path());
        assert_eq!(children.len(), 1);
        assert_eq!(
            active_word(&children[0]).1,
            format!("two words{}child", std::path::MAIN_SEPARATOR)
        );
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_oversized_stdout_and_stderr() {
        let cwd = tempfile::tempdir().unwrap();
        for command in ["yes x", "yes x >&2"] {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                invoke(command, cwd.path(), None),
            )
            .await
            .unwrap();
            assert!(result.unwrap_err().to_string().contains("exceeds 8 MiB"));
        }
    }

    #[tokio::test]
    async fn completed_filenames_round_trip_as_one_literal_argument() {
        let cwd = tempfile::tempdir().unwrap();
        let filename = "two words'$(false)";
        std::fs::write(cwd.path().join(filename), "").unwrap();
        for query in ["printf 'two w", "printf two\\ w"] {
            let (start, found) = completions(query, cwd.path());
            assert_eq!(start, "printf ".len());
            let [candidate] = found.as_slice() else {
                panic!("{found:?}")
            };
            let (ok, output) = invoke(&format!("printf %s {candidate}"), cwd.path(), None)
                .await
                .unwrap();
            assert!(ok);
            assert_eq!(output, filename);
        }
        assert_eq!(active_word("cat | gr"), (6, "gr".to_string(), true));
    }
}
