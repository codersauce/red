use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use path_absolutize::*;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter},
    process::{ChildStdin, Command as TokioCommand},
    sync::mpsc::{self, error::TryRecvError},
};

use super::{
    capabilities::get_client_capabilities_with_options, InboundMessage, LspClient, OutboundMessage,
    ResponseError,
};
use crate::config::LanguageServerConfig;
use crate::lsp::{
    parse_notification, types::*, Notification, NotificationRequest, Request, ResponseMessage,
};
use crate::{log, lsp::LspError};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Idle time after the last document change before diagnostics are
/// requested. Typing produces one didChange per keystroke; requesting
/// diagnostics for each is wasted server work.
const DIAGNOSTICS_DEBOUNCE: Duration = Duration::from_millis(250);

fn bytecount_newlines(text: &str) -> usize {
    text.as_bytes().iter().filter(|&&b| b == b'\n').count()
}

fn file_uri(path: impl AsRef<Path>) -> Result<String, LspError> {
    Ok(format!(
        "file://{}",
        path.as_ref().absolutize()?.to_string_lossy()
    ))
}

fn workspace_uri(path: impl AsRef<Path>) -> Result<String, LspError> {
    file_uri(path)
}

fn did_open_params(
    file: &str,
    contents: &str,
    language_id: &str,
) -> Result<serde_json::Value, LspError> {
    Ok(json!({
        "textDocument": {
            "uri": file_uri(file)?,
            "languageId": language_id,
            "version": 1,
            "text": contents,
        }
    }))
}

