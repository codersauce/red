//! Direct client for the installed Codex app-server.
//!
//! Red runs the user's installed Codex app-server directly and supplements its
//! native capabilities with bounded, editor-aware dynamic tools. An explicitly
//! selected review-safe profile retains isolated, proposal-backed edits.

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

use crate::agent_tools::{
    editor_tool_schemas, ensure_agent_path_disclosable, EditorToolCall, EditorToolRequest,
};

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
const MAX_PENDING_APPROVALS: usize = 32;
const MAX_MODEL_BYTES: usize = 256;
const MAX_REASONING_EFFORT_BYTES: usize = 64;
const REVIEW_SAFE_INSTRUCTIONS: &str = "You are Red's coding assistant. You have no shell or native patch tool. Use list_files and search_files to locate relevant code. Use get_editor_state, open_file, select_text, and run_editor_action to inspect and navigate the editor. Always use read_file before reasoning about a file, and use apply_edits or write_file for every edit. Edits are reviewable editor proposals and never touch disk. Do not claim a change was saved. Keep responses concise.";

/// Capability profile selected for an installed Codex app-server.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CodexExecutionMode {
    /// Preserve the user's actual Codex configuration, tools, and approvals.
    #[default]
    Native,
    /// Disable native writes and run solely through Red's proposal workspace.
    ReviewSafe,
}

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
    /// Whether Codex uses native capabilities or isolated review-safe tools.
    pub execution_mode: CodexExecutionMode,
    /// Whether newly created Codex threads survive app-server shutdown.
    pub persistent_threads: bool,
    /// Whether prompts request Codex's built-in planning collaboration mode.
    pub plan_mode: bool,
    /// Optional model override applied to newly created threads.
    pub model: Option<String>,
    /// Optional reasoning effort applied when a new turn starts.
    pub reasoning_effort: Option<String>,
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
            execution_mode: CodexExecutionMode::Native,
            persistent_threads: true,
            plan_mode: false,
            model: None,
            reasoning_effort: None,
        }
    }

    #[must_use]
    /// Appends literal process arguments without shell expansion.
    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    #[must_use]
    /// Selects native Codex capabilities or Red's isolated review-safe profile.
    pub fn execution_mode(mut self, mode: CodexExecutionMode) -> Self {
        self.execution_mode = mode;
        self
    }

    #[must_use]
    /// Controls whether newly created conversations are persisted by Codex.
    pub fn persistent_threads(mut self, persistent: bool) -> Self {
        self.persistent_threads = persistent;
        self
    }

    #[must_use]
    /// Requests native planning once the server provides the effective model.
    pub fn plan_mode(mut self, enabled: bool) -> Self {
        self.plan_mode = enabled;
        self
    }

    #[must_use]
    /// Selects a Codex model without overriding the remaining user configuration.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    #[must_use]
    /// Selects the reasoning effort advertised by the installed Codex model.
    pub fn reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        self.reasoning_effort = Some(effort.into());
        self
    }
}

/// Commands sent from the editor owner to the Codex worker.
#[derive(Debug, Clone)]
pub enum CodexCommand {
    /// Creates an app-server thread for a workspace.
    NewSession {
        /// Physical workspace root.
        cwd: PathBuf,
    },
    /// Resumes a persisted app-server thread in its original workspace.
    ResumeSession {
        /// Persisted Codex thread identifier.
        session_id: String,
        /// Physical workspace root.
        cwd: PathBuf,
    },
    /// Recovers an automatically persisted thread or starts one fresh session.
    RecoverSession {
        /// Persisted Codex thread identifier to attempt exactly once.
        session_id: String,
        /// Physical workspace root used if the thread is no longer available.
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
    /// Adds input directly to an existing, steerable Codex turn.
    Steer {
        /// Active Codex thread identifier.
        session_id: String,
        /// Additional user instructions.
        text: String,
    },
    /// Requests models available to an active conversation.
    ListModels {
        /// Conversation receiving the model-catalog activity.
        session_id: String,
    },
    /// Selects the model and optional reasoning effort for subsequent turns.
    SetModel {
        /// Conversation whose future turns use the selected model.
        session_id: String,
        /// Exact model identifier returned by the Codex model catalog.
        model: String,
        /// Optional supported reasoning effort for the selected model.
        reasoning_effort: Option<String>,
    },
    /// Requests durable conversations belonging to a workspace.
    ListSessions {
        /// Conversation receiving the workspace session-list activity.
        session_id: String,
        /// Physical workspace root used to filter Codex threads.
        cwd: PathBuf,
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
    /// Returns bounded visible and staged files that differ from disk.
    async fn overlay_files(&mut self, _session_id: &str) -> Result<Vec<(String, String)>> {
        Ok(Vec::new())
    }
}

#[derive(Debug)]
struct Session {
    cwd: PathBuf,
    active_turn: Option<String>,
    cancelled: Arc<AtomicBool>,
    tool_calls: usize,
    model: Option<String>,
    reasoning_effort: Option<String>,
    close_requested: bool,
}

enum Pending {
    Config {
        cwd: PathBuf,
    },
    Requirements {
        cwd: PathBuf,
        config: Value,
    },
    Start {
        cwd: PathBuf,
    },
    Resume {
        cwd: PathBuf,
        session_id: String,
        recover: bool,
    },
    Turn {
        session_id: String,
    },
    Interrupt {
        session_id: String,
        turn_id: String,
    },
    Steer {
        session_id: String,
    },
    Models {
        session_id: String,
    },
    Sessions {
        session_id: String,
    },
}

#[derive(Debug, Clone)]
struct WorkerSettings {
    execution_mode: CodexExecutionMode,
    persistent_threads: bool,
    plan_mode: bool,
    model: Option<String>,
    reasoning_effort: Option<String>,
}

#[derive(Debug)]
struct PendingApproval {
    id: Value,
    session_id: String,
    turn_id: String,
    responses: HashMap<String, Value>,
    declined: Value,
}

enum InternalEvent {
    ToolResult {
        id: Value,
        session_id: String,
        turn_id: String,
        tool: String,
        title: String,
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
    let settings = WorkerSettings {
        execution_mode: spec.execution_mode,
        persistent_threads: spec.persistent_threads,
        plan_mode: spec.plan_mode,
        model: spec.model.clone(),
        reasoning_effort: spec.reasoning_effort.clone(),
    };
    let mut command = Command::new(&spec.command);
    command.arg("app-server").arg("--stdio").args(&spec.args);
    if settings.execution_mode == CodexExecutionMode::ReviewSafe {
        for feature in ["apps", "connectors", "plugins", "remote_plugin"] {
            command.arg("-c").arg(format!("features.{feature}=false"));
        }
    }
    let mut child = command
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
    let mut approvals = HashMap::<String, PendingApproval>::new();

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
                    &mut approvals,
                    &mut next_id,
                    &settings,
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
                    &mut approvals,
                    &mut next_id,
                    Arc::clone(&host),
                    internal_tx.clone(),
                    &settings,
                ).await?;
            }
            internal = internal_rx.recv() => {
                let Some(InternalEvent::ToolResult {
                    id,
                    session_id,
                    turn_id,
                    tool,
                    title,
                    result,
                }) = internal else {
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
                if active {
                    let status = if result.is_ok() { "completed" } else { "failed" };
                    send_activity(
                        &events,
                        &session_id,
                        json!({
                            "session_update": "tool_call_update",
                            "tool_call_id": id_key(&id),
                            "title": title,
                            "kind": tool,
                            "status": status,
                        }),
                    )
                    .await;
                    if result.is_ok() && matches!(tool.as_str(), "write_file" | "apply_edits") {
                        let _ = events
                            .send(CodexEvent::ProposalsChanged {
                                session_id: session_id.clone(),
                            })
                            .await;
                    }
                }
                send_tool_result(&mut input, id, result).await?;
            }
        }
    }

