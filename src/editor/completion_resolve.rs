//! Deferred completion details, including import edits computed only after selection.

use super::{
    Action, CompletionResponse, CompletionResponseItem, CompletionSnapshot, Editor, ResponseMessage,
};

#[derive(Debug, Clone)]
pub(super) struct PendingCompletionResolution {
    item: CompletionResponseItem,
    commit_character: Option<char>,
    snapshot: CompletionSnapshot,
}

#[derive(Debug, Clone)]
pub(super) struct PendingCompletionRefresh {
    item: CompletionResponseItem,
    commit_character: Option<char>,
    snapshot: CompletionSnapshot,
}

fn completion_candidate_matches(
    selected: &CompletionResponseItem,
    candidate: &CompletionResponseItem,
) -> bool {
    selected.label == candidate.label
        && selected.kind == candidate.kind
        && selected.label_details == candidate.label_details
        && selected.detail == candidate.detail
}

fn refreshed_completion_item(
    selected: &CompletionResponseItem,
    response: CompletionResponse,
) -> Option<CompletionResponseItem> {
    let mut candidates = response
        .items()
        .into_iter()
        .filter(|candidate| completion_candidate_matches(selected, candidate))
        .collect::<Vec<_>>();
    if candidates.len() == 1 {
        return candidates.pop();
    }
    let exact_data = candidates
        .iter()
        .position(|candidate| candidate.data == selected.data)?;
    Some(candidates.swap_remove(exact_data))
}

