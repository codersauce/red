//! Discovery of persisted Codex conversations for Red's session picker.

use super::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::{BufRead as _, BufReader},
    time::SystemTime,
};

const PAGE_LIMIT: usize = 100;
const MAX_SESSIONS: usize = 4096;
const MAX_PAGES: usize = 100;
const MAX_SESSION_METADATA_BYTES: u64 = 64 * 1024;
const MAX_SESSION_PREVIEW_CHARS: usize = 12 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSessionScope {
    Red,
    Workspace,
    All,
}

#[derive(Debug, Clone)]
pub struct AgentSessionListRequest {
    pub cwd: PathBuf,
    pub scope: AgentSessionScope,
    pub query: String,
    pub exclude_session_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AgentSessionPreviewRequest {
    pub cwd: PathBuf,
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionSummary {
    pub id: String,
    pub cwd: String,
    pub label: String,
    pub detail: String,
    pub snippet: String,
    pub source: String,
    pub time_group: String,
    pub is_red: bool,
    pub updated_at: i64,
}

pub(super) struct PendingSessionRequest {
    request_id: i64,
    request: AgentSessionListRequest,
    threads: Vec<(Value, String)>,
    cursors: HashSet<String>,
}

impl PendingSessionRequest {
    pub(super) fn request_id(&self) -> i64 {
        self.request_id
    }
}

async fn complete(
    events: &mpsc::Sender<CodexEvent>,
    request_id: i64,
    result: std::result::Result<Value, String>,
) {
    events
        .send(CodexEvent::SessionListCompleted { request_id, result })
        .await
        .ok();
}

pub(super) fn session_preview(session_id: &str, message: &Value) -> Result<Value, String> {
    if let Some(error) = message.get("error") {
        return Err(error["message"]
            .as_str()
            .unwrap_or("Codex could not load the session preview")
            .to_string());
    }
    let thread = message
        .pointer("/result/thread")
        .filter(|thread| thread.is_object())
        .ok_or_else(|| "Codex returned an invalid session preview".to_string())?;
    if thread["id"].as_str() != Some(session_id) {
        return Err("Codex returned a preview for a different session".to_string());
    }
    let turns = thread["turns"]
        .as_array()
        .ok_or_else(|| "Codex returned a session without conversation history".to_string())?;
    for turn in turns.iter().rev() {
        let Some(items) = turn["items"].as_array() else {
            continue;
        };
        let prompt = items
            .iter()
            .rev()
            .find(|item| item["type"].as_str() == Some("userMessage"))
            .map(preview_item_text)
            .unwrap_or_default();
        let response = items
            .iter()
            .rev()
            .find(|item| item["type"].as_str() == Some("agentMessage"))
            .map(preview_item_text)
            .unwrap_or_default();
        if !prompt.is_empty() || !response.is_empty() {
            return Ok(json!({
                "session_id": session_id,
                "prompt": prompt,
                "response": response,
            }));
        }
    }
    Ok(json!({"session_id": session_id, "prompt": "", "response": ""}))
}

fn preview_item_text(item: &Value) -> String {
    let text = item["text"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| {
            item["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        });
    text.trim()
        .chars()
        .take(MAX_SESSION_PREVIEW_CHARS)
        .collect()
}

fn interactive_source_kinds(scope: AgentSessionScope) -> Value {
    match scope {
        AgentSessionScope::Red => json!(["appServer", "vscode"]),
        AgentSessionScope::Workspace | AgentSessionScope::All => {
            json!(["cli", "vscode", "appServer", "unknown"])
        }
    }
}

async fn list_page(
    state: PendingSessionRequest,
    cursor: Option<String>,
    input: &mut (impl AsyncWrite + Unpin),
    pending: &mut HashMap<String, Pending>,
    next_id: &mut u64,
) -> Result<()> {
    let id = rpc_id(next_id);
    let query = state.request.query.trim();
    let (method, mut params) = if query.is_empty() {
        (
            "thread/list",
            json!({
                "limit": PAGE_LIMIT,
                "cursor": cursor,
                "archived": false,
                "sortKey": "updated_at",
                "sortDirection": "desc",
                "modelProviders": [],
                "sourceKinds": interactive_source_kinds(state.request.scope),
            }),
        )
    } else {
        (
            "thread/search",
            json!({
                "searchTerm": query,
                "limit": PAGE_LIMIT,
                "cursor": cursor,
                "archived": false,
                "sortKey": "updated_at",
                "sortDirection": "desc",
                "sourceKinds": interactive_source_kinds(state.request.scope),
            }),
        )
    };
    if query.is_empty() && state.request.scope == AgentSessionScope::Workspace {
        params["cwd"] = json!(state.request.cwd);
    }
    pending.insert(id.clone(), Pending::Sessions(state));
    write_message(
        input,
        &json!({"id": id, "method": method, "params": params}),
    )
    .await
}

pub(super) async fn handle_command(
    request_id: i64,
    request: AgentSessionListRequest,
    input: &mut (impl AsyncWrite + Unpin),
    pending: &mut HashMap<String, Pending>,
    next_id: &mut u64,
) -> Result<()> {
    list_page(
        PendingSessionRequest {
            request_id,
            request,
            threads: Vec::new(),
            cursors: HashSet::new(),
        },
        None,
        input,
        pending,
        next_id,
    )
    .await
}

pub(super) async fn handle_response(
    request: PendingSessionRequest,
    message: Value,
    input: &mut (impl AsyncWrite + Unpin),
    events: &mpsc::Sender<CodexEvent>,
    pending: &mut HashMap<String, Pending>,
    next_id: &mut u64,
) -> Result<()> {
    let request_id = request.request_id();
    if let Some(error) = message.get("error") {
        complete(
            events,
            request_id,
            Err(error["message"]
                .as_str()
                .unwrap_or("Codex session search failed")
                .to_string()),
        )
        .await;
        return Ok(());
    }
    let Some(page) = message.pointer("/result/data").and_then(Value::as_array) else {
        complete(
            events,
            request_id,
            Err("Codex returned an invalid session list".into()),
        )
        .await;
        return Ok(());
    };
    let searching = !request.request.query.trim().is_empty();
    let mut threads = request.threads;
    for entry in page {
        let (thread, snippet) = if searching {
            (&entry["thread"], clean_text(&entry["snippet"]))
        } else {
            (entry, String::new())
        };
        let Some(id) = thread["id"].as_str().filter(|id| !id.is_empty()) else {
            continue;
        };
        if !threads
            .iter()
            .any(|(existing, _)| existing["id"].as_str() == Some(id))
        {
            threads.push((thread.clone(), snippet));
        }
    }
    if let Some(cursor) = message
        .pointer("/result/nextCursor")
        .and_then(Value::as_str)
        .filter(|cursor| !cursor.is_empty())
    {
        let mut cursors = request.cursors;
        if threads.len() > MAX_SESSIONS
            || cursors.len() >= MAX_PAGES
            || !cursors.insert(cursor.to_string())
        {
            complete(
                events,
                request_id,
                Err("Codex session-list pagination did not finish".into()),
            )
            .await;
        } else {
            list_page(
                PendingSessionRequest {
                    request_id,
                    request: request.request,
                    threads,
                    cursors,
                },
                Some(cursor.to_string()),
                input,
                pending,
                next_id,
            )
            .await?;
        }
        return Ok(());
    }

    let mut sessions = threads
        .into_iter()
        .filter_map(|(thread, snippet)| summarize(&request.request, &thread, snippet))
        .collect::<Vec<_>>();
    sessions.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| right.id.cmp(&left.id))
    });
    complete(events, request_id, Ok(json!({"sessions": sessions}))).await;
    Ok(())
}

fn summarize(
    request: &AgentSessionListRequest,
    thread: &Value,
    snippet: String,
) -> Option<AgentSessionSummary> {
    if thread["ephemeral"].as_bool() == Some(true) {
        return None;
    }
    let id = thread["id"].as_str()?.to_string();
    if request.exclude_session_id.as_deref() == Some(id.as_str()) {
        return None;
    }
    let cwd = thread["cwd"].as_str().unwrap_or_default().to_string();
    let is_red = is_red_thread(thread);
    if request.scope == AgentSessionScope::Red && !is_red {
        return None;
    }
    if request.scope == AgentSessionScope::Workspace && !same_path(&request.cwd, Path::new(&cwd)) {
        return None;
    }
    let source = source_label(&thread["source"], is_red);
    let updated_at = thread["updatedAt"].as_i64().unwrap_or_default();
    let label = nonempty(&thread["name"])
        .or_else(|| nonempty(&thread["preview"]))
        .map(|value| clean_text(&Value::String(value)))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Untitled session".to_string());
    let location = Path::new(&cwd)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(cwd.as_str());
    let age = session_age(updated_at);
    let detail = format!("{} · {source} · {location}", relative_time(age));
    Some(AgentSessionSummary {
        id,
        cwd,
        label,
        detail,
        snippet,
        source,
        time_group: session_time_group(age).to_string(),
        is_red,
        updated_at,
    })
}

fn is_red_thread(thread: &Value) -> bool {
    if thread["threadSource"].as_str() == Some("red") {
        return true;
    }
    if thread["source"].as_str() != Some("vscode") {
        return false;
    }

    let Some(path) = thread["path"].as_str() else {
        return false;
    };
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut line = String::new();
    if BufReader::new(std::io::Read::take(file, MAX_SESSION_METADATA_BYTES))
        .read_line(&mut line)
        .is_err()
    {
        return false;
    }
    let Ok(metadata) = serde_json::from_str::<Value>(&line) else {
        return false;
    };
    metadata["type"].as_str() == Some("session_meta")
        && metadata.pointer("/payload/id").and_then(Value::as_str) == thread["id"].as_str()
        && metadata
            .pointer("/payload/originator")
            .and_then(Value::as_str)
            == Some("red")
}

fn nonempty(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn clean_text(value: &Value) -> String {
    value
        .as_str()
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn source_label(source: &Value, is_red: bool) -> String {
    if is_red {
        return "Red".to_string();
    }
    match source.as_str() {
        Some("cli") => "Codex CLI".to_string(),
        Some("vscode") => "VS Code".to_string(),
        Some("appServer") => "Codex app".to_string(),
        Some("exec") => "Codex exec".to_string(),
        Some("unknown") | None => source
            .get("custom")
            .and_then(Value::as_str)
            .unwrap_or("Codex")
            .to_string(),
        Some(other) => other.to_string(),
    }
}

fn session_age(timestamp: i64) -> i64 {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(timestamp);
    now.saturating_sub(timestamp).max(0)
}

fn session_time_group(seconds: i64) -> &'static str {
    match seconds {
        0..=86_399 => "Recent",
        86_400..=604_799 => "This week",
        604_800..=2_591_999 => "This month",
        _ => "Earlier",
    }
}

fn relative_time(seconds: i64) -> String {
    match seconds {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86_399 => format!("{}h ago", seconds / 3600),
        86_400..=2_591_999 => format!("{}d ago", seconds / 86_400),
        _ => format!("{}mo ago", seconds / 2_592_000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_filters_scope_and_formats_red_origin() {
        let cwd = PathBuf::from("/work/red");
        let request = AgentSessionListRequest {
            cwd: cwd.clone(),
            scope: AgentSessionScope::Red,
            query: String::new(),
            exclude_session_id: Some("current".into()),
        };
        let mut thread = json!({
            "id": "other",
            "cwd": cwd,
            "ephemeral": false,
            "threadSource": "red",
            "source": "appServer",
            "name": null,
            "preview": "  Fix   the picker  ",
            "updatedAt": 1,
        });
        let summary = summarize(&request, &thread, String::new()).unwrap();
        assert_eq!(summary.label, "Fix the picker");
        assert_eq!(summary.source, "Red");
        assert_eq!(summary.time_group, "Earlier");
        assert!(summary.is_red);

        thread["source"] = json!("vscode");
        let cli_summary = summarize(&request, &thread, String::new()).unwrap();
        assert_eq!(cli_summary.label, "Fix the picker");
        assert_eq!(cli_summary.source, "Red");
        assert!(cli_summary.is_red);
    }

    #[test]
    fn session_time_groups_follow_relative_age_boundaries() {
        assert_eq!(session_time_group(0), "Recent");
        assert_eq!(session_time_group(86_399), "Recent");
        assert_eq!(session_time_group(86_400), "This week");
        assert_eq!(session_time_group(604_799), "This week");
        assert_eq!(session_time_group(604_800), "This month");
        assert_eq!(session_time_group(2_591_999), "This month");
        assert_eq!(session_time_group(2_592_000), "Earlier");
    }

    #[test]
    fn preview_uses_the_latest_turn_without_opening_or_changing_a_thread() {
        let response = json!({"result":{"thread":{
            "id":"thread",
            "turns":[
                {"items":[
                    {"type":"userMessage","content":[{"type":"text","text":"Old question"}]},
                    {"type":"agentMessage","text":"Old answer"}
                ]},
                {"items":[
                    {"type":"userMessage","content":[{"type":"text","text":"Latest question"}]},
                    {"type":"agentMessage","text":"Latest answer"}
                ]}
            ]
        }}});
        let preview = session_preview("thread", &response).unwrap();
        assert_eq!(preview["prompt"], "Latest question");
        assert_eq!(preview["response"], "Latest answer");
        assert!(session_preview("different", &response).is_err());
    }

    #[test]
    fn preview_keeps_an_unanswered_latest_prompt() {
        let response = json!({"result":{"thread":{
            "id":"thread",
            "turns":[{"items":[
                {"type":"userMessage","content":[{"type":"text","text":"Still running"}]}
            ]}]
        }}});
        let preview = session_preview("thread", &response).unwrap();
        assert_eq!(preview["prompt"], "Still running");
        assert_eq!(preview["response"], "");
    }

    #[test]
    fn workspace_scope_rejects_threads_from_other_roots() {
        let request = AgentSessionListRequest {
            cwd: PathBuf::from("/work/red"),
            scope: AgentSessionScope::Workspace,
            query: String::new(),
            exclude_session_id: None,
        };
        let thread = json!({
            "id": "other",
            "cwd": "/work/codex",
            "ephemeral": false,
            "threadSource": "user",
            "source": "cli",
            "preview": "Different root",
            "updatedAt": 1,
        });
        assert!(summarize(&request, &thread, String::new()).is_none());
    }

    #[test]
    fn red_scope_recovers_legacy_vscode_sessions_from_rollout_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rollout.jsonl");
        std::fs::write(
            &path,
            format!(
                "{}\n",
                json!({
                    "type": "session_meta",
                    "payload": {"id": "legacy-red", "originator": "red"}
                })
            ),
        )
        .unwrap();
        let request = AgentSessionListRequest {
            cwd: PathBuf::from("/work/red"),
            scope: AgentSessionScope::Red,
            query: String::new(),
            exclude_session_id: None,
        };
        let mut thread = json!({
            "id": "legacy-red",
            "cwd": "/work/red",
            "ephemeral": false,
            "threadSource": null,
            "source": "vscode",
            "path": path,
            "preview": "Earlier Red session",
            "updatedAt": 1
        });

        let session = summarize(&request, &thread, String::new()).unwrap();
        assert!(session.is_red);
        assert_eq!(session.source, "Red");

        thread["id"] = json!("different-session");
        assert!(summarize(&request, &thread, String::new()).is_none());
    }
}
