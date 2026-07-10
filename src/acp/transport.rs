//! Bounded JSON-RPC transport and ACP child-process lifecycle.

use std::{collections::HashMap, ffi::OsString, path::PathBuf, sync::Arc, time::Duration};

use agent_client_protocol_schema::v1::{
    ClientNotification, ClientRequest, PermissionOptionId, ReadTextFileRequest,
    ReadTextFileResponse, RequestId, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionId, SessionNotification,
    SessionUpdate, WriteTextFileRequest, WriteTextFileResponse,
};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter},
    process::{Child, ChildStdin, Command},
    sync::{mpsc, oneshot, Mutex},
    task::JoinHandle,
    time::timeout,
};

use super::{
    initialize_request, AcpBridge, AcpCodec, BridgeCommand, BridgeEvent, BridgeWireMessage,
    WIRE_PROTOCOL_VERSION,
};

const DEFAULT_QUEUE_CAPACITY: usize = 32;
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Launch configuration for an ACP adapter executable.
#[derive(Debug, Clone)]
pub struct AcpProcessSpec {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub current_dir: Option<PathBuf>,
    pub environment: HashMap<OsString, OsString>,
    pub queue_capacity: usize,
    pub request_timeout: Duration,
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
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
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
                        let response: agent_client_protocol_schema::v1::NewSessionResponse =
                            spawned.client.request(*request).await?;
                        worker
                            .send(BridgeEvent::SessionCreated {
                                session_id: response.session_id,
                            })
                            .await
                            .map_err(|_| anyhow::anyhow!("ACP bridge event receiver stopped"))?;
                    }
                    BridgeCommand::Prompt { session_id, .. } => {
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
                                    message: error.to_string(),
                                },
                            };
                            let _ = events.send(event).await;
                        });
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
                    let mut pending = pending_permissions.lock().await;
                    let cancelled = pending
                        .iter()
                        .filter(|(_, permission)| permission.session_id == session_id)
                        .map(|(id, _)| id.clone())
                        .collect::<Vec<_>>();
                    for id in cancelled {
                        if let Some(permission) = pending.remove(&id) {
                            let _ = permission.response.send(/*option_id*/ None);
                        }
                    }
                    drop(pending);
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
        self.inner.write_text_file(request).await
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
        if let SessionUpdate::AgentMessageChunk(chunk) = &notification.update {
            if let agent_client_protocol_schema::v1::ContentBlock::Text(text) = &chunk.content {
                self.events
                    .send(BridgeEvent::Update {
                        session_id: notification.session_id.clone(),
                        text: text.text.clone(),
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
            .stderr(std::process::Stdio::inherit())
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
            stdout: BufReader::new(stdout).lines(),
            codec: AcpCodec::default(),
            commands: command_rx,
            pending: HashMap::new(),
            host: Box::new(host),
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
    /// # Errors
    ///
    /// Returns transport, timeout, JSON-RPC, or response-schema errors.
    pub async fn request<T: DeserializeOwned>(&self, request: ClientRequest) -> anyhow::Result<T> {
        let (response_tx, response_rx) = oneshot::channel();
        self.commands
            .send(ProcessCommand::Request {
                request: Box::new(request),
                response: response_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("ACP adapter process has stopped"))?;
        let response = timeout(self.request_timeout, response_rx)
            .await
            .map_err(|_| anyhow::anyhow!("ACP request timed out after {:?}", self.request_timeout))?
            .map_err(|_| anyhow::anyhow!("ACP adapter dropped the pending response"))??;
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
    stdout: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    codec: AcpCodec,
    commands: mpsc::Receiver<ProcessCommand>,
    pending: HashMap<RequestId, oneshot::Sender<Result<Value, AcpRpcError>>>,
    host: Box<dyn AcpHost>,
    shutdown_timeout: Duration,
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
                line = self.stdout.next_line() => {
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
                let (request_id, line) = self.encode_request(*request)?;
                self.pending.insert(request_id, response);
                self.write_line(&line).await?;
                Ok(false)
            }
            ProcessCommand::Notification(notification) => {
                let line = self.codec.encode_notification(notification)?;
                self.write_line(&line).await?;
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
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("ACP adapter stdin is closed"))?;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
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
            anyhow::bail!("ACP response has unknown request id {id}");
        };
        let response = if let Some(result) = value.get("result") {
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
        let _ = pending.send(response);
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
                let notification = serde_json::from_value::<SessionNotification>(params)?;
                self.host.session_update(notification).await?;
                return Ok(());
            }
            // Stable ACP currently defines `session/update`; extension notifications are
            // intentionally ignored until a registered handler owns their semantics.
            return Ok(());
        };

        let id: RequestId = serde_json::from_value(id_value)?;
        let result = match method {
            "fs/read_text_file" => serde_json::to_value(
                self.host
                    .read_text_file(serde_json::from_value(params)?)
                    .await?,
            )?,
            "fs/write_text_file" => serde_json::to_value(
                self.host
                    .write_text_file(serde_json::from_value(params)?)
                    .await?,
            )?,
            "session/request_permission" => serde_json::to_value(
                self.host
                    .request_permission(serde_json::from_value(params)?)
                    .await?,
            )?,
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
        self.write_response(id, result).await
    }

    async fn write_response(&mut self, id: RequestId, result: Value) -> anyhow::Result<()> {
        let response = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        let mut line = serde_json::to_string(&response)?;
        line.push('\n');
        self.write_line(&line).await
    }

    async fn write_error(
        &mut self,
        id: RequestId,
        code: i64,
        message: String,
    ) -> anyhow::Result<()> {
        let response = json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        });
        let mut line = serde_json::to_string(&response)?;
        line.push('\n');
        self.write_line(&line).await
    }

    async fn stop_child(&mut self) -> anyhow::Result<()> {
        if let Some(mut stdin) = self.stdin.take() {
            stdin.shutdown().await?;
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
}
