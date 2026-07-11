use std::{
    collections::HashMap,
    path::PathBuf,
    process::Stdio,
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter},
    process::{ChildStdin, Command as TokioCommand},
    sync::mpsc::{self, error::TryRecvError},
};

use super::{
    capabilities::get_client_capabilities_with_options, file_uri, InboundMessage, LspClient,
    OutboundMessage, ResponseError, ServerRequest, ServerResponse,
};
use crate::config::LanguageServerConfig;
use crate::lsp::{
    parse_notification, types::*, Notification, NotificationRequest, Request, ResponseMessage,
};
use crate::{log, lsp::LspError};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_LSP_FRAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_LSP_HEADER_BYTES: usize = 16 * 1024;
const MAX_LSP_STDERR_LINE_BYTES: usize = 64 * 1024;
const MAX_PENDING_LSP_MESSAGES: usize = 512;
const MAX_PENDING_LSP_BYTES: usize = 16 * 1024 * 1024;

/// Idle time after the last document change before diagnostics are
/// requested. Typing produces one didChange per keystroke; requesting
/// diagnostics for each is wasted server work.
const DIAGNOSTICS_DEBOUNCE: Duration = Duration::from_millis(250);

fn bytecount_newlines(text: &str) -> usize {
    text.as_bytes().iter().filter(|&&b| b == b'\n').count()
}

fn json_value_size(value: &Value) -> usize {
    match value {
        Value::Null => 4,
        Value::Bool(_) => 5,
        Value::Number(_) => 32,
        Value::String(value) => value.len().saturating_mul(6).saturating_add(2),
        Value::Array(values) => values.iter().fold(2usize, |size, value| {
            size.saturating_add(json_value_size(value))
                .saturating_add(1)
        }),
        Value::Object(values) => values.iter().fold(2usize, |size, (key, value)| {
            size.saturating_add(key.len().saturating_mul(6))
                .saturating_add(json_value_size(value))
                .saturating_add(4)
        }),
    }
}

fn outbound_message_size(message: &OutboundMessage) -> usize {
    match message {
        OutboundMessage::Request(request) => request
            .method
            .len()
            .saturating_add(json_value_size(&request.params))
            .saturating_add(64),
        OutboundMessage::Notification(notification) => notification
            .method
            .len()
            .saturating_add(json_value_size(&notification.params))
            .saturating_add(48),
        OutboundMessage::Response(response) => json_value_size(&response.id)
            .saturating_add(response.result.as_ref().map_or(0, json_value_size))
            .saturating_add(response.error.as_ref().map_or(0, json_value_size))
            .saturating_add(64),
    }
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
    #[cfg(test)]
    pub(super) fn with_test_channels(
        request_tx: mpsc::Sender<OutboundMessage>,
        response_rx: mpsc::Receiver<InboundMessage>,
        config: LanguageServerConfig,
        workspace_root: PathBuf,
    ) -> Self {
        Self {
            request_tx,
            response_rx,
            files_versions: HashMap::new(),
            files_content: HashMap::new(),
            pending_responses: HashMap::new(),
            initialize_id: None,
            initialized: true,
            initialize_failed: false,
            pending_messages: Vec::new(),
            pending_message_bytes: 0,
            failed_pending_requests: Vec::new(),
            server_capabilities: None,
            pending_diagnostics: HashMap::new(),
            child: None,
            config,
            workspace_root,
        }
    }

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
                            let _ = rtx.send(InboundMessage::ProcessingError(err)).await;
                        }
                    }
                    OutboundMessage::Notification(req) => {
                        if let Err(err) = lsp_send_notification(&mut stdin, &req).await {
                            let _ = rtx.send(InboundMessage::ProcessingError(err)).await;
                        }
                    }
                    OutboundMessage::Response(response) => {
                        if let Err(err) = lsp_send_response(&mut stdin, &response).await {
                            let _ = rtx.send(InboundMessage::ProcessingError(err)).await;
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
                let body = match read_lsp_frame(&mut reader).await {
                    Ok(Some(body)) => body,
                    Ok(None) => break,
                    Err(error) => {
                        log!("[lsp] invalid stdout frame: {error}");
                        let _ = rtx.send(InboundMessage::ProcessingError(error)).await;
                        break;
                    }
                };

                if let Err(error) = process_lsp_message(&body, &rtx).await {
                    log!("[lsp] error processing message: {error}");
                    let _ = rtx.send(InboundMessage::ProcessingError(error)).await;
                    break;
                }
            }
        });

        // Language servers commonly write operational logs to stderr. Keep
        // those in the log file, and only surface panic/fatal-looking lines.
        let rtx = response_tx.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            loop {
                let line = match read_bounded_line(&mut reader, MAX_LSP_STDERR_LINE_BYTES).await {
                    Ok(Some(line)) => line,
                    Ok(None) => break,
                    Err(error) => {
                        log!("[lsp] invalid stderr line: {error}");
                        let _ = rtx.send(InboundMessage::ProcessingError(error)).await;
                        break;
                    }
                };
                let message = String::from_utf8_lossy(&line)
                    .trim_end_matches(['\r', '\n'])
                    .to_string();

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
            pending_message_bytes: 0,
            failed_pending_requests: Vec::new(),
            initialize_id: None,
            initialized: false,
            initialize_failed: false,
            pending_diagnostics: HashMap::new(),
            server_capabilities: None,
            child: Some(child),
            config,
            workspace_root,
        })
    }
}

