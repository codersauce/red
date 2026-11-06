//! Codex CLI rollout JSONL files under `~/.codex/sessions/YYYY/MM/DD`.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::SystemTime,
};

use chrono::Datelike;
use serde_json::Value;

use super::{tools, SessionData, Tail, WatchOptions, Watcher};

pub(crate) struct CodexState {
    pub last_prompt: String,
    pub last_say: String,
}

pub(crate) fn scan(watcher: &mut Watcher, options: &WatchOptions, cutoff: SystemTime) {
    if !options.codex_sessions.is_dir() {
        return;
    }
    let days = (options.since.as_secs() / 86_400) as i64 + 2;
    let today = chrono::Local::now().date_naive();
    for offset in -days..=1 {
        let day = today + chrono::Duration::days(offset);
        let directory = options.codex_sessions.join(format!(
            "{:04}/{:02}/{:02}",
            day.year(),
            day.month(),
            day.day()
        ));
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !name.starts_with("rollout-")
                || path.extension().and_then(|ext| ext.to_str()) != Some("jsonl")
            {
                continue;
            }
            let key = format!("codex:{}", path.display());
            if watcher.has(&key) || !super::modified_after(&path, cutoff) {
                continue;
            }
            let Some(cwd) = rollout_cwd(&path) else {
                continue;
            };
            if !super::within(&options.root, Path::new(&cwd)) {
                continue;
            }
            let mut data = SessionData::new(key, "codex");
            data.cwd = PathBuf::from(cwd);
            data.name = "codex".to_string();
            watcher.insert(
                data,
                super::Extra::Codex(CodexState {
                    last_prompt: String::new(),
                    last_say: String::new(),
                }),
                Tail::new(path),
            );
        }
    }
}

pub(crate) fn ingest(data: &mut SessionData, state: &mut CodexState, line: &[u8]) {
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return;
    };
    let time = super::parse_time(value.get("timestamp").and_then(Value::as_str));
    let payload = value.get("payload").cloned().unwrap_or(Value::Null);
    match value.get("type").and_then(Value::as_str).unwrap_or("") {
        "session_meta" | "turn_context" => {
            if let Some(cwd) = payload.get("cwd").and_then(Value::as_str) {
                if !cwd.is_empty() {
                    data.cwd = PathBuf::from(cwd);
                }
            }
            if value.get("type").and_then(Value::as_str) == Some("session_meta") {
                if let Some(role) = payload
                    .get("agent_role")
                    .and_then(Value::as_str)
                    .or_else(|| payload.get("agent_nickname").and_then(Value::as_str))
                {
                    if !role.is_empty() {
                        data.name = role.to_ascii_lowercase();
                    }
                }
            }
            data.touch(time);
        }
        "response_item" => ingest_item(data, state, time, &payload),
        "event_msg" => ingest_event(data, state, time, &payload),
        _ => {}
    }
}

