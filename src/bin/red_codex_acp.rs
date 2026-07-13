//! Reviewable ACP adapter backed by an installed Codex app-server.
//!
//! Codex runs without an execution environment, so it cannot expose its native shell or
//! patch tools. Bounded dynamic tools provide workspace discovery and route every file
//! read and write through the ACP client. Writes therefore remain Red proposals.

use std::{
    collections::HashMap,
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::{Context, Result};
use clap::Parser;
use ignore::WalkBuilder;
use serde_json::{json, Value};
use tokio::{
    io::{
        AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
        BufWriter,
    },
    process::Command,
    sync::mpsc,
    time::{timeout, Instant},
};

const MAX_FRAME_BYTES: usize = 1024 * 1024;
const MAX_TOOL_CONTENT_BYTES: usize = 960 * 1024;
const MAX_SESSIONS: usize = 64;
const MAX_PENDING: usize = 64;
const MAX_TOOL_CALLS: usize = 32;
const MAX_FILES: usize = 4_096;
const MAX_SEARCH_RESULTS: usize = 200;
const MAX_SEARCH_BYTES: u64 = 32 * 1024 * 1024;
const MAX_WALK_ENTRIES: usize = 65_536;
const MAX_WALK_TIME: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const SETUP_TIMEOUT: Duration = Duration::from_secs(25);
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const INSTRUCTIONS: &str = "You are Red's coding assistant. You have no shell or native patch tool. Use list_files and search_files to locate relevant code, always use read_file before reasoning about a file, and use write_file for every edit. Writes are reviewable editor proposals and never touch disk. Do not claim a change was saved. Keep responses concise.";

#[derive(Debug, Parser)]
#[command(
    name = "red_codex_acp",
    version,
    about = "Red's reviewable Codex ACP adapter"
)]
struct Args {
    /// Installed Codex executable to run as an app-server.
    #[arg(long, env = "RED_CODEX_COMMAND", default_value = "codex")]
    codex: String,
}

#[derive(Debug)]
struct Session {
    cwd: PathBuf,
    cancelled: Arc<AtomicBool>,
    prompt_id: Option<Value>,
    turn_id: Option<String>,
    tool_calls: usize,
}

#[derive(Debug)]
enum Pending {
    Account {
        outer_id: Option<Value>,
        cwd: Option<PathBuf>,
        deadline: Instant,
    },
    Start {
        outer_id: Option<Value>,
        cwd: PathBuf,
        deadline: Instant,
    },
    TurnStart {
        session_id: String,
        closed: bool,
    },
}

#[derive(Debug)]
struct Callback {
    app_id: Value,
    session_id: String,
    turn_id: String,
    method: &'static str,
}

#[derive(Debug)]
enum Event {
    Acp(Value),
    App(Value),
    ToolResult {
        app_id: Value,
        session_id: String,
        turn_id: String,
        result: std::result::Result<Value, String>,
    },
    SetupTimeout(String),
    CallbackTimeout(String),
    AcpClosed,
    AppClosed,
    InvalidAcp,
    InvalidApp,
}

struct Adapter {
    acp_out: mpsc::Sender<Value>,
    app_out: mpsc::Sender<Value>,
    events: mpsc::Sender<Event>,
    next_id: AtomicU64,
    sessions: HashMap<String, Session>,
    pending: HashMap<String, Pending>,
    callbacks: HashMap<String, Callback>,
    can_read: bool,
    can_write: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    let codex = red::agent_check::find_executable_on_path(&args.codex)
        .unwrap_or_else(|| PathBuf::from(&args.codex));
    let mut child = Command::new(&codex)
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // App-server diagnostics can contain local context. The bridge emits only
        // content-free lifecycle diagnostics of its own.
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("failed to start Codex executable {:?}", args.codex))?;
    let child_stdin = child
        .stdin
        .take()
        .context("Codex app-server stdin is unavailable")?;
    let child_stdout = child
        .stdout
        .take()
        .context("Codex app-server stdout is unavailable")?;
    let mut app_stdin = BufWriter::new(child_stdin);
    let mut app_stdout = BufReader::new(child_stdout);

    write_message(
        &mut app_stdin,
        &json!({
            "id": "red-codex-initialize",
            "method": "initialize",
            "params": {
                "clientInfo": {"name": "red_codex_acp", "title": "Red Codex ACP", "version": env!("CARGO_PKG_VERSION")},
                "capabilities": {"experimentalApi": true}
            }
        }),
    )
    .await?;
    let initialized = timeout(HANDSHAKE_TIMEOUT, read_bounded_line(&mut app_stdout))
        .await
        .context("Codex app-server initialization timed out")??
        .context("Codex app-server closed during initialization")?;
    let initialized: Value = serde_json::from_slice(&initialized)
        .context("Codex app-server returned an invalid initialization response")?;
    anyhow::ensure!(
        initialized.get("id").and_then(Value::as_str) == Some("red-codex-initialize")
            && initialized.get("result").is_some()
            && initialized.get("error").is_none(),
        "Codex app-server does not support the required experimental API"
    );
    write_message(
        &mut app_stdin,
        &json!({"method": "initialized", "params": {}}),
    )
    .await?;

    let (events, mut event_rx) = mpsc::channel(128);
    let (acp_out, acp_rx) = mpsc::channel(64);
    let (app_out, app_rx) = mpsc::channel(64);
    let acp_writer = tokio::spawn(writer_task(BufWriter::new(tokio::io::stdout()), acp_rx));
    let app_writer = tokio::spawn(writer_task(app_stdin, app_rx));
    spawn_reader(BufReader::new(tokio::io::stdin()), events.clone(), true);
    spawn_reader(app_stdout, events.clone(), false);

    let mut adapter = Adapter {
        acp_out,
        app_out,
        events: events.clone(),
        next_id: AtomicU64::new(1),
        sessions: HashMap::new(),
        pending: HashMap::new(),
        callbacks: HashMap::new(),
        can_read: false,
        can_write: false,
    };
    while let Some(event) = event_rx.recv().await {
        match event {
            Event::Acp(message) => adapter.handle_acp(message).await?,
            Event::App(message) => adapter.handle_app(message).await?,
            Event::ToolResult {
                app_id,
                session_id,
                turn_id,
                result,
            } => {
                adapter
                    .send_workspace_result(app_id, &session_id, &turn_id, result)
                    .await?
            }
            Event::SetupTimeout(id) => adapter.setup_timeout(&id).await?,
            Event::CallbackTimeout(id) => adapter.callback_timeout(&id).await?,
            Event::InvalidAcp => eprintln!("event=codex_acp_invalid_json level=warn source=client"),
            Event::InvalidApp => {
                eprintln!("event=codex_acp_invalid_json level=error source=app_server");
                adapter
                    .fail_active_prompts("Codex app-server returned invalid data")
                    .await?;
                break;
            }
            Event::AcpClosed => {
                adapter.cancel_active_turns();
                break;
            }
            Event::AppClosed => {
                eprintln!("event=codex_acp_transport_closed level=error source=app_server");
                adapter
                    .fail_active_prompts("Codex app-server stopped")
                    .await?;
                break;
            }
        }
    }

    drop(adapter);
    drop(events);
    let _ = child.start_kill();
    let _ = timeout(SHUTDOWN_TIMEOUT, child.wait()).await;
    acp_writer.await.context("ACP writer task failed")??;
    app_writer.await.context("Codex writer task failed")??;
    Ok(())
}

fn spawn_reader<R>(mut reader: R, events: mpsc::Sender<Event>, acp: bool)
where
    R: AsyncBufRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            match read_bounded_line(&mut reader).await {
                Ok(Some(line)) => match serde_json::from_slice(&line) {
                    Ok(message) => {
                        if events
                            .send(if acp {
                                Event::Acp(message)
                            } else {
                                Event::App(message)
                            })
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = events
                            .send(if acp {
                                Event::InvalidAcp
                            } else {
                                Event::InvalidApp
                            })
                            .await;
                        if !acp {
                            break;
                        }
                    }
                },
                Ok(None) => {
                    let _ = events
                        .send(if acp {
                            Event::AcpClosed
                        } else {
                            Event::AppClosed
                        })
                        .await;
                    break;
                }
                Err(_) => {
                    let _ = events
                        .send(if acp {
                            Event::InvalidAcp
                        } else {
                            Event::InvalidApp
                        })
                        .await;
                    break;
                }
            }
        }
    });
}

