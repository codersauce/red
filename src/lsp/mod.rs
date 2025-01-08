use std::{
    collections::HashMap,
    fmt::{self, Display, Formatter},
    path::Path,
    process::{self, Stdio},
    sync::atomic::AtomicUsize,
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

pub use self::types::{Diagnostic, TextDocumentPublishDiagnostics};

mod types;

static ID: AtomicUsize = AtomicUsize::new(1);

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
}

impl Request {
    pub fn new(method: &str, params: Value) -> Request {
        Request {
            id: next_id() as i64,
            method: method.to_string(),
            params,
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
    ProcessingError(String), // TODO: This should be an error type
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(untagged)]
pub enum ParsedNotification {
    PublishDiagnostics(TextDocumentPublishDiagnostics),
}

pub async fn start_lsp() -> anyhow::Result<RealLspClient> {
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
            // log!("[lsp] editor requested to send message: {:#?}", message);

            match message {
                OutboundMessage::Request(req) => {
                    if let Err(err) = lsp_send_request(&mut stdin, &req).await {
                        rtx.send(InboundMessage::ProcessingError(err.to_string()))
                            .await
                            .unwrap();
                    }
                }
                OutboundMessage::Notification(req) => {
                    if let Err(err) = lsp_send_notification(&mut stdin, &req).await {
                        rtx.send(InboundMessage::ProcessingError(err.to_string()))
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
                    // log!("[lsp] error reading stdout: {}", err);
                    rtx.send(InboundMessage::ProcessingError(err.to_string()))
                        .await
                        .unwrap();
                    continue;
                }
            };

            if read > 0 {
                // log!("[lsp] incoming line: {:?}", line);
                if line.starts_with("Content-Length: ") {
                    let Ok(len) = line
                        .trim_start_matches("Content-Length: ")
                        .trim()
                        .parse::<usize>()
                    else {
                        // log!("Error parsing Content-Length: {}", line);
                        rtx.send(InboundMessage::ProcessingError(
                            "Error parsing Content-Length".to_string(),
                        ))
                        .await
                        .unwrap();
                        continue;
                    };

                    reader.read_line(&mut line).await.unwrap(); // empty line

                    let mut body = vec![0; len];
                    if let Err(err) = reader.read_exact(&mut body).await {
                        // log!("[lsp] error reading body: {}", err);
                        rtx.send(InboundMessage::ProcessingError(err.to_string()))
                            .await
                            .unwrap();
                        continue;
                    };

                    let body = String::from_utf8_lossy(&body);
                    let res = serde_json::from_str::<serde_json::Value>(&body).unwrap();
                    // trucates res to 100 characters
                    // log!(
                    //     "[lsp] incoming message: {}",
                    //     res.to_string().chars().take(100).collect::<String>()
                    // );

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
                        // TODO: error handling
                        let id = id.as_i64().unwrap();
                        let result = res["result"].clone();

                        rtx.send(InboundMessage::Message(ResponseMessage { id, result }))
                            .await
                            .unwrap();
                    } else {
                        // if there's no id, it's a notification
                        let method = res["method"].as_str().unwrap().to_string();
                        let params = res["params"].clone();

                        // log!("body: {body}");

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
                                // log!("[lsp] error parsint notification: {}", err);
                                rtx.send(InboundMessage::ProcessingError(err.to_string()))
                                    .await
                                    .unwrap();
                                continue;
                            }
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
                // log!("[lsp] incoming stderr: {:?}", line);
                match rtx
                    .send(InboundMessage::ProcessingError(line.clone()))
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

fn parse_notification(method: &str, params: &Value) -> anyhow::Result<Option<ParsedNotification>> {
    if method == "textDocument/publishDiagnostics" {
        return Ok(serde_json::from_value(params.clone())?);
    }

    Ok(None)
}

#[async_trait::async_trait]
pub trait LspClient: Send {
    async fn initialize(&mut self) -> anyhow::Result<()>;
    async fn did_open(&mut self, file: &str, contents: &str) -> anyhow::Result<()>;
    async fn did_change(&mut self, file: &str, contents: &str) -> anyhow::Result<()>;
    async fn hover(&mut self, file: &str, x: usize, y: usize) -> anyhow::Result<i64>;
    async fn goto_definition(&mut self, file: &str, x: usize, y: usize) -> anyhow::Result<i64>;
    async fn send_request(&mut self, method: &str, params: Value) -> anyhow::Result<i64>;
    async fn send_notification(&mut self, method: &str, params: Value) -> anyhow::Result<()>;
    async fn recv_response(&mut self) -> anyhow::Result<Option<(InboundMessage, Option<String>)>>;
}

pub struct RealLspClient {
    request_tx: mpsc::Sender<OutboundMessage>,
    response_rx: mpsc::Receiver<InboundMessage>,
    files_versions: HashMap<String, usize>,
    // FIXME: there's a potential for requests there errored out to be stuck in this HashMap
    // we might need to add a timeout for requests and remove them from this map if they take too long
    pending_responses: HashMap<i64, String>,
}

#[async_trait::async_trait]
impl LspClient for RealLspClient {
    async fn send_request(&mut self, method: &str, params: Value) -> anyhow::Result<i64> {
        let req = Request::new(method, params);
        let id = req.id;

        self.pending_responses.insert(id, method.to_string());
        self.request_tx.send(OutboundMessage::Request(req)).await?;

        // log!("[lsp] request {id} sent: {:?}", method);
        Ok(id)
    }

    async fn send_notification(&mut self, method: &str, params: Value) -> anyhow::Result<()> {
        self.request_tx
            .send(OutboundMessage::Notification(NotificationRequest {
                method: method.to_string(),
                params,
            }))
            .await?;
        Ok(())
    }

    async fn recv_response(&mut self) -> anyhow::Result<Option<(InboundMessage, Option<String>)>> {
        match self.response_rx.try_recv() {
            Ok(msg) => {
                if let InboundMessage::Message(msg) = &msg {
                    if let Some(method) = self.pending_responses.remove(&msg.id) {
                        return Ok(Some((InboundMessage::Message(msg.clone()), Some(method))));
                    }
                }
                Ok(Some((msg, None)))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    async fn initialize(&mut self) -> anyhow::Result<()> {
        self.send_request(
            "initialize",
            json!({
                "processId": process::id(),
                "clientInfo": {
                    "name": "red",
                    "version": "0.1.0",
                },
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
                    }
                },
            }),
        )
        .await?;

        // TODO: do we need to do anything with response?
        _ = self.recv_response().await;

        self.send_notification("initialized", json!({})).await?;

        Ok(())
    }

    async fn did_open(&mut self, file: &str, contents: &str) -> anyhow::Result<()> {
        // log!("[lsp] did_open file: {}", file);
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

    async fn did_change(&mut self, file: &str, contents: &str) -> anyhow::Result<()> {
        // log!("[lsp] did_change file: {}", file);
        // increment and get version
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

    async fn hover(&mut self, file: &str, x: usize, y: usize) -> anyhow::Result<i64> {
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

    async fn goto_definition(&mut self, file: &str, x: usize, y: usize) -> anyhow::Result<i64> {
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
}

pub async fn lsp_send_request(
    stdin: &mut BufWriter<ChildStdin>,
    req: &Request,
) -> anyhow::Result<i64> {
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
) -> anyhow::Result<()> {
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
