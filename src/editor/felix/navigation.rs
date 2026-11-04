//! Host-owned destinations that depend on Red's history, workspace, or LSP client.
use super::*;
use felix_core::Navigation;

impl Editor {
    pub(in crate::editor) fn felix_mark_modified(&mut self) {
        let id = self.current_buffer().id();
        self.felix.modified_since_access.insert(id);
    }

    pub(in crate::editor) fn note_felix_buffer_departure(&mut self) {
        let Some((doc, view, _, _)) = self.felix_owner() else {
            return;
        };
        let accessed = self.felix.accessed_docs.entry(view).or_default();
        accessed.retain(|&id| id != doc);
        accessed.push(doc);
        if self.felix.modified_since_access.remove(&doc) {
            let recent = self.felix.modified_docs.entry(view).or_default();
            recent.retain(|&id| id != doc);
            recent.push(doc);
            if recent.len() > 2 {
                recent.remove(0);
            }
        }
    }

    pub(in crate::editor) async fn split_felix_file(
        &mut self,
        vertical: bool,
        frame: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let rope = self.current_buffer().contents_snapshot();
        let text = rope.slice(..);
        let selections = self.felix_active_selection().cloned().unwrap_or_else(|| {
            let cursor = self
                .current_buffer()
                .position_to_char_idx(self.cursor_text_position());
            Selections::single(text, FelixRange::at(text, cursor))
        });
        self.open_felix_paths(
            selected_paths(text, &selections),
            Some(vertical),
            frame,
            runtime,
        )
        .await
    }

    async fn open_felix_paths(
        &mut self,
        paths: Vec<String>,
        vertical: Option<bool>,
        frame: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let root = self
            .current_file_name()
            .and_then(|f| {
                std::path::Path::new(&f)
                    .parent()
                    .map(std::path::Path::to_owned)
            })
            .unwrap_or_default();
        for path in paths {
            let action = if path.starts_with("https://")
                || path.starts_with("http://")
                || path.starts_with("mailto:")
            {
                Action::OpenExternalUrl(path)
            } else {
                let path = if path.starts_with("file://") {
                    PathBuf::from(self.uri_to_file(&path))
                } else if let Some(rest) = path.strip_prefix("~/") {
                    std::env::home_dir().unwrap_or_default().join(rest)
                } else {
                    root.join(path)
                };
                let file = path.to_string_lossy().into_owned();
                match vertical {
                    Some(true) => Action::SplitVerticalWithFile(file),
                    Some(false) => Action::SplitHorizontalWithFile(file),
                    None => Action::OpenFile(file),
                }
            };
            Box::pin(self.execute(&action, frame, runtime)).await?;
        }
        Ok(())
    }

