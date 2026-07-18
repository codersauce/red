//! Bounded JSON-RPC transport and ACP child-process lifecycle.

use std::{
    collections::{HashMap, VecDeque},
    ffi::OsString,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use agent_client_protocol_schema::v1::{
    AuthenticateRequest, AuthenticateResponse, CancelNotification, ClientNotification,
    ClientRequest, CloseSessionResponse, PermissionOptionId, ReadTextFileRequest,
    ReadTextFileResponse, RequestId, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionId, SessionNotification,
    SessionUpdate, StopReason, WriteTextFileRequest, WriteTextFileResponse,
};
use async_trait::async_trait;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter},
    process::{Child, ChildStdin, Command},
    sync::{mpsc, oneshot, Mutex},
    task::JoinHandle,
    time::timeout,
};

use super::{
    initialize_request, AcpBridge, AcpCodec, BridgeCommand, BridgeEvent, BridgeWireMessage,
    MAX_MESSAGE_BYTES, WIRE_PROTOCOL_VERSION,
};

const DEFAULT_QUEUE_CAPACITY: usize = 32;
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const CLOSE_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CANCELLED_SESSIONS: usize = 256;

/// Launch configuration for an ACP adapter executable.
#[derive(Debug, Clone)]
pub struct AcpProcessSpec {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub current_dir: Option<PathBuf>,
    pub environment: HashMap<OsString, OsString>,
    pub authentication_method: Option<String>,
    pub queue_capacity: usize,
    pub request_timeout: Duration,
    pub write_timeout: Duration,
    pub shutdown_timeout: Duration,
}

impl AcpProcessSpec {
    #[must_use]
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            current_dir: None,
            environment: HashMap::new(),
            authentication_method: None,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            write_timeout: DEFAULT_WRITE_TIMEOUT,
            shutdown_timeout: DEFAULT_SHUTDOWN_TIMEOUT,
        }
    }

    #[must_use]
    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    #[must_use]
    pub fn current_dir(mut self, current_dir: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(current_dir.into());
        self
    }

    #[must_use]
    pub fn authentication_method(mut self, method_id: impl Into<String>) -> Self {
        self.authentication_method = Some(method_id.into());
        self
    }
}

/// Host callbacks for requests and notifications initiated by an ACP agent.
#[async_trait]
pub trait AcpHost: Send + 'static {
    async fn read_text_file(
        &mut self,
        request: ReadTextFileRequest,
    ) -> anyhow::Result<ReadTextFileResponse>;

    async fn write_text_file(
        &mut self,
        request: WriteTextFileRequest,
    ) -> anyhow::Result<WriteTextFileResponse>;

    async fn request_permission(
        &mut self,
        request: RequestPermissionRequest,
    ) -> anyhow::Result<RequestPermissionResponse>;

    async fn session_update(&mut self, notification: SessionNotification) -> anyhow::Result<()>;
}

/// Safe default host used when Red has no filesystem or permission UI attached.
#[derive(Debug, Default)]
pub struct NoopAcpHost;

#[async_trait]
impl AcpHost for NoopAcpHost {
    async fn read_text_file(
        &mut self,
        request: ReadTextFileRequest,
    ) -> anyhow::Result<ReadTextFileResponse> {
        anyhow::bail!(
            "agent requested unavailable file read: {}",
            request.path.display()
        )
    }

    async fn write_text_file(
        &mut self,
        request: WriteTextFileRequest,
    ) -> anyhow::Result<WriteTextFileResponse> {
        anyhow::bail!(
            "agent requested unavailable file write: {}",
            request.path.display()
        )
    }

    async fn request_permission(
        &mut self,
        _request: RequestPermissionRequest,
    ) -> anyhow::Result<RequestPermissionResponse> {
        Ok(RequestPermissionResponse::new(
            RequestPermissionOutcome::Cancelled,
        ))
    }

    async fn session_update(&mut self, _notification: SessionNotification) -> anyhow::Result<()> {
        Ok(())
    }
}

/// JSON-RPC error returned by the adapter.
#[derive(Debug, Clone, thiserror::Error)]
#[error("ACP request failed ({code}): {message}")]
pub struct AcpRpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

/// Cloneable handle to one actor-owned ACP child process.
#[derive(Debug, Clone)]
pub struct AcpClient {
    commands: mpsc::Sender<ProcessCommand>,
    request_timeout: Duration,
}