async fn spawn_lsp_process(
    config: &LanguageServerConfig,
) -> Result<tokio::process::Child, LspError> {
    let mut command = TokioCommand::new(&config.command);
    command
        .args(&config.args)
        .envs(&config.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    Ok(command.spawn()?)
}

impl RealLspClient {
    pub async fn start(
        config: LanguageServerConfig,
        workspace_root: PathBuf,
    ) -> Result<RealLspClient, LspError> {
        let mut child = spawn_lsp_process(&config).await?;
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

        // Language servers commonly write operational logs to stderr. Keep
        // those in the log file, and only surface panic/fatal-looking lines.
        let rtx = response_tx.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            while let Ok(read) = reader.read_line(&mut line).await {
                if read == 0 {
                    break;
                }

                let message = line.trim_end_matches(['\r', '\n']).to_string();
                line.clear();

                if !message.is_empty() {
                    log!("[lsp] incoming stderr: {:?}", message);
                }

                if should_surface_server_stderr(&message) {
                    match rtx
                        .send(InboundMessage::ProcessingError(LspError::ServerError(
                            message,
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
            pending_diagnostics: None,
            server_capabilities: None,
            child: Some(child),
            config,
            workspace_root,
        })
    }
}

fn should_surface_server_stderr(line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() {
        return false;
    }

    let lower = line.to_ascii_lowercase();
    lower.starts_with("fatal")
        || lower.starts_with("[fatal]")
        || lower.contains("panicked")
        || lower.contains("thread '")
}

async fn process_lsp_message(
    body: &[u8],
    rtx: &mpsc::Sender<InboundMessage>,
) -> Result<(), LspError> {
    let body = String::from_utf8_lossy(body);
    let res = serde_json::from_str::<serde_json::Value>(&body).map_err(LspError::JsonError)?;

    if let Some(error) = res.get("error") {
        let id = res.get("id").and_then(Value::as_i64);
        let code = error["code"].as_i64().unwrap();
        let message = error["message"].as_str().unwrap().to_string();
        let data = error.get("data").cloned();

        rtx.send(InboundMessage::Error(ResponseError {
            id,
            code,
            message,
            data,
        }))
        .await
        .map_err(|e| LspError::ChannelInboundError(e.to_string()))?;

        return Ok(());
    }

    // Responses have an id and no method. Server-to-client requests also have
    // an id, but must not be matched against our pending client requests.
    if let Some(id) = res.get("id").filter(|_| res.get("method").is_none()) {
        let id = id.as_i64().unwrap();
        let result = res["result"].clone();

        // Avoid serializing the (possibly very large) result just to log it.
        log!("[lsp] incoming response: id={}", id);

        rtx.send(InboundMessage::Message(ResponseMessage {
            id,
            result,
            request: None,
        }))
        .await
        .map_err(|e| LspError::ChannelInboundError(e.to_string()))?;
    } else if let Some(method) = res.get("method").and_then(Value::as_str) {
        // if there's a method, it's a notification or a server-to-client request
        let method = method.to_string();
        let params = res.get("params").cloned().unwrap_or(Value::Null);

        if let Some(id) = res.get("id").and_then(Value::as_i64) {
            log!("[lsp] incoming request: id={}, method={}", id, method);
        } else {
            log!("[lsp] incoming notification: method={}", method);
        }

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
    } else {
        log!("[lsp] unknown message: {}", res);
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
    /// Debounced diagnostics request: (uri, due time).
    pending_diagnostics: Option<(String, Instant)>,
    child: Option<tokio::process::Child>,
    config: LanguageServerConfig,
    workspace_root: PathBuf,
}

impl RealLspClient {
    fn can_request_diagnostics(&self) -> bool {
        self.server_capabilities
            .as_ref()
            .map(|caps| caps.diagnostic_provider.is_some())
            .unwrap_or(false)
    }

    fn position_at_byte(text: &str, byte_offset: usize) -> Position {
        let before = &text[..byte_offset];
        let line = bytecount_newlines(before);
        let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let character = before[line_start..].chars().map(char::len_utf16).sum();

        Position { line, character }
    }

    /// Computes the minimal single-range change between two versions of a
    /// document by trimming the common prefix and suffix.
    ///
    /// This runs on every keystroke with the full old and new buffer
    /// contents, so it must stay allocation-free until the (small) changed
    /// region is extracted. A general diff (Myers) here cost ~10ms per
    /// keystroke on a 400KB file; this is microseconds.
    fn calculate_changes(old_text: &str, new_text: &str) -> Vec<TextDocumentContentChangeEvent> {
        if old_text == new_text {
            return Vec::new();
        }

        // Common prefix, backed up to a char boundary.
        let mut prefix = old_text
            .as_bytes()
            .iter()
            .zip(new_text.as_bytes())
            .take_while(|(a, b)| a == b)
            .count();
        while !old_text.is_char_boundary(prefix) {
            prefix -= 1;
        }

        // Common suffix of the remainders, backed up to char boundaries.
        let old_rest = &old_text[prefix..];
        let new_rest = &new_text[prefix..];
        let mut suffix = old_rest
            .as_bytes()
            .iter()
            .rev()
            .zip(new_rest.as_bytes().iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        while !old_rest.is_char_boundary(old_rest.len() - suffix)
            || !new_rest.is_char_boundary(new_rest.len() - suffix)
        {
            suffix -= 1;
        }

        let old_end = old_text.len() - suffix;
        let new_end = new_text.len() - suffix;

        vec![TextDocumentContentChangeEvent {
            range: Some(Range {
                start: Self::position_at_byte(old_text, prefix),
                end: Self::position_at_byte(old_text, old_end),
            }),
            range_length: None,
            text: new_text[prefix..new_end].to_string(),
        }]
    }

    pub async fn did_open_with_language_id(
        &mut self,
        file: &str,
        contents: &str,
        language_id: &str,
    ) -> Result<(), LspError> {
        log!("[lsp] did_open file: {} language_id: {}", file, language_id);
        let params = did_open_params(file, contents, language_id)?;

        self.files_content
            .insert(file.to_string(), contents.to_string());
        self.files_versions.insert(file.to_string(), 1);
        <Self as LspClient>::send_notification(self, "textDocument/didOpen", params, false).await?;

        Ok(())
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
        trigger_character: Option<char>,
    ) -> Result<i64, LspError> {
        let context = if let Some(trigger_character) = trigger_character {
            json!({
                "triggerKind": 2,
                "triggerCharacter": trigger_character.to_string(),
            })
        } else {
            json!({
                "triggerKind": 1,
            })
        };

        let params = json!({
            "textDocument": {
                "uri": file_uri,
            },
            "position": {
                "line": line,
                "character": character,
            },
            "context": context,
        });

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

        Ok(Some(
            self.send_request("textDocument/diagnostic", params, false)
                .await?,
        ))
    }

    async fn recv_response(
        &mut self,
    ) -> Result<Option<(InboundMessage, Option<String>)>, LspError> {
        // Send the debounced diagnostics request once the document has been
        // quiet long enough. This is polled every editor tick.
        if let Some((_, due)) = &self.pending_diagnostics {
            if Instant::now() >= *due {
                let (uri, _) = self.pending_diagnostics.take().unwrap();
                self.request_diagnostics(&uri).await?;
            }
        }

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
                    InboundMessage::RequestError {
                        id,
                        error: LspError::RequestTimeout(now.duration_since(request.timestamp)),
                    },
                    Some(request.method),
                )));
            }
        }

        match self.response_rx.try_recv() {
            Ok(mut msg) => {
                match &mut msg {
                    InboundMessage::Message(msg) => {
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
                    InboundMessage::Error(error) => {
                        if let Some(id) = error.id {
                            if let Some(request) = self.pending_responses.get(&id) {
                                let method = request.method.clone();
                                log!(
                                    "[lsp] rcv_error: id={} method={} code={} message={}",
                                    id,
                                    method,
                                    error.code,
                                    error.message
                                );

                                if !error.is_retrigger_cancellation() {
                                    self.pending_responses.remove(&id);
                                }

                                return Ok(Some((msg, Some(method))));
                            }
                        }
                    }
                    _ => {}
                }
                Ok(Some((msg, None)))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(err) => Err(LspError::ProtocolError(err.to_string())),
        }
    }

    async fn initialize(&mut self) -> Result<(), LspError> {
        let workspace_name = self
            .config
            .workspace_name
            .clone()
            .or_else(|| {
                self.workspace_root
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| "workspace".to_string());
        let initialize_params = get_client_capabilities_with_options(
            workspace_uri(&self.workspace_root)?,
            workspace_name,
            self.config
                .initialization_options
                .clone()
                .unwrap_or(serde_json::Value::Null),
        );

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
        let language_id = self.config.language_id.clone();
        self.did_open_with_language_id(file, contents, &language_id)
            .await
    }

    async fn did_change(&mut self, file: &str, contents: String) -> Result<(), LspError> {
        let uri = format!("file://{}", Path::new(file).absolutize()?.to_string_lossy());
        // Diagnostics are debounced: typing produces a didChange per
        // keystroke, and requesting diagnostics for every one of them floods
        // the server. The request is sent from `recv_response` once the
        // document has been quiet for DIAGNOSTICS_DEBOUNCE.
        self.pending_diagnostics = Some((uri.clone(), Instant::now() + DIAGNOSTICS_DEBOUNCE));

        // Get or create version for this file
        let version = self.files_versions.entry(file.to_string()).or_insert(0);
        *version += 1;

        // Determine sync kind from server capabilities
        let sync_kind = self
            .server_capabilities
            .as_ref()
            .and_then(|caps| caps.text_document_sync.as_ref())
            .and_then(|sync| match sync.change_kind() {
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
                    text: contents.clone(),
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
                Self::calculate_changes(old_content, &contents)
            }
            _ => return Ok(()),
        };

        // Update stored content, reusing the caller's buffer copy.
        self.files_content.insert(file.to_string(), contents);

        let params = json!({
            "textDocument": {
                "uri": uri,
                "version": version,
            },
            "contentChanges": content_changes
        });

        log!(
            "[lsp] did_change file: {} sync_kind: {:?} changes: {}",
            uri,
            sync_kind,
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

    async fn references(
        &mut self,
        file: &str,
        x: usize,
        y: usize,
        include_declaration: bool,
    ) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": format!("file://{}", Path::new(file).absolutize()?.to_string_lossy()),
            },
            "position": {
                "line": y,
                "character": x,
            },
            "context": {
                "includeDeclaration": include_declaration,
            },
        });

        self.send_request("textDocument/references", params, false)
            .await
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
                            log!("[lsp] {} exited with status: {}", self.config.command, status);
                        }
                    }
                    Err(e) => {
                        log!("[lsp] error waiting for {} to exit: {}", self.config.command, e);
                    }
                }
            }
        }

        Ok(())
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
    stdin.flush().await?;

    Ok(())
}

