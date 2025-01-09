use std::{
    collections::HashMap,
    env,
    fmt::{self, Display, Formatter},
    path::{Path, PathBuf},
    process::{self, Stdio},
    sync::atomic::AtomicUsize,
    time::{Duration, Instant},
};

use path_absolutize::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter},
    process::{ChildStdin, Command},
    sync::mpsc::{self, error::TryRecvError},
};

use crate::log;

pub use self::types::{Diagnostic, Range, TextDocumentPublishDiagnostics};

mod types;

static ID: AtomicUsize = AtomicUsize::new(1);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum LspError {
    RequestTimeout(Duration),
    ServerError(String),
    ProtocolError(String),
    IoError(std::io::Error),
    JsonError(serde_json::Error),
    ChannelError(tokio::sync::mpsc::error::SendError<OutboundMessage>),
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
}

pub async fn start_lsp() -> Result<RealLspClient, LspError> {
    let mut child = Command::new("rust-analyzer")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let (request_tx, mut request_rx) = mpsc::channel::<OutboundMessage>(32);
    let (response_tx, response_rx) = mpsc::channel::<InboundMessage>(32);

    // Sends requests from the editor into LSP's stdin
    let rtx = response_tx.clone();
    tokio::spawn(async move {
        let mut stdin = BufWriter::new(stdin);
        while let Some(message) = request_rx.recv().await {
            log!("[lsp] editor requested to send message: {:#?}", message);
            match message {
                OutboundMessage::Request(req) => {
                    if let Err(err) = lsp_send_request(&mut stdin, &req).await {
                        rtx.send(InboundMessage::ProcessingError(err))
                            .await
                            .unwrap();
                    }
                }
                OutboundMessage::Notification(req) => {
                    if let Err(err) = lsp_send_notification(&mut stdin, &req).await {
                        rtx.send(InboundMessage::ProcessingError(err))
                            .await
                            .unwrap();
                    }
                }
            }
        }
    });

    // Sends responses from LSP's stdout to the editor
    let rtx = response_tx.clone();
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);

        loop {
            let mut line = String::new();
            let read = match reader.read_line(&mut line).await {
                Ok(n) => n,
                Err(err) => {
                    log!("[lsp] error reading stdout: {}", err);
                    rtx.send(InboundMessage::ProcessingError(LspError::IoError(err)))
                        .await
                        .unwrap();
                    continue;
                }
            };

            if read > 0 && line.starts_with("Content-Length: ") {
                let len = match line
                    .trim_start_matches("Content-Length: ")
                    .trim()
                    .parse::<usize>()
                {
                    Ok(len) => len,
                    Err(_) => {
                        log!(
                            "[lsp] invalid Content-Length: {}",
                            line.trim_start_matches("Content-Length: ").trim()
                        );
                        rtx.send(InboundMessage::ProcessingError(LspError::ProtocolError(
                            "Invalid Content-Length".to_string(),
                        )))
                        .await
                        .unwrap();
                        continue;
                    }
                };

                reader.read_line(&mut line).await.unwrap(); // empty line

                let mut body = vec![0; len];
                if let Err(err) = reader.read_exact(&mut body).await {
                    log!(
                        "[lsp] error reading body of length {}: {}",
                        len,
                        err.to_string()
                    );
                    rtx.send(InboundMessage::ProcessingError(LspError::IoError(err)))
                        .await
                        .unwrap();
                    continue;
                };

                let body = String::from_utf8_lossy(&body);
                let res = match serde_json::from_str::<serde_json::Value>(&body) {
                    Ok(res) => res,
                    Err(err) => {
                        log!("[lsp] error parsing JSON: {}", err);
                        rtx.send(InboundMessage::ProcessingError(LspError::JsonError(err)))
                            .await
                            .unwrap();
                        continue;
                    }
                };

                if let Some(error) = res.get("error") {
                    let code = error["code"].as_i64().unwrap();
                    let message = error["message"].as_str().unwrap().to_string();
                    let data = error.get("data").cloned();

                    rtx.send(InboundMessage::Error(ResponseError {
                        code,
                        message,
                        data,
                    }))
                    .await
                    .unwrap();

                    continue;
                }

                // if there's an id, it's a response
                if let Some(id) = res.get("id") {
                    let id = id.as_i64().unwrap();
                    let result = res["result"].clone();

                    log!(
                        "[lsp] incoming response: id={}, result={}",
                        id,
                        result.to_string()
                    );
                    rtx.send(InboundMessage::Message(ResponseMessage { id, result }))
                        .await
                        .unwrap();
                } else {
                    // if there's no id, it's a notification
                    let method = res["method"].as_str().unwrap().to_string();
                    let params = res["params"].clone();

                    log!(
                        "[lsp] incoming notification: method={}, params={}",
                        method,
                        params.to_string()
                    );

                    match parse_notification(&method, &params) {
                        Ok(Some(parsed_notification)) => {
                            rtx.send(InboundMessage::Notification(parsed_notification))
                                .await
                                .unwrap();
                        }
                        Ok(None) => {
                            rtx.send(InboundMessage::UnknownNotification(Notification {
                                method,
                                params,
                            }))
                            .await
                            .unwrap();
                        }
                        Err(err) => {
                            rtx.send(InboundMessage::ProcessingError(err))
                                .await
                                .unwrap();
                            continue;
                        }
                    }
                }
            }
        }
    });

    // Sends errors from LSP's stderr to the editor
    let rtx = response_tx.clone();
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        while let Ok(read) = reader.read_line(&mut line).await {
            if read > 0 {
                log!("[lsp] incoming stderr: {:?}", line);
                match rtx
                    .send(InboundMessage::ProcessingError(LspError::ServerError(
                        line.clone(),
                    )))
                    .await
                {
                    Ok(_) => (),
                    Err(err) => {
                        log!("[lsp] error sending stderr to editor: {}", err);
                    }
                }
            }
        }
    });

    Ok(RealLspClient {
        request_tx,
        response_rx,
        files_versions: HashMap::new(),
        pending_responses: HashMap::new(),
    })
}

