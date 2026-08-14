//! Editor-owned diagnostics picker built from the latest URI-keyed LSP snapshot.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use ropey::Rope;
use serde_json::json;

use crate::{
    buffer::Buffer,
    lsp::{normalized_file_path, Diagnostic, DiagnosticSeverity},
    plugin::{LocationColumnEncoding, OpenLocationTarget, PluginLocation},
    ui::{Picker, PickerItem, PickerPreview, MAX_UNFOCUSED_PREVIEW_BYTES},
    unicode_utils::grapheme_to_byte,
    utils::get_workspace_path,
};

use super::{utf16_to_grapheme, Action, Editor};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DiagnosticFilter {
    All,
    Errors,
}

impl DiagnosticFilter {
    fn includes(self, diagnostic: &Diagnostic) -> bool {
        match self {
            Self::All => true,
            Self::Errors => matches!(
                diagnostic.severity.as_ref(),
                Some(DiagnosticSeverity::Error)
            ),
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::All => "Diagnostics",
            Self::Errors => "Errors",
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Self::All => "Filter diagnostics",
            Self::Errors => "Filter errors",
        }
    }

    fn empty_message(self) -> &'static str {
        match self {
            Self::All => "No diagnostics",
            Self::Errors => "No errors",
        }
    }
}

#[derive(Debug)]
struct DiagnosticEntry {
    path: PathBuf,
    display_path: String,
    diagnostic: Diagnostic,
}

struct DiagnosticPickerModel {
    items: Vec<PickerItem>,
    actions: HashMap<String, Action>,
}

impl Editor {
    pub(super) fn open_diagnostics_picker(&mut self, filter: DiagnosticFilter) {
        let diagnostic_paths = filtered_diagnostic_paths(&self.diagnostics, filter);
        let preview_contents = diagnostic_preview_contents(&self.buffer_manager, &diagnostic_paths);
        let model = diagnostic_picker_model(
            &self.diagnostics,
            filter,
            &get_workspace_path(),
            &preview_contents,
        );
        let actions = model.actions;
        let mut picker = Picker::builder()
            .title(filter.title())
            .structured_items(model.items)
            .filter_action(diagnostic_filter_score)
            .placeholder(filter.placeholder())
            .history_key(match filter {
                DiagnosticFilter::All => "diagnostics",
                DiagnosticFilter::Errors => "diagnostic-errors",
            })
            .location_preview_contents(preview_contents)
            .select_action(move |item| {
                actions.get(&item).cloned().unwrap_or_else(|| {
                    Action::Print("diagnostic is no longer available".to_string())
                })
            })
            .build(self);
        picker.set_empty_message(Some(filter.empty_message().to_string()));
        self.current_dialog = Some(Box::new(picker));
    }
}

fn filtered_diagnostic_paths(
    diagnostics_by_uri: &HashMap<String, Vec<Diagnostic>>,
    filter: DiagnosticFilter,
) -> HashSet<String> {
    diagnostics_by_uri
        .iter()
        .filter(|(_, diagnostics)| {
            diagnostics
                .iter()
                .any(|diagnostic| filter.includes(diagnostic))
        })
        .filter_map(|(uri, _)| normalized_file_path(uri).ok())
        .collect()
}

fn diagnostic_preview_contents(
    buffers: &[Buffer],
    diagnostic_paths: &HashSet<String>,
) -> HashMap<String, Rope> {
    buffers
        .iter()
        .filter_map(|buffer| {
            let uri = buffer.uri().ok().flatten()?;
            let path = normalized_file_path(&uri).ok()?;
            diagnostic_paths
                .contains(&path)
                .then(|| (path, buffer.contents_snapshot()))
        })
        .collect()
}

