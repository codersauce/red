//! Bounded, bidirectional JSON-RPC 2.0 transport for standalone plugin processes.

use std::{
    collections::HashMap,
    fmt,
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    sync::{mpsc, oneshot, watch, Mutex, OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
    time::timeout,
};

pub(crate) const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_QUEUED_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_OUTSTANDING_REQUESTS: usize = 64;
const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024;
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const REAP_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const DOCUMENT_CREATE_METHOD: &str = "red/DocumentCreate";
pub(crate) const DOCUMENT_SHOW_METHOD: &str = "red/DocumentShow";
pub(crate) const DOCUMENT_WATCH_METHOD: &str = "red/DocumentWatch";
pub(crate) const DOCUMENT_UNWATCH_METHOD: &str = "red/DocumentUnwatch";
pub(crate) const DOCUMENT_EVENT_METHOD: &str = "document/event";
pub(crate) const DOCUMENT_SNAPSHOT_METHOD: &str = "red/DocumentSnapshot";
pub(crate) const DOCUMENT_APPLY_METHOD: &str = "red/DocumentApply";
pub(crate) const DOCUMENT_UNDO_METHOD: &str = "red/DocumentUndo";
pub(crate) const CONFIG_GET_METHOD: &str = "red/ConfigGet";
pub(crate) const STORAGE_GET_METHOD: &str = "red/StorageGet";
pub(crate) const STORAGE_SET_METHOD: &str = "red/StorageSet";
pub(crate) const STORAGE_DELETE_METHOD: &str = "red/StorageDelete";
pub(crate) const TEXT_PANEL_CREATE_METHOD: &str = "red/TextPanelCreate";
pub(crate) const TEXT_PANEL_SET_CONTENT_METHOD: &str = "red/TextPanelSetContent";
pub(crate) const TEXT_PANEL_FOCUS_METHOD: &str = "red/TextPanelFocus";
pub(crate) const TEXT_PANEL_DISPOSE_METHOD: &str = "red/TextPanelDispose";
pub(crate) const PANEL_EVENT_METHOD: &str = "panel/event";
pub(crate) const CLIPBOARD_WRITE_METHOD: &str = "red/ClipboardWrite";
pub(crate) const STATUSLINE_SET_METHOD: &str = "red/StatuslineSet";
pub(crate) const STATUSLINE_CLEAR_METHOD: &str = "red/StatuslineClear";
pub(crate) const PICKER_OPEN_METHOD: &str = "red/PickerOpen";
pub(crate) const PICKER_EVENT_METHOD: &str = "picker/event";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RpcStatuslineSide {
    Left,
    #[default]
    Right,
}

/// Initialization parameters sent before an RPC plugin becomes active.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RpcInitializeParams {
    pub protocol_versions: Vec<u32>,
    pub red_api_version: String,
    pub plugin_id: String,
    pub host_methods: Vec<String>,
}

/// JSON-RPC error object used in either direction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcErrorObject {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcErrorObject {
    pub(crate) fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("host method not found: {method}"),
            data: None,
        }
    }

    pub(crate) fn response_too_large(method: &str) -> Self {
        Self {
            code: -32006,
            message: format!(
                "{method} response exceeds the {MAX_FRAME_BYTES}-byte RPC frame limit"
            ),
            data: None,
        }
    }
}

impl fmt::Display for RpcErrorObject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "RPC error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for RpcErrorObject {}

/// One host request received from the plugin process.
pub struct RpcHostRequest {
    pub method: String,
    pub params: Value,
    id: Value,
    writer: RpcWriter,
    _request_slot: OwnedSemaphorePermit,
    _byte_budget: OwnedSemaphorePermit,
}

impl RpcHostRequest {
    /// Sends exactly one result or error for this request.
    pub async fn respond(self, result: std::result::Result<Value, RpcErrorObject>) -> Result<()> {
        match result {
            Ok(result) => {
                let id = self.id;
                let response = RpcWriter::serialize(
                    json!({"jsonrpc": "2.0", "id": id.clone(), "result": result}),
                )?;
                if response.len() > MAX_FRAME_BYTES {
                    let error = RpcErrorObject::response_too_large(&self.method);
                    self.writer
                        .enqueue(json!({"jsonrpc": "2.0", "id": id, "error": error}))
                } else {
                    self.writer.enqueue_bytes(response)
                }
            }
            Err(error) => self
                .writer
                .enqueue(json!({"jsonrpc": "2.0", "id": self.id, "error": error})),
        }
    }
}