fn parse_notification(
    method: &str,
    params: &Value,
) -> Result<Option<ParsedNotification>, LspError> {
    if method == "textDocument/publishDiagnostics" {
        return Ok(Some(serde_json::from_value(params.clone())?));
    }

    Ok(None)
}

#[async_trait::async_trait]
pub trait LspClient: Send {
    async fn initialize(&mut self) -> Result<(), LspError>;
    async fn did_open(&mut self, file: &str, contents: &str) -> Result<(), LspError>;
    async fn did_change(&mut self, file: &str, contents: &str) -> Result<(), LspError>;
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
    async fn send_request(&mut self, method: &str, params: Value) -> Result<i64, LspError>;
    async fn send_notification(&mut self, method: &str, params: Value) -> Result<(), LspError>;
    async fn recv_response(&mut self)
        -> Result<Option<(InboundMessage, Option<String>)>, LspError>;
}

pub struct RealLspClient {
    request_tx: mpsc::Sender<OutboundMessage>,
    response_rx: mpsc::Receiver<InboundMessage>,
    files_versions: HashMap<String, usize>,
    pending_responses: HashMap<i64, (String, Instant)>,
}

#[async_trait::async_trait]
impl LspClient for RealLspClient {
    async fn send_request(&mut self, method: &str, params: Value) -> Result<i64, LspError> {
        let req = Request::new(method, params);
        let id = req.id;
        let timestamp = req.timestamp;

        self.pending_responses
            .insert(id, (method.to_string(), timestamp));
        self.request_tx.send(OutboundMessage::Request(req)).await?;

        Ok(id)
    }

