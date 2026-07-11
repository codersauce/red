//! First-party ACP adapter backed by the OpenAI Responses API.
//!
//! The model can inspect the workspace with bounded read-only tools, while all file
//! reads and writes requested explicitly by the model cross the ACP client boundary.
//! In particular, this process never writes workspace files directly.

use std::{
    collections::{hash_map::Entry, HashMap},
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::{Context, Result};
use clap::Parser;
use ignore::WalkBuilder;
use reqwest::Url;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter},
    sync::{mpsc, oneshot, watch, Mutex},
    time::timeout,
};

const MAX_ACP_LINE_BYTES: usize = 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOOL_CONTENT_BYTES: usize = 960 * 1024;
const MAX_HISTORY_BYTES: usize = 256 * 1024;
const MAX_TOOL_ROUNDS: usize = 12;
const MAX_TOOL_CALLS: usize = 32;
const MAX_SESSIONS: usize = 64;
const MAX_PENDING_CALLBACKS: usize = 64;
const MAX_FILES: usize = 4_096;
const MAX_SEARCH_RESULTS: usize = 200;
const MAX_SEARCH_BYTES: u64 = 32 * 1024 * 1024;
const MAX_WALK_ENTRIES: usize = 65_536;
const MAX_WALK_TIME: Duration = Duration::from_secs(5);
const CLIENT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const HTTP_TIMEOUT: Duration = Duration::from_secs(180);
const INSTRUCTIONS: &str = "You are Red's coding assistant. Use list_files and search_files to locate relevant code. Always use read_file before reasoning about a file and write_file for every edit; writes are reviewable proposals and never touch disk. Do not claim a change was saved. Keep responses concise.";

#[derive(Debug, Parser)]
#[command(
    name = "red_openai_acp",
    version,
    about = "Red's reviewable OpenAI ACP adapter"
)]
struct Args {
    /// OpenAI Responses model to use.
    #[arg(long, env = "RED_OPENAI_MODEL", default_value = "gpt-5.6-terra")]
    model: String,
    /// API base URL. HTTP is accepted only for a loopback test server.
    #[arg(
        long,
        env = "RED_OPENAI_BASE_URL",
        default_value = "https://api.openai.com/v1"
    )]
    base_url: String,
    /// Explicitly allow sending credentials and workspace context to a custom HTTPS API host.
    #[arg(long, env = "RED_OPENAI_ALLOW_CUSTOM_HOST", default_value_t = false)]
    allow_custom_host: bool,
}

#[derive(Debug, Clone)]
struct Session {
    cwd: PathBuf,
    history: Vec<Value>,
}

#[derive(Debug)]
struct FunctionCall {
    call_id: String,
    name: String,
    arguments: Value,
}

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<std::result::Result<Value, String>>>>>;
type Sessions = Arc<Mutex<HashMap<String, Session>>>;
type Active = Arc<Mutex<HashMap<String, watch::Sender<bool>>>>;

#[derive(Clone)]
struct Adapter {
    http: reqwest::Client,
    responses_url: Url,
    model: Arc<str>,
    api_key: Arc<str>,
    outgoing: mpsc::Sender<Value>,
    pending: Pending,
    sessions: Sessions,
    active: Active,
    next_callback_id: Arc<AtomicU64>,
    can_read: bool,
    can_write: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    let responses_url = responses_url(&args.base_url, args.allow_custom_host)?;
    let api_key = std::env::var("OPENAI_API_KEY")
        .unwrap_or_default()
        .trim()
        .to_string();
    let http = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .context("failed to construct OpenAI HTTP client")?;
    let (outgoing, mut outgoing_rx) = mpsc::channel::<Value>(64);
    let writer = tokio::spawn(async move {
        let stdout = tokio::io::stdout();
        let mut stdout = BufWriter::new(stdout);
        while let Some(message) = outgoing_rx.recv().await {
            let mut line = serde_json::to_vec(&message)?;
            anyhow::ensure!(
                line.len() < MAX_ACP_LINE_BYTES,
                "outgoing ACP message exceeds {MAX_ACP_LINE_BYTES} bytes"
            );
            line.push(b'\n');
            stdout.write_all(&line).await?;
            stdout.flush().await?;
        }
        Ok::<(), anyhow::Error>(())
    });