impl Editor {
    /// Ask the originating server for edits it intentionally omitted from its list.
    pub(super) async fn resolve_completion_item(
        &mut self,
        item: &CompletionResponseItem,
        commit_character: Option<char>,
    ) -> anyhow::Result<bool> {
        if item.data.is_none()
            || item
                .additional_text_edits
                .as_ref()
                .is_some_and(|edits| !edits.is_empty())
        {
            return Ok(false);
        }

        let Some(file) = self.current_buffer().file.clone() else {
            return Ok(false);
        };
        let resolves_completion = self
            .lsp
            .server_capabilities_for_file(&file)
            .and_then(|capabilities| capabilities.completion_provider.as_ref())
            .and_then(|completion| completion.resolve_provider)
            .unwrap_or(false);
        let has_deferred_imports = item
            .data
            .as_ref()
            .and_then(|data| data.get("imports"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|imports| !imports.is_empty());
        // Some rust-analyzer versions report resolveProvider=false while still
        // returning fly-import candidates that can only be completed by resolve.
        if !resolves_completion && !has_deferred_imports {
            return Ok(false);
        }

        let snapshot = self
            .completion_snapshot
            .clone()
            .unwrap_or_else(|| self.activate_completion_snapshot(self.completion_snapshot()));
        if !self.completion_snapshot_is_current(&snapshot) {
            return Ok(false);
        }

        if snapshot.initial_revision != snapshot.revision {
            let position = self.cursor_lsp_position();
            if let Some(uri) = snapshot.uri.as_deref() {
                match self
                    .lsp
                    .request_completion(uri, position.line, position.character, None)
                    .await
                {
                    Ok(request_id) if request_id > 0 => {
                        self.pending_completion_refreshes.insert(
                            request_id,
                            PendingCompletionRefresh {
                                item: item.clone(),
                                commit_character,
                                snapshot,
                            },
                        );
                        return Ok(true);
                    }
                    Ok(_) => {}
                    Err(error) => {
                        self.set_legacy_message(Some(format!(
                            "completion refresh failed; resolving the earlier item: {error}"
                        )));
                    }
                }
            }
        }

        let request_id = match self
            .lsp
            .send_request_for_file(
                &file,
                "completionItem/resolve",
                serde_json::to_value(item)?,
                false,
            )
            .await
        {
            Ok(request_id) if request_id > 0 => request_id,
            Ok(_) => return Ok(false),
            Err(error) => {
                self.set_legacy_message(Some(format!(
                    "completion resolve failed; inserting without deferred edits: {error}"
                )));
                return Ok(false);
            }
        };

        self.pending_completion_resolutions.insert(
            request_id,
            PendingCompletionResolution {
                item: item.clone(),
                commit_character,
                snapshot,
            },
        );
        Ok(true)
    }

    pub(super) fn resolved_completion_action(
        &mut self,
        response: &ResponseMessage,
    ) -> Option<Action> {
        let pending = self.pending_completion_resolutions.remove(&response.id)?;
        let item = match serde_json::from_value::<CompletionResponseItem>(response.result.clone()) {
            Ok(item) => item,
            Err(error) => {
                self.set_legacy_message(Some(format!(
                    "invalid completion resolve response; inserting without deferred edits: {error}"
                )));
                pending.item.clone()
            }
        };
        self.apply_resolved_completion_action(pending, item)
    }

    pub(super) fn completion_resolution_failed(
        &mut self,
        request_id: i64,
        error: &str,
    ) -> Option<Action> {
        let pending = self.pending_completion_resolutions.remove(&request_id)?;
        self.set_legacy_message(Some(format!(
            "completion resolve failed; inserting without deferred edits: {error}"
        )));
        let item = pending.item.clone();
        self.apply_resolved_completion_action(pending, item)
    }

    fn apply_resolved_completion_action(
        &mut self,
        pending: PendingCompletionResolution,
        item: CompletionResponseItem,
    ) -> Option<Action> {
        if !self.is_insert() || !self.completion_snapshot_is_current(&pending.snapshot) {
            self.set_legacy_message(Some(
                "completion resolve response is stale; buffer or cursor changed".to_string(),
            ));
            return None;
        }

        self.completion_snapshot = Some(pending.snapshot);
        Some(Action::ApplyResolvedCompletion {
            item: Box::new(item),
            commit_character: pending.commit_character,
        })
    }

    pub(super) fn refreshed_completion_action(
        &mut self,
        response: &ResponseMessage,
    ) -> Option<Action> {
        let pending = self.pending_completion_refreshes.remove(&response.id)?;
        if !self.is_insert() || !self.completion_snapshot_is_current(&pending.snapshot) {
            self.set_legacy_message(Some(
                "completion refresh response is stale; buffer or cursor changed".to_string(),
            ));
            return None;
        }

        let refreshed = serde_json::from_value::<CompletionResponse>(response.result.clone())
            .ok()
            .and_then(|response| refreshed_completion_item(&pending.item, response));
        self.finish_completion_refresh(pending, refreshed)
    }

    pub(super) fn completion_refresh_failed(
        &mut self,
        request_id: i64,
        error: &str,
    ) -> Option<Action> {
        let pending = self.pending_completion_refreshes.remove(&request_id)?;
        self.set_legacy_message(Some(format!(
            "completion refresh failed; inserting without deferred edits: {error}"
        )));
        self.finish_completion_refresh(pending, None)
    }

    fn finish_completion_refresh(
        &mut self,
        mut pending: PendingCompletionRefresh,
        refreshed: Option<CompletionResponseItem>,
    ) -> Option<Action> {
        let action = if let Some(item) = refreshed {
            pending.snapshot.initial_revision = pending.snapshot.revision;
            pending.snapshot.original_range = pending.snapshot.current_range.clone();
            Action::ApplyCompletion {
                item: Box::new(item),
                commit_character: pending.commit_character,
            }
        } else {
            self.set_legacy_message(Some(
                "completion refresh did not return the selected item; inserting without deferred edits"
                    .to_string(),
            ));
            Action::ApplyResolvedCompletion {
                item: Box::new(pending.item),
                commit_character: pending.commit_character,
            }
        };
        self.completion_snapshot = Some(pending.snapshot);
        Some(action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        buffer::Buffer,
        config::Config,
        editor::Mode,
        lsp::{
            CompletionResponse, InboundMessage, LspError, LspManager, Position, Range,
            ResponseError, TextEdit,
        },
        theme::Theme,
    };
    use serde_json::json;
    use std::time::Duration;

    fn editor(contents: &str) -> Editor {
        let mut config = Config::default();
        config.lsp.enabled = false;
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            lsp,
            80,
            12,
            config,
            Theme::default(),
            vec![Buffer::new(
                Some("/tmp/red-completion-resolve.rs".to_string()),
                contents.to_string(),
            )],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        editor.mode = Mode::Insert;
        editor.cy = 1;
        editor.cx = 3;
        editor
    }

    fn completion() -> CompletionResponseItem {
        serde_json::from_value(json!({
            "label": "GalacticWidget",
            "textEdit": {
                "range": {
                    "start": { "line": 1, "character": 0 },
                    "end": { "line": 1, "character": 3 }
                },
                "newText": "GalacticWidget"
            },
            "data": { "resolve": "galactic-widget" }
        }))
        .unwrap()
    }

    fn deferred_completion(end_character: usize) -> CompletionResponseItem {
        serde_json::from_value(json!({
            "label": "GalacticWidget",
            "textEdit": {
                "range": {
                    "start": { "line": 1, "character": 0 },
                    "end": { "line": 1, "character": end_character }
                },
                "newText": "GalacticWidget"
            },
            "data": {
                "position": {
                    "textDocument": { "uri": "file:///tmp/red-completion-resolve.rs" },
                    "position": { "line": 1, "character": 3 }
                },
                "imports": [{ "fullImportPath": "crate::symbols::GalacticWidget" }],
                "version": 1,
                "hash": "completion-hash"
            }
        }))
        .unwrap()
    }

    fn pending(editor: &mut Editor, request_id: i64) {
        let snapshot = editor.activate_completion_snapshot(editor.completion_snapshot());
        editor.pending_completion_resolutions.insert(
            request_id,
            PendingCompletionResolution {
                item: completion(),
                commit_character: None,
                snapshot,
            },
        );
        // Selecting a completion closes its popup before the resolve response arrives.
        editor.completion_snapshot = None;
    }

    #[tokio::test]
    async fn resolved_completion_applies_the_import_and_symbol_as_one_undo_step() {
        let mut editor = editor("mod symbols;\nGal\n");
        pending(&mut editor, 17);
        let mut item = completion();
        item.additional_text_edits = Some(vec![TextEdit {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 0,
                },
            },
            new_text: "use crate::symbols::GalacticWidget;\n".to_string(),
        }]);
        let response = InboundMessage::Message(ResponseMessage {
            id: 17,
            result: serde_json::to_value(item).unwrap(),
            request: None,
        });

        let action = editor
            .handle_lsp_message(&response, Some("completionItem/resolve".to_string()))
            .expect("resolved completion should produce an editor action");
        editor.test_execute_production_action(action).await.unwrap();

        assert_eq!(
            editor.current_buffer().contents(),
            "use crate::symbols::GalacticWidget;\nmod symbols;\nGalacticWidget\n"
        );
        editor
            .test_execute_production_action(Action::Undo)
            .await
            .unwrap();
        assert_eq!(editor.current_buffer().contents(), "mod symbols;\nGal\n");
    }