/// A running ACP adapter and its lifecycle task.
#[derive(Debug)]
pub struct AcpSpawn {
    pub client: AcpClient,
    pub task: JoinHandle<anyhow::Result<()>>,
}

/// Start an adapter and drive Red's bounded core-to-plugin bridge.
///
/// Session updates are sent to both the bridge and the supplied host. The host remains
/// authoritative for filesystem and permission requests.
///
/// # Errors
///
/// Returns an error when the adapter cannot be started.
pub fn start_bridge(
    spec: AcpProcessSpec,
    host: impl AcpHost,
    capacity: std::num::NonZeroUsize,
) -> anyhow::Result<(AcpBridge, JoinHandle<anyhow::Result<()>>)> {
    let authentication_method = spec.authentication_method.clone();
    let (bridge, mut worker) = AcpBridge::channel(capacity);
    let pending_permissions = Arc::new(Mutex::new(HashMap::new()));
    let event_host = BridgeAcpHost {
        inner: host,
        events: worker.events.clone(),
        pending_permissions: Arc::clone(&pending_permissions),
    };
    let spawned = AcpSpawn::start(spec, event_host)?;
    let task = tokio::spawn(async move {
        let initialized: agent_client_protocol_schema::v1::InitializeResponse =
            spawned.client.request(initialize_request()).await?;
        anyhow::ensure!(
            initialized.protocol_version == WIRE_PROTOCOL_VERSION,
            "adapter selected unsupported ACP protocol version {}",
            initialized.protocol_version
        );
        let supports_session_close = initialized
            .agent_capabilities
            .session_capabilities
            .close
            .is_some();
        if let Some(method_id) = authentication_method {
            anyhow::ensure!(
                initialized
                    .auth_methods
                    .iter()
                    .any(|method| method.id().0.as_ref() == method_id),
                "adapter did not advertise ACP authentication method `{method_id}`"
            );
            let _: AuthenticateResponse = spawned
                .client
                .request(ClientRequest::AuthenticateRequest(
                    AuthenticateRequest::new(method_id),
                ))
                .await?;
        }

        while let Some(command) = worker.recv().await {
            if let BridgeCommand::PermissionResponse {
                request_id,
                option_id,
            } = command
            {
                if let Some(pending) = pending_permissions.lock().await.remove(&request_id) {
                    let _ = pending
                        .response
                        .send(option_id.map(PermissionOptionId::new));
                }
                continue;
            }
            match command.clone().into_wire() {
                BridgeWireMessage::Request(request) => match command {
                    BridgeCommand::NewSession { .. } => {
                        let event = match spawned
                            .client
                            .request::<agent_client_protocol_schema::v1::NewSessionResponse>(
                                *request,
                            )
                            .await
                        {
                            Ok(response) => BridgeEvent::SessionCreated {
                                session_id: response.session_id,
                            },
                            Err(error) => BridgeEvent::Failed {
                                session_id: None,
                                message: format!("ACP session could not be created: {error}"),
                            },
                        };
                        worker
                            .send(event)
                            .await
                            .map_err(|_| anyhow::anyhow!("ACP bridge event receiver stopped"))?;
                    }
                    BridgeCommand::Prompt { session_id, .. }
                    | BridgeCommand::PromptWithContext { session_id, .. } => {
                        let client = spawned.client.clone();
                        let events = worker.events.clone();
                        tokio::spawn(async move {
                            let event = match client
                                .request::<agent_client_protocol_schema::v1::PromptResponse>(
                                    *request,
                                )
                                .await
                            {
                                Ok(response) => BridgeEvent::Completed {
                                    session_id,
                                    stop_reason: stop_reason_name(response.stop_reason),
                                },
                                Err(error) => BridgeEvent::Failed {
                                    session_id: Some(session_id),
                                    message: error.to_string(),
                                },
                            };
                            let _ = events.send(event).await;
                        });
                    }
                    BridgeCommand::CloseSession { session_id } => {
                        cancel_pending_permissions(&pending_permissions, &session_id).await;
                        spawned
                            .client
                            .notify(ClientNotification::CancelNotification(
                                CancelNotification::new(session_id.clone()),
                            ))
                            .await?;
                        if supports_session_close {
                            let error = match timeout(
                                CLOSE_REQUEST_TIMEOUT,
                                spawned.client.request::<CloseSessionResponse>(*request),
                            )
                            .await
                            {
                                Ok(Ok(_)) => None,
                                Ok(Err(error)) => Some(error.to_string()),
                                Err(_) => Some(format!(
                                    "ACP session close timed out after {CLOSE_REQUEST_TIMEOUT:?}"
                                )),
                            };
                            if let Some(error) = error {
                                worker
                                    .send(BridgeEvent::Failed {
                                        session_id: Some(session_id),
                                        message: format!(
                                            "ACP session could not be closed: {error}"
                                        ),
                                    })
                                    .await
                                    .map_err(|_| {
                                        anyhow::anyhow!("ACP bridge event receiver stopped")
                                    })?;
                            }
                        }
                    }
                    BridgeCommand::Cancel { .. } => unreachable!("cancel is a notification"),
                    BridgeCommand::PermissionResponse { .. } => {
                        unreachable!("permission response was handled before wire encoding")
                    }
                },
                BridgeWireMessage::Notification(notification) => {
                    let BridgeCommand::Cancel { session_id } = command else {
                        unreachable!("only cancellation is an ACP bridge notification")
                    };
                    cancel_pending_permissions(&pending_permissions, &session_id).await;
                    spawned.client.notify(notification).await?;
                    worker
                        .send(BridgeEvent::Cancelled { session_id })
                        .await
                        .map_err(|_| anyhow::anyhow!("ACP bridge event receiver stopped"))?;
                }
            }
        }

        spawned.client.shutdown().await?;
        spawned.task.await??;
        Ok(())
    });
    Ok((bridge, task))
}

