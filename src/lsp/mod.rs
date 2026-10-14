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
    path::{Path, PathBuf},
    sync::atomic::AtomicUsize,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

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
mod workspace_settings;
mod workspace_watch;

/// Ordered incremental edits and their cheap, exact document snapshots.
/// Ranges use UTF-16 coordinates in the document produced by the preceding edit.
#[derive(Debug, Clone)]
pub struct DocumentChange {
    pub before: ropey::Rope,
    pub after: ropey::Rope,
    pub changes: Vec<TextDocumentContentChangeEvent>,
}

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
    /// Managed server instance that owns this request, when routed by [`LspManager`].
    pub source: Option<LspRequestSource>,
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
            source: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// Stable identity for one managed language-server process.
pub struct LspRequestSource {
    /// Manager key composed from the configured server name and workspace root.
    pub key: String,
    /// Process instance identity, changed whenever the server is restarted.
    pub instance: u64,
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
    /// Managed server identity that received the request.
    #[serde(skip)]
    pub source: Option<LspRequestSource>,
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
    pub(crate) request: Option<Request>,
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
    /// Important server stderr that should be visible without failing the transport.
    ServerStderr(String),
    /// Transport or parsing failure not tied to one request.
    ProcessingError(LspError),
    /// Managed server failure with the producer identity needed for owned cleanup.
    ClientFailure {
        source: LspRequestSource,
        error: LspError,
    },
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

fn text_document_params(file: &str) -> Result<Value, LspError> {
    Ok(json!({ "textDocument": { "uri": file_uri(file)? } }))
}

fn document_position_params(file: &str, x: usize, y: usize) -> Result<Value, LspError> {
    Ok(json!({
        "textDocument": { "uri": file_uri(file)? },
        "position": { "line": y, "character": x },
    }))
}

#[async_trait::async_trait]
/// Editor-facing abstraction over one or more language-server processes.
///
/// Methods accepting `x` and `y` receive Red character columns and zero-based
/// lines; implementations own conversion to UTF-16 LSP [`Position`] values.
/// Returned integers are request IDs whose results arrive through
/// [`Self::recv_response`].
pub trait LspClient: std::any::Any + Send {
    /// Rebuilds document routing while preserving unaffected active server clients.
    async fn reconfigure(
        &mut self,
        _config: crate::config::LspConfig,
    ) -> Result<Vec<String>, LspError> {
        Ok(Vec::new())
    }

    /// Performs the LSP initialize/initialized handshake.
    async fn initialize(&mut self) -> Result<(), LspError>;
    /// Opens a document and establishes its initial version.
    async fn did_open(&mut self, file: &str, contents: &str) -> Result<(), LspError>;
    /// Publishes the current full document text as the next version.
    async fn did_change(&mut self, file: &str, contents: String) -> Result<(), LspError>;
    /// Publishes known edits when supported, otherwise falls back to full text.
    async fn did_change_edits(
        &mut self,
        file: &str,
        change: DocumentChange,
    ) -> Result<(), LspError> {
        self.did_change(file, change.after.to_string()).await
    }

