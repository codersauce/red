//! One language-server process, bounded JSON-RPC transport, and client-side protocol state.
//!
//! [`RealLspClient`] owns process stdio, monotonically increasing document versions,
//! request correlation, capabilities, queued pre-initialization messages, diagnostics
//! debounce, and orderly shutdown. Reader and writer tasks communicate through bounded
//! channels; they never mutate editor state directly.
//!
//! Frames, headers, stderr lines, pending message counts, and pending bytes are bounded.
//! A malformed or oversized stream becomes a processing error rather than allocating
//! without limit. Requests sent before successful initialization are retained only
//! within the documented queue budgets.

use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use serde::Serialize;
use serde_json::{json, Map, Value};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter},
    process::{ChildStdin, Command as TokioCommand},
    sync::mpsc::{self, error::TryRecvError},
};

use super::{
    capabilities::get_client_capabilities_with_options, file_uri,
    workspace_watch::WorkspaceFileWatcher, InboundMessage, LspClient, OutboundMessage,
    ResponseError, ServerRequest, ServerResponse,
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
const MAX_LSP_STDERR_TAIL_LINES: usize = 8;
const MAX_PENDING_LSP_MESSAGES: usize = 512;
const MAX_PENDING_LSP_BYTES: usize = 16 * 1024 * 1024;
const SHUTDOWN_RESPONSE_TIMEOUT: Duration = Duration::from_secs(1);
#[cfg(windows)]
const PROCESS_EXIT_GRACE: Duration = Duration::from_millis(100);
#[cfg(not(windows))]
const PROCESS_EXIT_GRACE: Duration = Duration::from_secs(5);

/// Idle time after the last document change before diagnostics are
/// requested. Typing produces one didChange per keystroke; requesting
/// diagnostics for each is wasted server work.
const DIAGNOSTICS_DEBOUNCE: Duration = Duration::from_millis(250);

/// Apply the negotiated save options, including to saves queued before initialize.
fn prepare_did_save(params: &mut Value, capabilities: Option<&ServerCapabilities>) -> bool {
    let Some(include_text) = capabilities
        .and_then(|caps| caps.text_document_sync.as_ref())
        .and_then(TextDocumentSyncCapability::save_include_text)
    else {
        return false;
    };
    if !include_text {
        if let Some(params) = params.as_object_mut() {
            params.remove("text");
        }
    }
    true
}

fn diagnostic_request_uri(request: &Request) -> Option<&str> {
    if request.method != "textDocument/diagnostic" {
        return None;
    }
    request.params["textDocument"]["uri"].as_str()
}

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

fn did_change_params(
    uri: &str,
    version: usize,
    content_changes: Vec<TextDocumentContentChangeEvent>,
) -> Value {
    let content_changes = content_changes
        .into_iter()
        .map(|change| {
            let mut value = Map::new();
            if let Some(range) = change.range {
                value.insert("range".to_string(), json!(range));
            }
            if let Some(range_length) = change.range_length {
                value.insert("rangeLength".to_string(), Value::from(range_length));
            }
            value.insert("text".to_string(), Value::String(change.text));
            Value::Object(value)
        })
        .collect();

    let mut params = json!({
        "textDocument": {
            "uri": uri,
            "version": version,
        },
    });
    params["contentChanges"] = Value::Array(content_changes);
    params
}

#[derive(Serialize)]
struct NotificationEnvelope<'a> {
    jsonrpc: &'static str,
    method: &'a str,
    params: &'a Value,
}

fn notification_body(req: &NotificationRequest) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&NotificationEnvelope {
        jsonrpc: "2.0",
        method: &req.method,
        params: &req.params,
    })
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

struct LspProcessMonitor {
    stdout_closed: Arc<AtomicBool>,
    stderr_closed: Arc<AtomicBool>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    failure_reported: bool,
    workspace_watcher: Option<WorkspaceFileWatcher>,
}

impl LspProcessMonitor {
    fn stderr_summary(&self) -> Option<String> {
        let stderr_tail = self
            .stderr_tail
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        stderr_tail
            .iter()
            .find(|line| {
                let lower = line.to_ascii_lowercase();
                lower.starts_with("error:")
                    || lower.starts_with("fatal")
                    || lower.contains("panicked")
            })
            .or_else(|| stderr_tail.back())
            .cloned()
    }
}

fn lsp_command_display(config: &LanguageServerConfig) -> String {
    std::iter::once(config.command.as_str())
        .chain(config.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
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
            failure_reason: None,
            pending_messages: Vec::new(),
            pending_message_bytes: 0,
            failed_pending_requests: Vec::new(),
            server_capabilities: None,
            pending_diagnostics: HashMap::new(),
            child: None,
            process_monitor: None,
            config,
            workspace_root,
        }
    }

    pub async fn start(
        config: LanguageServerConfig,
        workspace_root: PathBuf,
    ) -> Result<RealLspClient, LspError> {
        let mut child = spawn_lsp_process(&config).await?;
        let workspace_watcher = match WorkspaceFileWatcher::new(&workspace_root, &config) {
            Ok(watcher) => Some(watcher),
            Err(error) => {
                log!(
                    "[lsp] could not watch workspace {}: {error}",
                    workspace_root.display()
                );
                None
            }
        };
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let (request_tx, mut request_rx) = mpsc::channel::<OutboundMessage>(512);
        let (response_tx, response_rx) = mpsc::channel::<InboundMessage>(512);
        let stdout_closed = Arc::new(AtomicBool::new(false));
        let stderr_closed = Arc::new(AtomicBool::new(false));
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));

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
        let stdout_closed_task = Arc::clone(&stdout_closed);
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
            stdout_closed_task.store(true, Ordering::Release);
        });

        // Language servers commonly write operational logs to stderr. Keep
        // those in the log file, and only surface panic/fatal-looking lines.
        let rtx = response_tx.clone();
        let stderr_closed_task = Arc::clone(&stderr_closed);
        let stderr_tail_task = Arc::clone(&stderr_tail);
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
                    let mut tail = stderr_tail_task
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if tail.len() == MAX_LSP_STDERR_TAIL_LINES {
                        tail.pop_front();
                    }
                    tail.push_back(message.clone());
                }

                if should_surface_server_stderr(&message) {
                    match rtx.send(InboundMessage::ServerStderr(message)).await {
                        Ok(_) => (),
                        Err(err) => {
                            log!("[lsp] error sending stderr to editor: {}", err);
                        }
                    }
                }
            }
            stderr_closed_task.store(true, Ordering::Release);
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
            failure_reason: None,
            pending_diagnostics: HashMap::new(),
            server_capabilities: None,
            child: Some(child),
            process_monitor: Some(LspProcessMonitor {
                stdout_closed,
                stderr_closed,
                stderr_tail,
                failure_reported: false,
                workspace_watcher,
            }),
            config,
            workspace_root,
        })
    }
}