async fn writer_task<W>(mut writer: W, mut messages: mpsc::Receiver<Value>) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    while let Some(message) = messages.recv().await {
        write_message(&mut writer, &message).await?;
    }
    Ok(())
}

impl Adapter {
    async fn handle_acp(&mut self, message: Value) -> Result<()> {
        let id = message.get("id").cloned();
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            self.complete_callback(message).await?;
            return Ok(());
        };
        match method {
            "initialize" => {
                let fs = message
                    .get("params")
                    .and_then(|params| params.get("clientCapabilities"))
                    .and_then(|capabilities| capabilities.get("fs"));
                self.can_read = fs
                    .and_then(|fs| fs.get("readTextFile"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.can_write = fs
                    .and_then(|fs| fs.get("writeTextFile"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.send_acp_result(
                    id,
                    json!({
                        "protocolVersion": 1,
                        "agentCapabilities": {
                            "loadSession": false,
                            "promptCapabilities": {"image": false, "audio": false, "embeddedContext": false},
                            "mcpCapabilities": {"http": false, "sse": false},
                            "sessionCapabilities": {"close": {}}
                        },
                        "authMethods": [{
                            "id": "codex_login",
                            "name": "Codex login",
                            "description": "Authenticate the installed Codex CLI with `codex login` before starting a session."
                        }],
                        "agentInfo": {"name": "red-codex-acp", "version": env!("CARGO_PKG_VERSION")}
                    }),
                )
                .await?;
            }
            "authenticate" => self.check_account(id, None).await?,
            "session/new" => {
                let pending_sessions = self
                    .pending
                    .values()
                    .filter(|pending| {
                        matches!(
                            pending,
                            Pending::Account { cwd: Some(_), .. } | Pending::Start { .. }
                        )
                    })
                    .count();
                if self.sessions.len().saturating_add(pending_sessions) >= MAX_SESSIONS {
                    self.send_acp_error(id, -32_000, "Codex session capacity reached")
                        .await?;
                    return Ok(());
                }
                if !self.can_read || !self.can_write {
                    self.send_acp_error(
                        id,
                        -32_000,
                        "Red filesystem callbacks are required for reviewable Codex sessions",
                    )
                    .await?;
                    return Ok(());
                }
                let cwd = message
                    .get("params")
                    .and_then(|params| params.get("cwd"))
                    .and_then(Value::as_str)
                    .map(PathBuf::from);
                let Some(cwd) = cwd else {
                    self.send_acp_error(id, -32_602, "Codex session requires a workspace root")
                        .await?;
                    return Ok(());
                };
                let cwd = match validate_workspace_root(&cwd) {
                    Ok(cwd) => cwd,
                    Err(_) => {
                        self.send_acp_error(id, -32_602, "Codex workspace root is invalid")
                            .await?;
                        return Ok(());
                    }
                };
                self.check_account(id, Some(cwd)).await?;
            }
            "session/prompt" => {
                let Some(params) = message.get("params") else {
                    self.send_acp_error(id, -32_602, "Codex prompt parameters are missing")
                        .await?;
                    return Ok(());
                };
                let session_id = params
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let text = prompt_text(params.get("prompt"));
                if text.trim().is_empty() || text.len() > MAX_TOOL_CONTENT_BYTES {
                    self.send_acp_error(
                        id,
                        -32_602,
                        "Codex prompt is empty or exceeds the size limit",
                    )
                    .await?;
                    return Ok(());
                }
                let Some(session) = self.sessions.get_mut(&session_id) else {
                    self.send_acp_error(id, -32_602, "Codex session was not found")
                        .await?;
                    return Ok(());
                };
                if session.prompt_id.is_some() {
                    self.send_acp_error(id, -32_000, "a Codex turn is already active")
                        .await?;
                    return Ok(());
                }
                session.prompt_id = id;
                session.turn_id = None;
                session.tool_calls = 0;
                session.cancelled = Arc::new(AtomicBool::new(false));
                let app_id = self.next_app_id();
                self.pending.insert(
                    id_key(&app_id),
                    Pending::TurnStart {
                        session_id: session_id.clone(),
                        closed: false,
                    },
                );
                self.send_app(json!({
                    "id": app_id,
                    "method": "turn/start",
                    "params": {
                        "threadId": session_id,
                        "input": [{"type": "text", "text": text}],
                        "approvalPolicy": "never",
                        "sandboxPolicy": {"type": "readOnly", "access": {"type": "fullAccess"}},
                        "environments": []
                    }
                }))
                .await?;
            }
            "session/cancel" => {
                let session_id = message
                    .get("params")
                    .and_then(|params| params.get("sessionId"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.cancel_session(&session_id).await?;
            }
            "session/close" => {
                let Some(session_id) = message
                    .get("params")
                    .and_then(|params| params.get("sessionId"))
                    .and_then(Value::as_str)
                else {
                    self.send_acp_error(id, -32_602, "Codex session close requires a session id")
                        .await?;
                    return Ok(());
                };
                let session_id = session_id.to_string();
                self.cancel_session(&session_id).await?;
                for pending in self.pending.values_mut() {
                    if let Pending::TurnStart {
                        session_id: pending_session,
                        closed,
                    } = pending
                    {
                        if pending_session == &session_id {
                            *closed = true;
                        }
                    }
                }
                let mut session = self.sessions.remove(&session_id);
                let prompt_id = session
                    .as_mut()
                    .and_then(|session| session.prompt_id.take());
                if session.is_some() {
                    self.unsubscribe_thread(&session_id).await?;
                    self.archive_thread(&session_id).await?;
                }
                self.send_acp_result(prompt_id, json!({"stopReason": "cancelled"}))
                    .await?;
                self.send_acp_result(id, json!({})).await?;
            }
            _ => {
                self.send_acp_error(id, -32_601, "unsupported ACP method")
                    .await?
            }
        }
        Ok(())
    }

    async fn check_account(&mut self, outer_id: Option<Value>, cwd: Option<PathBuf>) -> Result<()> {
        if self.pending.len() >= MAX_PENDING {
            self.send_acp_error(outer_id, -32_000, "Codex request capacity reached")
                .await?;
            return Ok(());
        }
        let app_id = self.next_app_id();
        let key = id_key(&app_id);
        let deadline = Instant::now() + SETUP_TIMEOUT;
        self.pending.insert(
            key.clone(),
            Pending::Account {
                outer_id,
                cwd,
                deadline,
            },
        );
        self.spawn_setup_timeout(key, deadline);
        self.send_app(json!({
            "id": app_id,
            "method": "account/read",
            "params": {"refreshToken": true}
        }))
        .await
    }

    async fn handle_app(&mut self, message: Value) -> Result<()> {
        if message.get("method").is_none() {
            self.complete_app_request(message).await?;
            return Ok(());
        }
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match method {
            "item/tool/call" => self.handle_dynamic_tool(message).await?,
            "item/agentMessage/delta" => {
                let params = message.get("params").unwrap_or(&Value::Null);
                let session_id = params
                    .get("threadId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let text = params
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let turn_id = params
                    .get("turnId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let active = self.sessions.get(session_id).is_some_and(|session| {
                    session.prompt_id.is_some()
                        && session.turn_id.as_deref() == Some(turn_id)
                        && !session.cancelled.load(Ordering::Relaxed)
                });
                if active && !text.is_empty() {
                    self.send_update(session_id, text).await?;
                }
            }
            "turn/completed" => {
                let params = message.get("params").unwrap_or(&Value::Null);
                let session_id = params
                    .get("threadId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let status = params
                    .get("turn")
                    .and_then(|turn| turn.get("status"))
                    .and_then(Value::as_str)
                    .unwrap_or("completed");
                let turn_id = params
                    .get("turn")
                    .and_then(|turn| turn.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                self.complete_turn(&session_id, turn_id, status).await?;
            }
            "item/fileChange/requestApproval" | "item/commandExecution/requestApproval" => {
                // These methods should be unreachable with an empty environment. Never
                // allow a native Codex mutation if a future server exposes them anyway.
                if let Some(id) = message.get("id").cloned() {
                    self.send_app(json!({"id": id, "result": {"decision": "decline"}}))
                        .await?;
                }
            }
            "item/permissions/requestApproval" => {
                if let Some(id) = message.get("id").cloned() {
                    self.send_app(json!({
                        "id": id,
                        "result": {"permissions": {}, "scope": "turn", "strictAutoReview": true}
                    }))
                    .await?;
                }
            }
            _ if message.get("id").is_some() => {
                self.send_app(json!({
                    "id": message.get("id").cloned(),
                    "error": {"code": -32601, "message": "unsupported Codex server request"}
                }))
                .await?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn complete_app_request(&mut self, message: Value) -> Result<()> {
        let Some(id) = message.get("id") else {
            return Ok(());
        };
        let Some(pending) = self.pending.remove(&id_key(id)) else {
            if let Some(session_id) = message
                .get("result")
                .and_then(|result| result.get("thread"))
                .and_then(|thread| thread.get("id"))
                .and_then(Value::as_str)
            {
                self.unsubscribe_thread(session_id).await?;
            }
            return Ok(());
        };
        let errored = message.get("error").is_some();
        match pending {
            Pending::Account {
                outer_id,
                cwd,
                deadline,
            } => {
                if Instant::now() >= deadline {
                    self.send_acp_error(outer_id, -32_000, "Codex session setup timed out")
                        .await?;
                    return Ok(());
                }
                let result = message.get("result").unwrap_or(&Value::Null);
                let needs_auth = result
                    .get("requiresOpenaiAuth")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
                    && result.get("account").is_none_or(Value::is_null);
                if errored || needs_auth {
                    self.send_acp_error(
                        outer_id,
                        -32_001,
                        "Codex is not authenticated; run `codex login` and try again",
                    )
                    .await?;
                    return Ok(());
                }
                if let Some(cwd) = cwd {
                    self.start_session(outer_id, cwd, deadline).await?;
                } else {
                    self.send_acp_result(outer_id, json!({})).await?;
                }
            }
            Pending::Start {
                outer_id,
                cwd,
                deadline,
            } => {
                let session_id = message
                    .get("result")
                    .and_then(|result| result.get("thread"))
                    .and_then(|thread| thread.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if Instant::now() >= deadline {
                    self.send_acp_error(outer_id, -32_000, "Codex session setup timed out")
                        .await?;
                    if !session_id.is_empty() {
                        self.unsubscribe_thread(&session_id).await?;
                    }
                    return Ok(());
                }
                if errored || session_id.is_empty() {
                    self.send_acp_error(
                        outer_id,
                        -32_000,
                        "Codex could not start a reviewable session; the installed version may be incompatible",
                    )
                    .await?;
                    return Ok(());
                }
                if !self.sessions.contains_key(&session_id) && self.sessions.len() >= MAX_SESSIONS {
                    self.send_acp_error(outer_id, -32_000, "Codex session capacity reached")
                        .await?;
                    return Ok(());
                }
                self.sessions.insert(
                    session_id.clone(),
                    Session {
                        cwd,
                        cancelled: Arc::new(AtomicBool::new(false)),
                        prompt_id: None,
                        turn_id: None,
                        tool_calls: 0,
                    },
                );
                self.send_acp_result(outer_id, json!({"sessionId": session_id}))
                    .await?;
            }
            Pending::TurnStart { session_id, closed } => {
                if errored {
                    let prompt_id = self
                        .sessions
                        .get_mut(&session_id)
                        .and_then(|session| session.prompt_id.take());
                    self.send_acp_error(
                        prompt_id,
                        -32_000,
                        "Codex could not start the requested turn",
                    )
                    .await?;
                    return Ok(());
                }
                let turn_id = message
                    .get("result")
                    .and_then(|result| result.get("turn"))
                    .and_then(|turn| turn.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if closed {
                    if !turn_id.is_empty() {
                        self.interrupt_turn(&session_id, &turn_id).await?;
                    }
                    return Ok(());
                }
                let mut interrupt = None;
                if let Some(session) = self.sessions.get_mut(&session_id) {
                    if turn_id.is_empty() {
                        let prompt_id = session.prompt_id.take();
                        self.send_acp_error(
                            prompt_id,
                            -32_000,
                            "Codex returned an invalid turn identifier",
                        )
                        .await?;
                        return Ok(());
                    }
                    session.turn_id = Some(turn_id.clone());
                    if session.cancelled.load(Ordering::Relaxed) {
                        interrupt = Some(turn_id);
                    }
                }
                if let Some(turn_id) = interrupt {
                    self.interrupt_turn(&session_id, &turn_id).await?;
                }
            }
        }
        Ok(())
    }

    async fn start_session(
        &mut self,
        outer_id: Option<Value>,
        cwd: PathBuf,
        deadline: Instant,
    ) -> Result<()> {
        if self.pending.len() >= MAX_PENDING {
            self.send_acp_error(outer_id, -32_000, "Codex request capacity reached")
                .await?;
            return Ok(());
        }
        let app_id = self.next_app_id();
        let key = id_key(&app_id);
        self.pending.insert(
            key.clone(),
            Pending::Start {
                outer_id,
                cwd: cwd.clone(),
                deadline,
            },
        );
        self.spawn_setup_timeout(key, deadline);
        self.send_app(json!({
            "id": app_id,
            "method": "thread/start",
            "params": {
                "cwd": cwd,
                "approvalPolicy": "never",
                "sandbox": "read-only",
                "environments": [],
                "dynamicTools": tool_definitions(),
                "baseInstructions": INSTRUCTIONS,
                "serviceName": "red_codex_acp"
            }
        }))
        .await
    }

    fn spawn_setup_timeout(&self, id: String, deadline: Instant) {
        let events = self.events.clone();
        tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            let _ = events.send(Event::SetupTimeout(id)).await;
        });
    }

    async fn handle_dynamic_tool(&mut self, message: Value) -> Result<()> {
        let Some(app_id) = message.get("id").cloned() else {
            return Ok(());
        };
        let params = message.get("params").unwrap_or(&Value::Null);
        let session_id = params
            .get("threadId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let turn_id = params
            .get("turnId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let tool = params
            .get("tool")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
        if serde_json::to_vec(&arguments)?.len() > MAX_TOOL_CONTENT_BYTES {
            self.send_dynamic_result(
                app_id,
                Err("Codex tool arguments exceed the size limit".to_string()),
            )
            .await?;
            return Ok(());
        }
        let Some(session) = self.sessions.get_mut(&session_id) else {
            self.send_dynamic_result(
                app_id,
                Err("Codex tool references an unknown session".to_string()),
            )
            .await?;
            return Ok(());
        };
        if session.prompt_id.is_none() || session.turn_id.as_deref() != Some(turn_id) {
            self.send_dynamic_result(
                app_id,
                Err("Codex tool references an inactive turn".to_string()),
            )
            .await?;
            return Ok(());
        }
        if session.cancelled.load(Ordering::Relaxed) {
            self.send_dynamic_result(app_id, Err("Codex turn was cancelled".to_string()))
                .await?;
            return Ok(());
        }
        session.tool_calls = session.tool_calls.saturating_add(1);
        if session.tool_calls > MAX_TOOL_CALLS {
            self.send_dynamic_result(app_id, Err("Codex tool-call limit reached".to_string()))
                .await?;
            return Ok(());
        }
        let cwd = session.cwd.clone();
        let cancelled = Arc::clone(&session.cancelled);
        match tool {
            "list_files" => {
                if validate_arguments(&arguments, &[]).is_err() {
                    self.send_dynamic_result(
                        app_id,
                        Err("list_files received invalid arguments".to_string()),
                    )
                    .await?;
                    return Ok(());
                }
                self.spawn_workspace_tool(
                    app_id,
                    session_id.clone(),
                    turn_id.to_string(),
                    move || list_files(&cwd, &cancelled).map(|files| json!({"files": files})),
                );
            }
            "search_files" => {
                let query = validate_arguments(&arguments, &["query"])
                    .and_then(|_| required_string(&arguments, "query").map(str::to_string));
                let Ok(query) = query else {
                    self.send_dynamic_result(
                        app_id,
                        Err("search_files received invalid arguments".to_string()),
                    )
                    .await?;
                    return Ok(());
                };
                if query.is_empty() || query.len() > 1024 {
                    self.send_dynamic_result(
                        app_id,
                        Err("search_files query is empty or too large".to_string()),
                    )
                    .await?;
                    return Ok(());
                }
                self.spawn_workspace_tool(
                    app_id,
                    session_id.clone(),
                    turn_id.to_string(),
                    move || {
                        search_files(&cwd, &query, &cancelled)
                            .map(|matches| json!({"matches": matches}))
                    },
                );
            }
            "read_file" | "write_file" => {
                let required = if tool == "read_file" {
                    &["path"][..]
                } else {
                    &["path", "content"][..]
                };
                if validate_arguments(&arguments, required).is_err() {
                    self.send_dynamic_result(
                        app_id,
                        Err(format!("{tool} received invalid arguments")),
                    )
                    .await?;
                    return Ok(());
                }
                let path = required_string(&arguments, "path")
                    .and_then(|path| resolve_workspace_path(&cwd, path));
                let Ok(path) = path else {
                    self.send_dynamic_result(
                        app_id,
                        Err("Codex tool path is outside the workspace or unsafe".to_string()),
                    )
                    .await?;
                    return Ok(());
                };
                if tool == "write_file"
                    && required_string(&arguments, "content")
                        .map_or(true, |content| content.len() > MAX_TOOL_CONTENT_BYTES)
                {
                    self.send_dynamic_result(
                        app_id,
                        Err("write_file content exceeds the size limit".to_string()),
                    )
                    .await?;
                    return Ok(());
                }
                if self.callbacks.len() >= MAX_PENDING {
                    self.send_dynamic_result(
                        app_id,
                        Err("ACP filesystem callback capacity reached".to_string()),
                    )
                    .await?;
                    return Ok(());
                }
                let callback_id = format!(
                    "red-codex-fs-{}",
                    self.next_id.fetch_add(1, Ordering::Relaxed)
                );
                let method = if tool == "read_file" {
                    "fs/read_text_file"
                } else {
                    "fs/write_text_file"
                };
                let mut params = json!({"sessionId": session_id, "path": path});
                if tool == "write_file" {
                    params["content"] = arguments["content"].clone();
                }
                let key = id_key(&Value::String(callback_id.clone()));
                self.callbacks.insert(
                    key.clone(),
                    Callback {
                        app_id,
                        session_id,
                        turn_id: turn_id.to_string(),
                        method,
                    },
                );
                self.send_acp(json!({
                    "jsonrpc": "2.0",
                    "id": callback_id,
                    "method": method,
                    "params": params
                }))
                .await?;
                let events = self.events.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(CALLBACK_TIMEOUT).await;
                    let _ = events.send(Event::CallbackTimeout(key)).await;
                });
            }
            _ => {
                self.send_dynamic_result(app_id, Err("unsupported Codex dynamic tool".to_string()))
                    .await?;
            }
        }
        Ok(())
    }

    fn spawn_workspace_tool<F>(&self, app_id: Value, session_id: String, turn_id: String, work: F)
    where
        F: FnOnce() -> Result<Value> + Send + 'static,
    {
        let events = self.events.clone();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(work)
                .await
                .map_err(|_| "Codex workspace tool failed".to_string())
                .and_then(|result| result.map_err(|_| "Codex workspace tool failed".to_string()));
            let _ = events
                .send(Event::ToolResult {
                    app_id,
                    session_id,
                    turn_id,
                    result,
                })
                .await;
        });
    }

    async fn complete_callback(&mut self, message: Value) -> Result<()> {
        let Some(id) = message.get("id") else {
            return Ok(());
        };
        let Some(callback) = self.callbacks.remove(&id_key(id)) else {
            return Ok(());
        };
        let Some(session) = self.sessions.get(&callback.session_id) else {
            return self
                .send_dynamic_result(
                    callback.app_id,
                    Err("Codex tool references an unknown session".to_string()),
                )
                .await;
        };
        if session.prompt_id.is_none()
            || session.turn_id.as_deref() != Some(callback.turn_id.as_str())
        {
            return self
                .send_dynamic_result(
                    callback.app_id,
                    Err("Codex tool references an inactive turn".to_string()),
                )
                .await;
        }
        if session.cancelled.load(Ordering::Relaxed) {
            return self
                .send_dynamic_result(callback.app_id, Err("Codex turn was cancelled".to_string()))
                .await;
        }
        if message.get("error").is_some() {
            self.send_dynamic_result(
                callback.app_id,
                Err("ACP client rejected the filesystem request".to_string()),
            )
            .await?;
            return Ok(());
        }
        let result = message.get("result").cloned().unwrap_or_else(|| json!({}));
        if callback.method == "fs/read_text_file" {
            let content = result
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if content.len() > MAX_TOOL_CONTENT_BYTES {
                self.send_dynamic_result(
                    callback.app_id,
                    Err("ACP file content exceeds the size limit".to_string()),
                )
                .await?;
                return Ok(());
            }
        }
        self.send_dynamic_result(callback.app_id, Ok(result)).await
    }

    async fn callback_timeout(&mut self, id: &str) -> Result<()> {
        if let Some(callback) = self.callbacks.remove(id) {
            self.send_dynamic_result(
                callback.app_id,
                Err("ACP filesystem request timed out".to_string()),
            )
            .await?;
        }
        Ok(())
    }

    async fn setup_timeout(&mut self, id: &str) -> Result<()> {
        let Some(pending) = self.pending.remove(id) else {
            return Ok(());
        };
        let outer_id = match pending {
            Pending::Account { outer_id, .. } | Pending::Start { outer_id, .. } => outer_id,
            Pending::TurnStart { .. } => return Ok(()),
        };
        self.send_acp_error(outer_id, -32_000, "Codex session setup timed out")
            .await
    }

    async fn send_workspace_result(
        &self,
        app_id: Value,
        session_id: &str,
        turn_id: &str,
        result: std::result::Result<Value, String>,
    ) -> Result<()> {
        let Some(session) = self.sessions.get(session_id) else {
            return self
                .send_dynamic_result(
                    app_id,
                    Err("Codex tool references an unknown session".to_string()),
                )
                .await;
        };
        if session.prompt_id.is_none() || session.turn_id.as_deref() != Some(turn_id) {
            return self
                .send_dynamic_result(
                    app_id,
                    Err("Codex tool references an inactive turn".to_string()),
                )
                .await;
        }
        if session.cancelled.load(Ordering::Relaxed) {
            return self
                .send_dynamic_result(app_id, Err("Codex turn was cancelled".to_string()))
                .await;
        }
        self.send_dynamic_result(app_id, result).await
    }

    async fn cancel_session(&mut self, session_id: &str) -> Result<()> {
        let turn_id = self.sessions.get_mut(session_id).and_then(|session| {
            (!session.cancelled.swap(true, Ordering::Relaxed))
                .then(|| session.turn_id.clone())
                .flatten()
        });
        let callbacks: Vec<_> = self
            .callbacks
            .iter()
            .filter(|(_, callback)| callback.session_id == session_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in callbacks {
            if let Some(callback) = self.callbacks.remove(&id) {
                self.send_dynamic_result(
                    callback.app_id,
                    Err("Codex turn was cancelled".to_string()),
                )
                .await?;
            }
        }
        if let Some(turn_id) = turn_id {
            self.interrupt_turn(session_id, &turn_id).await?;
        }
        Ok(())
    }

    async fn interrupt_turn(&mut self, session_id: &str, turn_id: &str) -> Result<()> {
        let app_id = self.next_app_id();
        self.send_app(json!({
            "id": app_id,
            "method": "turn/interrupt",
            "params": {"threadId": session_id, "turnId": turn_id}
        }))
        .await
    }

    async fn unsubscribe_thread(&mut self, session_id: &str) -> Result<()> {
        let app_id = self.next_app_id();
        self.send_app(json!({
            "id": app_id,
            "method": "thread/unsubscribe",
            "params": {"threadId": session_id}
        }))
        .await
    }

    async fn archive_thread(&mut self, session_id: &str) -> Result<()> {
        let app_id = self.next_app_id();
        self.send_app(json!({
            "id": app_id,
            "method": "thread/archive",
            "params": {"threadId": session_id}
        }))
        .await
    }

    async fn complete_turn(&mut self, session_id: &str, turn_id: &str, status: &str) -> Result<()> {
        let (prompt_id, cancelled) = {
            let Some(session) = self.sessions.get_mut(session_id) else {
                return Ok(());
            };
            if session.turn_id.as_deref() != Some(turn_id) {
                return Ok(());
            }
            let prompt_id = session.prompt_id.take();
            session.turn_id = None;
            let cancelled = session.cancelled.load(Ordering::Relaxed) || status == "interrupted";
            session.cancelled.store(true, Ordering::Relaxed);
            (prompt_id, cancelled)
        };
        let callbacks: Vec<_> = self
            .callbacks
            .iter()
            .filter(|(_, callback)| {
                callback.session_id == session_id && callback.turn_id == turn_id
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in callbacks {
            if let Some(callback) = self.callbacks.remove(&id) {
                self.send_dynamic_result(
                    callback.app_id,
                    Err("Codex tool references an inactive turn".to_string()),
                )
                .await?;
            }
        }
        if status == "failed" {
            return self
                .send_acp_error(prompt_id, -32_000, "Codex turn failed")
                .await;
        }
        let stop_reason = if cancelled { "cancelled" } else { "end_turn" };
        self.send_acp_result(prompt_id, json!({"stopReason": stop_reason}))
            .await
    }

    async fn fail_active_prompts(&mut self, message: &str) -> Result<()> {
        self.cancel_active_turns();
        let prompts: Vec<_> = self
            .sessions
            .values_mut()
            .filter_map(|session| session.prompt_id.take())
            .collect();
        for id in prompts {
            self.send_acp_error(Some(id), -32_000, message).await?;
        }
        Ok(())
    }

    fn cancel_active_turns(&mut self) {
        for session in self
            .sessions
            .values_mut()
            .filter(|session| session.prompt_id.is_some())
        {
            session.cancelled.store(true, Ordering::Relaxed);
        }
    }

    async fn send_dynamic_result(
        &self,
        id: Value,
        result: std::result::Result<Value, String>,
    ) -> Result<()> {
        let (success, text) = match result {
            Ok(value) => (true, serde_json::to_string(&value)?),
            Err(error) => (false, error),
        };
        anyhow::ensure!(
            text.len() <= MAX_TOOL_CONTENT_BYTES,
            "Codex dynamic-tool response exceeds the size limit"
        );
        self.send_app(json!({
            "id": id,
            "result": {"contentItems": [{"type": "inputText", "text": text}], "success": success}
        }))
        .await
    }

    async fn send_update(&self, session_id: &str, text: &str) -> Result<()> {
        anyhow::ensure!(
            text.len() <= MAX_TOOL_CONTENT_BYTES,
            "Codex output exceeds the size limit"
        );
        self.send_acp(json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": session_id,
                "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}
            }
        }))
        .await
    }

    async fn send_acp_result(&self, id: Option<Value>, result: Value) -> Result<()> {
        if let Some(id) = id {
            self.send_acp(json!({"jsonrpc": "2.0", "id": id, "result": result}))
                .await?;
        }
        Ok(())
    }

    async fn send_acp_error(&self, id: Option<Value>, code: i64, message: &str) -> Result<()> {
        if let Some(id) = id {
            self.send_acp(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": code, "message": message}
            }))
            .await?;
        }
        Ok(())
    }

    async fn send_acp(&self, message: Value) -> Result<()> {
        ensure_message_fits(&message)?;
        self.acp_out
            .send(message)
            .await
            .context("ACP output channel is closed")
    }

    async fn send_app(&self, message: Value) -> Result<()> {
        ensure_message_fits(&message)?;
        self.app_out
            .send(message)
            .await
            .context("Codex app-server output channel is closed")
    }

    fn next_app_id(&self) -> Value {
        Value::String(format!(
            "red-codex-app-{}",
            self.next_id.fetch_add(1, Ordering::Relaxed)
        ))
    }
}

async fn read_bounded_line(reader: &mut (impl AsyncBufRead + Unpin)) -> Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let bytes = reader
        .take((MAX_FRAME_BYTES + 1) as u64)
        .read_until(b'\n', &mut line)
        .await?;
    if bytes == 0 {
        return Ok(None);
    }
    anyhow::ensure!(
        line.len() <= MAX_FRAME_BYTES,
        "incoming frame exceeds the size limit"
    );
    anyhow::ensure!(
        line.last() == Some(&b'\n'),
        "incoming frame is not newline-terminated"
    );
    line.pop();
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Ok(Some(line))
}

async fn write_message(writer: &mut (impl AsyncWrite + Unpin), message: &Value) -> Result<()> {
    ensure_message_fits(message)?;
    let mut encoded = serde_json::to_vec(message)?;
    encoded.push(b'\n');
    writer.write_all(&encoded).await?;
    writer.flush().await?;
    Ok(())
}

fn ensure_message_fits(message: &Value) -> Result<()> {
    anyhow::ensure!(
        serde_json::to_vec(message)?.len().saturating_add(1) <= MAX_FRAME_BYTES,
        "encoded frame exceeds the size limit"
    );
    Ok(())
}

fn prompt_text(prompt: Option<&Value>) -> String {
    prompt
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn validate_workspace_root(cwd: &Path) -> Result<PathBuf> {
    anyhow::ensure!(cwd.is_absolute(), "workspace root must be absolute");

    let inspected = physical_workspace_root(cwd);

    for ancestor in inspected.ancestors() {
        let metadata =
            std::fs::symlink_metadata(ancestor).context("failed to inspect workspace root")?;
        anyhow::ensure!(
            !metadata.file_type().is_symlink(),
            "workspace root cannot contain a symlink"
        );
    }

    let metadata =
        std::fs::symlink_metadata(&inspected).context("failed to inspect workspace root")?;
    anyhow::ensure!(
        metadata.file_type().is_dir(),
        "workspace root must be a directory"
    );
    Ok(cwd.to_path_buf())
}

fn physical_workspace_root(cwd: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        for (alias, target) in [
            (Path::new("/var"), Path::new("/private/var")),
            (Path::new("/tmp"), Path::new("/private/tmp")),
            (Path::new("/etc"), Path::new("/private/etc")),
        ] {
            if let Ok(suffix) = cwd.strip_prefix(alias) {
                return target.join(suffix);
            }
        }
    }
    cwd.to_path_buf()
}

fn validate_arguments(arguments: &Value, required: &[&str]) -> Result<()> {
    let object = arguments
        .as_object()
        .context("tool arguments must be an object")?;
    anyhow::ensure!(
        object.len() == required.len(),
        "tool arguments contain an unexpected field"
    );
    anyhow::ensure!(
        required.iter().all(|field| object.contains_key(*field)),
        "tool arguments are missing a field"
    );
    Ok(())
}

fn required_string<'a>(object: &'a Value, field: &str) -> Result<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("tool requires string field {field:?}"))
}

fn resolve_workspace_path(cwd: &Path, raw: &str) -> Result<PathBuf> {
    validate_workspace_root(cwd)?;
    anyhow::ensure!(!raw.is_empty(), "workspace path cannot be empty");
    let candidate = Path::new(raw);
    let mut resolved = if candidate.is_absolute() {
        PathBuf::new()
    } else {
        cwd.to_path_buf()
    };
    for component in candidate.components() {
        match component {
            Component::Prefix(prefix) => {
                anyhow::ensure!(
                    candidate.is_absolute(),
                    "workspace path has a relative prefix"
                );
                resolved.push(prefix.as_os_str());
            }
            Component::RootDir => resolved.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => anyhow::bail!("workspace path contains parent traversal"),
            Component::Normal(part) => resolved.push(part),
        }
    }
    anyhow::ensure!(
        resolved.starts_with(cwd),
        "workspace path is outside the session root"
    );
    let mut current = cwd.to_path_buf();
    for component in resolved.strip_prefix(cwd)?.components() {
        current.push(component.as_os_str());
        if let Ok(metadata) = std::fs::symlink_metadata(&current) {
            anyhow::ensure!(
                !metadata.file_type().is_symlink(),
                "workspace path contains a symlink"
            );
        }
    }
    Ok(resolved)
}

fn list_files(cwd: &Path, cancelled: &AtomicBool) -> Result<Vec<String>> {
    validate_workspace_root(cwd)?;
    let mut files = Vec::new();
    let mut entries = 0usize;
    let started = std::time::Instant::now();
    for entry in WalkBuilder::new(cwd)
        .hidden(false)
        .follow_links(false)
        .build()
    {
        anyhow::ensure!(
            !cancelled.load(Ordering::Relaxed),
            "workspace list was cancelled"
        );
        entries = entries.saturating_add(1);
        if entries > MAX_WALK_ENTRIES || started.elapsed() >= MAX_WALK_TIME {
            break;
        }
        let entry = entry.context("failed to inspect workspace")?;
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(cwd)
            .context("workspace walker escaped its root")?;
        files.push(relative.to_string_lossy().replace('\\', "/"));
        if files.len() >= MAX_FILES {
            break;
        }
    }
    files.sort_unstable();
    Ok(files)
}

fn search_files(cwd: &Path, query: &str, cancelled: &AtomicBool) -> Result<Vec<Value>> {
    #[cfg(not(unix))]
    {
        let _ = (cwd, query, cancelled);
        anyhow::bail!("workspace content search is unavailable on this platform");
    }

    #[cfg(unix)]
    {
        let mut results = Vec::new();
        let mut scanned_bytes = 0u64;
        for path in list_files(cwd, cancelled)? {
            anyhow::ensure!(
                !cancelled.load(Ordering::Relaxed),
                "workspace search was cancelled"
            );
            let Some((content, bytes)) = read_workspace_file(cwd, &path)? else {
                continue;
            };
            scanned_bytes = scanned_bytes.saturating_add(bytes);
            if scanned_bytes > MAX_SEARCH_BYTES {
                break;
            }
            for (line, text) in content.lines().enumerate() {
                anyhow::ensure!(
                    !cancelled.load(Ordering::Relaxed),
                    "workspace search was cancelled"
                );
                if text.contains(query) {
                    results.push(json!({"path": path, "line": line + 1, "text": text.chars().take(300).collect::<String>()}));
                    if results.len() >= MAX_SEARCH_RESULTS {
                        return Ok(results);
                    }
                }
            }
        }
        Ok(results)
    }
}

#[cfg(unix)]
fn open_workspace_file(cwd: &Path, relative: &Path) -> Result<Option<File>> {
    use std::os::fd::{AsRawFd, FromRawFd};

    use nix::{
        fcntl::{openat, OFlag},
        sys::stat::Mode,
    };

    let components: Vec<_> = relative.components().collect();
    if components.is_empty() {
        return Ok(None);
    }
    let inspected = physical_workspace_root(cwd);

    let root = openat(
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
    // SAFETY: `openat` returned a new owned descriptor and `File` becomes its sole owner.
    let mut directory = unsafe { File::from_raw_fd(root) };
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
        // SAFETY: `openat` returned a new owned descriptor and `File` becomes its sole owner.
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
        // SAFETY: `openat` returned a new owned descriptor and `File` becomes its sole owner.
        let file = unsafe { File::from_raw_fd(descriptor) };
        if final_component {
            return Ok(Some(file));
        }
        directory = file;
    }
    Ok(None)
}

#[cfg(not(unix))]
fn open_workspace_file(cwd: &Path, relative: &Path) -> Result<Option<File>> {
    let _ = (cwd, relative);
    Ok(None)
}

fn read_workspace_file(cwd: &Path, relative: &str) -> Result<Option<(String, u64)>> {
    let Some(file) = open_workspace_file(cwd, Path::new(relative))? else {
        return Ok(None);
    };
    let metadata = file
        .metadata()
        .context("failed to inspect workspace file")?;
    if !metadata.is_file() || metadata.len() > MAX_TOOL_CONTENT_BYTES as u64 {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(MAX_TOOL_CONTENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .context("failed to read workspace file")?;
    if bytes.len() > MAX_TOOL_CONTENT_BYTES {
        return Ok(None);
    }
    let byte_count = bytes.len() as u64;
    let Ok(content) = String::from_utf8(bytes) else {
        return Ok(None);
    };
    Ok(Some((content, byte_count)))
}

fn tool_definitions() -> Value {
    json!([
        {"type": "function", "name": "list_files", "description": "List up to 4096 files under the current workspace, respecting ignore files.", "inputSchema": {"type": "object", "properties": {}, "required": [], "additionalProperties": false}},
        {"type": "function", "name": "search_files", "description": "Search small text files in the workspace and return at most 200 matching lines.", "inputSchema": {"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"], "additionalProperties": false}},
        {"type": "function", "name": "read_file", "description": "Read a workspace file through the editor so unsaved buffer contents are visible.", "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"], "additionalProperties": false}},
        {"type": "function", "name": "write_file", "description": "Stage complete workspace-file contents as a reviewable editor proposal. This never writes to disk.", "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}, "required": ["path", "content"], "additionalProperties": false}}
    ])
}

fn id_key(id: &Value) -> String {
    serde_json::to_string(id).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_tools_have_strict_bounded_shapes() {
        let tools = tool_definitions();
        let tools = tools.as_array().unwrap();
        assert_eq!(tools.len(), 4);
        for tool in tools {
            assert_eq!(tool["type"], "function");
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        }
    }

    #[tokio::test]
    async fn setup_timeout_releases_capacity_and_unsubscribes_a_late_thread() {
        let workspace = tempfile::tempdir().unwrap();
        let (acp_out, mut acp_rx) = mpsc::channel(128);
        let (app_out, mut app_rx) = mpsc::channel(128);
        let (events, mut event_rx) = mpsc::channel(128);
        let mut adapter = Adapter {
            acp_out,
            app_out,
            events,
            next_id: AtomicU64::new(1),
            sessions: HashMap::new(),
            pending: HashMap::new(),
            callbacks: HashMap::new(),
            can_read: true,
            can_write: true,
        };
        for id in 0..MAX_PENDING {
            adapter.pending.insert(
                id_key(&json!(id)),
                Pending::Start {
                    outer_id: Some(json!(id + 100)),
                    cwd: workspace.path().to_path_buf(),
                    deadline: Instant::now() + SETUP_TIMEOUT,
                },
            );
        }
        let expired_id = json!(0);
        let expired_key = id_key(&expired_id);
        adapter.spawn_setup_timeout(expired_key.clone(), Instant::now());
        let Event::SetupTimeout(id) = event_rx.recv().await.unwrap() else {
            panic!("expected a Codex setup timeout");
        };
        adapter.setup_timeout(&id).await.unwrap();

        let timed_out = acp_rx.recv().await.unwrap();
        assert_eq!(timed_out["id"], 100);
        assert_eq!(
            timed_out["error"]["message"],
            "Codex session setup timed out"
        );
        assert_eq!(adapter.pending.len(), MAX_PENDING - 1);

        adapter
            .complete_app_request(json!({
                "id": expired_id,
                "result": {"thread": {"id": "late-thread"}}
            }))
            .await
            .unwrap();
        let unsubscribe = app_rx.recv().await.unwrap();
        assert_eq!(unsubscribe["method"], "thread/unsubscribe");
        assert_eq!(unsubscribe["params"]["threadId"], "late-thread");
        assert!(adapter.sessions.is_empty());

        adapter
            .check_account(Some(json!(1000)), Some(workspace.path().to_path_buf()))
            .await
            .unwrap();
        let account = app_rx.recv().await.unwrap();
        assert_eq!(account["method"], "account/read");
        assert_eq!(adapter.pending.len(), MAX_PENDING);
    }

    #[tokio::test]
    async fn expired_setup_responses_never_start_or_register_a_session() {
        let workspace = tempfile::tempdir().unwrap();
        let (acp_out, mut acp_rx) = mpsc::channel(8);
        let (app_out, mut app_rx) = mpsc::channel(8);
        let (events, _event_rx) = mpsc::channel(8);
        let mut adapter = Adapter {
            acp_out,
            app_out,
            events,
            next_id: AtomicU64::new(1),
            sessions: HashMap::new(),
            pending: HashMap::new(),
            callbacks: HashMap::new(),
            can_read: true,
            can_write: true,
        };
        let deadline = Instant::now() - Duration::from_millis(1);
        let account_id = json!("expired-account");
        adapter.pending.insert(
            id_key(&account_id),
            Pending::Account {
                outer_id: Some(json!(2)),
                cwd: Some(workspace.path().to_path_buf()),
                deadline,
            },
        );

        adapter
            .complete_app_request(json!({
                "id": account_id,
                "result": {"account": {"type": "chatgpt"}, "requiresOpenaiAuth": true}
            }))
            .await
            .unwrap();
        let timed_out = acp_rx.recv().await.unwrap();
        assert_eq!(timed_out["id"], 2);
        assert_eq!(
            timed_out["error"]["message"],
            "Codex session setup timed out"
        );
        assert!(app_rx.try_recv().is_err());
        assert!(adapter.pending.is_empty());

        let start_id = json!("expired-start");
        adapter.pending.insert(
            id_key(&start_id),
            Pending::Start {
                outer_id: Some(json!(3)),
                cwd: workspace.path().to_path_buf(),
                deadline,
            },
        );
        adapter
            .complete_app_request(json!({
                "id": start_id,
                "result": {"thread": {"id": "expired-thread"}}
            }))
            .await
            .unwrap();
        let timed_out = acp_rx.recv().await.unwrap();
        assert_eq!(timed_out["id"], 3);
        assert_eq!(
            timed_out["error"]["message"],
            "Codex session setup timed out"
        );
        let unsubscribe = app_rx.recv().await.unwrap();
        assert_eq!(unsubscribe["method"], "thread/unsubscribe");
        assert_eq!(unsubscribe["params"]["threadId"], "expired-thread");
        assert!(adapter.sessions.is_empty());
        assert!(adapter.pending.is_empty());
    }

    #[tokio::test]
    async fn completed_turn_does_not_reenable_its_workspace_cancellation_token() {
        let workspace = tempfile::tempdir().unwrap();
        let (acp_out, mut acp_rx) = mpsc::channel(8);
        let (app_out, mut app_rx) = mpsc::channel(8);
        let (events, _event_rx) = mpsc::channel(8);
        let cancelled = Arc::new(AtomicBool::new(false));
        let stale_worker_token = Arc::clone(&cancelled);
        let mut adapter = Adapter {
            acp_out,
            app_out,
            events,
            next_id: AtomicU64::new(1),
            sessions: HashMap::from([(
                "session".to_string(),
                Session {
                    cwd: workspace.path().to_path_buf(),
                    cancelled,
                    prompt_id: Some(json!(3)),
                    turn_id: Some("old-turn".to_string()),
                    tool_calls: 0,
                },
            )]),
            pending: HashMap::new(),
            callbacks: HashMap::new(),
            can_read: true,
            can_write: true,
        };

        adapter
            .complete_turn("session", "old-turn", "completed")
            .await
            .unwrap();
        assert_eq!(
            acp_rx.recv().await.unwrap()["result"]["stopReason"],
            "end_turn"
        );
        assert!(stale_worker_token.load(Ordering::Relaxed));

        adapter
            .handle_acp(json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "session/prompt",
                "params": {
                    "sessionId": "session",
                    "prompt": [{"type": "text", "text": "start a fresh turn"}]
                }
            }))
            .await
            .unwrap();
        assert_eq!(app_rx.recv().await.unwrap()["method"], "turn/start");
        let fresh_token = &adapter.sessions["session"].cancelled;
        assert!(!Arc::ptr_eq(&stale_worker_token, fresh_token));
        assert!(stale_worker_token.load(Ordering::Relaxed));
        assert!(!fresh_token.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn stale_workspace_results_never_cross_turn_boundaries() {
        let workspace = tempfile::tempdir().unwrap();
        let (acp_out, _acp_rx) = mpsc::channel(8);
        let (app_out, mut app_rx) = mpsc::channel(8);
        let (events, _event_rx) = mpsc::channel(8);
        let cancelled = Arc::new(AtomicBool::new(false));
        let adapter = Adapter {
            acp_out,
            app_out,
            events,
            next_id: AtomicU64::new(1),
            sessions: HashMap::from([(
                "session".to_string(),
                Session {
                    cwd: workspace.path().to_path_buf(),
                    cancelled: Arc::clone(&cancelled),
                    prompt_id: Some(json!(4)),
                    turn_id: Some("fresh-turn".to_string()),
                    tool_calls: 0,
                },
            )]),
            pending: HashMap::new(),
            callbacks: HashMap::new(),
            can_read: true,
            can_write: true,
        };

        adapter
            .send_workspace_result(
                json!("stale-tool"),
                "session",
                "old-turn",
                Ok(json!({"matches": [{"text": "stale private contents"}]})),
            )
            .await
            .unwrap();
        let stale = app_rx.recv().await.unwrap();
        assert_eq!(stale["id"], "stale-tool");
        assert_eq!(stale["result"]["success"], false);
        let stale_text = stale["result"]["contentItems"][0]["text"].as_str().unwrap();
        assert!(stale_text.contains("inactive turn"));
        assert!(!stale_text.contains("stale private contents"));

        cancelled.store(true, Ordering::Relaxed);
        adapter
            .send_workspace_result(
                json!("cancelled-tool"),
                "session",
                "fresh-turn",
                Ok(json!({"files": ["cancelled-private.rs"]})),
            )
            .await
            .unwrap();
        let cancelled = app_rx.recv().await.unwrap();
        assert_eq!(cancelled["id"], "cancelled-tool");
        assert_eq!(cancelled["result"]["success"], false);
        let cancelled_text = cancelled["result"]["contentItems"][0]["text"]
            .as_str()
            .unwrap();
        assert!(cancelled_text.contains("cancelled"));
        assert!(!cancelled_text.contains("cancelled-private.rs"));
    }

    #[tokio::test]
    async fn late_filesystem_callback_never_crosses_turn_boundaries() {
        let workspace = tempfile::tempdir().unwrap();
        let (acp_out, mut acp_rx) = mpsc::channel(8);
        let (app_out, mut app_rx) = mpsc::channel(128);
        let (events, _event_rx) = mpsc::channel(8);
        let cancelled = Arc::new(AtomicBool::new(false));
        let stale_worker_token = Arc::clone(&cancelled);
        let callback_id = json!("late-callback-0");
        let mut adapter = Adapter {
            acp_out,
            app_out,
            events,
            next_id: AtomicU64::new(1),
            sessions: HashMap::from([(
                "session".to_string(),
                Session {
                    cwd: workspace.path().to_path_buf(),
                    cancelled,
                    prompt_id: Some(json!(3)),
                    turn_id: Some("old-turn".to_string()),
                    tool_calls: 1,
                },
            )]),
            pending: HashMap::new(),
            callbacks: (0..MAX_PENDING)
                .map(|index| {
                    (
                        id_key(&json!(format!("late-callback-{index}"))),
                        Callback {
                            app_id: json!(format!("old-tool-{index}")),
                            session_id: "session".to_string(),
                            turn_id: "old-turn".to_string(),
                            method: "fs/read_text_file",
                        },
                    )
                })
                .collect(),
            can_read: true,
            can_write: true,
        };

        adapter
            .complete_turn("session", "old-turn", "completed")
            .await
            .unwrap();
        assert_eq!(
            acp_rx.recv().await.unwrap()["result"]["stopReason"],
            "end_turn"
        );
        for _ in 0..MAX_PENDING {
            let stale = app_rx.recv().await.unwrap();
            assert_eq!(stale["result"]["success"], false);
            let stale_text = stale["result"]["contentItems"][0]["text"].as_str().unwrap();
            assert!(stale_text.contains("inactive turn"));
        }
        assert!(adapter.callbacks.is_empty());
        adapter
            .handle_acp(json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "session/prompt",
                "params": {
                    "sessionId": "session",
                    "prompt": [{"type": "text", "text": "start a fresh turn"}]
                }
            }))
            .await
            .unwrap();
        assert_eq!(app_rx.recv().await.unwrap()["method"], "turn/start");
        adapter.sessions.get_mut("session").unwrap().turn_id = Some("fresh-turn".to_string());
        adapter
            .handle_dynamic_tool(json!({
                "id": "fresh-tool",
                "method": "item/tool/call",
                "params": {
                    "threadId": "session",
                    "turnId": "fresh-turn",
                    "tool": "read_file",
                    "arguments": {"path": "fresh.rs"}
                }
            }))
            .await
            .unwrap();
        let fresh = acp_rx.recv().await.unwrap();
        assert_eq!(fresh["method"], "fs/read_text_file");
        assert_eq!(adapter.callbacks.len(), 1);

        adapter
            .complete_callback(json!({
                "id": callback_id,
                "result": {"content": "stale unsaved contents"}
            }))
            .await
            .unwrap();
        assert!(app_rx.try_recv().is_err());
        assert!(stale_worker_token.load(Ordering::Relaxed));
        assert_eq!(adapter.callbacks.len(), 1);
    }