fn diagnostic_picker_model(
    diagnostics_by_uri: &HashMap<String, Vec<Diagnostic>>,
    filter: DiagnosticFilter,
    workspace: &Path,
    preview_contents: &HashMap<String, Rope>,
) -> DiagnosticPickerModel {
    let mut entries = diagnostics_by_uri
        .iter()
        .filter_map(|(uri, diagnostics)| {
            normalized_file_path(uri)
                .ok()
                .map(|path| (PathBuf::from(path), diagnostics))
        })
        .flat_map(|(path, diagnostics)| {
            let display_path = path
                .strip_prefix(workspace)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            diagnostics
                .iter()
                .filter(move |diagnostic| filter.includes(diagnostic))
                .cloned()
                .map(move |diagnostic| DiagnosticEntry {
                    path: path.clone(),
                    display_path: display_path.clone(),
                    diagnostic,
                })
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        severity_rank(left.diagnostic.severity.as_ref())
            .cmp(&severity_rank(right.diagnostic.severity.as_ref()))
            .then_with(|| left.display_path.cmp(&right.display_path))
            .then_with(|| {
                left.diagnostic
                    .range
                    .start
                    .line
                    .cmp(&right.diagnostic.range.start.line)
            })
            .then_with(|| {
                left.diagnostic
                    .range
                    .start
                    .character
                    .cmp(&right.diagnostic.range.start.character)
            })
            .then_with(|| left.diagnostic.message.cmp(&right.diagnostic.message))
    });

    let mut items = Vec::with_capacity(entries.len());
    let mut actions = HashMap::with_capacity(entries.len());
    for (index, entry) in entries.into_iter().enumerate() {
        let id = index.to_string();
        let path = entry.path.to_string_lossy().into_owned();
        let line = entry.diagnostic.range.start.line;
        let column = entry.diagnostic.range.start.character;
        let severity = severity_label(entry.diagnostic.severity.as_ref());
        let origin = diagnostic_origin(&entry.diagnostic);
        let preview_line = preview_contents
            .get(&path)
            .and_then(|contents| diagnostic_preview_line(contents, &entry.diagnostic));
        let display_column = preview_line
            .as_deref()
            .and_then(|line| diagnostic_display_column(line, &entry.diagnostic))
            .unwrap_or(column);
        let location = format!("{}:{}:{}", entry.display_path, line + 1, display_column + 1);
        let annotation = origin.as_ref().map_or_else(
            || location.clone(),
            |origin| format!("{origin}  {location}"),
        );
        let message = entry.diagnostic.message.replace(['\r', '\n'], " ");
        let search_text = format!(
            "{} {} {} {}",
            message,
            entry.display_path,
            origin.as_deref().unwrap_or_default(),
            severity
        );
        let matches = preview_line
            .as_deref()
            .map(|line| diagnostic_preview_matches(line, &entry.diagnostic))
            .unwrap_or_default();

        items.push(PickerItem {
            id: id.clone(),
            icon: None,
            label: message.clone(),
            kind: Some(severity.to_string()),
            annotation: Some(annotation),
            detail: None,
            data: json!({
                "search_text": search_text,
                "location": {
                    "path": path,
                    "line": line,
                    "column": column,
                },
                "annotation_align": "right",
                "compact_annotation": location,
            }),
            matches: Vec::new(),
            detail_matches: Vec::new(),
            preview: Some(PickerPreview::Location {
                path: path.clone(),
                line: Some(line),
                column: Some(column),
                matches,
            }),
        });
        actions.insert(
            id,
            Action::OpenLocation(
                PluginLocation {
                    path,
                    line,
                    column,
                    column_encoding: LocationColumnEncoding::Utf16,
                },
                OpenLocationTarget::Current,
            ),
        );
    }

    DiagnosticPickerModel { items, actions }
}