struct BridgeAcpHost<H> {
    inner: H,
    events: mpsc::Sender<BridgeEvent>,
    pending_permissions: Arc<Mutex<HashMap<String, PendingPermission>>>,
}

struct PendingPermission {
    session_id: SessionId,
    response: oneshot::Sender<Option<PermissionOptionId>>,
}

async fn cancel_pending_permissions(
    pending_permissions: &Mutex<HashMap<String, PendingPermission>>,
    session_id: &SessionId,
) {
    let mut pending = pending_permissions.lock().await;
    let cancelled = pending
        .iter()
        .filter(|(_, permission)| &permission.session_id == session_id)
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    for id in cancelled {
        if let Some(permission) = pending.remove(&id) {
            let _ = permission.response.send(/*option_id*/ None);
        }
    }
}

#[async_trait]
impl<H: AcpHost> AcpHost for BridgeAcpHost<H> {
    async fn read_text_file(
        &mut self,
        request: ReadTextFileRequest,
    ) -> anyhow::Result<ReadTextFileResponse> {
        self.inner.read_text_file(request).await
    }

    async fn write_text_file(
        &mut self,
        request: WriteTextFileRequest,
    ) -> anyhow::Result<WriteTextFileResponse> {
        let session_id = request.session_id.clone();
        let response = self.inner.write_text_file(request).await?;
        self.events
            .send(BridgeEvent::ProposalsChanged { session_id })
            .await
            .map_err(|_| anyhow::anyhow!("ACP bridge event receiver stopped"))?;
        Ok(response)
    }

    async fn request_permission(
        &mut self,
        request: RequestPermissionRequest,
    ) -> anyhow::Result<RequestPermissionResponse> {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (response_tx, response_rx) = oneshot::channel();
        self.pending_permissions.lock().await.insert(
            request_id.clone(),
            PendingPermission {
                session_id: request.session_id.clone(),
                response: response_tx,
            },
        );
        self.events
            .send(BridgeEvent::PermissionRequested {
                request_id: request_id.clone(),
                session_id: request.session_id,
                tool_call: serde_json::to_value(request.tool_call)?,
                options: request.options.clone(),
            })
            .await
            .map_err(|_| anyhow::anyhow!("ACP bridge event receiver stopped"))?;
        let selected = response_rx.await.unwrap_or(/*option_id*/ None);
        self.pending_permissions.lock().await.remove(&request_id);
        let outcome = if let Some(option_id) = selected {
            anyhow::ensure!(
                request
                    .options
                    .iter()
                    .any(|option| option.option_id == option_id),
                "permission response selected an option the agent did not provide"
            );
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id))
        } else {
            RequestPermissionOutcome::Cancelled
        };
        Ok(RequestPermissionResponse::new(outcome))
    }

    async fn session_update(&mut self, notification: SessionNotification) -> anyhow::Result<()> {
        match &notification.update {
            SessionUpdate::AgentMessageChunk(chunk)
                if matches!(
                    chunk.content,
                    agent_client_protocol_schema::v1::ContentBlock::Text(_)
                ) =>
            {
                let agent_client_protocol_schema::v1::ContentBlock::Text(text) = &chunk.content
                else {
                    unreachable!("text-chunk guard must match text content")
                };
                self.events
                    .send(BridgeEvent::Update {
                        session_id: notification.session_id.clone(),
                        text: text.text.clone(),
                    })
                    .await
                    .map_err(|_| anyhow::anyhow!("ACP bridge event receiver stopped"))?;
            }
            _ => {
                self.events
                    .send(BridgeEvent::Activity {
                        session_id: notification.session_id.clone(),
                        update: serde_json::to_value(&notification.update)?,
                    })
                    .await
                    .map_err(|_| anyhow::anyhow!("ACP bridge event receiver stopped"))?;
            }
        }
        self.inner.session_update(notification).await
    }
}