    let mut adapter = Adapter {
        http,
        responses_url,
        model: Arc::from(args.model),
        api_key: Arc::from(api_key),
        outgoing,
        pending: Arc::new(Mutex::new(HashMap::new())),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        active: Arc::new(Mutex::new(HashMap::new())),
        next_callback_id: Arc::new(AtomicU64::new(1)),
        can_read: false,
        can_write: false,
    };
    let stdin = tokio::io::stdin();
    let mut stdin = BufReader::new(stdin);
    while let Some(line) = read_bounded_line(&mut stdin).await? {
        let message: Value = match serde_json::from_slice(&line) {
            Ok(message) => message,
            Err(_) => {
                eprintln!("event=acp_invalid_json level=warn");
                continue;
            }
        };
        adapter.handle_message(message).await?;
    }
    adapter.active.lock().await.clear();
    drop(adapter.outgoing);
    writer.await.context("ACP writer task failed")??;
    Ok(())
}

impl Adapter {
    async fn handle_message(&mut self, message: Value) -> Result<()> {
        let id = message.get("id").cloned();
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            self.complete_callback(message).await;
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
                self.send_result(
                    id,
                    json!({
                        "protocolVersion": 1,
                        "agentCapabilities": {
                            "loadSession": false,
                            "promptCapabilities": {"image": false, "audio": false, "embeddedContext": false},
                            "mcpCapabilities": {"http": false, "sse": false}
                        },
                        "authMethods": [{
                            "id": "openai_api_key",
                            "name": "OPENAI_API_KEY",
                            "description": "Authenticate with an OpenAI API key from the environment."
                        }],
                        "agentInfo": {"name": "red-openai-acp", "version": env!("CARGO_PKG_VERSION")}
                    }),
                )
                .await?;
            }
            "authenticate" => {
                if self.api_key.is_empty() {
                    self.send_error(id, -32_001, "OPENAI_API_KEY is not configured")
                        .await?;
                } else {
                    self.send_result(id, json!({})).await?;
                }
            }
            "session/new" => {
                let Some(cwd) = message
                    .get("params")
                    .and_then(|params| params.get("cwd"))
                    .and_then(Value::as_str)
                else {
                    self.send_error(id, -32_602, "session/new requires cwd")
                        .await?;
                    return Ok(());
                };
                let cwd = PathBuf::from(cwd);
                if !cwd.is_absolute() || !cwd.is_dir() {
                    self.send_error(
                        id,
                        -32_602,
                        "session cwd is not an existing absolute directory",
                    )
                    .await?;
                    return Ok(());
                }
                let session_id = format!("red-openai-{}", uuid::Uuid::new_v4());
                let inserted = {
                    let mut sessions = self.sessions.lock().await;
                    if sessions.len() >= MAX_SESSIONS {
                        false
                    } else {
                        sessions.insert(
                            session_id.clone(),
                            Session {
                                cwd,
                                history: Vec::new(),
                            },
                        );
                        true
                    }
                };
                if !inserted {
                    self.send_error(id, -32_000, "ACP session capacity reached")
                        .await?;
                    return Ok(());
                }
                self.send_result(id, json!({"sessionId": session_id}))
                    .await?;
            }
            "session/prompt" => {
                let params = message.get("params");
                let Some(session_id) = params
                    .and_then(|params| params.get("sessionId"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                else {
                    self.send_error(id, -32_602, "session/prompt requires sessionId")
                        .await?;
                    return Ok(());
                };
                let prompt = prompt_text(params.and_then(|params| params.get("prompt")));
                if prompt.is_empty() {
                    self.send_error(id, -32_602, "session/prompt requires text content")
                        .await?;
                    return Ok(());
                }
                let Some(session) = self.sessions.lock().await.get(&session_id).cloned() else {
                    self.send_error(id, -32_602, "unknown ACP session").await?;
                    return Ok(());
                };
                if self.api_key.is_empty() {
                    self.send_error(id, -32_001, "OPENAI_API_KEY is not configured")
                        .await?;
                    return Ok(());
                }
                if !self.can_read || !self.can_write {
                    self.send_error(
                        id,
                        -32_602,
                        "Red OpenAI ACP requires fs/read_text_file and fs/write_text_file capabilities",
                    )
                    .await?;
                    return Ok(());
                }
                let (cancel_tx, cancel_rx) = watch::channel(false);
                let inserted = match self.active.lock().await.entry(session_id.clone()) {
                    Entry::Vacant(entry) => {
                        entry.insert(cancel_tx);
                        true
                    }
                    Entry::Occupied(_) => false,
                };
                if !inserted {
                    self.send_error(
                        id,
                        -32_000,
                        "an ACP prompt is already active for this session",
                    )
                    .await?;
                    return Ok(());
                }
                let adapter = self.clone();
                tokio::spawn(async move {
                    let result = adapter
                        .run_prompt(&session_id, session, &prompt, cancel_rx)
                        .await;
                    adapter.active.lock().await.remove(&session_id);
                    match result {
                        Ok(stop_reason) => {
                            let _ = adapter
                                .send_result(id, json!({"stopReason": stop_reason}))
                                .await;
                        }
                        Err(error) => {
                            eprintln!("event=openai_prompt_failed level=error");
                            let _ = adapter.send_error(id, -32_000, &error.to_string()).await;
                        }
                    }
                });
            }
            "session/cancel" => {
                if let Some(session_id) = message
                    .get("params")
                    .and_then(|params| params.get("sessionId"))
                    .and_then(Value::as_str)
                {
                    if let Some(cancel) = self.active.lock().await.get(session_id) {
                        let _ = cancel.send(true);
                    }
                }
            }
            _ if id.is_some() => {
                self.send_error(id, -32_601, "unsupported ACP method")
                    .await?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn complete_callback(&self, message: Value) {
        let Some(id) = message.get("id") else {
            return;
        };
        let key = id_key(id);
        let Some(response) = self.pending.lock().await.remove(&key) else {
            return;
        };
        let result = if let Some(error) = message.get("error") {
            Err(error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("ACP client rejected the request")
                .to_string())
        } else {
            Ok(message.get("result").cloned().unwrap_or(Value::Null))
        };
        let _ = response.send(result);
    }

    async fn run_prompt(
        &self,
        session_id: &str,
        session: Session,
        prompt: &str,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<&'static str> {
        let mut input = session.history;
        input.push(json!({"role": "user", "content": prompt}));
        let mut calls = 0usize;
        for _round in 0..MAX_TOOL_ROUNDS {
            if *cancel.borrow() {
                return Ok("cancelled");
            }
            let body = json!({
                "model": self.model.as_ref(),
                "instructions": INSTRUCTIONS,
                "input": input,
                "tools": tool_definitions(),
                "parallel_tool_calls": false,
                "store": false,
                "include": ["reasoning.encrypted_content"],
                "max_output_tokens": 8192
            });
            let encoded = serde_json::to_vec(&body)?;
            anyhow::ensure!(
                encoded.len() <= MAX_RESPONSE_BYTES,
                "OpenAI request exceeds {MAX_RESPONSE_BYTES} bytes"
            );
            let send = self
                .http
                .post(self.responses_url.clone())
                .bearer_auth(self.api_key.as_ref())
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(encoded)
                .send();
            let response = tokio::select! {
                result = send => result.context("OpenAI request failed")?,
                _ = cancel.changed() => return Ok("cancelled"),
            };
            let status = response.status();
            let response_body = read_response_body(response, &mut cancel).await?;
            if !status.is_success() {
                anyhow::bail!("OpenAI request failed with HTTP {}", status.as_u16());
            }
            let response: Value = serde_json::from_slice(&response_body)
                .context("OpenAI response was not valid JSON")?;
            let output = response
                .get("output")
                .and_then(Value::as_array)
                .context("OpenAI response did not contain output")?;
            let function_calls: Vec<FunctionCall> = output
                .iter()
                .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
                .map(validate_function_call)
                .collect::<Result<_>>()?;
            if function_calls.is_empty() {
                let text = output_text(output);
                if !text.is_empty() {
                    self.send_update(session_id, &text).await?;
                }
                input.extend(output.iter().cloned());
                self.store_history(session_id, input).await;
                return Ok(stop_reason(&response));
            }
            calls = calls.saturating_add(function_calls.len());
            anyhow::ensure!(calls <= MAX_TOOL_CALLS, "OpenAI tool-call limit reached");
            input.extend(output.iter().cloned());
            for call in function_calls {
                let result = self
                    .run_tool(session_id, &session.cwd, &call, &mut cancel)
                    .await;
                let output = match result {
                    Ok(output) => output,
                    Err(error) => json!({"ok": false, "error": error.to_string()}).to_string(),
                };
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": call.call_id,
                    "output": output
                }));
            }
        }
        anyhow::bail!("OpenAI tool-round limit reached")
    }

