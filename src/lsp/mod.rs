//! Language Server Protocol process abstraction, routing, synchronization, and edit safety.
//!
//! [`LspManager`] selects or lazily starts a client for each document, while
//! [`RealLspClient`] owns one server process, JSON-RPC framing, initialization state,
//! request correlation, pending messages, diagnostics debounce, and shutdown. The editor
//! polls [`InboundMessage`] values and remains the authority that applies resulting
//! state.
//!
//! Editor cursor positions are grapheme indices and buffer edits use Unicode scalar
//! indices; LSP [`Position`] values use UTF-16 code units. Conversion belongs in
//! [`edit`] and [`workspace_edit`], including split-surrogate rejection, revision
//! validation, workspace confinement, and rollback of ordered resource operations.

use std::{
    fmt::{self, Display, Formatter},
    path::PathBuf,
    sync::atomic::AtomicUsize,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use self::types::*;
pub use capabilities::get_client_capabilities;
pub use client::RealLspClient;
pub use edit::{
    apply_text_edits, file_path, file_uri, text_edit_char_range, workspace_edit_operations,
    workspace_edits, DocumentEdit, WorkspaceEditOperation,
};
pub use manager::{DocumentInfo, LspManager};
pub use workspace_edit::{
    apply_workspace_resource_operations, normalized_file_path, prepare_workspace_edit,
    OpenWorkspaceDocument, PreparedWorkspaceDocument, PreparedWorkspaceEdit,
    MAX_WORKSPACE_EDIT_TOTAL_BYTES,
};

pub mod capabilities;
pub mod client;
pub mod edit;
pub mod manager;
pub mod types;
pub mod workspace_edit;

#[derive(Debug)]
/// Failure returned by the LSP transport, protocol, or lifecycle boundary.
pub enum LspError {
    /// A request exceeded its configured deadline.
    RequestTimeout(Duration),
    /// The server returned a JSON-RPC error response.
    ServerError(String),
    /// A message violated Red's expected protocol shape or lifecycle.
    ProtocolError(String),
    /// Process or pipe I/O failed.
    IoError(std::io::Error),
    /// JSON serialization or deserialization failed.
    JsonError(serde_json::Error),
    /// The outbound writer task is no longer accepting messages.
    ChannelError(Box<tokio::sync::mpsc::error::SendError<OutboundMessage>>),
    /// The inbound reader task could not deliver a message.
    ChannelInboundError(String),
    /// A server payload could not be interpreted as the requested result.
    ParseError(String),
    /// An operation was attempted before the initialize handshake completed.
    NotInitialized,
}

impl std::error::Error for LspError {}

#[cfg(unix)]
impl From<nix::errno::Errno> for LspError {
    fn from(error: nix::errno::Errno) -> Self {
        Self::IoError(std::io::Error::from_raw_os_error(error as i32))
    }
}

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
        LspError::ChannelError(Box::new(err))
    }
}

#[derive(Debug)]
/// JSON-RPC notification queued for the server writer.
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

#[derive(Debug, Clone)]
/// Correlated JSON-RPC request and its local timing metadata.
pub struct Request {
    /// Numeric JSON-RPC request identifier.
    pub id: i64,
    /// LSP method name.
    pub method: String,
    /// Method-specific JSON parameters.
    pub params: Value,
    /// Local enqueue time used for timeout and latency accounting.
    pub timestamp: Instant,
}

static ID: AtomicUsize = AtomicUsize::new(1);

impl Request {
    /// Creates a request with the next process-wide correlation ID.
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
/// Successful response paired with its originating request when known.
pub struct ResponseMessage {
    /// JSON-RPC request identifier.
    pub id: i64,
    /// Method-specific result payload.
    pub result: Value,
    /// Request metadata recovered from the pending-request table.
    pub request: Option<Request>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Request initiated by a language server toward Red.
pub struct ServerRequest {
    /// Server-selected JSON-RPC identifier, which may be numeric or textual.
    pub id: Value,
    /// LSP method name.
    pub method: String,
    /// Method-specific parameters.
    pub params: Value,
    /// Managed client key that received the request.
    pub source: Option<String>,
}

#[derive(Debug)]
/// Response Red sends for a server-initiated request.
pub struct ServerResponse {
    /// Identifier copied from the server request.
    pub id: Value,
    /// Successful result, mutually exclusive with `error`.
    pub result: Option<Value>,
    /// JSON-RPC error object, mutually exclusive with `result`.
    pub error: Option<Value>,
}

#[derive(Debug)]
#[allow(unused)]
/// Raw server notification not recognized by Red's typed notification set.
pub struct Notification {
    method: String,
    params: Value,
}

#[derive(Debug)]
#[allow(unused)]
pub struct ResponseError {
    pub(crate) id: Option<i64>,
    pub(crate) code: i64,
    pub(crate) message: String,
    pub(crate) data: Option<Value>,
}

#[derive(Debug)]
/// Message consumed by the asynchronous server writer.
pub enum OutboundMessage {
    /// Correlated request expecting a response.
    Request(Request),
    /// Fire-and-forget notification.
    Notification(NotificationRequest),
    /// Reply to a server-initiated request.
    Response(ServerResponse),
}

impl Display for OutboundMessage {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            OutboundMessage::Request(req) => write!(f, "Request({})", req),
            OutboundMessage::Notification(req) => write!(f, "Notification({})", req),
            OutboundMessage::Response(response) => {
                write!(f, "Response(id={})", response.id)
            }
        }
    }
}