    #[tokio::test]
    async fn app_server_failure_cancels_every_active_workspace_token() {
        let workspace = tempfile::tempdir().unwrap();
        let (acp_out, mut acp_rx) = mpsc::channel(8);
        let (app_out, _app_rx) = mpsc::channel(8);
        let (events, _event_rx) = mpsc::channel(8);
        let first_token = Arc::new(AtomicBool::new(false));
        let second_token = Arc::new(AtomicBool::new(false));
        let idle_token = Arc::new(AtomicBool::new(false));
        let mut adapter = Adapter {
            acp_out,
            app_out,
            events,
            next_id: AtomicU64::new(1),
            sessions: HashMap::from([
                (
                    "first".to_string(),
                    Session {
                        cwd: workspace.path().to_path_buf(),
                        cancelled: Arc::clone(&first_token),
                        prompt_id: Some(json!(3)),
                        turn_id: Some("first-turn".to_string()),
                        tool_calls: 1,
                    },
                ),
                (
                    "second".to_string(),
                    Session {
                        cwd: workspace.path().to_path_buf(),
                        cancelled: Arc::clone(&second_token),
                        prompt_id: Some(json!(4)),
                        turn_id: Some("second-turn".to_string()),
                        tool_calls: 1,
                    },
                ),
                (
                    "idle".to_string(),
                    Session {
                        cwd: workspace.path().to_path_buf(),
                        cancelled: Arc::clone(&idle_token),
                        prompt_id: None,
                        turn_id: None,
                        tool_calls: 0,
                    },
                ),
            ]),
            pending: HashMap::new(),
            callbacks: HashMap::new(),
            can_read: true,
            can_write: true,
        };

        adapter
            .fail_active_prompts("Codex app-server stopped")
            .await
            .unwrap();

        let responses = [acp_rx.recv().await.unwrap(), acp_rx.recv().await.unwrap()];
        assert!(responses.iter().any(|response| response["id"] == 3));
        assert!(responses.iter().any(|response| response["id"] == 4));
        assert!(responses
            .iter()
            .all(|response| response["error"]["message"] == "Codex app-server stopped"));
        assert!(first_token.load(Ordering::Relaxed));
        assert!(second_token.load(Ordering::Relaxed));
        assert!(!idle_token.load(Ordering::Relaxed));
    }

