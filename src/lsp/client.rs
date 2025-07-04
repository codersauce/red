use std::{
    collections::HashMap,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};

use path_absolutize::*;
use serde_json::{json, Value};
use similar::{Algorithm, DiffOp, TextDiff};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter},
    process::{ChildStdin, Command as TokioCommand},
    sync::mpsc::{self, error::TryRecvError},
};

use super::{get_client_capabilities, InboundMessage, LspClient, OutboundMessage, ResponseError};
use crate::lsp::{
    parse_notification, types::*, Notification, NotificationRequest, Request, ResponseMessage,
};
use crate::{log, lsp::LspError, utils::get_workspace_uri};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub async fn start_lsp() -> Result<RealLspClient, LspError> {
    let mut child = TokioCommand::new("rust-analyzer")
        // .env("RA_LOG", "lsp_server=debug")
        // .arg("--log-file")
        // .arg("/tmp/rust-analyzer.log")
        // .arg("--no-log-buffering")
        .arg("-v")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let (request_tx, mut request_rx) = mpsc::channel::<OutboundMessage>(512);
    let (response_tx, response_rx) = mpsc::channel::<InboundMessage>(512);

    // Sends requests from the editor into LSP's stdin
    let rtx = response_tx.clone();
    tokio::spawn(async move {
        let mut stdin = BufWriter::new(stdin);
        while let Some(message) = request_rx.recv().await {
            match message {
                OutboundMessage::Request(req) => {
                    log!("[lsp] sending message: id={} method={}", req.id, req.method);
                    if let Err(err) = lsp_send_request(&mut stdin, &req).await {
                        rtx.send(InboundMessage::ProcessingError(err))
                            .await
                            .unwrap();
                    }
                }
                OutboundMessage::Notification(req) => {
                    log!("[lsp] sending notification: method={}", req.method);
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

            if read == 0 {
                // EOF reached
                break;
            }

            if line.starts_with("Content-Length: ") {
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
                        // rtx.send(InboundMessage::ProcessingError(LspError::ProtocolError(
                        //     "Invalid Content-Length".to_string(),
                        // )))
                        // .await
                        // .unwrap();
                        continue;
                    }
                };

                // reader.read_line(&mut line).await.unwrap(); // empty line
                let mut empty_line = String::new();
                if let Err(err) = reader.read_line(&mut empty_line).await {
                    log!("[lsp] error reading empty line: {}", err);
                    continue;
                }

                let mut body = vec![0; len];
                if let Err(err) = reader.read_exact(&mut body).await {
                    log!(
                        "[lsp] error reading body of length {}: {}",
                        len,
                        err.to_string()
                    );
                    // rtx.send(InboundMessage::ProcessingError(LspError::IoError(err)))
                    //     .await
                    //     .unwrap();
                    continue;
                };

                match process_lsp_message(&body, &rtx).await {
                    Ok(_) => (),
                    Err(err) => {
                        log!("[lsp] error processing message: {}", err);
                        continue;
                    }
                }
            } else {
                log!("[lsp] invalid line: {}", line);
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
        files_content: HashMap::new(),
        pending_responses: HashMap::new(),
        pending_messages: Vec::new(),
        initialize_id: None,
        initialized: false,
        server_capabilities: None,
        child: Some(child),
    })
}

async fn process_lsp_message(
    body: &[u8],
    rtx: &mpsc::Sender<InboundMessage>,
) -> Result<(), LspError> {
    let body = String::from_utf8_lossy(body);
    let res = serde_json::from_str::<serde_json::Value>(&body).map_err(LspError::JsonError)?;

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
        .map_err(|e| LspError::ChannelInboundError(e.to_string()))?;

        return Ok(());
    }

    // if there's an id, it's a response
    if let Some(id) = res.get("id") {
        let id = id.as_i64().unwrap();
        let result = res["result"].clone();

        log!(
            "[lsp] incoming response: id={}, result={}",
            id,
            if result.to_string().len() > 250 {
                let s = result.to_string();
                let truncate_at = s
                    .char_indices()
                    .take(250)
                    .last()
                    .map(|(i, c)| i + c.len_utf8())
                    .unwrap_or(s.len());
                format!("{}...", &s[..truncate_at])
            } else {
                result.to_string()
            }
        );

        rtx.send(InboundMessage::Message(ResponseMessage {
            id,
            result,
            request: None,
        }))
        .await
        .map_err(|e| LspError::ChannelInboundError(e.to_string()))?;
    } else {
        // if there's no id, it's a notification
        let method = res["method"].as_str().unwrap().to_string();
        let params = res["params"].clone();

        log!(
            "[lsp] incoming notification: method={}, params={}",
            method,
            params
        );

        match parse_notification(&method, &params) {
            Ok(Some(parsed_notification)) => {
                rtx.send(InboundMessage::Notification(parsed_notification))
                    .await
                    .map_err(|e| LspError::ChannelInboundError(e.to_string()))?;
            }
            Ok(None) => {
                rtx.send(InboundMessage::UnknownNotification(Notification {
                    method,
                    params,
                }))
                .await
                .map_err(|e| LspError::ChannelInboundError(e.to_string()))?;
            }
            Err(err) => {
                rtx.send(InboundMessage::ProcessingError(err))
                    .await
                    .map_err(|e| LspError::ChannelInboundError(e.to_string()))?;
            }
        }
    }

    Ok(())
}

pub struct RealLspClient {
    request_tx: mpsc::Sender<OutboundMessage>,
    response_rx: mpsc::Receiver<InboundMessage>,
    files_versions: HashMap<String, usize>,
    files_content: HashMap<String, String>,
    pending_responses: HashMap<i64, Request>,
    initialize_id: Option<i64>,
    initialized: bool,
    pending_messages: Vec<OutboundMessage>,
    server_capabilities: Option<ServerCapabilities>,
    child: Option<tokio::process::Child>,
}

impl RealLspClient {
    fn can_request_diagnostics(&self) -> bool {
        self.server_capabilities
            .as_ref()
            .map(|caps| caps.diagnostic_provider.is_some())
            .unwrap_or(false)
    }

    fn calculate_position(text: &str, char_offset: usize) -> Position {
        let mut line = 0;
        let mut character = 0;

        for (i, c) in text.chars().enumerate() {
            if i >= char_offset {
                break;
            }
            if c == '\n' {
                line += 1;
                character = 0;
            } else {
                character += 1;
            }
        }

        Position { line, character }
    }

    fn calculate_changes(old_text: &str, new_text: &str) -> Vec<TextDocumentContentChangeEvent> {
        let diff = TextDiff::configure()
            .algorithm(Algorithm::Myers)
            .timeout(std::time::Duration::from_secs(1))
            .diff_chars(old_text, new_text);

        let mut changes = Vec::new();
        let mut current_change = String::new();
        let mut start_offset = 0;
        let mut old_offset = 0;

        for group in diff.grouped_ops(3) {
            // Group changes that are close together
            for op in group {
                match op {
                    DiffOp::Delete {
                        old_index, old_len, ..
                    } => {
                        if !current_change.is_empty() {
                            // Flush pending insert
                            let start_pos = Self::calculate_position(old_text, start_offset);
                            changes.push(TextDocumentContentChangeEvent {
                                range: Some(Range {
                                    start: start_pos,
                                    end: start_pos,
                                }),
                                range_length: None,
                                text: std::mem::take(&mut current_change),
                            });
                        }

                        let start_pos = Self::calculate_position(old_text, old_index);
                        let end_pos = Self::calculate_position(old_text, old_index + old_len);

                        changes.push(TextDocumentContentChangeEvent {
                            range: Some(Range {
                                start: start_pos,
                                end: end_pos,
                            }),
                            range_length: None,
                            text: String::new(),
                        });

                        start_offset = old_index + old_len;
                        old_offset = old_index + old_len;
                    }
                    DiffOp::Insert {
                        new_index, new_len, ..
                    } => {
                        if current_change.is_empty() {
                            start_offset = old_offset;
                        }
                        // Convert character indices to byte indices for safe string slicing
                        let byte_start = new_text
                            .char_indices()
                            .nth(new_index)
                            .map(|(i, _)| i)
                            .unwrap_or(new_text.len());
                        let byte_end = new_text
                            .char_indices()
                            .nth(new_index + new_len)
                            .map(|(i, _)| i)
                            .unwrap_or(new_text.len());
                        current_change.push_str(&new_text[byte_start..byte_end]);
                    }
                    DiffOp::Equal { old_index, len, .. } => {
                        if !current_change.is_empty() {
                            // Flush pending insert
                            let start_pos = Self::calculate_position(old_text, start_offset);
                            changes.push(TextDocumentContentChangeEvent {
                                range: Some(Range {
                                    start: start_pos,
                                    end: start_pos,
                                }),
                                range_length: None,
                                text: std::mem::take(&mut current_change),
                            });
                        }
                        old_offset = old_index + len;
                    }
                    DiffOp::Replace {
                        old_index,
                        old_len,
                        new_index,
                        new_len,
                    } => {
                        if !current_change.is_empty() {
                            // Flush pending insert
                            let start_pos = Self::calculate_position(old_text, start_offset);
                            changes.push(TextDocumentContentChangeEvent {
                                range: Some(Range {
                                    start: start_pos,
                                    end: start_pos,
                                }),
                                range_length: None,
                                text: std::mem::take(&mut current_change),
                            });
                        }

                        let start_pos = Self::calculate_position(old_text, old_index);
                        let end_pos = Self::calculate_position(old_text, old_index + old_len);

                        // Convert character indices to byte indices for safe string slicing
                        let byte_start = new_text
                            .char_indices()
                            .nth(new_index)
                            .map(|(i, _)| i)
                            .unwrap_or(new_text.len());
                        let byte_end = new_text
                            .char_indices()
                            .nth(new_index + new_len)
                            .map(|(i, _)| i)
                            .unwrap_or(new_text.len());

                        changes.push(TextDocumentContentChangeEvent {
                            range: Some(Range {
                                start: start_pos,
                                end: end_pos,
                            }),
                            range_length: None,
                            text: new_text[byte_start..byte_end].to_string(),
                        });

                        start_offset = old_index + old_len;
                        old_offset = old_index + old_len;
                    }
                }
            }
        }

        // Flush any remaining changes
        if !current_change.is_empty() {
            let start_pos = Self::calculate_position(old_text, start_offset);
            changes.push(TextDocumentContentChangeEvent {
                range: Some(Range {
                    start: start_pos,
                    end: start_pos,
                }),
                range_length: None,
                text: current_change,
            });
        }

        changes
    }
}

#[async_trait::async_trait]
impl LspClient for RealLspClient {
    async fn send_request(
        &mut self,
        method: &str,
        params: Value,
        force: bool,
    ) -> Result<i64, LspError> {
        log!("[lsp] send_request: method={} force={force}", method);

        let req = Request::new(method, params);
        let id = req.id;
        let msg = OutboundMessage::Request(req.clone());

        if !self.initialized && !force {
            log!(
                "[lsp] client not initialized yet, adding request to pending: {}",
                id
            );
            self.pending_messages.push(msg);
            return Ok(id);
        }

        self.pending_responses.insert(id, req);
        self.request_tx.send(msg).await?;

        Ok(id)
    }

    async fn send_notification(
        &mut self,
        method: &str,
        params: Value,
        force: bool,
    ) -> Result<(), LspError> {
        log!("[lsp] send_notification: method={} force={force}", method);

        let msg = OutboundMessage::Notification(NotificationRequest {
            method: method.to_string(),
            params,
        });

        if !self.initialized && !force {
            log!(
                "[lsp] client not initialized yet, adding notification to pending: {}",
                method
            );
            self.pending_messages.push(msg);
            return Ok(());
        }

        self.request_tx.send(msg).await?;
        Ok(())
    }

    async fn request_completion(
        &mut self,
        file_uri: &str,
        line: usize,
        character: usize,
    ) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": file_uri,
            },
            "position": {
                "line": line,
                "character": character,
            },
        });

        log!("request_completion: params={}", params);

        self.send_request("textDocument/completion", params, false)
            .await
    }

    async fn request_diagnostics(&mut self, file_uri: &str) -> Result<Option<i64>, LspError> {
        if !self.can_request_diagnostics() {
            return Ok(None);
        }

        let params = json!({
            "textDocument": {
                "uri": file_uri,
            },
        });

        log!("request_diagnostics: params={}", params);

        Ok(Some(
            self.send_request("textDocument/diagnostic", params, false)
                .await?,
        ))
    }

    async fn recv_response(
        &mut self,
    ) -> Result<Option<(InboundMessage, Option<String>)>, LspError> {
        // Check for timeouts
        let now = Instant::now();
        let timed_out: Vec<_> = self
            .pending_responses
            .iter()
            .filter(|(_, Request { timestamp, .. })| {
                now.duration_since(*timestamp) > REQUEST_TIMEOUT
            })
            .map(|(&id, _)| id)
            .collect();

        for id in timed_out {
            if let Some(request) = self.pending_responses.remove(&id) {
                return Ok(Some((
                    InboundMessage::ProcessingError(LspError::RequestTimeout(
                        now.duration_since(request.timestamp),
                    )),
                    Some(request.method),
                )));
            }
        }

        match self.response_rx.try_recv() {
            Ok(mut msg) => {
                if let InboundMessage::Message(msg) = &mut msg {
                    if let Some(req) = self.pending_responses.remove(&msg.id) {
                        log!("[lsp] rcv_response: id={} method={}", msg.id, req.method);
                        if req.method == "initialize" {
                            log!("[lsp] server initialized");

                            // Parse the initialize result
                            // https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#initialized
                            let init_result: InitializeResult =
                                serde_json::from_value(msg.result.clone())
                                    .map_err(LspError::JsonError)?;

                            // log!("[lsp] server capabilities: {:#?}", init_result.capabilities);
                            self.server_capabilities = Some(init_result.capabilities);

                            if let Some(server_info) = &init_result.server_info {
                                log!(
                                    "[lsp] server info: {} {}",
                                    server_info.name,
                                    server_info.version.as_deref().unwrap_or("unknown version")
                                );
                            }

                            self.send_notification("initialized", json!({}), true)
                                .await?;
                            // self.send_notification(
                            //     "$/setTrace",
                            //     json!({ "value": "verbose" }),
                            //     true,
                            // )
                            // .await?;
                            self.initialized = true;

                            log!(
                                "[lsp] sending {} pending messages",
                                self.pending_messages.len()
                            );
                            for msg in self.pending_messages.drain(..) {
                                self.request_tx.send(msg).await?;
                            }
                        }

                        let method = req.method.clone();
                        msg.request = Some(req);

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
        let initialize_params = get_client_capabilities(get_workspace_uri());

        // log!("initialize_params: {:#?}", initialize_params);
        let initialize_params = match serde_json::to_value(initialize_params) {
            Ok(params) => params,
            Err(err) => {
                log!("[lsp] error serializing initialize params: {}", err);
                return Err(LspError::JsonError(err));
            }
        };

        self.initialize_id = Some(
            self.send_request("initialize", initialize_params, true)
                .await?,
        );

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

        self.files_content
            .insert(file.to_string(), contents.to_string());
        self.files_versions.insert(file.to_string(), 1);
        self.send_notification("textDocument/didOpen", params, false)
            .await?;

        Ok(())
    }

    async fn did_change(&mut self, file: &str, contents: &str) -> Result<(), LspError> {
        log!("[lsp] did_change file: {}", file);
        self.request_diagnostics(file).await?;

        // Get or create version for this file
        let version = self.files_versions.entry(file.to_string()).or_insert(0);
        *version += 1;

        // Get the file URI
        let uri = format!("file://{}", Path::new(file).absolutize()?.to_string_lossy());

        // Determine sync kind from server capabilities
        let sync_kind = self
            .server_capabilities
            .as_ref()
            .and_then(|caps| caps.text_document_sync.as_ref())
            .and_then(|sync| match sync.change {
                Some(TextDocumentSyncKind::Full) | None => Some(TextDocumentSyncKind::Full),
                Some(TextDocumentSyncKind::Incremental) => Some(TextDocumentSyncKind::Incremental),
                _ => None,
            })
            .unwrap_or(TextDocumentSyncKind::Full);

        // Prepare the content changes based on sync kind
        let content_changes = match sync_kind {
            TextDocumentSyncKind::Full => {
                // Full sync: send entire content
                vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: contents.to_string(),
                }]
            }
            TextDocumentSyncKind::Incremental => {
                // Get the old content or empty string if it's the first change
                let old_content = self
                    .files_content
                    .get(file)
                    .map(String::as_str)
                    .unwrap_or("");

                // Calculate actual changes
                Self::calculate_changes(old_content, contents)
            }
            _ => return Ok(()),
        };

        log!(
            "sync_kind: {:?} content_changes: {:#?}",
            sync_kind,
            content_changes
        );

        // Update stored content
        self.files_content
            .insert(file.to_string(), contents.to_string());

        let params = json!({
            "textDocument": {
                "uri": uri,
                "version": version,
            },
            "contentChanges": content_changes
        });

        log!("[lsp] did_change content_changes: {:#?}", content_changes);

        // Log params without the actual content for debugging
        log!(
            "[lsp] did_change file: {} sync_kind: {:?} version: {} changes: {}",
            uri,
            sync_kind,
            version,
            content_changes.len()
        );

        self.send_notification("textDocument/didChange", params, false)
            .await?;

        Ok(())
    }

    // async fn did_change(&mut self, file: &str, contents: &str) -> Result<(), LspError> {
    //     log!("[lsp] did_change file: {}", file);
    //     let version = self.files_versions.entry(file.to_string()).or_insert(0);
    //     *version += 1;
    //
    //     let params = json!({
    //         "textDocument": {
    //             "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
    //             "version": version,
    //         },
    //         "contentChanges": [
    //             {
    //                 "text": contents,
    //             }
    //         ]
    //     });
    //
    //     // log params without the contents
    //     log!(
    //         "[lsp] did_change file: {} params: {}",
    //         file,
    //         json!({
    //             "textDocument": {
    //                 "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
    //                 "version": version,
    //             },
    //             "contentChanges": [
    //                 {
    //                     "text": contents,
    //                 }
    //             ]
    //         })
    //     );
    //
    //     self.send_notification("textDocument/didChange", params, false)
    //         .await?;
    //
    //     Ok(())
    // }

    async fn will_save(&mut self, file: &str) -> Result<(), LspError> {
        log!("will_save file: {}", file);

        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
            },
            "reason": 1,
        });

        self.send_notification("textDocument/willSave", params, false)
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

        self.send_request("textDocument/hover", params, false).await
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

        self.send_request("textDocument/definition", params, false)
            .await
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

        self.send_request("textDocument/completion", params, false)
            .await
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

        self.send_request("textDocument/formatting", params, false)
            .await
    }

    async fn document_symbols(&mut self, file: &str) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
            }
        });

        self.send_request("textDocument/documentSymbol", params, false)
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

        self.send_request("textDocument/codeAction", params, false)
            .await
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

        self.send_request("textDocument/documentHighlight", params, false)
            .await
    }

    async fn document_link(&mut self, file: &str) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
            }
        });

        self.send_request("textDocument/documentLink", params, false)
            .await
    }

    async fn document_color(&mut self, file: &str) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
            }
        });

        self.send_request("textDocument/documentColor", params, false)
            .await
    }

    async fn folding_range(&mut self, file: &str) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
            }
        });

        self.send_request("textDocument/foldingRange", params, false)
            .await
    }

    async fn workspace_symbol(&mut self, query: &str) -> Result<i64, LspError> {
        let params = json!({
            "query": query
        });

        self.send_request("workspace/symbol", params, false).await
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

        self.send_request("textDocument/prepareCallHierarchy", params, false)
            .await
    }

    async fn semantic_tokens_full(&mut self, file: &str) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
            }
        });

        self.send_request("textDocument/semanticTokens/full", params, false)
            .await
    }

    async fn inlay_hint(&mut self, file: &str, range: Range) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
            },
            "range": range
        });

        self.send_request("textDocument/inlayHint", params, false)
            .await
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

        self.send_request("textDocument/signatureHelp", params, false)
            .await
    }

    fn get_server_capabilities(&self) -> Option<&ServerCapabilities> {
        self.server_capabilities.as_ref()
    }

    async fn shutdown(&mut self) -> Result<(), LspError> {
        // Send shutdown request and wait for response
        self.send_request("shutdown", serde_json::Value::Null, true)
            .await?;

        // Send exit notification
        self.send_notification("exit", serde_json::Value::Null, true)
            .await?;

        // Take ownership of child process and response channel
        let mut child = std::mem::take(&mut self.child).unwrap();

        // Create a timeout future
        let timeout_future = tokio::time::sleep(std::time::Duration::from_secs(5));

        // Wait for either timeout or process exit
        tokio::select! {
            _ = timeout_future => {
                log!("[lsp] shutdown timeout reached, forcing exit");
                // Kill the process if it hasn't exited
                let _ = child.kill().await;
            }
            status = child.wait() => {
                match status {
                    Ok(status) => {
                        if !status.success() {
                            log!("[lsp] rust-analyzer exited with status: {}", status);
                        }
                    }
                    Err(e) => {
                        log!("[lsp] error waiting for rust-analyzer to exit: {}", e);
                    }
                }
            }
        }

        Ok(())
    }

    // async fn shutdown(&mut self) -> Result<(), LspError> {
    //     // Send shutdown request
    //     log!("      lsp request shutdown");
    //     self.send_request("shutdown", serde_json::Value::Null, true)
    //         .await?;
    //
    //     // Send exit notification
    //     log!("      lsp notify shutdown");
    //     self.send_notification("exit", serde_json::Value::Null, true)
    //         .await?;
    //
    //     let mut child = std::mem::take(&mut self.child).unwrap();
    //
    //     // Wait for the process to exit
    //     log!("      waiting for rust-analyzer to exit");
    //     match child.wait().await {
    //         Ok(status) => {
    //             if !status.success() {
    //                 log!("[lsp] rust-analyzer exited with status: {}", status);
    //             }
    //         }
    //         Err(e) => {
    //             log!("[lsp] error waiting for rust-analyzer to exit: {}", e);
    //         }
    //     }
    //
    //     Ok(())
    // }
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
    stdin.flush().await?;

    Ok(())
}