pub(crate) async fn read_lsp_frame(
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
    files_content: HashMap<String, ropey::Rope>,
    pending_responses: HashMap<i64, Request>,
    initialize_id: Option<i64>,
    initialized: bool,
    initialize_failed: bool,
    failure_reason: Option<String>,
    pending_messages: Vec<OutboundMessage>,
    pending_message_bytes: usize,
    failed_pending_requests: Vec<(i64, String)>,
    server_capabilities: Option<ServerCapabilities>,
    /// Debounced diagnostics requests keyed by normalized document URI.
    pending_diagnostics: HashMap<String, Instant>,
    child: Option<tokio::process::Child>,
    process_monitor: Option<LspProcessMonitor>,
    config: LanguageServerConfig,
    workspace_root: PathBuf,
}

impl RealLspClient {
    fn fail_server(&mut self, reason: impl Into<String>) {
        self.initialize_failed = true;
        self.initialized = false;
        if self.failure_reason.is_none() {
            self.failure_reason = Some(reason.into());
        }
        self.initialize_id = None;
        self.failed_pending_requests.extend(
            self.pending_responses
                .drain()
                .filter(|(_, request)| request.method != "initialize")
                .map(|(id, request)| (id, request.method)),
        );
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
        self.pending_diagnostics.clear();
    }

    fn poll_process_failure(&mut self) -> Option<LspError> {
        let monitor = self.process_monitor.as_mut()?;
        if monitor.failure_reported {
            return None;
        }

        let command = lsp_command_display(&self.config);
        let status = match self.child.as_mut()?.try_wait() {
            Ok(status) => status,
            Err(error) => {
                monitor.failure_reported = true;
                return Some(LspError::IoError(error));
            }
        };

        if let Some(status) = status {
            if !monitor.stderr_closed.load(Ordering::Acquire) {
                return None;
            }
            monitor.failure_reported = true;
            let mut reason = format!("{command} exited unexpectedly with {status}");
            if let Some(stderr) = monitor.stderr_summary() {
                reason.push_str(": ");
                reason.push_str(&stderr);
            }
            return Some(LspError::ServerError(reason));
        }

        if monitor.stdout_closed.load(Ordering::Acquire) {
            monitor.failure_reported = true;
            return Some(LspError::ProtocolError(format!(
                "{command} closed its stdout stream unexpectedly"
            )));
        }

        None
    }

    fn queue_pending(&mut self, message: OutboundMessage) -> Result<(), LspError> {
        if self.initialize_failed {
            let reason = self
                .failure_reason
                .as_deref()
                .unwrap_or("language server initialization has failed");
            return Err(LspError::ProtocolError(format!(
                "language server unavailable: {reason}"
            )));
        }

        let bytes = outbound_message_size(&message);
        let total = self.pending_message_bytes.saturating_add(bytes);
        if self.pending_messages.len() >= MAX_PENDING_LSP_MESSAGES || total > MAX_PENDING_LSP_BYTES
        {
            let error = LspError::ProtocolError(format!(
                "language server did not initialize before its pending queue exceeded {MAX_PENDING_LSP_MESSAGES} messages or {MAX_PENDING_LSP_BYTES} bytes"
            ));
            self.fail_server(error.to_string());
            return Err(error);
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

    fn has_pending_diagnostics(&self, uri: &str) -> bool {
        self.pending_responses
            .values()
            .any(|request| diagnostic_request_uri(request) == Some(uri))
    }

    fn schedule_diagnostics(&mut self, uri: &str) {
        let next = Instant::now() + DIAGNOSTICS_DEBOUNCE;
        self.pending_diagnostics
            .entry(uri.to_string())
            .and_modify(|due| *due = (*due).max(next))
            .or_insert(next);
    }

    fn schedule_workspace_diagnostics(&mut self) {
        let documents = self.files_versions.keys().cloned().collect::<Vec<_>>();
        for uri in documents {
            self.schedule_diagnostics(&uri);
        }
    }

    async fn publish_workspace_file_changes(&mut self) -> Result<(), LspError> {
        let changes = self
            .process_monitor
            .as_mut()
            .and_then(|monitor| monitor.workspace_watcher.as_mut())
            .map(WorkspaceFileWatcher::take_changes)
            .unwrap_or_default();
        let mut watched_changes = Vec::with_capacity(changes.len());
        for change in changes {
            let uri = file_uri(&change.path)?;
            // Open documents already have authoritative didChange/didSave streams.
            // Replaying their disk writes as workspace changes can cancel flycheck.
            if !self.files_versions.contains_key(&uri) {
                watched_changes.push(json!({ "uri": uri, "type": change.kind }));
            }
        }
        if watched_changes.is_empty() {
            return Ok(());
        }
        self.send_notification(
            "workspace/didChangeWatchedFiles",
            json!({ "changes": watched_changes }),
            false,
        )
        .await?;
        self.schedule_workspace_diagnostics();
        Ok(())
    }

    /// Consume expected background diagnostic cancellations without hiding
    /// failures of user commands or leaving their request owners unresolved.
    fn handle_diagnostic_cancellation(&mut self, request: &Request, error: &ResponseError) -> bool {
        let Some(uri) = diagnostic_request_uri(request) else {
            return false;
        };
        let retry = match error.code {
            -32800 => false, // RequestCancelled
            -32801 => true,  // ContentModified
            // The pull-diagnostics protocol defaults missing cancellation data
            // to retriggerRequest: true. Never retry immediately in a busy loop.
            -32802 => error
                .data
                .as_ref()
                .and_then(|data| data.get("retriggerRequest"))
                .and_then(Value::as_bool)
                .unwrap_or(true),
            _ => return false,
        };
        if retry && self.files_versions.contains_key(uri) {
            self.schedule_diagnostics(uri);
        }
        true
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
        if let Some(watcher) = self
            .process_monitor
            .as_mut()
            .and_then(|monitor| monitor.workspace_watcher.as_mut())
        {
            watcher.watch_document(std::path::Path::new(&super::file_path(&uri)?));
        }
        self.files_content
            .insert(uri.clone(), ropey::Rope::from_str(contents));
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

        if self.initialize_failed && !force {
            log!(
                "[lsp] skipping notification for unavailable server: {}",
                method
            );
            return Ok(());
        }

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
        if self.has_pending_diagnostics(file_uri) {
            self.schedule_diagnostics(file_uri);
            return Ok(None);
        }
        // An immediate request also satisfies any queued refresh for this URI.
        self.pending_diagnostics.remove(file_uri);

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
            let reason = self
                .failure_reason
                .as_deref()
                .unwrap_or("language server initialization or transport failed");
            return Ok(Some((
                InboundMessage::RequestError {
                    id,
                    error: LspError::ProtocolError(format!(
                        "language server unavailable before this request completed: {reason}"
                    )),
                },
                Some(method),
            )));
        }
        self.publish_workspace_file_changes().await?;
        // Send the debounced diagnostics request once the document has been
        // quiet long enough. This is polled every editor tick.
        let now = Instant::now();
        let due = self
            .pending_diagnostics
            .iter()
            .filter(|(uri, due)| now >= **due && !self.has_pending_diagnostics(uri))
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
                let elapsed = now.duration_since(request.timestamp);
                if request.method == "initialize" {
                    let error = LspError::RequestTimeout(elapsed);
                    self.fail_server(error.to_string());
                    return Ok(Some((
                        InboundMessage::ProcessingError(error),
                        Some(request.method),
                    )));
                }
                return Ok(Some((
                    InboundMessage::RequestError {
                        id,
                        error: LspError::RequestTimeout(elapsed),
                    },
                    Some(request.method),
                )));
            }
        }