    async fn run_tool(
        &self,
        session_id: &str,
        cwd: &Path,
        call: &FunctionCall,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<String> {
        let arguments = &call.arguments;
        match call.name.as_str() {
            "read_file" => {
                let path = resolve_workspace_path(cwd, required_string(arguments, "path")?)?;
                let result = self
                    .callback(
                        "fs/read_text_file",
                        json!({"sessionId": session_id, "path": path}),
                        cancel,
                    )
                    .await?;
                let content = result
                    .get("content")
                    .and_then(Value::as_str)
                    .context("ACP fs/read_text_file response did not contain content")?;
                anyhow::ensure!(
                    content.len() <= MAX_TOOL_CONTENT_BYTES,
                    "ACP file content exceeds {MAX_TOOL_CONTENT_BYTES} bytes"
                );
                Ok(json!({"ok": true, "content": content}).to_string())
            }
            "write_file" => {
                let path = resolve_workspace_path(cwd, required_string(arguments, "path")?)?;
                let content = required_string(arguments, "content")?;
                anyhow::ensure!(
                    content.len() <= MAX_TOOL_CONTENT_BYTES,
                    "ACP write content exceeds {MAX_TOOL_CONTENT_BYTES} bytes"
                );
                self.callback(
                    "fs/write_text_file",
                    json!({"sessionId": session_id, "path": path, "content": content}),
                    cancel,
                )
                .await?;
                Ok(json!({"ok": true, "status": "proposal staged for review"}).to_string())
            }
            "list_files" => {
                let cancelled = Arc::new(AtomicBool::new(false));
                let worker_cancelled = Arc::clone(&cancelled);
                let cwd = cwd.to_path_buf();
                let mut list = tokio::task::spawn_blocking(move || {
                    list_files(&cwd, worker_cancelled.as_ref())
                });
                let files = tokio::select! {
                    result = &mut list => result.context("workspace list task failed")??,
                    _ = cancel.changed() => {
                        cancelled.store(true, Ordering::Relaxed);
                        anyhow::bail!("ACP prompt was cancelled");
                    }
                };
                Ok(json!({"ok": true, "files": files}).to_string())
            }
            "search_files" => {
                let query = required_string(arguments, "query")?;
                anyhow::ensure!(!query.is_empty(), "search query cannot be empty");
                anyhow::ensure!(query.len() <= 512, "search query exceeds 512 bytes");
                let cancelled = Arc::new(AtomicBool::new(false));
                let worker_cancelled = Arc::clone(&cancelled);
                let cwd = cwd.to_path_buf();
                let query = query.to_string();
                let mut search = tokio::task::spawn_blocking(move || {
                    search_files(&cwd, &query, worker_cancelled.as_ref())
                });
                let results = tokio::select! {
                    result = &mut search => result.context("workspace search task failed")??,
                    _ = cancel.changed() => {
                        cancelled.store(true, Ordering::Relaxed);
                        anyhow::bail!("ACP prompt was cancelled");
                    }
                };
                Ok(json!({"ok": true, "results": results}).to_string())
            }
            _ => anyhow::bail!("unsupported OpenAI tool"),
        }
    }

    async fn callback(
        &self,
        method: &'static str,
        params: Value,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<Value> {
        let id = format!(
            "red-openai-{}",
            self.next_callback_id.fetch_add(1, Ordering::Relaxed)
        );
        let (response_tx, response_rx) = oneshot::channel();
        let key = id_key(&Value::String(id.clone()));
        {
            let mut pending = self.pending.lock().await;
            anyhow::ensure!(
                pending.len() < MAX_PENDING_CALLBACKS,
                "ACP filesystem callback capacity reached"
            );
            pending.insert(key.clone(), response_tx);
        }
        if let Err(error) = self
            .enqueue(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await
        {
            self.pending.lock().await.remove(&key);
            return Err(error);
        }
        let result = tokio::select! {
            result = timeout(CLIENT_REQUEST_TIMEOUT, response_rx) => result,
            _ = cancel.changed() => {
                self.pending.lock().await.remove(&key);
                anyhow::bail!("ACP prompt was cancelled");
            }
        };
        self.pending.lock().await.remove(&key);
        match result {
            Ok(Ok(Ok(result))) => Ok(result),
            Ok(Ok(Err(message))) => {
                anyhow::bail!("ACP client rejected filesystem request: {message}")
            }
            Ok(Err(_)) => anyhow::bail!("ACP filesystem response channel closed"),
            Err(_) => anyhow::bail!("ACP filesystem request timed out"),
        }
    }

    async fn send_result(&self, id: Option<Value>, result: Value) -> Result<()> {
        if let Some(id) = id {
            self.enqueue(json!({"jsonrpc": "2.0", "id": id, "result": result}))
                .await?;
        }
        Ok(())
    }

    async fn send_error(&self, id: Option<Value>, code: i64, message: &str) -> Result<()> {
        if let Some(id) = id {
            self.enqueue(
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
            )
            .await?;
        }
        Ok(())
    }

    async fn send_update(&self, session_id: &str, text: &str) -> Result<()> {
        anyhow::ensure!(
            text.len() <= MAX_TOOL_CONTENT_BYTES,
            "OpenAI output exceeds {MAX_TOOL_CONTENT_BYTES} bytes"
        );
        self.enqueue(json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": session_id,
                    "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}
                }
            }))
        .await?;
        Ok(())
    }