async fn read_lsp_frame(
    reader: &mut (impl AsyncBufRead + Unpin),
) -> Result<Option<Vec<u8>>, LspError> {
    let mut header_bytes = 0usize;
    let mut content_length = None;

    loop {
        let Some(line) = read_bounded_line(reader, MAX_LSP_HEADER_BYTES).await? else {
            if header_bytes == 0 {
                return Ok(None);
            }
            return Err(LspError::ProtocolError(
                "LSP frame ended before its header separator".to_string(),
            ));
        };
        header_bytes = header_bytes.checked_add(line.len()).ok_or_else(|| {
            LspError::ProtocolError("LSP frame header size overflowed".to_string())
        })?;
        if header_bytes > MAX_LSP_HEADER_BYTES {
            return Err(LspError::ProtocolError(format!(
                "LSP frame header exceeds {MAX_LSP_HEADER_BYTES} bytes"
            )));
        }

        let line = std::str::from_utf8(&line).map_err(|_| {
            LspError::ProtocolError("LSP frame header is not valid ASCII/UTF-8".to_string())
        })?;
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }

        let (name, value) = line.split_once(':').ok_or_else(|| {
            LspError::ProtocolError("LSP frame contains an invalid header".to_string())
        })?;
        if name.eq_ignore_ascii_case("Content-Length") {
            if content_length.is_some() {
                return Err(LspError::ProtocolError(
                    "LSP frame contains duplicate Content-Length headers".to_string(),
                ));
            }
            let length = value.trim().parse::<usize>().map_err(|_| {
                LspError::ProtocolError("LSP frame has an invalid Content-Length".to_string())
            })?;
            if length > MAX_LSP_FRAME_BYTES {
                return Err(LspError::ProtocolError(format!(
                    "LSP frame exceeds {MAX_LSP_FRAME_BYTES} bytes"
                )));
            }
            content_length = Some(length);
        }
    }

    let length = content_length.ok_or_else(|| {
        LspError::ProtocolError("LSP frame is missing Content-Length".to_string())
    })?;
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .await
        .map_err(LspError::IoError)?;
    Ok(Some(body))
}

async fn read_bounded_line(
    reader: &mut (impl AsyncBufRead + Unpin),
    limit: usize,
) -> Result<Option<Vec<u8>>, LspError> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await.map_err(LspError::IoError)?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(line))
            };
        }

        let complete = available.iter().position(|byte| *byte == b'\n');
        let consumed = complete.map_or(available.len(), |index| index + 1);
        if line.len().saturating_add(consumed) > limit {
            return Err(LspError::ProtocolError(format!(
                "LSP line exceeds {limit} bytes"
            )));
        }
        line.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if complete.is_some() {
            return Ok(Some(line));
        }
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
    let body = std::str::from_utf8(body)
        .map_err(|_| LspError::ProtocolError("LSP message body is not valid UTF-8".to_string()))?;
    let res = serde_json::from_str::<serde_json::Value>(body).map_err(LspError::JsonError)?;

    if let Some(error) = res.get("error") {
        let id = match res.get("id") {
            Some(Value::Null) | None => None,
            Some(id) => Some(id.as_i64().ok_or_else(|| {
                LspError::ProtocolError("LSP error response id is not an integer".to_string())
            })?),
        };
        let code = error.get("code").and_then(Value::as_i64).ok_or_else(|| {
            LspError::ProtocolError("LSP error response is missing an integer code".to_string())
        })?;
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                LspError::ProtocolError("LSP error response is missing a message".to_string())
            })?
            .to_string();
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
        let id = id.as_i64().ok_or_else(|| {
            LspError::ProtocolError("LSP response id is not an integer".to_string())
        })?;
        let result = res.get("result").cloned().ok_or_else(|| {
            LspError::ProtocolError("LSP response is missing a result".to_string())
        })?;

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

        if let Some(id) = res.get("id").cloned() {
            rtx.send(InboundMessage::ServerRequest(ServerRequest {
                id,
                method,
                params,
                source: None,
            }))
            .await
            .map_err(|e| LspError::ChannelInboundError(e.to_string()))?;
            return Ok(());
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
    initialize_failed: bool,
    pending_messages: Vec<OutboundMessage>,
    pending_message_bytes: usize,
    failed_pending_requests: Vec<(i64, String)>,
    server_capabilities: Option<ServerCapabilities>,
    /// Debounced diagnostics requests keyed by normalized document URI.
    pending_diagnostics: HashMap<String, Instant>,
    child: Option<tokio::process::Child>,
    config: LanguageServerConfig,
    workspace_root: PathBuf,
}

impl RealLspClient {
    fn fail_initialization(&mut self) {
        self.initialize_failed = true;
        self.failed_pending_requests
            .extend(
                self.pending_messages
                    .drain(..)
                    .filter_map(|message| match message {
                        OutboundMessage::Request(request) => Some((request.id, request.method)),
                        _ => None,
                    }),
            );
        self.pending_message_bytes = 0;
    }