    #[test]
    fn acp_disconnect_cancels_every_active_workspace_token_without_sending_a_response() {
        let workspace = tempfile::tempdir().unwrap();
        let (acp_out, mut acp_rx) = mpsc::channel(8);
        let (app_out, mut app_rx) = mpsc::channel(8);
        let (events, mut event_rx) = mpsc::channel(8);
        let active_token = Arc::new(AtomicBool::new(false));
        let idle_token = Arc::new(AtomicBool::new(false));
        let mut adapter = Adapter {
            acp_out,
            app_out,
            events,
            next_id: AtomicU64::new(1),
            sessions: HashMap::from([
                (
                    "active".to_string(),
                    Session {
                        cwd: workspace.path().to_path_buf(),
                        cancelled: Arc::clone(&active_token),
                        prompt_id: Some(json!(3)),
                        turn_id: Some("active-turn".to_string()),
                        tool_calls: 1,
                    },
                ),
                (
                    "idle".to_string(),
                    Session {
                        cwd: workspace.path().to_path_buf(),
                        cancelled: Arc::clone(&idle_token),
                        prompt_id: None,
                        turn_id: None,
                        tool_calls: 0,
                    },
                ),
            ]),
            pending: HashMap::new(),
            callbacks: HashMap::new(),
            can_read: true,
            can_write: true,
        };

        adapter.cancel_active_turns();

        assert!(active_token.load(Ordering::Relaxed));
        assert!(!idle_token.load(Ordering::Relaxed));
        assert_eq!(adapter.sessions["active"].prompt_id, Some(json!(3)));
        assert!(acp_rx.try_recv().is_err());
        assert!(app_rx.try_recv().is_err());
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn workspace_resolution_rejects_escape_and_symlink() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("safe.rs");
        std::fs::write(&file, "safe").unwrap();
        assert_eq!(
            resolve_workspace_path(root.path(), "safe.rs").unwrap(),
            file
        );
        assert!(resolve_workspace_path(root.path(), "../outside.rs").is_err());
        #[cfg(unix)]
        {
            let link = root.path().join("linked.rs");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            assert!(resolve_workspace_path(root.path(), "linked.rs").is_err());
        }
    }