    async fn store_history(&self, session_id: &str, mut history: Vec<Value>) {
        let mut sessions = self.sessions.lock().await;
        let Some(session) = sessions.get_mut(session_id) else {
            return;
        };
        while serde_json::to_vec(&history).is_ok_and(|history| history.len() > MAX_HISTORY_BYTES) {
            let next_turn = history
                .iter()
                .enumerate()
                .skip(1)
                .find(|(_, item)| item.get("role").and_then(Value::as_str) == Some("user"))
                .map(|(index, _)| index);
            let Some(next_turn) = next_turn else {
                history.clear();
                break;
            };
            history.drain(..next_turn);
        }
        session.history = history;
    }

    async fn enqueue(&self, message: Value) -> Result<()> {
        ensure_acp_message_fits(&message)?;
        self.outgoing
            .send(message)
            .await
            .context("ACP output channel is closed")?;
        Ok(())
    }
}

async fn read_bounded_line(reader: &mut (impl AsyncBufRead + Unpin)) -> Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let bytes = reader
        .take((MAX_ACP_LINE_BYTES + 1) as u64)
        .read_until(b'\n', &mut line)
        .await?;
    if bytes == 0 {
        return Ok(None);
    }
    anyhow::ensure!(
        line.len() <= MAX_ACP_LINE_BYTES,
        "incoming ACP message exceeds {MAX_ACP_LINE_BYTES} bytes"
    );
    anyhow::ensure!(
        line.last() == Some(&b'\n'),
        "incoming ACP frame is not newline-terminated"
    );
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Ok(Some(line))
}