/// One notification received from the plugin process.
pub struct RpcNotification {
    pub method: String,
    pub params: Value,
    _byte_budget: OwnedSemaphorePermit,
}

/// Inbound plugin messages that require host-side handling.
pub enum RpcIncoming {
    Request(RpcHostRequest),
    Notification(RpcNotification),
}

/// Result of a graceful connection shutdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcShutdown {
    pub forced: bool,
    pub diagnostics: String,
}

struct PendingRequest {
    response: oneshot::Sender<ResponseOutcome>,
}

enum ResponseOutcome {
    Result(Value),
    Error(RpcErrorObject),
    Disconnected(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConnectionPhase {
    Starting,
    Initializing,
    Ready,
    ShuttingDown,
    Failed,
    Closed,
}

struct ProtocolState {
    phase: ConnectionPhase,
    next_id: u64,
    pending: HashMap<String, PendingRequest>,
}

#[derive(Debug, Clone)]
struct CloseReport {
    reason: Option<String>,
    forced: bool,
    diagnostics: String,
}

enum Stop {
    Failure(String),
    Shutdown(Duration),
}

struct ConnectionState {
    protocol: Mutex<ProtocolState>,
    stop: mpsc::UnboundedSender<Stop>,
    closed: watch::Sender<Option<CloseReport>>,
}

impl ConnectionState {
    fn signal_failure(&self, reason: impl Into<String>) {
        let _ = self.stop.send(Stop::Failure(reason.into()));
    }

    async fn reserve_request(
        &self,
        method: &str,
        response: oneshot::Sender<ResponseOutcome>,
    ) -> Result<String> {
        let mut protocol = self.protocol.lock().await;
        match (protocol.phase, method) {
            (ConnectionPhase::Starting, "initialize") => {
                protocol.phase = ConnectionPhase::Initializing;
            }
            (ConnectionPhase::Ready, _) => {
                anyhow::ensure!(
                    method != "initialize",
                    "RPC connection was initialized more than once"
                );
            }
            _ => anyhow::bail!("RPC connection is not accepting `{method}` requests"),
        }
        anyhow::ensure!(
            protocol.pending.len() < MAX_OUTSTANDING_REQUESTS,
            "RPC connection already has {MAX_OUTSTANDING_REQUESTS} outstanding requests"
        );
        let id = protocol.next_id.to_string();
        protocol.next_id = protocol
            .next_id
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("RPC request id space is exhausted"))?;
        protocol
            .pending
            .insert(format!("s:{id}"), PendingRequest { response });
        Ok(id)
    }

    async fn remove_pending(&self, id: &str) -> Option<PendingRequest> {
        self.protocol.lock().await.pending.remove(id)
    }

    async fn begin_shutdown(&self) -> Result<()> {
        let mut protocol = self.protocol.lock().await;
        anyhow::ensure!(
            protocol.phase == ConnectionPhase::Ready,
            "RPC connection is not ready to shut down"
        );
        protocol.phase = ConnectionPhase::ShuttingDown;
        Ok(())
    }

    async fn mark_ready(&self) -> Result<()> {
        let mut protocol = self.protocol.lock().await;
        anyhow::ensure!(
            protocol.phase == ConnectionPhase::Initializing,
            "RPC connection was initialized more than once"
        );
        protocol.phase = ConnectionPhase::Ready;
        Ok(())
    }

    async fn ensure_ready(&self) -> Result<()> {
        anyhow::ensure!(
            self.protocol.lock().await.phase == ConnectionPhase::Ready,
            "RPC plugin sent a host call before initialization"
        );
        Ok(())
    }

    async fn is_shutting_down(&self) -> bool {
        self.protocol.lock().await.phase == ConnectionPhase::ShuttingDown
    }

    async fn fail(&self, reason: impl Into<String>) -> CloseReport {
        let reason = reason.into();
        let pending = {
            let mut protocol = self.protocol.lock().await;
            if matches!(
                protocol.phase,
                ConnectionPhase::Failed | ConnectionPhase::Closed
            ) {
                None
            } else {
                protocol.phase = ConnectionPhase::Failed;
                Some(std::mem::take(&mut protocol.pending))
            }
        };
        if let Some(pending) = pending {
            for request in pending.into_values() {
                let _ = request
                    .response
                    .send(ResponseOutcome::Disconnected(reason.clone()));
            }
            let _ = self.stop.send(Stop::Failure(reason));
        }
        self.wait_closed().await
    }

    async fn finish(&self, report: CloseReport) {
        let pending = {
            let mut protocol = self.protocol.lock().await;
            protocol.phase = ConnectionPhase::Closed;
            std::mem::take(&mut protocol.pending)
        };
        let message = report
            .reason
            .clone()
            .unwrap_or_else(|| "RPC connection closed".to_string());
        for request in pending.into_values() {
            let _ = request
                .response
                .send(ResponseOutcome::Disconnected(message.clone()));
        }
        self.closed.send_replace(Some(report));
    }

    async fn wait_closed(&self) -> CloseReport {
        let mut closed = self.closed.subscribe();
        loop {
            if let Some(report) = closed.borrow().clone() {
                return report;
            }
            if closed.changed().await.is_err() {
                return CloseReport {
                    reason: Some("RPC supervisor stopped without a report".to_string()),
                    forced: true,
                    diagnostics: String::new(),
                };
            }
        }
    }
}

struct OutboundFrame {
    bytes: Vec<u8>,
    written: Option<oneshot::Sender<std::result::Result<(), String>>>,
    _byte_budget: OwnedSemaphorePermit,
}

#[derive(Clone)]
struct RpcWriter {
    frames: mpsc::Sender<OutboundFrame>,
    byte_budget: Arc<Semaphore>,
    state: Arc<ConnectionState>,
}

impl RpcWriter {
    fn serialize(value: Value) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    fn encode(value: Value) -> Result<Vec<u8>> {
        let bytes = Self::serialize(value)?;
        anyhow::ensure!(
            bytes.len() <= MAX_FRAME_BYTES,
            "RPC frame exceeds {MAX_FRAME_BYTES} bytes"
        );
        Ok(bytes)
    }