    async fn send_notification(&mut self, method: &str, params: Value) -> Result<(), LspError> {
        self.request_tx
            .send(OutboundMessage::Notification(NotificationRequest {
                method: method.to_string(),
                params,
            }))
            .await?;
        Ok(())
    }

    async fn recv_response(
        &mut self,
    ) -> Result<Option<(InboundMessage, Option<String>)>, LspError> {
        // Check for timeouts
        let now = Instant::now();
        let timed_out: Vec<_> = self
            .pending_responses
            .iter()
            .filter(|(_, (_, timestamp))| now.duration_since(*timestamp) > REQUEST_TIMEOUT)
            .map(|(&id, _)| id)
            .collect();

        for id in timed_out {
            if let Some((method, timestamp)) = self.pending_responses.remove(&id) {
                return Ok(Some((
                    InboundMessage::ProcessingError(LspError::RequestTimeout(
                        now.duration_since(timestamp),
                    )),
                    Some(method),
                )));
            }
        }

        match self.response_rx.try_recv() {
            Ok(msg) => {
                if let InboundMessage::Message(msg) = &msg {
                    if let Some((method, _)) = self.pending_responses.remove(&msg.id) {
                        return Ok(Some((InboundMessage::Message(msg.clone()), Some(method))));
                    }
                }
                Ok(Some((msg, None)))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(err) => Err(LspError::ProtocolError(err.to_string())),
        }
    }

    async fn initialize(&mut self) -> Result<(), LspError> {
        // Get the current working directory
        let workspace_path = env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from("."));

        // Convert to URI format (file:///path/to/workspace)
        let workspace_uri = format!("file://{}", workspace_path.display()).replace("\\", "/"); // Handle Windows paths if needed

        self.send_request(
            "initialize",
            json!({
                "processId": process::id(),
                "clientInfo": {
                    "name": "red",
                    "version": "0.1.0",
                },
                "rootUri": workspace_uri,
                "workspaceFolders": [{
                    "uri": workspace_uri,
                    "name": "red"
                }],
                "capabilities": {
                    "textDocument": {
                        "completion": {
                            "completionItem": {
                                "snippetSupport": true,
                            }
                        },
                        "definition": {
                            "dynamicRegistration": true,
                            "linkSupport": false,
                        },
                        "synchronization": {
                            "dynamicRegistration": true,
                            "willSave": true,
                            "willSaveWaitUntil": true,
                            "didSave": true,
                        },
                        "hover": {
                            "dynamicRegistration": true,
                            "contentFormat": ["plaintext"],
                        },
                        "formatting": {
                            "dynamicRegistration": true,
                        },
                        "documentSymbol": {
                            "dynamicRegistration": true,
                            "symbolKind": {
                                "valueSet": [
                                    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
                                    17, 18, 19, 20, 21, 22, 23, 24, 25, 26
                                ]
                            },
                            "hierarchicalDocumentSymbolSupport": true
                        },
                        "codeAction": {
                            "dynamicRegistration": true,
                            "codeActionLiteralSupport": {
                                "codeActionKind": {
                                    "valueSet": [
                                        "quickfix",
                                        "refactor",
                                        "refactor.extract",
                                        "refactor.inline",
                                        "refactor.rewrite",
                                        "source",
                                        "source.organizeImports"
                                    ]
                                }
                            }
                        },
                        "signatureHelp": {
                            "dynamicRegistration": true,
                            "signatureInformation": {
                                "documentationFormat": ["plaintext", "markdown"],
                                "parameterInformation": {
                                    "labelOffsetSupport": true
                                },
                                "activeParameterSupport": true
                            }
                        },
                        "documentHighlight": {
                            "dynamicRegistration": true
                        },
                        "documentLink": {
                            "dynamicRegistration": true,
                            "tooltipSupport": true
                        },
                        "colorProvider": {
                            "dynamicRegistration": true
                        },
                        "foldingRange": {
                            "dynamicRegistration": true,
                            "lineFoldingOnly": true
                        },
                        "semanticTokens": {
                            "dynamicRegistration": true,
                            "requests": {
                                "full": true
                            },
                            "tokenTypes": [
                                "namespace", "type", "class", "enum", "interface",
                                "struct", "typeParameter", "parameter", "variable",
                                "property", "enumMember", "event", "function",
                                "method", "macro", "keyword", "modifier", "comment",
                                "string", "number", "regexp", "operator"
                            ],
                            "tokenModifiers": [
                                "declaration", "definition", "readonly", "static",
                                "deprecated", "abstract", "async", "modification",
                                "documentation", "defaultLibrary"
                            ],
                            "formats": ["relative"]
                        },
                        "inlayHint": {
                            "dynamicRegistration": true,
                            "resolveSupport": {
                                "properties": ["tooltip", "textEdits", "label.tooltip", "label.location", "label.command"]
                            }
                        }
                    }
                },
                "workspace": {
                    "symbol": {
                        "dynamicRegistration": true,
                        "symbolKind": {
                            "valueSet": [
                                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
                                17, 18, 19, 20, 21, 22, 23, 24, 25, 26
                            ]
                        }
                    },
                    "workspaceEdit": {
                        "documentChanges": true,
                        "resourceOperations": ["create", "rename", "delete"]
                    }
                }
            }),
        )
        .await?;

        // TODO: do we need to do anything with response?
        _ = self.recv_response().await;

        self.send_notification("initialized", json!({})).await?;

        Ok(())
    }

