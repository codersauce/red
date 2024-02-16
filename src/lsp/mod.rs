use std::{
    io::Write,
    process::Stdio,
    // io::{BufRead, Read, Write},
    // process::{ChildStdin, Command, Stdio},
    sync::atomic::AtomicUsize,
};

use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter},
    process::{ChildStdin, ChildStdout, Command},
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

async fn start_lsp() -> anyhow::Result<LspClient> {
    let mut child = Command::new("rust-analyzer")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let (request_tx, mut request_rx) = mpsc::channel::<Message>(32);
    let (response_tx, response_rx) = mpsc::channel::<Response>(32);

    // Sends requests from the editor into LSP's stdin
    let rtx = response_tx.clone();
    tokio::spawn(async move {
        let mut stdin = BufWriter::new(stdin);
        while let Some(message) = request_rx.recv().await {
            println!("Requested to send message: {:?}", message);

            match message {
                Message::Request(req) => {
                    if let Err(err) = lsp_send_request(&mut stdin, &req).await {
                        rtx.send(Response::ProcessingError(err.to_string()))
                            .await
                            .unwrap();
                    }
                }
                Message::Notification(req) => {
                    if let Err(err) = lsp_send_notification(&mut stdin, &req).await {
                        rtx.send(Response::ProcessingError(err.to_string()))
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
            println!("Waiting to read line");
            let read = match reader.read_line(&mut line).await {
                Ok(n) => {
                    println!("got: {}", n);
                    n
                }
                Err(err) => {
                    log!("Error reading from LSP's stdout: {}", err);
                    rtx.send(Response::ProcessingError(err.to_string()))
                        .await
                        .unwrap();
                    continue;
                }
            };
            println!("here");

            if read > 0 {
                println!("Received line: {:?}", line);
                if line.starts_with("Content-Length: ") {
                    let Ok(len) = line
                        .trim_start_matches("Content-Length: ")
                        .trim()
                        .parse::<usize>()
                    else {
                        log!("Error parsing Content-Length: {}", line);
                        rtx.send(Response::ProcessingError(
                            "Error parsing Content-Length".to_string(),
                        ))
                        .await
                        .unwrap();
                        continue;
                    };

                    reader.read_line(&mut line).await.unwrap(); // empty line

                    let mut body = vec![0; len];
                    if let Err(err) = reader.read_exact(&mut body).await {
                        log!("Error reading body: {}", err);
                        rtx.send(Response::ProcessingError(err.to_string()))
                            .await
                            .unwrap();
                        continue;
                    };

                    let body = String::from_utf8_lossy(&body);
                    let res = serde_json::from_str::<serde_json::Value>(&body).unwrap();
                    let id = res["id"].as_u64().unwrap(); // TODO: error handling
                    let result = res["result"].clone();

                    rtx.send(Response::Message(ResponseMessage {
                        id: id as usize,
                        result,
                    }))
                    .await
                    .unwrap();
                }
            }
        }
    });

    // Sends errors from LSP's stderr to the editor
    let rtx = response_tx.clone();
    tokio::spawn(async move {
        // FIXME: improve this handler, it's sending line by line to the editor
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        println!("Reading stderr");
        while let Ok(read) = reader.read_line(&mut line).await {
            println!("Read: {}", read);
            if read > 0 {
                println!("Received error: {:?}", line);
                rtx.send(Response::ProcessingError(line.clone()))
                    .await
                    .unwrap();
            }
        }
        println!("Finished reading stderr");
    });

    Ok(LspClient {
        request_tx,
        response_rx,
    })
}

// pub fn start_lsp2() -> anyhow::Result<LspClient> {
//     let (lsp_request_tx, lsp_request_rx) = std::sync::mpsc::channel::<Message>();
//     let (lsp_response_tx, lsp_response_rx) = std::sync::mpsc::channel::<Response>();
//
//     let mut process = Command::new("rust-analyzer")
//         .stdin(Stdio::piped())
//         .stdout(Stdio::piped())
//         .spawn()?;
//
//     let stdout = process.stdout.unwrap();
//
//     let response_tx = lsp_response_tx.clone();
//     thread::spawn(move || {
//         let stdin = process.stdin.as_mut().unwrap();
//
//         while let Ok(message) = lsp_request_rx.recv() {
//             match message {
//                 Message::Request(req) => {
//                     if let Err(err) = lsp_send_request(stdin, &req) {
//                         response_tx
//                             .send(Response::ProcessingError(err.to_string()))
//                             .unwrap();
//                     }
//                 }
//                 Message::Notification(req) => {
//                     if let Err(err) = lsp_send_notification(stdin, &req) {
//                         response_tx
//                             .send(Response::ProcessingError(err.to_string()))
//                             .unwrap();
//                     }
//                 }
//             }
//         }
//     });
//
//     thread::spawn(move || {
//         let mut reader = std::io::BufReader::new(stdout);
//
//         loop {
//             let mut content_length = String::new();
//             reader.read_line(&mut content_length).unwrap();
//
//             let mut empty_line = String::new();
//             reader.read_line(&mut empty_line).unwrap();
//
//             let content_length = content_length.strip_prefix("Content-Length: ").unwrap();
//             let content_length = content_length.trim().parse::<usize>().unwrap();
//
//             let mut body = vec![0; content_length];
//             reader.read_exact(&mut body).unwrap();
//
//             let body = String::from_utf8_lossy(&body);
//             let res = serde_json::from_str::<serde_json::Value>(&body).unwrap();
//             let id = res["id"].as_u64().unwrap();
//             let result = res["result"].clone();
//
//             lsp_response_tx
//                 .send(Response::Message(ResponseMessage {
//                     id: id as usize,
//                     result,
//                 }))
//                 .unwrap();
//         }
//     });
//
//     Ok(LspClient {
//         lsp_request_tx,
//         lsp_response_rx,
//     })
// }

pub struct LspClient {
    request_tx: mpsc::Sender<Message>,
    response_rx: mpsc::Receiver<Response>,
}

impl LspClient {
    pub async fn start() -> anyhow::Result<LspClient> {
        start_lsp().await
    }

    pub async fn send_request(&mut self, method: &str, params: Value) -> anyhow::Result<()> {
        self.request_tx
            .send(Message::Request(Request {
                method: method.to_string(),
                params,
            }))
            .await?;
        println!("Sent request: {:?}", method);
        Ok(())
    }

    pub async fn send_notification(&mut self, method: &str, params: Value) -> anyhow::Result<()> {
        self.request_tx
            .send(Message::Notification(Request {
                method: method.to_string(),
                params,
            }))
            .await?;
        Ok(())
    }

    pub async fn recv_response(&mut self) -> Option<Response> {
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
    println!("Sending request: {:?}", req);
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
