//! Bounded, read-only project inspection for an inline request. Editor snapshots
//! override disk contents, and no operation opens a buffer or changes source.

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{io::AsyncReadExt, process::Command, time::timeout};

#[cfg(all(test, unix))]
mod tests;

pub(crate) const MAX_FILE_BYTES: usize = 512 * 1024;
pub(crate) const MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 32 * 1024;
const MAX_FILES: usize = 1024;
const MAX_MATCHES: usize = 100;
const MAX_SEARCH_BYTES: usize = 16 * 1024 * 1024;

/// The complete read-only tool allowlist for an inline provider thread.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "tool", rename_all = "snake_case", deny_unknown_fields)]
pub enum InlineContextCall {
    ListFiles {},
    SearchFiles {
        query: String,
    },
    ReadFile {
        path: String,
        #[serde(default = "first_line")]
        start_line: usize,
        #[serde(default = "default_line_count")]
        line_count: usize,
    },
    ReadGitDiff {
        path: String,
    },
}

const fn first_line() -> usize {
    1
}
const fn default_line_count() -> usize {
    200
}

impl InlineContextCall {
    pub(crate) fn parse(name: &str, arguments: Value) -> Result<Self> {
        let Value::Object(mut arguments) = arguments else {
            anyhow::bail!("inline context arguments must be an object")
        };
        ensure!(
            !arguments.contains_key("tool"),
            "inline context arguments cannot override the tool name"
        );
        arguments.insert("tool".into(), name.into());
        let call: Self = serde_json::from_value(Value::Object(arguments))
            .context("unsupported or invalid inline context tool")?;
        match &call {
            Self::SearchFiles { query } => ensure!(
                !query.is_empty() && query.len() <= 1024,
                "invalid search query"
            ),
            Self::ReadFile {
                start_line,
                line_count,
                ..
            } => ensure!(
                *start_line > 0 && (1..=200).contains(line_count),
                "read_file requires a positive start_line and 1–200 lines"
            ),
            _ => {}
        }
        Ok(call)
    }

    pub(crate) fn path(&self) -> Option<&str> {
        match self {
            Self::ReadFile { path, .. } | Self::ReadGitDiff { path } => Some(path),
            _ => None,
        }
    }

    pub(crate) fn describe_result(&self, value: &Value) -> String {
        let source = if let Some(revision) = value["revision"].as_u64() {
            format!("editor revision {revision}")
        } else {
            "disk".into()
        };
        let description = match self {
            Self::ListFiles {} => "Listed project files".into(),
            Self::SearchFiles { query } => format!("Searched project for {query:?}"),
            Self::ReadFile { .. } => format!(
                "Read {}:{}–{} · {source}",
                value["path"].as_str().unwrap_or_default(),
                value["start_line"],
                value["end_line"]
            ),
            Self::ReadGitDiff { .. } => format!(
                "Compared {} with HEAD {} · {source}",
                value["path"].as_str().unwrap_or_default(),
                value["base_commit"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(12)
                    .collect::<String>()
            ),
        };
        crate::ui::first_prompt_line(bounded_text(&description, 512))
            .chars()
            .filter(|ch| {
                !ch.is_control() && !matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            })
            .collect()
    }
}

pub(crate) fn tool_definitions() -> Vec<Value> {
    vec![
        json!({"type":"function","name":"list_files","description":"List readable project files without changing editor focus. Respects ignores and sensitive-file exclusions.","inputSchema":{"type":"object","properties":{},"additionalProperties":false}}),
        json!({"type":"function","name":"search_files","description":"Search literal text in project files. Unsaved editor buffers override disk. Results use one-based file lines and may be truncated.","inputSchema":{"type":"object","properties":{"query":{"type":"string","minLength":1,"maxLength":1024}},"required":["query"],"additionalProperties":false}}),
        json!({"type":"function","name":"read_file","description":"Read up to 200 lines of a project file without opening it. Unsaved buffer text is authoritative. Returned line numbers are file-relative, not target-relative.","inputSchema":{"type":"object","properties":{"path":{"type":"string"},"start_line":{"type":"integer","minimum":1},"line_count":{"type":"integer","minimum":1,"maximum":200}},"required":["path"],"additionalProperties":false}}),
        json!({"type":"function","name":"read_git_diff","description":"Compare one tracked file at HEAD with its current editor text, including unsaved changes. Returns a bounded unified diff and the exact base commit. Does not modify Git or the editor.","inputSchema":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}}),
    ]
}

#[derive(Debug, Clone)]
pub(crate) struct VisibleText {
    pub content: String,
    pub revision: u64,
    pub dirty: bool,
}