fn diagnostic_filter_score(item: &PickerItem, query: &str) -> Option<i64> {
    if query.trim().is_empty() {
        return Some(0);
    }
    let matcher = SkimMatcherV2::default();
    let search_text = item
        .data
        .get("search_text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&item.label);
    query.split_whitespace().try_fold(0_i64, |total, token| {
        matcher
            .fuzzy_match(search_text, token)
            .map(|score| total.saturating_add(score))
    })
}

fn diagnostic_preview_line(contents: &Rope, diagnostic: &Diagnostic) -> Option<String> {
    let line = contents.get_line(diagnostic.range.start.line)?;
    if u64::try_from(line.len_bytes()).unwrap_or(u64::MAX) > MAX_UNFOCUSED_PREVIEW_BYTES {
        return None;
    }
    let mut line = line.to_string();
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
    Some(line)
}

fn diagnostic_preview_matches(line: &str, diagnostic: &Diagnostic) -> Vec<[usize; 2]> {
    let range = &diagnostic.range;
    let start = grapheme_to_byte(line, utf16_to_grapheme(line, range.start.character));
    let end = if range.end.line == range.start.line {
        grapheme_to_byte(line, utf16_to_grapheme(line, range.end.character))
    } else {
        line.len()
    };
    (start < end).then_some([start, end]).into_iter().collect()
}

fn diagnostic_display_column(line: &str, diagnostic: &Diagnostic) -> Option<usize> {
    let start = &diagnostic.range.start;
    Some(utf16_to_grapheme(line, start.character))
}

fn severity_rank(severity: Option<&DiagnosticSeverity>) -> u8 {
    match severity {
        Some(DiagnosticSeverity::Error) => 0,
        Some(DiagnosticSeverity::Warning) => 1,
        Some(DiagnosticSeverity::Information) => 2,
        Some(DiagnosticSeverity::Hint) => 3,
        None => 4,
    }
}

fn severity_label(severity: Option<&DiagnosticSeverity>) -> &'static str {
    match severity {
        Some(DiagnosticSeverity::Error) => "Error",
        Some(DiagnosticSeverity::Warning) => "Warning",
        Some(DiagnosticSeverity::Information) => "Info",
        Some(DiagnosticSeverity::Hint) => "Hint",
        None => "Diagnostic",
    }
}