async fn read_response_body(
    mut response: reqwest::Response,
    cancel: &mut watch::Receiver<bool>,
) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let next = tokio::select! {
            next = response.chunk() => next.context("failed to read OpenAI response")?,
            _ = cancel.changed() => anyhow::bail!("ACP prompt was cancelled"),
        };
        let Some(chunk) = next else {
            return Ok(body);
        };
        anyhow::ensure!(
            body.len().saturating_add(chunk.len()) <= MAX_RESPONSE_BYTES,
            "OpenAI response exceeds {MAX_RESPONSE_BYTES} bytes"
        );
        body.extend_from_slice(&chunk);
    }
}

fn responses_url(base: &str, allow_custom_host: bool) -> Result<Url> {
    let mut url = Url::parse(base).context("RED_OPENAI_BASE_URL is not a valid URL")?;
    anyhow::ensure!(
        url.username().is_empty() && url.password().is_none(),
        "API base URL must not include credentials"
    );
    anyhow::ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "API base URL must not include a query or fragment"
    );
    let loopback = matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "::1" | "[::1]")
    );
    anyhow::ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && loopback),
        "API base URL must use HTTPS (HTTP is allowed only for loopback tests)"
    );
    anyhow::ensure!(
        loopback || url.host_str() == Some("api.openai.com") || allow_custom_host,
        "custom API hosts require --allow-custom-host or RED_OPENAI_ALLOW_CUSTOM_HOST=true"
    );
    let path = url.path().trim_end_matches('/');
    let path = if path.ends_with("/responses") {
        path.to_string()
    } else {
        format!("{path}/responses")
    };
    url.set_path(&path);
    Ok(url)
}