    fn enqueue(&self, value: Value) -> Result<()> {
        let bytes = Self::encode(value).inspect_err(|error| {
            self.state.signal_failure(error.to_string());
        })?;
        self.enqueue_bytes(bytes)
    }

    fn enqueue_bytes(&self, bytes: Vec<u8>) -> Result<()> {
        let result = (|| {
            let permits = u32::try_from(bytes.len()).expect("frame limit fits in u32");
            let byte_budget = self
                .byte_budget
                .clone()
                .try_acquire_many_owned(permits)
                .map_err(|_| anyhow::anyhow!("RPC outbound byte budget is exhausted"))?;
            self.frames
                .try_send(OutboundFrame {
                    bytes,
                    written: None,
                    _byte_budget: byte_budget,
                })
                .map_err(|error| anyhow::anyhow!("RPC writer queue rejected a frame: {error}"))
        })();
        if let Err(error) = &result {
            self.state.signal_failure(error.to_string());
        }
        result
    }

    async fn send(&self, value: Value, deadline: Duration) -> Result<()> {
        let operation = async {
            let bytes = Self::encode(value)?;
            let permits = u32::try_from(bytes.len()).expect("frame limit fits in u32");
            let byte_budget = self.byte_budget.clone().acquire_many_owned(permits).await?;
            let (written, acknowledgement) = oneshot::channel();
            self.frames
                .send(OutboundFrame {
                    bytes,
                    written: Some(written),
                    _byte_budget: byte_budget,
                })
                .await
                .map_err(|_| anyhow::anyhow!("RPC writer stopped"))?;
            acknowledgement
                .await
                .context("RPC writer dropped a frame")?
                .map_err(anyhow::Error::msg)
        };
        match timeout(deadline, operation).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                let report = self.state.fail(error.to_string()).await;
                Err(close_error(report))
            }
            Err(_) => {
                let report = self
                    .state
                    .fail(format!(
                        "RPC write timed out after {} ms",
                        deadline.as_millis()
                    ))
                    .await;
                Err(close_error(report))
            }
        }
    }
}

