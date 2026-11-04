//! Native services called by document-window Space bindings.
use super::*;
use felix_core::CommentMode;

impl Editor {
    pub(in crate::editor) async fn felix_native_document_command(
        &mut self,
        action: &Action,
        frame: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let Some(owner) = self.felix_owner() else {
            return Ok(());
        };
        match action {
            Action::FelixChangedFiles => {
                // A fresh picker ID lets an old scan finish after Escape without
                // replacing the rows of a newly opened picker.
                let id = self.felix.changed_files_job.take().map_or(-1, |(id, job)| {
                    job.abort();
                    id.wrapping_sub(1)
                });
                let mut picker = Picker::builder()
                    .id(id)
                    .title("Changed files")
                    .structured_items(Vec::new())
                    .history_key("felix_changed_files")
                    .status("Reading Git status…")
                    .busy(true)
                    .select_action(Action::OpenFile)
                    .build(self);
                picker.set_empty_message(Some("No changed files".into()));
                self.current_dialog = Some(Box::new(picker));
                let cwd = self.statusline_git_cache.working_directory.clone();
                let runtime = runtime.clone();
                let job = tokio::spawn(async move {
                    let status = match changed_files(&cwd).await {
                        Ok(items) => {
                            runtime.send_request(PluginRequest::UpdatePickerItems { id, items });
                            None
                        }
                        Err(error) => Some(error.to_string()),
                    };
                    runtime.send_request(PluginRequest::UpdatePickerStatus { id, status });
                    runtime.send_request(PluginRequest::UpdatePickerBusy { id, busy: false });
                });
                self.felix.changed_files_job = Some((id, job));
            }
            Action::FelixFormatRange => {
                self.felix.range_format_request = None;
                let selected = self
                    .felix_active_selection()
                    .cloned()
                    .unwrap_or_else(|| self.felix_selection_at_cursor());
                anyhow::ensure!(
                    selected.ranges().len() == 1,
                    "format_selections only supports a single selection for now"
                );
                const MISSING: &str = "No configured language server supports range formatting";
                let file = self.current_buffer().file.clone().context(MISSING)?;
                anyhow::ensure!(self.config.lsp.enabled, MISSING);
                self.ensure_current_buffer_lsp_opened().await?;
                anyhow::ensure!(self.lsp.supports_range_formatting(&file), MISSING);
                let selected = selected.primary();
                let source = self.current_buffer();
                let position = |index| source.position_to_lsp(source.char_idx_to_position(index));
                let range = crate::lsp::Range {
                    start: position(selected.start()),
                    end: position(selected.end()),
                };
                let indentation = self.indentation();
                let id = self
                    .lsp
                    .format_range_with_options(
                        &file,
                        range,
                        indentation.tab_width,
                        indentation.expand_tab,
                    )
                    .await?;
                anyhow::ensure!(id > 0, MISSING);
                self.felix.range_format_request = Some((id, owner));
            }
            Action::FelixSymbolHighlights => {
                self.felix.highlight_request = None;
                let file = self
                    .current_buffer()
                    .file
                    .clone()
                    .context("Document highlights require a file-backed buffer")?;
                self.ensure_current_buffer_lsp_opened().await?;
                let pos = self.cursor_lsp_position();
                let id = self
                    .lsp
                    .document_highlight(&file, pos.character, pos.line)
                    .await?;
                anyhow::ensure!(
                    id > 0,
                    "No language server provides document highlights for this file"
                );
                self.felix.highlight_request = Some((id, owner));
            }
            _ => {
                let mode = match action {
                    Action::FelixToggleComments => CommentMode::Auto,
                    Action::FelixLineComments => CommentMode::Line,
                    Action::FelixBlockComments => CommentMode::Block,
                    _ => return Ok(()),
                };
                self.execute_felix(Input::Comment(mode), owner, frame, runtime)
                    .await?;
            }
        }
        Ok(())
    }

    pub(in crate::editor) fn felix_format_request(&mut self, id: i64) -> Option<Owner> {
        self.felix
            .range_format_request
            .take_if(|(pending, _)| *pending == id)
            .map(|(_, owner)| owner)
    }

    pub(in crate::editor) fn felix_native_owner_is_current(&self, owner: Owner) -> bool {
        self.config.editing_model == crate::config::EditingModel::Helix
            && self.felix_owner() == Some(owner)
            && self.current_dialog.is_none()
            && !self.panel_manager.has_focused_panel()
            && !self.workspace_manager.is_active()
    }

    pub(in crate::editor) fn finish_felix_highlight_error(
        &mut self,
        id: i64,
        message: &str,
    ) -> bool {
        let Some((_, owner)) = self
            .felix
            .highlight_request
            .take_if(|(pending, _)| *pending == id)
        else {
            return false;
        };
        if self.felix_native_owner_is_current(owner) {
            self.set_legacy_message(Some(message.to_owned()));
        }
        true
    }