        match self.response_rx.try_recv() {
            Ok(mut msg) => {
                if matches!(
                    msg,
                    InboundMessage::ProcessingError(_) | InboundMessage::ServerStderr(_)
                ) {
                    if let Some(error) = self.poll_process_failure() {
                        msg = InboundMessage::ProcessingError(error);
                    }
                }
                match &mut msg {
                    InboundMessage::Message(msg) => {
                        if let Some(req) = self.pending_responses.remove(&msg.id) {
                            log!("[lsp] rcv_response: id={} method={}", msg.id, req.method);
                            if req.method == "initialize" {
                                log!("[lsp] server initialized");

                                // Parse the initialize result
                                // https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#initialized
                                let init_result: InitializeResult =
                                    match serde_json::from_value(msg.result.clone()) {
                                        Ok(init_result) => init_result,
                                        Err(error) => {
                                            let error = LspError::ProtocolError(format!(
                                                "invalid initialize response: {error}"
                                            ));
                                            self.fail_server(error.to_string());
                                            return Ok(Some((
                                                InboundMessage::ProcessingError(error),
                                                Some(req.method),
                                            )));
                                        }
                                    };

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
                                    if let OutboundMessage::Notification(notification) = &mut msg {
                                        if notification.method == "textDocument/didSave"
                                            && !prepare_did_save(
                                                &mut notification.params,
                                                self.server_capabilities.as_ref(),
                                            )
                                        {
                                            continue;
                                        }
                                    }
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
                            if let Some(request) = self.pending_responses.remove(&id) {
                                let method = request.method.clone();
                                log!(
                                    "[lsp] rcv_error: id={} method={} code={} message={}",
                                    id,
                                    method,
                                    error.code,
                                    error.message
                                );

                                if self.handle_diagnostic_cancellation(&request, error) {
                                    return Ok(None);
                                }
                                if method == "initialize" {
                                    let error = LspError::ServerError(format!(
                                        "language server rejected initialization: {}",
                                        error.message
                                    ));
                                    self.fail_server(error.to_string());
                                    return Ok(Some((
                                        InboundMessage::ProcessingError(error),
                                        Some(method),
                                    )));
                                }

                                return Ok(Some((msg, Some(method))));
                            }
                        }
                    }
                    InboundMessage::ServerRequest(request)
                        if request.method == "window/workDoneProgress/create" =>
                    {
                        self.request_tx
                            .send(OutboundMessage::Response(ServerResponse {
                                id: request.id.clone(),
                                result: Some(Value::Null),
                                error: None,
                            }))
                            .await?;
                        return Ok(None);
                    }
                    InboundMessage::ServerRequest(request)
                        if request.method == "workspace/diagnostic/refresh" =>
                    {
                        self.schedule_workspace_diagnostics();
                        self.request_tx
                            .send(OutboundMessage::Response(ServerResponse {
                                id: request.id.clone(),
                                result: Some(Value::Null),
                                error: None,
                            }))
                            .await?;
                        return Ok(None);
                    }
                    InboundMessage::ServerRequest(request)
                        if request.method == "workspace/configuration" =>
                    {
                        let items = request
                            .params
                            .get("items")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        let settings = self.config.settings.as_ref();
                        let values = items
                            .iter()
                            .map(|item| {
                                let Some(section) = item.get("section").and_then(Value::as_str)
                                else {
                                    return settings.cloned().unwrap_or(Value::Null);
                                };
                                settings
                                    .and_then(|settings| {
                                        section
                                            .split('.')
                                            .try_fold(settings, |value: &Value, part| {
                                                value.get(part)
                                            })
                                    })
                                    .cloned()
                                    .or_else(|| {
                                        settings.and_then(|value| value.get(section)).cloned()
                                    })
                                    .unwrap_or(Value::Null)
                            })
                            .collect::<Vec<_>>();
                        self.request_tx
                            .send(OutboundMessage::Response(ServerResponse {
                                id: request.id.clone(),
                                result: Some(Value::Array(values)),
                                error: None,
                            }))
                            .await?;
                        return Ok(None);
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
                if let InboundMessage::ProcessingError(error) = &msg {
                    if let Some(monitor) = self.process_monitor.as_mut() {
                        monitor.failure_reported = true;
                    }
                    self.fail_server(error.to_string());
                }
                Ok(Some((msg, None)))
            }
            Err(TryRecvError::Empty) => {
                if let Some(error) = self.poll_process_failure() {
                    self.fail_server(error.to_string());
                    return Ok(Some((InboundMessage::ProcessingError(error), None)));
                }
                Ok(None)
            }
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
        crate::editor::perf::increment("edit:lsp_full_text_bytes", contents.len() as u64);
        let uri = file_uri(file)?;
        // Diagnostics are debounced: typing produces a didChange per
        // keystroke, and requesting diagnostics for every one of them floods
        // the server. The request is sent from `recv_response` once the
        // document has been quiet for DIAGNOSTICS_DEBOUNCE.
        self.schedule_diagnostics(&uri);

        // Get or create version for this file
        let version = self.files_versions.entry(uri.clone()).or_insert(0);
        *version += 1;
        let version = *version;

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
                // Once capabilities are known, a full-sync server never needs the
                // previous document contents. Move the caller's allocation directly
                // into the outbound event and release any didOpen-era copy.
                let text = if self.server_capabilities.is_some() {
                    self.files_content.remove(&uri);
                    contents
                } else {
                    let text = contents.clone();
                    self.files_content
                        .insert(uri.clone(), ropey::Rope::from_str(&contents));
                    text
                };
                vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text,
                }]
            }
            TextDocumentSyncKind::Incremental => {
                // Get the old content or empty string if it's the first change
                let old_content = self
                    .files_content
                    .get(&uri)
                    .map(ToString::to_string)
                    .unwrap_or_default();

                // Legacy callers do not supply canonical edit ranges.
                let changes = Self::calculate_changes(&old_content, &contents);
                self.files_content
                    .insert(uri.clone(), ropey::Rope::from_str(&contents));
                changes
            }
            _ => return Ok(()),
        };

