use std::{
    fmt::{self, Display, Formatter},
    process::Stdio,
    sync::atomic::AtomicUsize,
};

use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter},
    process::{ChildStdin, Command},
    sync::mpsc,
};

use crate::log;

use self::types::{
    ClientCapabilities, CompletionClientCapabilities, CompletionItem, InitializeParams,
    TextDocumentClientCapabilities,
};

mod types;

static ID: AtomicUsize = AtomicUsize::new(1);

#[derive(Debug)]
pub struct Request {
    method: String,
    params: Value,
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
            "Request {{ method: {}, params: {} }}",
            self.method, truncated_params
        )
    }
}

#[derive(Debug)]
pub struct ResponseMessage {
    id: usize,
    result: Value,
}

#[derive(Debug)]
pub struct Notification {
    method: String,
    params: Value,
}

#[derive(Debug)]
pub struct ResponseError {
    code: i64,
    message: String,
    data: Option<Value>,
}

#[derive(Debug)]
pub enum OutboundMessage {
    Request(Request),
    Notification(Request),
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
    Notification(Notification),
    Error(ResponseError),
    ProcessingError(String), // TODO: This should be an error type
}

pub async fn start_lsp() -> anyhow::Result<LspClient> {
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
            log!("[lsp] editor requested to send message: {}", message);

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
                    log!("[lsp] error reading stdout: {}", err);
                    rtx.send(InboundMessage::ProcessingError(err.to_string()))
                        .await
                        .unwrap();
                    continue;
                }
            };

            if read > 0 {
                log!("[lsp] incoming line: {:?}", line);
                if line.starts_with("Content-Length: ") {
                    let Ok(len) = line
                        .trim_start_matches("Content-Length: ")
                        .trim()
                        .parse::<usize>()
                    else {
                        log!("Error parsing Content-Length: {}", line);
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
                        log!("[lsp] error reading body: {}", err);
                        rtx.send(InboundMessage::ProcessingError(err.to_string()))
                            .await
                            .unwrap();
                        continue;
                    };

                    let body = String::from_utf8_lossy(&body);
                    let res = serde_json::from_str::<serde_json::Value>(&body).unwrap();
                    // trucates res to 100 characters
                    log!(
                        "[lsp] incoming message: {}",
                        res.to_string().chars().take(100).collect::<String>()
                    );

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
                        let id = id.as_u64().unwrap();
                        let result = res["result"].clone();

                        rtx.send(InboundMessage::Message(ResponseMessage {
                            id: id as usize,
                            result,
                        }))
                        .await
                        .unwrap();
                    } else {
                        // if there's no id, it's a notification
                        let method = res["method"].as_str().unwrap().to_string();
                        let params = res["params"].clone();

                        rtx.send(InboundMessage::Message(ResponseMessage {
                            id: 0,
                            result: json!({ "method": method, "params": params }),
                        }))
                        .await
                        .unwrap();
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
                rtx.send(InboundMessage::ProcessingError(line.clone()))
                    .await
                    .unwrap();
            }
        }
    });

    Ok(LspClient {
        request_tx,
        response_rx,
    })
}

pub struct LspClient {
    request_tx: mpsc::Sender<OutboundMessage>,
    response_rx: mpsc::Receiver<InboundMessage>,
}

impl LspClient {
    pub async fn start() -> anyhow::Result<LspClient> {
        start_lsp().await
    }

    pub async fn send_request(&mut self, method: &str, params: Value) -> anyhow::Result<()> {
        self.request_tx
            .send(OutboundMessage::Request(Request {
                method: method.to_string(),
                params,
            }))
            .await?;
        log!("[lsp] request sent: {:?}", method);
        Ok(())
    }

    pub async fn send_notification(&mut self, method: &str, params: Value) -> anyhow::Result<()> {
        self.request_tx
            .send(OutboundMessage::Notification(Request {
                method: method.to_string(),
                params,
            }))
            .await?;
        Ok(())
    }

    pub async fn recv_response(&mut self) -> Option<InboundMessage> {
        self.response_rx.recv().await
    }

    pub async fn initialize(&mut self) -> anyhow::Result<()> {
        let params = InitializeParams {
            capabilities: ClientCapabilities {
                text_document: Some(TextDocumentClientCapabilities {
                    completion: Some(CompletionClientCapabilities {
                        completion_item: Some(CompletionItem {
                            snippet_support: Some(true),
                            ..Default::default()
                        }),
                    }),
                }),
            },
            ..Default::default()
        };

        self.send_request("initialize", json!(params)).await?;

        // TODO: do we need to do anything with response?
        _ = self.recv_response().await;

        self.send_notification("initialized", json!({})).await?;

        Ok(())
    }

    pub async fn did_open(&mut self, file: &str, contents: &str) -> anyhow::Result<()> {
        log!("[lsp] did_open file: {}", file);
        let params = json!({
            "textDocument": {
                "uri": format!("file:///{}", file),
                "languageId": "rust",
                "version": 1,
                "text": contents,
            }
        });

        self.send_notification("textDocument/didOpen", params)
            .await?;

        Ok(())
    }

    pub async fn goto_definition(
        &mut self,
        file: &str,
        x: usize,
        y: usize,
    ) -> anyhow::Result<Option<(usize, usize, usize, usize)>> {
        let params = json!({
            "textDocument": {
                "uri": format!("file:///{}", file),
            },
            "position": {
                "line": y,
                "character": x,
            }
        });

        self.send_request("textDocument/definition", params).await?;

        if let Some(response) = self.recv_response().await {
            match response {
                InboundMessage::Message(response) => {
                    log!("[lsp] goto definition response: {:?}", response.result);
                    if let Some(range) = response.result["range"].as_object() {
                        return Ok(Some((
                            range["start"]["line"].as_u64().unwrap() as usize,
                            range["start"]["character"].as_u64().unwrap() as usize,
                            range["end"]["line"].as_u64().unwrap() as usize,
                            range["end"]["character"].as_u64().unwrap() as usize,
                        )));
                    }
                }
                InboundMessage::Error(err) => {
                    anyhow::bail!("Error: {}", err.message);
                }
                InboundMessage::ProcessingError(err) => {
                    anyhow::bail!("Error processing response: {}", err);
                }
                InboundMessage::Notification(notification) => {
                    log!("Unhandled notification: {:?}", notification);
                }
            }
        }

        Ok(None)
    }
}

pub async fn lsp_send_request(
    stdin: &mut BufWriter<ChildStdin>,
    req: &Request,
) -> anyhow::Result<usize> {
    let id = next_id();
    let req = json!({
        "id": id,
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
    req: &Request,
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
        let mut client = LspClient::start().await.unwrap();
        client.initialize().await.unwrap();
    }
}