fn stop_reason_name(reason: agent_client_protocol_schema::v1::StopReason) -> String {
    serde_json::to_value(reason)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

enum ProcessCommand {
    Request {
        request: Box<ClientRequest>,
        response: oneshot::Sender<Result<Value, AcpRpcError>>,
    },
    Notification(ClientNotification),
    PruneClosedRequests,
    Shutdown(oneshot::Sender<anyhow::Result<()>>),
}

impl AcpSpawn {
    /// Start an adapter with piped NDJSON stdin/stdout and a bounded command queue.
    ///
    /// # Errors
    ///
    /// Returns an error when the executable cannot be started or its stdio cannot be captured.
    pub fn start(spec: AcpProcessSpec, host: impl AcpHost) -> anyhow::Result<Self> {
        anyhow::ensure!(
            spec.queue_capacity > 0,
            "ACP queue capacity must be non-zero"
        );

        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .envs(&spec.environment)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // Adapter diagnostics must never write directly into Red's alternate screen.
            // Actionable failures are delivered through ACP's structured error channel.
            .stderr(std::process::Stdio::null())
            .kill_on_drop(/*kill_on_drop*/ true);
        if let Some(current_dir) = &spec.current_dir {
            command.current_dir(current_dir);
        }

        let mut child = command.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("ACP adapter stdin was not captured"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("ACP adapter stdout was not captured"))?;
        let (command_tx, command_rx) = mpsc::channel(spec.queue_capacity);
        let actor = ProcessActor {
            child,
            stdin: Some(BufWriter::new(stdin)),
            stdout: BufReader::new(stdout),
            codec: AcpCodec::default(),
            commands: command_rx,
            pending: HashMap::new(),
            cancelled_sessions: CancelledSessions::new(
                spec.queue_capacity
                    .clamp(DEFAULT_QUEUE_CAPACITY, MAX_CANCELLED_SESSIONS),
            ),
            pending_capacity: spec.queue_capacity,
            host: Box::new(host),
            write_timeout: spec.write_timeout,
            shutdown_timeout: spec.shutdown_timeout,
        };
        let task = tokio::spawn(actor.run());

        Ok(Self {
            client: AcpClient {
                commands: command_tx,
                request_timeout: spec.request_timeout,
            },
            task,
        })
    }
}

impl AcpClient {
    /// Send a typed ACP request and deserialize the correlated result.
    ///
    /// Setup and control requests use the configured timeout. Prompt turns remain active
    /// until the adapter responds or their session is explicitly cancelled.
    ///
    /// # Errors
    ///
    /// Returns transport, timeout, JSON-RPC, or response-schema errors.
    pub async fn request<T: DeserializeOwned>(&self, request: ClientRequest) -> anyhow::Result<T> {
        let long_running = matches!(&request, ClientRequest::PromptRequest(_));
        let (response_tx, response_rx) = oneshot::channel();
        self.commands
            .send(ProcessCommand::Request {
                request: Box::new(request),
                response: response_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("ACP adapter process has stopped"))?;
        let response = if long_running {
            response_rx
                .await
                .map_err(|_| anyhow::anyhow!("ACP adapter dropped the pending response"))??
        } else {
            match timeout(self.request_timeout, response_rx).await {
                Ok(response) => response
                    .map_err(|_| anyhow::anyhow!("ACP adapter dropped the pending response"))??,
                Err(_) => {
                    let _ = self.commands.try_send(ProcessCommand::PruneClosedRequests);
                    anyhow::bail!("ACP request timed out after {:?}", self.request_timeout);
                }
            }
        };
        Ok(serde_json::from_value(response)?)
    }

    /// Send a typed ACP notification without waiting for a response.
    ///
    /// # Errors
    ///
    /// Returns an error if the adapter process has stopped.
    pub async fn notify(&self, notification: ClientNotification) -> anyhow::Result<()> {
        self.commands
            .send(ProcessCommand::Notification(notification))
            .await
            .map_err(|_| anyhow::anyhow!("ACP adapter process has stopped"))
    }

    /// Close stdin, wait briefly for a clean exit, then kill an unresponsive adapter.
    ///
    /// # Errors
    ///
    /// Returns an error if the lifecycle actor has already stopped or shutdown fails.
    pub async fn shutdown(&self) -> anyhow::Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.commands
            .send(ProcessCommand::Shutdown(response_tx))
            .await
            .map_err(|_| anyhow::anyhow!("ACP adapter process has stopped"))?;
        response_rx
            .await
            .map_err(|_| anyhow::anyhow!("ACP adapter dropped the shutdown response"))?
    }
}

struct ProcessActor {
    child: Child,
    stdin: Option<BufWriter<ChildStdin>>,
    stdout: BufReader<tokio::process::ChildStdout>,
    codec: AcpCodec,
    commands: mpsc::Receiver<ProcessCommand>,
    pending: HashMap<RequestId, PendingRequest>,
    cancelled_sessions: CancelledSessions,
    pending_capacity: usize,
    host: Box<dyn AcpHost>,
    write_timeout: Duration,
    shutdown_timeout: Duration,
}

struct PendingRequest {
    response: oneshot::Sender<Result<Value, AcpRpcError>>,
    prompt_session: Option<SessionId>,
    close_session: Option<SessionId>,
}

struct CancelledSessions {
    sessions: VecDeque<SessionId>,
    capacity: usize,
    overflowed: bool,
}

impl CancelledSessions {
    fn new(capacity: usize) -> Self {
        Self {
            sessions: VecDeque::with_capacity(capacity),
            capacity,
            overflowed: false,
        }
    }

    fn contains(&self, session_id: &SessionId) -> bool {
        self.overflowed || self.sessions.contains(session_id)
    }

    fn insert(&mut self, session_id: SessionId) {
        self.remove(&session_id);
        if self.sessions.len() == self.capacity {
            self.overflowed = true;
            return;
        }
        self.sessions.push_back(session_id);
    }

    fn remove(&mut self, session_id: &SessionId) {
        self.sessions.retain(|session| session != session_id);
    }
}

impl ProcessActor {
    async fn run(mut self) -> anyhow::Result<()> {
        loop {
            tokio::select! {
                command = self.commands.recv() => {
                    let Some(command) = command else {
                        return self.stop_child().await;
                    };
                    if self.handle_command(command).await? {
                        return Ok(());
                    }
                }
                line = read_bounded_line(&mut self.stdout) => {
                    match line? {
                        Some(line) => self.handle_incoming(&line).await?,
                        None => {
                            let status = self.child.wait().await?;
                            anyhow::bail!("ACP adapter exited unexpectedly with {status}");
                        }
                    }
                }
            }
        }
    }

    async fn handle_command(&mut self, command: ProcessCommand) -> anyhow::Result<bool> {
        match command {
            ProcessCommand::Request { request, response } => {
                self.prune_closed_requests();
                if self.pending.len() >= self.pending_capacity {
                    let _ = response.send(Err(AcpRpcError {
                        code: -32_000,
                        message: format!(
                            "ACP pending request capacity {} reached",
                            self.pending_capacity
                        ),
                        data: None,
                    }));
                    return Ok(false);
                }
                let prompt_session = match request.as_ref() {
                    ClientRequest::PromptRequest(request) => Some(request.session_id.clone()),
                    _ => None,
                };
                let close_session = match request.as_ref() {
                    ClientRequest::CloseSessionRequest(request) => Some(request.session_id.clone()),
                    _ => None,
                };
                if prompt_session
                    .as_ref()
                    .is_some_and(|session_id| self.cancelled_sessions.contains(session_id))
                {
                    let message = if self.cancelled_sessions.overflowed {
                        "ACP cancelled-session capacity reached; restart the adapter before prompting again"
                    } else {
                        "ACP session was cancelled; start a new session before prompting again"
                    };
                    let _ = response.send(Err(AcpRpcError {
                        code: -32_000,
                        message: message.to_string(),
                        data: None,
                    }));
                    return Ok(false);
                }
                let (request_id, line) = self.encode_request(*request)?;
                self.pending.insert(
                    request_id,
                    PendingRequest {
                        response,
                        prompt_session,
                        close_session,
                    },
                );
                self.write_line(&line).await?;
                Ok(false)
            }
            ProcessCommand::Notification(notification) => {
                let cancelled_session = match &notification {
                    ClientNotification::CancelNotification(cancel) => {
                        Some(cancel.session_id.clone())
                    }
                    _ => None,
                };
                let line = self.codec.encode_notification(notification)?;
                self.write_line(&line).await?;
                if let Some(session_id) = cancelled_session {
                    self.cancelled_sessions.insert(session_id.clone());
                    self.cancel_prompt(&session_id)?;
                }
                Ok(false)
            }
            ProcessCommand::PruneClosedRequests => {
                self.prune_closed_requests();
                Ok(false)
            }
            ProcessCommand::Shutdown(response) => {
                let result = self.stop_child().await;
                let _ = response.send(result);
                Ok(true)
            }
        }
    }

    fn encode_request(&mut self, request: ClientRequest) -> anyhow::Result<(RequestId, String)> {
        let line = self.codec.encode_request(request)?;
        let value: Value = serde_json::from_str(&line)?;
        let id = serde_json::from_value(
            value
                .get("id")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("encoded ACP request has no id"))?,
        )?;
        Ok((id, line))
    }

    async fn write_line(&mut self, line: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            line.len() <= MAX_MESSAGE_BYTES,
            "ACP message exceeds {MAX_MESSAGE_BYTES} bytes"
        );
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("ACP adapter stdin is closed"))?;
        match timeout(self.write_timeout, async {
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await
        })
        .await
        {
            Ok(result) => result?,
            Err(_) => anyhow::bail!(
                "ACP adapter stdin write timed out after {:?}",
                self.write_timeout
            ),
        }
        Ok(())
    }

    async fn handle_incoming(&mut self, line: &str) -> anyhow::Result<()> {
        let value: Value = self.codec.decode_line(line)?;
        if value.get("method").is_some() {
            return self.handle_agent_call(value).await;
        }

        let id: RequestId = serde_json::from_value(
            value
                .get("id")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("ACP response has no request id"))?,
        )?;
        let Some(pending) = self.pending.remove(&id) else {
            // Timed-out setup/control requests are pruned locally. Their late responses,
            // and duplicate responses from an adapter, must not terminate a healthy actor.
            return Ok(());
        };
        let response = if let Some(result) = value.get("result") {
            if let Some(session_id) = &pending.close_session {
                self.cancelled_sessions.remove(session_id);
            }
            Ok(result.clone())
        } else {
            let error = value
                .get("error")
                .ok_or_else(|| anyhow::anyhow!("ACP response has neither result nor error"))?;
            Err(AcpRpcError {
                code: error.get("code").and_then(Value::as_i64).unwrap_or(-32_603),
                message: error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown adapter error")
                    .to_string(),
                data: error.get("data").cloned(),
            })
        };
        let _ = pending.response.send(response);
        Ok(())
    }

    async fn handle_agent_call(&mut self, value: Value) -> anyhow::Result<()> {
        let method = value
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("ACP call has no method"))?;
        let params = value.get("params").cloned().unwrap_or(Value::Null);
        let Some(id_value) = value.get("id").cloned() else {
            if method == "session/update" {
                let notification = match serde_json::from_value::<SessionNotification>(params) {
                    Ok(notification) => notification,
                    Err(_) => {
                        crate::log!(
                            "{}",
                            json!({
                                "event": "acp_session_update_rejected",
                                "service": "red",
                                "reason": "invalid_parameters",
                            })
                        );
                        return Ok(());
                    }
                };
                let session_id = notification.session_id.to_string();
                if self.host.session_update(notification).await.is_err() {
                    crate::log!(
                        "{}",
                        json!({
                            "event": "acp_session_update_rejected",
                            "service": "red",
                            "session_id": session_id,
                            "reason": "callback_failed",
                        })
                    );
                }
                return Ok(());
            }
            // Stable ACP currently defines `session/update`; extension notifications are
            // intentionally ignored until a registered handler owns their semantics.
            return Ok(());
        };

        let id: RequestId = serde_json::from_value(id_value)?;
        let result = match method {
            "fs/read_text_file" => match serde_json::from_value(params) {
                Ok(request) => host_response(self.host.read_text_file(request).await),
                Err(error) => Err(invalid_params(error)),
            },
            "fs/write_text_file" => match serde_json::from_value::<WriteTextFileRequest>(params) {
                Ok(request) if self.has_active_prompt(&request.session_id) => {
                    host_response(self.host.write_text_file(request).await)
                }
                Ok(_) => Err(AcpRpcError {
                    code: -32_000,
                    message: "ACP filesystem writes require an active prompt".to_string(),
                    data: None,
                }),
                Err(error) => Err(invalid_params(error)),
            },
            "session/request_permission" => match serde_json::from_value(params) {
                Ok(request) => host_response(self.host.request_permission(request).await),
                Err(error) => Err(invalid_params(error)),
            },
            _ => {
                self.write_error(
                    id,
                    /*code*/ -32_601,
                    format!("unsupported ACP method `{method}`"),
                )
                .await?;
                return Ok(());
            }
        };
        match result {
            Ok(result) => self.write_response(id, result).await,
            Err(error) => self.write_error(id, error.code, error.message).await,
        }
    }

    async fn write_response(&mut self, id: RequestId, result: Value) -> anyhow::Result<()> {
        self.write_line(&encode_host_response(&id, result)?).await
    }

    async fn write_error(
        &mut self,
        id: RequestId,
        code: i64,
        message: String,
    ) -> anyhow::Result<()> {
        self.write_line(&encode_host_error(&id, code, message)?)
            .await
    }

    async fn stop_child(&mut self) -> anyhow::Result<()> {
        if let Some(mut stdin) = self.stdin.take() {
            match timeout(self.shutdown_timeout, stdin.shutdown()).await {
                Ok(result) => result?,
                Err(_) => {
                    self.child.kill().await?;
                    let _ = self.child.wait().await?;
                    return Ok(());
                }
            }
        }
        match timeout(self.shutdown_timeout, self.child.wait()).await {
            Ok(status) => {
                let status = status?;
                anyhow::ensure!(status.success(), "ACP adapter exited with {status}");
            }
            Err(_) => {
                self.child.kill().await?;
                let _ = self.child.wait().await?;
            }
        }
        Ok(())
    }

    fn prune_closed_requests(&mut self) {
        self.pending
            .retain(|_, pending| !pending.response.is_closed());
    }

    fn has_active_prompt(&self, session_id: &SessionId) -> bool {
        self.pending
            .values()
            .any(|pending| pending.prompt_session.as_ref() == Some(session_id))
    }

    fn cancel_prompt(&mut self, session_id: &SessionId) -> anyhow::Result<()> {
        let cancelled = self
            .pending
            .iter()
            .filter(|(_, pending)| pending.prompt_session.as_ref() == Some(session_id))
            .map(|(request_id, _)| request_id.clone())
            .collect::<Vec<_>>();
        let response = serde_json::to_value(
            agent_client_protocol_schema::v1::PromptResponse::new(StopReason::Cancelled),
        )?;
        for request_id in cancelled {
            if let Some(pending) = self.pending.remove(&request_id) {
                let _ = pending.response.send(Ok(response.clone()));
            }
        }
        Ok(())
    }
}