    pub(super) async fn navigate_felix(
        &mut self,
        command: Navigation,
        selections: &Selections,
        frame: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let Some((doc, view, _, _)) = self.felix_owner() else {
            return Ok(());
        };
        let rope = self.current_buffer().contents_snapshot();
        let text = rope.slice(..);
        let extend = self.felix.session.mode == felix_core::Mode::Select;
        let result = match command {
            Navigation::History { later, count } => {
                let (undos, redos, clamped) = self
                    .current_buffer()
                    .undo_history
                    .chronological_path(later, count);
                for _ in 0..undos {
                    self.undo_transaction(frame, runtime).await?;
                }
                for target in redos {
                    if self
                        .current_buffer_mut()
                        .undo_history
                        .choose_redo_node(target)
                    {
                        self.redo_transaction(frame, runtime).await?;
                    }
                }
                if clamped {
                    self.set_legacy_message(Some(
                        if later {
                            "Already at newest change"
                        } else {
                            "Already at oldest change"
                        }
                        .into(),
                    ));
                }
                None
            }
            Navigation::Alternate | Navigation::ModifiedBuffer => {
                let history = if command == Navigation::Alternate {
                    &self.felix.accessed_docs
                } else {
                    &self.felix.modified_docs
                };
                let idx = history
                    .get(&view)
                    .into_iter()
                    .flatten()
                    .rev()
                    .filter(|&&id| id != doc)
                    .find_map(|&id| self.buffer_manager.iter().position(|b| b.id() == id));
                if let Some(idx) = idx {
                    self.set_current_buffer(frame, idx).await?;
                    self.felix.session = Session::default();
                    self.mode = Mode::Normal;
                }
                None
            }
            Navigation::LastEdit => self
                .current_buffer()
                .undo_history
                .latest_transaction()
                .and_then(|tx| {
                    let primary = self
                        .felix
                        .history
                        .get(&(doc, tx.id.clone()))
                        .map(|(s, _)| s.primary());
                    let mut pos = None;
                    for edit in &tx.edits {
                        let crate::undo::TextEdit::Replace {
                            start_char,
                            old_text,
                            new_text,
                            ..
                        } = edit;
                        let removed = old_text.chars().count();
                        let added = new_text.chars().count();
                        let end = start_char + removed;
                        let target = pos.get_or_insert(start_char + added);
                        if primary.is_some_and(|r| r.start() <= end && *start_char <= r.end())
                            || (*start_char <= *target && *target <= end)
                        {
                            *target = start_char + added;
                        } else if end < *target {
                            *target = *target - removed + added;
                        }
                    }
                    pos.map(|p| {
                        selections
                            .transform(text, |r| r.move_to(text, p.min(text.len_chars()), extend))
                    })
                }),
            Navigation::CenterHorizontal => {
                if !self.wrap {
                    let width = self
                        .window_manager
                        .active_window()
                        .map_or_else(|| self.vwidth(), |w| self.window_vwidth(w));
                    self.vleft = self
                        .current_cursor_display_col()
                        .saturating_sub(width.saturating_sub(self.gutter_width()) / 2);
                }
                None
            }
            Navigation::Definition(method) => {
                if let (Some(file), Some(uri)) = (self.current_file_name(), self.current_uri()?) {
                    self.ensure_current_buffer_lsp_opened().await?;
                    let params =
                        json!({"textDocument":{"uri":uri},"position":self.cursor_lsp_position()});
                    match self
                        .lsp
                        .send_request_for_file(&file, method, params, false)
                        .await
                    {
                        Ok(id) => {
                            if let Some(owner) = self.felix_owner() {
                                self.felix.definition_owners.insert(id, owner);
                            }
                            self.pending_definition_requests.insert(id);
                        }
                        Err(e) => self.set_routine_warning(Some(e.to_string())),
                    }
                }
                None
            }
            Navigation::Diagnostic { forward, edge } => self.current_uri()?.and_then(|uri| {
                let diagnostics = self.diagnostics.get(&uri)?;
                let cursor = selections.primary().cursor(text);
                let r = diagnostic_destination(text, diagnostics, cursor, forward, edge)?;
                Some(Selections::single(text, r))
            }),
            Navigation::Change { .. } | Navigation::ChangeObject => {
                match self.current_file_name() {
                    Some(file) => match felix_git_hunks(&file, rope.clone()).await {
                        Some(hunks) => {
                            Some(hunk_destinations(text, selections, &hunks, command, extend))
                        }
                        None => {
                            self.set_legacy_message(Some(
                                "Diff is not available in current buffer".into(),
                            ));
                            None
                        }
                    },
                    None => {
                        self.set_legacy_message(Some(
                            "Diff is not available in current buffer".into(),
                        ));
                        None
                    }
                }
            }
            Navigation::File => {
                let paths = selected_paths(text, selections);
                self.open_felix_paths(paths, None, frame, runtime).await?;
                None
            }
        };
        if let Some(sels) = result {
            self.place_felix_cursor(&sels);
            self.felix.selections.insert((doc, view), sels);
        }
        Ok(())
    }
}

async fn felix_git_hunks(file: &str, rope: ropey::Rope) -> Option<Vec<crate::editing::LineHunk>> {
    let path = std::fs::canonicalize(file)
        .or_else(|_| std::path::absolute(file))
        .ok()?;
    let parent = path.parent()?;
    let name = path.file_name()?.to_str()?;
    let mut cmd = tokio::process::Command::new("git");
    cmd.current_dir(parent)
        .args(["cat-file", "--filters", &format!("HEAD:./{name}")]);
    let output = git_status_output(cmd, Duration::from_secs(3)).await.ok()?;
    if !output.status.success() {
        return None;
    }
    let base = String::from_utf8(output.stdout).ok()?;
    tokio::task::spawn_blocking(move || crate::editing::line_hunks(&base, &rope.to_string()))
        .await
        .ok()
}