#[cfg(test)]
mod test {
    use std::time::Instant;

    use crate::config::default_language_servers;
    use crate::lsp::{get_client_capabilities, ParsedNotification};

    use super::*;

    #[tokio::test]
    async fn test_start_real_lsp() {
        if std::env::var_os("RED_RUN_REAL_LSP_TESTS").is_none() {
            return;
        }

        let config = default_language_servers()
            .remove("rust")
            .expect("default Rust LSP config must exist");
        let mut client = RealLspClient::start(config, std::env::current_dir().unwrap())
            .await
            .unwrap();
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

    #[tokio::test]
    async fn retrigger_cancellation_keeps_pending_completion_request() {
        let (request_tx, _request_rx) = mpsc::channel(1);
        let (response_tx, response_rx) = mpsc::channel(4);
        let request = Request {
            id: 42,
            method: "textDocument/completion".to_string(),
            params: json!({}),
            timestamp: Instant::now(),
        };
        let mut client = RealLspClient {
            request_tx,
            response_rx,
            files_versions: HashMap::new(),
            files_content: HashMap::new(),
            pending_responses: HashMap::from([(request.id, request)]),
            initialize_id: None,
            initialized: true,
            pending_diagnostics: None,
            pending_messages: Vec::new(),
            server_capabilities: None,
            child: None,
            config: default_language_servers()
                .remove("rust")
                .expect("default Rust LSP config must exist"),
            workspace_root: std::env::current_dir().unwrap(),
        };

        response_tx
            .send(InboundMessage::Error(ResponseError {
                id: Some(42),
                code: -32802,
                message: "server cancelled the request".to_string(),
                data: Some(json!({ "retriggerRequest": true })),
            }))
            .await
            .unwrap();
        response_tx
            .send(InboundMessage::Message(ResponseMessage {
                id: 42,
                result: json!({
                    "isIncomplete": false,
                    "items": [{ "label": "add_extension" }]
                }),
                request: None,
            }))
            .await
            .unwrap();

        let Some((first_message, first_method)) = client.recv_response().await.unwrap() else {
            panic!("expected retrigger cancellation response");
        };
        assert_eq!(first_method.as_deref(), Some("textDocument/completion"));
        assert!(matches!(first_message, InboundMessage::Error(_)));
        assert!(client.pending_responses.contains_key(&42));

        let Some((second_message, second_method)) = client.recv_response().await.unwrap() else {
            panic!("expected completion response");
        };
        assert_eq!(second_method.as_deref(), Some("textDocument/completion"));
        let InboundMessage::Message(response) = second_message else {
            panic!("expected completion message");
        };
        assert_eq!(response.id, 42);
        assert_eq!(
            response
                .request
                .as_ref()
                .map(|request| request.method.as_str()),
            Some("textDocument/completion")
        );
        assert!(!client.pending_responses.contains_key(&42));
    }

    #[tokio::test]
    async fn process_lsp_message_preserves_error_response_id() {
        let (response_tx, mut response_rx) = mpsc::channel(1);
        let body = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 42,
            "error": {
                "code": -32802,
                "message": "server cancelled the request",
                "data": { "retriggerRequest": true }
            }
        }))
        .unwrap();

        process_lsp_message(&body, &response_tx).await.unwrap();

        let Some(InboundMessage::Error(error)) = response_rx.recv().await else {
            panic!("expected error response");
        };
        assert_eq!(error.id, Some(42));
        assert!(error.is_retrigger_cancellation());
    }

    #[tokio::test]
    async fn server_request_id_does_not_complete_pending_client_request() {
        let (request_tx, _request_rx) = mpsc::channel(1);
        let (response_tx, response_rx) = mpsc::channel(4);
        let request = Request {
            id: 31,
            method: "textDocument/completion".to_string(),
            params: json!({}),
            timestamp: Instant::now(),
        };
        let mut client = RealLspClient {
            request_tx,
            response_rx,
            files_versions: HashMap::new(),
            files_content: HashMap::new(),
            pending_responses: HashMap::from([(request.id, request)]),
            initialize_id: None,
            initialized: true,
            pending_diagnostics: None,
            pending_messages: Vec::new(),
            server_capabilities: None,
            child: None,
            config: default_language_servers()
                .remove("rust")
                .expect("default Rust LSP config must exist"),
            workspace_root: std::env::current_dir().unwrap(),
        };

        let server_request = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 31,
            "method": "workspace/configuration",
            "params": { "items": [] }
        }))
        .unwrap();
        process_lsp_message(&server_request, &response_tx)
            .await
            .unwrap();

        let completion_response = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 31,
            "result": {
                "isIncomplete": true,
                "items": [{ "label": "symlink_metadata" }]
            }
        }))
        .unwrap();
        process_lsp_message(&completion_response, &response_tx)
            .await
            .unwrap();

        let Some((first_message, first_method)) = client.recv_response().await.unwrap() else {
            panic!("expected server request");
        };
        assert!(matches!(
            first_message,
            InboundMessage::UnknownNotification(_)
        ));
        assert_eq!(first_method, None);
        assert!(client.pending_responses.contains_key(&31));

        let Some((second_message, second_method)) = client.recv_response().await.unwrap() else {
            panic!("expected completion response");
        };
        assert_eq!(second_method.as_deref(), Some("textDocument/completion"));
        assert!(matches!(second_message, InboundMessage::Message(_)));
        assert!(!client.pending_responses.contains_key(&31));
    }

    fn single_change(old: &str, new: &str) -> TextDocumentContentChangeEvent {
        let mut changes = RealLspClient::calculate_changes(old, new);
        assert_eq!(
            changes.len(),
            1,
            "expected one change for {old:?} -> {new:?}"
        );
        changes.pop().unwrap()
    }

    fn apply_change(old: &str, change: &TextDocumentContentChangeEvent) -> String {
        let range = change.range.as_ref().expect("range change");
        let mut offset = 0;
        let mut start = None;
        let mut end = None;
        let mut line = 0;
        let mut character = 0;
        for (i, c) in old.char_indices() {
            if line == range.start.line && character == range.start.character && start.is_none() {
                start = Some(i);
            }
            if line == range.end.line && character == range.end.character && end.is_none() {
                end = Some(i);
            }
            if c == '\n' {
                line += 1;
                character = 0;
            } else {
                character += c.len_utf16();
            }
            offset = i + c.len_utf8();
        }
        let start = start.unwrap_or(offset);
        let end = end.unwrap_or(offset);
        format!("{}{}{}", &old[..start], change.text, &old[end..])
    }

    #[test]
    fn test_calculate_changes_roundtrip() {
        let cases = [
            ("hello world", "hello brave world"), // insert
            ("hello brave world", "hello world"), // delete
            ("hello world", "hello earth"),       // replace
            (
                "line one\nline two\nline three",
                "line one\nline 2\nline three",
            ), // mid-line
            ("fn main() {}", "fn main() {}\n"),   // append
            ("", "new content"),                  // from empty
            ("ab", "aXb"),                        // insert between equal chars
            ("aa", "aaa"),                        // ambiguous repeat
            ("héllo wörld", "héllo wørld"),       // multi-byte
            ("a👋b", "a👋👋b"),                   // emoji insert
        ];
        for (old, new) in cases {
            let change = single_change(old, new);
            assert_eq!(
                apply_change(old, &change),
                new,
                "applying change {change:?} to {old:?} should produce {new:?}"
            );
        }
    }

    #[test]
    fn test_calculate_changes_equal_input_is_empty() {
        assert!(RealLspClient::calculate_changes("same", "same").is_empty());
        assert!(RealLspClient::calculate_changes("", "").is_empty());
    }

    #[test]
    fn test_calculate_changes_positions_are_line_relative() {
        let old = "first\nsecond\nthird";
        let new = "first\nsecXond\nthird";
        let change = single_change(old, new);
        let range = change.range.unwrap();
        assert_eq!(range.start.line, 1);
        assert_eq!(range.start.character, 3);
        assert_eq!(range.end.line, 1);
        assert_eq!(change.text, "X");
    }

    #[test]
    fn test_calculate_changes_positions_use_utf16_units() {
        let change = single_change("😀 target", "😀 Xtarget");
        let range = change.range.unwrap();

        assert_eq!(range.start.line, 0);
        assert_eq!(range.start.character, 3);
        assert_eq!(range.end.line, 0);
        assert_eq!(range.end.character, 3);
        assert_eq!(change.text, "X");
    }

    #[test]
    fn test_taplo_info_stderr_is_not_surface_error() {
        assert!(!should_surface_server_stderr(
            r#"INFO taplo: registered request handler method="initialize""#
        ));
        assert!(!should_surface_server_stderr(
            r#"WARN taplo: workspace fallback in use"#
        ));
        assert!(!should_surface_server_stderr(
            "ERROR taplo:initialize:initialize: failed to add schemas from catalog"
        ));
    }

    #[test]
    fn test_fatal_stderr_is_surface_error() {
        assert!(should_surface_server_stderr(
            "FATAL language server failed to start"
        ));
        assert!(should_surface_server_stderr(
            "thread 'main' panicked at src/main.rs:1"
        ));
    }

    #[test]
    fn test_initialize_result_accepts_text_document_sync_kind() {
        let response = json!({
            "capabilities": {
                "textDocumentSync": 1,
                "semanticTokensProvider": {
                    "legend": {
                        "tokenTypes": [],
                        "tokenModifiers": []
                    },
                    "range": true,
                    "full": true
                }
            },
            "serverInfo": {
                "name": "taplo"
            }
        });

        let init_result: InitializeResult = serde_json::from_value(response).unwrap();
        let sync = init_result.capabilities.text_document_sync.unwrap();

        assert!(matches!(
            sync.change_kind(),
            Some(TextDocumentSyncKind::Full)
        ));
    }

    #[test]
    fn test_initialize_result_accepts_simple_inlay_hint_provider() {
        let response = json!({
            "capabilities": {
                "inlayHintProvider": true
            }
        });

        let init_result: InitializeResult = serde_json::from_value(response).unwrap();

        assert!(matches!(
            init_result.capabilities.inlay_hint_provider,
            Some(InlayHintProviderCapability::Simple(true))
        ));
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

        assert!(json.is_incomplete());
        assert_eq!(json.items().len(), 225);
    }

    #[test]
    fn test_parse_completion_response_array() {
        let json = serde_json::json!([
            {
                "label": "alpha",
                "kind": 1
            }
        ]);
        let response = serde_json::from_value::<CompletionResponse>(json).unwrap();

        assert!(!response.is_incomplete());
        assert_eq!(response.items().len(), 1);
    }

    #[test]
    fn test_parse_initialize() {
        let params = get_client_capabilities("file://uri".to_string());
        let json = serde_json::to_value(params).unwrap();
        println!("json: {}", serde_json::to_string_pretty(&json).unwrap());
    }

    #[test]
    fn test_did_open_params_uses_configured_language_id() {
        let params = did_open_params("main.py", "print('hello')", "python").unwrap();
        assert_eq!(params["textDocument"]["languageId"], "python");
        assert_eq!(params["textDocument"]["text"], "print('hello')");
    }
}