    async fn did_open(&mut self, file: &str, contents: &str) -> Result<(), LspError> {
        log!("[lsp] did_open file: {}", file);
        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
                "languageId": "rust",
                "version": 1,
                "text": contents,
            }
        });

        self.send_notification("textDocument/didOpen", params)
            .await?;

        Ok(())
    }

    async fn did_change(&mut self, file: &str, contents: &str) -> Result<(), LspError> {
        log!("[lsp] did_change file: {}", file);
        let version = self.files_versions.entry(file.to_string()).or_insert(0);
        *version += 1;

        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
                "version": version,
            },
            "contentChanges": [
                {
                    "text": contents,
                }
            ]
        });

        self.send_notification("textDocument/didChange", params)
            .await?;

        Ok(())
    }

    async fn hover(&mut self, file: &str, x: usize, y: usize) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
            },
            "position": {
                "line": y,
                "character": x,
            }
        });

        self.send_request("textDocument/hover", params).await
    }

    async fn goto_definition(&mut self, file: &str, x: usize, y: usize) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
            },
            "position": {
                "line": y,
                "character": x,
            }
        });

        self.send_request("textDocument/definition", params).await
    }

    async fn completion(&mut self, file: &str, x: usize, y: usize) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
            },
            "position": {
                "line": y,
                "character": x,
            },
            "context": {
                "triggerKind": 1
            }
        });

        self.send_request("textDocument/completion", params).await
    }

    async fn format_document(&mut self, file: &str) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
            },
            "options": {
                "tabSize": 4,
                "insertSpaces": true,
                "trimTrailingWhitespace": true,
                "insertFinalNewline": true,
                "trimFinalNewlines": true
            }
        });

        self.send_request("textDocument/formatting", params).await
    }

    async fn document_symbols(&mut self, file: &str) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
            }
        });

        self.send_request("textDocument/documentSymbol", params)
            .await
    }

    async fn code_action(
        &mut self,
        file: &str,
        range: Range,
        diagnostics: Vec<Diagnostic>,
    ) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
            },
            "range": range,
            "context": {
                "diagnostics": diagnostics
            }
        });

        self.send_request("textDocument/codeAction", params).await
    }

    async fn document_highlight(
        &mut self,
        file: &str,
        x: usize,
        y: usize,
    ) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
            },
            "position": {
                "line": y,
                "character": x,
            }
        });

        self.send_request("textDocument/documentHighlight", params)
            .await
    }

    async fn document_link(&mut self, file: &str) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
            }
        });

        self.send_request("textDocument/documentLink", params).await
    }

    async fn document_color(&mut self, file: &str) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
            }
        });

        self.send_request("textDocument/documentColor", params)
            .await
    }

    async fn folding_range(&mut self, file: &str) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
            }
        });

        self.send_request("textDocument/foldingRange", params).await
    }

    async fn workspace_symbol(&mut self, query: &str) -> Result<i64, LspError> {
        let params = json!({
            "query": query
        });

        self.send_request("workspace/symbol", params).await
    }

    async fn call_hierarchy_prepare(
        &mut self,
        file: &str,
        x: usize,
        y: usize,
    ) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
            },
            "position": {
                "line": y,
                "character": x,
            }
        });

        self.send_request("textDocument/prepareCallHierarchy", params)
            .await
    }

    async fn semantic_tokens_full(&mut self, file: &str) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
            }
        });

        self.send_request("textDocument/semanticTokens/full", params)
            .await
    }

    async fn inlay_hint(&mut self, file: &str, range: Range) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
            },
            "range": range
        });

        self.send_request("textDocument/inlayHint", params).await
    }

    async fn signature_help(&mut self, file: &str, x: usize, y: usize) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
            },
            "position": {
                "line": y,
                "character": x,
            }
        });

        self.send_request("textDocument/signatureHelp", params)
            .await
    }
}

