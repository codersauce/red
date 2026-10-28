//! Claude Code JSONL transcripts under `~/.claude/projects`.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::SystemTime,
};

use serde_json::Value;

use super::{
    tools::{self, apply_hunks},
    within, SessionData, Tail, WatchOptions, Watcher,
};

pub(crate) struct ClaudeState {
    pub sub: bool,
}

pub(crate) fn scan(watcher: &mut Watcher, options: &WatchOptions, cutoff: SystemTime) {
    if !options.claude_projects.is_dir() {
        return;
    }
    let prefixes = claude_prefixes(&options.root);
    let Ok(projects) = fs::read_dir(&options.claude_projects) else {
        watcher.note(
            "claude",
            format!(
                "claude: could not read {}",
                options.claude_projects.display()
            ),
        );
        return;
    };
    watcher.clear_note("claude");
    for project in projects.flatten() {
        let name = project.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !project.path().is_dir() || !prefix_matches(&prefixes, name) {
            continue;
        }
        let directory = project.path();
        let mut files = Vec::new();
        if let Ok(entries) = fs::read_dir(&directory) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") && path.is_file()
                {
                    files.push(path);
                    continue;
                }
                if !path.is_dir() {
                    continue;
                }
                let subagents = path.join("subagents");
                if let Ok(agents) = fs::read_dir(subagents) {
                    for agent in agents.flatten() {
                        let agent_path = agent.path();
                        if agent_path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                            files.push(agent_path);
                        }
                    }
                }
            }
        }
        for file in files {
            let key = format!("claude:{}", file.display());
            if watcher.has(&key) || !super::modified_after(&file, cutoff) {
                continue;
            }
            let Some(cwd) = transcript_cwd(&file) else {
                continue;
            };
            if !within(&options.root, Path::new(&cwd)) {
                continue;
            }
            let sub = file
                .components()
                .any(|component| component.as_os_str() == "subagents");
            let mut data = SessionData::new(key, "claude");
            data.cwd = PathBuf::from(&cwd);
            data.name = "claude".to_string();
            if sub {
                apply_subagent_meta(&mut data, &file);
            }
            watcher.insert(
                data,
                super::Extra::Claude(ClaudeState { sub }),
                Tail::new(file),
            );
        }
    }
}

pub(crate) fn ingest(data: &mut SessionData, state: &ClaudeState, line: &[u8]) {
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return;
    };
    if value.get("isSidechain").and_then(Value::as_bool) == Some(true) && !state.sub {
        return;
    }
    let time = super::parse_time(value.get("timestamp").and_then(Value::as_str));
    if let Some(cwd) = value.get("cwd").and_then(Value::as_str) {
        if !cwd.is_empty() {
            data.cwd = PathBuf::from(cwd);
        }
    }
    match value.get("type").and_then(Value::as_str).unwrap_or("") {
        "ai-title" => {
            if !state.sub {
                if let Some(title) = value.get("aiTitle").and_then(Value::as_str) {
                    if !title.is_empty() {
                        data.title = title.to_string();
                        data.touch(time);
                    }
                }
            }
        }
        "system" => {
            if value.get("subtype").and_then(Value::as_str) == Some("turn_duration") {
                data.end_turn(time, "");
            }
        }
        "assistant" => {
            if let Some(message) = value.get("message") {
                ingest_assistant(data, time, message);
            }
        }
        "user" => {
            if let Some(message) = value.get("message") {
                ingest_user(data, time, message, value.get("toolUseResult"));
            }
        }
        _ => {}
    }
}

fn ingest_assistant(data: &mut SessionData, time: Option<SystemTime>, message: &Value) {
    let Some(content) = message.get("content") else {
        return;
    };
    let mut saw_text = false;
    if let Some(text) = content.as_str() {
        data.say(time, text);
        saw_text = true;
    } else if let Some(blocks) = content.as_array() {
        for block in blocks {
            match block.get("type").and_then(Value::as_str).unwrap_or("") {
                "text" => {
                    data.say(
                        time,
                        block.get("text").and_then(Value::as_str).unwrap_or(""),
                    );
                    saw_text = true;
                }
                "tool_use" => {
                    let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                    let input = block.get("input").cloned().unwrap_or(Value::Null);
                    let mut events = tools::tool_events(name, &input, &data.cwd, false);
                    let call_id = block.get("id").and_then(Value::as_str).unwrap_or("");
                    for event in &mut events {
                        event.call_id = call_id.to_string();
                    }
                    data.add(time, events);
                }
                _ => data.touch(time),
            }
        }
    }
    let stop = message.get("stop_reason").and_then(Value::as_str) == Some("end_turn");
    if stop && saw_text && data.pending().is_none() {
        data.end_turn(time, "");
    }
}

