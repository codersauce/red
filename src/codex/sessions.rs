//! Discovery of persisted Codex conversations for Red's session picker.

use super::*;
use crate::agent_conversation::{typed_prompt, EDITOR_CONTEXT_PREFIX, RECAP_PREFIX};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::{BufRead as _, BufReader},
    time::SystemTime,
};

const PAGE_LIMIT: usize = 100;
const MAX_SESSIONS: usize = 4096;
/// Sessions shown at once. Searching reaches older ones, and a long list costs
/// the plugin more to render than a person will scroll through.
const MAX_LISTED_SESSIONS: usize = 200;
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
    /// Also search conversation text, which Codex does by scanning every
    /// rollout and can take many seconds. Otherwise a query matches titles.
    pub search_contents: bool,
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
    /// A Red thread from this workspace rejoins as itself; anything else
    /// continues in a new thread here.
    pub resumable: bool,
    pub updated_at: i64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ListPhase {
    /// `thread/list`, filtered by title when there is a query.
    Titles,
    /// `thread/search` over conversation text.
    Contents,
}

pub(super) struct PendingSessionRequest {
    request_id: i64,
    request: AgentSessionListRequest,
    phase: ListPhase,
    /// Every thread id this phase has seen, listed or not.
    seen: HashSet<String>,
    sessions: Vec<AgentSessionSummary>,
    cursors: HashSet<String>,
    cursor: Option<String>,
    /// Threads per page. Older Codex threads can carry a whole pasted prompt as
    /// their preview, so a page too large to read is retried at half the size.
    limit: usize,
    /// Matching threads were left out.
    more: bool,
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
                .filter(|text| !text.starts_with(EDITOR_CONTEXT_PREFIX))
                .collect::<Vec<_>>()
                .join("\n")
        });
    typed_prompt(&text)
        .trim()
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
    input: &mut (impl AsyncWrite + Unpin),
    pending: &mut HashMap<String, Pending>,
    next_id: &mut u64,
) -> Result<()> {
    let id = rpc_id(next_id);
    let (method, params) = match state.phase {
        ListPhase::Titles => {
            // The state DB answers without rescanning every rollout, which
            // took seconds with a thousand threads.
            let mut params = json!({
                "limit": state.limit,
                "cursor": state.cursor,
                "archived": false,
                "sortKey": "updated_at",
                "sortDirection": "desc",
                "modelProviders": [],
                "sourceKinds": interactive_source_kinds(state.request.scope),
                "useStateDbOnly": true,
            });
            if state.request.scope != AgentSessionScope::All {
                params["cwd"] = json!(workspace_paths(&state.request.cwd));
            }
            ("thread/list", params)
        }
        ListPhase::Contents => (
            "thread/search",
            json!({
                "searchTerm": state.request.query.trim(),
                "limit": state.limit,
                "cursor": state.cursor,
                "archived": false,
                "sortKey": "updated_at",
                "sortDirection": "desc",
                "sourceKinds": interactive_source_kinds(state.request.scope),
            }),
        ),
    };
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
            phase: ListPhase::Titles,
            seen: HashSet::new(),
            sessions: Vec::new(),
            cursors: HashSet::new(),
            cursor: None,
            limit: PAGE_LIMIT,
            more: false,
        },
        input,
        pending,
        next_id,
    )
    .await
}

