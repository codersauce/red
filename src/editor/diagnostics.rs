//! Independent push and pull reports, combined only for display.
//!
//! A server can publish compiler diagnostics and return its own native diagnostics
//! from a pull request. An empty report clears only the channel that produced it.

use std::collections::HashMap;

use crate::lsp::Diagnostic;

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
        let mut merged = reports.push.clone();
        for diagnostic in &reports.pull {
            if !merged.contains(diagnostic) {
                merged.push(diagnostic.clone());
            }
        }
        merged
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
}
