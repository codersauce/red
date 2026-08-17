//! Temporarily replace language services without disturbing the user's clients.

use super::*;
use crate::{config::LspConfig, lsp::LspManager};

/// The suspended client keeps its processes and outstanding requests alive.
/// A lesson gets separate diagnostics, request maps, and completion state.
pub(super) struct SavedLanguageState {
    client: Box<dyn LspClient>,
    coordinator: lsp_coordinator::LspCoordinator,
    config: LspConfig,
    disable_ai: bool,
    show_diagnostics: bool,
    diagnostics: HashMap<String, Vec<Diagnostic>>,
    document_symbols: HashMap<i64, PendingDocumentSymbols>,
    workspace_symbols: HashMap<i64, RequestId>,
    references: HashMap<i64, RequestId>,
    inlay_hints: HashMap<i64, RequestId>,
    edit_requests: HashMap<i64, PendingLspEdit>,
    format_saves: HashMap<i64, PendingLspFormatSave>,
    revision_snapshots: HashMap<i64, Vec<(String, u64)>>,
    completions: HashMap<i64, PendingCompletion>,
    scheduled_completion: Option<ScheduledCompletion>,
    inline_completion: inline_completion::InlineCompletionState,
    completion_snapshot: Option<CompletionSnapshot>,
}

pub(super) fn practice_language_services(
    lesson: Lesson,
    workspace: Option<&PracticeWorkspace>,
) -> anyhow::Result<(LspConfig, Box<dyn LspClient>)> {
    let mut config = LspConfig {
        enabled: lesson.is_lsp_practice(),
        format_on_save: false,
        servers: HashMap::new(),
    };
    let client: Box<dyn LspClient> = if lesson.is_lsp_practice() {
        let workspace =
            workspace.ok_or_else(|| anyhow::anyhow!("LSP practice workspace is missing"))?;
        let mut husk = crate::config::default_language_servers()
            .remove("husk")
            .ok_or_else(|| anyhow::anyhow!("bundled Husk language server is missing"))?;
        // A loose, native Husk file avoids package/dependency diagnostics and
        // never reads the user's language-server configuration.
        husk.initialization_options = Some(json!({"semanticProfile": "native"}));
        config.servers.insert("husk".into(), husk);
        Box::new(LspManager::for_workspace(config.clone(), workspace.root())?)
    } else {
        Box::new(LspManager::new(config.clone()))
    };
    Ok((config, client))
}

impl SavedLanguageState {
    pub(super) fn original_config(&self) -> &LspConfig {
        &self.config
    }

    pub fn install(editor: &mut Editor, config: LspConfig, client: Box<dyn LspClient>) -> Self {
        let show_diagnostics = config.enabled;
        let saved = Self {
            client: std::mem::replace(&mut editor.lsp, client),
            coordinator: std::mem::take(&mut editor.lsp_coordinator),
            config: std::mem::replace(&mut editor.config.lsp, config),
            disable_ai: std::mem::replace(&mut editor.config.disable_ai, true),
            show_diagnostics: std::mem::replace(
                &mut editor.config.show_diagnostics,
                show_diagnostics,
            ),
            diagnostics: std::mem::take(&mut editor.diagnostics),
            document_symbols: std::mem::take(&mut editor.pending_plugin_document_symbols),
            workspace_symbols: std::mem::take(&mut editor.pending_plugin_workspace_symbols),
            references: std::mem::take(&mut editor.pending_plugin_references),
            inlay_hints: std::mem::take(&mut editor.pending_plugin_inlay_hints),
            edit_requests: std::mem::take(&mut editor.pending_lsp_edit_requests),
            format_saves: std::mem::take(&mut editor.pending_lsp_format_saves),
            revision_snapshots: std::mem::take(&mut editor.pending_lsp_revision_snapshots),
            completions: std::mem::take(&mut editor.pending_completions),
            scheduled_completion: editor.scheduled_completion.take(),
            inline_completion: std::mem::take(&mut editor.inline_completion),
            completion_snapshot: editor.completion_snapshot.take(),
        };
        editor.sync_diagnostic_gutter_signs();
        saved
    }