/// One supervised, bidirectional RPC plugin process.
pub struct RpcConnection {
    writer: RpcWriter,
    state: Arc<ConnectionState>,
    reader_task: JoinHandle<()>,
    writer_task: JoinHandle<()>,
    supervisor_task: Option<JoinHandle<()>>,
}

/// Cloneable request handle that keeps process ownership in [`RpcConnection`].
#[derive(Clone)]
pub(crate) struct RpcRequester {
    writer: RpcWriter,
    state: Arc<ConnectionState>,
}

pub(crate) struct RpcPendingResponse {
    receiver: oneshot::Receiver<ResponseOutcome>,
    state: Arc<ConnectionState>,
    key: String,
    method: String,
    deadline: Duration,
}

impl RpcPendingResponse {
    pub(crate) async fn wait(self) -> Result<Value> {
        receive_response(
            &self.state,
            &self.key,
            &self.method,
            self.receiver,
            self.deadline,
        )
        .await
    }
}

impl RpcRequester {
    pub(crate) async fn request(
        &self,
        method: &str,
        params: Value,
        deadline: Duration,
    ) -> Result<Value> {
        request(&self.writer, &self.state, method, params, deadline).await
    }

    pub(crate) async fn begin_request(
        &self,
        method: &str,
        params: Value,
        deadline: Duration,
    ) -> Result<RpcPendingResponse> {
        anyhow::ensure!(!method.trim().is_empty(), "RPC method is empty");
        ensure_named_params(&params)?;
        let (response, receiver) = oneshot::channel();
        let id = match self.state.reserve_request(method, response).await {
            Ok(id) => id,
            Err(error) => {
                self.state.signal_failure(error.to_string());
                return Err(error);
            }
        };
        let key = format!("s:{id}");
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if let Err(error) = self.writer.enqueue(frame) {
            self.state.remove_pending(&key).await;
            return Err(error);
        }
        Ok(RpcPendingResponse {
            receiver,
            state: Arc::clone(&self.state),
            key,
            method: method.to_string(),
            deadline,
        })
    }
}

