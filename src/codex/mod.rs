//! Direct client for the installed Codex app-server.
//!
//! Red deliberately runs Codex read-only and exposes bounded dynamic tools for
//! editor-aware reads and reviewable proposal writes. No ACP adapter sits
//! between the editor and Codex.

use std::{
    collections::HashMap,
    ffi::OsString,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::{fs::File, io::Read as _, path::Component};

use anyhow::{Context, Result};
use async_trait::async_trait;
use ignore::WalkBuilder;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter},
    process::Command,
    sync::{mpsc, Mutex},
    task::JoinHandle,
    time::timeout,
};

use crate::agent_tools::{editor_tool_schemas, EditorToolCall, EditorToolRequest};

const APP_FRAME_BYTES: usize = 1024 * 1024;
const TOOL_CONTENT_BYTES: usize = 960 * 1024;
const MAX_TOOL_CALLS: usize = 32;
const MAX_FILES: usize = 4096;
#[cfg(unix)]
const MAX_MATCHES: usize = 200;
#[cfg(unix)]
const MAX_SEARCH_BYTES: u64 = 32 * 1024 * 1024;
const MAX_WALK_ENTRIES: usize = 65_536;
const MAX_WALK_TIME: Duration = Duration::from_secs(5);
const SETUP_TIMEOUT: Duration = Duration::from_secs(30);
const TOOL_TIMEOUT: Duration = Duration::from_secs(30);
const INSTRUCTIONS: &str = "You are Red's coding assistant. You have no shell or native patch tool. Use list_files and search_files to locate relevant code. Use get_editor_state, open_file, select_text, and run_editor_action to inspect and navigate the editor. Always use read_file before reasoning about a file, and use apply_edits or write_file for every edit. Edits are reviewable editor proposals and never touch disk. Do not claim a change was saved. Keep responses concise.";

/// Exact process launch specification for one Codex app-server worker.
#[derive(Debug, Clone)]
pub struct CodexProcessSpec {
    /// Resolved executable path.
    pub command: PathBuf,
    /// Additional literal arguments appended after Red's app-server arguments.
    pub args: Vec<OsString>,
    /// Explicit environment overrides.
    pub environment: HashMap<OsString, OsString>,
    /// Working directory used for process launch and thread configuration.
    pub current_dir: PathBuf,
}

impl CodexProcessSpec {
    #[must_use]
    /// Creates a launch specification with no additional arguments or environment.
    pub fn new(command: impl Into<PathBuf>, current_dir: impl Into<PathBuf>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            environment: HashMap::new(),
            current_dir: current_dir.into(),
        }
    }

    #[must_use]
    /// Appends literal process arguments without shell expansion.
    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }
}

/// Commands sent from the editor owner to the Codex worker.
#[derive(Debug, Clone)]
pub enum CodexCommand {
    /// Creates an ephemeral app-server thread for a workspace.
    NewSession {
        /// Physical workspace root.
        cwd: PathBuf,
    },
    /// Submits plain user text to a session.
    Prompt {
        /// Red session identifier.
        session_id: String,
        /// User prompt.
        text: String,
    },
    /// Submits user text with bounded editor context.
    PromptWithContext {
        /// Red session identifier.
        session_id: String,
        /// User prompt.
        text: String,
        /// Active document URI.
        uri: String,
        /// Bounded editor-provided context.
        context: String,
    },
    /// Interrupts the active turn for a session.
    Cancel {
        /// Red session identifier.
        session_id: String,
    },
    /// Closes the remote thread associated with a session.
    CloseSession {
        /// Red session identifier.
        session_id: String,
    },
    /// Resolves a surfaced permission request with an exact offered choice.
    PermissionResponse {
        /// App-server request identifier.
        request_id: String,
        /// Selected option, or `None` for denial/cancellation.
        option_id: Option<String>,
    },
}

/// Events delivered from the Codex worker to the editor owner.
#[derive(Debug, Clone)]
pub enum CodexEvent {
    /// A local session is associated with a started app-server thread.
    SessionCreated {
        /// Red session identifier.
        session_id: String,
    },
    /// Streamed assistant text for the active turn.
    Update {
        /// Owning session.
        session_id: String,
        /// Text delta.
        text: String,
    },
    /// Structured activity update for tool and reasoning presentation.
    Activity {
        /// Owning session.
        session_id: String,
        /// Bounded app-server update payload.
        update: Value,
    },
    /// Proposal contents changed for a session.
    ProposalsChanged {
        /// Owning session.
        session_id: String,
    },
    /// Active turn reached a terminal success state.
    Completed {
        /// Owning session.
        session_id: String,
        /// App-server stop reason.
        stop_reason: String,
    },
    /// Active turn was interrupted.
    Cancelled {
        /// Owning session.
        session_id: String,
    },
    /// App-server requested a user choice that Red can safely surface.
    PermissionRequested {
        /// App-server request identifier.
        request_id: String,
        /// Owning session.
        session_id: String,
        /// Descriptive tool-call payload.
        tool_call: Value,
        /// Exact selectable options supplied by the app-server.
        options: Value,
    },
    /// Session or worker operation failed.
    Failed {
        /// Owning session when the failure can be attributed.
        session_id: Option<String>,
        /// Sanitized user-facing failure message.
        message: String,
    },
}