pub async fn lsp_send_request(
    stdin: &mut BufWriter<ChildStdin>,
    req: &Request,
) -> Result<i64, LspError> {
    let id = req.id;
    let req = json!({
        "id": req.id,
        "jsonrpc": "2.0",
        "method": req.method,
        "params": req.params,
    });
    let body = serde_json::to_string(&req)?;
    let req = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    stdin.write_all(req.as_bytes()).await?;
    stdin.flush().await?;

    Ok(id)
}

pub async fn lsp_send_notification(
    stdin: &mut BufWriter<ChildStdin>,
    req: &NotificationRequest,
) -> Result<(), LspError> {
    let req = json!({
        "jsonrpc": "2.0",
        "method": req.method,
        "params": req.params,
    });
    let body = serde_json::to_string(&req)?;
    let req = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    stdin.write_all(req.as_bytes()).await?;

    Ok(())
}

pub fn next_id() -> usize {
    ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn test_start_lsp() {
        let mut client = start_lsp().await.unwrap();
        client.initialize().await.unwrap();
    }

    #[tokio::test]
    async fn test_parse_publish_diagnostics() {
        let msg = std::fs::read_to_string("src/lsp/fixtures/publish-diagnostics.json").unwrap();
        let msg: Value = serde_json::from_str(&msg).unwrap();
        let params = &msg["params"];
        let msg: ParsedNotification = serde_json::from_value(params.clone()).unwrap();

        let ParsedNotification::PublishDiagnostics(msg) = msg;

        assert_eq!(msg.diagnostics.len(), 7);
        let diag = &msg.diagnostics[0];
        let code = diag.code.as_ref().unwrap();
        assert_eq!(code.as_string(), "dead_code");
    }

    #[tokio::test]
    async fn test_parse_publish_diagnostics_with_uri() {
        let msg =
            std::fs::read_to_string("src/lsp/fixtures/publish-diagnostics-with-uri.json").unwrap();
        let msg: Value = serde_json::from_str(&msg).unwrap();
        let params = &msg["params"];
        let msg: ParsedNotification = serde_json::from_value(params.clone()).unwrap();

        let ParsedNotification::PublishDiagnostics(msg) = msg;

        assert_eq!(msg.diagnostics.len(), 4);
        let diag = &msg.diagnostics[0];
        let code = diag.code.as_ref().unwrap();
        assert_eq!(code.as_string(), "unused_imports");
    }
}
