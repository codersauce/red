//! Independent push and pull reports, combined only for display.
//!
//! A server can publish compiler diagnostics and return its own native diagnostics
//! from a pull request. An empty report clears only the channel that produced it.

use std::collections::HashMap;

use crate::lsp::{Diagnostic, Position, Range};

#[derive(Clone, Copy)]
pub(super) enum DiagnosticReportKind {
    Push,
    Pull,
}

#[derive(Default)]
struct DocumentReports {
    push: Vec<Diagnostic>,
    pull: Vec<Diagnostic>,
    provisional: bool,
}

#[derive(Default)]
pub(super) struct DiagnosticReports {
    documents: HashMap<String, DocumentReports>,
}

impl DiagnosticReports {
    pub(super) fn update(
        &mut self,
        uri: &str,
        kind: DiagnosticReportKind,
        diagnostics: &[Diagnostic],
    ) -> Vec<Diagnostic> {
        let reports = self.documents.entry(uri.to_string()).or_default();
        if reports.provisional {
            reports.push.clear();
            reports.pull.clear();
            reports.provisional = false;
        }
        match kind {
            DiagnosticReportKind::Push => reports.push = diagnostics.to_vec(),
            DiagnosticReportKind::Pull => reports.pull = diagnostics.to_vec(),
        }
        merged_reports(reports)
    }

    /// Restores both channels optimistically until the restarted server responds.
    pub(super) fn restore(
        &mut self,
        uri: String,
        push: Vec<Diagnostic>,
        pull: Vec<Diagnostic>,
    ) -> Vec<Diagnostic> {
        let reports = DocumentReports {
            push,
            pull,
            provisional: true,
        };
        let merged = merged_reports(&reports);
        self.documents.insert(uri, reports);
        merged
    }

    pub(super) fn entries(
        &self,
    ) -> impl Iterator<Item = (&str, &[Diagnostic], &[Diagnostic])> + '_ {
        self.documents.iter().map(|(uri, reports)| {
            (
                uri.as_str(),
                reports.push.as_slice(),
                reports.pull.as_slice(),
            )
        })
    }

    pub(super) fn has_provisional(&self) -> bool {
        self.documents.values().any(|reports| reports.provisional)
    }

    /// Move untouched diagnostic ranges with their text and discard edited ranges.
    ///
    /// Both channels must move together so a later update from one producer cannot
    /// resurrect another producer's old line and column.
    pub(super) fn rebase(
        &mut self,
        uri: &str,
        edit: &Range,
        replacement: &str,
    ) -> Option<Vec<Diagnostic>> {
        let reports = self.documents.get_mut(uri)?;
        for diagnostics in [&mut reports.push, &mut reports.pull] {
            diagnostics.retain_mut(|diagnostic| {
                let Some(range) = rebase_range(&diagnostic.range, edit, replacement) else {
                    return false;
                };
                diagnostic.range = range;
                true
            });
        }
        Some(merged_reports(reports))
    }

    pub(super) fn remove(&mut self, uri: &str) {
        self.documents.remove(uri);
    }

    pub(super) fn rename(&mut self, previous: &str, current: Option<&str>) {
        if let Some(reports) = self.documents.remove(previous) {
            if let Some(current) = current {
                self.documents.insert(current.to_string(), reports);
            }
        }
    }
}

fn merged_reports(reports: &DocumentReports) -> Vec<Diagnostic> {
    let mut merged = reports.push.clone();
    for diagnostic in &reports.pull {
        if !merged.contains(diagnostic) {
            merged.push(diagnostic.clone());
        }
    }
    merged
}

fn position_key(position: Position) -> (usize, usize) {
    (position.line, position.character)
}

fn replacement_end(start: Position, replacement: &str) -> Position {
    let mut end = start;
    for character in replacement.chars() {
        if character == '\n' {
            end.line += 1;
            end.character = 0;
        } else {
            end.character += character.len_utf16();
        }
    }
    end
}

fn shift_position(position: Position, edit: &Range, inserted_end: Position) -> Position {
    if position.line == edit.end.line {
        Position {
            line: inserted_end.line,
            character: inserted_end.character + position.character - edit.end.character,
        }
    } else {
        Position {
            line: inserted_end.line + position.line - edit.end.line,
            character: position.character,
        }
    }
}