    pub async fn restore(self, editor: &mut Editor) {
        if let Err(error) = editor.lsp.shutdown().await {
            log!("could not shut down tutorial language services: {error}");
        }
        editor.lsp = self.client;
        editor.lsp_coordinator = self.coordinator;
        editor.config.lsp = self.config;
        editor.config.disable_ai = self.disable_ai;
        editor.config.show_diagnostics = self.show_diagnostics;
        editor.diagnostics = self.diagnostics;
        editor.pending_plugin_document_symbols = self.document_symbols;
        editor.pending_plugin_workspace_symbols = self.workspace_symbols;
        editor.pending_plugin_references = self.references;
        editor.pending_plugin_inlay_hints = self.inlay_hints;
        editor.pending_lsp_edit_requests = self.edit_requests;
        editor.pending_lsp_format_saves = self.format_saves;
        editor.pending_lsp_revision_snapshots = self.revision_snapshots;
        editor.pending_completions = self.completions;
        editor.scheduled_completion = self.scheduled_completion;
        editor.inline_completion = self.inline_completion;
        editor.completion_snapshot = self.completion_snapshot;
        editor.sync_diagnostic_gutter_signs();
    }
}

impl Editor {
    /// Only the expected, revision-checked edit in the current owned file may
    /// reach the production workspace-edit transaction. Commands and resource
    /// operations are never part of this exercise.
    pub(super) fn learn_repair_edit_allowed(&self, action: &Action) -> bool {
        let Action::ApplyLspWorkspaceEdit {
            documents,
            expected_revisions,
            command,
            ..
        } = action
        else {
            return true;
        };
        let Some(session) = self.learn_session.as_ref() else {
            return false;
        };
        let [document] = documents.as_slice() else {
            return false;
        };
        let Some(uri) = self.current_buffer().uri().ok().flatten() else {
            return false;
        };
        session.lesson == Lesson::RepairTheCode
            && command.is_none()
            && document.uri == uri
            && expected_revisions.as_slice() == [(uri, self.current_buffer().revision())]
            && session.workspace.as_ref().is_some_and(|workspace| {
                lsp_file_path(&document.uri)
                    .is_ok_and(|file| workspace.permits_file(Path::new(&file)))
            })
            && self.current_buffer().contents() == crate::learn::HUSK_CONTENTS
            && crate::lsp::apply_text_edits(crate::learn::HUSK_CONTENTS, &document.edits)
                .is_ok_and(|contents| contents == crate::learn::HUSK_FIXED_CONTENTS)
    }