    /// Notifies the server after the supplied document contents were saved successfully.
    async fn did_save(&mut self, _file: &str, _contents: &str) -> Result<(), LspError> {
        Ok(())
    }
    /// Closes a document if this client tracks document lifecycles.
    async fn did_close(&mut self, _file: &str) -> Result<(), LspError> {
        Ok(())
    }
    /// Notifies the server immediately before Red saves a document.
    async fn will_save(&mut self, file: &str) -> Result<(), LspError> {
        self.send_notification(
            "textDocument/willSave",
            json!({
                "textDocument": { "uri": file_uri(file)? },
                "reason": 1,
            }),
            false,
        )
        .await
    }
    /// Requests hover information at an editor position.
    async fn hover(&mut self, file: &str, x: usize, y: usize) -> Result<i64, LspError> {
        self.send_request_for_file(
            file,
            "textDocument/hover",
            document_position_params(file, x, y)?,
            false,
        )
        .await
    }
    /// Requests definition locations at an editor position.
    async fn goto_definition(&mut self, file: &str, x: usize, y: usize) -> Result<i64, LspError> {
        self.send_request_for_file(
            file,
            "textDocument/definition",
            document_position_params(file, x, y)?,
            false,
        )
        .await
    }
    /// Requests completion candidates at an editor position.
    async fn completion(&mut self, file: &str, x: usize, y: usize) -> Result<i64, LspError> {
        let mut params = document_position_params(file, x, y)?;
        params["context"] = json!({ "triggerKind": 1 });
        self.send_request_for_file(file, "textDocument/completion", params, false)
            .await
    }
    /// Requests whole-document formatting with server defaults.
    async fn format_document(&mut self, file: &str) -> Result<i64, LspError> {
        self.format_document_with_options(file, 4, true).await
    }
    /// Requests whole-document formatting with explicit indentation options.
    async fn format_document_with_options(
        &mut self,
        file: &str,
        tab_size: usize,
        insert_spaces: bool,
    ) -> Result<i64, LspError> {
        self.send_request_for_file(
            file,
            "textDocument/formatting",
            json!({
                "textDocument": { "uri": file_uri(file)? },
                "options": {
                    "tabSize": tab_size,
                    "insertSpaces": insert_spaces,
                    "trimTrailingWhitespace": true,
                    "insertFinalNewline": true,
                    "trimFinalNewlines": true,
                }
            }),
            false,
        )
        .await
    }
    /// Requests formatting for one document range with explicit indentation options.
    async fn format_range_with_options(
        &mut self,
        _file: &str,
        _range: Range,
        _tab_size: usize,
        _insert_spaces: bool,
    ) -> Result<i64, LspError> {
        self.send_request_for_file(
            _file,
            "textDocument/rangeFormatting",
            json!({
                "textDocument": { "uri": file_uri(_file)? },
                "range": _range,
                "options": {
                    "tabSize": _tab_size,
                    "insertSpaces": _insert_spaces,
                    "trimTrailingWhitespace": true,
                    "insertFinalNewline": false,
                    "trimFinalNewlines": false,
                }
            }),
            false,
        )
        .await
    }
    /// Requests the symbol hierarchy for a document.
    async fn document_symbols(&mut self, file: &str) -> Result<i64, LspError> {
        self.send_request_for_file(
            file,
            "textDocument/documentSymbol",
            text_document_params(file)?,
            false,
        )
        .await
    }
    /// Requests actions applicable to a range and its known diagnostics.
    async fn code_action(
        &mut self,
        file: &str,
        range: Range,
        diagnostics: Vec<Diagnostic>,
    ) -> Result<i64, LspError> {
        self.send_request_for_file(
            file,
            "textDocument/codeAction",
            json!({
                "textDocument": { "uri": file_uri(file)? },
                "range": range,
                "context": { "diagnostics": diagnostics },
            }),
            false,
        )
        .await
    }
    /// Requests applicable actions from every capable server attached to a document.
    async fn code_actions(
        &mut self,
        file: &str,
        range: Range,
        diagnostics: Vec<(String, Vec<Diagnostic>)>,
    ) -> Result<Vec<i64>, LspError> {
        let diagnostics = diagnostics
            .into_iter()
            .flat_map(|(_, diagnostics)| diagnostics)
            .collect();
        self.code_action(file, range, diagnostics)
            .await
            .map(|id| (id > 0).then_some(id).into_iter().collect())
    }
    /// Requests callable signature help at an editor position.
    async fn signature_help(&mut self, file: &str, x: usize, y: usize) -> Result<i64, LspError> {
        self.send_request_for_file(
            file,
            "textDocument/signatureHelp",
            document_position_params(file, x, y)?,
            false,
        )
        .await
    }
    /// Requests signature help with automatic-trigger and retrigger context.
    async fn signature_help_with_context(
        &mut self,
        file: &str,
        x: usize,
        y: usize,
        _context: Option<SignatureHelpContext>,
    ) -> Result<i64, LspError> {
        self.signature_help(file, x, y).await
    }
    /// Requests a workspace rename from an editor position.
    async fn rename(
        &mut self,
        file: &str,
        x: usize,
        y: usize,
        new_name: &str,
    ) -> Result<i64, LspError> {
        let mut params = document_position_params(file, x, y)?;
        params["newName"] = Value::String(new_name.to_string());
        self.send_request_for_file(file, "textDocument/rename", params, false)
            .await
    }
    /// Requests document-local highlights related to an editor position.
    async fn document_highlight(
        &mut self,
        file: &str,
        x: usize,
        y: usize,
    ) -> Result<i64, LspError> {
        self.send_request_for_file(
            file,
            "textDocument/documentHighlight",
            document_position_params(file, x, y)?,
            false,
        )
        .await
    }
    /// Requests links embedded in a document.
    async fn document_link(&mut self, file: &str) -> Result<i64, LspError> {
        self.send_request_for_file(
            file,
            "textDocument/documentLink",
            text_document_params(file)?,
            false,
        )
        .await
    }
    /// Requests color literals and their ranges.
    async fn document_color(&mut self, file: &str) -> Result<i64, LspError> {
        self.send_request_for_file(
            file,
            "textDocument/documentColor",
            text_document_params(file)?,
            false,
        )
        .await
    }
    /// Requests foldable document ranges.
    async fn folding_range(&mut self, file: &str) -> Result<i64, LspError> {
        self.send_request_for_file(
            file,
            "textDocument/foldingRange",
            text_document_params(file)?,
            false,
        )
        .await
    }
    /// Searches workspace symbols using this client's default workspace.
    async fn workspace_symbol(&mut self, query: &str) -> Result<i64, LspError> {
        self.send_request("workspace/symbol", json!({ "query": query }), false)
            .await
    }
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
    ) -> Result<i64, LspError> {
        let mut params = document_position_params(file, x, y)?;
        params["context"] = json!({ "includeDeclaration": include_declaration });
        self.send_request_for_file(file, "textDocument/references", params, false)
            .await
    }
    /// Resolves call-hierarchy roots at an editor position.
    async fn call_hierarchy_prepare(
        &mut self,
        file: &str,
        x: usize,
        y: usize,
    ) -> Result<i64, LspError> {
        self.send_request_for_file(
            file,
            "textDocument/prepareCallHierarchy",
            document_position_params(file, x, y)?,
            false,
        )
        .await
    }
    /// Requests a complete semantic-token stream.
    async fn semantic_tokens_full(&mut self, file: &str) -> Result<i64, LspError> {
        self.send_request_for_file(
            file,
            "textDocument/semanticTokens/full",
            text_document_params(file)?,
            false,
        )
        .await
    }
    /// Requests inlay hints for an LSP range.
    async fn inlay_hint(&mut self, file: &str, range: Range) -> Result<i64, LspError> {
        self.send_request_for_file(
            file,
            "textDocument/inlayHint",
            json!({
                "textDocument": { "uri": file_uri(file)? },
                "range": range,
            }),
            false,
        )
        .await
    }
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
    /// Returns `None` when the capability is unavailable or the refresh was
    /// queued behind an in-flight diagnostic request for the same document.
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

