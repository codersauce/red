use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use red::lsp::{
    Diagnostic, InboundMessage, LspClient, LspError, Range, ServerCapabilities, ServerRequest,
};
use serde_json::Value;

#[derive(Default)]
pub struct MockLsp;

unsafe impl Send for MockLsp {}
unsafe impl Sync for MockLsp {}

#[derive(Debug, Clone, PartialEq)]
pub enum LspEvent {
    DidOpen(String),
    DidChange(String),
    DidClose(String),
    RequestDiagnostics(String),
    Hover(String),
    GotoDefinition(String),
    FormatDocument(String),
    CodeAction {
        file: String,
        range: Range,
        diagnostic_count: usize,
    },
    SignatureHelp {
        file: String,
        x: usize,
        y: usize,
    },
    Rename {
        file: String,
        x: usize,
        y: usize,
        new_name: String,
    },
    DocumentSymbols(String),
    WorkspaceSymbols(String),
    References {
        file: String,
        x: usize,
        y: usize,
        include_declaration: bool,
    },
    RequestCompletion {
        uri: String,
        line: usize,
        character: usize,
        trigger_character: Option<char>,
    },
    SendRequest {
        method: String,
        params: Value,
    },
    WorkspaceEditResponse {
        id: Value,
        applied: bool,
        failure_reason: Option<String>,
    },
}

#[derive(Clone, Default)]
pub struct RecordingLsp {
    events: Arc<Mutex<Vec<LspEvent>>>,
    workspace_root: Option<PathBuf>,
    fail_next_did_open: bool,
    fail_next_did_change: bool,
}

impl RecordingLsp {
    pub fn with_workspace_root(root: &Path) -> Self {
        Self {
            events: Arc::default(),
            workspace_root: Some(root.to_path_buf()),
            fail_next_did_open: false,
            fail_next_did_change: false,
        }
    }
    pub fn events(&self) -> Arc<Mutex<Vec<LspEvent>>> {
        Arc::clone(&self.events)
    }

    pub fn failing_next_did_open() -> Self {
        Self {
            fail_next_did_open: true,
            ..Self::default()
        }
    }

    pub fn failing_next_did_change() -> Self {
        Self {
            fail_next_did_change: true,
            ..Self::default()
        }
    }

    fn record(&self, event: LspEvent) {
        self.events.lock().unwrap().push(event);
    }
}

#[async_trait::async_trait]
impl LspClient for MockLsp {
    async fn initialize(&mut self) -> Result<(), LspError> {
        Ok(())
    }

    async fn did_open(&mut self, _file: &str, _contents: &str) -> Result<(), LspError> {
        Ok(())
    }

    async fn did_change(&mut self, _file: &str, _contents: String) -> Result<(), LspError> {
        Ok(())
    }

    async fn hover(&mut self, _file: &str, _x: usize, _y: usize) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn goto_definition(
        &mut self,
        _file: &str,
        _x: usize,
        _y: usize,
    ) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn completion(&mut self, _file: &str, _x: usize, _y: usize) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn format_document(&mut self, _file: &str) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn document_symbols(&mut self, _file: &str) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn code_action(
        &mut self,
        _file: &str,
        _range: Range,
        _diagnostics: Vec<Diagnostic>,
    ) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn signature_help(&mut self, _file: &str, _x: usize, _y: usize) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn rename(
        &mut self,
        _file: &str,
        _x: usize,
        _y: usize,
        _new_name: &str,
    ) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn document_highlight(
        &mut self,
        _file: &str,
        _x: usize,
        _y: usize,
    ) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn document_link(&mut self, _file: &str) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn document_color(&mut self, _file: &str) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn folding_range(&mut self, _file: &str) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn workspace_symbol(&mut self, _query: &str) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn references(
        &mut self,
        _file: &str,
        _x: usize,
        _y: usize,
        _include_declaration: bool,
    ) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn call_hierarchy_prepare(
        &mut self,
        _file: &str,
        _x: usize,
        _y: usize,
    ) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn semantic_tokens_full(&mut self, _file: &str) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn inlay_hint(&mut self, _file: &str, _range: Range) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn send_request(
        &mut self,
        _method: &str,
        _params: Value,
        _: bool,
    ) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn send_notification(
        &mut self,
        _method: &str,
        _params: Value,
        _: bool,
    ) -> Result<(), LspError> {
        Ok(())
    }

    async fn request_completion(
        &mut self,
        _file_uri: &str,
        _line: usize,
        _character: usize,
        _trigger_character: Option<char>,
    ) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn recv_response(
        &mut self,
    ) -> Result<Option<(InboundMessage, Option<String>)>, LspError> {
        Ok(None)
    }

    fn get_server_capabilities(&self) -> Option<&ServerCapabilities> {
        None
    }

    async fn request_diagnostics(&mut self, _file: &str) -> Result<Option<i64>, LspError> {
        Ok(None)
    }

