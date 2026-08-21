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
        match kind {
            DiagnosticReportKind::Push => reports.push = diagnostics.to_vec(),
            DiagnosticReportKind::Pull => reports.pull = diagnostics.to_vec(),
        }
        merged_reports(reports)
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