        let change_count = content_changes.len();
        let params = did_change_params(&uri, version, content_changes);

        log!(
            "[lsp] did_change file: {} sync_kind: {:?} changes: {}",
            uri,
            sync_kind,
            change_count
        );

        self.send_notification("textDocument/didChange", params, false)
            .await?;

        Ok(())
    }

    async fn did_change_edits(
        &mut self,
        file: &str,
        change: super::DocumentChange,
    ) -> Result<(), LspError> {
        let uri = file_uri(file)?;
        let incremental = matches!(
            self.server_capabilities
                .as_ref()
                .and_then(|caps| caps.text_document_sync.as_ref())
                .and_then(|sync| sync.change_kind()),
            Some(TextDocumentSyncKind::Incremental)
        );
        let matches_before = self.files_content.get(&uri).is_some_and(|previous| {
            previous.is_instance(&change.before) || previous == &change.before
        });
        if !incremental || !matches_before || change.changes.is_empty() {
            return self.did_change(file, change.after.to_string()).await;
        }
        self.schedule_diagnostics(&uri);
        let version = self.files_versions.entry(uri.clone()).or_insert(0);
        *version += 1;
        let version = *version;
        crate::editor::perf::increment("edit:lsp_incremental_changes", change.changes.len() as u64);
        crate::editor::perf::increment(
            "edit:lsp_incremental_bytes",
            change
                .changes
                .iter()
                .map(|edit| edit.text.len() as u64)
                .sum(),
        );
        self.files_content.insert(uri.clone(), change.after);
        self.send_notification(
            "textDocument/didChange",
            did_change_params(&uri, version, change.changes),
            false,
        )
        .await
    }

    async fn did_save(&mut self, file: &str, contents: &str) -> Result<(), LspError> {
        let uri = file_uri(file)?;
        self.pending_diagnostics
            .insert(uri.clone(), Instant::now() + DIAGNOSTICS_DEBOUNCE);
        // Keep the saved snapshot until initialization tells us whether text is needed.
        let mut params = json!({ "textDocument": { "uri": uri }, "text": contents });
        if self.initialized && !prepare_did_save(&mut params, self.server_capabilities.as_ref()) {
            return Ok(());
        }
        self.send_notification("textDocument/didSave", params, false)
            .await
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
        self.signature_help_with_context(file, x, y, None).await
    }

    async fn signature_help_with_context(
        &mut self,
        file: &str,
        x: usize,
        y: usize,
        context: Option<super::SignatureHelpContext>,
    ) -> Result<i64, LspError> {
        let mut params = json!({
            "textDocument": {
                "uri": file_uri(file)?,
            },
            "position": {
                "line": y,
                "character": x,
            }
        });

        if let Some(context) = context {
            params["context"] = serde_json::to_value(context)?;
        }
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
        if self.initialize_failed {
            let Some(mut child) = self.child.take() else {
                return Ok(());
            };
            match child.try_wait() {
                Ok(Some(status)) => {
                    log!(
                        "[lsp] {} was already stopped during shutdown: {}",
                        self.config.command,
                        status
                    );
                }
                Ok(None) => {
                    log!(
                        "[lsp] terminating unavailable language server: {}",
                        self.config.command
                    );
                    if let Err(error) = child.start_kill() {
                        log!(
                            "[lsp] failed to terminate {}: {}",
                            self.config.command,
                            error
                        );
                    } else if let Err(error) = child.wait().await {
                        log!("[lsp] error reaping {}: {}", self.config.command, error);
                    }
                }
                Err(error) => {
                    log!(
                        "[lsp] failed to query {} during shutdown: {}",
                        self.config.command,
                        error
                    );
                }
            }
            return Ok(());
        }

        let shutdown_id = self
            .send_request("shutdown", serde_json::Value::Null, true)
            .await?;
        let response = tokio::time::timeout(SHUTDOWN_RESPONSE_TIMEOUT, async {
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

        let process_wait_started = Instant::now();
        let timeout_future = tokio::time::sleep(PROCESS_EXIT_GRACE);

        // Wait for either timeout or process exit
        tokio::select! {
            _ = timeout_future => {
                log!(
                    "[lsp] {} did not exit within {:?}, forcing termination",
                    self.config.command,
                    PROCESS_EXIT_GRACE
                );
                match child.start_kill() {
                    Ok(()) => {
                        if let Err(error) = child.wait().await {
                            log!("[lsp] error reaping {}: {}", self.config.command, error);
                        }
                    }
                    Err(error) => {
                        log!("[lsp] failed to terminate {}: {}", self.config.command, error);
                    }
                }
            }
            status = child.wait() => {
                match status {
                    Ok(status) => {
                        log!(
                            "[lsp] {} exited naturally after {:?}",
                            self.config.command,
                            process_wait_started.elapsed()
                        );
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
    let body = notification_body(req)?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stdin.write_all(header.as_bytes()).await?;
    stdin.write_all(&body).await?;
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

    #[cfg(unix)]
    #[tokio::test]
    async fn exited_server_fails_initialization_with_stderr_and_preserves_editor_cleanup() {
        let config = LanguageServerConfig {
            command: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                "printf 'error: husk lsp is unavailable\\n' >&2; exit 23".to_string(),
            ],
            language_id: "husk".to_string(),
            file_extensions: vec!["hk".to_string()],
            filenames: Vec::new(),
            documents: Vec::new(),
            root_markers: Vec::new(),
            env: HashMap::new(),
            initialization_options: None,
            settings: None,
            workspace_name: None,
        };
        let mut client = RealLspClient::start(config, std::env::current_dir().unwrap())
            .await
            .unwrap();
        client.initialize().await.unwrap();
        let request_id = client
            .send_request("textDocument/documentSymbol", json!({}), false)
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let process_monitor = client
                    .process_monitor
                    .as_ref()
                    .expect("real LSP client must monitor its child process");
                if process_monitor.stdout_closed.load(Ordering::Acquire)
                    && process_monitor.stderr_closed.load(Ordering::Acquire)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("exited language server pipes must close");

        let (message, method) = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(message) = client.recv_response().await.unwrap() {
                    break message;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("exited language server must report its failure");
        let InboundMessage::ProcessingError(error) = message else {
            panic!("expected exited server to produce a processing error");
        };
        assert!(method.is_none());
        assert!(error.to_string().contains("exit status: 23"));
        assert!(error.to_string().contains("error: husk lsp is unavailable"));

        let (message, method) = client.recv_response().await.unwrap().unwrap();
        let InboundMessage::RequestError { id, error } = message else {
            panic!("expected queued request to fail");
        };
        assert_eq!(id, request_id);
        assert_eq!(method.as_deref(), Some("textDocument/documentSymbol"));
        assert!(error.to_string().contains("error: husk lsp is unavailable"));

        client.did_close("/tmp/unavailable.hk").await.unwrap();
        client.shutdown().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn panic_shaped_stderr_does_not_fail_a_running_server() {
        let config = LanguageServerConfig {
            command: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                "printf \"thread 'main' panicked in a cargo child\\n\" >&2; exec sleep 10"
                    .to_string(),
            ],
            language_id: "rust".to_string(),
            file_extensions: vec!["rs".to_string()],
            filenames: Vec::new(),
            documents: Vec::new(),
            root_markers: Vec::new(),
            env: HashMap::new(),
            initialization_options: None,
            settings: None,
            workspace_name: None,
        };
        let mut client = RealLspClient::start(config, std::env::current_dir().unwrap())
            .await
            .unwrap();

        let (message, method) = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(message) = client.recv_response().await.unwrap() {
                    break message;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("panic-shaped stderr should be surfaced while the server is running");

        assert!(matches!(
            message,
            InboundMessage::ServerStderr(ref message)
                if message.contains("panicked in a cargo child")
        ));
        assert!(method.is_none());
        assert!(!client.initialize_failed);
        assert!(client.child.as_mut().unwrap().try_wait().unwrap().is_none());

        client.child.as_mut().unwrap().start_kill().unwrap();
        client.child.as_mut().unwrap().wait().await.unwrap();
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
        assert_eq!(diag.source.as_deref(), Some("rustc"));
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
            failure_reason: None,
            server_capabilities: None,
            child: None,
            process_monitor: None,
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
    async fn work_done_progress_is_negotiated_created_and_forwarded() {
        let (request_tx, mut requests) = mpsc::channel(4);
        let (responses, response_rx) = mpsc::channel(4);
        let config = default_language_servers().remove("rust").unwrap();
        let mut client = RealLspClient::with_test_channels(
            request_tx,
            response_rx,
            config,
            std::env::current_dir().unwrap(),
        );
        client.initialized = false;

        client.initialize().await.unwrap();

        let Some(OutboundMessage::Request(initialize)) = requests.recv().await else {
            panic!("expected an LSP initialize request");
        };
        assert_eq!(initialize.method, "initialize");
        assert_eq!(
            initialize.params["capabilities"]["window"]["workDoneProgress"],
            json!(true)
        );

        let initialization = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": initialize.id,
            "result": { "capabilities": {} }
        }))
        .unwrap();
        process_lsp_message(&initialization, &responses)
            .await
            .unwrap();
        let Some((InboundMessage::Message(_), method)) = client.recv_response().await.unwrap()
        else {
            panic!("expected the LSP initialization response");
        };
        assert_eq!(method.as_deref(), Some("initialize"));

        let Some(OutboundMessage::Notification(initialized)) = requests.recv().await else {
            panic!("expected the initialized notification");
        };
        assert_eq!(initialized.method, "initialized");

        for token in [json!("rustAnalyzer/Indexing"), json!(7)] {
            let create = serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": token.clone(),
                "method": "window/workDoneProgress/create",
                "params": { "token": token.clone() }
            }))
            .unwrap();
            process_lsp_message(&create, &responses).await.unwrap();

            assert!(client.recv_response().await.unwrap().is_none());
            let Some(OutboundMessage::Response(created)) = requests.recv().await else {
                panic!("expected progress-token creation to receive a response");
            };
            assert_eq!(created.id, token);
            assert_eq!(created.result, Some(Value::Null));
            assert!(created.error.is_none());

            for kind in ["begin", "report", "end"] {
                let progress = serde_json::to_vec(&json!({
                    "jsonrpc": "2.0",
                    "method": "$/progress",
                    "params": {
                        "token": token.clone(),
                        "value": { "kind": kind, "title": "Indexing" }
                    }
                }))
                .unwrap();
                process_lsp_message(&progress, &responses).await.unwrap();

                let Some((
                    InboundMessage::Notification(ParsedNotification::Progress(progress)),
                    method,
                )) = client.recv_response().await.unwrap()
                else {
                    panic!("expected the {kind} progress notification");
                };
                assert!(method.is_none());
                assert_eq!(serde_json::to_value(progress.token).unwrap(), token);
                assert_eq!(progress.value["kind"], kind);
            }
        }

        assert!(requests.try_recv().is_err());
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
        let mut config = default_language_servers()
            .remove("rust")
            .expect("default Rust LSP config must exist");
        config.settings = Some(json!({
            "rust-analyzer": { "cargo": { "allFeatures": true } },
            "flat.key": "literal"
        }));
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
            failure_reason: None,
            server_capabilities: None,
            child: None,
            process_monitor: None,
            config,
            workspace_root: std::env::current_dir().unwrap(),
        };

        let server_request = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 31,
            "method": "workspace/configuration",
            "params": {
                "items": [
                    { "section": "rust-analyzer.cargo.allFeatures" },
                    { "section": "flat.key" },
                    { "section": "missing" },
                    {}
                ]
            }
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
            panic!("expected workspace configuration response");
        };
        assert_eq!(response.id, json!(31));
        assert_eq!(
            response.result,
            Some(json!([
                true,
                "literal",
                null,
                { "rust-analyzer": { "cargo": { "allFeatures": true } }, "flat.key": "literal" }
            ]))
        );
        assert!(response.error.is_none());
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
            failure_reason: None,
            server_capabilities: None,
            child: None,
            process_monitor: None,
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
            failure_reason: None,
            server_capabilities: None,
            child: None,
            process_monitor: None,
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
            failure_reason: None,
            server_capabilities: None,
            child: None,
            process_monitor: None,
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
            failure_reason: None,
            server_capabilities: None,
            child: None,
            process_monitor: None,
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
    async fn invalid_initialize_response_fails_queued_requests_immediately() {
        let (request_tx, _request_rx) = mpsc::channel(4);
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
            failure_reason: None,
            server_capabilities: None,
            child: None,
            process_monitor: None,
            config: default_language_servers()
                .remove("rust")
                .expect("default Rust LSP config must exist"),
            workspace_root: std::env::current_dir().unwrap(),
        };
        let queued_id = client
            .send_request("textDocument/documentSymbol", json!({}), false)
            .await
            .unwrap();
        response_tx
            .send(InboundMessage::Message(ResponseMessage {
                id: 800,
                result: json!({
                    "capabilities": {
                        "textDocumentSync": {
                            "change": "incremental"
                        }
                    }
                }),
                request: None,
            }))
            .await
            .unwrap();

        let (message, method) = client.recv_response().await.unwrap().unwrap();
        let InboundMessage::ProcessingError(error) = message else {
            panic!("expected invalid initialize response to fail the server");
        };
        assert_eq!(method.as_deref(), Some("initialize"));
        assert!(error.to_string().contains("invalid initialize response"));
        assert!(client.initialize_failed);

        let (message, method) = client.recv_response().await.unwrap().unwrap();
        let InboundMessage::RequestError { id, error } = message else {
            panic!("expected queued request to fail");
        };
        assert_eq!(id, queued_id);
        assert_eq!(method.as_deref(), Some("textDocument/documentSymbol"));
        assert!(error.to_string().contains("invalid initialize response"));
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
            failure_reason: None,
            server_capabilities: None,
            child: None,
            process_monitor: None,
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
        assert!(error.to_string().contains("pending queue exceeded"));
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
            failure_reason: None,
            server_capabilities: None,
            child: None,
            process_monitor: None,
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
        assert!(error.to_string().contains("invalid stdout frame"));
    }

    const DIAGNOSTIC_URI: &str = "file:///tmp/diagnostics.rs";

    fn diagnostic_test_client(
        uri: &str,
    ) -> (
        RealLspClient,
        mpsc::Receiver<OutboundMessage>,
        mpsc::Sender<InboundMessage>,
    ) {
        let (request_tx, request_rx) = mpsc::channel(8);
        let (response_tx, response_rx) = mpsc::channel(8);
        let config = default_language_servers().remove("rust").unwrap();
        let mut client = RealLspClient::with_test_channels(
            request_tx,
            response_rx,
            config,
            std::env::current_dir().unwrap(),
        );
        client.server_capabilities = Some(
            serde_json::from_value(json!({
                "diagnosticProvider": {
                    "interFileDependencies": false,
                    "workspaceDiagnostics": false
                }
            }))
            .unwrap(),
        );
        client.files_versions.insert(uri.to_string(), 1);
        (client, request_rx, response_tx)
    }

    fn diagnostic_error(id: i64, code: i64, data: Option<Value>) -> InboundMessage {
        InboundMessage::Error(ResponseError {
            id: Some(id),
            code,
            message: "server cancelled the request".to_string(),
            data,
        })
    }

    #[tokio::test]
    async fn diagnostic_cancellation_retries_quietly_after_the_debounce() {
        for (code, data) in [
            (-32802, Some(json!({ "retriggerRequest": true }))),
            (-32802, None),
            (-32802, Some(json!({}))),
            (-32801, None),
        ] {
            let (mut client, mut requests, responses) = diagnostic_test_client(DIAGNOSTIC_URI);
            let id = client
                .request_diagnostics(DIAGNOSTIC_URI)
                .await
                .unwrap()
                .unwrap();
            let Some(OutboundMessage::Request(original)) = requests.recv().await else {
                panic!("expected diagnostic request");
            };
            responses
                .send(diagnostic_error(id, code, data))
                .await
                .unwrap();

            let before = Instant::now();
            assert!(client.recv_response().await.unwrap().is_none());
            assert!(!client.pending_responses.contains_key(&id));
            assert!(client.pending_diagnostics[DIAGNOSTIC_URI] >= before + DIAGNOSTICS_DEBOUNCE);
            assert!(requests.try_recv().is_err());

            // Expire the deadline directly so the test does not sleep.
            client
                .pending_diagnostics
                .insert(DIAGNOSTIC_URI.to_string(), Instant::now());
            assert!(client.recv_response().await.unwrap().is_none());
            let Some(OutboundMessage::Request(retry)) = requests.recv().await else {
                panic!("expected a fresh diagnostic request");
            };
            assert_ne!(retry.id, id);
            assert_eq!(retry.method, original.method);
            assert_eq!(retry.params, original.params);
            assert!(client.pending_diagnostics.is_empty());

            responses
                .send(InboundMessage::Message(ResponseMessage {
                    id: retry.id,
                    result: json!({ "kind": "full", "items": [] }),
                    request: None,
                }))
                .await
                .unwrap();
            let Some((InboundMessage::Message(result), method)) =
                client.recv_response().await.unwrap()
            else {
                panic!("expected successful retry response");
            };
            assert_eq!(method.as_deref(), Some("textDocument/diagnostic"));
            assert_eq!(result.request.unwrap().params, original.params);
            assert!(client.pending_responses.is_empty());
            assert!(requests.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn diagnostic_cancellation_honors_no_retry() {
        for (code, data) in [
            (-32802, Some(json!({ "retriggerRequest": false }))),
            (-32800, None),
        ] {
            let (mut client, mut requests, responses) = diagnostic_test_client(DIAGNOSTIC_URI);
            let id = client
                .request_diagnostics(DIAGNOSTIC_URI)
                .await
                .unwrap()
                .unwrap();
            requests.recv().await.unwrap();
            responses
                .send(diagnostic_error(id, code, data))
                .await
                .unwrap();

            assert!(client.recv_response().await.unwrap().is_none());
            assert!(client.pending_responses.is_empty());
            assert!(client.pending_diagnostics.is_empty());
            assert!(requests.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn diagnostic_cancellation_preserves_a_later_document_refresh() {
        for retrigger in [true, false] {
            let (mut client, mut requests, responses) = diagnostic_test_client(DIAGNOSTIC_URI);
            let id = client
                .request_diagnostics(DIAGNOSTIC_URI)
                .await
                .unwrap()
                .unwrap();
            requests.recv().await.unwrap();
            let later = Instant::now() + Duration::from_secs(10);
            client
                .pending_diagnostics
                .insert(DIAGNOSTIC_URI.to_string(), later);
            responses
                .send(diagnostic_error(
                    id,
                    -32802,
                    Some(json!({
                        "retriggerRequest": retrigger
                    })),
                ))
                .await
                .unwrap();

            assert!(client.recv_response().await.unwrap().is_none());
            assert_eq!(client.pending_diagnostics[DIAGNOSTIC_URI], later);
            assert!(requests.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn diagnostic_requests_coalesce_per_document_while_in_flight() {
        let (mut client, mut requests, responses) = diagnostic_test_client(DIAGNOSTIC_URI);
        let id = client
            .request_diagnostics(DIAGNOSTIC_URI)
            .await
            .unwrap()
            .unwrap();
        requests.recv().await.unwrap();
        assert!(client
            .request_diagnostics(DIAGNOSTIC_URI)
            .await
            .unwrap()
            .is_none());

        let other_uri = "file:///tmp/other.rs";
        let now = Instant::now();
        client
            .pending_diagnostics
            .insert(DIAGNOSTIC_URI.to_string(), now);
        client
            .pending_diagnostics
            .insert(other_uri.to_string(), now);
        assert!(client.recv_response().await.unwrap().is_none());
        let Some(OutboundMessage::Request(other)) = requests.recv().await else {
            panic!("expected diagnostics for the other document");
        };
        assert_eq!(diagnostic_request_uri(&other), Some(other_uri));
        assert!(requests.try_recv().is_err());
        assert!(client.pending_diagnostics.contains_key(DIAGNOSTIC_URI));

        responses
            .send(InboundMessage::Message(ResponseMessage {
                id,
                result: json!({ "kind": "full", "items": [] }),
                request: None,
            }))
            .await
            .unwrap();
        assert!(matches!(
            client.recv_response().await.unwrap(),
            Some((InboundMessage::Message(_), _))
        ));
        assert!(client.recv_response().await.unwrap().is_none());
        let Some(OutboundMessage::Request(refresh)) = requests.recv().await else {
            panic!("expected one coalesced refresh");
        };
        assert_ne!(refresh.id, id);
        assert_eq!(diagnostic_request_uri(&refresh), Some(DIAGNOSTIC_URI));
        assert!(client.pending_diagnostics.is_empty());
        assert!(requests.try_recv().is_err());
    }

    #[tokio::test]
    async fn diagnostic_cancellation_does_not_resurrect_closed_documents() {
        let path = std::env::temp_dir().join("diagnostics.rs");
        let path = path.to_str().unwrap();
        let uri = file_uri(path).unwrap();
        for close_before_response in [true, false] {
            let (mut client, mut requests, responses) = diagnostic_test_client(&uri);
            let id = client.request_diagnostics(&uri).await.unwrap().unwrap();
            requests.recv().await.unwrap();
            if close_before_response {
                client.did_close(path).await.unwrap();
                requests.recv().await.unwrap();
            }
            responses
                .send(diagnostic_error(id, -32802, None))
                .await
                .unwrap();
            assert!(client.recv_response().await.unwrap().is_none());
            if !close_before_response {
                assert!(client.pending_diagnostics.contains_key(&uri));
                client.did_close(path).await.unwrap();
                requests.recv().await.unwrap();
            }

            assert!(!client.files_versions.contains_key(&uri));
            assert!(client.pending_responses.is_empty());
            assert!(client.pending_diagnostics.is_empty());
            assert!(requests.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn diagnostic_cancellation_handling_preserves_other_request_errors() {
        for (method, code) in [
            ("textDocument/diagnostic", -32603),
            ("textDocument/formatting", -32802),
            ("textDocument/completion", -32802),
            ("workspace/symbol", -32802),
            ("textDocument/inlayHint", -32801),
        ] {
            let (mut client, mut requests, responses) = diagnostic_test_client(DIAGNOSTIC_URI);
            let id = client
                .send_request(
                    method,
                    json!({
                        "textDocument": { "uri": DIAGNOSTIC_URI }
                    }),
                    false,
                )
                .await
                .unwrap();
            requests.recv().await.unwrap();
            responses
                .send(diagnostic_error(
                    id,
                    code,
                    Some(json!({
                        "retriggerRequest": true
                    })),
                ))
                .await
                .unwrap();

            let Some((InboundMessage::Error(error), response_method)) =
                client.recv_response().await.unwrap()
            else {
                panic!("expected error to reach its request owner");
            };
            assert_eq!(error.id, Some(id));
            assert_eq!(error.code, code);
            assert_eq!(response_method.as_deref(), Some(method));
            assert!(client.pending_responses.is_empty());
            assert!(client.pending_diagnostics.is_empty());
            assert!(requests.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn did_save_honors_negotiated_options_and_queued_initialization() {
        for (sync, expected_text) in [
            (json!({ "save": true }), Some(false)),
            (json!({ "save": { "includeText": true } }), Some(true)),
            (json!({ "save": {} }), Some(false)),
            (json!({ "save": false }), None),
            (json!({ "change": 2 }), None),
            (json!(2), Some(false)),
            (json!(0), None),
        ] {
            for queued in [false, true] {
                let (request_tx, mut requests) = mpsc::channel(8);
                let (responses, response_rx) = mpsc::channel(8);
                let config = default_language_servers().remove("rust").unwrap();
                let mut client = RealLspClient::with_test_channels(
                    request_tx,
                    response_rx,
                    config,
                    std::env::current_dir().unwrap(),
                );
                let capabilities = json!({ "textDocumentSync": sync });
                client.initialized = !queued;
                client.server_capabilities = if queued {
                    None
                } else {
                    Some(serde_json::from_value(capabilities.clone()).unwrap())
                };
                if queued {
                    client.initialize().await.unwrap();
                    requests.recv().await.unwrap();
                }
                client.did_open("/tmp/saved.rs", "before").await.unwrap();
                client
                    .did_change("/tmp/saved.rs", "saved".to_string())
                    .await
                    .unwrap();
                client.did_save("/tmp/saved.rs", "saved").await.unwrap();
                if queued {
                    assert!(requests.try_recv().is_err());
                    responses
                        .send(InboundMessage::Message(ResponseMessage {
                            id: client.initialize_id.unwrap(),
                            result: json!({ "capabilities": capabilities }),
                            request: None,
                        }))
                        .await
                        .unwrap();
                    client.recv_response().await.unwrap();
                }
                let mut methods = Vec::new();
                let mut saved = None;
                while let Ok(message) = requests.try_recv() {
                    if let OutboundMessage::Notification(notification) = message {
                        methods.push(notification.method.clone());
                        if notification.method == "textDocument/didSave" {
                            saved = Some(notification.params);
                        }
                    }
                }
                let mut expected = Vec::new();
                if queued {
                    expected.push("initialized");
                }
                expected.extend(["textDocument/didOpen", "textDocument/didChange"]);
                if let Some(include_text) = expected_text {
                    expected.push("textDocument/didSave");
                    let mut params =
                        json!({ "textDocument": { "uri": file_uri("/tmp/saved.rs").unwrap() } });
                    if include_text {
                        params["text"] = json!("saved");
                    }
                    assert_eq!(saved, Some(params));
                } else {
                    assert!(saved.is_none());
                }
                assert_eq!(methods, expected);
            }
        }
    }

    #[tokio::test]
    async fn workspace_diagnostic_refresh_acknowledges_and_requeues_open_documents() {
        let (request_tx, mut requests) = mpsc::channel(4);
        let (responses, response_rx) = mpsc::channel(1);
        let config = default_language_servers().remove("rust").unwrap();
        let mut client = RealLspClient::with_test_channels(
            request_tx,
            response_rx,
            config,
            std::env::current_dir().unwrap(),
        );
        let first = "file:///workspace/src/lib.rs";
        let second = "file:///workspace/src/recap.rs";
        client.files_versions.insert(first.to_string(), 1);
        client.files_versions.insert(second.to_string(), 1);

        responses
            .send(InboundMessage::ServerRequest(ServerRequest {
                id: json!("refresh-1"),
                method: "workspace/diagnostic/refresh".to_string(),
                params: json!(null),
                source: None,
            }))
            .await
            .unwrap();

        assert!(client.recv_response().await.unwrap().is_none());
        let OutboundMessage::Response(response) = requests.recv().await.unwrap() else {
            panic!("workspace diagnostic refresh must receive a JSON-RPC response");
        };
        assert_eq!(response.id, json!("refresh-1"));
        assert_eq!(response.result, Some(Value::Null));
        assert!(response.error.is_none());
        assert_eq!(client.pending_diagnostics.len(), 2);
        assert!(client.pending_diagnostics.contains_key(first));
        assert!(client.pending_diagnostics.contains_key(second));
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
            failure_reason: None,
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
            process_monitor: None,
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
            failure_reason: None,
            server_capabilities: Some(
                serde_json::from_value(json!({ "textDocumentSync": 2 })).unwrap(),
            ),
            child: None,
            process_monitor: None,
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

    #[tokio::test]
    async fn canonical_edits_use_incremental_sync_and_keep_shared_snapshots() {
        let (request_tx, mut request_rx) = mpsc::channel(8);
        let (_response_tx, response_rx) = mpsc::channel(1);
        let config = default_language_servers().remove("rust").unwrap();
        let mut client = RealLspClient::with_test_channels(
            request_tx,
            response_rx,
            config,
            std::env::current_dir().unwrap(),
        );
        client.server_capabilities =
            Some(serde_json::from_value(json!({ "textDocumentSync": 2 })).unwrap());
        let file = "/tmp/canonical-sync.rs";
        let before = ropey::Rope::from_str("a😀b\r\nnext\n");
        let mut after = before.clone();
        after.remove(1..2);
        after.insert(1, "λ");
        client.did_open(file, &before.to_string()).await.unwrap();
        request_rx.recv().await.unwrap();
        client
            .did_change_edits(
                file,
                crate::lsp::DocumentChange {
                    before,
                    after: after.clone(),
                    changes: vec![TextDocumentContentChangeEvent {
                        range: Some(Range {
                            start: Position {
                                line: 0,
                                character: 1,
                            },
                            end: Position {
                                line: 0,
                                character: 3,
                            },
                        }),
                        range_length: None,
                        text: "λ".into(),
                    }],
                },
            )
            .await
            .unwrap();
        let Some(OutboundMessage::Notification(notification)) = request_rx.recv().await else {
            panic!("expected didChange");
        };
        assert_eq!(
            notification.params["contentChanges"],
            json!([{ "range": { "start": { "line": 0, "character": 1 }, "end": { "line": 0, "character": 3 } }, "text": "λ" }])
        );
        assert_eq!(notification.params["textDocument"]["version"], 2);
        assert!(client.files_content[&file_uri(file).unwrap()].is_instance(&after));

        // A stale or newly opened preimage must not receive obsolete ranges.
        client
            .did_change_edits(
                file,
                crate::lsp::DocumentChange {
                    before: ropey::Rope::from_str("stale"),
                    after: ropey::Rope::from_str("latest"),
                    changes: vec![TextDocumentContentChangeEvent {
                        range: Some(Range {
                            start: Position {
                                line: 0,
                                character: 99,
                            },
                            end: Position {
                                line: 0,
                                character: 99,
                            },
                        }),
                        range_length: None,
                        text: "wrong".into(),
                    }],
                },
            )
            .await
            .unwrap();
        let Some(OutboundMessage::Notification(notification)) = request_rx.recv().await else {
            panic!("expected fallback didChange");
        };
        assert_eq!(notification.params["contentChanges"][0]["text"], "latest");
        assert_eq!(
            client.files_content[&file_uri(file).unwrap()].to_string(),
            "latest"
        );
    }

    #[tokio::test]
    async fn full_sync_moves_contents_into_notification_and_releases_cached_copy() {
        let (request_tx, mut request_rx) = mpsc::channel(4);
        let (_response_tx, response_rx) = mpsc::channel(1);
        let config = default_language_servers()
            .remove("rust")
            .expect("default Rust LSP config must exist");
        let mut client = RealLspClient::with_test_channels(
            request_tx,
            response_rx,
            config,
            std::env::current_dir().unwrap(),
        );
        client.server_capabilities =
            Some(serde_json::from_value(json!({ "textDocumentSync": 1 })).unwrap());
        let file = "/tmp/full-sync.rs";
        let uri = file_uri(file).unwrap();
        client.did_open(file, "old").await.unwrap();
        assert_eq!(client.files_content[&uri].to_string(), "old");

        let contents = "updated 👋".repeat(64);
        let contents_ptr = contents.as_ptr();
        client.did_change(file, contents).await.unwrap();

        assert!(!client.files_content.contains_key(&uri));
        assert!(matches!(
            request_rx.recv().await,
            Some(OutboundMessage::Notification(notification))
                if notification.method == "textDocument/didOpen"
        ));
        let Some(OutboundMessage::Notification(notification)) = request_rx.recv().await else {
            panic!("expected didChange notification");
        };
        assert_eq!(notification.method, "textDocument/didChange");
        assert_eq!(notification.params["textDocument"]["version"], 2);
        let text = notification.params["contentChanges"][0]["text"]
            .as_str()
            .unwrap();
        assert_eq!(text, "updated 👋".repeat(64));
        assert_eq!(text.as_ptr(), contents_ptr);
    }

    #[tokio::test]
    async fn pre_initialization_full_sync_retains_latest_contents_for_incremental_server() {
        let (request_tx, mut request_rx) = mpsc::channel(4);
        let (_response_tx, response_rx) = mpsc::channel(1);
        let config = default_language_servers()
            .remove("rust")
            .expect("default Rust LSP config must exist");
        let mut client = RealLspClient::with_test_channels(
            request_tx,
            response_rx,
            config,
            std::env::current_dir().unwrap(),
        );
        let file = "/tmp/pending-sync.rs";
        let uri = file_uri(file).unwrap();
        client.did_open(file, "old").await.unwrap();

        client.did_change(file, "latest".to_string()).await.unwrap();

        assert_eq!(client.files_content[&uri].to_string(), "latest");
        assert!(matches!(
            request_rx.recv().await,
            Some(OutboundMessage::Notification(notification))
                if notification.method == "textDocument/didOpen"
        ));
        let Some(OutboundMessage::Notification(notification)) = request_rx.recv().await else {
            panic!("expected didChange notification");
        };
        assert_eq!(notification.params["contentChanges"][0]["text"], "latest");
    }

    #[test]
    fn notification_body_serializes_the_borrowed_params_without_changing_the_protocol() {
        let request = NotificationRequest {
            method: "textDocument/didChange".to_string(),
            params: json!({
                "textDocument": { "uri": "file:///tmp/test.rs", "version": 2 },
                "contentChanges": [{ "text": "updated 👋" }],
            }),
        };

        let body = notification_body(&request).unwrap();

        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didChange",
                "params": request.params,
            })
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
                "labelDetails": {
                    "detail": "()",
                    "description": "typing"
                },
                "kind": 1
            }
        ]);
        let response = serde_json::from_value::<CompletionResponse>(json).unwrap();

        assert!(!response.is_incomplete());
        let items = response.items();
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0]
                .label_details
                .as_ref()
                .and_then(|details| details.description.as_deref()),
            Some("typing")
        );
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