/// Editor-side bounded command sender and non-blocking event receiver.
pub struct CodexBridge {
    commands: mpsc::Sender<CodexCommand>,
    events: mpsc::Receiver<CodexEvent>,
}

/// Worker-side bounded command receiver and event sender.
pub struct CodexBridgeWorker {
    commands: mpsc::Receiver<CodexCommand>,
    events: mpsc::Sender<CodexEvent>,
}

impl CodexBridge {
    #[must_use]
    /// Creates paired editor and worker endpoints with the supplied non-zero capacity.
    pub fn channel(capacity: NonZeroUsize) -> (Self, CodexBridgeWorker) {
        let (commands, command_rx) = mpsc::channel(capacity.get());
        let (event_tx, events) = mpsc::channel(capacity.get());
        (
            Self { commands, events },
            CodexBridgeWorker {
                commands: command_rx,
                events: event_tx,
            },
        )
    }

    /// Sends a command with backpressure.
    pub async fn send(&self, command: CodexCommand) -> Result<()> {
        self.commands
            .send(command)
            .await
            .context("Codex command channel is closed")
    }

    /// Attempts to send a command without waiting for channel capacity.
    pub fn try_send(&self, command: CodexCommand) -> Result<()> {
        self.commands
            .try_send(command)
            .context("Codex command channel is unavailable")
    }

    /// Returns the next ready worker event without blocking.
    pub fn try_recv(&mut self) -> Option<CodexEvent> {
        self.events.try_recv().ok()
    }

    #[must_use]
    /// Returns whether at least one worker event is buffered.
    pub fn has_pending_events(&self) -> bool {
        !self.events.is_empty()
    }
}

impl CodexBridgeWorker {
    /// Waits for the next editor command or channel closure.
    pub async fn recv(&mut self) -> Option<CodexCommand> {
        self.commands.recv().await
    }

    /// Sends an event to the editor with backpressure.
    pub async fn send(&self, event: CodexEvent) -> Result<()> {
        self.events
            .send(event)
            .await
            .context("Codex event channel is closed")
    }
}

#[async_trait]
/// Editor and proposal operations exposed to bounded Codex dynamic tools.
pub trait CodexToolHost: Send + 'static {
    /// Reads authoritative visible or staged contents for one session.
    async fn read_file(&mut self, session_id: &str, path: &str) -> Result<Value>;
    /// Stages complete proposed contents without mutating disk.
    async fn write_file(&mut self, session_id: &str, path: &str, content: String) -> Result<Value>;
    /// Dispatches an editor-owned semantic tool request.
    async fn editor_tool(&mut self, request: EditorToolRequest) -> Result<Value>;
}

#[derive(Debug)]
struct Session {
    cwd: PathBuf,
    active_turn: Option<String>,
    cancelled: Arc<AtomicBool>,
    tool_calls: usize,
}

enum Pending {
    Config { cwd: PathBuf },
    Requirements { cwd: PathBuf, config: Value },
    Start { cwd: PathBuf },
    Turn { session_id: String },
    Interrupt { session_id: String },
}

enum InternalEvent {
    ToolResult {
        id: Value,
        session_id: String,
        turn_id: String,
        result: std::result::Result<Value, String>,
    },
}

/// Starts a bounded Codex app-server worker and returns its editor bridge.
pub fn start_codex(
    spec: CodexProcessSpec,
    host: impl CodexToolHost,
    capacity: NonZeroUsize,
) -> Result<(CodexBridge, JoinHandle<Result<()>>)> {
    let (bridge, worker) = CodexBridge::channel(capacity);
    let task = tokio::spawn(run(spec, host, worker.commands, worker.events));
    Ok((bridge, task))
}

