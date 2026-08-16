//! Opt-in Copilot transport. The editor owns consent, snapshots, and text mutation;
//! this worker owns the official language server and its JSON-RPC lifecycle.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use ignore::gitignore::GitignoreBuilder;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncWrite, AsyncWriteExt, BufReader},
    process::Command,
    sync::{mpsc, watch},
    task::JoinHandle,
};

use crate::{
    buffer::BufferId,
    lsp::{file_uri, Position, Range},
    undo::TextPosition,
};

/// Operations accepted by the `:Copilot` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopilotCommand {
    Enable,
    Disable,
    SignIn,
    SignOut,
    Status,
    Restart,
    Complete,
}

impl CopilotCommand {
    pub(crate) const ALL: &[(Self, &str)] = &[
        (Self::Enable, "enable"),
        (Self::Disable, "disable"),
        (Self::SignIn, "signin"),
        (Self::SignOut, "signout"),
        (Self::Status, "status"),
        (Self::Restart, "restart"),
        (Self::Complete, "complete"),
    ];

    pub(crate) fn parse(input: &str) -> Option<Self> {
        if input.is_empty() {
            return Some(Self::Status);
        }
        Self::ALL
            .iter()
            .find_map(|(command, name)| (*name == input).then_some(*command))
    }

    pub(crate) fn usage() -> String {
        format!(
            "Usage: Copilot {}",
            Self::ALL
                .iter()
                .map(|(_, name)| *name)
                .collect::<Vec<_>>()
                .join("|")
        )
    }
}

const MAX_SUGGESTION_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CopilotConfig {
    /// Enables transmission of eligible source files to GitHub Copilot.
    pub enabled: bool,
    /// Official language-server executable, invoked without a shell.
    pub command: String,
    pub args: Vec<String>,
    pub debounce_ms: u64,
    pub max_file_bytes: usize,
    /// Gitignore-style patterns for documents Red never syncs to the provider.
    pub excluded_patterns: Vec<String>,
}

impl Default for CopilotConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            command: "copilot-language-server".into(),
            args: vec!["--stdio".into()],
            debounce_ms: 150,
            max_file_bytes: 256 * 1024,
            excluded_patterns: vec![
                ".env".into(),
                ".env.*".into(),
                "*.pem".into(),
                "*.key".into(),
                "**/.git/**".into(),
            ],
        }
    }
}

impl CopilotConfig {
    pub(crate) fn allows(&self, root: &Path, path: &Path, bytes: usize) -> bool {
        if bytes > self.max_file_bytes {
            return false;
        }
        let Ok(canonical_root) = root.canonicalize() else {
            return false;
        };
        // Windows canonical paths have a verbatim prefix, while document URIs
        // round-trip to ordinary drive paths. Compare both lexical names in the
        // same document-path form, without resolving away excluded aliases.
        let Some(lexical_root) = normalized_document_path(root) else {
            return false;
        };
        let Some(lexical_path) = normalized_document_path(path) else {
            return false;
        };
        let Ok(relative_path) = lexical_path.strip_prefix(&lexical_root) else {
            return false;
        };
        let mut builder = GitignoreBuilder::new(&canonical_root);
        builder.allow_unclosed_class(false);
        for pattern in &self.excluded_patterns {
            if builder.add_line(None, pattern).is_err() {
                return false;
            }
        }
        let Ok(ignore) = builder.build() else {
            return false;
        };
        if ignore
            .matched_path_or_any_parents(relative_path, false)
            .is_ignore()
        {
            return false;
        }
        // A harmless-looking symlink must not bypass either workspace confinement
        // or exclusions on its real target. New files resolve through their parent.
        let resolved: std::io::Result<PathBuf> = path.canonicalize().or_else(|_| {
            let parent = path
                .parent()
                .ok_or_else(|| std::io::Error::other("missing parent"))?;
            Ok(parent
                .canonicalize()?
                .join(path.file_name().unwrap_or_default()))
        });
        match resolved {
            Ok(resolved) => resolved
                .strip_prefix(&canonical_root)
                .is_ok_and(|relative| {
                    !ignore
                        .matched_path_or_any_parents(relative, false)
                        .is_ignore()
                }),
            Err(_) => false,
        }
    }
}