/// `None` masks an oversized open buffer: never silently fall back to stale disk.
#[derive(Debug)]
pub(crate) struct InlineContextSnapshot {
    pub root: PathBuf,
    pub visible: BTreeMap<String, Option<VisibleText>>,
}

pub(crate) fn resolve_path(root: &Path, path: &str) -> Result<(PathBuf, String)> {
    crate::codex::validate_workspace_root(root)?;
    let physical = crate::codex::physical_workspace_root(Path::new(path));
    let full = crate::editor::resolve_agent_tool_path(
        root,
        physical.to_str().context("file path is not UTF-8")?,
    )?;
    let relative = full.strip_prefix(root)?;
    ensure!(!relative.as_os_str().is_empty(), "expected a project file");
    for component in relative.components() {
        let name = Path::new(component.as_os_str());
        ensure!(
            !component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(".git")
                && !crate::editor::agent_context_path_is_sensitive(name),
            "inline context path is restricted"
        );
    }
    let relative = relative
        .to_str()
        .context("file path is not UTF-8")?
        .replace('\\', "/");
    Ok((full, relative))
}

struct FileText {
    content: String,
    revision: Option<u64>,
    dirty: bool,
}

impl FileText {
    fn source(&self) -> &'static str {
        if self.revision.is_some() {
            "editor"
        } else {
            "disk"
        }
    }
}

impl InlineContextSnapshot {
    pub(crate) async fn execute(self, call: InlineContextCall) -> Result<Value> {
        let value = match call {
            InlineContextCall::ReadGitDiff { path } => self.git_diff(&path).await?,
            call => tokio::task::spawn_blocking(move || self.execute_read(call))
                .await
                .context("inline context task failed")??,
        };
        ensure!(
            serde_json::to_vec(&value)?.len() <= 256 * 1024,
            "inline context response is too large; narrow the request"
        );
        Ok(value)
    }

    fn read(&self, path: &str) -> Result<(String, FileText)> {
        let (_, relative) = resolve_path(&self.root, path)?;
        let text = match self.visible.get(&relative) {
            Some(Some(text)) => FileText {
                content: text.content.clone(),
                revision: Some(text.revision),
                dirty: text.dirty,
            },
            Some(None) => anyhow::bail!("open buffer exceeds the inline context limit"),
            None => {
                let content = crate::codex::read_inline_workspace_file(
                    &self.root,
                    &relative,
                    MAX_FILE_BYTES,
                )?
                .context("file is unavailable, binary, or exceeds the 512 KiB limit")?;
                FileText {
                    content,
                    revision: None,
                    dirty: false,
                }
            }
        };
        ensure!(
            !text.content.contains('\0'),
            "inline context cannot read binary data"
        );
        Ok((relative, text))
    }

    fn files(&self) -> Result<(BTreeSet<String>, bool)> {
        let started = Instant::now();
        let mut files = BTreeSet::new();
        let mut truncated = false;
        for (index, entry) in ignore::WalkBuilder::new(&self.root)
            .follow_links(false)
            .hidden(false)
            .build()
            .enumerate()
        {
            if index >= 65_536
                || started.elapsed() >= Duration::from_secs(5)
                || files.len() >= MAX_FILES
            {
                truncated = true;
                break;
            }
            let Ok(entry) = entry else { continue };
            if entry.file_type().is_some_and(|kind| kind.is_file()) {
                if let Some(path) = entry.path().to_str() {
                    if let Ok((_, relative)) = resolve_path(&self.root, path) {
                        files.insert(relative);
                    }
                }
            }
        }
        // Open files are most relevant and include newly created, unsaved files.
        for path in self.visible.keys() {
            if resolve_path(&self.root, path).is_err() {
                continue;
            }
            if files.len() >= MAX_FILES && !files.contains(path) {
                truncated = true;
                files.pop_last();
            }
            files.insert(path.clone());
        }
        Ok((files, truncated))
    }