impl RpcConnection {
    /// Spawns an already configured plugin command with piped standard streams.
    pub fn spawn(mut command: Command) -> Result<(Self, mpsc::Receiver<RpcIncoming>)> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("failed to start RPC plugin")?;
        let stdin = child
            .stdin
            .take()
            .context("RPC plugin stdin was not captured")?;
        let stdout = child
            .stdout
            .take()
            .context("RPC plugin stdout was not captured")?;
        let stderr = child
            .stderr
            .take()
            .context("RPC plugin stderr was not captured")?;
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let (stop, stop_requests) = mpsc::unbounded_channel();
        let (closed, _) = watch::channel(None);
        let state = Arc::new(ConnectionState {
            protocol: Mutex::new(ProtocolState {
                phase: ConnectionPhase::Starting,
                next_id: 1,
                pending: HashMap::new(),
            }),
            stop,
            closed,
        });
        let (frames, outbound) = mpsc::channel(MAX_OUTSTANDING_REQUESTS);
        let writer = RpcWriter {
            frames,
            byte_budget: Arc::new(Semaphore::new(MAX_QUEUED_BYTES)),
            state: Arc::clone(&state),
        };
        let (incoming, requests) = mpsc::channel(MAX_OUTSTANDING_REQUESTS);
        let reader_task = tokio::spawn(read_messages(
            stdout,
            incoming,
            Arc::new(Semaphore::new(MAX_OUTSTANDING_REQUESTS)),
            Arc::new(Semaphore::new(MAX_QUEUED_BYTES)),
            writer.clone(),
            Arc::clone(&state),
        ));
        let writer_task = tokio::spawn(write_messages(stdin, outbound, Arc::clone(&state)));
        let supervisor_task = tokio::spawn(supervise_child(
            child,
            stop_requests,
            Arc::clone(&state),
            stderr,
            diagnostics,
        ));
        Ok((
            Self {
                writer,
                state,
                reader_task,
                writer_task,
                supervisor_task: Some(supervisor_task),
            },
            requests,
        ))
    }

    /// Sends one request and correlates its response within the same deadline.
    pub async fn request(&self, method: &str, params: Value, deadline: Duration) -> Result<Value> {
        request(&self.writer, &self.state, method, params, deadline).await
    }

    pub(crate) fn requester(&self) -> RpcRequester {
        RpcRequester {
            writer: self.writer.clone(),
            state: Arc::clone(&self.state),
        }
    }

    pub(crate) fn notification_size(method: &str, params: &Value) -> Result<usize> {
        anyhow::ensure!(!method.trim().is_empty(), "RPC method is empty");
        ensure_named_params(params)?;
        Ok(RpcWriter::encode(json!({"jsonrpc": "2.0", "method": method, "params": params}))?.len())
    }

    /// Sends one notification without creating a pending response.
    pub async fn notify(&self, method: &str, params: Value) -> Result<()> {
        anyhow::ensure!(!method.trim().is_empty(), "RPC method is empty");
        ensure_named_params(&params)?;
        self.state.ensure_ready().await?;
        self.writer
            .enqueue(json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    /// Negotiates protocol version 1 and emits `initialized` after validation.
    pub async fn initialize(
        &self,
        params: RpcInitializeParams,
        deadline: Duration,
    ) -> Result<Value> {
        let result = self.request_initialize(params, deadline).await?;
        self.finish_initialize().await?;
        Ok(result)
    }

    pub(crate) async fn request_initialize(
        &self,
        params: RpcInitializeParams,
        deadline: Duration,
    ) -> Result<Value> {
        let result = self
            .request("initialize", serde_json::to_value(params)?, deadline)
            .await?;
        if result.get("protocol_version").and_then(Value::as_u64) != Some(1) {
            let report = self
                .state
                .fail("RPC plugin did not select protocol version 1")
                .await;
            return Err(close_error(report));
        }
        Ok(result)
    }

    pub(crate) async fn finish_initialize(&self) -> Result<()> {
        self.state.mark_ready().await?;
        self.writer
            .enqueue(json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}))?;
        Ok(())
    }

    /// Requests graceful shutdown, then terminates and reaps after the grace period.
    pub async fn shutdown(
        mut self,
        request_deadline: Duration,
        grace: Duration,
    ) -> Result<RpcShutdown> {
        self.request("shutdown", Value::Null, request_deadline)
            .await?;
        self.state.begin_shutdown().await?;
        self.writer
            .send(
                json!({"jsonrpc": "2.0", "method": "exit", "params": Value::Null}),
                WRITE_TIMEOUT,
            )
            .await?;
        let _ = self.state.stop.send(Stop::Shutdown(grace));
        let report = self.state.wait_closed().await;
        self.reader_task.abort();
        self.writer_task.abort();
        if let Some(supervisor) = self.supervisor_task.take() {
            let _ = supervisor.await;
        }
        if let Some(reason) = report.reason {
            anyhow::bail!(format_close_message(&reason, &report.diagnostics));
        }
        Ok(RpcShutdown {
            forced: report.forced,
            diagnostics: report.diagnostics,
        })
    }
}

async fn request(
    writer: &RpcWriter,
    state: &Arc<ConnectionState>,
    method: &str,
    params: Value,
    deadline: Duration,
) -> Result<Value> {
    RpcRequester {
        writer: writer.clone(),
        state: Arc::clone(state),
    }
    .begin_request(method, params, deadline)
    .await?
    .wait()
    .await
}