    fn queue_pending(&mut self, message: OutboundMessage) -> Result<(), LspError> {
        if self.initialize_failed {
            return Err(LspError::ProtocolError(
                "language server initialization has failed".to_string(),
            ));
        }

        let bytes = outbound_message_size(&message);
        let total = self.pending_message_bytes.saturating_add(bytes);
        if self.pending_messages.len() >= MAX_PENDING_LSP_MESSAGES || total > MAX_PENDING_LSP_BYTES
        {
            self.fail_initialization();
            return Err(LspError::ProtocolError(format!(
                "language server did not initialize before its pending queue exceeded {MAX_PENDING_LSP_MESSAGES} messages or {MAX_PENDING_LSP_BYTES} bytes"
            )));
        }

        self.pending_message_bytes = total;
        self.pending_messages.push(message);
        Ok(())
    }

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

        let splits_crlf = |text: &str, offset: usize| {
            offset > 0
                && offset < text.len()
                && text.as_bytes()[offset - 1] == b'\r'
                && text.as_bytes()[offset] == b'\n'
        };
        if splits_crlf(old_text, prefix)
            || splits_crlf(old_text, old_end)
            || splits_crlf(new_text, prefix)
            || splits_crlf(new_text, new_end)
        {
            return vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: new_text.to_string(),
            }];
        }

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

        let uri = file_uri(file)?;
        self.files_content.insert(uri.clone(), contents.to_string());
        self.files_versions.insert(uri, 1);
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
            self.queue_pending(msg)?;
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
            self.queue_pending(msg)?;
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
        if let Some((id, method)) = self.failed_pending_requests.pop() {
            return Ok(Some((
                InboundMessage::RequestError {
                    id,
                    error: LspError::ProtocolError(
                        "language server initialization or transport failed before this request completed"
                            .to_string(),
                    ),
                },
                Some(method),
            )));
        }
        // Send the debounced diagnostics request once the document has been
        // quiet long enough. This is polled every editor tick.
        let now = Instant::now();
        let due = self
            .pending_diagnostics
            .iter()
            .filter(|(_, due)| now >= **due)
            .map(|(uri, _)| uri.clone())
            .collect::<Vec<_>>();
        for uri in due {
            self.pending_diagnostics.remove(&uri);
            self.request_diagnostics(&uri).await?;
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
                if request.method == "initialize" {
                    self.fail_initialization();
                }
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
                                for mut msg in self.pending_messages.drain(..) {
                                    if let OutboundMessage::Request(request) = &mut msg {
                                        request.timestamp = Instant::now();
                                        self.pending_responses.insert(request.id, request.clone());
                                    }
                                    self.request_tx.send(msg).await?;
                                }
                                self.pending_message_bytes = 0;
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

                                self.pending_responses.remove(&id);
                                if method == "initialize" {
                                    self.fail_initialization();
                                }

                                return Ok(Some((msg, Some(method))));
                            }
                        }
                    }
                    InboundMessage::ServerRequest(request)
                        if request.method != "workspace/applyEdit" =>
                    {
                        self.request_tx
                            .send(OutboundMessage::Response(ServerResponse {
                                id: request.id.clone(),
                                result: None,
                                error: Some(json!({
                                    "code": -32601,
                                    "message": format!("unsupported LSP request: {}", request.method),
                                })),
                            }))
                            .await?;
                        return Ok(None);
                    }
                    _ => {}
                }
                if matches!(msg, InboundMessage::ProcessingError(_)) {
                    self.failed_pending_requests.extend(
                        self.pending_responses
                            .drain()
                            .map(|(id, request)| (id, request.method)),
                    );
                    if !self.initialized {
                        self.fail_initialization();
                    }
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
            file_uri(&self.workspace_root)?,
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
        let uri = file_uri(file)?;
        // Diagnostics are debounced: typing produces a didChange per
        // keystroke, and requesting diagnostics for every one of them floods
        // the server. The request is sent from `recv_response` once the
        // document has been quiet for DIAGNOSTICS_DEBOUNCE.
        self.pending_diagnostics
            .insert(uri.clone(), Instant::now() + DIAGNOSTICS_DEBOUNCE);

        // Get or create version for this file
        let version = self.files_versions.entry(uri.clone()).or_insert(0);
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
                    .get(&uri)
                    .map(String::as_str)
                    .unwrap_or("");

                // Calculate actual changes
                Self::calculate_changes(old_content, &contents)
            }
            _ => return Ok(()),
        };

        // Update stored content, reusing the caller's buffer copy.
        self.files_content.insert(uri.clone(), contents);

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

    async fn did_close(&mut self, file: &str) -> Result<(), LspError> {
        let uri = file_uri(file)?;
        self.files_content.remove(&uri);
        self.files_versions.remove(&uri);
        self.pending_diagnostics.remove(&uri);
        self.send_notification(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": uri } }),
            false,
        )
        .await
    }

    // async fn did_change(&mut self, file: &str, contents: &str) -> Result<(), LspError> {
    //     log!("[lsp] did_change file: {}", file);
    //     let version = self.files_versions.entry(file.to_string()).or_insert(0);
    //     *version += 1;
    //
    //     let params = json!({
    //         "textDocument": {
    //             "uri": file_uri(file)?,
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
    //                 "uri": file_uri(file)?,
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
                "uri": file_uri(file)?,
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
                "uri": file_uri(file)?,
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
                "uri": file_uri(file)?,
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
                "uri": file_uri(file)?,
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
        self.format_document_with_options(file, 4, true).await
    }

    async fn format_document_with_options(
        &mut self,
        file: &str,
        tab_size: usize,
        insert_spaces: bool,
    ) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": file_uri(file)?,
            },
            "options": {
                "tabSize": tab_size,
                "insertSpaces": insert_spaces,
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
                "uri": file_uri(file)?,
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
                "uri": file_uri(file)?,
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
                "uri": file_uri(file)?,
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
                "uri": file_uri(file)?,
            }
        });

        self.send_request("textDocument/documentLink", params, false)
            .await
    }

    async fn document_color(&mut self, file: &str) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": file_uri(file)?,
            }
        });

        self.send_request("textDocument/documentColor", params, false)
            .await
    }

    async fn folding_range(&mut self, file: &str) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": file_uri(file)?,
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
                "uri": file_uri(file)?,
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
                "uri": file_uri(file)?,
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
                "uri": file_uri(file)?,
            }
        });

        self.send_request("textDocument/semanticTokens/full", params, false)
            .await
    }

    async fn inlay_hint(&mut self, file: &str, range: Range) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": file_uri(file)?,
            },
            "range": range
        });

        self.send_request("textDocument/inlayHint", params, false)
            .await
    }

    async fn signature_help(&mut self, file: &str, x: usize, y: usize) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": {
                "uri": file_uri(file)?,
            },
            "position": {
                "line": y,
                "character": x,
            }
        });

        self.send_request("textDocument/signatureHelp", params, false)
            .await
    }

    async fn rename(
        &mut self,
        file: &str,
        x: usize,
        y: usize,
        new_name: &str,
    ) -> Result<i64, LspError> {
        let params = json!({
            "textDocument": { "uri": file_uri(file)? },
            "position": { "line": y, "character": x },
            "newName": new_name,
        });

        self.send_request("textDocument/rename", params, false)
            .await
    }

    fn get_server_capabilities(&self) -> Option<&ServerCapabilities> {
        self.server_capabilities.as_ref()
    }

    fn supports_document_formatting(&self, _file: &str) -> bool {
        matches!(
            self.server_capabilities
                .as_ref()
                .and_then(|capabilities| capabilities.document_formatting_provider.as_ref()),
            Some(
                DocumentFormattingProviderCapability::Simple(true)
                    | DocumentFormattingProviderCapability::Options(_)
            )
        )
    }

    fn document_version(&self, file: &str) -> Option<i64> {
        let uri = file_uri(file).ok()?;
        self.files_versions
            .get(&uri)
            .and_then(|version| i64::try_from(*version).ok())
    }

    fn workspace_root_for_file(&self, _file: &str) -> Option<PathBuf> {
        Some(self.workspace_root.clone())
    }

    fn workspace_root_for_request(&self, _request: &ServerRequest) -> Option<PathBuf> {
        Some(self.workspace_root.clone())
    }

    async fn respond_workspace_edit(
        &mut self,
        request: &ServerRequest,
        applied: bool,
        failure_reason: Option<&str>,
    ) -> Result<(), LspError> {
        self.request_tx
            .send(OutboundMessage::Response(ServerResponse {
                id: request.id.clone(),
                result: Some(json!({
                    "applied": applied,
                    "failureReason": failure_reason,
                })),
                error: None,
            }))
            .await?;
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), LspError> {
        let shutdown_id = self
            .send_request("shutdown", serde_json::Value::Null, true)
            .await?;
        let response = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let Some(message) = self.response_rx.recv().await else {
                    return Err(LspError::ProtocolError(
                        "LSP response channel closed during shutdown".to_string(),
                    ));
                };
                match message {
                    InboundMessage::Message(message) if message.id == shutdown_id => {
                        return Ok(());
                    }
                    InboundMessage::Error(error) if error.id == Some(shutdown_id) => {
                        return Err(LspError::ProtocolError(format!(
                            "LSP shutdown failed: {}",
                            error.message
                        )));
                    }
                    InboundMessage::ProcessingError(error) => return Err(error),
                    _ => {}
                }
            }
        })
        .await;
        self.pending_responses.remove(&shutdown_id);
        match response {
            Ok(Ok(())) => {}
            Ok(Err(error)) => log!("[lsp] shutdown response failed: {error}"),
            Err(_) => log!("[lsp] shutdown response timed out; sending exit"),
        }

        // Send exit notification
        self.send_notification("exit", serde_json::Value::Null, true)
            .await?;

        // Take ownership of child process and response channel
        let Some(mut child) = std::mem::take(&mut self.child) else {
            return Ok(());
        };

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