fn rebase_range(range: &Range, edit: &Range, replacement: &str) -> Option<Range> {
    let start = position_key(range.start);
    let end = position_key(range.end);
    let edit_start = position_key(edit.start);
    let edit_end = position_key(edit.end);
    let insertion = edit_start == edit_end;
    let point = start == end;
    let intersects = if insertion {
        start < edit_start && edit_start < end
    } else if point {
        edit_start <= start && start < edit_end
    } else {
        start < edit_end && edit_start < end
    };
    if intersects {
        return None;
    }

    if end <= edit_start && !(point && insertion && start == edit_start) {
        return Some(range.clone());
    }
    if start < edit_end {
        return None;
    }

    let inserted_end = replacement_end(edit.start, replacement);
    Some(Range {
        start: shift_position(range.start, edit, inserted_end),
        end: shift_position(range.end, edit, inserted_end),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        buffer::Buffer,
        config::Config,
        editor::{Action, Editor},
        lsp::{
            InboundMessage, LspManager, ParsedNotification, Request, ResponseMessage,
            TextDocumentPublishDiagnostics,
        },
        theme::Theme,
        undo::{TextPosition, TextRange},
    };
    use serde_json::json;
    use std::time::Duration;

    fn diagnostic(message: &str) -> Diagnostic {
        serde_json::from_value(json!({
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
            "severity": 1, "source": "rustc", "message": message
        })).unwrap()
    }

    fn diagnostic_on_line(message: &str, line: usize, start: usize, end: usize) -> Diagnostic {
        let mut diagnostic = diagnostic(message);
        diagnostic.range = Range {
            start: Position {
                line,
                character: start,
            },
            end: Position {
                line,
                character: end,
            },
        };
        diagnostic
    }

    fn editor(file: String, contents: &str, enabled: bool) -> Editor {
        let mut config = Config::default();
        config.lsp.enabled = enabled;
        config.show_diagnostics = true;
        config.formatting.on_save = false;
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            lsp,
            60,
            12,
            config,
            Theme::default(),
            vec![Buffer::new(Some(file), contents.to_string())],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        editor
    }

    fn push(editor: &mut Editor, uri: &str, items: Vec<Diagnostic>) {
        editor.handle_lsp_message(
            &InboundMessage::Notification(ParsedNotification::PublishDiagnostics(
                TextDocumentPublishDiagnostics {
                    uri: Some(uri.to_string()),
                    diagnostics: items,
                },
            )),
            None,
        );
    }

    fn pull(editor: &mut Editor, uri: &str, report: serde_json::Value) {
        editor.handle_lsp_message(
            &InboundMessage::Message(ResponseMessage {
                id: 1,
                result: report,
                request: Some(Request::new(
                    "textDocument/diagnostic",
                    json!({ "textDocument": { "uri": uri } }),
                )),
            }),
            Some("textDocument/diagnostic".to_string()),
        );
    }

    fn diagnostic_workspace(contents: &str) -> (tempfile::TempDir, std::path::PathBuf, String) {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"diagnostic-cache\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let file = root.path().join("main.rs");
        std::fs::write(&file, contents).unwrap();
        let uri = crate::lsp::file_uri(&file).unwrap();
        (root, file, uri)
    }

    fn cached_editor(file: &std::path::Path, contents: &str, cache: &std::path::Path) -> Editor {
        let mut editor = editor(file.to_string_lossy().into_owned(), contents, true);
        editor.enable_diagnostic_cache(cache.to_path_buf());
        editor
    }

    #[test]
    fn pushed_and_pulled_diagnostics_replace_only_their_own_reports() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("main.rs");
        let mut editor = editor(path.to_string_lossy().into_owned(), "x", false);
        let uri = crate::lsp::file_uri(&path).unwrap();
        let compiler = diagnostic("compiler error");
        let native = diagnostic("native error");
        push(&mut editor, &uri, vec![compiler.clone()]);
        pull(&mut editor, &uri, json!({ "kind": "full", "items": [] }));
        assert_eq!(editor.diagnostics[&uri], vec![compiler.clone()]);
        pull(
            &mut editor,
            &uri,
            json!({ "kind": "full", "items": [compiler, native] }),
        );
        assert_eq!(
            editor.diagnostics[&uri],
            vec![compiler.clone(), native.clone()]
        );
        pull(
            &mut editor,
            &uri,
            json!({ "kind": "unchanged", "resultId": "same" }),
        );
        assert_eq!(editor.diagnostics[&uri].len(), 2);
        push(&mut editor, &uri, vec![]);
        assert_eq!(editor.diagnostics[&uri], vec![compiler, native]);
        pull(&mut editor, &uri, json!({ "kind": "full", "items": [] }));
        assert!(editor.diagnostics[&uri].is_empty());
    }

    #[test]
    fn cached_diagnostics_restore_both_reports_and_gutter_signs_after_restart() {
        let (root, file, uri) = diagnostic_workspace("x\n");
        let cache = root.path().join("cache");
        let compiler = diagnostic("cached compiler error");
        let native = diagnostic("cached native error");

        let mut first = cached_editor(&file, "x\n", &cache);
        push(&mut first, &uri, vec![compiler.clone()]);
        pull(
            &mut first,
            &uri,
            json!({ "kind": "full", "items": [native.clone()] }),
        );
        first.persist_diagnostic_cache(true);

        let mut restarted = cached_editor(&file, "x\n", &cache);
        assert_eq!(restarted.diagnostics[&uri], vec![compiler, native]);
        assert!(restarted.diagnostic_reports.has_provisional());
        assert!(restarted.gutter_sign_manager.visible_sign(0, 0).is_some());

        let refresh = restarted.handle_lsp_message(
            &InboundMessage::Message(ResponseMessage {
                id: 1,
                result: json!({}),
                request: Some(Request::new("initialize", json!({}))),
            }),
            Some("initialize".to_string()),
        );
        assert!(matches!(refresh, Some(Action::RefreshDiagnostics)));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            assert_eq!(
                std::fs::metadata(&cache).unwrap().permissions().mode() & 0o777,
                0o700
            );
            let cached_file = std::fs::read_dir(&cache)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            assert_eq!(
                std::fs::metadata(cached_file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn cached_diagnostics_reject_changed_document_contents() {
        let (root, file, uri) = diagnostic_workspace("before\n");
        let cache = root.path().join("cache");
        let mut first = cached_editor(&file, "before\n", &cache);
        push(&mut first, &uri, vec![diagnostic("outdated error")]);
        first.persist_diagnostic_cache(true);

        std::fs::write(&file, "after\n").unwrap();
        let restarted = cached_editor(&file, "after\n", &cache);

        assert!(!restarted.diagnostics.contains_key(&uri));
        assert!(!restarted.diagnostic_reports.has_provisional());
    }

    #[test]
    fn cached_diagnostics_validate_unsaved_buffer_contents_instead_of_disk() {
        let (root, file, uri) = diagnostic_workspace("saved on disk\n");
        let cache = root.path().join("cache");
        let unsaved = "restored unsaved contents\n";
        let mut first = cached_editor(&file, unsaved, &cache);
        push(&mut first, &uri, vec![diagnostic("unsaved buffer error")]);
        first.persist_diagnostic_cache(true);

        let resumed = cached_editor(&file, unsaved, &cache);
        assert_eq!(resumed.diagnostics[&uri][0].message, "unsaved buffer error");

        let clean_restart = cached_editor(&file, "saved on disk\n", &cache);
        assert!(!clean_restart.diagnostics.contains_key(&uri));
    }

    #[test]
    fn cached_diagnostics_reject_changed_language_server_configuration() {
        let (root, file, uri) = diagnostic_workspace("x\n");
        let cache = root.path().join("cache");
        let mut first = cached_editor(&file, "x\n", &cache);
        push(&mut first, &uri, vec![diagnostic("old server error")]);
        first.persist_diagnostic_cache(true);

        let mut restarted = editor(file.to_string_lossy().into_owned(), "x\n", true);
        restarted
            .config
            .lsp
            .servers
            .get_mut("rust")
            .unwrap()
            .args
            .push("--different-configuration".to_string());
        restarted.enable_diagnostic_cache(cache);

        assert!(!restarted.diagnostics.contains_key(&uri));
    }

    #[test]
    fn cached_diagnostics_reject_changed_workspace_manifests() {
        let (root, file, uri) = diagnostic_workspace("x\n");
        let cache = root.path().join("cache");
        let mut first = cached_editor(&file, "x\n", &cache);
        push(&mut first, &uri, vec![diagnostic("old dependency error")]);
        first.persist_diagnostic_cache(true);

        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"changed-dependencies\"\nversion = \"0.2.0\"\n",
        )
        .unwrap();
        let restarted = cached_editor(&file, "x\n", &cache);

        assert!(!restarted.diagnostics.contains_key(&uri));
    }

    #[test]
    fn first_fresh_report_replaces_both_provisional_diagnostic_channels() {
        let (root, file, uri) = diagnostic_workspace("x\n");
        let cache = root.path().join("cache");
        let mut first = cached_editor(&file, "x\n", &cache);
        push(&mut first, &uri, vec![diagnostic("outdated compiler")]);
        pull(
            &mut first,
            &uri,
            json!({ "kind": "full", "items": [diagnostic("outdated native")] }),
        );
        first.persist_diagnostic_cache(true);

        let mut restarted = cached_editor(&file, "x\n", &cache);
        let current = diagnostic("fresh compiler");
        push(&mut restarted, &uri, vec![current.clone()]);

        assert_eq!(restarted.diagnostics[&uri], vec![current]);
        assert!(!restarted.diagnostic_reports.has_provisional());

        let native = diagnostic("fresh native");
        pull(
            &mut restarted,
            &uri,
            json!({ "kind": "full", "items": [native.clone()] }),
        );
        assert_eq!(restarted.diagnostics[&uri][1], native);
    }

    #[test]
    fn fresh_empty_report_removes_cached_findings_for_the_next_restart() {
        let (root, file, uri) = diagnostic_workspace("x\n");
        let cache = root.path().join("cache");
        let mut first = cached_editor(&file, "x\n", &cache);
        push(&mut first, &uri, vec![diagnostic("resolved error")]);
        first.persist_diagnostic_cache(true);

        let mut refreshed = cached_editor(&file, "x\n", &cache);
        push(&mut refreshed, &uri, vec![]);
        assert!(refreshed.diagnostics[&uri].is_empty());
        refreshed.persist_diagnostic_cache(true);

        let restarted = cached_editor(&file, "x\n", &cache);
        assert!(!restarted.diagnostics.contains_key(&uri));
    }

    #[test]
    fn cached_diagnostics_restore_unopened_workspace_documents() {
        let (root, file, _) = diagnostic_workspace("fn main() {}\n");
        let other = root.path().join("other.rs");
        std::fs::write(&other, "broken();\n").unwrap();
        let other_uri = crate::lsp::file_uri(&other).unwrap();
        let cache = root.path().join("cache");
        let mut first = cached_editor(&file, "fn main() {}\n", &cache);
        push(
            &mut first,
            &other_uri,
            vec![diagnostic("unopened document error")],
        );
        first.persist_diagnostic_cache(true);

        let restarted = cached_editor(&file, "fn main() {}\n", &cache);
        assert_eq!(
            restarted.diagnostics[&other_uri][0].message,
            "unopened document error"
        );

        std::fs::write(&other, "fixed();\n").unwrap();
        let changed = cached_editor(&file, "fn main() {}\n", &cache);
        assert!(!changed.diagnostics.contains_key(&other_uri));
    }

    #[test]
    fn cached_diagnostics_load_when_a_workspace_is_opened_after_startup() {
        let (root, file, uri) = diagnostic_workspace("x\n");
        let cache = root.path().join("cache");
        let mut first = cached_editor(&file, "x\n", &cache);
        push(&mut first, &uri, vec![diagnostic("lazy workspace error")]);
        first.persist_diagnostic_cache(true);

        let config = Config {
            show_diagnostics: true,
            ..Config::default()
        };
        let mut restarted = Editor::with_size(
            Box::new(LspManager::new(config.lsp.clone())),
            60,
            12,
            config,
            Theme::default(),
            vec![Buffer::new(None, String::new())],
        )
        .unwrap();
        restarted.test_disable_terminal_output();
        restarted.enable_diagnostic_cache(cache);
        assert!(restarted.diagnostics.is_empty());

        restarted.buffer_manager.add_buffer(Buffer::new(
            Some(file.to_string_lossy().into_owned()),
            "x\n".to_string(),
        ));
        restarted.restore_cached_diagnostics();

        assert_eq!(
            restarted.diagnostics[&uri][0].message,
            "lazy workspace error"
        );
    }

    #[test]
    fn cached_diagnostics_expire_instead_of_restoring_old_findings() {
        let (root, file, uri) = diagnostic_workspace("x\n");
        let cache = root.path().join("cache");
        let mut first = cached_editor(&file, "x\n", &cache);
        push(&mut first, &uri, vec![diagnostic("old cached error")]);
        first.persist_diagnostic_cache(true);

        let cached_file = std::fs::read_dir(&cache)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&cached_file).unwrap()).unwrap();
        saved["captured_at_ms"] = json!(0);
        std::fs::write(&cached_file, serde_json::to_vec(&saved).unwrap()).unwrap();

        let restarted = cached_editor(&file, "x\n", &cache);
        assert!(!restarted.diagnostics.contains_key(&uri));
    }

    #[test]
    fn cached_diagnostics_do_not_cross_workspace_roots() {
        let (first_root, first_file, first_uri) = diagnostic_workspace("x\n");
        let (second_root, second_file, second_uri) = diagnostic_workspace("x\n");
        let cache = tempfile::tempdir().unwrap();
        let mut first = cached_editor(&first_file, "x\n", cache.path());
        push(&mut first, &first_uri, vec![diagnostic("first workspace")]);
        first.persist_diagnostic_cache(true);

        let second = cached_editor(&second_file, "x\n", cache.path());
        assert!(!second.diagnostics.contains_key(&first_uri));
        assert!(!second.diagnostics.contains_key(&second_uri));
        assert_ne!(first_root.path(), second_root.path());
    }

    #[test]
    fn diagnostic_report_rename_and_close_do_not_resurrect_old_items() {
        let mut reports = DiagnosticReports::default();
        let compiler = diagnostic("compiler");
        reports.update(
            "old",
            DiagnosticReportKind::Push,
            std::slice::from_ref(&compiler),
        );
        reports.rename("old", Some("new"));
        assert!(reports
            .update("old", DiagnosticReportKind::Pull, &[])
            .is_empty());
        assert_eq!(
            reports.update("new", DiagnosticReportKind::Pull, &[]),
            vec![compiler]
        );
        reports.remove("new");
        assert!(reports
            .update("new", DiagnosticReportKind::Pull, &[])
            .is_empty());
    }

    #[test]
    fn diagnostic_ranges_follow_new_lines_and_utf16_insertions_in_both_channels() {
        let mut reports = DiagnosticReports::default();
        let uri = "file:///workspace/main.rs";
        let pushed = diagnostic_on_line("compiler", 1, 2, 5);
        let pulled = diagnostic_on_line("native", 2, 1, 4);
        reports.update(uri, DiagnosticReportKind::Push, &[pushed]);
        reports.update(uri, DiagnosticReportKind::Pull, &[pulled]);

        let moved = reports
            .rebase(
                uri,
                &Range {
                    start: Position {
                        line: 0,
                        character: 0,
                    },
                    end: Position {
                        line: 0,
                        character: 0,
                    },
                },
                "intro\n",
            )
            .unwrap();
        assert_eq!(moved[0].range.start.line, 2);
        assert_eq!(moved[1].range.start.line, 3);

        let moved = reports
            .rebase(
                uri,
                &Range {
                    start: Position {
                        line: 2,
                        character: 0,
                    },
                    end: Position {
                        line: 2,
                        character: 0,
                    },
                },
                "😀",
            )
            .unwrap();
        assert_eq!(moved[0].range.start.character, 4);
        assert_eq!(moved[0].range.end.character, 7);
        assert_eq!(moved[1].range.start.line, 3);

        let retained = reports.update(uri, DiagnosticReportKind::Pull, &[]);
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].range.start.line, 2);
        assert_eq!(retained[0].range.start.character, 4);

        let moved = reports
            .rebase(
                uri,
                &Range {
                    start: Position {
                        line: 0,
                        character: 0,
                    },
                    end: Position {
                        line: 1,
                        character: 0,
                    },
                },
                "",
            )
            .unwrap();
        assert_eq!(moved[0].range.start.line, 1);
        assert_eq!(moved[0].range.start.character, 4);
    }

    #[test]
    fn edited_diagnostic_ranges_disappear_until_the_server_recomputes_them() {
        let mut reports = DiagnosticReports::default();
        let uri = "file:///workspace/main.rs";
        reports.update(
            uri,
            DiagnosticReportKind::Push,
            &[diagnostic_on_line("edited", 0, 2, 6)],
        );
        reports.update(
            uri,
            DiagnosticReportKind::Pull,
            &[diagnostic_on_line("untouched", 1, 0, 3)],
        );

        let remaining = reports
            .rebase(
                uri,
                &Range {
                    start: Position {
                        line: 0,
                        character: 3,
                    },
                    end: Position {
                        line: 0,
                        character: 4,
                    },
                },
                "replacement\n",
            )
            .unwrap();

        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].message, "untouched");
        assert_eq!(remaining[0].range.start.line, 2);
        assert!(reports
            .update(uri, DiagnosticReportKind::Pull, &[])
            .is_empty());
    }

    #[test]
    fn diagnostic_annotations_and_gutter_signs_move_with_editor_edits() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("main.rs");
        let mut editor = editor(
            path.to_string_lossy().into_owned(),
            "first\nsecond\nthird\n",
            false,
        );
        let uri = crate::lsp::file_uri(&path).unwrap();
        push(
            &mut editor,
            &uri,
            vec![diagnostic_on_line("second-line problem", 1, 1, 4)],
        );
        assert!(editor.gutter_sign_manager.visible_sign(0, 1).is_some());
        let mut frame = crate::editor::RenderBuffer::new(60, 12, &crate::theme::Style::default());
        let row_text = |frame: &crate::editor::RenderBuffer, line: usize| {
            frame.cells[line * frame.width..(line + 1) * frame.width]
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
        };
        editor.render(&mut frame).unwrap();
        assert!(row_text(&frame, 1).contains("second-line problem"));

        editor.begin_transaction("insert before diagnostic");
        editor.replace_range(TextRange::insertion(TextPosition::new(0, 0)), "inserted\n");
        editor.commit_transaction(editor.cursor_snapshot());
        assert_eq!(editor.diagnostics[&uri][0].range.start.line, 2);
        assert!(editor.gutter_sign_manager.visible_sign(0, 1).is_none());
        assert!(editor.gutter_sign_manager.visible_sign(0, 2).is_some());
        editor.render(&mut frame).unwrap();
        assert!(!row_text(&frame, 1).contains("second-line problem"));
        assert!(row_text(&frame, 2).contains("second-line problem"));

        editor.begin_transaction("edit diagnostic range");
        editor.replace_range(
            TextRange::new(TextPosition::new(2, 2), TextPosition::new(2, 3)),
            "x",
        );
        editor.commit_transaction(editor.cursor_snapshot());
        assert!(editor.diagnostics[&uri].is_empty());
        assert!(editor.gutter_sign_manager.visible_sign(0, 2).is_none());
        editor.render(&mut frame).unwrap();
        assert!(!row_text(&frame, 2).contains("second-line problem"));
    }

    #[tokio::test]
    async fn undo_and_redo_hide_stale_diagnostics_until_recomputed() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("main.rs");
        let mut editor = editor(
            path.to_string_lossy().into_owned(),
            "first\nsecond\n",
            false,
        );
        let uri = crate::lsp::file_uri(&path).unwrap();
        push(
            &mut editor,
            &uri,
            vec![diagnostic_on_line("second-line problem", 1, 0, 3)],
        );

        editor.begin_transaction("insert before diagnostic");
        editor.replace_range(TextRange::insertion(TextPosition::new(0, 0)), "inserted\n");
        editor.commit_transaction(editor.cursor_snapshot());
        assert_eq!(editor.diagnostics[&uri][0].range.start.line, 2);

        editor
            .test_execute_production_action(Action::Undo)
            .await
            .unwrap();
        assert!(!editor.diagnostics.contains_key(&uri));
        assert!(editor.gutter_sign_manager.visible_sign(0, 1).is_none());

        push(
            &mut editor,
            &uri,
            vec![diagnostic_on_line("second-line problem", 1, 0, 3)],
        );
        editor
            .test_execute_production_action(Action::Redo)
            .await
            .unwrap();
        assert!(!editor.diagnostics.contains_key(&uri));
        assert!(editor.gutter_sign_manager.visible_sign(0, 2).is_none());
    }

    async fn wait_for_compiler_report(editor: &mut Editor, uri: &str, message: Option<&str>) {
        tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                if let Some((incoming, method)) = editor.lsp.recv_response().await.unwrap() {
                    let expected = match &incoming {
                        InboundMessage::Notification(ParsedNotification::PublishDiagnostics(
                            report,
                        )) if report.uri.as_deref() == Some(uri) => match message {
                            Some(message) => report
                                .diagnostics
                                .iter()
                                .any(|d| d.message.contains(message)),
                            None => report.diagnostics.is_empty(),
                        },
                        _ => false,
                    };
                    if let Some(action) = editor.handle_lsp_message(&incoming, method) {
                        editor.test_execute_production_action(action).await.unwrap();
                    }
                    if expected {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("rust-analyzer should publish the updated compiler report");
    }

    async fn replace_and_save(editor: &mut Editor, contents: &str) {
        let end = editor.current_buffer().char_idx_to_position(usize::MAX);
        editor
            .current_buffer_mut()
            .replace_range_raw(TextRange::new(TextPosition::new(0, 0), end), contents);
        let file = editor.current_buffer().file.clone().unwrap();
        editor
            .lsp
            .did_change(&file, contents.to_string())
            .await
            .unwrap();
        editor
            .test_execute_production_action(Action::Save)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn real_rust_analyzer_refreshes_compiler_errors_after_repeated_saves() {
        if std::env::var_os("RED_RUN_REAL_LSP_TESTS").is_none() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("Cargo.toml"),
            "[package]\nname = \"red-diagnostics-repro\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        let path = root.path().join("src/main.rs");
        let good = "fn main() {}\n";
        let bad = "fn main() { let _ = 0.5f; }\n";
        std::fs::write(&path, bad).unwrap();
        let mut editor = editor(path.to_string_lossy().into_owned(), bad, true);
        let uri = crate::lsp::file_uri(&path).unwrap();
        editor.ensure_buffer_lsp_opened(0).await.unwrap();
        wait_for_compiler_report(&mut editor, &uri, Some("invalid suffix")).await;
        replace_and_save(&mut editor, good).await;
        wait_for_compiler_report(&mut editor, &uri, None).await;
        for _ in 0..2 {
            replace_and_save(&mut editor, bad).await;
            wait_for_compiler_report(&mut editor, &uri, Some("invalid suffix")).await;
            pull(&mut editor, &uri, json!({ "kind": "full", "items": [] }));
            assert!(editor.diagnostics[&uri]
                .iter()
                .any(|d| d.message.contains("invalid suffix")));
            replace_and_save(&mut editor, good).await;
            wait_for_compiler_report(&mut editor, &uri, None).await;
        }
        editor.lsp.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn real_rust_analyzer_relinks_externally_changed_parent_modules() {
        if std::env::var_os("RED_RUN_REAL_LSP_TESTS").is_none() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("src");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"red-module-watch-repro\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let parent = source.join("main.rs");
        let child = source.join("recap.rs");
        std::fs::write(&parent, "fn main() {}\n").unwrap();
        std::fs::write(&child, "pub fn recap() {}\n").unwrap();
        let uri = crate::lsp::file_uri(&child).unwrap();
        let mut editor = editor(
            child.to_string_lossy().into_owned(),
            "pub fn recap() {}\n",
            true,
        );
        editor.ensure_buffer_lsp_opened(0).await.unwrap();

        for expect_unlinked in [true, false] {
            if !expect_unlinked {
                std::fs::write(&parent, "mod recap;\nfn main() {}\n").unwrap();
            }
            tokio::time::timeout(Duration::from_secs(45), async {
                loop {
                    let unlinked = editor.diagnostics.get(&uri).is_some_and(|diagnostics| {
                        diagnostics.iter().any(|diagnostic| {
                            diagnostic
                                .code
                                .as_ref()
                                .is_some_and(|code| code.as_string() == "unlinked-file")
                        })
                    });
                    if unlinked == expect_unlinked
                        && (!expect_unlinked || editor.diagnostics.contains_key(&uri))
                    {
                        break;
                    }
                    if let Some((incoming, method)) = editor.lsp.recv_response().await.unwrap() {
                        if let Some(action) = editor.handle_lsp_message(&incoming, method) {
                            editor.test_execute_production_action(action).await.unwrap();
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap_or_else(|_| {
                panic!("expected unlinked-file diagnostic presence to become {expect_unlinked}")
            });
        }

        editor.lsp.shutdown().await.unwrap();
    }
}