fn encode_host_response(id: &RequestId, result: Value) -> anyhow::Result<String> {
    let response = json!({ "jsonrpc": "2.0", "id": id, "result": result });
    let mut line = serde_json::to_string(&response)?;
    line.push('\n');
    if line.len() <= MAX_MESSAGE_BYTES {
        return Ok(line);
    }

    encode_host_error(
        id,
        /*code*/ -32_000,
        format!("ACP host response exceeds {MAX_MESSAGE_BYTES} bytes"),
    )
}

fn encode_host_error(id: &RequestId, code: i64, message: String) -> anyhow::Result<String> {
    let response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        },
    });
    let mut line = serde_json::to_string(&response)?;
    line.push('\n');
    if line.len() <= MAX_MESSAGE_BYTES {
        return Ok(line);
    }

    let response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32_000,
            "message": format!("ACP host error exceeds {MAX_MESSAGE_BYTES} bytes"),
        },
    });
    let mut line = serde_json::to_string(&response)?;
    line.push('\n');
    Ok(line)
}

fn host_response<T: Serialize>(response: anyhow::Result<T>) -> Result<Value, AcpRpcError> {
    let response = response.map_err(|error| AcpRpcError {
        code: -32_000,
        message: error.to_string(),
        data: None,
    })?;
    serde_json::to_value(response).map_err(|error| AcpRpcError {
        code: -32_603,
        message: format!("failed to serialize ACP host response: {error}"),
        data: None,
    })
}