/// Uses the same lexical path representation as buffer document URIs.
fn normalized_document_path(path: &Path) -> Option<PathBuf> {
    let uri = file_uri(path).ok()?;
    crate::lsp::normalized_file_path(&uri)
        .ok()
        .map(PathBuf::from)
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Snapshot {
    pub generation: u64,
    pub buffer_id: BufferId,
    pub revision: u64,
    pub cursor: TextPosition,
    pub uri: String,
}

#[derive(Debug, Clone)]
pub(crate) struct CompletionRequest {
    pub snapshot: Snapshot,
    pub language_id: String,
    pub contents: String,
    pub position: Position,
    pub tab_size: usize,
    pub insert_spaces: bool,
    pub automatic: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CompletionItem {
    pub insert_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<Value>,
    /// Preserve provider extensions when reporting the full displayed item.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Debug)]
pub(crate) enum Control {
    SignIn,
    FinishSignIn(Value),
    SignOut,
    Shown(CompletionItem),
    Accepted(CompletionItem),
    Respond { id: Value, result: Value },
}

#[derive(Debug)]
pub(crate) enum Event {
    Status(String),
    SignIn {
        user_code: String,
        command: Value,
    },
    Message {
        id: Value,
        message: String,
        actions: Vec<Value>,
    },
    Completion {
        snapshot: Snapshot,
        items: Vec<CompletionItem>,
    },
    Stopped(String),
}

pub(crate) struct Bridge {
    desired: watch::Sender<Option<CompletionRequest>>,
    controls: mpsc::Sender<Control>,
    events: mpsc::Receiver<Event>,
    worker: JoinHandle<()>,
}

impl Bridge {
    #[cfg(test)]
    pub(crate) fn test_channels() -> (
        Self,
        watch::Receiver<Option<CompletionRequest>>,
        mpsc::Receiver<Control>,
        mpsc::Sender<Event>,
    ) {
        let (desired, requests) = watch::channel(None);
        let (controls, commands) = mpsc::channel(32);
        let (events_tx, events) = mpsc::channel(32);
        let worker = tokio::spawn(std::future::pending());
        (
            Self {
                desired,
                controls,
                events,
                worker,
            },
            requests,
            commands,
            events_tx,
        )
    }
    pub fn start(config: CopilotConfig, root: PathBuf) -> Self {
        let (desired, requests) = watch::channel(None);
        let (controls, commands) = mpsc::channel(32);
        let (events_tx, events) = mpsc::channel(32);
        let worker = tokio::spawn(async move {
            let result = run(config, root, requests, commands, events_tx.clone()).await;
            let message = match result {
                Ok(()) => "Copilot stopped".to_string(),
                Err(error) => format!("Copilot: {error:#}"),
            };
            let _ = events_tx.send(Event::Stopped(message)).await;
        });
        Self {
            desired,
            controls,
            events,
            worker,
        }
    }

    pub fn request(&self, request: CompletionRequest) {
        self.desired.send_replace(Some(request));
    }
    pub fn cancel(&self) {
        self.desired.send_replace(None);
    }
    pub fn control(&self, command: Control) -> Result<()> {
        self.controls
            .try_send(command)
            .map_err(|_| anyhow!("Copilot is busy or stopped"))
    }
    pub fn poll(&mut self) -> Option<Event> {
        self.events.try_recv().ok()
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

struct ReaderTask(JoinHandle<()>);
impl Drop for ReaderTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Document {
    uri: String,
    contents: String,
    version: u64,
}
enum Pending {
    Initialize,
    SignIn,
    Completion(Snapshot),
    Other,
}

struct Protocol<W> {
    writer: W,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    completion: Option<(u64, tokio::time::Instant)>,
    document: Option<Document>,
}

impl<W: AsyncWrite + Unpin> Protocol<W> {
    async fn send(&mut self, value: Value) -> Result<()> {
        let body = serde_json::to_vec(&value)?;
        self.writer
            .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
            .await?;
        self.writer.write_all(&body).await?;
        self.writer.flush().await?;
        Ok(())
    }
    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.send(json!({"jsonrpc":"2.0", "method":method, "params":params}))
            .await
    }
    async fn request(&mut self, method: &str, params: Value, pending: Pending) -> Result<u64> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))
            .await?;
        if !matches!(pending, Pending::Other) {
            self.pending.insert(id, pending);
        }
        Ok(id)
    }
    async fn respond(&mut self, id: Value, result: Value) -> Result<()> {
        self.send(json!({"jsonrpc":"2.0", "id":id, "result":result}))
            .await
    }
    async fn cancel(&mut self) -> Result<()> {
        if let Some((id, _)) = self.completion.take() {
            self.pending.remove(&id);
            self.notify("$/cancelRequest", json!({"id":id})).await?;
        }
        Ok(())
    }
    async fn complete(&mut self, request: CompletionRequest) -> Result<()> {
        self.cancel().await?;
        let uri = &request.snapshot.uri;
        if self
            .document
            .as_ref()
            .is_some_and(|document| &document.uri != uri)
        {
            let old = self.document.take().expect("document exists");
            self.notify(
                "textDocument/didClose",
                json!({"textDocument":{"uri":old.uri}}),
            )
            .await?;
        }
        if let Some(document) = self.document.as_ref() {
            if document.contents != request.contents {
                let end = end_position(&document.contents);
                let version = document.version + 1;
                self.notify("textDocument/didChange", json!({
                    "textDocument":{"uri":uri,"version":version},
                    "contentChanges":[{"range":{"start":{"line":0,"character":0},"end":end},"text":request.contents}]
                })).await?;
                self.document = Some(Document {
                    uri: uri.clone(),
                    contents: request.contents,
                    version,
                });
            }
        } else {
            self.notify(
                "textDocument/didOpen",
                json!({"textDocument":{
                    "uri":uri,"languageId":request.language_id,"version":1,"text":request.contents
                }}),
            )
            .await?;
            self.document = Some(Document {
                uri: uri.clone(),
                contents: request.contents,
                version: 1,
            });
        }
        self.notify("textDocument/didFocus", json!({"textDocument":{"uri":uri}}))
            .await?;
        let version = self
            .document
            .as_ref()
            .expect("document was synchronized")
            .version;
        let id = self.request("textDocument/inlineCompletion", json!({
            "textDocument":{"uri":uri,"version":version},"position":request.position,
            "context":{"triggerKind":if request.automatic {2} else {1}},
            "formattingOptions":{"tabSize":request.tab_size,"insertSpaces":request.insert_spaces}
        }), Pending::Completion(request.snapshot)).await?;
        self.completion = Some((id, tokio::time::Instant::now() + REQUEST_TIMEOUT));
        Ok(())
    }
    async fn control(&mut self, control: Control) -> Result<()> {
        match control {
            Control::SignIn => {
                self.request("signIn", json!({}), Pending::SignIn).await?;
            }
            Control::SignOut => {
                self.cancel().await?;
                self.request("signOut", json!({}), Pending::Other).await?;
            }
            Control::FinishSignIn(command) => {
                if command.get("command").and_then(Value::as_str)
                    == Some("github.copilot.finishDeviceFlow")
                {
                    self.request("workspace/executeCommand", command, Pending::Other)
                        .await?;
                }
            }
            Control::Shown(item) => {
                self.notify("textDocument/didShowCompletion", json!({"item":item}))
                    .await?
            }
            Control::Accepted(item) => {
                if let Some(command) = item.command.filter(|command| {
                    command.get("command").and_then(Value::as_str)
                        == Some("github.copilot.didAcceptCompletionItem")
                }) {
                    self.request("workspace/executeCommand", command, Pending::Other)
                        .await?;
                }
            }
            Control::Respond { id, result } => self.respond(id, result).await?,
        }
        Ok(())
    }
}

fn end_position(text: &str) -> Position {
    let line = text.bytes().filter(|byte| *byte == b'\n').count();
    let tail = text.rsplit('\n').next().unwrap_or_default();
    Position {
        line,
        character: tail.encode_utf16().count(),
    }
}

async fn run(
    config: CopilotConfig,
    root: PathBuf,
    mut requests: watch::Receiver<Option<CompletionRequest>>,
    mut controls: mpsc::Receiver<Control>,
    events: mpsc::Sender<Event>,
) -> Result<()> {
    let mut child = Command::new(&config.command).args(&config.args).current_dir(&root)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true)
        .spawn().with_context(|| format!("could not start {}; install @github/copilot-language-server or configure [copilot].command", config.command))?;
    let stdout = child.stdout.take().context("missing Copilot stdout")?;
    let writer = child.stdin.take().context("missing Copilot stdin")?;
    let (messages_tx, mut messages) = mpsc::channel(32);
    let _reader = ReaderTask(tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        loop {
            let result = match crate::lsp::client::read_lsp_frame(&mut reader).await {
                Ok(Some(body)) => {
                    serde_json::from_slice::<Value>(&body).map_err(anyhow::Error::from)
                }
                Ok(None) => break,
                Err(error) => Err(error.into()),
            };
            let failed = result.is_err();
            if messages_tx.send(result).await.is_err() || failed {
                break;
            }
        }
    }));
    let mut protocol = Protocol {
        writer,
        next_id: 0,
        pending: HashMap::new(),
        completion: None,
        document: None,
    };
    protocol.request("initialize",json!({
        "processId":std::process::id(),"rootUri":file_uri(&root)?,
        "workspaceFolders":[{"uri":file_uri(&root)?,"name":root.file_name().unwrap_or_default().to_string_lossy()}],
        "capabilities":{"workspace":{"workspaceFolders":true,"configuration":true}},
        "initializationOptions":{"editorInfo":{"name":"Red","version":env!("CARGO_PKG_VERSION")},"editorPluginInfo":{"name":"Red Copilot","version":env!("CARGO_PKG_VERSION")}}
    }),Pending::Initialize).await?;
    let mut initialized = false;
    let mut deferred_controls = Vec::new();
    let initialize_deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            changed = requests.changed(), if initialized => {
                if changed.is_err() { break; }
                let request = requests.borrow_and_update().clone();
                if let Some(request) = request { protocol.complete(request).await?; }
                else { protocol.cancel().await?; }
            }
            command = controls.recv() => {
                let Some(command) = command else { break; };
                if initialized { protocol.control(command).await?; }
                else if deferred_controls.len() < 32 { deferred_controls.push(command); }
            }
            message = messages.recv() => {
                let value = message.context("language server closed its output")??;
                if handle_message(&mut protocol, value, &events).await? {
                    initialized = true;
                    for control in deferred_controls.drain(..) {
                        protocol.control(control).await?;
                    }
                }
            }
            _ = tick.tick() => {
                if !initialized && tokio::time::Instant::now()>=initialize_deadline {bail!("initialization timed out");}
                if protocol.completion.is_some_and(|(_,deadline)|tokio::time::Instant::now()>=deadline) {protocol.cancel().await?;}
                if let Some(status)=child.try_wait()? {bail!("language server exited with {status}");}
            }
        }
    }
    Ok(())
}

