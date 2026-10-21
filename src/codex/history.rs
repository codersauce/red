//! Paged reads of persisted Codex thread history.
//!
//! A long Codex conversation can hold hundreds of megabytes of tool output, so
//! Red never asks for a whole thread in one response. It pages the newest turns
//! with `thread/turns/list` and stops once it has as much as the pane keeps.

use super::*;

/// Turns per page when only the messages are needed.
const SUMMARY_PAGE_TURNS: usize = 50;
/// Turns per page when tool calls matter, since they carry Red's annotations.
const FULL_PAGE_TURNS: usize = 10;
/// The latest exchange is almost always within the newest few turns.
const PREVIEW_TURNS: usize = 5;
/// Exchanges kept for a restored transcript, matching its 512-item limit.
const MAX_HISTORY_TURNS: usize = 256;
/// Message text kept for a restored transcript or a continuation.
const MAX_HISTORY_TEXT_BYTES: usize = 1024 * 1024;
const MAX_HISTORY_PAGES: usize = 64;
const METHOD_NOT_FOUND: i64 = -32601;

/// What a history read is for, and what happens when it finishes.
pub(super) enum HistoryPurpose {
    Preview {
        request_id: i64,
    },
    /// A thread rejoined with `excludeTurns`; its model is announced after the
    /// transcript, as for every other restoration.
    Resume {
        model_info: Option<AgentModelInfo>,
    },
    Continue {
        cwd: PathBuf,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TurnView {
    Summary,
    Full,
}

impl TurnView {
    fn name(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Full => "full",
        }
    }
}

pub(super) struct PendingHistory {
    purpose: HistoryPurpose,
    session_id: String,
    view: TurnView,
    cursor: Option<String>,
    /// Newest first, as the pages arrive.
    turns: Vec<Value>,
    text_bytes: usize,
    pages: usize,
}

/// Starts reading a thread's newest turns for `purpose`.
pub(super) async fn start(
    purpose: HistoryPurpose,
    session_id: String,
    input: &mut (impl AsyncWrite + Unpin),
    pending: &mut HashMap<String, Pending>,
    next_id: &mut u64,
) -> Result<()> {
    // Only a resumed Red thread replays annotations from its tool calls.
    let view = match purpose {
        HistoryPurpose::Resume { .. } => TurnView::Full,
        HistoryPurpose::Preview { .. } | HistoryPurpose::Continue { .. } => TurnView::Summary,
    };
    request_page(
        PendingHistory {
            purpose,
            session_id,
            view,
            cursor: None,
            turns: Vec::new(),
            text_bytes: 0,
            pages: 0,
        },
        input,
        pending,
        next_id,
    )
    .await
}

async fn request_page(
    state: PendingHistory,
    input: &mut (impl AsyncWrite + Unpin),
    pending: &mut HashMap<String, Pending>,
    next_id: &mut u64,
) -> Result<()> {
    let limit = match (&state.purpose, state.view) {
        (HistoryPurpose::Preview { .. }, _) => PREVIEW_TURNS,
        (_, TurnView::Full) => FULL_PAGE_TURNS,
        (_, TurnView::Summary) => SUMMARY_PAGE_TURNS,
    };
    let id = rpc_id(next_id);
    let message = json!({
        "id": id,
        "method": "thread/turns/list",
        "params": {
            "threadId": state.session_id,
            "cursor": state.cursor,
            "limit": limit,
            "sortDirection": "desc",
            "itemsView": state.view.name(),
        }
    });
    pending.insert(id, Pending::History(state));
    write_message(input, &message).await
}

pub(super) async fn handle_response(
    mut state: PendingHistory,
    message: Value,
    input: &mut (impl AsyncWrite + Unpin),
    events: &mpsc::Sender<CodexEvent>,
    pending: &mut HashMap<String, Pending>,
    next_id: &mut u64,
) -> Result<()> {
    if let Some(error) = message.get("error") {
        let code = error["code"].as_i64();
        if code == Some(FRAME_TOO_LARGE_CODE) && state.view == TurnView::Full {
            // Tool output made this page too large: keep going with messages only.
            state.view = TurnView::Summary;
            return request_page(state, input, pending, next_id).await;
        }
        if code == Some(METHOD_NOT_FOUND) && state.pages == 0 {
            return read_whole_thread(state, input, events, pending, next_id).await;
        }
        let reason = error["message"]
            .as_str()
            .unwrap_or("Codex could not read the session history")
            .to_string();
        return finish(state, false, Some(reason), input, events, pending, next_id).await;
    }
    let Some(page) = message.pointer("/result/data").and_then(Value::as_array) else {
        let reason = "Codex returned an invalid session history".to_string();
        return finish(state, false, Some(reason), input, events, pending, next_id).await;
    };
    for turn in page {
        state.text_bytes += turn_text_bytes(turn);
        state.turns.push(turn.clone());
    }
    state.pages += 1;
    state.cursor = message
        .pointer("/result/nextCursor")
        .and_then(Value::as_str)
        .map(str::to_string);
    let complete = state.cursor.is_none();
    let enough = matches!(state.purpose, HistoryPurpose::Preview { .. })
        || state.turns.len() >= MAX_HISTORY_TURNS
        || state.text_bytes >= MAX_HISTORY_TEXT_BYTES
        || state.pages >= MAX_HISTORY_PAGES;
    if complete || enough {
        return finish(state, complete, None, input, events, pending, next_id).await;
    }
    request_page(state, input, pending, next_id).await
}

/// Falls back to one `thread/read` for an app-server without turn paging.
async fn read_whole_thread(
    state: PendingHistory,
    input: &mut (impl AsyncWrite + Unpin),
    events: &mpsc::Sender<CodexEvent>,
    pending: &mut HashMap<String, Pending>,
    next_id: &mut u64,
) -> Result<()> {
    let request = match state.purpose {
        HistoryPurpose::Preview { request_id } => Pending::SessionPreview {
            request_id,
            session_id: state.session_id.clone(),
        },
        HistoryPurpose::Continue { cwd } => Pending::ContinueRead {
            cwd,
            source_session_id: state.session_id.clone(),
        },
        // A resumed thread keeps the transcript Red saved for it.
        HistoryPurpose::Resume { .. } => {
            return finish(state, false, None, input, events, pending, next_id).await;
        }
    };
    let id = rpc_id(next_id);
    let message = json!({
        "id": id,
        "method": "thread/read",
        "params": {"threadId": state.session_id, "includeTurns": true}
    });
    pending.insert(id, request);
    write_message(input, &message).await
}

async fn finish(
    state: PendingHistory,
    complete: bool,
    failure: Option<String>,
    input: &mut (impl AsyncWrite + Unpin),
    events: &mpsc::Sender<CodexEvent>,
    pending: &mut HashMap<String, Pending>,
    next_id: &mut u64,
) -> Result<()> {
    let PendingHistory {
        purpose,
        session_id,
        mut turns,
        ..
    } = state;
    turns.reverse();
    let mut thread = json!({"id": session_id, "turns": turns});
    if !complete {
        thread["historyTruncated"] = json!(true);
    }
    // A failed first page has nothing to show, except for a resumed thread,
    // whose saved transcript still stands.
    let failure = failure.filter(|_| thread["turns"].as_array().is_none_or(Vec::is_empty));
    match purpose {
        HistoryPurpose::Preview { request_id } => {
            let result = match failure {
                Some(message) => Err(message),
                None => {
                    sessions::session_preview(&session_id, &json!({"result": {"thread": thread}}))
                }
            };
            events
                .send(CodexEvent::SessionPreviewCompleted { request_id, result })
                .await
                .ok();
        }
        HistoryPurpose::Continue { cwd } => {
            if let Some(message) = failure {
                events
                    .send(CodexEvent::SessionRestoreFailed {
                        session_id,
                        message,
                    })
                    .await
                    .ok();
                return Ok(());
            }
            start_thread_request(
                ThreadRequest::Agent {
                    cwd,
                    selection: None,
                    launch: SessionLaunch::Continue {
                        source_session_id: session_id,
                        source_thread: thread,
                    },
                },
                input,
                pending,
                next_id,
            )
            .await?;
        }
        HistoryPurpose::Resume { model_info } => {
            if let Some(message) = &failure {
                crate::log!("Codex history for {session_id} is unavailable: {message}");
            }
            events
                .send(CodexEvent::SessionRestored {
                    session_id: session_id.clone(),
                    thread,
                })
                .await
                .ok();
            if let Some(model_info) = model_info {
                events
                    .send(CodexEvent::SessionModelChanged {
                        session_id,
                        model_info,
                    })
                    .await
                    .ok();
            }
        }
    }
    Ok(())
}

fn turn_text_bytes(turn: &Value) -> usize {
    turn["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| match item["type"].as_str() {
            Some("userMessage") => item["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|content| content["text"].as_str())
                .map(str::len)
                .sum(),
            Some("agentMessage") => item["text"].as_str().map_or(0, str::len),
            _ => 0,
        })
        .sum()
}