fn ingest_user(
    data: &mut SessionData,
    time: Option<SystemTime>,
    message: &Value,
    tool_use_result: Option<&Value>,
) {
    let Some(content) = message.get("content") else {
        return;
    };
    if let Some(text) = content.as_str() {
        ingest_prompt(data, time, text, message_is_meta(message));
        return;
    }
    let Some(blocks) = content.as_array() else {
        return;
    };
    for block in blocks {
        match block.get("type").and_then(Value::as_str).unwrap_or("") {
            "text" => ingest_prompt(
                data,
                time,
                block.get("text").and_then(Value::as_str).unwrap_or(""),
                message_is_meta(message),
            ),
            "tool_result" => apply_result(data, time, block, tool_use_result),
            _ => {}
        }
    }
}

fn message_is_meta(message: &Value) -> bool {
    message.get("isMeta").and_then(Value::as_bool) == Some(true)
}

fn ingest_prompt(data: &mut SessionData, time: Option<SystemTime>, text: &str, meta: bool) {
    let text = text.trim();
    if meta || text.is_empty() {
        return;
    }
    if text.starts_with("[Request interrupted") {
        data.end_turn(time, "interrupted");
        return;
    }
    if text.starts_with('<') && !text.contains("command-name") {
        return;
    }
    let text = command_text(text);
    if text.is_empty() {
        return;
    }
    data.prompt(time, &text);
}

fn command_text(text: &str) -> String {
    let Some(name_at) = text.find("<command-name>") else {
        return text.to_string();
    };
    let rest = &text[name_at + "<command-name>".len()..];
    let Some(name) = rest.split("</command-name>").next() else {
        return text.to_string();
    };
    let args = text
        .split_once("<command-args>")
        .and_then(|(_, rest)| rest.split_once("</command-args>"))
        .map(|(args, _)| args.trim())
        .unwrap_or("");
    format!("{name} {args}").trim().to_string()
}

fn apply_result(
    data: &mut SessionData,
    time: Option<SystemTime>,
    block: &Value,
    tool_use_result: Option<&Value>,
) {
    let Some(call_id) = block.get("tool_use_id").and_then(Value::as_str) else {
        return;
    };
    let failed = block.get("is_error").and_then(Value::as_bool) == Some(true);
    let indexes = data.call_indexes(call_id);
    for index in indexes {
        let Some(event) = data.events.get_mut(index) else {
            continue;
        };
        if event.kind == super::Kind::Read {
            if let Some(file) = tool_use_result.and_then(|result| result.get("file")) {
                let start = file.get("startLine").and_then(Value::as_i64);
                let count = file.get("numLines").and_then(Value::as_i64);
                if let (Some(start), Some(count)) = (start, count) {
                    if count > 0 {
                        let line = start.saturating_sub(1).max(0) as usize;
                        event.line = Some(line);
                        event.end_line = Some(line + count as usize - 1);
                    }
                }
            }
        }
        if event.kind == super::Kind::Edit {
            if tool_use_result
                .and_then(|result| result.get("type"))
                .and_then(Value::as_str)
                == Some("create")
            {
                event.text = "write".to_string();
            }
            if let Some(patch) = tool_use_result.and_then(|result| result.get("structuredPatch")) {
                apply_hunks(event, patch);
            }
        }
        if event.outcome == super::Outcome::Pending {
            event.outcome = if failed {
                super::Outcome::Failed
            } else {
                super::Outcome::Succeeded
            };
        }
    }
    data.touch(time);
}

fn apply_subagent_meta(data: &mut SessionData, file: &Path) {
    data.name = "subagent".to_string();
    let meta = file.with_extension("meta.json");
    let Ok(bytes) = fs::read(meta) else {
        return;
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return;
    };
    if let Some(kind) = value.get("agentType").and_then(Value::as_str) {
        if !kind.is_empty() {
            data.name = kind.trim_end_matches("-purpose").to_ascii_lowercase();
        }
    }
    if let Some(title) = value.get("description").and_then(Value::as_str) {
        if !title.is_empty() {
            data.title = title.to_string();
        }
    }
}

fn transcript_cwd(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let mut buffer = vec![0; 256 * 1024];
    let read = file.read(&mut buffer).ok()?;
    buffer.truncate(read);
    for line in buffer.split(|byte| *byte == b'\n') {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if let Some(cwd) = value.get("cwd").and_then(Value::as_str) {
            if !cwd.is_empty() {
                return Some(cwd.to_string());
            }
        }
    }
    None
}

fn claude_prefixes(root: &Path) -> Vec<String> {
    let mut paths = vec![super::lexical(root)];
    if let Ok(canonical) = root.canonicalize() {
        paths.push(canonical);
    }
    let mut encoded: Vec<String> = paths.into_iter().map(|path| encode_claude(&path)).collect();
    encoded.sort();
    encoded.dedup();
    encoded
}

fn encode_claude(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

fn prefix_matches(prefixes: &[String], name: &str) -> bool {
    prefixes
        .iter()
        .any(|prefix| name == prefix || name.starts_with(&format!("{prefix}-")))
}