    /// Returns capabilities for the ready server selected to handle `method`.
    fn server_capabilities_for_method(
        &self,
        file: &str,
        _method: &str,
    ) -> Option<&ServerCapabilities> {
        self.server_capabilities_for_file(file)
    }

    /// Returns capabilities for a previously captured managed server identity.
    fn server_capabilities_for_source(
        &self,
        _source: &LspRequestSource,
    ) -> Option<&ServerCapabilities> {
        self.get_server_capabilities()
    }

    /// Returns the ready managed server selected to handle `method`.
    fn request_source_for_file(&self, file: &str, method: &str) -> Option<LspRequestSource> {
        self.server_capabilities_for_method(file, method)?;
        Some(LspRequestSource {
            key: file.to_string(),
            instance: self.server_instance_for_file(file).unwrap_or_default(),
        })
    }

    /// Returns configured managed-client keys that may handle `method` for `file`.
    fn request_sources_for_file(&self, file: &str, method: &str) -> Vec<String> {
        self.request_source_for_file(file, method)
            .map(|source| vec![source.key])
            .unwrap_or_default()
    }

    /// Returns every configured managed-client key attached to `file`.
    fn attachment_sources_for_file(&self, file: &str) -> Vec<String> {
        self.server_capabilities_for_file(file)
            .map(|_| vec![file.to_string()])
            .unwrap_or_default()
    }

    /// Reports whether a previously captured managed server identity is still current.
    fn request_source_is_current(&self, source: &LspRequestSource) -> bool {
        self.server_instance_for_file(&source.key)
            .is_none_or(|instance| instance == source.instance)
    }

    /// Reports whether a current managed server still owns the open document.
    fn request_source_owns_file(&self, source: &LspRequestSource, _file: &str) -> bool {
        self.request_source_is_current(source)
    }

    /// Reports whether a current managed server owns the workspace containing `file`.
    fn request_source_owns_workspace_file(&self, source: &LspRequestSource, file: &str) -> bool {
        self.request_source_is_current(source)
            && self
                .workspace_root_for_source(source)
                .is_some_and(|root| Path::new(file).starts_with(root))
    }

    /// Returns the document version owned by a specific managed server.
    fn document_version_for_source(&self, file: &str, _source: &LspRequestSource) -> Option<i64> {
        self.document_version(file)
    }

    /// Returns the workspace root owned by a specific managed server.
    fn workspace_root_for_source(&self, source: &LspRequestSource) -> Option<PathBuf> {
        self.workspace_root_for_file(&source.key)
    }

    /// Returns the configured server name associated with `file`.
    fn server_name_for_file(&self, _file: &str) -> Option<String> {
        None
    }

    /// Read-only lifecycle status; this must not start a server.
    fn server_status_for_file(&self, file: &str) -> &'static str {
        if self.server_capabilities_for_file(file).is_some() {
            "ready"
        } else {
            "not_started"
        }
    }

    /// Identity of a ready server instance, invalidated on replacement or failure.
    fn server_instance_for_file(&self, file: &str) -> Option<u64> {
        self.server_capabilities_for_file(file).map(|_| 0)
    }

    /// Best-effort cancellation, routed to the same server as the original request.
    async fn cancel_request_for_file(&mut self, _file: &str, _id: i64) -> Result<(), LspError> {
        Ok(())
    }

    /// Reports whether the server associated with `file` supports formatting.
    fn supports_document_formatting(&self, _file: &str) -> bool {
        true
    }

    /// Reports whether the server associated with `file` supports range formatting.
    fn supports_range_formatting(&self, _file: &str) -> bool {
        false
    }

    /// Reports whether any attachment needs canonical incremental edit ranges.
    fn requires_incremental_change(&self, file: &str) -> bool {
        self.server_capabilities_for_file(file)
            .is_some_and(|capabilities| {
                matches!(
                    capabilities
                        .text_document_sync
                        .as_ref()
                        .and_then(|sync| sync.change_kind()),
                    Some(TextDocumentSyncKind::Incremental)
                )
            })
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