async fn receive_response(
    state: &Arc<ConnectionState>,
    key: &str,
    method: &str,
    receiver: oneshot::Receiver<ResponseOutcome>,
    deadline: Duration,
) -> Result<Value> {
    match timeout(deadline, receiver).await {
        Ok(Ok(ResponseOutcome::Result(result))) => Ok(result),
        Ok(Ok(ResponseOutcome::Error(error))) => Err(error.into()),
        Ok(Ok(ResponseOutcome::Disconnected(reason))) => {
            let report = state.wait_closed().await;
            Err(anyhow::anyhow!(format_close_message(
                &reason,
                &report.diagnostics
            )))
        }
        Ok(Err(error)) => {
            state.remove_pending(key).await;
            let report = state.fail(error.to_string()).await;
            Err(close_error(report))
        }
        Err(_) => {
            state.remove_pending(key).await;
            let report = state
                .fail(format!(
                    "RPC request `{method}` timed out after {} ms",
                    deadline.as_millis()
                ))
                .await;
            Err(close_error(report))
        }
    }
}

impl Drop for RpcConnection {
    fn drop(&mut self) {
        self.reader_task.abort();
        self.writer_task.abort();
        let _ = self
            .state
            .stop
            .send(Stop::Failure("RPC connection dropped".to_string()));
    }
}

async fn write_messages(
    mut stdin: ChildStdin,
    mut frames: mpsc::Receiver<OutboundFrame>,
    state: Arc<ConnectionState>,
) {
    while let Some(frame) = frames.recv().await {
        let write = async {
            stdin.write_all(&frame.bytes).await?;
            stdin.flush().await
        };
        let result = match timeout(WRITE_TIMEOUT, write).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(format!("failed to write RPC frame: {error}")),
            Err(_) => Err(format!(
                "RPC frame write timed out after {} ms",
                WRITE_TIMEOUT.as_millis()
            )),
        };
        let failed = result.as_ref().err().cloned();
        if let Some(written) = frame.written {
            let _ = written.send(result);
        }
        if let Some(reason) = failed {
            state.fail(reason).await;
            return;
        }
    }
}

async fn read_messages(
    stdout: ChildStdout,
    incoming: mpsc::Sender<RpcIncoming>,
    request_slots: Arc<Semaphore>,
    byte_budget: Arc<Semaphore>,
    writer: RpcWriter,
    state: Arc<ConnectionState>,
) {
    let mut stdout = BufReader::new(stdout);
    let mut frame = Vec::new();
    loop {
        let result = async {
            let Some(frame_len) =
                read_bounded_frame(&mut stdout, &mut frame, MAX_FRAME_BYTES).await?
            else {
                if state.is_shutting_down().await {
                    return Ok(());
                }
                anyhow::bail!("RPC plugin closed stdout");
            };
            let message = decode_frame(&frame, &writer).await?;
            route_message(
                message,
                frame_len,
                &incoming,
                &request_slots,
                &byte_budget,
                &writer,
                &state,
            )
            .await?;
            frame.clear();
            Ok::<(), anyhow::Error>(())
        }
        .await;
        if let Err(error) = result {
            state.fail(error.to_string()).await;
            return;
        }
    }
}

async fn read_bounded_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    frame: &mut Vec<u8>,
    limit: usize,
) -> Result<Option<usize>> {
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            anyhow::ensure!(frame.is_empty(), "RPC frame ended before a newline");
            return Ok(None);
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        anyhow::ensure!(
            frame.len().saturating_add(consumed) <= limit,
            "RPC frame exceeds {limit} bytes"
        );
        frame.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(Some(frame.len()));
        }
    }
}

enum ParsedMessage {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    Response {
        key: String,
        outcome: ResponseOutcome,
    },
}

async fn decode_frame(frame: &[u8], writer: &RpcWriter) -> Result<ParsedMessage> {
    let body = frame.strip_suffix(b"\n").unwrap_or(frame);
    let body = body.strip_suffix(b"\r").unwrap_or(body);
    let value: Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(error) => {
            let _ = writer
                .send(
                    error_response(Value::Null, -32700, "parse error"),
                    WRITE_TIMEOUT,
                )
                .await;
            return Err(error).context("RPC plugin returned invalid JSON");
        }
    };
    let id = value.get("id").cloned().unwrap_or(Value::Null);
    match classify_envelope(value) {
        Ok(message) => Ok(message),
        Err(error) => {
            let _ = writer
                .send(error_response(id, -32600, "invalid request"), WRITE_TIMEOUT)
                .await;
            Err(error)
        }
    }
}