    #[test]
    fn cancelled_workspace_tools_stop_before_reading() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("private.txt"), "private content").unwrap();
        let cancelled = AtomicBool::new(true);
        assert!(list_files(root.path(), &cancelled).is_err());
        assert!(search_files(root.path(), "private", &cancelled).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn workspace_search_reader_rejects_a_swapped_ancestor_and_fifo() {
        use nix::{sys::stat::Mode, unistd::mkfifo};
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("parent");
        let project = parent.join("project");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(outside.join("project")).unwrap();
        std::fs::write(outside.join("project/secret.txt"), "outside secret").unwrap();
        validate_workspace_root(&project).unwrap();

        std::fs::rename(&parent, temp.path().join("original-parent")).unwrap();
        symlink(&outside, &parent).unwrap();
        assert!(read_workspace_file(&project, "secret.txt").is_err());

        let fifo_root = temp.path().join("fifo-project");
        std::fs::create_dir(&fifo_root).unwrap();
        mkfifo(
            &fifo_root.join("blocked.fifo"),
            Mode::S_IRUSR | Mode::S_IWUSR,
        )
        .unwrap();
        assert!(read_workspace_file(&fifo_root, "blocked.fifo")
            .unwrap()
            .is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn workspace_search_reader_preserves_the_trusted_macos_aliases() {
        let var_temp = tempfile::tempdir().unwrap();
        let var_physical = var_temp.path().canonicalize().unwrap();
        let var_alias = Path::new("/var").join(var_physical.strip_prefix("/private/var").unwrap());
        let tmp_temp = tempfile::Builder::new()
            .prefix("red-codex-acp-")
            .tempdir_in("/private/tmp")
            .unwrap();
        let tmp_physical = tmp_temp.path().canonicalize().unwrap();
        let tmp_alias = Path::new("/tmp").join(tmp_physical.strip_prefix("/private/tmp").unwrap());

        for (alias, physical) in [
            (var_alias.as_path(), var_physical.as_path()),
            (tmp_alias.as_path(), tmp_physical.as_path()),
        ] {
            std::fs::write(physical.join("safe.txt"), "safe contents").unwrap();
            assert_eq!(validate_workspace_root(alias).unwrap(), alias);
            assert_eq!(
                resolve_workspace_path(alias, "safe.txt").unwrap(),
                alias.join("safe.txt")
            );

            let (contents, _) = read_workspace_file(alias, "safe.txt").unwrap().unwrap();

            assert_eq!(contents, "safe contents");
        }

        let etc_alias = Path::new("/etc");
        assert_eq!(validate_workspace_root(etc_alias).unwrap(), etc_alias);
        assert_eq!(
            resolve_workspace_path(etc_alias, "hosts").unwrap(),
            etc_alias.join("hosts")
        );
        let (contents, _) = read_workspace_file(etc_alias, "hosts").unwrap().unwrap();
        assert_eq!(
            contents,
            std::fs::read_to_string("/private/etc/hosts").unwrap()
        );
    }

    #[tokio::test]
    async fn bounded_frame_rejects_continuation_and_escaping_heavy_payload() {
        let bytes = vec![b'x'; MAX_FRAME_BYTES + 1];
        let mut reader = BufReader::new(bytes.as_slice());
        assert!(read_bounded_line(&mut reader).await.is_err());
        let escaped = "\\".repeat(MAX_TOOL_CONTENT_BYTES);
        assert!(ensure_message_fits(&json!({"value": escaped})).is_err());
    }
}
