use red::lsp::{InboundMessage, LspClient};
use serde_json::Value;

pub struct MockLsp;

#[async_trait::async_trait]
impl LspClient for MockLsp {
    async fn initialize(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn did_open(&mut self, _file: &str, _contents: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn did_change(&mut self, _file: &str, _contents: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn hover(&mut self, _file: &str, _x: usize, _y: usize) -> anyhow::Result<i64> {
        Ok(0)
    }

    async fn goto_definition(&mut self, _file: &str, _x: usize, _y: usize) -> anyhow::Result<i64> {
        Ok(0)
    }

    async fn send_request(&mut self, _method: &str, _params: Value) -> anyhow::Result<i64> {
        Ok(0)
    }

    async fn send_notification(&mut self, _method: &str, _params: Value) -> anyhow::Result<()> {
        Ok(())
    }

    async fn recv_response(&mut self) -> anyhow::Result<Option<(InboundMessage, Option<String>)>> {
        Ok(None)
    }
}

pub fn mock_lsp() -> Box<dyn LspClient> {
    Box::new(MockLsp)
}