#[derive(Debug)]
/// Message delivered from an LSP reader task to the editor.
pub enum InboundMessage {
    /// Successful response to a Red-initiated request.
    Message(ResponseMessage),
    /// Recognized typed notification.
    Notification(ParsedNotification),
    /// Well-formed notification whose method Red does not interpret.
    UnknownNotification(Notification),
    /// Request that requires an editor-owned response.
    ServerRequest(ServerRequest),
    /// JSON-RPC error that could not be paired as a normal response.
    Error(ResponseError),
    /// Error paired with a known Red request ID.
    RequestError { id: i64, error: LspError },
    /// Transport or parsing failure not tied to one request.
    ProcessingError(LspError),
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(untagged)]
/// Server notifications that affect editor-owned state.
pub enum ParsedNotification {
    /// Diagnostics replacement for one text document.
    PublishDiagnostics(TextDocumentPublishDiagnostics),
    /// Work-done or partial-result progress update.
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

/// Allocates a process-wide JSON-RPC request identifier.
pub fn next_id() -> usize {
    ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

#[async_trait::async_trait]
/// Editor-facing abstraction over one or more language-server processes.
///
/// Methods accepting `x` and `y` receive Red character columns and zero-based
/// lines; implementations own conversion to UTF-16 LSP [`Position`] values.
/// Returned integers are request IDs whose results arrive through
/// [`Self::recv_response`].
pub trait LspClient: std::any::Any + Send {
    /// Performs the LSP initialize/initialized handshake.
    async fn initialize(&mut self) -> Result<(), LspError>;
    /// Opens a document and establishes its initial version.
    async fn did_open(&mut self, file: &str, contents: &str) -> Result<(), LspError>;
    /// Publishes the current full document text as the next version.
    async fn did_change(&mut self, file: &str, contents: String) -> Result<(), LspError>;
    /// Closes a document if this client tracks document lifecycles.
    async fn did_close(&mut self, _file: &str) -> Result<(), LspError> {
        Ok(())
    }
    /// Notifies the server immediately before Red saves a document.
    async fn will_save(&mut self, file: &str) -> Result<(), LspError>;
    /// Requests hover information at an editor position.
    async fn hover(&mut self, file: &str, x: usize, y: usize) -> Result<i64, LspError>;
    /// Requests definition locations at an editor position.
    async fn goto_definition(&mut self, file: &str, x: usize, y: usize) -> Result<i64, LspError>;
    /// Requests completion candidates at an editor position.
    async fn completion(&mut self, file: &str, x: usize, y: usize) -> Result<i64, LspError>;
    /// Requests whole-document formatting with server defaults.
    async fn format_document(&mut self, file: &str) -> Result<i64, LspError>;
    /// Requests whole-document formatting with explicit indentation options.
    async fn format_document_with_options(
        &mut self,
        file: &str,
        _tab_size: usize,
        _insert_spaces: bool,
    ) -> Result<i64, LspError> {
        self.format_document(file).await
    }
    /// Requests the symbol hierarchy for a document.
    async fn document_symbols(&mut self, file: &str) -> Result<i64, LspError>;
    /// Requests actions applicable to a range and its known diagnostics.
    async fn code_action(
        &mut self,
        file: &str,
        range: Range,
        diagnostics: Vec<Diagnostic>,
    ) -> Result<i64, LspError>;
    /// Requests callable signature help at an editor position.
    async fn signature_help(&mut self, file: &str, x: usize, y: usize) -> Result<i64, LspError>;
    /// Requests a workspace rename from an editor position.
    async fn rename(
        &mut self,
        file: &str,
        x: usize,
        y: usize,
        new_name: &str,
    ) -> Result<i64, LspError>;
    /// Requests document-local highlights related to an editor position.
    async fn document_highlight(&mut self, file: &str, x: usize, y: usize)
        -> Result<i64, LspError>;
    /// Requests links embedded in a document.
    async fn document_link(&mut self, file: &str) -> Result<i64, LspError>;
    /// Requests color literals and their ranges.
    async fn document_color(&mut self, file: &str) -> Result<i64, LspError>;
    /// Requests foldable document ranges.
    async fn folding_range(&mut self, file: &str) -> Result<i64, LspError>;
    /// Searches workspace symbols using this client's default workspace.
    async fn workspace_symbol(&mut self, query: &str) -> Result<i64, LspError>;
    /// Searches workspace symbols using the workspace associated with `file`.
    async fn workspace_symbol_for_file(
        &mut self,
        _file: &str,
        query: &str,
    ) -> Result<i64, LspError> {
        self.workspace_symbol(query).await
    }
    /// Requests references to the symbol at an editor position.
    async fn references(
        &mut self,
        file: &str,
        x: usize,
        y: usize,
        include_declaration: bool,
    ) -> Result<i64, LspError>;
    /// Resolves call-hierarchy roots at an editor position.
    async fn call_hierarchy_prepare(
        &mut self,
        file: &str,
        x: usize,
        y: usize,
    ) -> Result<i64, LspError>;
    /// Requests a complete semantic-token stream.
    async fn semantic_tokens_full(&mut self, file: &str) -> Result<i64, LspError>;
    /// Requests inlay hints for an LSP range.
    async fn inlay_hint(&mut self, file: &str, range: Range) -> Result<i64, LspError>;
    /// Sends an arbitrary request to this client.
    ///
    /// `force` bypasses capability gating used by known high-level requests.
    async fn send_request(
        &mut self,
        method: &str,
        params: Value,
        force: bool,
    ) -> Result<i64, LspError>;
    /// Sends an arbitrary request through the client associated with `file`.
    async fn send_request_for_file(
        &mut self,
        _file: &str,
        method: &str,
        params: Value,
        force: bool,
    ) -> Result<i64, LspError> {
        self.send_request(method, params, force).await
    }
    /// Sends an arbitrary request through an explicitly named managed client.
    async fn send_request_for_source(
        &mut self,
        _source: &str,
        method: &str,
        params: Value,
        force: bool,
    ) -> Result<i64, LspError> {
        self.send_request(method, params, force).await
    }
    /// Sends an arbitrary notification, optionally bypassing lifecycle gating.
    async fn send_notification(
        &mut self,
        method: &str,
        params: Value,
        force: bool,
    ) -> Result<(), LspError>;

    /// Sends completion using an already converted LSP line and UTF-16 column.
    async fn request_completion(
        &mut self,
        file_uri: &str,
        line: usize,
        character: usize,
        trigger_character: Option<char>,
    ) -> Result<i64, LspError>;

    /// Pulls diagnostics when the server advertises support.
    ///
    /// Returns `None` when the capability is unavailable.
    async fn request_diagnostics(&mut self, file_uri: &str) -> Result<Option<i64>, LspError>;

    // TODO: Request code lens information if this capability is enabled, returns None otherwise
    // async fn request_code_lens(&mut self, file_uri: &str) -> Result<Option<i64>, LspError>;

    // TODO: Request code action information if this capability is enabled, returns None otherwise
    // async fn request_code_action(&mut self, file_uri: &str) -> Result<Option<i64>, LspError>;

    // TODO: Request inlay hint information if this capability is enabled, returns None otherwise
    // async fn request_inlay_hint(&mut self, file_uri: &str) -> Result<Option<i64>, LspError>;

    // TODO: Request document symbol information if this capability is enabled, returns None otherwise
    // async fn request_document_symbol(&mut self, file_uri: &str) -> Result<Option<i64>, LspError>;

    /// Receives the next inbound message and its managed client key, if any.
    async fn recv_response(&mut self)
        -> Result<Option<(InboundMessage, Option<String>)>, LspError>;

    /// Returns capabilities negotiated by this client's primary server.
    fn get_server_capabilities(&self) -> Option<&ServerCapabilities>;

    /// Returns capabilities for the managed client associated with `file`.
    fn server_capabilities_for_file(&self, _file: &str) -> Option<&ServerCapabilities> {
        self.get_server_capabilities()
    }

    /// Reports whether the server associated with `file` supports formatting.
    fn supports_document_formatting(&self, _file: &str) -> bool {
        true
    }

    /// Returns the most recently published LSP document version.
    fn document_version(&self, _file: &str) -> Option<i64> {
        None
    }

    /// Returns the workspace root selected for `file`.
    fn workspace_root_for_file(&self, _file: &str) -> Option<PathBuf> {
        None
    }

    /// Returns the workspace root associated with a server-initiated request.
    fn workspace_root_for_request(&self, _request: &ServerRequest) -> Option<PathBuf> {
        None
    }

    /// Replies to a server `workspace/applyEdit` request.
    async fn respond_workspace_edit(
        &mut self,
        _request: &ServerRequest,
        _applied: bool,
        _failure_reason: Option<&str>,
    ) -> Result<(), LspError> {
        Ok(())
    }

    /// Performs graceful server shutdown and releases the transport.
    async fn shutdown(&mut self) -> Result<(), LspError>;
}