fn validate_function_call(call: &Value) -> Result<FunctionCall> {
    let call_id = call
        .get("call_id")
        .and_then(Value::as_str)
        .context("OpenAI function call did not contain call_id")?;
    anyhow::ensure!(
        !call_id.is_empty() && call_id.len() <= 256,
        "OpenAI function call_id is invalid"
    );
    let name = call
        .get("name")
        .and_then(Value::as_str)
        .context("OpenAI function call did not contain name")?;
    anyhow::ensure!(
        matches!(
            name,
            "read_file" | "write_file" | "list_files" | "search_files"
        ),
        "unsupported OpenAI tool"
    );
    let arguments = call
        .get("arguments")
        .and_then(Value::as_str)
        .context("OpenAI function call did not contain arguments")?;
    anyhow::ensure!(
        arguments.len() <= MAX_TOOL_CONTENT_BYTES,
        "OpenAI tool arguments exceed {MAX_TOOL_CONTENT_BYTES} bytes"
    );
    let arguments = serde_json::from_str(arguments)
        .context("OpenAI function-call arguments were not valid JSON")?;
    Ok(FunctionCall {
        call_id: call_id.to_string(),
        name: name.to_string(),
        arguments,
    })
}

fn ensure_acp_message_fits(message: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(message)?.len().saturating_add(1);
    anyhow::ensure!(
        bytes <= MAX_ACP_LINE_BYTES,
        "encoded ACP message exceeds {MAX_ACP_LINE_BYTES} bytes"
    );
    Ok(())
}

fn prompt_text(prompt: Option<&Value>) -> String {
    prompt
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|block| {
            (block.get("type").and_then(Value::as_str) == Some("text"))
                .then(|| block.get("text").and_then(Value::as_str))
                .flatten()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn output_text(output: &[Value]) -> String {
    output
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
        .filter_map(|message| message.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|content| content.get("type").and_then(Value::as_str) == Some("output_text"))
        .filter_map(|content| content.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("")
}

fn stop_reason(response: &Value) -> &'static str {
    match response
        .get("incomplete_details")
        .and_then(|details| details.get("reason"))
        .and_then(Value::as_str)
    {
        Some("max_output_tokens") => "max_tokens",
        Some("content_filter") => "refusal",
        _ => "end_turn",
    }
}

fn required_string<'a>(object: &'a Value, field: &str) -> Result<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("OpenAI tool requires string field {field:?}"))
}