async fn run<H: CodexToolHost>(
    spec: CodexProcessSpec,
    host: H,
    mut commands: mpsc::Receiver<CodexCommand>,
    events: mpsc::Sender<CodexEvent>,
) -> Result<()> {
    let mut child = Command::new(&spec.command)
        .arg("app-server")
        .arg("--stdio")
        .args(&spec.args)
        .arg("-c")
        .arg("features.apps=false")
        .arg("-c")
        .arg("features.connectors=false")
        .arg("-c")
        .arg("features.plugins=false")
        .arg("-c")
        .arg("features.remote_plugin=false")
        .arg("-c")
        .arg("features.hooks=false")
        .arg("-c")
        .arg("features.codex_hooks=false")
        .envs(&spec.environment)
        .current_dir(&spec.current_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("failed to start Codex executable {:?}", spec.command))?;
    let mut input = BufWriter::new(child.stdin.take().context("Codex stdin is unavailable")?);
    let mut output = BufReader::new(child.stdout.take().context("Codex stdout is unavailable")?);

    request(
        &mut input,
        &mut output,
        json!({
            "id": "red-initialize",
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "red",
                    "title": "Red Editor",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {"experimentalApi": true}
            }
        }),
        "red-initialize",
    )
    .await
    .context("Codex app-server initialization failed")?;
    write_message(&mut input, &json!({"method": "initialized", "params": {}})).await?;
    let account = request(
        &mut input,
        &mut output,
        json!({
            "id": "red-account",
            "method": "account/read",
            "params": {"refreshToken": true}
        }),
        "red-account",
    )
    .await?;
    let authenticated = account
        .pointer("/result/account")
        .is_some_and(|account| !account.is_null())
        || account
            .pointer("/result/requiresOpenaiAuth")
            .and_then(Value::as_bool)
            == Some(false);
    anyhow::ensure!(
        authenticated,
        "Codex is not authenticated; run `codex login` and try again"
    );

    let (lines_tx, mut lines_rx) = mpsc::channel::<Result<Value>>(128);
    tokio::spawn(async move {
        loop {
            let result = read_message(&mut output).await;
            let done = matches!(&result, Ok(None));
            let message = result.and_then(|value| value.context("Codex app-server stopped"));
            if lines_tx.send(message).await.is_err() || done {
                break;
            }
        }
    });
    let host = Arc::new(Mutex::new(host));
    let (internal_tx, mut internal_rx) = mpsc::channel(128);
    let mut next_id = 1_u64;
    let mut pending = HashMap::<String, Pending>::new();
    let mut sessions = HashMap::<String, Session>::new();

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break };
                handle_command(
                    command,
                    &mut input,
                    &events,
                    &mut pending,
                    &mut sessions,
                    &mut next_id,
                ).await?;
            }
            message = lines_rx.recv() => {
                let Some(message) = message else {
                    anyhow::bail!("Codex app-server output channel stopped");
                };
                handle_message(
                    message?,
                    &mut input,
                    &events,
                    &mut pending,
                    &mut sessions,
                    &mut next_id,
                    Arc::clone(&host),
                    internal_tx.clone(),
                ).await?;
            }
            internal = internal_rx.recv() => {
                let Some(InternalEvent::ToolResult { id, session_id, turn_id, result }) = internal else {
                    continue;
                };
                let active = sessions.get(&session_id).is_some_and(|session| {
                    session.active_turn.as_deref() == Some(&turn_id)
                        && !session.cancelled.load(Ordering::Relaxed)
                });
                let result = if active {
                    result
                } else {
                    Err("Codex tool references an inactive turn".to_string())
                };
                send_tool_result(&mut input, id, result).await?;
            }
        }
    }

    drop(input);
    let _ = timeout(Duration::from_secs(2), child.wait()).await;
    Ok(())
}

async fn handle_command(
    command: CodexCommand,
    input: &mut (impl AsyncWrite + Unpin),
    events: &mpsc::Sender<CodexEvent>,
    pending: &mut HashMap<String, Pending>,
    sessions: &mut HashMap<String, Session>,
    next_id: &mut u64,
) -> Result<()> {
    match command {
        CodexCommand::NewSession { cwd } => {
            let id = rpc_id(next_id);
            pending.insert(id.clone(), Pending::Config { cwd: cwd.clone() });
            write_message(
                input,
                &json!({
                    "id": id,
                    "method": "config/read",
                    "params": {"includeLayers": false, "cwd": cwd}
                }),
            )
            .await?;
        }
        CodexCommand::Prompt { session_id, text } => {
            start_turn(session_id, text, input, events, pending, sessions, next_id).await?;
        }
        CodexCommand::PromptWithContext {
            session_id,
            text,
            uri,
            context,
        } => {
            let text =
                format!("{text}\n\nActive editor context from {uri}:\n\n```text\n{context}\n```");
            start_turn(session_id, text, input, events, pending, sessions, next_id).await?;
        }
        CodexCommand::Cancel { session_id } => {
            stop_session(session_id, false, input, events, pending, sessions, next_id).await?;
        }
        CodexCommand::CloseSession { session_id } => {
            stop_session(session_id, true, input, events, pending, sessions, next_id).await?;
        }
        CodexCommand::PermissionResponse { .. } => {}
    }
    Ok(())
}

