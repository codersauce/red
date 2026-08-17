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