fn resolve_workspace_path(cwd: &Path, raw: &str) -> Result<PathBuf> {
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
    let metadata = std::fs::symlink_metadata(cwd).context("failed to inspect workspace root")?;
    anyhow::ensure!(
        metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
        "workspace root must be a directory and cannot be a symlink"
    );
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
                let text: String = text.chars().take(300).collect();
                results.push(json!({"path": path, "line": line + 1, "text": text}));
                if results.len() >= MAX_SEARCH_RESULTS {
                    return Ok(results);
                }
            }
        }
    }
    Ok(results)
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
    let root = openat(
        None,
        cwd,
        OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW,
        Mode::empty(),
    )
    .context("failed to safely open workspace root")?;
    // SAFETY: `openat` returned a new owned descriptor and `File` becomes its sole owner.
    let mut directory = unsafe { File::from_raw_fd(root) };
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            anyhow::bail!("workspace walker returned a non-normal path");
        };
        let final_component = index + 1 == components.len();
        let mut flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW;
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
    // Windows does not expose a portable, component-wise no-follow open through `std`.
    // Refuse content search instead of racing a reparse-point replacement.
    Ok(None)
}

fn read_workspace_file(cwd: &Path, relative: &str) -> Result<Option<(String, u64)>> {
    let relative = Path::new(relative);
    let Some(file) = open_workspace_file(cwd, relative)? else {
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
        {
            "type": "function",
            "name": "list_files",
            "description": "List up to 4096 files under the current workspace, respecting ignore files.",
            "strict": true,
            "parameters": {"type": "object", "properties": {}, "required": [], "additionalProperties": false}
        },
        {
            "type": "function",
            "name": "search_files",
            "description": "Search small text files in the workspace and return at most 200 matching lines.",
            "strict": true,
            "parameters": {"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"], "additionalProperties": false}
        },
        {
            "type": "function",
            "name": "read_file",
            "description": "Read a workspace file through the editor so unsaved buffer contents are visible.",
            "strict": true,
            "parameters": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"], "additionalProperties": false}
        },
        {
            "type": "function",
            "name": "write_file",
            "description": "Stage complete workspace-file contents as a reviewable editor proposal. This never writes to disk.",
            "strict": true,
            "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}, "required": ["path", "content"], "additionalProperties": false}
        }
    ])
}