async fn start_turn(
    session_id: String,
    text: String,
    input: &mut (impl AsyncWrite + Unpin),
    events: &mpsc::Sender<CodexEvent>,
    pending: &mut HashMap<String, Pending>,
    sessions: &mut HashMap<String, Session>,
    next_id: &mut u64,
) -> Result<()> {
    let Some(session) = sessions.get_mut(&session_id) else {
        events
            .send(CodexEvent::Failed {
                session_id: Some(session_id),
                message: "Codex session was not found".to_string(),
            })
            .await
            .ok();
        return Ok(());
    };
    if session.active_turn.is_some() {
        return Ok(());
    }
    session.cancelled.store(false, Ordering::Relaxed);
    session.tool_calls = 0;
    let id = rpc_id(next_id);
    pending.insert(
        id.clone(),
        Pending::Turn {
            session_id: session_id.clone(),
        },
    );
    write_message(
        input,
        &json!({
            "id": id,
            "method": "turn/start",
            "params": {
                "threadId": session_id,
                "input": [{"type": "text", "text": text}],
                "approvalPolicy": "never",
                "sandboxPolicy": {
                    "type": "readOnly"
                },
                "environments": []
            }
        }),
    )
    .await
}

async fn stop_session(
    session_id: String,
    close: bool,
    input: &mut (impl AsyncWrite + Unpin),
    events: &mpsc::Sender<CodexEvent>,
    pending: &mut HashMap<String, Pending>,
    sessions: &mut HashMap<String, Session>,
    next_id: &mut u64,
) -> Result<()> {
    let turn_id = sessions.get_mut(&session_id).and_then(|session| {
        session.cancelled.store(true, Ordering::Relaxed);
        session.active_turn.take()
    });
    if let Some(turn_id) = turn_id {
        let id = rpc_id(next_id);
        pending.insert(
            id.clone(),
            Pending::Interrupt {
                session_id: session_id.clone(),
            },
        );
        write_message(
            input,
            &json!({
                "id": id,
                "method": "turn/interrupt",
                "params": {"threadId": session_id, "turnId": turn_id}
            }),
        )
        .await?;
    } else {
        events
            .send(CodexEvent::Cancelled {
                session_id: session_id.clone(),
            })
            .await
            .ok();
    }
    if close {
        sessions.remove(&session_id);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_message<H: CodexToolHost>(
    message: Value,
    input: &mut (impl AsyncWrite + Unpin),
    events: &mpsc::Sender<CodexEvent>,
    pending: &mut HashMap<String, Pending>,
    sessions: &mut HashMap<String, Session>,
    next_id: &mut u64,
    host: Arc<Mutex<H>>,
    internal: mpsc::Sender<InternalEvent>,
) -> Result<()> {
    if message.get("method").is_none() {
        return handle_response(message, input, events, pending, sessions, next_id).await;
    }
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match method {
        "item/agentMessage/delta" => {
            let params = &message["params"];
            let session_id = params["threadId"].as_str().unwrap_or_default();
            let turn_id = params["turnId"].as_str().unwrap_or_default();
            let text = params["delta"].as_str().unwrap_or_default();
            if !text.is_empty()
                && sessions.get(session_id).is_some_and(|session| {
                    session.active_turn.as_deref() == Some(turn_id)
                        && !session.cancelled.load(Ordering::Relaxed)
                })
            {
                events
                    .send(CodexEvent::Update {
                        session_id: session_id.to_string(),
                        text: text.to_string(),
                    })
                    .await
                    .ok();
            }
        }
        "turn/completed" => {
            let params = &message["params"];
            let session_id = params["threadId"].as_str().unwrap_or_default().to_string();
            let turn_id = params["turn"]["id"].as_str().unwrap_or_default();
            let status = params["turn"]["status"]
                .as_str()
                .unwrap_or("completed")
                .to_string();
            if let Some(session) = sessions.get_mut(&session_id) {
                if session.active_turn.as_deref() == Some(turn_id) {
                    session.active_turn = None;
                    events
                        .send(CodexEvent::Completed {
                            session_id,
                            stop_reason: status,
                        })
                        .await
                        .ok();
                }
            }
        }
        "item/tool/call" => {
            handle_tool_call(message, input, sessions, host, internal).await?;
        }
        "item/fileChange/requestApproval" | "item/commandExecution/requestApproval" => {
            if let Some(id) = message.get("id") {
                write_message(input, &json!({"id": id, "result": {"decision": "decline"}})).await?;
            }
        }
        "item/permissions/requestApproval" => {
            if let Some(id) = message.get("id") {
                write_message(
                    input,
                    &json!({
                        "id": id,
                        "result": {"permissions": {}, "scope": "turn", "strictAutoReview": true}
                    }),
                )
                .await?;
            }
        }
        _ if message.get("id").is_some() => {
            write_message(
                input,
                &json!({
                    "id": message["id"],
                    "error": {"code": -32601, "message": "unsupported Codex server request"}
                }),
            )
            .await?;
        }
        _ => {}
    }
    Ok(())
}

async fn handle_response(
    message: Value,
    input: &mut (impl AsyncWrite + Unpin),
    events: &mpsc::Sender<CodexEvent>,
    pending: &mut HashMap<String, Pending>,
    sessions: &mut HashMap<String, Session>,
    next_id: &mut u64,
) -> Result<()> {
    let key = id_key(&message["id"]);
    let Some(request) = pending.remove(&key) else {
        return Ok(());
    };
    if let Some(error) = message.get("error") {
        let session_id = match &request {
            Pending::Turn { session_id } | Pending::Interrupt { session_id } => {
                Some(session_id.clone())
            }
            _ => None,
        };
        events
            .send(CodexEvent::Failed {
                session_id,
                message: error["message"]
                    .as_str()
                    .unwrap_or("Codex request failed")
                    .to_string(),
            })
            .await
            .ok();
        return Ok(());
    }
    match request {
        Pending::Config { cwd } => {
            let Some(config) = restricted_config(&message) else {
                events
                    .send(CodexEvent::Failed {
                        session_id: None,
                        message: "Codex could not restrict configured tools".to_string(),
                    })
                    .await
                    .ok();
                return Ok(());
            };
            let id = rpc_id(next_id);
            pending.insert(id.clone(), Pending::Requirements { cwd, config });
            write_message(
                input,
                &json!({"id": id, "method": "configRequirements/read"}),
            )
            .await?;
        }
        Pending::Requirements { cwd, config } => {
            if !requirements_are_safe(&message) {
                events
                    .send(CodexEvent::Failed {
                        session_id: None,
                        message: "Managed Codex requirements prevent a reviewable session"
                            .to_string(),
                    })
                    .await
                    .ok();
                return Ok(());
            }
            let id = rpc_id(next_id);
            pending.insert(id.clone(), Pending::Start { cwd: cwd.clone() });
            write_message(
                input,
                &json!({
                    "id": id,
                    "method": "thread/start",
                    "params": {
                        "cwd": cwd,
                        "ephemeral": true,
                        "approvalPolicy": "never",
                        "sandbox": "read-only",
                        "environments": [],
                        "config": config,
                        "dynamicTools": tool_definitions(),
                        "baseInstructions": INSTRUCTIONS,
                        "serviceName": "red"
                    }
                }),
            )
            .await?;
        }
        Pending::Start { cwd } => {
            let session_id = message
                .pointer("/result/thread/id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if session_id.is_empty() {
                events
                    .send(CodexEvent::Failed {
                        session_id: None,
                        message: "Codex returned an invalid thread".to_string(),
                    })
                    .await
                    .ok();
            } else {
                sessions.insert(
                    session_id.clone(),
                    Session {
                        cwd,
                        active_turn: None,
                        cancelled: Arc::new(AtomicBool::new(false)),
                        tool_calls: 0,
                    },
                );
                events
                    .send(CodexEvent::SessionCreated { session_id })
                    .await
                    .ok();
            }
        }
        Pending::Turn { session_id } => {
            let turn_id = message
                .pointer("/result/turn/id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if let Some(session) = sessions.get_mut(&session_id) {
                session.active_turn = Some(turn_id);
            }
        }
        Pending::Interrupt { session_id } => {
            events.send(CodexEvent::Cancelled { session_id }).await.ok();
        }
    }
    Ok(())
}

async fn handle_tool_call<H: CodexToolHost>(
    message: Value,
    input: &mut (impl AsyncWrite + Unpin),
    sessions: &mut HashMap<String, Session>,
    host: Arc<Mutex<H>>,
    internal: mpsc::Sender<InternalEvent>,
) -> Result<()> {
    let Some(id) = message.get("id").cloned() else {
        return Ok(());
    };
    let params = &message["params"];
    let session_id = params["threadId"].as_str().unwrap_or_default().to_string();
    let turn_id = params["turnId"].as_str().unwrap_or_default().to_string();
    let tool = params["tool"].as_str().unwrap_or_default().to_string();
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if serde_json::to_vec(&arguments)?.len() > TOOL_CONTENT_BYTES {
        return send_tool_result(
            input,
            id,
            Err("tool arguments exceed the limit".to_string()),
        )
        .await;
    }
    let Some(session) = sessions.get_mut(&session_id) else {
        return send_tool_result(input, id, Err("unknown Codex session".to_string())).await;
    };
    if session.active_turn.as_deref() != Some(&turn_id) || session.cancelled.load(Ordering::Relaxed)
    {
        return send_tool_result(input, id, Err("inactive Codex turn".to_string())).await;
    }
    session.tool_calls += 1;
    if session.tool_calls > MAX_TOOL_CALLS {
        return send_tool_result(input, id, Err("tool-call limit reached".to_string())).await;
    }
    let cwd = session.cwd.clone();
    let cancelled = Arc::clone(&session.cancelled);
    tokio::spawn(async move {
        let result = timeout(TOOL_TIMEOUT, async {
            match tool.as_str() {
                "list_files" => tokio::task::spawn_blocking(move || list_files(&cwd, &cancelled))
                    .await
                    .context("list_files task failed")?,
                "search_files" => {
                    let query = required_string(&arguments, "query")?.to_string();
                    tokio::task::spawn_blocking(move || search_files(&cwd, &query, &cancelled))
                        .await
                        .context("search_files task failed")?
                }
                "read_file" => {
                    let path = required_string(&arguments, "path")?;
                    host.lock().await.read_file(&session_id, path).await
                }
                "write_file" => {
                    let path = required_string(&arguments, "path")?;
                    let content = required_string(&arguments, "content")?.to_string();
                    host.lock()
                        .await
                        .write_file(&session_id, path, content)
                        .await
                }
                "get_editor_state" | "open_file" | "select_text" | "apply_edits"
                | "run_editor_action" => {
                    let call = EditorToolCall::parse(&tool, arguments)?;
                    host.lock()
                        .await
                        .editor_tool(EditorToolRequest {
                            session_id: session_id.clone(),
                            call,
                        })
                        .await
                }
                _ => anyhow::bail!("unsupported Codex dynamic tool"),
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("Codex dynamic tool timed out"))
        .and_then(|result| result)
        .map_err(|error| error.to_string());
        let _ = internal
            .send(InternalEvent::ToolResult {
                id,
                session_id,
                turn_id,
                result,
            })
            .await;
    });
    Ok(())
}

fn list_files(root: &Path, cancelled: &AtomicBool) -> Result<Value> {
    Ok(json!({"files": list_file_paths(root, cancelled)?}))
}

fn list_file_paths(root: &Path, cancelled: &AtomicBool) -> Result<Vec<String>> {
    validate_workspace_root(root)?;
    let mut files = Vec::new();
    let mut entries = 0_usize;
    let started = Instant::now();
    for entry in WalkBuilder::new(root)
        .follow_links(false)
        .hidden(false)
        .build()
    {
        if cancelled.load(Ordering::Relaxed) {
            anyhow::bail!("Codex turn was cancelled");
        }
        entries = entries.saturating_add(1);
        if entries > MAX_WALK_ENTRIES || started.elapsed() >= MAX_WALK_TIME {
            break;
        }
        let entry = entry?;
        if entry.file_type().is_some_and(|kind| kind.is_file()) {
            if let Ok(path) = entry.path().strip_prefix(root) {
                files.push(path.to_string_lossy().replace('\\', "/"));
                if files.len() == MAX_FILES {
                    break;
                }
            }
        }
    }
    files.sort_unstable();
    Ok(files)
}

fn search_files(root: &Path, query: &str, cancelled: &AtomicBool) -> Result<Value> {
    anyhow::ensure!(
        !query.is_empty() && query.len() <= 1024,
        "invalid search query"
    );
    #[cfg(not(unix))]
    {
        let _ = (root, cancelled);
        anyhow::bail!("workspace content search is unavailable on this platform");
    }

    #[cfg(unix)]
    {
        let files = list_file_paths(root, cancelled)?;
        let mut matches = Vec::new();
        let mut searched = 0_u64;
        for relative in files {
            if cancelled.load(Ordering::Relaxed) {
                anyhow::bail!("Codex turn was cancelled");
            }
            let Some((content, bytes)) = read_workspace_file(root, &relative)? else {
                continue;
            };
            searched = searched.saturating_add(bytes);
            if searched > MAX_SEARCH_BYTES {
                break;
            }
            for (line, text) in content.lines().enumerate() {
                if cancelled.load(Ordering::Relaxed) {
                    anyhow::bail!("Codex turn was cancelled");
                }
                if text.contains(query) {
                    matches.push(json!({
                        "path": relative,
                        "line": line + 1,
                        "text": text.chars().take(300).collect::<String>()
                    }));
                    if matches.len() == MAX_MATCHES {
                        return Ok(json!({"matches": matches}));
                    }
                }
            }
        }
        Ok(json!({"matches": matches}))
    }
}

fn validate_workspace_root(root: &Path) -> Result<()> {
    anyhow::ensure!(root.is_absolute(), "workspace root must be absolute");
    let inspected = physical_workspace_root(root);
    for ancestor in inspected.ancestors() {
        let metadata =
            std::fs::symlink_metadata(ancestor).context("failed to inspect workspace root")?;
        anyhow::ensure!(
            !metadata.file_type().is_symlink(),
            "workspace root cannot contain a symlink"
        );
    }
    anyhow::ensure!(
        std::fs::symlink_metadata(inspected)?.is_dir(),
        "workspace root must be a directory"
    );
    Ok(())
}

fn physical_workspace_root(root: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        for (alias, target) in [
            (Path::new("/var"), Path::new("/private/var")),
            (Path::new("/tmp"), Path::new("/private/tmp")),
            (Path::new("/etc"), Path::new("/private/etc")),
        ] {
            if let Ok(suffix) = root.strip_prefix(alias) {
                return target.join(suffix);
            }
        }
    }
    root.to_path_buf()
}

#[cfg(unix)]
fn open_workspace_file(root: &Path, relative: &Path) -> Result<Option<File>> {
    use std::os::fd::{AsRawFd, FromRawFd};

    use nix::{
        fcntl::{openat, OFlag},
        sys::stat::Mode,
    };

    let components = relative.components().collect::<Vec<_>>();
    if components.is_empty() {
        return Ok(None);
    }
    let inspected = physical_workspace_root(root);
    let descriptor = openat(
        None,
        Path::new("/"),
        OFlag::O_RDONLY
            | OFlag::O_CLOEXEC
            | OFlag::O_DIRECTORY
            | OFlag::O_NOFOLLOW
            | OFlag::O_NONBLOCK,
        Mode::empty(),
    )
    .context("failed to safely open filesystem root")?;
    // SAFETY: `openat` returned a new descriptor and `File` becomes its sole owner.
    let mut directory = unsafe { File::from_raw_fd(descriptor) };
    for component in inspected.components() {
        let name = match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => name,
            Component::ParentDir | Component::Prefix(_) => {
                anyhow::bail!("workspace root contains a non-normal path component");
            }
        };
        let descriptor = openat(
            Some(directory.as_raw_fd()),
            name,
            OFlag::O_RDONLY
                | OFlag::O_CLOEXEC
                | OFlag::O_DIRECTORY
                | OFlag::O_NOFOLLOW
                | OFlag::O_NONBLOCK,
            Mode::empty(),
        )
        .context("failed to safely open workspace root component")?;
        // SAFETY: `openat` returned a new descriptor and `File` becomes its sole owner.
        directory = unsafe { File::from_raw_fd(descriptor) };
    }
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            anyhow::bail!("workspace walker returned a non-normal path");
        };
        let final_component = index + 1 == components.len();
        let mut flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK;
        if !final_component {
            flags |= OFlag::O_DIRECTORY;
        }
        let descriptor = match openat(Some(directory.as_raw_fd()), *name, flags, Mode::empty()) {
            Ok(descriptor) => descriptor,
            Err(_) => return Ok(None),
        };
        // SAFETY: `openat` returned a new descriptor and `File` becomes its sole owner.
        let file = unsafe { File::from_raw_fd(descriptor) };
        if final_component {
            return Ok(Some(file));
        }
        directory = file;
    }
    Ok(None)
}