fn ingest_item(
    data: &mut SessionData,
    state: &mut CodexState,
    time: Option<SystemTime>,
    payload: &Value,
) {
    match payload.get("type").and_then(Value::as_str).unwrap_or("") {
        "message" => {
            let text = text_value(payload.get("content"));
            match payload.get("role").and_then(Value::as_str).unwrap_or("") {
                "assistant" => remember_say(data, state, time, &text),
                "user" => {
                    let trimmed = text.trim();
                    if !trimmed.starts_with('<') && !trimmed.starts_with("# AGENTS.md") {
                        remember_prompt(data, state, time, trimmed);
                    }
                }
                _ => data.touch(time),
            }
        }
        "function_call" => {
            let name = payload.get("name").and_then(Value::as_str).unwrap_or("");
            if name == "write_stdin" {
                data.touch(time);
                return;
            }
            let arguments = decode_arguments(payload.get("arguments"));
            let mut events = tools::tool_events(name, &arguments, &data.cwd, false);
            let call_id = payload.get("call_id").and_then(Value::as_str).unwrap_or("");
            for event in &mut events {
                event.call_id = call_id.to_string();
            }
            data.add(time, events);
        }
        "custom_tool_call" => {
            let name = payload.get("name").and_then(Value::as_str).unwrap_or("");
            let input = payload.get("input").and_then(Value::as_str).unwrap_or("");
            let arguments = serde_json::json!({ "input": input });
            let mut events = tools::tool_events(name, &arguments, &data.cwd, false);
            let call_id = payload.get("call_id").and_then(Value::as_str).unwrap_or("");
            for event in &mut events {
                event.call_id = call_id.to_string();
            }
            data.add(time, events);
        }
        "local_shell_call" => {
            let Some(action) = payload.get("action") else {
                return;
            };
            let cwd = action
                .get("working_directory")
                .and_then(Value::as_str)
                .filter(|cwd| !cwd.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| data.cwd.clone());
            let command = action
                .get("command")
                .and_then(Value::as_array)
                .map(|parts| {
                    let words: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
                    argv_command(&words)
                })
                .unwrap_or_default();
            let mut event =
                tools::shell_event(super::Event::bare(super::Kind::Tool), &command, &cwd);
            event.tool = "local_shell".to_string();
            if let Some(call_id) = payload.get("call_id").and_then(Value::as_str) {
                event.call_id = call_id.to_string();
            }
            data.add(time, vec![event]);
        }
        "web_search_call" => {
            let mut event = super::Event::bare(super::Kind::Web);
            event.tool = "web_search".to_string();
            event.text = payload
                .pointer("/action/query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            event.outcome = super::Outcome::Succeeded;
            data.add(time, vec![event]);
        }
        "function_call_output" | "custom_tool_call_output" => {
            let call_id = payload.get("call_id").and_then(Value::as_str).unwrap_or("");
            let output = text_value(payload.get("output"));
            finish_call(data, time, call_id, &output);
        }
        _ => data.touch(time),
    }
}

fn ingest_event(
    data: &mut SessionData,
    state: &mut CodexState,
    time: Option<SystemTime>,
    payload: &Value,
) {
    match payload.get("type").and_then(Value::as_str).unwrap_or("") {
        "user_message" => remember_prompt(
            data,
            state,
            time,
            payload.get("message").and_then(Value::as_str).unwrap_or(""),
        ),
        "agent_message" => remember_say(
            data,
            state,
            time,
            payload.get("message").and_then(Value::as_str).unwrap_or(""),
        ),
        "task_started" | "turn_started" => {
            data.turn_ended = false;
            data.touch(time);
        }
        "task_complete" | "turn_complete" => {
            let text = payload
                .get("last_agent_message")
                .and_then(Value::as_str)
                .unwrap_or("");
            data.end_turn(time, text);
        }
        "turn_aborted" => data.end_turn(time, "interrupted"),
        "exec_command_end" | "patch_apply_end" => {
            let call_id = payload.get("call_id").and_then(Value::as_str).unwrap_or("");
            let failed = payload.get("success").and_then(Value::as_bool) == Some(false)
                || payload
                    .get("exit_code")
                    .and_then(Value::as_i64)
                    .is_some_and(|code| code != 0);
            for index in data.call_indexes(call_id) {
                if let Some(event) = data.events.get_mut(index) {
                    if event.outcome == super::Outcome::Pending {
                        event.outcome = if failed {
                            super::Outcome::Failed
                        } else {
                            super::Outcome::Succeeded
                        };
                    }
                }
            }
            data.touch(time);
        }
        _ => data.touch(time),
    }
}

fn finish_call(data: &mut SessionData, time: Option<SystemTime>, call_id: &str, output: &str) {
    let failed =
        output.starts_with("Error") || output.starts_with("apply_patch verification failed");
    for index in data.call_indexes(call_id) {
        if let Some(event) = data.events.get_mut(index) {
            if event.outcome == super::Outcome::Pending {
                event.outcome = if failed {
                    super::Outcome::Failed
                } else {
                    super::Outcome::Succeeded
                };
            }
        }
    }
    data.touch(time);
}

fn remember_prompt(
    data: &mut SessionData,
    state: &mut CodexState,
    time: Option<SystemTime>,
    text: &str,
) {
    let text = text.trim();
    if text.is_empty() || text == state.last_prompt {
        return;
    }
    state.last_prompt = text.to_string();
    state.last_say.clear();
    data.prompt(time, text);
}

fn remember_say(
    data: &mut SessionData,
    state: &mut CodexState,
    time: Option<SystemTime>,
    text: &str,
) {
    let text = text.trim();
    if text.is_empty() || text == state.last_say {
        return;
    }
    state.last_say = text.to_string();
    data.say(time, text);
}

fn decode_arguments(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(text)) => serde_json::from_str(text).unwrap_or(Value::Null),
        Some(Value::Object(_)) => value.cloned().unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

fn text_value(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| part.as_str())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn argv_command(argv: &[&str]) -> String {
    if argv.len() >= 3 {
        let program = Path::new(argv[0])
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(argv[0]);
        if matches!(program, "bash" | "sh" | "zsh" | "fish")
            && matches!(argv[1], "-c" | "-lc" | "-ic")
        {
            return argv[2].to_string();
        }
    }
    argv.join(" ")
}

fn rollout_cwd(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let mut buffer = vec![0; 1024 * 1024];
    let read = file.read(&mut buffer).ok()?;
    buffer.truncate(read);
    let line = buffer
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or(&buffer);
    let value: Value = serde_json::from_slice(line).ok()?;
    value
        .pointer("/payload/cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
        .map(str::to_string)
}