fn id_key(id: &Value) -> String {
    serde_json::to_string(id).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_requires_tls_except_loopback() {
        assert_eq!(
            responses_url("https://api.openai.com/v1/", false)
                .unwrap()
                .as_str(),
            "https://api.openai.com/v1/responses"
        );
        assert_eq!(
            responses_url("http://127.0.0.1:8080/v1", false)
                .unwrap()
                .as_str(),
            "http://127.0.0.1:8080/v1/responses"
        );
        assert!(responses_url("http://example.test/v1", false).is_err());
        assert!(responses_url("https://example.test/v1", false).is_err());
        assert!(responses_url("https://example.test/v1", true).is_ok());
        assert!(responses_url("https://user:secret@example.test/v1", true).is_err());
        assert!(responses_url("https://example.test/v1?token=secret", true).is_err());
    }

    #[test]
    fn workspace_resolution_rejects_escape_and_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        assert_eq!(
            resolve_workspace_path(&root, "src/main.rs").unwrap(),
            root.join("src/main.rs")
        );
        assert!(resolve_workspace_path(&root, "src/../main.rs").is_err());
        assert!(resolve_workspace_path(&root, "../workspace/main.rs").is_err());
        assert!(resolve_workspace_path(&root, "../secret").is_err());
        assert!(resolve_workspace_path(&root, "/tmp/secret").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(temp.path(), root.join("link")).unwrap();
            assert!(resolve_workspace_path(&root, "link/secret").is_err());
        }
    }

    #[tokio::test]
    async fn bounded_line_requires_newline_and_rejects_continuation() {
        let mut complete = BufReader::new(&b"{}\n"[..]);
        assert_eq!(
            read_bounded_line(&mut complete).await.unwrap(),
            Some(b"{}".to_vec())
        );

        let mut unterminated = BufReader::new(&b"{}"[..]);
        assert!(read_bounded_line(&mut unterminated).await.is_err());

        let oversized = vec![b'x'; MAX_ACP_LINE_BYTES + 1];
        let mut oversized = BufReader::new(oversized.as_slice());
        assert!(read_bounded_line(&mut oversized).await.is_err());
    }

    #[test]
    fn cancelled_search_stops_before_reading_workspace() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("file.txt"), "needle").unwrap();
        let cancelled = AtomicBool::new(true);

        assert!(search_files(temp.path(), "needle", &cancelled).is_err());
    }

    #[test]
    fn cancelled_list_stops_before_walking_workspace() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("file.txt"), "needle").unwrap();
        let cancelled = AtomicBool::new(true);

        assert!(list_files(temp.path(), &cancelled).is_err());
    }

    #[test]
    fn list_refuses_non_directory_or_linked_workspace_roots() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("file.txt");
        std::fs::write(&file, "must not be walked").unwrap();
        let cancelled = AtomicBool::new(false);
        assert!(list_files(&file, &cancelled).is_err());

        #[cfg(unix)]
        {
            let outside = temp.path().join("outside");
            let linked_root = temp.path().join("linked-root");
            std::fs::create_dir(&outside).unwrap();
            std::fs::write(outside.join("secret-name.txt"), "must not be listed").unwrap();
            std::os::unix::fs::symlink(&outside, &linked_root).unwrap();
            assert!(list_files(&linked_root, &cancelled).is_err());
        }
    }

    #[test]
    fn bounded_search_read_refuses_oversized_files_and_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let large = temp.path().join("large.txt");
        std::fs::write(&large, vec![b'x'; MAX_TOOL_CONTENT_BYTES + 1]).unwrap();
        assert!(read_workspace_file(temp.path(), "large.txt")
            .unwrap()
            .is_none());

        #[cfg(unix)]
        {
            let outside = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(outside.path(), "must not be read").unwrap();
            std::os::unix::fs::symlink(outside.path(), temp.path().join("link.txt")).unwrap();
            assert!(read_workspace_file(temp.path(), "link.txt")
                .unwrap()
                .is_none());

            let linked_root = temp.path().join("linked-root");
            std::os::unix::fs::symlink(outside.path().parent().unwrap(), &linked_root).unwrap();
            assert!(read_workspace_file(&linked_root, "link.txt").is_err());
        }
    }

    #[test]
    fn escaping_heavy_acp_payload_is_rejected_before_enqueue() {
        let content = "\"".repeat(MAX_TOOL_CONTENT_BYTES);
        let message = json!({
            "jsonrpc": "2.0",
            "id": "write-1",
            "method": "fs/write_text_file",
            "params": {"sessionId": "s1", "path": "/workspace/file.txt", "content": content}
        });

        assert!(ensure_acp_message_fits(&message).is_err());
    }

    #[test]
    fn malformed_function_calls_are_rejected_before_tool_execution() {
        assert!(validate_function_call(
            &json!({"type": "function_call", "name": "write_file", "arguments": "{}"})
        )
        .is_err());
        assert!(validate_function_call(
            &json!({"type": "function_call", "call_id": "x", "name": "unknown", "arguments": "{}"})
        )
        .is_err());
        assert!(validate_function_call(&json!({"type": "function_call", "call_id": "x", "name": "write_file", "arguments": "{"})).is_err());
        assert!(validate_function_call(&json!({"type": "function_call", "call_id": "x".repeat(257), "name": "write_file", "arguments": "{}"})).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_prefix_is_supported_for_absolute_inputs() {
        let root = PathBuf::from(r"C:\workspace");
        assert_eq!(
            resolve_workspace_path(&root, r"C:\workspace\file.txt").unwrap(),
            PathBuf::from(r"C:\workspace\file.txt")
        );
    }
}
