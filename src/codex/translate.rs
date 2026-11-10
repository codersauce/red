//! The only place Codex app-server JSON becomes Red's agent types.
//!
//! Everything the editor or plugins read about models, saved sessions,
//! restored history and turn activity is built here, so the rest of Red never
//! looks at the wire format.

use std::{
    io::{BufRead as _, BufReader},
    path::Path,
    time::SystemTime,
};

use serde_json::{Map, Value};

use crate::agent_backend::types::{
    Activity, AnnotationChange, AnnotationDraft, EffortOption, ModelInfo, ModelOption,
    RestoredHistory, RestoredItem, RestoredTurn, SessionPreview, SessionQuery, SessionScope,
    SessionSummary, ToolActivity, ToolKind, ToolStatus,
};
use crate::agent_conversation::{EDITOR_CONTEXT_PREFIX, RECAP_FOLLOW_UP, RECAP_PREFIX};

const MAX_SESSION_METADATA_BYTES: u64 = 64 * 1024;
const MAX_SESSION_PREVIEW_CHARS: usize = 12 * 1024;

pub(super) fn session_preview(session_id: &str, message: &Value) -> Result<SessionPreview, String> {
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
            return Ok(SessionPreview {
                session_id: session_id.to_string(),
                prompt,
                response,
            });
        }
    }
    Ok(SessionPreview {
        session_id: session_id.to_string(),
        prompt: String::new(),
        response: String::new(),
    })
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