#[cfg(unix)]
fn read_workspace_file(root: &Path, relative: &str) -> Result<Option<(String, u64)>> {
    let Some(file) = open_workspace_file(root, Path::new(relative))? else {
        return Ok(None);
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > TOOL_CONTENT_BYTES as u64 {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(TOOL_CONTENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > TOOL_CONTENT_BYTES {
        return Ok(None);
    }
    let byte_count = bytes.len() as u64;
    let Ok(content) = String::from_utf8(bytes) else {
        return Ok(None);
    };
    Ok(Some((content, byte_count)))
}

fn restricted_config(response: &Value) -> Option<Value> {
    let configured = response
        .pointer("/result/config/mcp_servers")?
        .as_object()?;
    let mut mcp_servers = serde_json::Map::new();
    for name in configured.keys() {
        mcp_servers.insert(name.clone(), json!({"enabled": false}));
    }
    Some(json!({
        "mcp_servers": mcp_servers,
        "features": {
            "apps": false,
            "connectors": false,
            "plugins": false,
            "remote_plugin": false,
            "skill_mcp_dependency_install": false,
            "hooks": false,
            "codex_hooks": false
        },
        "orchestrator": {"mcp": {"enabled": false}},
        "notify": []
    }))
}

fn requirements_are_safe(response: &Value) -> bool {
    let Some(features) = response
        .pointer("/result/requirements/featureRequirements")
        .and_then(Value::as_object)
    else {
        return response
            .pointer("/result/requirements")
            .is_none_or(Value::is_null);
    };
    [
        "apps",
        "connectors",
        "plugins",
        "skill_mcp_dependency_install",
        "hooks",
        "codex_hooks",
    ]
    .iter()
    .all(|name| features.get(*name).and_then(Value::as_bool) != Some(true))
}

fn tool_definitions() -> Value {
    let mut tools = vec![
        json!({"type": "function", "name": "list_files", "description": "List workspace files, respecting ignore files.", "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}}),
        json!({"type": "function", "name": "search_files", "description": "Search workspace text files.", "inputSchema": {"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"], "additionalProperties": false}}),
        json!({"type": "function", "name": "read_file", "description": "Read through Red so unsaved contents are visible.", "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"], "additionalProperties": false}}),
        json!({"type": "function", "name": "write_file", "description": "Stage complete contents as a reviewable Red proposal.", "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}, "required": ["path", "content"], "additionalProperties": false}}),
    ];
    tools.extend(editor_tool_schemas("inputSchema"));
    Value::Array(tools)
}

async fn send_tool_result(
    input: &mut (impl AsyncWrite + Unpin),
    id: Value,
    result: std::result::Result<Value, String>,
) -> Result<()> {
    let (mut success, text) = match result {
        Ok(value) => (true, serde_json::to_string(&value)?),
        Err(error) => (false, error),
    };
    let text = if text.len() <= TOOL_CONTENT_BYTES {
        text
    } else {
        success = false;
        "Codex dynamic-tool response exceeds the size limit".to_string()
    };
    write_message(
        input,
        &json!({
            "id": id,
            "result": {
                "contentItems": [{"type": "inputText", "text": text}],
                "success": success
            }
        }),
    )
    .await
}

async fn request(
    input: &mut (impl AsyncWrite + Unpin),
    output: &mut (impl AsyncBufReadExt + Unpin),
    message: Value,
    expected_id: &str,
) -> Result<Value> {
    write_message(input, &message).await?;
    timeout(SETUP_TIMEOUT, async {
        loop {
            let message = read_message(output)
                .await?
                .context("Codex app-server stopped during setup")?;
            if message["id"].as_str() == Some(expected_id) {
                anyhow::ensure!(
                    message.get("error").is_none(),
                    "{}",
                    message["error"]["message"]
                        .as_str()
                        .unwrap_or("Codex setup request failed")
                );
                return Ok(message);
            }
        }
    })
    .await
    .context("Codex app-server setup timed out")?
}

async fn read_message(reader: &mut (impl AsyncBufReadExt + Unpin)) -> Result<Option<Value>> {
    let mut line = Vec::new();
    let bytes = reader
        .take((APP_FRAME_BYTES + 1) as u64)
        .read_until(b'\n', &mut line)
        .await?;
    if bytes == 0 {
        return Ok(None);
    }
    anyhow::ensure!(
        line.len() <= APP_FRAME_BYTES && line.last() == Some(&b'\n'),
        "Codex app-server frame exceeds the limit"
    );
    line.pop();
    Ok(Some(serde_json::from_slice(&line)?))
}

async fn write_message(writer: &mut (impl AsyncWrite + Unpin), message: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(message)?;
    anyhow::ensure!(
        bytes.len().saturating_add(1) <= APP_FRAME_BYTES,
        "Codex app-server frame exceeds the limit"
    );
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

fn rpc_id(next_id: &mut u64) -> String {
    let id = format!("red-{}", *next_id);
    *next_id += 1;
    id
}

fn id_key(id: &Value) -> String {
    id.as_str()
        .map(str::to_string)
        .unwrap_or_else(|| id.to_string())
}

fn required_string<'a>(arguments: &'a Value, name: &str) -> Result<&'a str> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .with_context(|| format!("missing string argument {name}"))
}

#[must_use]
pub fn find_executable(command: &str) -> Option<PathBuf> {
    let command = Path::new(command);
    if command.components().count() > 1 {
        return is_executable(command).then(|| command.to_path_buf());
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).find_map(|directory| find_in_directory(&directory, command))
    })
}

#[cfg(not(windows))]
fn find_in_directory(directory: &Path, command: &Path) -> Option<PathBuf> {
    let candidate = directory.join(command);
    is_executable(&candidate).then_some(candidate)
}

#[cfg(windows)]
fn find_in_directory(directory: &Path, command: &Path) -> Option<PathBuf> {
    let candidate = directory.join(command);
    if is_executable(&candidate) {
        return Some(candidate);
    }
    if candidate.extension().is_some() {
        return None;
    }
    std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
        .split(';')
        .filter(|extension| !extension.is_empty())
        .map(|extension| directory.join(format!("{}{}", command.to_string_lossy(), extension)))
        .find(|path| is_executable(path))
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::{symlink, PermissionsExt as _};

    use super::*;

    #[test]
    fn search_is_bounded_to_regular_files_below_a_physical_root() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("inside.txt"), "needle inside\n").unwrap();
        std::fs::write(outside.path().join("outside.txt"), "needle outside\n").unwrap();
        symlink(
            outside.path().join("outside.txt"),
            root.path().join("linked.txt"),
        )
        .unwrap();

        let result = search_files(root.path(), "needle", &AtomicBool::new(false)).unwrap();

        assert_eq!(result["matches"].as_array().unwrap().len(), 1);
        assert_eq!(result["matches"][0]["path"], "inside.txt");
    }

    #[test]
    fn search_rejects_a_symlinked_workspace_root() {
        let directory = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let root = directory.path().join("workspace");
        symlink(target.path(), &root).unwrap();

        let error = search_files(&root, "needle", &AtomicBool::new(false)).unwrap_err();

        assert!(error.to_string().contains("symlink"));
    }

    #[test]
    fn executable_discovery_rejects_non_executable_files() {
        let directory = tempfile::tempdir().unwrap();
        let command = directory.path().join("codex");
        std::fs::write(&command, "not executable").unwrap();
        std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o644)).unwrap();

        assert!(find_executable(command.to_str().unwrap()).is_none());
    }
}