    #[tokio::test]
    async fn resolved_import_after_passthrough_typing_uses_current_server_ranges() {
        let mut editor = editor("mod symbols;\nGala\n");
        editor.cx = 4;
        let revision = editor.current_buffer().revision();
        let snapshot = CompletionSnapshot {
            buffer_id: editor.current_buffer().id(),
            initial_revision: revision.wrapping_sub(1),
            revision,
            uri: editor.current_buffer().uri().unwrap(),
            cursor: Some(editor.cursor_text_position()),
            original_range: Some(Range {
                start: Position {
                    line: 1,
                    character: 0,
                },
                end: Position {
                    line: 1,
                    character: 3,
                },
            }),
            current_range: Some(Range {
                start: Position {
                    line: 1,
                    character: 0,
                },
                end: Position {
                    line: 1,
                    character: 4,
                },
            }),
        };
        editor.pending_completion_refreshes.insert(
            18,
            PendingCompletionRefresh {
                item: deferred_completion(3),
                commit_character: None,
                snapshot,
            },
        );
        let mut resolved = deferred_completion(4);
        resolved.additional_text_edits = Some(vec![TextEdit {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 0,
                },
            },
            new_text: "use crate::symbols::GalacticWidget;\n".to_string(),
        }]);
        let response = InboundMessage::Message(ResponseMessage {
            id: 18,
            result: serde_json::to_value(CompletionResponse::Items(vec![resolved])).unwrap(),
            request: None,
        });

        let action = editor
            .handle_lsp_message(&response, Some("textDocument/completion".to_string()))
            .expect("refreshed completion should produce an editor action");
        editor.test_execute_production_action(action).await.unwrap();

        assert_eq!(
            editor.current_buffer().contents(),
            "use crate::symbols::GalacticWidget;\nmod symbols;\nGalacticWidget\n"
        );
    }

    #[test]
    fn resolved_completion_is_ignored_after_the_cursor_moves() {
        let mut editor = editor("mod symbols;\nGal\n");
        pending(&mut editor, 19);
        editor.cx = 2;
        let response = InboundMessage::Message(ResponseMessage {
            id: 19,
            result: serde_json::to_value(completion()).unwrap(),
            request: None,
        });

        assert!(editor
            .handle_lsp_message(&response, Some("completionItem/resolve".to_string()))
            .is_none());
        assert!(editor.pending_completion_resolutions.is_empty());
        assert_eq!(editor.current_buffer().contents(), "mod symbols;\nGal\n");
    }

    #[tokio::test]
    async fn resolve_failures_still_insert_the_original_completion() {
        for failure in ["malformed", "server", "timeout"] {
            let mut editor = editor("mod symbols;\nGal\n");
            pending(&mut editor, 23);
            let response = match failure {
                "malformed" => InboundMessage::Message(ResponseMessage {
                    id: 23,
                    result: json!({ "not": "a completion item" }),
                    request: None,
                }),
                "server" => InboundMessage::Error(ResponseError {
                    id: Some(23),
                    code: -32603,
                    message: "resolve failed".to_string(),
                    data: None,
                    request: None,
                }),
                _ => InboundMessage::RequestError {
                    id: 23,
                    error: LspError::RequestTimeout(Duration::from_secs(1)),
                },
            };

            let action = editor
                .handle_lsp_message(&response, Some("completionItem/resolve".to_string()))
                .expect("a failed resolve should fall back to the selected completion");
            editor.test_execute_production_action(action).await.unwrap();

            assert_eq!(
                editor.current_buffer().contents(),
                "mod symbols;\nGalacticWidget\n"
            );
            assert!(editor.pending_completion_resolutions.is_empty());
        }
    }

    #[tokio::test]
    async fn real_rust_analyzer_completion_resolves_and_imports_an_out_of_scope_type() {
        if std::env::var_os("RED_RUN_REAL_LSP_TESTS").is_none() {
            return;
        }

        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("src");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"red-completion-import-repro\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(source.join("symbols.rs"), "pub struct GalacticWidget;\n").unwrap();
        let path = source.join("main.rs");
        let contents = "mod symbols;\n\nfn main() {\n    Gala\n}\n";
        std::fs::write(&path, contents).unwrap();

        let mut config = Config::default();
        config.formatting.on_save = false;
        config
            .lsp
            .servers
            .get_mut("rust")
            .and_then(|server| server.initialization_options.as_mut())
            .expect("default rust-analyzer initialization options")["checkOnSave"] = json!(false);
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            lsp,
            80,
            12,
            config,
            Theme::default(),
            vec![Buffer::new(
                Some(path.to_string_lossy().into_owned()),
                contents.to_string(),
            )],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        editor.mode = Mode::Insert;
        editor.cy = 3;
        editor.cx = 8;
        editor.ensure_buffer_lsp_opened(0).await.unwrap();
        editor.request_completion(None).await.unwrap();

        let item = tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                if let Some((incoming, method)) = editor.lsp.recv_response().await.unwrap() {
                    let candidate = match (&incoming, method.as_deref()) {
                        (InboundMessage::Message(response), Some("textDocument/completion")) => {
                            serde_json::from_value::<CompletionResponse>(response.result.clone())
                                .ok()
                                .and_then(|response| {
                                    response
                                        .items()
                                        .into_iter()
                                        .find(|item| item.label == "GalacticWidget")
                                })
                        }
                        _ => None,
                    };
                    let completion_response = method.as_deref() == Some("textDocument/completion");
                    if let Some(action) = editor.handle_lsp_message(&incoming, method) {
                        editor.test_execute_production_action(action).await.unwrap();
                    }
                    if let Some(candidate) = candidate {
                        break candidate;
                    }
                    if completion_response {
                        editor.request_completion(None).await.unwrap();
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("rust-analyzer should offer the out-of-scope type as a completion");

        assert!(
            item.data.is_some(),
            "auto-import completion must be resolvable"
        );
        assert!(
            item.additional_text_edits
                .as_ref()
                .is_none_or(Vec::is_empty),
            "rust-analyzer should defer import edits until selection"
        );
        assert!(
            item.data
                .as_ref()
                .and_then(|data| data.get("imports"))
                .and_then(serde_json::Value::as_array)
                .is_some_and(|imports| !imports.is_empty()),
            "rust-analyzer should identify the deferred import in completion data"
        );
        editor
            .test_execute_production_action(Action::ApplyCompletion {
                item: Box::new(item),
                commit_character: None,
            })
            .await
            .unwrap();
        assert_eq!(editor.current_buffer().contents(), contents);
        assert_eq!(editor.pending_completion_resolutions.len(), 1);
        editor
            .test_execute_production_action(Action::CloseDialog)
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                if let Some((incoming, method)) = editor.lsp.recv_response().await.unwrap() {
                    let resolved = method.as_deref() == Some("completionItem/resolve");
                    if let Some(action) = editor.handle_lsp_message(&incoming, method) {
                        editor.test_execute_production_action(action).await.unwrap();
                    }
                    if resolved {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("rust-analyzer should resolve the selected completion");

        let imported = editor.current_buffer().contents();
        assert!(
            imported.contains("symbols::GalacticWidget;"),
            "expected an automatic import, got:\n{imported}"
        );
        assert!(
            imported.contains("    GalacticWidget\n"),
            "expected the selected type to replace the prefix, got:\n{imported}"
        );
        editor.lsp.did_close(path.to_str().unwrap()).await.unwrap();
        editor.lsp.shutdown().await.unwrap();
    }
}
