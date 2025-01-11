use std::{
    fmt::{self, Display, Formatter},
    sync::atomic::AtomicUsize,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use self::types::*;
pub use capabilities::get_client_capabilities;
pub use client::{start_lsp, RealLspClient};

pub mod capabilities;
pub mod client;
pub mod types;

#[derive(Debug)]
pub enum LspError {
    RequestTimeout(Duration),
    ServerError(String),
    ProtocolError(String),
    IoError(std::io::Error),
    JsonError(serde_json::Error),
    ChannelError(tokio::sync::mpsc::error::SendError<OutboundMessage>),
    ChannelInboundError(String),
    ParseError(String),
    NotInitialized,
}

impl std::error::Error for LspError {}

impl Display for LspError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            LspError::RequestTimeout(duration) => {
                write!(f, "LSP request timed out after {:?}", duration)
            }
            LspError::ServerError(msg) => write!(f, "LSP server error: {}", msg),
            LspError::ProtocolError(msg) => write!(f, "LSP protocol error: {}", msg),
            LspError::IoError(err) => write!(f, "IO error: {}", err),
            LspError::JsonError(err) => write!(f, "JSON error: {}", err),
            LspError::ChannelError(err) => write!(f, "Channel error: {}", err),
            LspError::ChannelInboundError(err) => write!(f, "Channel inbound error: {}", err),
            LspError::NotInitialized => write!(f, "LSP client not initialized yet"),
            LspError::ParseError(msg) => write!(f, "Parse error: {}", msg),
        }
    }
}

impl From<std::io::Error> for LspError {
    fn from(err: std::io::Error) -> Self {
        LspError::IoError(err)
    }
}

impl From<serde_json::Error> for LspError {
    fn from(err: serde_json::Error) -> Self {
        LspError::JsonError(err)
    }
}

impl From<tokio::sync::mpsc::error::SendError<OutboundMessage>> for LspError {
    fn from(err: tokio::sync::mpsc::error::SendError<OutboundMessage>) -> Self {
        LspError::ChannelError(err)
    }
}

#[derive(Debug)]
pub struct NotificationRequest {
    method: String,
    params: Value,
}

impl Display for NotificationRequest {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let truncated_params = if self.params.to_string().len() > 100 {
            format!("{}...", &self.params.to_string()[..100])
        } else {
            self.params.to_string()
        };

        write!(
            f,
            "Request {{ method: {}, params: {} }}",
            self.method, truncated_params
        )
    }
}

#[derive(Debug)]
pub struct Request {
    id: i64,
    method: String,
    params: Value,
    timestamp: Instant,
}

static ID: AtomicUsize = AtomicUsize::new(1);

impl Request {
    pub fn new(method: &str, params: Value) -> Request {
        Request {
            id: next_id() as i64,
            method: method.to_string(),
            params,
            timestamp: Instant::now(),
        }
    }
}

impl Display for Request {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let truncated_params = if self.params.to_string().len() > 100 {
            format!("{}...", &self.params.to_string()[..100])
        } else {
            self.params.to_string()
        };

        write!(
            f,
            "Request {{ id: {}, method: {}, params: {} }}",
            self.id, self.method, truncated_params
        )
    }
}

#[derive(Debug, Clone)]
pub struct ResponseMessage {
    pub id: i64,
    pub result: Value,
}

#[derive(Debug)]
#[allow(unused)]
pub struct Notification {
    method: String,
    params: Value,
}

#[derive(Debug)]
#[allow(unused)]
pub struct ResponseError {
    code: i64,
    message: String,
    data: Option<Value>,
}

#[derive(Debug)]
pub enum OutboundMessage {
    Request(Request),
    Notification(NotificationRequest),
}

impl Display for OutboundMessage {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            OutboundMessage::Request(req) => write!(f, "Request({})", req),
            OutboundMessage::Notification(req) => write!(f, "Notification({})", req),
        }
    }
}

#[derive(Debug)]
pub enum InboundMessage {
    Message(ResponseMessage),
    Notification(ParsedNotification),
    UnknownNotification(Notification),
    Error(ResponseError),
    ProcessingError(LspError),
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(untagged)]
pub enum ParsedNotification {
    PublishDiagnostics(TextDocumentPublishDiagnostics),
    Progress(ProgressParams),
}

fn parse_notification(
    method: &str,
    params: &Value,
) -> Result<Option<ParsedNotification>, LspError> {
    if method == "textDocument/publishDiagnostics" {
        return Ok(Some(serde_json::from_value(params.clone())?));
    }
    if method == "$/progress" {
        return Ok(Some(serde_json::from_value(params.clone())?));
    }

    Ok(None)
}

pub fn next_id() -> usize {
    ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

#[async_trait::async_trait]
pub trait LspClient: std::any::Any + Send {
    async fn initialize(&mut self) -> Result<(), LspError>;
    async fn did_open(&mut self, file: &str, contents: &str) -> Result<(), LspError>;
    async fn did_change(&mut self, file: &str, contents: &str) -> Result<(), LspError>;
    async fn will_save(&mut self, file: &str) -> Result<(), LspError>;
    async fn hover(&mut self, file: &str, x: usize, y: usize) -> Result<i64, LspError>;
    async fn goto_definition(&mut self, file: &str, x: usize, y: usize) -> Result<i64, LspError>;
    async fn completion(&mut self, file: &str, x: usize, y: usize) -> Result<i64, LspError>;
    async fn format_document(&mut self, file: &str) -> Result<i64, LspError>;
    async fn document_symbols(&mut self, file: &str) -> Result<i64, LspError>;
    async fn code_action(
        &mut self,
        file: &str,
        range: Range,
        diagnostics: Vec<Diagnostic>,
    ) -> Result<i64, LspError>;
    async fn signature_help(&mut self, file: &str, x: usize, y: usize) -> Result<i64, LspError>;
    async fn document_highlight(&mut self, file: &str, x: usize, y: usize)
        -> Result<i64, LspError>;
    async fn document_link(&mut self, file: &str) -> Result<i64, LspError>;
    async fn document_color(&mut self, file: &str) -> Result<i64, LspError>;
    async fn folding_range(&mut self, file: &str) -> Result<i64, LspError>;
    async fn workspace_symbol(&mut self, query: &str) -> Result<i64, LspError>;
    async fn call_hierarchy_prepare(
        &mut self,
        file: &str,
        x: usize,
        y: usize,
    ) -> Result<i64, LspError>;
    async fn semantic_tokens_full(&mut self, file: &str) -> Result<i64, LspError>;
    async fn inlay_hint(&mut self, file: &str, range: Range) -> Result<i64, LspError>;
    async fn send_request(
        &mut self,
        method: &str,
        params: Value,
        force: bool,
    ) -> Result<i64, LspError>;
    async fn send_notification(
        &mut self,
        method: &str,
        params: Value,
        force: bool,
    ) -> Result<(), LspError>;
    async fn request_completion(
        &mut self,
        file_uri: &str,
        line: usize,
        character: usize,
    ) -> Result<i64, LspError>;
    async fn request_diagnostics(&mut self, file_uri: &str) -> Result<i64, LspError>;
    async fn recv_response(&mut self)
        -> Result<Option<(InboundMessage, Option<String>)>, LspError>;
    fn get_server_capabilities(&self) -> Option<&ServerCapabilities>;
    async fn shutdown(&mut self) -> Result<(), LspError>;
}