pub async fn lsp_send_response(
    stdin: &mut BufWriter<ChildStdin>,
    response: &ServerResponse,
) -> Result<(), LspError> {
    let body = if let Some(error) = &response.error {
        json!({ "jsonrpc": "2.0", "id": response.id, "error": error })
    } else {
        json!({ "jsonrpc": "2.0", "id": response.id, "result": response.result })
    };
    let body = serde_json::to_string(&body)?;
    let frame = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    stdin.write_all(frame.as_bytes()).await?;
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
    async fn retrigger_cancellation_clears_pending_completion_request() {
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
            pending_diagnostics: HashMap::new(),
            pending_messages: Vec::new(),
            pending_message_bytes: 0,
            failed_pending_requests: Vec::new(),
            initialize_failed: false,
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
        assert!(!client.pending_responses.contains_key(&42));

        let Some((second_message, second_method)) = client.recv_response().await.unwrap() else {
            panic!("expected completion response");
        };
        assert_eq!(second_method, None);
        let InboundMessage::Message(response) = second_message else {
            panic!("expected completion message");
        };
        assert_eq!(response.id, 42);
        assert!(response.request.is_none());
        assert!(!client.pending_responses.contains_key(&42));
    }

    #[tokio::test]
    async fn lsp_frame_reader_accepts_optional_headers_and_multiple_frames() {
        let first = br#"{"jsonrpc":"2.0","id":1,"result":{}}"#;
        let second = br#"{"jsonrpc":"2.0","method":"window/logMessage"}"#;
        let frames = format!(
            "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-length: {}\r\nX-Test: ok\r\n\r\n{}Content-Length: {}\r\n\r\n{}",
            first.len(),
            std::str::from_utf8(first).unwrap(),
            second.len(),
            std::str::from_utf8(second).unwrap(),
        );
        let mut reader = BufReader::with_capacity(7, frames.as_bytes());

        assert_eq!(
            read_lsp_frame(&mut reader).await.unwrap(),
            Some(first.to_vec())
        );
        assert_eq!(
            read_lsp_frame(&mut reader).await.unwrap(),
            Some(second.to_vec())
        );
        assert!(read_lsp_frame(&mut reader).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn lsp_frame_reader_rejects_invalid_oversized_and_truncated_frames() {
        let invalid_frames = [
            format!("Content-Length: {}\r\n\r\n", MAX_LSP_FRAME_BYTES + 1),
            "Content-Length: 1\r\nContent-Length: 1\r\n\r\nx".to_string(),
            "Content-Type: application/json\r\n\r\n{}".to_string(),
            "Content-Length: nope\r\n\r\n".to_string(),
            "broken header\r\n\r\n".to_string(),
            "Content-Length: 3\r\n\r\n{}".to_string(),
            format!("X-Test: {}\r\n\r\n", "x".repeat(MAX_LSP_HEADER_BYTES)),
        ];

        for frame in invalid_frames {
            let mut reader = BufReader::with_capacity(11, frame.as_bytes());
            assert!(read_lsp_frame(&mut reader).await.is_err());
        }
    }

    #[tokio::test]
    async fn bounded_lsp_stderr_reader_rejects_an_oversized_line() {
        let mut complete = BufReader::with_capacity(3, b"warning\n".as_slice());
        assert_eq!(
            read_bounded_line(&mut complete, MAX_LSP_STDERR_LINE_BYTES)
                .await
                .unwrap(),
            Some(b"warning\n".to_vec())
        );
        assert!(read_bounded_line(&mut complete, MAX_LSP_STDERR_LINE_BYTES)
            .await
            .unwrap()
            .is_none());

        let oversized = vec![b'x'; MAX_LSP_STDERR_LINE_BYTES + 1];
        let mut oversized = BufReader::with_capacity(5, oversized.as_slice());
        assert!(read_bounded_line(&mut oversized, MAX_LSP_STDERR_LINE_BYTES)
            .await
            .is_err());
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
        assert_eq!(error.code, -32802);
        assert_eq!(error.data, Some(json!({ "retriggerRequest": true })));
    }

    #[tokio::test]
    async fn process_lsp_message_rejects_invalid_utf8_and_malformed_responses() {
        let (response_tx, _response_rx) = mpsc::channel(1);
        let invalid = [
            vec![0xff, 0xfe],
            br#"{"jsonrpc":"2.0","id":"wrong","result":{}}"#.to_vec(),
            br#"{"jsonrpc":"2.0","id":1}"#.to_vec(),
            br#"{"jsonrpc":"2.0","id":1,"error":{"message":"missing code"}}"#.to_vec(),
            br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32603}}"#.to_vec(),
        ];
        for body in invalid {
            assert!(process_lsp_message(&body, &response_tx).await.is_err());
        }
    }

    #[tokio::test]
    async fn server_request_id_does_not_complete_pending_client_request() {
        let (request_tx, mut request_rx) = mpsc::channel(2);
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
            pending_diagnostics: HashMap::new(),
            pending_messages: Vec::new(),
            pending_message_bytes: 0,
            failed_pending_requests: Vec::new(),
            initialize_failed: false,
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

        assert!(client.recv_response().await.unwrap().is_none());
        let Some(OutboundMessage::Response(response)) = request_rx.recv().await else {
            panic!("expected method-not-found response");
        };
        assert_eq!(response.id, json!(31));
        assert_eq!(
            response
                .error
                .as_ref()
                .and_then(|error| error["code"].as_i64()),
            Some(-32601)
        );
        assert!(client.pending_responses.contains_key(&31));

        let Some((second_message, second_method)) = client.recv_response().await.unwrap() else {
            panic!("expected completion response");
        };
        assert_eq!(second_method.as_deref(), Some("textDocument/completion"));
        assert!(matches!(second_message, InboundMessage::Message(_)));
        assert!(!client.pending_responses.contains_key(&31));
    }

    #[tokio::test]
    async fn daily_driver_requests_use_encoded_file_uris_and_lsp_positions() {
        let (request_tx, mut request_rx) = mpsc::channel(5);
        let (_response_tx, response_rx) = mpsc::channel(1);
        let mut client = RealLspClient {
            request_tx,
            response_rx,
            files_versions: HashMap::new(),
            files_content: HashMap::new(),
            pending_responses: HashMap::new(),
            initialize_id: None,
            initialized: true,
            pending_diagnostics: HashMap::new(),
            pending_messages: Vec::new(),
            pending_message_bytes: 0,
            failed_pending_requests: Vec::new(),
            initialize_failed: false,
            server_capabilities: None,
            child: None,
            config: default_language_servers()
                .remove("rust")
                .expect("default Rust LSP config must exist"),
            workspace_root: std::env::current_dir().unwrap(),
        };
        let path = std::env::current_dir()
            .unwrap()
            .join("folder with spaces")
            .join("café #1%.rs");
        let path = path.to_string_lossy();
        let uri = file_uri(path.as_ref()).unwrap();
        let range = Range {
            start: Position {
                line: 1,
                character: 3,
            },
            end: Position {
                line: 1,
                character: 7,
            },
        };
        let diagnostic: Diagnostic = serde_json::from_value(json!({
            "range": range,
            "severity": 1,
            "message": "example diagnostic"
        }))
        .unwrap();
        client.files_versions.insert(uri.clone(), 7);
        assert_eq!(client.document_version(path.as_ref()), Some(7));
        assert_eq!(client.document_version("missing.rs"), None);

        client
            .format_document_with_options(path.as_ref(), 2, true)
            .await
            .unwrap();
        client
            .code_action(path.as_ref(), range.clone(), vec![diagnostic])
            .await
            .unwrap();
        client.signature_help(path.as_ref(), 3, 1).await.unwrap();
        client.rename(path.as_ref(), 3, 1, "renamed").await.unwrap();

        let Some(OutboundMessage::Request(formatting)) = request_rx.recv().await else {
            panic!("expected formatting request");
        };
        assert_eq!(formatting.method, "textDocument/formatting");
        assert_eq!(formatting.params["textDocument"]["uri"], uri);
        assert_eq!(formatting.params["options"]["tabSize"], json!(2));
        assert_eq!(formatting.params["options"]["insertSpaces"], json!(true));

        let Some(OutboundMessage::Request(code_action)) = request_rx.recv().await else {
            panic!("expected code-action request");
        };
        assert_eq!(code_action.method, "textDocument/codeAction");
        assert_eq!(code_action.params["textDocument"]["uri"], uri);
        assert_eq!(code_action.params["range"], json!(range));
        assert_eq!(
            code_action.params["context"]["diagnostics"][0]["message"],
            "example diagnostic"
        );

        let Some(OutboundMessage::Request(signature_help)) = request_rx.recv().await else {
            panic!("expected signature-help request");
        };
        assert_eq!(signature_help.method, "textDocument/signatureHelp");
        assert_eq!(signature_help.params["textDocument"]["uri"], uri);
        assert_eq!(
            signature_help.params["position"],
            json!({ "line": 1, "character": 3 })
        );

        let Some(OutboundMessage::Request(rename)) = request_rx.recv().await else {
            panic!("expected rename request");
        };
        assert_eq!(rename.method, "textDocument/rename");
        assert_eq!(rename.params["textDocument"]["uri"], uri);
        assert_eq!(
            rename.params["position"],
            json!({ "line": 1, "character": 3 })
        );
        assert_eq!(rename.params["newName"], "renamed");
    }

    #[tokio::test]
    async fn workspace_apply_edit_request_is_preserved_and_receives_a_response() {
        let (request_tx, mut request_rx) = mpsc::channel(2);
        let (response_tx, response_rx) = mpsc::channel(2);
        let mut client = RealLspClient {
            request_tx,
            response_rx,
            files_versions: HashMap::new(),
            files_content: HashMap::new(),
            pending_responses: HashMap::new(),
            initialize_id: None,
            initialized: true,
            pending_diagnostics: HashMap::new(),
            pending_messages: Vec::new(),
            pending_message_bytes: 0,
            failed_pending_requests: Vec::new(),
            initialize_failed: false,
            server_capabilities: None,
            child: None,
            config: default_language_servers()
                .remove("rust")
                .expect("default Rust LSP config must exist"),
            workspace_root: std::env::current_dir().unwrap(),
        };
        let body = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": "server-edit-1",
            "method": "workspace/applyEdit",
            "params": { "label": "Update imports", "edit": { "changes": {} } }
        }))
        .unwrap();
        process_lsp_message(&body, &response_tx).await.unwrap();

        let Some((InboundMessage::ServerRequest(request), method)) =
            client.recv_response().await.unwrap()
        else {
            panic!("expected workspace/applyEdit request");
        };
        assert_eq!(method, None);
        assert_eq!(request.id, json!("server-edit-1"));
        assert_eq!(request.method, "workspace/applyEdit");
        assert_eq!(request.params["label"], "Update imports");

        client
            .respond_workspace_edit(&request, false, Some("buffer changed"))
            .await
            .unwrap();
        let Some(OutboundMessage::Response(response)) = request_rx.recv().await else {
            panic!("expected workspace edit response");
        };
        assert_eq!(response.id, json!("server-edit-1"));
        assert_eq!(
            response.result,
            Some(json!({ "applied": false, "failureReason": "buffer changed" }))
        );
        assert!(response.error.is_none());
    }

    #[tokio::test]
    async fn shutdown_waits_for_the_response_before_sending_exit() {
        let (request_tx, mut request_rx) = mpsc::channel(4);
        let (response_tx, response_rx) = mpsc::channel(4);
        let mut client = RealLspClient {
            request_tx,
            response_rx,
            files_versions: HashMap::new(),
            files_content: HashMap::new(),
            pending_responses: HashMap::new(),
            initialize_id: None,
            initialized: true,
            pending_diagnostics: HashMap::new(),
            pending_messages: Vec::new(),
            pending_message_bytes: 0,
            failed_pending_requests: Vec::new(),
            initialize_failed: false,
            server_capabilities: None,
            child: None,
            config: default_language_servers()
                .remove("rust")
                .expect("default Rust LSP config must exist"),
            workspace_root: std::env::current_dir().unwrap(),
        };
        let observer = tokio::spawn(async move {
            let Some(OutboundMessage::Request(request)) = request_rx.recv().await else {
                panic!("expected shutdown request");
            };
            assert_eq!(request.method, "shutdown");
            assert!(
                tokio::time::timeout(Duration::from_millis(20), request_rx.recv())
                    .await
                    .is_err()
            );
            response_tx
                .send(InboundMessage::Message(ResponseMessage {
                    id: request.id,
                    result: Value::Null,
                    request: None,
                }))
                .await
                .unwrap();
            let Some(OutboundMessage::Notification(notification)) = request_rx.recv().await else {
                panic!("expected exit notification");
            };
            assert_eq!(notification.method, "exit");
        });

        client.shutdown().await.unwrap();
        observer.await.unwrap();
    }

    #[tokio::test]
    async fn queued_requests_are_registered_when_initialization_drains() {
        let (request_tx, mut request_rx) = mpsc::channel(4);
        let (response_tx, response_rx) = mpsc::channel(4);
        let initialize = Request {
            id: 800,
            method: "initialize".to_string(),
            params: json!({}),
            timestamp: Instant::now(),
        };
        let mut client = RealLspClient {
            request_tx,
            response_rx,
            files_versions: HashMap::new(),
            files_content: HashMap::new(),
            pending_responses: HashMap::from([(initialize.id, initialize)]),
            initialize_id: Some(800),
            initialized: false,
            pending_diagnostics: HashMap::new(),
            pending_messages: Vec::new(),
            pending_message_bytes: 0,
            failed_pending_requests: Vec::new(),
            initialize_failed: false,
            server_capabilities: None,
            child: None,
            config: default_language_servers()
                .remove("rust")
                .expect("default Rust LSP config must exist"),
            workspace_root: std::env::current_dir().unwrap(),
        };
        let queued_id = client
            .send_request("textDocument/formatting", json!({ "queued": true }), false)
            .await
            .unwrap();
        assert_eq!(client.pending_messages.len(), 1);
        assert!(!client.pending_responses.contains_key(&queued_id));
        response_tx
            .send(InboundMessage::Message(ResponseMessage {
                id: 800,
                result: json!({ "capabilities": {} }),
                request: None,
            }))
            .await
            .unwrap();

        client.recv_response().await.unwrap();

        let Some(OutboundMessage::Notification(initialized)) = request_rx.recv().await else {
            panic!("expected initialized notification");
        };
        assert_eq!(initialized.method, "initialized");
        let Some(OutboundMessage::Request(queued)) = request_rx.recv().await else {
            panic!("expected queued formatting request");
        };
        assert_eq!(queued.id, queued_id);
        assert!(client.pending_responses.contains_key(&queued_id));
        assert!(client.pending_messages.is_empty());
        assert_eq!(client.pending_message_bytes, 0);
    }

    #[tokio::test]
    async fn failed_or_overflowed_initialization_fails_each_queued_request_and_bounds_memory() {
        let (request_tx, _request_rx) = mpsc::channel(1);
        let (_response_tx, response_rx) = mpsc::channel(1);
        let mut client = RealLspClient {
            request_tx,
            response_rx,
            files_versions: HashMap::new(),
            files_content: HashMap::new(),
            pending_responses: HashMap::new(),
            initialize_id: None,
            initialized: false,
            pending_diagnostics: HashMap::new(),
            pending_messages: Vec::new(),
            pending_message_bytes: 0,
            failed_pending_requests: Vec::new(),
            initialize_failed: false,
            server_capabilities: None,
            child: None,
            config: default_language_servers()
                .remove("rust")
                .expect("default Rust LSP config must exist"),
            workspace_root: std::env::current_dir().unwrap(),
        };
        let request_id = client
            .send_request("textDocument/formatting", json!({}), false)
            .await
            .unwrap();
        let error = client
            .send_notification(
                "textDocument/didChange",
                json!({ "text": "x".repeat(MAX_PENDING_LSP_BYTES) }),
                false,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("pending queue exceeded"));
        assert!(client.pending_messages.is_empty());
        assert_eq!(client.pending_message_bytes, 0);
        let Some((InboundMessage::RequestError { id, error }, method)) =
            client.recv_response().await.unwrap()
        else {
            panic!("expected failed queued request");
        };
        assert_eq!(id, request_id);
        assert_eq!(method.as_deref(), Some("textDocument/formatting"));
        assert!(error
            .to_string()
            .contains("initialization or transport failed"));
        assert!(client
            .send_request("textDocument/rename", json!({}), false)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn transport_failure_drains_every_in_flight_request_as_a_request_error() {
        let (request_tx, _request_rx) = mpsc::channel(2);
        let (response_tx, response_rx) = mpsc::channel(2);
        let request = Request {
            id: 801,
            method: "textDocument/formatting".to_string(),
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
            pending_diagnostics: HashMap::new(),
            pending_messages: Vec::new(),
            pending_message_bytes: 0,
            failed_pending_requests: Vec::new(),
            initialize_failed: false,
            server_capabilities: None,
            child: None,
            config: default_language_servers()
                .remove("rust")
                .expect("default Rust LSP config must exist"),
            workspace_root: std::env::current_dir().unwrap(),
        };
        response_tx
            .send(InboundMessage::ProcessingError(LspError::ProtocolError(
                "invalid stdout frame".to_string(),
            )))
            .await
            .unwrap();

        let Some((InboundMessage::ProcessingError(_), None)) =
            client.recv_response().await.unwrap()
        else {
            panic!("expected the transport failure");
        };
        assert!(client.pending_responses.is_empty());
        let Some((InboundMessage::RequestError { id, error }, method)) =
            client.recv_response().await.unwrap()
        else {
            panic!("expected the failed formatting request");
        };
        assert_eq!(id, 801);
        assert_eq!(method.as_deref(), Some("textDocument/formatting"));
        assert!(error.to_string().contains("transport failed"));
    }

    #[tokio::test]
    async fn diagnostics_debounce_is_tracked_per_document() {
        let (request_tx, mut request_rx) = mpsc::channel(4);
        let (_response_tx, response_rx) = mpsc::channel(1);
        let mut client = RealLspClient {
            request_tx,
            response_rx,
            files_versions: HashMap::new(),
            files_content: HashMap::new(),
            pending_responses: HashMap::new(),
            initialize_id: None,
            initialized: true,
            pending_diagnostics: HashMap::from([
                (
                    "file:///tmp/one.rs".to_string(),
                    Instant::now() - Duration::from_secs(1),
                ),
                (
                    "file:///tmp/two.rs".to_string(),
                    Instant::now() - Duration::from_secs(1),
                ),
            ]),
            pending_messages: Vec::new(),
            pending_message_bytes: 0,
            failed_pending_requests: Vec::new(),
            initialize_failed: false,
            server_capabilities: Some(
                serde_json::from_value(json!({
                    "diagnosticProvider": {
                        "interFileDependencies": false,
                        "workspaceDiagnostics": false
                    }
                }))
                .unwrap(),
            ),
            child: None,
            config: default_language_servers()
                .remove("rust")
                .expect("default Rust LSP config must exist"),
            workspace_root: std::env::current_dir().unwrap(),
        };

        assert!(client.recv_response().await.unwrap().is_none());
        let mut uris = Vec::new();
        for _ in 0..2 {
            let Some(OutboundMessage::Request(request)) = request_rx.recv().await else {
                panic!("expected diagnostics request");
            };
            assert_eq!(request.method, "textDocument/diagnostic");
            uris.push(
                request.params["textDocument"]["uri"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
        }
        uris.sort();
        assert_eq!(uris, ["file:///tmp/one.rs", "file:///tmp/two.rs"]);
        assert!(client.pending_diagnostics.is_empty());
    }

    #[tokio::test]
    async fn document_state_uses_normalized_uri_across_relative_absolute_close_and_reopen() {
        let (request_tx, mut request_rx) = mpsc::channel(8);
        let (_response_tx, response_rx) = mpsc::channel(1);
        let mut client = RealLspClient {
            request_tx,
            response_rx,
            files_versions: HashMap::new(),
            files_content: HashMap::new(),
            pending_responses: HashMap::new(),
            initialize_id: None,
            initialized: true,
            pending_diagnostics: HashMap::new(),
            pending_messages: Vec::new(),
            pending_message_bytes: 0,
            failed_pending_requests: Vec::new(),
            initialize_failed: false,
            server_capabilities: Some(
                serde_json::from_value(json!({ "textDocumentSync": 2 })).unwrap(),
            ),
            child: None,
            config: default_language_servers()
                .remove("rust")
                .expect("default Rust LSP config must exist"),
            workspace_root: std::env::current_dir().unwrap(),
        };
        let relative = "src/../normalized-lsp-state.rs";
        let absolute = std::env::current_dir()
            .unwrap()
            .join("normalized-lsp-state.rs")
            .to_string_lossy()
            .into_owned();
        client.did_open(relative, "old").await.unwrap();
        client
            .did_change(&absolute, "new".to_string())
            .await
            .unwrap();
        assert_eq!(client.document_version(relative), Some(2));
        assert_eq!(client.document_version(&absolute), Some(2));
        client.did_close(&absolute).await.unwrap();
        assert_eq!(client.document_version(relative), None);
        client.did_open(&absolute, "reopened").await.unwrap();
        assert_eq!(client.document_version(relative), Some(1));

        let mut methods = Vec::new();
        while let Ok(message) = request_rx.try_recv() {
            if let OutboundMessage::Notification(notification) = message {
                methods.push(notification.method);
            }
        }
        assert_eq!(
            methods,
            [
                "textDocument/didOpen",
                "textDocument/didChange",
                "textDocument/didClose",
                "textDocument/didOpen"
            ]
        );
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
    fn test_calculate_changes_falls_back_to_full_sync_when_a_crlf_pair_changes() {
        let change = single_change("a\r\n", "a\n");

        assert!(change.range.is_none());
        assert_eq!(change.text, "a\n");
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