pub(super) async fn handle_response(
    mut request: PendingSessionRequest,
    message: Value,
    input: &mut (impl AsyncWrite + Unpin),
    events: &mpsc::Sender<CodexEvent>,
    pending: &mut HashMap<String, Pending>,
    next_id: &mut u64,
) -> Result<()> {
    let request_id = request.request_id();
    if let Some(error) = message.get("error") {
        if error["code"].as_i64() == Some(FRAME_TOO_LARGE_CODE) && request.limit > 1 {
            request.limit /= 2;
            return list_page(request, input, pending, next_id).await;
        }
        let message = error["message"]
            .as_str()
            .unwrap_or("Codex session search failed")
            .to_string();
        // One thread too large to list, or a failed text search, still leaves
        // the sessions found before it.
        if !request.sessions.is_empty() {
            crate::log!("Codex session listing stopped early: {message}");
            return finish(request.sessions, true, request_id, events).await;
        }
        complete(events, request_id, Err(message)).await;
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
    let terms = search_terms(&request.request.query);
    for entry in page {
        let (thread, snippet) = match request.phase {
            ListPhase::Titles => (entry, String::new()),
            ListPhase::Contents => (&entry["thread"], clean_text(&entry["snippet"])),
        };
        let Some(id) = thread["id"].as_str().filter(|id| !id.is_empty()) else {
            continue;
        };
        if !request.seen.insert(id.to_string()) {
            continue;
        }
        if let Some(listed) = request.sessions.iter_mut().find(|session| session.id == id) {
            // A title match also found in the text gains its snippet.
            if listed.snippet.is_empty() {
                listed.snippet = snippet;
            }
            continue;
        }
        if request.phase == ListPhase::Titles && !title_matches(thread, &terms) {
            continue;
        }
        request
            .sessions
            .extend(summarize(&request.request, thread, snippet));
    }
    let next_cursor = message
        .pointer("/result/nextCursor")
        .and_then(Value::as_str)
        .filter(|cursor| !cursor.is_empty())
        .map(str::to_string);
    if request.sessions.len() >= MAX_LISTED_SESSIONS {
        let more = request.more || next_cursor.is_some() || wants_contents(&request);
        return finish(request.sessions, more, request_id, events).await;
    }
    if let Some(cursor) = next_cursor {
        if request.seen.len() <= MAX_SESSIONS
            && request.cursors.len() < MAX_PAGES
            && request.cursors.insert(cursor.clone())
        {
            request.cursor = Some(cursor);
            return list_page(request, input, pending, next_id).await;
        }
        // Too many threads to read through; list what was found.
        request.more = true;
    }
    if wants_contents(&request) {
        request.phase = ListPhase::Contents;
        request.seen.clear();
        request.cursors.clear();
        request.cursor = None;
        request.limit = PAGE_LIMIT;
        return list_page(request, input, pending, next_id).await;
    }
    finish(request.sessions, request.more, request_id, events).await
}

/// Whether a title search still has the conversation text to search.
fn wants_contents(request: &PendingSessionRequest) -> bool {
    request.phase == ListPhase::Titles
        && request.request.search_contents
        && !request.request.query.trim().is_empty()
}

fn search_terms(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

/// Whether every term appears, ignoring case, in the thread's name or prompt.
fn title_matches(thread: &Value, terms: &[String]) -> bool {
    if terms.is_empty() {
        return true;
    }
    let mut title = thread["name"].as_str().unwrap_or_default().to_lowercase();
    title.push('\n');
    title.push_str(&typed_prompt(thread["preview"].as_str().unwrap_or_default()).to_lowercase());
    terms.iter().all(|term| title.contains(term.as_str()))
}

/// The workspace as Codex may have recorded it, before and after resolving links.
fn workspace_paths(cwd: &Path) -> Vec<PathBuf> {
    let mut paths = vec![cwd.to_path_buf()];
    if let Ok(canonical) = cwd.canonicalize() {
        if canonical != cwd {
            paths.push(canonical);
        }
    }
    paths
}

/// Reports the most recent sessions; `more` says older ones were left out.
async fn finish(
    mut sessions: Vec<AgentSessionSummary>,
    more: bool,
    request_id: i64,
    events: &mpsc::Sender<CodexEvent>,
) -> Result<()> {
    sessions.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| right.id.cmp(&left.id))
    });
    let more = more || sessions.len() > MAX_LISTED_SESSIONS;
    sessions.truncate(MAX_LISTED_SESSIONS);
    complete(
        events,
        request_id,
        Ok(json!({"sessions": sessions, "more": more})),
    )
    .await;
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
    if request.scope != AgentSessionScope::All && !same_path(&request.cwd, Path::new(&cwd)) {
        return None;
    }
    let source = source_label(&thread["source"], is_red);
    let updated_at = thread["updatedAt"].as_i64().unwrap_or_default();
    let label = session_label(thread);
    let resumable = is_red && same_path(&request.cwd, Path::new(&cwd));
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
        resumable,
        updated_at,
    })
}

fn session_label(thread: &Value) -> String {
    if let Some(name) = nonempty(&thread["name"]) {
        return clean_text(&Value::String(name));
    }
    let preview = thread["preview"].as_str().unwrap_or_default();
    let prompt = clean_text(&Value::String(typed_prompt(preview).to_string()));
    if !prompt.is_empty() {
        prompt
    } else if preview.starts_with(RECAP_PREFIX) {
        "Continued conversation".to_string()
    } else {
        "Untitled session".to_string()
    }
}