fn hunk_destinations(
    text: ropey::RopeSlice<'_>,
    sels: &Selections,
    hunks: &[crate::editing::LineHunk],
    cmd: Navigation,
    extend: bool,
) -> Selections {
    let range = |h: &crate::editing::LineHunk| {
        let anchor = text.line_to_char(h.new_lines.start.min(text.len_lines()));
        let head = if h.new_lines.is_empty() {
            felix_core::boundary(text, anchor, true)
        } else {
            text.line_to_char(h.new_lines.end.min(text.len_lines()))
        };
        FelixRange { anchor, head }
    };
    if let Navigation::Change {
        forward,
        edge: true,
        ..
    } = cmd
    {
        return (if forward { hunks.last() } else { hunks.first() })
            .map_or_else(|| sels.clone(), |h| Selections::single(text, range(h)));
    }
    sels.transform(text, |r| {
        let row = text.char_to_line(r.cursor(text));
        let (h, forward) = match cmd {
            Navigation::ChangeObject => (
                hunks.iter().find(|h| h.new_lines.contains(&row)),
                r.head >= r.anchor,
            ),
            Navigation::Change { forward, count, .. } => {
                let eligible = if forward {
                    hunks.partition_point(|h| h.new_lines.start <= row)
                } else {
                    hunks.partition_point(|h| {
                        h.new_lines.end < row || (h.new_lines.end == row && !h.new_lines.is_empty())
                    })
                };
                let idx = if forward {
                    (eligible < hunks.len())
                        .then(|| eligible.saturating_add(count - 1).min(hunks.len() - 1))
                } else {
                    eligible
                        .checked_sub(1)
                        .map(|index| index.saturating_sub(count - 1))
                };
                (idx.and_then(|i| hunks.get(i)), forward)
            }
            _ => unreachable!(),
        };
        let Some(h) = h else { return r };
        let new = range(h);
        if extend && cmd != Navigation::ChangeObject {
            FelixRange {
                anchor: r.anchor,
                head: if new.head < r.anchor {
                    new.anchor
                } else {
                    new.head
                },
            }
        } else if forward {
            new
        } else {
            FelixRange {
                anchor: new.head,
                head: new.anchor,
            }
        }
    })
}

/// Paths explicitly selected with the cursor take precedence over the token at
/// a single caret. Keep tokenization bounded even on minified source lines.
fn selected_paths(text: ropey::RopeSlice<'_>, sels: &Selections) -> Vec<String> {
    let r = sels.primary();
    if sels.ranges().len() != 1 || r.end() - r.start() != 1 {
        return sels
            .ranges()
            .iter()
            .filter_map(|r| {
                let s = text.slice(r.start()..r.end()).to_string();
                let s = s.trim();
                (!s.is_empty()).then(|| s.to_owned())
            })
            .collect();
    }
    let row = text.char_to_line(r.start());
    let first = text.line_to_char(row).max(r.start().saturating_sub(1000));
    let last = text
        .line_to_char(row + 1)
        .min(r.start().saturating_add(1000));
    let slice = text.slice(first..last);
    let line = slice.to_string();
    let cursor = slice.char_to_byte(r.start() - first);
    static PATH: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?:\\[ \t]|[\w@./:+#$%?!,;~&=\\-])+").expect("valid path pattern")
    });
    PATH.find_iter(&line)
        .find(|token| token.start() <= cursor + 1 && token.end() >= cursor)
        .map(|token| token.as_str().replace("\\ ", " "))
        .into_iter()
        .collect()
}