    pub(in crate::editor) fn apply_felix_highlights(&mut self, response: &ResponseMessage) -> bool {
        let Some((_, owner)) = self
            .felix
            .highlight_request
            .take_if(|(pending, _)| *pending == response.id)
        else {
            return false;
        };
        if !self.felix_native_owner_is_current(owner) {
            return true;
        }
        let rope = self.current_buffer().contents_snapshot();
        let text = rope.slice(..);
        let cursor = self.felix_active_selection().map_or_else(
            || {
                self.current_buffer()
                    .position_to_char_idx(self.cursor_text_position())
            },
            |s| s.primary().cursor(text),
        );
        if let Some(selected) = highlight_selections(text, &response.result, cursor) {
            self.set_felix_active_selection(selected);
        }
        true
    }
}

fn utf16_offset(text: ropey::RopeSlice<'_>, position: LspPosition) -> Option<usize> {
    let line = text.get_line(position.line)?;
    let at = position.character;
    let content_len = line.len_chars()
        - line
            .chars_at(line.len_chars())
            .reversed()
            .take_while(|ch| matches!(ch, '\r' | '\n'))
            .count();
    let content = line.slice(..content_len);
    if at > content.len_utf16_cu() {
        return None;
    }
    let col = content.utf16_cu_to_char(at);
    (content.char_to_utf16_cu(col) == at).then(|| text.line_to_char(position.line) + col)
}

fn highlight_selections(
    text: ropey::RopeSlice<'_>,
    value: &Value,
    cursor: usize,
) -> Option<Selections> {
    let mut ranges = Vec::new();
    let mut primary = 0;
    for item in value.as_array()? {
        let Some(range) = item
            .get("range")
            .and_then(|r| serde_json::from_value::<crate::lsp::Range>(r.clone()).ok())
        else {
            continue;
        };
        let (Some(start), Some(end)) = (
            utf16_offset(text, range.start),
            utf16_offset(text, range.end),
        ) else {
            continue;
        };
        if start >= end {
            continue;
        }
        if (start..end).contains(&cursor) {
            primary = ranges.len();
        }
        ranges.push(FelixRange {
            anchor: start,
            head: end,
        });
    }
    Selections::new(text, ranges, primary)
}

async fn changed_files(cwd: &Path) -> anyhow::Result<Vec<PickerItem>> {
    let path = cwd.to_string_lossy().into_owned();
    let root = tokio::task::spawn_blocking(move || git_status_root(&path))
        .await?
        .context("No Git repository for changed files")?;
    let mut command = tokio::process::Command::new("git");
    command
        .arg("-C")
        .arg(&root)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null());
    let output = git_status_output(command, Duration::from_secs(30)).await?;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(parse_git_status_records(&output.stdout, &root)
        .into_iter()
        .filter_map(|row| {
            let path = row["absolute_path"].as_str()?.to_owned();
            Some(PickerItem {
                id: path.clone(),
                label: row["path"].as_str()?.to_owned(),
                annotation: Some(row["status"].as_str()?.to_owned()),
                kind: Some("FilePath".into()),
                icon: None,
                detail: None,
                data: Value::Null,
                matches: Vec::new(),
                detail_matches: Vec::new(),
                preview: Some(PickerPreview::Location {
                    path,
                    line: None,
                    column: None,
                    matches: Vec::new(),
                }),
            })
        })
        .collect())
}

type CommentTokens = (
    Option<&'static str>,
    &'static [(&'static str, &'static str)],
);
// Default line token and block pairs for the languages bundled by Red.
pub(super) fn comment_tokens(language: Option<&str>) -> CommentTokens {
    match language.unwrap_or_default() {
        "rust" => (Some("//"), &[("/*", "*/"), ("/**", "*/"), ("/*!", "*/")]),
        "toml" | "fish" | "python" | "ruby" | "bash" | "yaml" | "make" | "dockerfile" | "sh"
        | "py" | "gitcommit" => (Some("#"), &[]),
        "c" | "cpp" | "c-sharp" | "go" | "javascript" | "jsx" | "typescript" | "tsx" | "svelte"
        | "kotlin" | "swift" | "java" | "husk" => (Some("//"), &[("/*", "*/")]),
        "css" => (None, &[("/*", "*/")]),
        "sql" => (Some("--"), &[("/*", "*/")]),
        "html" | "vue" | "markdown" => (None, &[("<!--", "-->")]),
        "php" => (Some("//"), &[("/**", "*/"), ("/*", "*/")]),
        "lua" => (Some("--"), &[("--[[", "--]]")]),
        "zig" => (Some("//"), &[]),
        "powershell" => (Some("#"), &[("<#", "#>")]),
        _ => (None, &[]),
    }
}