    drop(input);
    let _ = timeout(Duration::from_secs(2), child.wait()).await;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_command(
    command: CodexCommand,
    input: &mut (impl AsyncWrite + Unpin),
    events: &mpsc::Sender<CodexEvent>,
    pending: &mut HashMap<String, Pending>,
    sessions: &mut HashMap<String, Session>,
    approvals: &mut HashMap<String, PendingApproval>,
    next_id: &mut u64,
    settings: &WorkerSettings,
) -> Result<()> {
    match command {
        CodexCommand::NewSession { cwd } => {
            request_session_config(cwd, input, pending, next_id).await?;
        }
        CodexCommand::ResumeSession { session_id, cwd } => {
            request_session_resume(session_id, cwd, false, input, pending, next_id).await?;
        }
        CodexCommand::RecoverSession { session_id, cwd } => {
            request_session_resume(session_id, cwd, true, input, pending, next_id).await?;
        }
        CodexCommand::Prompt { session_id, text } => {
            start_turn(
                session_id, text, input, events, pending, sessions, next_id, settings,
            )
            .await?;
        }
        CodexCommand::PromptWithContext {
            session_id,
            text,
            uri,
            context,
        } => {
            let text =
                format!("{text}\n\nActive editor context from {uri}:\n\n```text\n{context}\n```");
            start_turn(
                session_id, text, input, events, pending, sessions, next_id, settings,
            )
            .await?;
        }
        CodexCommand::Steer { session_id, text } => {
            steer_turn(session_id, text, input, events, pending, sessions, next_id).await?;
        }
        CodexCommand::ListModels { session_id } => {
            let id = rpc_id(next_id);
            pending.insert(id.clone(), Pending::Models { session_id });
            write_message(
                input,
                &json!({"id": id, "method": "model/list", "params": {}}),
            )
            .await?;
        }
        CodexCommand::SetModel {
            session_id,
            model,
            reasoning_effort,
        } => {
            let valid_model = !model.trim().is_empty()
                && model.len() <= MAX_MODEL_BYTES
                && !model.chars().any(char::is_control);
            let valid_effort = reasoning_effort.as_deref().is_none_or(|effort| {
                !effort.trim().is_empty()
                    && effort.len() <= MAX_REASONING_EFFORT_BYTES
                    && !effort.chars().any(char::is_control)
            });
            if !valid_model || !valid_effort {
                let _ = events
                    .send(CodexEvent::Failed {
                        session_id: Some(session_id),
                        message: "Codex model or reasoning effort is invalid".to_string(),
                    })
                    .await;
                return Ok(());
            }
            let Some(session) = sessions.get_mut(&session_id) else {
                let _ = events
                    .send(CodexEvent::Failed {
                        session_id: Some(session_id),
                        message: "Codex session was not found".to_string(),
                    })
                    .await;
                return Ok(());
            };
            session.model = Some(model.clone());
            session.reasoning_effort.clone_from(&reasoning_effort);
            send_activity(
                events,
                &session_id,
                json!({
                    "session_update": "model_selected",
                    "model": model,
                    "reasoning_effort": reasoning_effort,
                }),
            )
            .await;
        }
        CodexCommand::ListSessions { session_id, cwd } => {
            let id = rpc_id(next_id);
            pending.insert(id.clone(), Pending::Sessions { session_id });
            write_message(
                input,
                &json!({
                    "id": id,
                    "method": "thread/list",
                    "params": {"cwd": cwd, "archived": false, "limit": 50},
                }),
            )
            .await?;
        }
        CodexCommand::Cancel { session_id } => {
            stop_session(
                session_id, false, input, events, pending, sessions, approvals, next_id,
            )
            .await?;
        }
        CodexCommand::CloseSession { session_id } => {
            stop_session(
                session_id, true, input, events, pending, sessions, approvals, next_id,
            )
            .await?;
        }
        CodexCommand::PermissionResponse {
            request_id,
            option_id,
        } => {
            if let Some(approval) = approvals.remove(&request_id) {
                let response = option_id
                    .as_deref()
                    .and_then(|option| approval.responses.get(option))
                    .cloned()
                    .unwrap_or(approval.declined);
                write_message(input, &json!({"id": approval.id, "result": response})).await?;
            }
        }
    }
    Ok(())
}

async fn request_session_config(
    cwd: PathBuf,
    input: &mut (impl AsyncWrite + Unpin),
    pending: &mut HashMap<String, Pending>,
    next_id: &mut u64,
) -> Result<()> {
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
    .await
}

async fn request_session_resume(
    session_id: String,
    cwd: PathBuf,
    recover: bool,
    input: &mut (impl AsyncWrite + Unpin),
    pending: &mut HashMap<String, Pending>,
    next_id: &mut u64,
) -> Result<()> {
    let id = rpc_id(next_id);
    pending.insert(
        id.clone(),
        Pending::Resume {
            cwd: cwd.clone(),
            session_id: session_id.clone(),
            recover,
        },
    );
    write_message(
        input,
        &json!({
            "id": id,
            "method": "thread/resume",
            "params": {"threadId": session_id, "cwd": cwd},
        }),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn start_turn(
    session_id: String,
    text: String,
    input: &mut (impl AsyncWrite + Unpin),
    events: &mpsc::Sender<CodexEvent>,
    pending: &mut HashMap<String, Pending>,
    sessions: &mut HashMap<String, Session>,
    next_id: &mut u64,
    settings: &WorkerSettings,
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
    let mut params = json!({
        "threadId": session_id,
        "input": [{"type": "text", "text": text}],
    });
    if settings.execution_mode == CodexExecutionMode::ReviewSafe {
        params["approvalPolicy"] = json!("never");
        params["sandboxPolicy"] = json!({"type": "readOnly"});
        params["environments"] = json!([]);
    }
    if let Some(model) = session.model.as_ref().or(settings.model.as_ref()) {
        params["model"] = json!(model);
    }
    if let Some(effort) = session
        .reasoning_effort
        .as_ref()
        .or(settings.reasoning_effort.as_ref())
    {
        params["effort"] = json!(effort);
    }
    if settings.plan_mode {
        if let Some(model) = session.model.as_ref().or(settings.model.as_ref()) {
            params["collaborationMode"] = json!({
                "mode": "plan",
                "settings": {
                    "model": model,
                    "reasoning_effort": session
                        .reasoning_effort
                        .as_deref()
                        .or(settings.reasoning_effort.as_deref())
                        .unwrap_or("medium"),
                    "developer_instructions": null,
                },
            });
        }
    }
    write_message(
        input,
        &json!({"id": id, "method": "turn/start", "params": params}),
    )
    .await
}

async fn steer_turn(
    session_id: String,
    text: String,
    input: &mut (impl AsyncWrite + Unpin),
    events: &mpsc::Sender<CodexEvent>,
    pending: &mut HashMap<String, Pending>,
    sessions: &HashMap<String, Session>,
    next_id: &mut u64,
) -> Result<()> {
    let Some(turn_id) = sessions
        .get(&session_id)
        .filter(|session| !session.cancelled.load(Ordering::Relaxed))
        .and_then(|session| session.active_turn.as_deref())
    else {
        let _ = events
            .send(CodexEvent::Failed {
                session_id: Some(session_id),
                message: "there is no active Codex turn to steer".to_string(),
            })
            .await;
        return Ok(());
    };
    let id = rpc_id(next_id);
    pending.insert(
        id.clone(),
        Pending::Steer {
            session_id: session_id.clone(),
        },
    );
    write_message(
        input,
        &json!({
            "id": id,
            "method": "turn/steer",
            "params": {
                "threadId": session_id,
                "expectedTurnId": turn_id,
                "input": [{"type": "text", "text": text}],
            },
        }),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn stop_session(
    session_id: String,
    close: bool,
    input: &mut (impl AsyncWrite + Unpin),
    events: &mpsc::Sender<CodexEvent>,
    pending: &mut HashMap<String, Pending>,
    sessions: &mut HashMap<String, Session>,
    approvals: &mut HashMap<String, PendingApproval>,
    next_id: &mut u64,
) -> Result<()> {
    let turn_id = sessions.get_mut(&session_id).and_then(|session| {
        session.cancelled.store(true, Ordering::Relaxed);
        session.close_requested |= close;
        session.active_turn.clone()
    });
    let has_active_turn = turn_id.is_some();
    reject_session_approvals(input, approvals, &session_id).await?;
    if let Some(turn_id) = turn_id {
        let id = rpc_id(next_id);
        pending.insert(
            id.clone(),
            Pending::Interrupt {
                session_id: session_id.clone(),
                turn_id: turn_id.clone(),
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
    if close && !has_active_turn {
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
    approvals: &mut HashMap<String, PendingApproval>,
    next_id: &mut u64,
    host: Arc<Mutex<H>>,
    internal: mpsc::Sender<InternalEvent>,
    settings: &WorkerSettings,
) -> Result<()> {
    if message.get("method").is_none() {
        return handle_response(message, input, events, pending, sessions, next_id, settings).await;
    }
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match method {
        "turn/started" => {
            let params = &message["params"];
            let session_id = params["threadId"].as_str().unwrap_or_default();
            let turn_id = params
                .pointer("/turn/id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if let Some(session) = sessions.get_mut(session_id) {
                if session.active_turn.is_none() && !turn_id.is_empty() {
                    session.active_turn = Some(turn_id.to_string());
                }
                if session.active_turn.as_deref() == Some(turn_id) {
                    send_activity(
                        events,
                        session_id,
                        json!({"session_update": "turn_started", "turn_id": turn_id}),
                    )
                    .await;
                }
            }
        }
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
        "item/started" | "item/completed" => {
            forward_item_activity(events, sessions, method, &message["params"]).await;
        }
        "item/reasoning/summaryTextDelta" => {
            let params = &message["params"];
            let session_id = params["threadId"].as_str().unwrap_or_default();
            let turn_id = params["turnId"].as_str().unwrap_or_default();
            let delta = params["delta"].as_str().unwrap_or_default();
            if !delta.is_empty() && session_accepts_activity(sessions, session_id, turn_id) {
                send_activity(
                    events,
                    session_id,
                    json!({
                        "session_update": "agent_thought_chunk",
                        "content": {"type": "text", "text": delta},
                        "item_id": params["itemId"],
                    }),
                )
                .await;
            }
        }
        "turn/plan/updated" => {
            let params = &message["params"];
            let session_id = params["threadId"].as_str().unwrap_or_default();
            let turn_id = params["turnId"].as_str().unwrap_or_default();
            if session_accepts_activity(sessions, session_id, turn_id) {
                send_activity(
                    events,
                    session_id,
                    json!({
                        "session_update": "plan",
                        "turn_id": turn_id,
                        "explanation": params["explanation"],
                        "plan": params["plan"],
                    }),
                )
                .await;
            }
        }
        "thread/tokenUsage/updated" => {
            let params = &message["params"];
            let session_id = params["threadId"].as_str().unwrap_or_default();
            if sessions.contains_key(session_id) {
                send_activity(
                    events,
                    session_id,
                    json!({
                        "session_update": "token_usage",
                        "token_usage": params["tokenUsage"],
                    }),
                )
                .await;
            }
        }
        "error" => {
            let params = &message["params"];
            let session_id = params["threadId"].as_str().unwrap_or_default();
            let turn_id = params["turnId"].as_str().unwrap_or_default();
            if session_accepts_activity(sessions, session_id, turn_id) {
                send_activity(
                    events,
                    session_id,
                    json!({
                        "session_update": "error",
                        "error": params["error"],
                        "will_retry": params["willRetry"],
                    }),
                )
                .await;
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
            let Some(session) = sessions.get_mut(&session_id) else {
                return Ok(());
            };
            if session.active_turn.as_deref() != Some(turn_id) {
                return Ok(());
            }
            session.active_turn = None;
            session.cancelled.store(false, Ordering::Relaxed);
            let close_requested = session.close_requested;
            approvals.retain(|_, approval| {
                approval.session_id != session_id || approval.turn_id != turn_id
            });
            if status == "failed" {
                let message = params
                    .pointer("/turn/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("Codex turn failed")
                    .to_string();
                let _ = events
                    .send(CodexEvent::Failed {
                        session_id: Some(session_id.clone()),
                        message,
                    })
                    .await;
            } else {
                let _ = events
                    .send(CodexEvent::Completed {
                        session_id: session_id.clone(),
                        stop_reason: status,
                    })
                    .await;
            }
            if close_requested {
                sessions.remove(&session_id);
            }
        }
        "item/tool/call" => {
            handle_tool_call(message, input, events, sessions, host, internal).await?;
        }
        "item/fileChange/requestApproval"
        | "item/commandExecution/requestApproval"
        | "item/permissions/requestApproval" => {
            handle_approval_request(message, input, events, sessions, approvals, settings).await?;
        }
        "serverRequest/resolved" => {
            if let Some(id) = message.pointer("/params/requestId") {
                approvals.remove(&id_key(id));
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

fn session_accepts_activity(
    sessions: &HashMap<String, Session>,
    session_id: &str,
    turn_id: &str,
) -> bool {
    !session_id.is_empty()
        && !turn_id.is_empty()
        && sessions.get(session_id).is_some_and(|session| {
            session.active_turn.as_deref() == Some(turn_id)
                && !session.cancelled.load(Ordering::Relaxed)
        })
}

async fn send_activity(events: &mpsc::Sender<CodexEvent>, session_id: &str, update: Value) {
    if session_id.is_empty() {
        return;
    }
    let _ = events
        .send(CodexEvent::Activity {
            session_id: session_id.to_string(),
            update,
        })
        .await;
}

async fn forward_item_activity(
    events: &mpsc::Sender<CodexEvent>,
    sessions: &HashMap<String, Session>,
    method: &str,
    params: &Value,
) {
    let session_id = params["threadId"].as_str().unwrap_or_default();
    let turn_id = params["turnId"].as_str().unwrap_or_default();
    if !session_accepts_activity(sessions, session_id, turn_id) {
        return;
    }
    let item = &params["item"];
    let kind = item["type"].as_str().unwrap_or_default();
    if matches!(kind, "agentMessage" | "userMessage" | "reasoning") {
        return;
    }
    let default_status = if method == "item/completed" {
        "completed"
    } else {
        "in_progress"
    };
    let status = item["status"]
        .as_str()
        .map(normalized_item_status)
        .unwrap_or(default_status);
    let title = item_activity_title(kind, item);
    let session_update = if method == "item/completed" {
        "tool_call_update"
    } else {
        "tool_call"
    };
    send_activity(
        events,
        session_id,
        json!({
            "session_update": session_update,
            "tool_call_id": item["id"],
            "title": title,
            "kind": kind,
            "status": status,
            "item": item,
        }),
    )
    .await;
}

fn normalized_item_status(status: &str) -> &str {
    match status {
        "inProgress" => "in_progress",
        "notStarted" => "pending",
        other => other,
    }
}

fn item_activity_title(kind: &str, item: &Value) -> String {
    let title = match kind {
        "commandExecution" => item["command"]
            .as_str()
            .map(|command| format!("Running {command}"))
            .unwrap_or_else(|| "Running a command".to_string()),
        "fileChange" => {
            let paths = item["changes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|change| change["path"].as_str())
                .take(3)
                .collect::<Vec<_>>();
            if paths.is_empty() {
                "Updating workspace files".to_string()
            } else {
                format!("Updating {}", paths.join(", "))
            }
        }
        "mcpToolCall" => {
            let server = item["server"].as_str().unwrap_or("MCP");
            let tool = item["tool"].as_str().unwrap_or("tool");
            format!("Calling {server}: {tool}")
        }
        "dynamicToolCall" => item["tool"]
            .as_str()
            .map(|tool| format!("Using {tool}"))
            .unwrap_or_else(|| "Using an editor tool".to_string()),
        "webSearch" => item["query"]
            .as_str()
            .map(|query| format!("Searching for {query}"))
            .unwrap_or_else(|| "Searching the web".to_string()),
        "plan" => "Preparing a plan".to_string(),
        "contextCompaction" => "Compacting conversation context".to_string(),
        "enteredReviewMode" => "Reviewing changes".to_string(),
        "exitedReviewMode" => "Review completed".to_string(),
        "collabToolCall" => "Coordinating an agent".to_string(),
        _ => item["title"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| "Working".to_string()),
    };
    title.chars().take(240).collect()
}

async fn reject_session_approvals(
    input: &mut (impl AsyncWrite + Unpin),
    approvals: &mut HashMap<String, PendingApproval>,
    session_id: &str,
) -> Result<()> {
    let request_ids = approvals
        .iter()
        .filter(|(_, approval)| approval.session_id == session_id)
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    for request_id in request_ids {
        if let Some(approval) = approvals.remove(&request_id) {
            write_message(
                input,
                &json!({"id": approval.id, "result": approval.declined}),
            )
            .await?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_approval_request(
    message: Value,
    input: &mut (impl AsyncWrite + Unpin),
    events: &mpsc::Sender<CodexEvent>,
    sessions: &HashMap<String, Session>,
    approvals: &mut HashMap<String, PendingApproval>,
    settings: &WorkerSettings,
) -> Result<()> {
    let Some(id) = message.get("id").cloned() else {
        return Ok(());
    };
    let params = &message["params"];
    let session_id = params["threadId"].as_str().unwrap_or_default();
    let turn_id = params["turnId"].as_str().unwrap_or_default();
    let method = message["method"].as_str().unwrap_or_default();
    let is_permission = method == "item/permissions/requestApproval";
    let declined = if is_permission {
        json!({"permissions": {}, "scope": "turn", "strictAutoReview": true})
    } else {
        json!({"decision": "decline"})
    };
    if settings.execution_mode == CodexExecutionMode::ReviewSafe
        || !session_accepts_activity(sessions, session_id, turn_id)
        || approvals.len() >= MAX_PENDING_APPROVALS
    {
        return write_message(input, &json!({"id": id, "result": declined})).await;
    }

    let (options, responses) = if is_permission {
        permission_approval_options(params)
    } else {
        decision_approval_options(params)
    };
    if options.is_empty() {
        return write_message(input, &json!({"id": id, "result": declined})).await;
    }
    let request_id = id_key(&id);
    if approvals.contains_key(&request_id) {
        return write_message(input, &json!({"id": id, "result": declined})).await;
    }
    approvals.insert(
        request_id.clone(),
        PendingApproval {
            id,
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
            responses,
            declined,
        },
    );
    if events
        .send(CodexEvent::PermissionRequested {
            request_id: request_id.clone(),
            session_id: session_id.to_string(),
            tool_call: params.clone(),
            options: Value::Array(options),
        })
        .await
        .is_err()
    {
        if let Some(approval) = approvals.remove(&request_id) {
            write_message(
                input,
                &json!({"id": approval.id, "result": approval.declined}),
            )
            .await?;
        }
    }
    Ok(())
}

fn decision_approval_options(params: &Value) -> (Vec<Value>, HashMap<String, Value>) {
    let decisions = params["availableDecisions"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| {
            vec![
                json!("accept"),
                json!("acceptForSession"),
                json!("decline"),
                json!("cancel"),
            ]
        });
    let mut options = Vec::with_capacity(decisions.len().min(8));
    let mut responses = HashMap::new();
    for decision in decisions.into_iter().take(8) {
        let (option_id, label) = if let Some(value) = decision.as_str() {
            let label = match value {
                "accept" => "Allow once",
                "acceptForSession" => "Allow for this session",
                "decline" => "Decline",
                "cancel" => "Cancel the turn",
                _ => continue,
            };
            (value.to_string(), label)
        } else if let Some(object) = decision.as_object() {
            if object.contains_key("acceptWithExecpolicyAmendment") {
                (
                    "acceptWithExecpolicyAmendment".to_string(),
                    "Allow and save the proposed command rule",
                )
            } else if object.contains_key("applyNetworkPolicyAmendment") {
                (
                    "applyNetworkPolicyAmendment".to_string(),
                    "Apply the proposed network rule",
                )
            } else {
                continue;
            }
        } else {
            continue;
        };
        responses.insert(option_id.clone(), json!({"decision": decision}));
        options.push(json!({
            "option_id": option_id,
            "name": label,
            "kind": "approval",
        }));
    }
    (options, responses)
}

fn permission_approval_options(params: &Value) -> (Vec<Value>, HashMap<String, Value>) {
    let requested = params["permissions"].clone();
    if !requested.is_object() {
        return (Vec::new(), HashMap::new());
    }
    let choices = [
        (
            "accept",
            "Allow for this turn",
            json!({"permissions": requested, "scope": "turn"}),
        ),
        (
            "acceptForSession",
            "Allow for this session",
            json!({"permissions": requested, "scope": "session"}),
        ),
        (
            "decline",
            "Decline",
            json!({"permissions": {}, "scope": "turn", "strictAutoReview": true}),
        ),
    ];
    let mut options = Vec::with_capacity(choices.len());
    let mut responses = HashMap::with_capacity(choices.len());
    for (option_id, name, response) in choices {
        options.push(json!({
            "option_id": option_id,
            "name": name,
            "kind": "permission",
        }));
        responses.insert(option_id.to_string(), response);
    }
    (options, responses)
}

#[allow(clippy::too_many_arguments)]
async fn handle_response(
    message: Value,
    input: &mut (impl AsyncWrite + Unpin),
    events: &mpsc::Sender<CodexEvent>,
    pending: &mut HashMap<String, Pending>,
    sessions: &mut HashMap<String, Session>,
    next_id: &mut u64,
    settings: &WorkerSettings,
) -> Result<()> {
    let key = id_key(&message["id"]);
    let Some(request) = pending.remove(&key) else {
        return Ok(());
    };
    if let Some(error) = message.get("error") {
        if let Pending::Resume {
            cwd, recover: true, ..
        } = &request
        {
            return request_session_config(cwd.clone(), input, pending, next_id).await;
        }
        let session_id = match &request {
            Pending::Turn { session_id }
            | Pending::Interrupt { session_id, .. }
            | Pending::Steer { session_id }
            | Pending::Models { session_id }
            | Pending::Sessions { session_id }
            | Pending::Resume { session_id, .. } => Some(session_id.clone()),
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
            if settings.execution_mode == CodexExecutionMode::Native {
                let id = rpc_id(next_id);
                pending.insert(id.clone(), Pending::Start { cwd: cwd.clone() });
                let mut params = json!({
                    "cwd": cwd,
                    "dynamicTools": tool_definitions(),
                    "serviceName": "red",
                });
                if !settings.persistent_threads {
                    params["ephemeral"] = json!(true);
                }
                if let Some(model) = &settings.model {
                    params["model"] = json!(model);
                }
                write_message(
                    input,
                    &json!({"id": id, "method": "thread/start", "params": params}),
                )
                .await?;
            } else {
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
        }
        Pending::Requirements { cwd, mut config } => {
            let Some(hooks_enabled) = required_hooks_mode(&message) else {
                events
                    .send(CodexEvent::Failed {
                        session_id: None,
                        message: "Managed Codex requirements prevent a reviewable session"
                            .to_string(),
                    })
                    .await
                    .ok();
                return Ok(());
            };
            config["features"]["hooks"] = json!(hooks_enabled);
            let id = rpc_id(next_id);
            pending.insert(id.clone(), Pending::Start { cwd: cwd.clone() });
            let mut params = json!({
                "cwd": cwd,
                "ephemeral": !settings.persistent_threads,
                "approvalPolicy": "never",
                "sandbox": "read-only",
                "environments": [],
                "config": config,
                "dynamicTools": tool_definitions(),
                "baseInstructions": REVIEW_SAFE_INSTRUCTIONS,
                "serviceName": "red",
            });
            if let Some(model) = &settings.model {
                params["model"] = json!(model);
            }
            write_message(
                input,
                &json!({"id": id, "method": "thread/start", "params": params}),
            )
            .await?;
        }
        Pending::Start { cwd } => {
            let model = message
                .pointer("/result/model")
                .and_then(Value::as_str)
                .or_else(|| {
                    message
                        .pointer("/result/thread/model")
                        .and_then(Value::as_str)
                })
                .map(str::to_string);
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
                        model,
                        reasoning_effort: settings.reasoning_effort.clone(),
                        close_requested: false,
                    },
                );
                events
                    .send(CodexEvent::SessionCreated { session_id })
                    .await
                    .ok();
            }
        }
        Pending::Resume {
            cwd,
            session_id,
            recover,
        } => {
            let returned_id = message
                .pointer("/result/thread/id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if returned_id != session_id {
                if recover {
                    return request_session_config(cwd, input, pending, next_id).await;
                }
                let _ = events
                    .send(CodexEvent::Failed {
                        session_id: Some(session_id),
                        message: "Codex returned an unexpected resumed thread".to_string(),
                    })
                    .await;
            } else {
                let model = message
                    .pointer("/result/model")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        message
                            .pointer("/result/thread/model")
                            .and_then(Value::as_str)
                    })
                    .map(str::to_string);
                sessions.insert(
                    session_id.clone(),
                    Session {
                        cwd,
                        active_turn: None,
                        cancelled: Arc::new(AtomicBool::new(false)),
                        tool_calls: 0,
                        model,
                        reasoning_effort: settings.reasoning_effort.clone(),
                        close_requested: false,
                    },
                );
                let _ = events.send(CodexEvent::SessionCreated { session_id }).await;
            }
        }
        Pending::Turn { session_id } => {
            let turn_id = message
                .pointer("/result/turn/id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if let Some(session) = sessions.get_mut(&session_id) {
                if turn_id.is_empty() {
                    let _ = events
                        .send(CodexEvent::Failed {
                            session_id: Some(session_id),
                            message: "Codex returned an invalid turn".to_string(),
                        })
                        .await;
                } else {
                    session.active_turn = Some(turn_id);
                }
            }
        }
        Pending::Interrupt {
            session_id,
            turn_id,
        } => {
            if sessions
                .get(&session_id)
                .is_some_and(|session| session.active_turn.as_deref() == Some(turn_id.as_str()))
            {
                let _ = events.send(CodexEvent::Cancelled { session_id }).await;
            }
        }
        Pending::Steer { session_id } => {
            let turn_id = message
                .pointer("/result/turnId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if sessions
                .get(&session_id)
                .is_some_and(|session| session.active_turn.as_deref() == Some(turn_id))
            {
                send_activity(
                    events,
                    &session_id,
                    json!({"session_update": "steer", "turn_id": turn_id}),
                )
                .await;
            }
        }
        Pending::Models { session_id } => {
            let models = message
                .pointer("/result/data")
                .filter(|data| data.is_array())
                .cloned()
                .unwrap_or_else(|| json!([]));
            send_activity(
                events,
                &session_id,
                json!({
                    "session_update": "models",
                    "models": models,
                    "next_cursor": message.pointer("/result/nextCursor"),
                }),
            )
            .await;
        }
        Pending::Sessions { session_id } => {
            let threads = message
                .pointer("/result/data")
                .filter(|data| data.is_array())
                .cloned()
                .unwrap_or_else(|| json!([]));
            send_activity(
                events,
                &session_id,
                json!({
                    "session_update": "sessions",
                    "sessions": threads,
                    "next_cursor": message.pointer("/result/nextCursor"),
                }),
            )
            .await;
        }
    }
    Ok(())
}

async fn handle_tool_call<H: CodexToolHost>(
    message: Value,
    input: &mut (impl AsyncWrite + Unpin),
    events: &mpsc::Sender<CodexEvent>,
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
    let title = dynamic_tool_activity_title(&tool, &arguments);
    send_activity(
        events,
        &session_id,
        json!({
            "session_update": "tool_call",
            "tool_call_id": id_key(&id),
            "title": title,
            "kind": tool,
            "status": "in_progress",
        }),
    )
    .await;
    let dispatched_tool = tool.clone();
    tokio::spawn(async move {
        let result = timeout(TOOL_TIMEOUT, async {
            match dispatched_tool.as_str() {
                "list_files" => {
                    let overlays = host.lock().await.overlay_files(&session_id).await?;
                    tokio::task::spawn_blocking(move || {
                        list_files_with_overlays(&cwd, &cancelled, &overlays)
                    })
                    .await
                    .context("list_files task failed")?
                }
                "search_files" => {
                    let query = required_string(&arguments, "query")?.to_string();
                    let overlays = host.lock().await.overlay_files(&session_id).await?;
                    tokio::task::spawn_blocking(move || {
                        search_files_with_overlays(&cwd, &query, &cancelled, &overlays)
                    })
                    .await
                    .context("search_files task failed")?
                }
                "read_file" => {
                    let path = required_string(&arguments, "path")?;
                    ensure_agent_path_disclosable(&cwd, Path::new(path))?;
                    host.lock().await.read_file(&session_id, path).await
                }
                "write_file" => {
                    let path = required_string(&arguments, "path")?;
                    ensure_agent_path_disclosable(&cwd, Path::new(path))?;
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
                tool,
                title,
                result,
            })
            .await;
    });
    Ok(())
}

fn dynamic_tool_activity_title(tool: &str, arguments: &Value) -> String {
    let path = arguments["path"].as_str().unwrap_or_default();
    match tool {
        "list_files" => "Listing workspace files".to_string(),
        "search_files" => arguments["query"]
            .as_str()
            .map(|query| {
                format!(
                    "Searching for {}",
                    query.chars().take(160).collect::<String>()
                )
            })
            .unwrap_or_else(|| "Searching workspace files".to_string()),
        "read_file" => format!("Reading {path}"),
        "write_file" => format!("Proposing changes to {path}"),
        "get_editor_state" | "open_file" | "select_text" | "apply_edits" | "run_editor_action" => {
            EditorToolCall::parse(tool, arguments.clone())
                .map_or_else(|_| format!("Using {tool}"), |call| call.activity_title())
        }
        _ => format!("Using {tool}"),
    }
    .chars()
    .take(240)
    .collect()
}

#[cfg(test)]
fn list_files(root: &Path, cancelled: &AtomicBool) -> Result<Value> {
    list_files_with_overlays(root, cancelled, &[])
}

fn list_files_with_overlays(
    root: &Path,
    cancelled: &AtomicBool,
    overlays: &[(String, String)],
) -> Result<Value> {
    Ok(json!({
        "files": file_paths_with_overlays(root, cancelled, overlays)?
    }))
}

fn file_paths_with_overlays(
    root: &Path,
    cancelled: &AtomicBool,
    overlays: &[(String, String)],
) -> Result<Vec<String>> {
    let mut files = list_file_paths(root, cancelled)?;
    for (relative, _) in overlays.iter().take(MAX_FILES) {
        if cancelled.load(Ordering::Relaxed) {
            anyhow::bail!("Codex turn was cancelled");
        }
        if ensure_agent_path_disclosable(root, Path::new(relative)).is_ok() {
            files.push(relative.clone());
        }
    }
    files.sort_unstable();
    files.dedup();
    files.truncate(MAX_FILES);
    Ok(files)
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
            if ensure_agent_path_disclosable(root, entry.path()).is_err() {
                continue;
            }
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

#[cfg(test)]
fn search_files(root: &Path, query: &str, cancelled: &AtomicBool) -> Result<Value> {
    search_files_with_overlays(root, query, cancelled, &[])
}

fn search_files_with_overlays(
    root: &Path,
    query: &str,
    cancelled: &AtomicBool,
    overlays: &[(String, String)],
) -> Result<Value> {
    anyhow::ensure!(
        !query.is_empty() && query.len() <= 1024,
        "invalid search query"
    );
    #[cfg(not(unix))]
    {
        let _ = (root, cancelled, overlays);
        anyhow::bail!("workspace content search is unavailable on this platform");
    }

    #[cfg(unix)]
    {
        let files = file_paths_with_overlays(root, cancelled, overlays)?;
        let overlays = overlays
            .iter()
            .filter(|(relative, content)| {
                content.len() <= TOOL_CONTENT_BYTES
                    && ensure_agent_path_disclosable(root, Path::new(relative)).is_ok()
            })
            .map(|(relative, content)| (relative.as_str(), content.as_str()))
            .collect::<HashMap<_, _>>();
        let mut matches = Vec::new();
        let mut searched = 0_u64;
        for relative in files {
            if cancelled.load(Ordering::Relaxed) {
                anyhow::bail!("Codex turn was cancelled");
            }
            let disk_content;
            let (content, bytes) = if let Some(content) = overlays.get(relative.as_str()) {
                (*content, content.len() as u64)
            } else {
                let Some((content, bytes)) = read_workspace_file(root, &relative)? else {
                    continue;
                };
                disk_content = content;
                (disk_content.as_str(), bytes)
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
            "hooks": false
        },
        "orchestrator": {"mcp": {"enabled": false}},
        "notify": []
    }))
}

/// Returns whether hooks must be enabled while rejecting other required
/// extension features that could escape Red's review boundary.
fn required_hooks_mode(response: &Value) -> Option<bool> {
    let Some(requirements) = response.pointer("/result/requirements") else {
        return Some(false);
    };
    if requirements.is_null() {
        return Some(false);
    }
    let features = response
        .pointer("/result/requirements/featureRequirements")
        .and_then(Value::as_object)?;
    if [
        "apps",
        "connectors",
        "plugins",
        "skill_mcp_dependency_install",
    ]
    .iter()
    .any(|name| features.get(*name).and_then(Value::as_bool) == Some(true))
    {
        return None;
    }

    Some(
        ["hooks", "codex_hooks"]
            .iter()
            .any(|name| features.get(*name).and_then(Value::as_bool) == Some(true)),
    )
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
    fn workspace_discovery_excludes_sensitive_and_ignored_files() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(root.path().join("visible.txt"), "visible\n").unwrap();
        std::fs::write(root.path().join("ignored.txt"), "ignored\n").unwrap();
        std::fs::write(root.path().join(".env"), "TOKEN=secret\n").unwrap();
        std::fs::write(root.path().join("private.key"), "private\n").unwrap();

        let result = list_files(root.path(), &AtomicBool::new(false)).unwrap();
        let files = result["files"].as_array().unwrap();

        assert!(files.iter().any(|path| path == "visible.txt"));
        for hidden in ["ignored.txt", ".env", "private.key"] {
            assert!(
                !files.iter().any(|path| path == hidden),
                "disclosed {hidden}"
            );
        }
    }

    #[test]
    fn discovery_includes_safe_visible_files_that_do_not_exist_on_disk() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".gitignore"), "ignored.rs\n").unwrap();
        let overlays = vec![
            ("draft.rs".to_string(), "unsaved draft\n".to_string()),
            ("ignored.rs".to_string(), "ignored draft\n".to_string()),
            (".env".to_string(), "TOKEN=secret\n".to_string()),
            ("../outside.rs".to_string(), "outside\n".to_string()),
        ];

        let result =
            list_files_with_overlays(root.path(), &AtomicBool::new(false), &overlays).unwrap();
        let files = result["files"].as_array().unwrap();

        assert!(files.iter().any(|path| path == "draft.rs"));
        for hidden in ["ignored.rs", ".env", "../outside.rs"] {
            assert!(
                !files.iter().any(|path| path == hidden),
                "disclosed {hidden}"
            );
        }
    }

    #[test]
    fn search_uses_visible_and_proposed_contents_instead_of_stale_disk() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("main.rs"), "old disk needle\n").unwrap();
        let overlays = vec![
            (
                "main.rs".to_string(),
                "visible unsaved marker\n".to_string(),
            ),
            ("draft.rs".to_string(), "proposed marker\n".to_string()),
        ];

        let result =
            search_files_with_overlays(root.path(), "marker", &AtomicBool::new(false), &overlays)
                .unwrap();
        let matches = result["matches"].as_array().unwrap();

        assert_eq!(matches.len(), 2);
        assert!(matches
            .iter()
            .any(|entry| entry["path"] == "main.rs" && entry["text"] == "visible unsaved marker"));
        assert!(matches
            .iter()
            .any(|entry| entry["path"] == "draft.rs" && entry["text"] == "proposed marker"));

        let stale = search_files_with_overlays(
            root.path(),
            "old disk needle",
            &AtomicBool::new(false),
            &overlays,
        )
        .unwrap();
        assert!(stale["matches"].as_array().unwrap().is_empty());
    }

    #[test]
    fn search_never_discloses_sensitive_or_ignored_overlay_contents() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".gitignore"), "ignored.rs\n").unwrap();
        let overlays = vec![
            (".env".to_string(), "hidden marker\n".to_string()),
            ("ignored.rs".to_string(), "hidden marker\n".to_string()),
            ("../outside.rs".to_string(), "hidden marker\n".to_string()),
        ];

        let result =
            search_files_with_overlays(root.path(), "marker", &AtomicBool::new(false), &overlays)
                .unwrap();

        assert!(result["matches"].as_array().unwrap().is_empty());
    }

    #[test]
    fn cancelled_overlay_discovery_and_search_stop_immediately() {
        let root = tempfile::tempdir().unwrap();
        let overlays = vec![("draft.rs".to_string(), "marker\n".to_string())];
        let cancelled = AtomicBool::new(true);

        assert!(list_files_with_overlays(root.path(), &cancelled, &overlays)
            .unwrap_err()
            .to_string()
            .contains("cancelled"));
        assert!(
            search_files_with_overlays(root.path(), "marker", &cancelled, &overlays)
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
    }

    #[test]
    fn approval_options_preserve_only_explicitly_supported_server_decisions() {
        let (options, responses) = decision_approval_options(&json!({
            "availableDecisions": ["accept", "decline", "unsupported"]
        }));

        assert_eq!(options.len(), 2);
        assert_eq!(responses["accept"], json!({"decision": "accept"}));
        assert_eq!(responses["decline"], json!({"decision": "decline"}));
        assert!(!responses.contains_key("unsupported"));
    }

    #[test]
    fn permission_approval_options_keep_turn_and_session_scopes_explicit() {
        let requested = json!({"network": {"enabled": true}});
        let (options, responses) = permission_approval_options(&json!({"permissions": requested}));

        assert_eq!(options.len(), 3);
        assert_eq!(
            responses["accept"],
            json!({"permissions": {"network": {"enabled": true}}, "scope": "turn"})
        );
        assert_eq!(
            responses["acceptForSession"],
            json!({"permissions": {"network": {"enabled": true}}, "scope": "session"})
        );
        assert_eq!(
            responses["decline"],
            json!({"permissions": {}, "scope": "turn", "strictAutoReview": true})
        );
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

    #[test]
    fn required_hooks_mode_allows_managed_only_hooks() {
        for feature in ["hooks", "codex_hooks"] {
            let response = json!({
                "result": {
                    "requirements": {
                        "allowManagedHooksOnly": true,
                        "featureRequirements": {(feature): true}
                    }
                }
            });

            assert_eq!(required_hooks_mode(&response), Some(true));
        }
    }

    #[test]
    fn required_hooks_mode_allows_hooks_without_managed_only_enforcement() {
        for managed_only in [Value::Null, json!(false)] {
            let response = json!({
                "result": {
                    "requirements": {
                        "allowManagedHooksOnly": managed_only,
                        "featureRequirements": {"hooks": true}
                    }
                }
            });

            assert_eq!(required_hooks_mode(&response), Some(true));
        }
    }

    #[test]
    fn required_hooks_mode_rejects_other_required_extensions() {
        for feature in [
            "apps",
            "connectors",
            "plugins",
            "skill_mcp_dependency_install",
        ] {
            let response = json!({
                "result": {
                    "requirements": {
                        "allowManagedHooksOnly": true,
                        "featureRequirements": {(feature): true}
                    }
                }
            });

            assert_eq!(required_hooks_mode(&response), None);
        }
    }

    #[test]
    fn required_hooks_mode_disables_hooks_without_requirements() {
        for response in [
            json!({"result": {"requirements": null}}),
            json!({"result": {}}),
        ] {
            assert_eq!(required_hooks_mode(&response), Some(false));
        }
    }
}