    fn execute_read(self, call: InlineContextCall) -> Result<Value> {
        match call {
            InlineContextCall::ListFiles {} => {
                let (files, truncated) = self.files()?;
                Ok(json!({"files":files,"truncated":truncated}))
            }
            InlineContextCall::SearchFiles { query } => {
                ensure!(
                    !query.is_empty() && query.len() <= 1024,
                    "invalid search query"
                );
                let (files, mut truncated) = self.files()?;
                let started = Instant::now();
                let mut files = files.into_iter().collect::<Vec<_>>();
                files.sort_by_key(|path| !self.visible.contains_key(path));
                let mut matches = Vec::new();
                let mut searched = 0;
                let mut output_bytes = 0;
                'files: for path in files {
                    if started.elapsed() >= Duration::from_secs(5) {
                        truncated = true;
                        break;
                    }
                    let Ok((path, text)) = self.read(&path) else {
                        truncated = true;
                        continue;
                    };
                    searched += text.content.len();
                    if searched > MAX_SEARCH_BYTES {
                        truncated = true;
                        break;
                    }
                    for (line, content) in text.content.lines().enumerate() {
                        if content.contains(&query) {
                            let snippet = content.chars().take(300).collect::<String>();
                            output_bytes += path.len() + snippet.len();
                            if matches.len() == MAX_MATCHES || output_bytes > MAX_TEXT_BYTES {
                                truncated = true;
                                break 'files;
                            }
                            matches.push(json!({"path":path,"line":line+1,"text":snippet,"source":text.source(),"revision":text.revision}));
                        }
                    }
                }
                Ok(json!({"matches":matches,"truncated":truncated}))
            }
            InlineContextCall::ReadFile {
                path,
                start_line,
                line_count,
            } => {
                ensure!(
                    start_line > 0 && (1..=200).contains(&line_count),
                    "invalid file line range"
                );
                let (path, text) = self.read(&path)?;
                let lines = text.content.split_inclusive('\n').collect::<Vec<_>>();
                let start = start_line.saturating_sub(1).min(lines.len());
                let mut end = start;
                let mut content = String::new();
                let mut clipped_line = false;
                for line in lines.iter().skip(start).take(line_count) {
                    if content.len() + line.len() > MAX_TEXT_BYTES {
                        if content.is_empty() {
                            content.push_str(bounded_text(line, MAX_TEXT_BYTES));
                            end += 1;
                            clipped_line = true;
                        }
                        break;
                    }
                    content.push_str(line);
                    end += 1;
                }
                Ok(
                    json!({"path":path,"source":text.source(),"revision":text.revision,"unsaved":text.dirty,"start_line":start+1,"end_line":end,"content":content,"truncated":clipped_line || end < lines.len(),"line_truncated":clipped_line,"next_line":(end < lines.len()).then_some(end+1)}),
                )
            }
            InlineContextCall::ReadGitDiff { .. } => {
                unreachable!("git requests use the async path")
            }
        }
    }

    async fn git_diff(self, path: &str) -> Result<Value> {
        let root = self.root.clone();
        let path = path.to_string();
        let (path, current) = tokio::task::spawn_blocking(move || self.read(&path))
            .await
            .context("inline file read failed")??;
        let commit = git_output(&root, &["rev-parse", "--verify", "HEAD^{commit}"], 256).await?;
        let commit = std::str::from_utf8(&commit)?.trim().to_string();
        let prefix = git_output(&root, &["rev-parse", "--show-prefix"], 4096).await?;
        let prefix = std::str::from_utf8(&prefix)?.trim_end_matches('\n');
        let object = format!("{commit}:{prefix}{path}");
        let size = git_output(&root, &["cat-file", "-s", &object], 64)
            .await
            .context("file is not available at HEAD")?;
        ensure!(
            std::str::from_utf8(&size)?.trim().parse::<usize>()? <= MAX_FILE_BYTES,
            "committed file exceeds the 512 KiB limit"
        );
        let before = String::from_utf8(
            git_output(&root, &["cat-file", "blob", &object], MAX_FILE_BYTES).await?,
        )
        .context("committed file is not UTF-8")?;
        ensure!(
            !before.contains('\0'),
            "committed file contains binary data"
        );
        tokio::task::spawn_blocking(move || {
        let diff = similar::TextDiff::configure()
            .timeout(Duration::from_millis(250))
            .diff_lines(&before, &current.content)
            .unified_diff()
            .header(&format!("HEAD/{path}"), &format!("current/{path}"))
            .to_string();
        json!(
            {"path":path,"base_commit":commit,"source":current.source(),"revision":current.revision,"unsaved":current.dirty,"diff":bounded_text(&diff, MAX_TEXT_BYTES),"truncated":diff.len()>MAX_TEXT_BYTES}
        )
        }).await.context("inline diff failed")
    }
}

fn bounded_text(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Only fixed read-only Git plumbing is used: no shell, diff driver, hook, or fetch.
async fn git_output(root: &Path, args: &[&str], limit: usize) -> Result<Vec<u8>> {
    timeout(Duration::from_secs(5), async {
        let mut child = Command::new("git")
            .arg("--no-optional-locks")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("could not start Git")?;
        let mut output = Vec::new();
        child
            .stdout
            .take()
            .context("missing Git output")?
            .take(limit as u64 + 1)
            .read_to_end(&mut output)
            .await?;
        if output.len() > limit {
            let _ = child.kill().await;
            anyhow::bail!("Git context exceeds the output limit");
        }
        ensure!(child.wait().await?.success(), "Git context is unavailable");
        Ok(output)
    })
    .await
    .context("Git context timed out")?
}