    pub(super) fn learn_diagnostic_present(&self) -> bool {
        self.current_buffer()
            .uri()
            .ok()
            .flatten()
            .and_then(|uri| self.diagnostics.get(&uri))
            .is_some_and(|diagnostics| {
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message == crate::learn::HUSK_DIAGNOSTIC)
            })
    }

    pub(super) fn learn_diagnostic_under_cursor(&self) -> bool {
        self.diagnostics_for_current_line()
            .is_some_and(|diagnostics| {
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message == crate::learn::HUSK_DIAGNOSTIC)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::Position;

    #[tokio::test]
    async fn learn_repair_uses_an_unsaved_undoable_confined_lsp_edit() {
        let config = Config::default();
        let client = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            client,
            140,
            38,
            config,
            Theme::default(),
            vec![Buffer::new(None, "original".into())],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        let mut buffer = RenderBuffer::new(140, 38, &Style::default());
        let mut runtime = Runtime::new();
        // Use a disabled test client; the retained tmux run covers real Husk.
        editor
            .start_learn_lesson(Lesson::SaveAPracticeFile, &mut buffer, &mut runtime)
            .await
            .unwrap();
        let workspace = editor
            .learn_session
            .as_ref()
            .unwrap()
            .workspace
            .as_ref()
            .unwrap();
        workspace
            .write_fixture("main.hk", crate::learn::HUSK_CONTENTS)
            .unwrap();
        let path = workspace.path("main.hk");
        let practice = Buffer::new(
            Some(path.to_string_lossy().into_owned()),
            crate::learn::HUSK_CONTENTS.into(),
        );
        let id = practice.id();
        *editor.current_buffer_mut() = practice;
        let session = editor.learn_session.as_mut().unwrap();
        session.practice_buffer_id = id;
        session.lesson = Lesson::RepairTheCode;
        session.step = PracticeStep::RepairLocate;
        editor
            .execute(&Action::OpenDiagnosticsPicker, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert!(editor.current_dialog.is_none());
        assert_eq!(
            editor.learn_session.as_ref().unwrap().step,
            PracticeStep::RepairLocate
        );
        editor.learn_session.as_mut().unwrap().step = PracticeStep::RepairApply;
        let uri = editor.current_buffer().uri().unwrap().unwrap();
        let revision = editor.current_buffer().revision();
        let edit = LspDocumentEdit {
            uri: uri.clone(),
            version: None,
            edits: vec![crate::lsp::TextEdit {
                range: Range {
                    start: Position {
                        line: 5,
                        character: 32,
                    },
                    end: Position {
                        line: 5,
                        character: 32,
                    },
                },
                new_text: ";".into(),
            }],
        };
        let action = |documents, expected_revisions, command| Action::ApplyLspWorkspaceEdit {
            documents,
            expected_revisions,
            command,
            label: "Insert missing semicolon".into(),
        };
        let valid = action(vec![edit.clone()], vec![(uri.clone(), revision)], None);
        assert!(editor.learn_repair_edit_allowed(&valid));
        assert!(!editor.learn_repair_edit_allowed(&action(vec![edit.clone()], vec![], None)));
        assert!(!editor.learn_repair_edit_allowed(&action(
            vec![edit.clone()],
            vec![(uri.clone(), revision + 1)],
            None
        )));
        assert!(!editor.learn_repair_edit_allowed(&action(
            vec![edit.clone()],
            vec![(uri.clone(), revision)],
            Some(Box::new(LspCommand {
                title: "outside command".into(),
                command: "unsafe".into(),
                arguments: None,
            }))
        )));
        let outside = tempfile::NamedTempFile::new().unwrap();
        let mut outside_edit = edit.clone();
        outside_edit.uri = crate::lsp::file_uri(outside.path()).unwrap();
        assert!(!editor.learn_repair_edit_allowed(&action(
            vec![outside_edit],
            vec![(uri.clone(), revision)],
            None
        )));
        let mut wrong = edit.clone();
        wrong.edits[0].new_text = "!".into();
        assert!(!editor.learn_repair_edit_allowed(&action(
            vec![wrong],
            vec![(uri.clone(), revision)],
            None
        )));
        assert!(!practice_action_allowed(
            Lesson::RepairTheCode,
            &Action::ApplyLspWorkspaceEditOperations {
                operations: vec![],
                expected_revisions: vec![],
                command: None,
                label: "resource edit".into(),
                response: None,
                save_after_uri: None,
                save_as: None,
                save_previous_file: None,
            }
        ));
        editor
            .execute(&valid, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(
            editor.current_buffer().contents(),
            crate::learn::HUSK_FIXED_CONTENTS
        );
        assert!(editor.current_buffer().is_dirty());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            crate::learn::HUSK_CONTENTS
        );
        assert_eq!(
            editor.learn_session.as_ref().unwrap().step,
            PracticeStep::RepairSave
        );
        assert!(matches!(
            editor
                .current_buffer()
                .undo_history
                .latest_transaction()
                .map(|tx| &tx.origin),
            Some(EditOrigin::Lsp { .. })
        ));
        editor
            .execute(&Action::Undo, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(
            editor.current_buffer().contents(),
            crate::learn::HUSK_CONTENTS
        );
        editor
            .execute(&Action::Redo, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(
            editor.current_buffer().contents(),
            crate::learn::HUSK_FIXED_CONTENTS
        );
        editor
            .execute(&Action::Save, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(
            editor.learn_session.as_ref().unwrap().step,
            PracticeStep::RepairSave
        );
        let refresh = editor.add_diagnostics(Some(&uri), &[]).unwrap();
        editor
            .execute(&refresh, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(
            editor.learn_session.as_ref().unwrap().step,
            PracticeStep::Complete
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            crate::learn::HUSK_FIXED_CONTENTS
        );
        editor
            .finish_learn_lesson(&mut buffer, &mut runtime)
            .await
            .unwrap();
        assert!(!path.exists());
        assert_eq!(editor.current_buffer().contents(), "original");
    }
}