fn invalid_params(error: serde_json::Error) -> AcpRpcError {
    AcpRpcError {
        code: -32_602,
        message: format!("invalid ACP request parameters: {error}"),
        data: None,
    }
}

async fn read_bounded_line(
    reader: &mut (impl AsyncBufRead + Unpin),
) -> anyhow::Result<Option<String>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            anyhow::ensure!(
                line.is_empty(),
                "ACP message is missing a terminating newline"
            );
            return Ok(None);
        }

        let complete = available.iter().position(|byte| *byte == b'\n');
        let consumed = complete.map_or(available.len(), |index| index + 1);
        anyhow::ensure!(
            line.len().saturating_add(consumed) <= MAX_MESSAGE_BYTES,
            "ACP message exceeds {MAX_MESSAGE_BYTES} bytes"
        );
        line.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if complete.is_some() {
            return Ok(Some(String::from_utf8(line)?));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bounded_reader_accepts_a_complete_message() {
        let mut reader = BufReader::with_capacity(3, b"{\"ok\":true}\n".as_slice());
        assert_eq!(
            read_bounded_line(&mut reader).await.unwrap().as_deref(),
            Some("{\"ok\":true}\n")
        );
        assert!(read_bounded_line(&mut reader).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn bounded_reader_rejects_oversized_and_unterminated_messages() {
        let oversized = format!("{}\n", "x".repeat(MAX_MESSAGE_BYTES));
        let mut reader = BufReader::new(oversized.as_bytes());
        assert!(read_bounded_line(&mut reader)
            .await
            .unwrap_err()
            .to_string()
            .contains("exceeds"));

        let mut reader = BufReader::new(b"unterminated".as_slice());
        assert!(read_bounded_line(&mut reader)
            .await
            .unwrap_err()
            .to_string()
            .contains("terminating newline"));
    }

    #[test]
    fn escaping_heavy_host_response_becomes_a_scoped_rpc_error() {
        let result = json!({ "content": "\"".repeat(MAX_MESSAGE_BYTES - 64 * 1024) });

        let line = encode_host_response(&RequestId::Number(7), result).unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();

        assert!(line.len() <= MAX_MESSAGE_BYTES);
        assert_eq!(response["id"], 7);
        assert_eq!(response["error"]["code"], -32_000);
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("host response exceeds"));
    }

    #[test]
    fn escaping_heavy_host_error_becomes_a_scoped_rpc_error() {
        let message = "\"".repeat(MAX_MESSAGE_BYTES - 64 * 1024);

        let line = encode_host_error(&RequestId::Number(9), -32_601, message).unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();

        assert!(line.len() <= MAX_MESSAGE_BYTES);
        assert_eq!(response["id"], 9);
        assert_eq!(response["error"]["code"], -32_000);
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("host error exceeds"));
    }

    #[test]
    fn cancelled_session_overflow_fails_closed_until_restart() {
        let mut sessions = CancelledSessions::new(/*capacity*/ 3);
        let first = SessionId::new("first");
        let second = SessionId::new("second");
        let third = SessionId::new("third");
        let fourth = SessionId::new("fourth");

        sessions.insert(first.clone());
        sessions.insert(second.clone());
        sessions.insert(third.clone());
        sessions.insert(first.clone());
        assert!(!sessions.overflowed);
        sessions.insert(fourth.clone());

        assert!(sessions.overflowed);
        assert!(sessions.contains(&first));
        assert!(sessions.contains(&second));
        assert!(sessions.contains(&third));
        assert!(sessions.contains(&fourth));
        assert_eq!(sessions.sessions.len(), 3);

        sessions.remove(&first);
        sessions.remove(&second);
        sessions.remove(&third);
        assert!(sessions.sessions.is_empty());
        assert!(sessions.contains(&first));
        assert!(sessions.contains(&SessionId::new("new-session")));
    }
}