    async fn will_save(&mut self, _file: &str) -> Result<(), LspError> {
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), LspError> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl LspClient for RecordingLsp {
    async fn initialize(&mut self) -> Result<(), LspError> {
        Ok(())
    }

    async fn did_open(&mut self, file: &str, _contents: &str) -> Result<(), LspError> {
        self.record(LspEvent::DidOpen(file.to_string()));
        if self.fail_next_did_open {
            self.fail_next_did_open = false;
            return Err(LspError::ServerError(
                "injected didOpen failure".to_string(),
            ));
        }
        Ok(())
    }

    async fn did_change(&mut self, file: &str, _contents: String) -> Result<(), LspError> {
        self.record(LspEvent::DidChange(file.to_string()));
        if self.fail_next_did_change {
            self.fail_next_did_change = false;
            return Err(LspError::ServerError(
                "injected didChange failure".to_string(),
            ));
        }
        Ok(())
    }

    async fn did_close(&mut self, file: &str) -> Result<(), LspError> {
        self.record(LspEvent::DidClose(file.to_string()));
        Ok(())
    }

    async fn hover(&mut self, file: &str, _x: usize, _y: usize) -> Result<i64, LspError> {
        self.record(LspEvent::Hover(file.to_string()));
        Ok(0)
    }

    async fn goto_definition(&mut self, file: &str, _x: usize, _y: usize) -> Result<i64, LspError> {
        self.record(LspEvent::GotoDefinition(file.to_string()));
        Ok(0)
    }

    async fn completion(&mut self, _file: &str, _x: usize, _y: usize) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn format_document(&mut self, file: &str) -> Result<i64, LspError> {
        self.record(LspEvent::FormatDocument(file.to_string()));
        Ok(45)
    }

    async fn document_symbols(&mut self, file: &str) -> Result<i64, LspError> {
        self.record(LspEvent::DocumentSymbols(file.to_string()));
        Ok(42)
    }

    async fn code_action(
        &mut self,
        file: &str,
        range: Range,
        diagnostics: Vec<Diagnostic>,
    ) -> Result<i64, LspError> {
        self.record(LspEvent::CodeAction {
            file: file.to_string(),
            range,
            diagnostic_count: diagnostics.len(),
        });
        Ok(46)
    }

    async fn signature_help(&mut self, file: &str, x: usize, y: usize) -> Result<i64, LspError> {
        self.record(LspEvent::SignatureHelp {
            file: file.to_string(),
            x,
            y,
        });
        Ok(47)
    }

    async fn rename(
        &mut self,
        file: &str,
        x: usize,
        y: usize,
        new_name: &str,
    ) -> Result<i64, LspError> {
        self.record(LspEvent::Rename {
            file: file.to_string(),
            x,
            y,
            new_name: new_name.to_string(),
        });
        Ok(48)
    }

    async fn document_highlight(
        &mut self,
        _file: &str,
        _x: usize,
        _y: usize,
    ) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn document_link(&mut self, _file: &str) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn document_color(&mut self, _file: &str) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn folding_range(&mut self, _file: &str) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn workspace_symbol(&mut self, query: &str) -> Result<i64, LspError> {
        self.record(LspEvent::WorkspaceSymbols(query.to_string()));
        Ok(43)
    }

    async fn references(
        &mut self,
        file: &str,
        x: usize,
        y: usize,
        include_declaration: bool,
    ) -> Result<i64, LspError> {
        self.record(LspEvent::References {
            file: file.to_string(),
            x,
            y,
            include_declaration,
        });
        Ok(44)
    }

    async fn call_hierarchy_prepare(
        &mut self,
        _file: &str,
        _x: usize,
        _y: usize,
    ) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn semantic_tokens_full(&mut self, _file: &str) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn inlay_hint(&mut self, _file: &str, _range: Range) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn send_request(
        &mut self,
        method: &str,
        params: Value,
        _: bool,
    ) -> Result<i64, LspError> {
        self.record(LspEvent::SendRequest {
            method: method.to_string(),
            params,
        });
        Ok(0)
    }

    async fn send_notification(
        &mut self,
        _method: &str,
        _params: Value,
        _: bool,
    ) -> Result<(), LspError> {
        Ok(())
    }

    async fn request_completion(
        &mut self,
        file_uri: &str,
        line: usize,
        character: usize,
        trigger_character: Option<char>,
    ) -> Result<i64, LspError> {
        self.record(LspEvent::RequestCompletion {
            uri: file_uri.to_string(),
            line,
            character,
            trigger_character,
        });
        Ok(0)
    }

    async fn recv_response(
        &mut self,
    ) -> Result<Option<(InboundMessage, Option<String>)>, LspError> {
        Ok(None)
    }

    fn get_server_capabilities(&self) -> Option<&ServerCapabilities> {
        None
    }

    fn workspace_root_for_file(&self, _file: &str) -> Option<PathBuf> {
        self.workspace_root.clone()
    }

    fn workspace_root_for_request(&self, _request: &ServerRequest) -> Option<PathBuf> {
        self.workspace_root.clone()
    }

    async fn respond_workspace_edit(
        &mut self,
        request: &ServerRequest,
        applied: bool,
        failure_reason: Option<&str>,
    ) -> Result<(), LspError> {
        self.record(LspEvent::WorkspaceEditResponse {
            id: request.id.clone(),
            applied,
            failure_reason: failure_reason.map(ToString::to_string),
        });
        Ok(())
    }

    async fn request_diagnostics(&mut self, file: &str) -> Result<Option<i64>, LspError> {
        self.record(LspEvent::RequestDiagnostics(file.to_string()));
        Ok(None)
    }

    async fn will_save(&mut self, _file: &str) -> Result<(), LspError> {
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), LspError> {
        Ok(())
    }
}
