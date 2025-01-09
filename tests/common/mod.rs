use red::lsp::{Diagnostic, InboundMessage, LspClient, LspError, Range};
use serde_json::Value;

#[derive(Default)]
pub struct MockLsp;

// Safe to implement Send + Sync since MockLsp is just a unit struct
unsafe impl Send for MockLsp {}
unsafe impl Sync for MockLsp {}

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

    async fn recv_response(
        &mut self,
    ) -> Result<Option<(InboundMessage, Option<String>)>, LspError> {
        Ok(None)
    }
}

pub fn mock_lsp() -> Box<dyn LspClient + Send> {
    Box::new(MockLsp)
}