#[cfg(test)]
mod test {
    use crate::lsp::ParsedNotification;

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

        let ParsedNotification::PublishDiagnostics(msg) = msg else {
            panic!("Expected PublishDiagnostics, got {:?}", msg);
        };

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

        let ParsedNotification::PublishDiagnostics(msg) = msg else {
            panic!("Expected PublishDiagnostics, got {:?}", msg);
        };

        assert_eq!(msg.diagnostics.len(), 4);
        let diag = &msg.diagnostics[0];
        let code = diag.code.as_ref().unwrap();
        assert_eq!(code.as_string(), "unused_imports");
    }

    // #[tokio::test]
    // async fn test_parse_initialize_result() {
    //     let response = json!({
    //         "capabilities": {
    //             "position_encoding": "utf-16",
    //             "text_document_sync": {
    //                 "open_close": true,
    //                 "change": 2,
    //                 "save": {}
    //             },
    //             "completion_provider": {
    //                 "trigger_characters": [":", ".", "'", "("],
    //                 "completion_item": {
    //                     "label_details_support": false
    //                 }
    //             },
    //             "hover_provider": true,
    //             "signature_help_provider": {
    //                 "trigger_characters": ["(", ",", "<"]
    //             },
    //             "definition_provider": true,
    //             "type_definition_provider": true,
    //             "implementation_provider": true,
    //             "references_provider": true,
    //             "document_highlight_provider": true,
    //             "document_symbol_provider": true,
    //             "workspace_symbol_provider": true,
    //             "code_action_provider": {
    //                 "code_action_kinds": ["", "quickfix", "refactor"],
    //                 "resolve_provider": true
    //             },
    //             "document_formatting_provider": true,
    //             "rename_provider": {
    //                 "prepare_provider": true
    //             },
    //             "folding_range_provider": true,
    //             "workspace": {
    //                 "workspace_folders": {
    //                     "supported": true,
    //                     "change_notifications": true
    //                 }
    //             }
    //         },
    //         "server_info": {
    //             "name": "rust-analyzer",
    //             "version": "1.83.0 (90b35a62 2024-11-26)"
    //         }
    //     });
    //
    //     let init_result: InitializeResult =
    //         serde_json::from_value(response).expect("Failed to parse initialize result");
    //
    //     assert!(init_result.capabilities.text_document_sync.is_some());
    //     assert!(init_result.capabilities.completion_provider.is_some());
    //     assert!(matches!(
    //         init_result.capabilities.hover_provider,
    //         Some(HoverProviderCapability::Simple(true))
    //     ));
    //     assert!(init_result.server_info.is_some());
    //
    //     let server_info = init_result.server_info.unwrap();
    //     assert_eq!(server_info.name, "rust-analyzer");
    //     assert_eq!(server_info.version.unwrap(), "1.83.0 (90b35a62 2024-11-26)");
    // }

    #[test]
    fn test_parse_completion_response() {
        let json_str = include_str!("../fixtures/lsp-completion-response.json");
        let json = serde_json::from_str::<CompletionResponse>(json_str).unwrap();

        assert!(json.is_incomplete);
        assert_eq!(json.items.len(), 225);
    }

    #[test]
    fn test_parse_initialize() {
        let params = get_client_capabilities("file://uri".to_string());
        let json = serde_json::to_value(params).unwrap();
        println!("json: {}", serde_json::to_string_pretty(&json).unwrap());
    }
}
