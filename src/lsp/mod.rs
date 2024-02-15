use std::{
    io::{BufRead, Read, Write},
    process::{ChildStdin, Command, Stdio},
    sync::atomic::AtomicUsize,
    thread,
};

use serde_json::{json, Value};

use self::types::InitializeParams;

mod types;

static ID: AtomicUsize = AtomicUsize::new(1);

#[derive(Debug)]
pub struct Request {
    method: String,
    params: Value,
}

#[derive(Debug)]
pub struct ResponseMessage {
    id: usize,
    result: Value,
}

#[derive(Debug)]
pub struct ResponseError {
    code: i64,
    message: String,
    data: Option<Value>,
}

#[derive(Debug)]
pub enum Message {
    Request(Request),
    Notification(Request),
}

#[derive(Debug)]
pub enum Response {
    Message(ResponseMessage),
    Error(ResponseError),
    ProcessingError(String), // TODO: This should be an error type
}

pub fn start_lsp() -> anyhow::Result<LspClient> {
    let (lsp_request_tx, lsp_request_rx) = std::sync::mpsc::channel::<Message>();
    let (lsp_response_tx, lsp_response_rx) = std::sync::mpsc::channel::<Response>();

    let mut process = Command::new("rust-analyzer")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;

    let stdout = process.stdout.unwrap();

    let response_tx = lsp_response_tx.clone();
    thread::spawn(move || {
        let stdin = process.stdin.as_mut().unwrap();

        while let Ok(message) = lsp_request_rx.recv() {
            match message {
                Message::Request(req) => {
                    if let Err(err) = lsp_send_request(stdin, &req) {
                        response_tx
                            .send(Response::ProcessingError(err.to_string()))
                            .unwrap();
                    }
                }
                Message::Notification(req) => {
                    if let Err(err) = lsp_send_notification(stdin, &req) {
                        response_tx
                            .send(Response::ProcessingError(err.to_string()))
                            .unwrap();
                    }
                }
            }
        }
    });

    thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stdout);

        loop {
            let mut content_length = String::new();
            reader.read_line(&mut content_length).unwrap();

            let mut empty_line = String::new();
            reader.read_line(&mut empty_line).unwrap();

            let content_length = content_length.strip_prefix("Content-Length: ").unwrap();
            let content_length = content_length.trim().parse::<usize>().unwrap();

            let mut body = vec![0; content_length];
            reader.read_exact(&mut body).unwrap();

            let body = String::from_utf8_lossy(&body);
            let res = serde_json::from_str::<serde_json::Value>(&body).unwrap();
            let id = res["id"].as_u64().unwrap();
            let result = res["result"].clone();

            lsp_response_tx
                .send(Response::Message(ResponseMessage {
                    id: id as usize,
                    result,
                }))
                .unwrap();
        }
    });

    Ok(LspClient {
        lsp_request_tx,
        lsp_response_rx,
    })
}

pub struct LspClient {
    lsp_request_tx: std::sync::mpsc::Sender<Message>,
    lsp_response_rx: std::sync::mpsc::Receiver<Response>,
}

impl LspClient {
    pub fn start() -> anyhow::Result<LspClient> {
        start_lsp()
    }

    pub fn send_request(&mut self, method: &str, params: Value) -> anyhow::Result<()> {
        self.lsp_request_tx.send(Message::Request(Request {
            method: method.to_string(),
            params,
        }))?;
        Ok(())
    }

    pub fn send_notification(&mut self, method: &str, params: Value) -> anyhow::Result<()> {
        self.lsp_request_tx.send(Message::Notification(Request {
            method: method.to_string(),
            params,
        }))?;
        Ok(())
    }

    pub fn recv_response(&mut self) -> anyhow::Result<Response> {
        self.lsp_response_rx.recv().map_err(|err| err.into())
    }

    pub fn initialize(&mut self, params: InitializeParams) -> anyhow::Result<()> {
        self.send_request("initialize", json!(params))?;
        _ = self.recv_response()?; // TODO do we need to do anything with response?
        self.send_notification("initialized", json!({}))?;

        Ok(())
    }
}

pub fn lsp_send_request(stdin: &mut ChildStdin, req: &Request) -> anyhow::Result<usize> {
    let id = next_id();
    let req = json!({
        "id": id,
        "jsonrpc": "2.0",
        "method": req.method,
        "params": req.params,
    });
    let body = serde_json::to_string(&req)?;
    let req = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    stdin.write_all(req.as_bytes())?;

    Ok(id)
}

pub fn lsp_send_notification(stdin: &mut ChildStdin, req: &Request) -> anyhow::Result<()> {
    let req = json!({
        "jsonrpc": "2.0",
        "method": req.method,
        "params": req.params,
    });
    let body = serde_json::to_string(&req)?;
    let req = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    stdin.write_all(req.as_bytes())?;

    Ok(())
}

pub fn next_id() -> usize {
    ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::{
        types::{CompletionClientCapabilities, CompletionItem, TextDocumentClientCapabilities},
        *,
    };

    #[test]
    fn test_start_lsp() {
        let mut client = LspClient::start().unwrap();
        client
            .initialize(InitializeParams {
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
            })
            .unwrap();
    }
}
