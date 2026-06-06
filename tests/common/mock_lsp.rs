use std::sync::{Arc, Mutex};

use red::lsp::{Diagnostic, InboundMessage, LspClient, LspError, Range, ServerCapabilities};
use serde_json::Value;

#[derive(Default)]
pub struct MockLsp;

unsafe impl Send for MockLsp {}
unsafe impl Sync for MockLsp {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspEvent {
    DidOpen(String),
    DidChange(String),
    RequestDiagnostics(String),
    Hover(String),
    GotoDefinition(String),
    DocumentSymbols(String),
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
}

#[derive(Clone, Default)]
pub struct RecordingLsp {
    events: Arc<Mutex<Vec<LspEvent>>>,
}

impl RecordingLsp {
    pub fn events(&self) -> Arc<Mutex<Vec<LspEvent>>> {
        Arc::clone(&self.events)
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

    async fn did_change(&mut self, _file: &str, _contents: &str) -> Result<(), LspError> {
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
        Ok(())
    }

    async fn did_change(&mut self, file: &str, _contents: &str) -> Result<(), LspError> {
        self.record(LspEvent::DidChange(file.to_string()));
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

    async fn format_document(&mut self, _file: &str) -> Result<i64, LspError> {
        Ok(0)
    }

    async fn document_symbols(&mut self, file: &str) -> Result<i64, LspError> {
        self.record(LspEvent::DocumentSymbols(file.to_string()));
        Ok(42)
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
