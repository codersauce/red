//! Discovery of persisted Codex conversations for Red's session picker.

use super::translate::{clean_text, session_summary, typed_prompt};
use super::*;
use crate::agent_backend::types::{SessionPage, SessionScope, SessionSummary};
use std::collections::HashSet;

const PAGE_LIMIT: usize = 100;
const MAX_SESSIONS: usize = 4096;
/// Sessions shown at once. Searching reaches older ones, and a long list costs
/// the plugin more to render than a person will scroll through.
const MAX_LISTED_SESSIONS: usize = 200;
const MAX_PAGES: usize = 100;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ListPhase {
    /// `thread/list`, filtered by title when there is a query.
    Titles,
    /// `thread/search` over conversation text.
    Contents,
}

pub(super) struct PendingSessionRequest {
    request_id: i64,
    request: SessionQuery,
    phase: ListPhase,
    /// Every thread id this phase has seen, listed or not.
    seen: HashSet<String>,
    sessions: Vec<SessionSummary>,
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
    result: std::result::Result<SessionPage, String>,
) {
    events
        .send(CodexEvent::SessionListCompleted { request_id, result })
        .await
        .ok();
}

fn interactive_source_kinds(scope: SessionScope) -> Value {
    match scope {
        SessionScope::Red => json!(["appServer", "vscode"]),
        SessionScope::Workspace | SessionScope::All => {
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
            if state.request.scope != SessionScope::All {
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
    request: SessionQuery,
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
            .extend(session_summary(&request.request, thread, snippet));
    }
    let next_cursor = message
        .pointer("/result/nextCursor")
        .and_then(Value::as_str)
        .filter(|cursor| !cursor.is_empty())
        .map(str::to_string);
    // Each phase stops at the cap, and `finish` keeps the newest of both, so
    // title matches filling the list don't hide newer matches in the text.
    let phase_matches = match request.phase {
        ListPhase::Titles => request.sessions.len(),
        ListPhase::Contents => request.seen.len(),
    };
    if phase_matches >= MAX_LISTED_SESSIONS {
        request.more |= next_cursor.is_some();
    } else if let Some(cursor) = next_cursor {
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
    mut sessions: Vec<SessionSummary>,
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
    complete(events, request_id, Ok(SessionPage { sessions, more })).await;
    Ok(())
}

#[cfg(test)]
#[cfg(test)]
mod tests {
    use super::*;

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
}