fn diagnostic_origin(diagnostic: &Diagnostic) -> Option<String> {
    let code = diagnostic.code.as_ref().map(|code| code.as_string());
    match (diagnostic.source.as_deref(), code) {
        (Some(source), Some(code)) => Some(format!("{source} ({code})")),
        (Some(source), None) => Some(source.to_string()),
        (None, Some(code)) => Some(code),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::{file_uri, DiagnosticCode, Position, Range};

    fn diagnostic(
        message: &str,
        severity: Option<DiagnosticSeverity>,
        line: usize,
        start: usize,
        end: usize,
    ) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Position {
                    line,
                    character: start,
                },
                end: Position {
                    line,
                    character: end,
                },
            },
            severity,
            code: None,
            source: None,
            message: message.to_string(),
            related_information: None,
            data: None,
            tags: None,
        }
    }

    #[test]
    fn all_diagnostics_sort_by_severity_path_and_position() {
        let workspace = tempfile::tempdir().unwrap();
        let alpha = workspace.path().join("alpha.py");
        let beta = workspace.path().join("beta.py");
        let diagnostics = HashMap::from([
            (
                file_uri(&beta).unwrap(),
                vec![diagnostic(
                    "warning",
                    Some(DiagnosticSeverity::Warning),
                    0,
                    0,
                    1,
                )],
            ),
            (
                file_uri(&alpha).unwrap(),
                vec![
                    diagnostic("hint", Some(DiagnosticSeverity::Hint), 1, 0, 1),
                    diagnostic("error", Some(DiagnosticSeverity::Error), 2, 0, 1),
                ],
            ),
        ]);

        let model = diagnostic_picker_model(
            &diagnostics,
            DiagnosticFilter::All,
            workspace.path(),
            &HashMap::new(),
        );

        assert_eq!(
            model
                .items
                .iter()
                .map(|item| (item.kind.as_deref(), item.label.as_str()))
                .collect::<Vec<_>>(),
            [
                (Some("Error"), "error"),
                (Some("Warning"), "warning"),
                (Some("Hint"), "hint"),
            ]
        );
    }

    #[test]
    fn errors_filter_excludes_other_severities_and_keeps_source_code_searchable() {
        let workspace = tempfile::tempdir().unwrap();
        let file = workspace.path().join("main.py");
        let mut error = diagnostic(
            "Do not assign a lambda",
            Some(DiagnosticSeverity::Error),
            8,
            3,
            9,
        );
        error.source = Some("Ruff".to_string());
        error.code = Some(DiagnosticCode::String("E731".to_string()));
        let diagnostics = HashMap::from([(
            file_uri(&file).unwrap(),
            vec![
                error,
                diagnostic("unused", Some(DiagnosticSeverity::Warning), 0, 0, 1),
            ],
        )]);

        let model = diagnostic_picker_model(
            &diagnostics,
            DiagnosticFilter::Errors,
            workspace.path(),
            &HashMap::new(),
        );

        assert_eq!(model.items.len(), 1);
        assert_eq!(
            model.items[0].annotation.as_deref(),
            Some("Ruff (E731)  main.py:9:4")
        );
        assert!(diagnostic_filter_score(&model.items[0], "ruff e731").is_some());
        assert_eq!(model.actions.len(), 1);
        let Action::OpenLocation(location, OpenLocationTarget::Current) = &model.actions["0"]
        else {
            panic!("diagnostic selection should open its source location");
        };
        assert_eq!(location.path, file.to_string_lossy());
        assert_eq!(location.line, 8);
        assert_eq!(location.column, 3);
        assert_eq!(location.column_encoding, LocationColumnEncoding::Utf16);
    }

    #[test]
    fn preview_matches_convert_utf16_offsets_to_utf8_bytes() {
        let diagnostic = diagnostic("emoji", Some(DiagnosticSeverity::Error), 0, 1, 3);

        assert_eq!(diagnostic_preview_matches("a😀z", &diagnostic), [[1, 5]]);
        assert_eq!(diagnostic_display_column("a😀z", &diagnostic), Some(1));
    }

    #[test]
    fn preview_snapshots_only_include_buffers_with_filtered_diagnostics() {
        let workspace = tempfile::tempdir().unwrap();
        let error_path = workspace.path().join("error.py");
        let warning_path = workspace.path().join("warning.py");
        let unrelated_path = workspace.path().join("unrelated.py");
        let diagnostics = HashMap::from([
            (
                file_uri(&error_path).unwrap(),
                vec![diagnostic(
                    "error",
                    Some(DiagnosticSeverity::Error),
                    0,
                    0,
                    1,
                )],
            ),
            (
                file_uri(&warning_path).unwrap(),
                vec![diagnostic(
                    "warning",
                    Some(DiagnosticSeverity::Warning),
                    0,
                    0,
                    1,
                )],
            ),
        ]);
        let buffers = vec![
            Buffer::new(
                Some(error_path.to_string_lossy().into_owned()),
                "unsaved error\n".to_string(),
            ),
            Buffer::new(
                Some(warning_path.to_string_lossy().into_owned()),
                "unsaved warning\n".to_string(),
            ),
            Buffer::new(
                Some(unrelated_path.to_string_lossy().into_owned()),
                "x".repeat(MAX_UNFOCUSED_PREVIEW_BYTES as usize * 2),
            ),
        ];

        let paths = filtered_diagnostic_paths(&diagnostics, DiagnosticFilter::Errors);
        let snapshots = diagnostic_preview_contents(&buffers, &paths);

        assert_eq!(snapshots.len(), 1);
        assert_eq!(
            snapshots
                .get(&error_path.to_string_lossy().into_owned())
                .map(ToString::to_string)
                .as_deref(),
            Some("unsaved error\n")
        );
        assert!(!snapshots.contains_key(&warning_path.to_string_lossy().into_owned()));
        assert!(!snapshots.contains_key(&unrelated_path.to_string_lossy().into_owned()));
    }
}