fn provider_settings() -> Value {
    json!({"telemetry": {"telemetryLevel": "off"}})
}

/// Returns true only after processing a successful initialization response.
async fn handle_message<W: AsyncWrite + Unpin>(
    protocol: &mut Protocol<W>,
    value: Value,
    events: &mpsc::Sender<Event>,
) -> Result<bool> {
    if let Some(method) = value.get("method").and_then(Value::as_str) {
        let params = &value["params"];
        if let Some(id) = value.get("id") {
            match method {
                "workspace/configuration" => {
                    let settings = provider_settings();
                    let result = params["items"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|item| match item["section"].as_str() {
                            Some("telemetry") => settings["telemetry"].clone(),
                            Some("telemetry.telemetryLevel") => json!("off"),
                            None => settings.clone(),
                            _ => Value::Null,
                        })
                        .collect::<Vec<_>>();
                    protocol.respond(id.clone(), json!(result)).await?;
                }
                "window/showMessageRequest" => {
                    events
                        .send(Event::Message {
                            id: id.clone(),
                            message: bounded_message(
                                params["message"].as_str().unwrap_or("Copilot notification"),
                            ),
                            actions: params["actions"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .take(16)
                                .cloned()
                                .collect(),
                        })
                        .await?;
                }
                "window/showDocument" => {
                    protocol
                        .respond(id.clone(), json!({"success": false}))
                        .await?;
                }
                "workspace/applyEdit" => {
                    protocol
                        .respond(
                            id.clone(),
                            json!({
                                "applied": false,
                                "failureReason": "Copilot edits require explicit user acceptance",
                            }),
                        )
                        .await?;
                }
                _ => {
                    protocol
                        .send(json!({
                            "jsonrpc": "2.0", "id": id,
                            "error": {"code": -32601, "message": "unsupported client request"},
                        }))
                        .await?;
                }
            }
        } else if matches!(method, "didChangeStatus" | "window/showMessage") {
            let message = params["message"]
                .as_str()
                .filter(|text| !text.is_empty())
                .or_else(|| params["kind"].as_str())
                .unwrap_or("Ready");
            events.send(Event::Status(bounded_message(message))).await?;
        }
        return Ok(false);
    }
    let Some(id) = value["id"].as_u64() else {
        return Ok(false);
    };
    let Some(pending) = protocol.pending.remove(&id) else {
        return Ok(false);
    };
    if protocol
        .completion
        .is_some_and(|(current, _)| current == id)
    {
        protocol.completion = None;
    }
    if !value["error"].is_null() {
        let message = bounded_message(
            value["error"]["message"]
                .as_str()
                .unwrap_or("request failed"),
        );
        if matches!(pending, Pending::Initialize) {
            bail!("initialization failed: {message}");
        }
        events.send(Event::Status(message)).await?;
        return Ok(false);
    }
    let result = &value["result"];
    match pending {
        Pending::Initialize => {
            protocol.notify("initialized", json!({})).await?;
            protocol
                .notify(
                    "workspace/didChangeConfiguration",
                    json!({"settings": provider_settings()}),
                )
                .await?;
            events.send(Event::Status("Ready".into())).await?;
            return Ok(true);
        }
        Pending::SignIn => {
            if let Some(code) = result["userCode"].as_str() {
                events
                    .send(Event::SignIn {
                        user_code: bounded_message(code),
                        command: result["command"].clone(),
                    })
                    .await?;
            } else {
                events
                    .send(Event::Status("Already signed in".into()))
                    .await?;
            }
        }
        Pending::Completion(snapshot) => {
            let items = result["items"]
                .as_array()
                .into_iter()
                .flatten()
                .take(8)
                .filter_map(|item| serde_json::from_value::<CompletionItem>(item.clone()).ok())
                .filter(|item| item.insert_text.len() <= MAX_SUGGESTION_BYTES)
                .collect();
            events.send(Event::Completion { snapshot, items }).await?;
        }
        Pending::Other => {}
    }
    Ok(false)
}

fn bounded_message(message: &str) -> String {
    message
        .chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
        .take(1000)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn privacy_defaults_and_invalid_patterns_fail_closed() {
        let mut config = CopilotConfig::default();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let root = root.as_path();
        assert!(!config.enabled);
        assert!(config.allows(root, &root.join("main.rs"), 100));
        assert!(!config.allows(root, &root.join(".env.local"), 100));
        assert!(!config.allows(root, Path::new("/outside/main.rs"), 100));
        assert!(!config.allows(root, &root.join("main.rs"), config.max_file_bytes + 1));
        config.excluded_patterns.push("[".into());
        assert!(!config.allows(root, &root.join("main.rs"), 100));
    }
    #[test]
    fn document_uri_paths_preserve_workspace_privacy() {
        let config = CopilotConfig::default();
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join(".env.local"), "secret").unwrap();
        let document_path = |path: &Path| {
            PathBuf::from(crate::lsp::normalized_file_path(&file_uri(path).unwrap()).unwrap())
        };
        let uri_root = document_path(&root);
        let existing = document_path(&root.join("src/main.rs"));
        let unsaved = document_path(&root.join("src/new.rs"));
        let excluded = document_path(&root.join(".env.local"));
        let outside_file = document_path(&outside.path().join("main.rs"));
        for workspace in [&root, &uri_root] {
            assert!(
                config.allows(workspace, &existing, 100),
                "{workspace:?}: {existing:?}"
            );
            assert!(config.allows(workspace, &unsaved, 100));
            assert!(!config.allows(workspace, &excluded, 100));
            assert!(!config.allows(workspace, &outside_file, 100));
            assert!(!config.allows(workspace, &existing, config.max_file_bytes + 1));
        }
    }

    #[cfg(unix)]
    #[test]
    fn workspace_aliases_keep_lexical_and_resolved_exclusions() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("workspace");
        let alias = directory.path().join("alias");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("main.rs"), "source").unwrap();
        std::fs::write(root.join(".env.local"), "secret").unwrap();
        symlink(&root, &alias).unwrap();
        symlink(root.join("main.rs"), root.join(".env")).unwrap();
        symlink(root.join(".env.local"), root.join("looks-safe.rs")).unwrap();
        let config = CopilotConfig::default();
        assert!(config.allows(&alias, &alias.join("main.rs"), 6));
        assert!(config.allows(&alias, &alias.join("new.rs"), 6));
        assert!(!config.allows(&alias, &alias.join(".env"), 6));
        assert!(!config.allows(&alias, &alias.join("looks-safe.rs"), 6));
    }

    #[test]
    fn document_end_uses_utf16() {
        assert_eq!(
            end_position("a\r\n😀"),
            Position {
                line: 1,
                character: 2
            }
        );
        assert_eq!(
            end_position("a\n"),
            Position {
                line: 1,
                character: 0
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_bypass_exclusions_or_workspace_boundary() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join(".env"), "secret").unwrap();
        std::fs::write(outside.path().join("source.rs"), "private").unwrap();
        symlink(root.join(".env"), root.join("looks-safe.rs")).unwrap();
        symlink(outside.path().join("source.rs"), root.join("outside.rs")).unwrap();
        let config = CopilotConfig::default();
        assert!(!config.allows(&root, &root.join("looks-safe.rs"), 6));
        assert!(!config.allows(&root, &root.join("outside.rs"), 7));
    }

    fn request(generation: u64, uri: &str, text: &str) -> CompletionRequest {
        CompletionRequest {
            snapshot: Snapshot {
                generation,
                buffer_id: crate::buffer::Buffer::new(None, String::new()).id(),
                revision: generation,
                cursor: TextPosition::new(0, 0),
                uri: uri.into(),
            },
            language_id: "rust".into(),
            contents: text.into(),
            position: Position {
                line: 0,
                character: 0,
            },
            tab_size: 4,
            insert_spaces: true,
            automatic: true,
        }
    }

    async fn read(reader: &mut BufReader<tokio::io::DuplexStream>) -> Value {
        let body = crate::lsp::client::read_lsp_frame(reader)
            .await
            .unwrap()
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn protocol_syncs_cancels_and_closes_documents() {
        let (writer, reader) = tokio::io::duplex(64 * 1024);
        let mut reader = BufReader::new(reader);
        let mut protocol = Protocol {
            writer,
            next_id: 0,
            pending: HashMap::new(),
            completion: None,
            document: None,
        };
        protocol
            .complete(request(1, "file:///workspace/a.rs", "😀"))
            .await
            .unwrap();
        assert_eq!(read(&mut reader).await["method"], "textDocument/didOpen");
        assert_eq!(read(&mut reader).await["method"], "textDocument/didFocus");
        let first = read(&mut reader).await;
        assert_eq!(first["method"], "textDocument/inlineCompletion");
        assert_eq!(first["params"]["textDocument"]["version"], 1);
        protocol
            .complete(request(2, "file:///workspace/a.rs", "😀x"))
            .await
            .unwrap();
        let cancel = read(&mut reader).await;
        assert_eq!(cancel["method"], "$/cancelRequest");
        assert_eq!(cancel["params"]["id"], first["id"]);
        let change = read(&mut reader).await;
        assert_eq!(
            change["params"]["contentChanges"][0]["range"]["end"]["character"],
            2
        );
        assert_eq!(change["params"]["textDocument"]["version"], 2);
        assert_eq!(read(&mut reader).await["method"], "textDocument/didFocus");
        assert_eq!(
            read(&mut reader).await["method"],
            "textDocument/inlineCompletion"
        );
        protocol
            .complete(request(3, "file:///workspace/b.rs", "fn b() {}"))
            .await
            .unwrap();
        assert_eq!(read(&mut reader).await["method"], "$/cancelRequest");
        assert_eq!(read(&mut reader).await["method"], "textDocument/didClose");
        assert_eq!(read(&mut reader).await["method"], "textDocument/didOpen");
        assert_eq!(read(&mut reader).await["method"], "textDocument/didFocus");
        assert_eq!(
            read(&mut reader).await["params"]["textDocument"]["version"],
            1
        );
        assert_eq!(protocol.pending.len(), 1);
    }

    #[tokio::test]
    async fn completion_commands_are_allowlisted() {
        let (writer, reader) = tokio::io::duplex(4096);
        let mut reader = BufReader::new(reader);
        let mut protocol = Protocol {
            writer,
            next_id: 0,
            pending: HashMap::new(),
            completion: None,
            document: None,
        };
        let mut item = CompletionItem {
            insert_text: "hello".into(),
            range: None,
            command: Some(json!({"command":"dangerous.command"})),
            extra: serde_json::Map::from_iter([("data".into(), json!({"id":"opaque"}))]),
        };
        protocol
            .control(Control::Accepted(item.clone()))
            .await
            .unwrap();
        assert!(protocol.pending.is_empty());
        item.command =
            Some(json!({"command":"github.copilot.didAcceptCompletionItem","arguments":["id"]}));
        protocol
            .control(Control::Shown(item.clone()))
            .await
            .unwrap();
        let shown = read(&mut reader).await;
        assert_eq!(shown["method"], "textDocument/didShowCompletion");
        assert_eq!(shown["params"]["item"]["data"]["id"], "opaque");
        protocol.control(Control::Accepted(item)).await.unwrap();
        assert_eq!(
            read(&mut reader).await["params"]["command"],
            "github.copilot.didAcceptCompletionItem"
        );
    }

    #[tokio::test]
    async fn initialization_authentication_and_server_requests_follow_protocol() {
        let (writer, reader) = tokio::io::duplex(8192);
        let mut reader = BufReader::new(reader);
        let mut protocol = Protocol {
            writer,
            next_id: 0,
            pending: HashMap::new(),
            completion: None,
            document: None,
        };
        let (events, mut received) = mpsc::channel(8);
        let id = protocol
            .request("initialize", json!({}), Pending::Initialize)
            .await
            .unwrap();
        assert_eq!(read(&mut reader).await["method"], "initialize");
        assert!(handle_message(
            &mut protocol,
            json!({"id":id,"result":{"capabilities":{}}}),
            &events
        )
        .await
        .unwrap());
        assert_eq!(read(&mut reader).await["method"], "initialized");
        let configuration = read(&mut reader).await;
        assert_eq!(
            configuration["params"]["settings"]["telemetry"]["telemetryLevel"],
            "off"
        );
        assert!(matches!(received.recv().await, Some(Event::Status(_))));
        protocol.control(Control::SignIn).await.unwrap();
        let signin = read(&mut reader).await;
        let command = json!({"command":"github.copilot.finishDeviceFlow","arguments":[]});
        handle_message(
            &mut protocol,
            json!({"id":signin["id"],"result":{"userCode":"ABCD-EFGH","command":command}}),
            &events,
        )
        .await
        .unwrap();
        assert!(
            matches!(received.recv().await,Some(Event::SignIn{user_code,..}) if user_code=="ABCD-EFGH")
        );
        handle_message(
            &mut protocol,
            json!({"id":"edit","method":"workspace/applyEdit","params":{}}),
            &events,
        )
        .await
        .unwrap();
        assert_eq!(read(&mut reader).await["result"]["applied"], false);
        handle_message(&mut protocol,json!({"id":"message","method":"window/showMessageRequest","params":{"message":"Account issue","actions":[{"title":"First"},{"title":"Second"}]}}),&events).await.unwrap();
        assert!(
            matches!(received.recv().await,Some(Event::Message{actions,..}) if actions.len()==2)
        );
    }
}