fn classify_envelope(value: Value) -> Result<ParsedMessage> {
    let Value::Object(mut object) = value else {
        anyhow::bail!("RPC message must be an object");
    };
    anyhow::ensure!(
        object.remove("jsonrpc").as_ref().and_then(Value::as_str) == Some("2.0"),
        "RPC message has an invalid jsonrpc version"
    );
    if let Some(method) = object.remove("method") {
        return classify_request(object, method);
    }
    classify_response(object)
}

fn classify_request(mut object: Map<String, Value>, method: Value) -> Result<ParsedMessage> {
    anyhow::ensure!(
        !object.contains_key("result") && !object.contains_key("error"),
        "RPC request contains response fields"
    );
    let method = method
        .as_str()
        .filter(|method| !method.is_empty())
        .ok_or_else(|| anyhow::anyhow!("RPC method must be a nonempty string"))?
        .to_string();
    let params = object.remove("params").unwrap_or(Value::Null);
    ensure_named_params(&params)?;
    if let Some(id) = object.remove("id") {
        rpc_id_key(&id)?;
        Ok(ParsedMessage::Request { id, method, params })
    } else {
        Ok(ParsedMessage::Notification { method, params })
    }
}

fn classify_response(mut object: Map<String, Value>) -> Result<ParsedMessage> {
    let id = object
        .remove("id")
        .ok_or_else(|| anyhow::anyhow!("RPC response is missing an id"))?;
    let key = rpc_id_key(&id)?;
    let result = object.remove("result");
    let error = object.remove("error");
    let outcome = match (result, error) {
        (Some(result), None) => ResponseOutcome::Result(result),
        (None, Some(error)) => ResponseOutcome::Error(
            serde_json::from_value(error).context("RPC response has an invalid error object")?,
        ),
        _ => anyhow::bail!("RPC response must contain exactly one result or error"),
    };
    Ok(ParsedMessage::Response { key, outcome })
}

fn rpc_id_key(id: &Value) -> Result<String> {
    match id {
        Value::String(value) => Ok(format!("s:{value}")),
        Value::Number(value) => Ok(format!("n:{value}")),
        _ => anyhow::bail!("RPC id must be a string or number"),
    }
}

fn ensure_named_params(params: &Value) -> Result<()> {
    anyhow::ensure!(
        params.is_null() || params.is_object(),
        "RPC params must be an object or null"
    );
    Ok(())
}

async fn route_message(
    message: ParsedMessage,
    frame_len: usize,
    incoming: &mpsc::Sender<RpcIncoming>,
    request_slots: &Arc<Semaphore>,
    byte_budget: &Arc<Semaphore>,
    writer: &RpcWriter,
    state: &ConnectionState,
) -> Result<()> {
    if let ParsedMessage::Response { key, outcome } = message {
        if let Some(pending) = state.remove_pending(&key).await {
            let _ = pending.response.send(outcome);
        }
        return Ok(());
    }
    state.ensure_ready().await?;
    let permits = u32::try_from(frame_len).expect("frame limit fits in u32");
    let bytes = byte_budget
        .clone()
        .try_acquire_many_owned(permits)
        .map_err(|_| anyhow::anyhow!("RPC inbound byte budget is exhausted"))?;
    let incoming_message = match message {
        ParsedMessage::Request { id, method, params } => {
            let request_slot = request_slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| anyhow::anyhow!("RPC has too many outstanding host requests"))?;
            RpcIncoming::Request(RpcHostRequest {
                method,
                params,
                id,
                writer: writer.clone(),
                _request_slot: request_slot,
                _byte_budget: bytes,
            })
        }
        ParsedMessage::Notification { method, params } => {
            RpcIncoming::Notification(RpcNotification {
                method,
                params,
                _byte_budget: bytes,
            })
        }
        ParsedMessage::Response { .. } => unreachable!("responses return above"),
    };
    incoming
        .try_send(incoming_message)
        .map_err(|_| anyhow::anyhow!("RPC incoming queue is full"))
}