fn diagnostic_destination(
    text: ropey::RopeSlice<'_>,
    diagnostics: &[Diagnostic],
    cursor: usize,
    forward: bool,
    edge: bool,
) -> Option<FelixRange> {
    let offset = |p: &crate::lsp::Position| {
        if p.line >= text.len_lines() {
            return text.len_chars();
        }
        let line = text.line(p.line);
        let scalar = line.utf16_cu_to_char(p.character.min(line.len_utf16_cu()));
        let line_len = line.len_chars()
            - line
                .chars_at(line.len_chars())
                .reversed()
                .take_while(|c| matches!(c, '\n' | '\r'))
                .count();
        text.line_to_char(p.line) + scalar.min(line_len)
    };
    let ranges = diagnostics.iter().map(|d| FelixRange {
        anchor: offset(&d.range.start),
        head: offset(&d.range.end),
    });
    match (forward, edge) {
        (true, true) => ranges.max_by_key(|r| r.anchor),
        (false, true) => ranges.min_by_key(|r| r.anchor),
        (true, false) => ranges
            .filter(|r| r.anchor > cursor)
            .min_by_key(|r| r.anchor),
        (false, false) => ranges
            .filter(|r| r.anchor < cursor)
            .max_by_key(|r| r.anchor)
            .map(|r| FelixRange {
                anchor: r.head,
                head: r.anchor,
            }),
    }
}

impl Editor {
    // Distinguish requests issued by Felix from native Red LSP requests even
    // when the server replies after the user moved to another view.
    pub(in crate::editor) fn felix_definition_response(
        &mut self,
        message: &InboundMessage,
    ) -> Option<Option<Action>> {
        let (id, response) = match message {
            InboundMessage::Message(response) => (response.id, Ok(&response.result)),
            InboundMessage::RequestError { id, error } => (*id, Err(error)),
            _ => return None,
        };
        let owner = self.felix.definition_owners.remove(&id)?;
        if self.felix_owner() != Some(owner)
            || self.config.editing_model != crate::config::EditingModel::Helix
            || self.panel_manager.has_focused_panel()
            || self.current_dialog.is_some()
        {
            return Some(None);
        }
        let result = match response {
            Ok(value) => value,
            Err(error) => return Some(Some(Action::PrintWarning(error.to_string()))),
        };
        let values = result
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(std::slice::from_ref(result));
        let destinations: Vec<_> = values
            .iter()
            .filter_map(|v| {
                let uri = v.get("targetUri").or_else(|| v.get("uri"))?.as_str()?;
                let range = v.get("targetSelectionRange").or_else(|| v.get("range"))?;
                let start: crate::lsp::Position =
                    serde_json::from_value(range.get("start")?.clone()).ok()?;
                Some(plugin::PluginLocation {
                    path: self.uri_to_file(uri),
                    line: start.line,
                    column: start.character,
                    column_encoding: plugin::LocationColumnEncoding::Utf16,
                })
            })
            .collect();
        let action = match destinations.as_slice() {
            [] => Action::PrintWarning("No LSP destination found".into()),
            [location] => {
                Action::OpenLocation(location.clone(), plugin::OpenLocationTarget::Current)
            }
            _ => {
                let items = destinations
                    .iter()
                    .enumerate()
                    .map(|(i, loc)| PickerItem {
                        id: i.to_string(),
                        label: format!("{}:{}:{}", loc.path, loc.line + 1, loc.column + 1),
                        annotation: None,
                        kind: Some("FilePath".into()),
                        icon: None,
                        detail: None,
                        data: Value::Null,
                        matches: Vec::new(),
                        detail_matches: Vec::new(),
                        preview: Some(PickerPreview::Location {
                            path: loc.path.clone(),
                            line: Some(loc.line),
                            column: None,
                            matches: Vec::new(),
                        }),
                    })
                    .collect();
                let picker = Picker::builder()
                    .title("LSP destinations")
                    .structured_items(items)
                    .select_action(move |id| {
                        id.parse::<usize>()
                            .ok()
                            .and_then(|i| destinations.get(i))
                            .map_or(Action::CloseDialog, |location| {
                                Action::OpenLocation(
                                    location.clone(),
                                    plugin::OpenLocationTarget::Current,
                                )
                            })
                    })
                    .build(self);
                self.current_dialog = Some(Box::new(picker));
                Action::ShowDialog
            }
        };
        Some(Some(action))
    }
}