pub(super) fn session_summary(
    request: &SessionQuery,
    thread: &Value,
    snippet: String,
) -> Option<SessionSummary> {
    if thread["ephemeral"].as_bool() == Some(true) {
        return None;
    }
    let id = thread["id"].as_str()?.to_string();
    if request.exclude_session_id.as_deref() == Some(id.as_str()) {
        return None;
    }
    let cwd = thread["cwd"].as_str().unwrap_or_default().to_string();
    let is_red = is_red_thread(thread);
    if request.scope == SessionScope::Red && !is_red {
        return None;
    }
    if request.scope != SessionScope::All && !same_path(&request.cwd, Path::new(&cwd)) {
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
    Some(SessionSummary {
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

/// Collapses runs of whitespace, as the picker shows titles and snippets.
pub(super) fn clean_text(value: &Value) -> String {
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

/// What the person typed, without the editor context and recap Red sent with
/// it. Codex joins a turn's text items with no separator in a thread preview.
pub(super) fn typed_prompt(text: &str) -> &str {
    let text = match text.strip_prefix(RECAP_PREFIX) {
        Some(recap) => recap
            .rsplit_once(RECAP_FOLLOW_UP)
            .map_or("", |(_, follow_up)| follow_up),
        None => text,
    };
    text.split_once(EDITOR_CONTEXT_PREFIX)
        .map_or(text, |(prompt, _)| prompt)
        .trim_end()
}

fn clean_persisted_user_text(text: &str) -> String {
    match typed_prompt(text) {
        "" => text,
        prompt => prompt,
    }
    .to_string()
}

/// A thread's turns as Red's transcript and annotation replay read them.
pub(super) fn restored_history(thread: &Value) -> RestoredHistory {
    RestoredHistory {
        truncated: thread.get("historyTruncated").and_then(Value::as_bool) == Some(true),
        turns: thread
            .get("turns")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|turn| RestoredTurn {
                id: turn
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(str::to_string),
                items: turn
                    .get("items")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(restored_item)
                    .collect(),
            })
            .collect(),
    }
}

fn restored_item(item: &Value) -> Option<RestoredItem> {
    let id = item.get("id").and_then(Value::as_str).map(str::to_string);
    match item.get("type").and_then(Value::as_str)? {
        "userMessage" => Some(RestoredItem::User {
            id,
            text: item
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|input| input.get("type").and_then(Value::as_str) == Some("text"))
                .find_map(|input| input.get("text").and_then(Value::as_str))
                .map(clean_persisted_user_text),
        }),
        "agentMessage" => Some(RestoredItem::Agent {
            id,
            text: item
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        }),
        "dynamicToolCall" => annotation_change(item).map(RestoredItem::Annotations),
        _ => None,
    }
}

/// The effect of a completed, successful annotation tool call.
fn annotation_change(item: &Value) -> Option<AnnotationChange> {
    if item.get("status").and_then(Value::as_str) != Some("completed")
        || item.get("success").and_then(Value::as_bool) != Some(true)
    {
        return None;
    }
    let tool = item.get("tool").and_then(Value::as_str)?;
    if !matches!(tool, "add_annotations" | "dismiss_annotations") {
        return None;
    }
    let output = item
        .get("contentItems")?
        .as_array()?
        .iter()
        .filter_map(|content| content.get("text").and_then(Value::as_str))
        .filter_map(|text| serde_json::from_str::<Value>(text).ok())
        .find(|output| output.get("ok").and_then(Value::as_bool) == Some(true))?;
    if tool == "dismiss_annotations" {
        return Some(AnnotationChange::Dismissed {
            ids: output
                .get("dismissed")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|id| id.as_str().map(str::to_string))
                .collect(),
        });
    }
    let line = |annotation: &Value, key: &str| {
        annotation
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|line| usize::try_from(line).ok())
    };
    Some(AnnotationChange::Added {
        path: output
            .get("path")
            .and_then(Value::as_str)
            .or_else(|| item.pointer("/arguments/path").and_then(Value::as_str))
            .map(str::to_string),
        annotations: output
            .get("annotations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|annotation| {
                Some(AnnotationDraft {
                    id: annotation.get("id").and_then(Value::as_str)?.to_string(),
                    start_line: line(annotation, "start_line"),
                    end_line: line(annotation, "end_line"),
                    message: annotation
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
            })
            .collect(),
    })
}

/// The model named by a thread start, resume or settings response.
pub(super) fn model_info(value: &Value) -> Option<ModelInfo> {
    let model = value.get("model")?.as_str()?.trim();
    if model.is_empty() {
        return None;
    }
    Some(ModelInfo {
        model: model.to_string(),
        provider: value
            .get("modelProvider")
            .and_then(Value::as_str)
            .map(str::to_string),
        effort: value
            .get("reasoningEffort")
            .or_else(|| value.get("effort"))
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// The defaults a workspace's configuration names for new conversations.
pub(super) struct ConfiguredModel {
    pub model: Option<String>,
    pub provider: Option<String>,
    pub effort: Option<String>,
}

pub(super) fn configured_model(config: &Value) -> ConfiguredModel {
    ConfiguredModel {
        model: nonempty(&config["model"]),
        provider: nonempty(&config["model_provider"]),
        effort: nonempty(&config["model_reasoning_effort"]),
    }
}

/// The reasoning effort a catalog entry runs with unless told otherwise.
pub(super) fn catalog_default_effort(model: &ModelOption) -> Option<String> {
    nonempty(&Value::from(model.default_reasoning_effort.clone()))
}

/// A catalog entry the user may select, or `None` for hidden or unnamed ones.
pub(super) fn model_option(model: &Value) -> Option<ModelOption> {
    if model["hidden"].as_bool() == Some(true) {
        return None;
    }
    let name = model["model"].as_str().filter(|name| !name.is_empty())?;
    let mut extra = model.as_object()?.clone();
    extra.remove("model");
    Some(ModelOption {
        model: name.to_string(),
        display_name: take_string(&mut extra, "displayName"),
        description: take_string(&mut extra, "description"),
        is_default: take_bool(&mut extra, "isDefault"),
        default_reasoning_effort: take_string(&mut extra, "defaultReasoningEffort"),
        supported_reasoning_efforts: take_efforts(&mut extra),
        extra,
    })
}

fn take_string(map: &mut Map<String, Value>, key: &str) -> Option<String> {
    let value = map.get(key)?.as_str()?.to_string();
    map.remove(key);
    Some(value)
}

fn take_bool(map: &mut Map<String, Value>, key: &str) -> Option<bool> {
    let value = map.get(key)?.as_bool()?;
    map.remove(key);
    Some(value)
}

fn take_efforts(map: &mut Map<String, Value>) -> Option<Vec<EffortOption>> {
    let efforts = map
        .get("supportedReasoningEfforts")?
        .as_array()?
        .iter()
        .map(|effort| {
            let mut extra = effort.as_object()?.clone();
            Some(EffortOption {
                reasoning_effort: take_string(&mut extra, "reasoningEffort")?,
                description: take_string(&mut extra, "description"),
                extra,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    map.remove("supportedReasoningEfforts");
    Some(efforts)
}

/// Converts only recognized items; never forwards raw arguments or file contents.
pub(super) fn activity(item: &Value, completed: bool, cwd: &Path) -> Option<Activity> {
    match item["type"].as_str()? {
        "reasoning" if !completed => return Some(Activity::Thought),
        "dynamicToolCall" => {}
        _ => return None,
    }
    let id = item["id"].as_str().filter(|id| !id.is_empty())?;
    let tool = item["tool"].as_str().unwrap_or("tool");
    let arguments = &item["arguments"];
    let full_path = label(arguments["path"].as_str().unwrap_or("file"), 2048);
    let path = compact_path(&full_path, cwd);
    let (kind, title) = tool_title(tool, arguments, &path);
    let (_, full_title) = tool_title(tool, arguments, &full_path);
    let content_text = item["contentItems"]
        .as_array()
        .and_then(|items| items.iter().find_map(|content| content["text"].as_str()));
    let status = if !completed {
        ToolStatus::InProgress
    } else if item["success"].as_bool() == Some(false) || item["status"].as_str() == Some("failed")
    {
        ToolStatus::Failed
    } else if matches!(item["status"].as_str(), Some("cancelled" | "declined")) {
        ToolStatus::Cancelled
    } else {
        ToolStatus::Completed
    };
    let detail = if status == ToolStatus::Failed {
        content_text
            .map(|text| label(text, 2048))
            .unwrap_or_else(|| "Tool failed".to_string())
    } else if completed {
        item["durationMs"]
            .as_u64()
            .map(|ms| format!("{ms} ms"))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let tool = ToolActivity {
        tool_call_id: id.to_string(),
        kind,
        title: label(&title, 72),
        full_title,
        status,
        detail,
    };
    Some(if completed {
        Activity::ToolFinished(tool)
    } else {
        Activity::ToolStarted(tool)
    })
}

fn tool_title(tool: &str, arguments: &Value, path: &str) -> (ToolKind, String) {
    match tool {
        "list_files" => (ToolKind::List, "Listing workspace files".to_string()),
        "search_files" => (
            ToolKind::Search,
            format!(
                "Searching for {}",
                label(arguments["query"].as_str().unwrap_or("text"), 80)
            ),
        ),
        "read_file" => (ToolKind::Read, format!("Reading {path}")),
        "write_file" | "apply_edits" => (ToolKind::Edit, format!("Editing {path}")),
        "create_directory" => (
            ToolKind::Create,
            format!("Creating {}/", path.trim_end_matches('/')),
        ),
        "get_editor_state" => (ToolKind::Inspect, "Inspecting editor state".to_string()),
        "open_file" => (ToolKind::Inspect, format!("Opening {path}")),
        "select_text" => (ToolKind::Inspect, format!("Selecting text in {path}")),
        "run_editor_action" => (
            ToolKind::Inspect,
            format!(
                "Running editor action {}",
                label(arguments["action"].as_str().unwrap_or(""), 80)
            ),
        ),
        "lsp_status" => (
            ToolKind::Inspect,
            format!("Checking language-server status for {path}"),
        ),
        "lsp_diagnostics" => match arguments["scope"].as_str().unwrap_or("workspace") {
            "file" => (
                ToolKind::Diagnostics,
                format!("Checking diagnostics for {path}"),
            ),
            "open_buffers" => (
                ToolKind::Diagnostics,
                "Checking diagnostics for open buffers".to_string(),
            ),
            _ => (
                ToolKind::Diagnostics,
                "Checking workspace diagnostics".to_string(),
            ),
        },
        "lsp_prepare_rename" => (
            ToolKind::Rename,
            format!("Checking rename support for {path}"),
        ),
        "lsp_preview_rename" => (ToolKind::Rename, format!("Previewing rename in {path}")),
        "lsp_apply_edit" => (ToolKind::Edit, "Applying language-server edits".to_string()),
        _ => (ToolKind::Tool, format!("Running {}", label(tool, 80))),
    }
}

fn compact_path(path: &str, cwd: &Path) -> String {
    let path = Path::new(path);
    let display = path.strip_prefix(cwd).unwrap_or_else(|_| {
        // Rooted paths need shortening even without a Windows drive prefix.
        if path.has_root() {
            path.file_name().map(Path::new).unwrap_or(path)
        } else {
            path
        }
    });
    let text = display.to_string_lossy();
    if text.chars().count() <= 52 {
        text.into_owned()
    } else {
        let tail: String = text.chars().rev().take(49).collect();
        format!("…{}", tail.chars().rev().collect::<String>())
    }
}

fn label(text: &str, limit: usize) -> String {
    let mut chars = text
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch });
    let mut result: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;

    fn item_update(item: &Value, completed: bool, cwd: &Path) -> Option<Value> {
        activity(item, completed, cwd).map(|activity| serde_json::to_value(activity).unwrap())
    }
    #[test]
    fn summary_filters_scope_and_formats_red_origin() {
        let cwd = PathBuf::from("/work/red");
        let request = SessionQuery {
            cwd: cwd.clone(),
            scope: SessionScope::Red,
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
        let summary = session_summary(&request, &thread, String::new()).unwrap();
        assert_eq!(summary.label, "Fix the picker");
        assert_eq!(summary.source, "Red");
        assert_eq!(summary.time_group, "Earlier");
        assert!(summary.is_red);

        thread["source"] = json!("vscode");
        let cli_summary = session_summary(&request, &thread, String::new()).unwrap();
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
            session_preview("thread", &response).unwrap().prompt,
            "Fix the parser"
        );
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
        assert_eq!(preview.prompt, "Latest question");
        assert_eq!(preview.response, "Latest answer");
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
        assert_eq!(preview.prompt, "Still running");
        assert_eq!(preview.response, "");
    }

    #[test]
    fn workspace_scope_rejects_threads_from_other_roots() {
        let request = SessionQuery {
            cwd: PathBuf::from("/work/red"),
            scope: SessionScope::Workspace,
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
        assert!(session_summary(&request, &thread, String::new()).is_none());
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
        let request = SessionQuery {
            cwd: PathBuf::from("/work/red"),
            scope: SessionScope::Red,
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

        let session = session_summary(&request, &thread, String::new()).unwrap();
        assert!(session.is_red);
        assert_eq!(session.source, "Red");

        thread["id"] = json!("different-session");
        assert!(session_summary(&request, &thread, String::new()).is_none());

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
        assert!(
            session_summary(&request, &thread, String::new())
                .unwrap()
                .is_red
        );
    }

    #[test]
    fn tool_lifecycle_is_bounded_and_does_not_expose_file_contents() {
        let item = json!({"id":"call", "type":"dynamicToolCall", "tool":"write_file",
            "arguments":{"path":"src/main.rs\nnext", "content":"private file contents"},
            "status":"completed", "success":true, "durationMs":12,
            "contentItems":[{"text":"private returned contents"}]});
        let start = item_update(&item, false, Path::new("/workspace")).unwrap();
        assert_eq!(start["title"], "Editing src/main.rs next");
        assert_eq!(start["kind"], "edit");
        assert_eq!(start["status"], "in_progress");
        let end = item_update(&item, true, Path::new("/workspace")).unwrap();
        assert_eq!(end["tool_call_id"], "call");
        assert_eq!(end["detail"], "12 ms");
        assert!(!end.to_string().contains("private"));
    }

    #[test]
    fn failures_and_reasoning_have_safe_presentations() {
        let item = json!({"id":"call", "type":"dynamicToolCall", "tool":"read_file",
            "success":false, "contentItems":[{"text":"é".repeat(3000)}]});
        let update = item_update(&item, true, Path::new("/workspace")).unwrap();
        assert_eq!(update["status"], "failed");
        assert_eq!(update["detail"].as_str().unwrap().chars().count(), 2049);
        assert_eq!(
            item_update(
                &json!({"type":"reasoning", "text":"private"}),
                false,
                Path::new("/workspace")
            ),
            Some(json!({"session_update":"agent_thought_chunk"}))
        );
        assert!(item_update(&json!({"type":"unknown"}), false, Path::new("/workspace")).is_none());
    }

    #[test]
    fn transport_failures_include_the_domain_error_detail() {
        let item = json!({
            "id": "rename",
            "type": "dynamicToolCall",
            "tool": "lsp_apply_edit",
            "status": "completed",
            "success": false,
            "contentItems": [{
                "text": serde_json::to_string(&json!({
                    "ok": false,
                    "status": "not_ready",
                    "message": "language server is not ready; retry"
                })).unwrap()
            }]
        });

        let update = item_update(&item, true, Path::new("/workspace")).unwrap();
        assert_eq!(update["status"], "failed");
        assert!(update["detail"].as_str().unwrap().contains("not ready"));
    }

    #[test]
    fn paths_are_compact_but_inspectable() {
        let cwd = Path::new("/workspace/project");
        for (path, expected) in [
            ("/workspace/project/src/main.rs", "Reading src/main.rs"),
            (
                "/Users/someone/.codex/memories/MEMORY.md",
                "Reading MEMORY.md",
            ),
        ] {
            let item = json!({"id":"call", "type":"dynamicToolCall", "tool":"read_file", "arguments":{"path":path}});
            let update = item_update(&item, false, cwd).unwrap();
            assert_eq!(update["title"], expected);
            assert_eq!(update["full_title"], format!("Reading {path}"));
        }
        assert_eq!(compact_path("src/main.rs", cwd), "src/main.rs");
        assert_eq!(compact_path("/outside/main.rs", cwd), "main.rs");
    }

    #[cfg(windows)]
    #[test]
    fn windows_activity_paths_keep_native_relative_paths_and_shorten_other_roots() {
        let cwd = Path::new(r"C:\workspace\project");
        for (path, expected) in [
            (r"C:\workspace\project\src\main.rs", r"src\main.rs"),
            (r"D:\other\main.rs", "main.rs"),
            (r"\other\main.rs", "main.rs"),
            (r"\\server\share\main.rs", "main.rs"),
            (r"src\main.rs", r"src\main.rs"),
            (r"C:main.rs", "C:main.rs"),
        ] {
            assert_eq!(compact_path(path, cwd), expected, "{path}");
        }
    }

    #[test]
    fn directory_creation_has_a_compact_activity_label() {
        let item = json!({"id":"mkdir", "type":"dynamicToolCall", "tool":"create_directory",
            "arguments":{"path":"/workspace/go/"}});
        let update = item_update(&item, false, Path::new("/workspace")).unwrap();
        assert_eq!(update["title"], "Creating go/");
        assert_eq!(update["kind"], "create");
        assert_eq!(update["full_title"], "Creating /workspace/go/");
    }

    #[test]
    fn lsp_tools_have_explicit_activity_titles_and_kinds() {
        let cases = [
            (
                "lsp_status",
                json!({"path":"src/main.rs"}),
                "inspect",
                "Checking language-server status for src/main.rs",
            ),
            (
                "lsp_diagnostics",
                json!({"scope":"file", "path":"src/main.rs"}),
                "diagnostics",
                "Checking diagnostics for src/main.rs",
            ),
            (
                "lsp_prepare_rename",
                json!({"path":"src/main.rs"}),
                "rename",
                "Checking rename support for src/main.rs",
            ),
            (
                "lsp_preview_rename",
                json!({"path":"src/main.rs", "position":{"line":4,"character":2}, "new_name":"private_symbol"}),
                "rename",
                "Previewing rename in src/main.rs",
            ),
            (
                "lsp_apply_edit",
                json!({"plan_id":"private-plan"}),
                "edit",
                "Applying language-server edits",
            ),
        ];

        for (tool, arguments, kind, title) in cases {
            let item = json!({
                "id": "call",
                "type": "dynamicToolCall",
                "tool": tool,
                "arguments": arguments,
            });
            let update = item_update(&item, false, Path::new("/workspace")).unwrap();
            assert_eq!(update["kind"], kind, "{tool}");
            assert_eq!(update["title"], title, "{tool}");
            assert!(!update.to_string().contains("private"), "{tool}");
        }
    }

    #[test]
    fn diagnostics_activity_titles_describe_the_requested_scope() {
        for (arguments, title) in [
            (
                json!({"scope":"open_buffers"}),
                "Checking diagnostics for open buffers",
            ),
            (
                json!({"scope":"workspace"}),
                "Checking workspace diagnostics",
            ),
        ] {
            let item = json!({
                "id": "diagnostics",
                "type": "dynamicToolCall",
                "tool": "lsp_diagnostics",
                "arguments": arguments,
            });
            let update = item_update(&item, false, Path::new("/workspace")).unwrap();
            assert_eq!(update["title"], title);
            assert_eq!(update["kind"], "diagnostics");
        }
    }

    #[test]
    fn model_metadata_tolerates_missing_and_optional_fields() {
        assert_eq!(model_info(&json!({})), None);
        assert_eq!(model_info(&json!({"model":""})), None);
        let info = model_info(&json!({"model":"custom", "reasoningEffort":null})).unwrap();
        assert_eq!(
            info,
            ModelInfo {
                model: "custom".into(),
                provider: None,
                effort: None
            }
        );
        assert_eq!(
            model_info(&json!({"model":"custom", "effort":"high"}))
                .unwrap()
                .effort
                .as_deref(),
            Some("high")
        );
    }

    #[test]
    fn restored_prompts_leave_out_the_recap_and_context_red_sent() {
        assert_eq!(
            clean_persisted_user_text(
                "Previous conversation (the last turn was interrupted):\nYou: earlier\nAgent: partial\n\nFollow-up:\nTry again\n\nActive editor context from file:///a.rs:\n\n```text\nx\n```"
            ),
            "Try again"
        );
        assert_eq!(
            typed_prompt("Fix it.Active editor context from file:///a.rs:"),
            "Fix it."
        );
        assert_eq!(clean_persisted_user_text("Plain prompt"), "Plain prompt");
        assert_eq!(
            clean_persisted_user_text("Active editor context from file:///a.rs:\nctx"),
            "Active editor context from file:///a.rs:\nctx"
        );
    }
}