async fn supervise_child(
    mut child: Child,
    mut stop: mpsc::UnboundedReceiver<Stop>,
    state: Arc<ConnectionState>,
    stderr: ChildStderr,
    diagnostics: Arc<Mutex<Vec<u8>>>,
) {
    let mut diagnostics_task = tokio::spawn(retain_diagnostics(stderr, Arc::clone(&diagnostics)));
    let (reason, forced) = tokio::select! {
        status = child.wait() => {
            let expected = state.protocol.lock().await.phase == ConnectionPhase::ShuttingDown;
            (child_exit_reason(status, expected), false)
        }
        request = stop.recv() => match request {
            Some(Stop::Failure(reason)) => {
                terminate_child(&mut child).await;
                (Some(reason), true)
            }
            Some(Stop::Shutdown(grace)) => {
                match timeout(grace, child.wait()).await {
                    Ok(status) => (child_exit_reason(status, true), false),
                    Err(_) => {
                        terminate_child(&mut child).await;
                        (None, true)
                    }
                }
            }
            None => {
                terminate_child(&mut child).await;
                (Some("RPC connection lost its supervisor".to_string()), true)
            }
        }
    };
    if timeout(REAP_TIMEOUT, &mut diagnostics_task).await.is_err() {
        diagnostics_task.abort();
    }
    let diagnostics = String::from_utf8_lossy(&diagnostics.lock().await)
        .trim()
        .to_string();
    state
        .finish(CloseReport {
            reason,
            forced,
            diagnostics,
        })
        .await;
}

fn child_exit_reason(status: std::io::Result<ExitStatus>, expected: bool) -> Option<String> {
    match status {
        Ok(status) if expected && status.success() => None,
        Ok(status) => Some(format!("RPC plugin exited with {status}")),
        Err(error) => Some(format!("failed to wait for RPC plugin: {error}")),
    }
}

async fn terminate_child(child: &mut Child) {
    let _ = child.start_kill();
    if timeout(REAP_TIMEOUT, child.wait()).await.is_err() {
        let _ = child.kill().await;
    }
}

async fn retain_diagnostics(mut stderr: ChildStderr, diagnostics: Arc<Mutex<Vec<u8>>>) {
    let mut chunk = [0_u8; 1024];
    while let Ok(read) = stderr.read(&mut chunk).await {
        if read == 0 {
            return;
        }
        let mut retained = diagnostics.lock().await;
        retained.extend_from_slice(&chunk[..read]);
        if retained.len() > MAX_DIAGNOSTIC_BYTES {
            let overflow = retained.len() - MAX_DIAGNOSTIC_BYTES;
            retained.drain(..overflow);
        }
    }
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message},
    })
}

fn close_error(report: CloseReport) -> anyhow::Error {
    let reason = report.reason.as_deref().unwrap_or("RPC connection closed");
    anyhow::anyhow!(format_close_message(reason, &report.diagnostics))
}

fn format_close_message(reason: &str, diagnostics: &str) -> String {
    if diagnostics.is_empty() {
        reason.to_string()
    } else {
        format!("{reason}: {diagnostics}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn bounded_reader_rejects_a_frame_before_growing_past_the_limit() {
        let (mut writer, reader) = duplex(16);
        writer.write_all(b"123456789").await.unwrap();
        drop(writer);
        let mut reader = BufReader::new(reader);
        let mut frame = Vec::new();

        let error = read_bounded_frame(&mut reader, &mut frame, 8)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("exceeds 8 bytes"));
        assert!(frame.len() <= 8);
    }

    #[test]
    fn protocol_v1_envelope_conformance() {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Case {
            expected: String,
            message: Value,
        }

        for line in include_str!("../../tests/fixtures/rpc_v1_envelopes.jsonl").lines() {
            let case: Case = serde_json::from_str(line).unwrap();
            let actual = match classify_envelope(case.message) {
                Ok(ParsedMessage::Request { .. }) => "request",
                Ok(ParsedMessage::Notification { .. }) => "notification",
                Ok(ParsedMessage::Response { .. }) => "response",
                Err(_) => "invalid",
            };
            assert_eq!(actual, case.expected, "{line}");
        }
    }
}