fn is_red_thread(thread: &Value) -> bool {
    if thread["threadSource"].as_str() == Some("red") {
        return true;
    }
    // Codex records the originator for newer threads, which saves reading
    // the rollout.
    if let Some(originator) = thread["originator"].as_str() {
        return originator == "red";
    }
    if thread["source"].as_str() != Some("vscode") {
        return false;
    }

    let Some(line) = thread["path"].as_str().and_then(first_rollout_line) else {
        return false;
    };
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

/// A rollout's first line, which holds its session metadata. Codex compresses
/// older rollouts in place, though its thread list still names the plain file.
fn first_rollout_line(path: &str) -> Option<String> {
    let reader: Box<dyn std::io::Read> = match std::fs::File::open(path) {
        Ok(file) => Box::new(file),
        Err(_) => {
            let file = std::fs::File::open(format!("{path}.zst")).ok()?;
            Box::new(ruzstd::decoding::StreamingDecoder::new(file).ok()?)
        }
    };
    let mut line = String::new();
    BufReader::new(std::io::Read::take(reader, MAX_SESSION_METADATA_BYTES))
        .read_line(&mut line)
        .ok()?;
    Some(line)
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
            search_contents: false,
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
    fn labels_and_previews_leave_out_what_red_sends_with_a_prompt() {
        let thread = |preview: &str| json!({"id": "thread", "name": null, "preview": preview});
        assert_eq!(
            session_label(&thread(
                "Reply with OK.Active editor context from file:///a.rs:\n\n```text\nx\n```"
            )),
            "Reply with OK."
        );
        assert_eq!(
            session_label(&thread(
                "Previous conversation (the last turn was interrupted):\nYou: earlier\n\nFollow-up:\nTry   again"
            )),
            "Try again"
        );
        assert_eq!(
            session_label(&thread(
                "Previous conversation (the last turn was interrupted):\nYou: cut"
            )),
            "Continued conversation"
        );

        let response = json!({"result":{"thread":{"id":"thread","turns":[{"items":[
            {"type":"userMessage","content":[
                {"type":"text","text":"Fix the parser"},
                {"type":"text","text":"Active editor context from file:///a.rs:\n\n```text\nx\n```"}
            ]},
            {"type":"agentMessage","text":"Fixed"}
        ]}]}}});
        assert_eq!(
            session_preview("thread", &response).unwrap()["prompt"],
            "Fix the parser"
        );
    }

    #[test]
    fn title_search_matches_every_word_ignoring_case() {
        let thread = json!({"name": "Fix the Session Picker", "preview": "unused"});
        assert!(title_matches(&thread, &search_terms("picker fix")));
        assert!(!title_matches(&thread, &search_terms("picker parser")));
        let unnamed =
            json!({"name": null, "preview": "Speed up LISTING.Active editor context from x"});
        assert!(title_matches(&unnamed, &search_terms("listing")));
        assert!(!title_matches(&unnamed, &search_terms("editor")));
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
            search_contents: false,
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
            search_contents: false,
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

        // An older rollout Codex has since compressed in place.
        let compressed = directory.path().join("compressed.jsonl");
        std::fs::write(
            directory.path().join("compressed.jsonl.zst"),
            [
                0x28, 0xb5, 0x2f, 0xfd, 0x04, 0x58, 0x1d, 0x02, 0x00, 0xd2, 0xc4, 0x0f, 0x15, 0xa0,
                0xa5, 0x6d, 0x14, 0x94, 0x4a, 0xb5, 0x90, 0xb9, 0xbf, 0xa7, 0xbf, 0x53, 0x15, 0xc4,
                0xb7, 0x71, 0x7f, 0x70, 0xb1, 0x02, 0x42, 0x4a, 0xbe, 0xab, 0xcb, 0x9a, 0xc6, 0x08,
                0x0a, 0x50, 0xd3, 0xc2, 0x77, 0x1d, 0xee, 0xbb, 0xd7, 0x32, 0x32, 0xe0, 0xf2, 0xa1,
                0x51, 0xf9, 0x2c, 0x07, 0xc7, 0x8a, 0x85, 0xcd, 0x7c, 0x00, 0x48, 0xe2, 0xee, 0xd7,
                0xe5, 0x97, 0x63, 0x8c, 0x02, 0x00, 0x46, 0xd4, 0xb7, 0x75,
            ],
        )
        .unwrap();
        thread["id"] = json!("compressed-red");
        thread["path"] = json!(compressed);
        assert!(summarize(&request, &thread, String::new()).unwrap().is_red);
    }
}
