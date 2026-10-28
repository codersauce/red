//! Gemini CLI chat logs under `~/.gemini/tmp/<project>/chats`.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::{Instant, SystemTime},
};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{tools, SessionData, Tail, WatchOptions, Watcher};

pub(crate) struct GeminiMemory {
    path: PathBuf,
    legacy: bool,
    mtime: Option<SystemTime>,
    messages: HashMap<String, GemMsg>,
    order: Vec<String>,
    last_change: Option<Instant>,
    published_end: bool,
}

struct GemMsg {
    timestamp: String,
    kind: String,
    content: Value,
    tool_calls: Vec<GemTool>,
}

struct GemTool {
    id: String,
    name: String,
    args: Value,
    status: String,
    result: Value,
}

pub(crate) struct GeminiRebuild {
    pub events: Vec<super::Event>,
    pub name: String,
    pub prompt: String,
    pub turn_ended: bool,
    pub started: Option<SystemTime>,
    pub updated: Option<SystemTime>,
}

impl GeminiMemory {
    fn new(path: PathBuf) -> Self {
        Self {
            legacy: path.extension().and_then(|ext| ext.to_str()) == Some("json"),
            path,
            mtime: None,
            messages: HashMap::new(),
            order: Vec::new(),
            last_change: None,
            published_end: false,
        }
    }

    pub(crate) fn read(&mut self, tail: &mut Tail) -> std::io::Result<bool> {
        if self.legacy {
            let modified = fs::metadata(&self.path)?.modified().ok();
            if modified == self.mtime {
                return Ok(false);
            }
            self.mtime = modified;
            let bytes = fs::read(&self.path)?;
            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                if let Some(messages) = value.get("messages").and_then(Value::as_array) {
                    self.replace_all(messages);
                    self.last_change = Some(Instant::now());
                    self.published_end = false;
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        let mut changed = false;
        for line in tail.read_lines()? {
            if apply_record(self, &line) {
                changed = true;
            }
        }
        if changed {
            self.last_change = Some(Instant::now());
            self.published_end = false;
        }
        Ok(changed || self.should_end())
    }

    pub(crate) fn should_end(&self) -> bool {
        !self.published_end && self.quiet_reply()
    }

    pub(crate) fn rebuild(&mut self, cwd: &Path, name: &str) -> GeminiRebuild {
        let ending = self.quiet_reply();
        self.published_end = ending;
        let mut events = Vec::new();
        let mut prompt = String::new();
        let mut started = None;
        let mut updated = None;
        let order = if self.order.len() > 300 {
            &self.order[self.order.len() - 300..]
        } else {
            self.order.as_slice()
        };
        for id in order {
            let Some(message) = self.messages.get(id) else {
                continue;
            };
            let time = super::parse_time(Some(message.timestamp.as_str()));
            if let Some(time) = time {
                started = Some(started.map_or(time, |current: SystemTime| current.min(time)));
                updated = Some(updated.map_or(time, |current: SystemTime| current.max(time)));
            }
            match message.kind.as_str() {
                "user" => {
                    let text = parts_text(&message.content);
                    if prompt.is_empty() {
                        prompt = super::one_line(&text, 120);
                    }
                    let mut event = super::Event::bare(super::Kind::You);
                    event.text = super::one_line(&text, 400);
                    event.time = time.unwrap_or(SystemTime::UNIX_EPOCH);
                    if !event.text.is_empty() {
                        events.push(event);
                    }
                }
                "gemini" => {
                    let text = parts_text(&message.content);
                    if !text.trim().is_empty() {
                        let mut event = super::Event::bare(super::Kind::Say);
                        event.text = super::one_line(&text, 400);
                        event.time = time.unwrap_or(SystemTime::UNIX_EPOCH);
                        events.push(event);
                    }
                    for call in &message.tool_calls {
                        let mut call_events = tools::tool_events(&call.name, &call.args, cwd, true);
                        for event in &mut call_events {
                            event.call_id = call.id.clone();
                            event.time = time.unwrap_or(SystemTime::UNIX_EPOCH);
                            apply_tool_status(event, call);
                        }
                        events.extend(call_events);
                    }
                }
                _ => {}
            }
        }
        if ending {
            let mut event = super::Event::bare(super::Kind::Done);
            event.text = "turn ended".to_string();
            event.time = updated.unwrap_or(SystemTime::UNIX_EPOCH);
            events.push(event);
        }
        GeminiRebuild {
            events,
            name: name.to_string(),
            prompt,
            turn_ended: ending,
            started,
            updated,
        }
    }

    fn replace_all(&mut self, messages: &[Value]) {
        self.messages.clear();
        self.order.clear();
        for message in messages {
            if let Some(parsed) = parse_msg(message) {
                self.insert(parsed);
            }
        }
    }

    fn insert(&mut self, message: ParsedMsg) {
        if !self.messages.contains_key(&message.id) {
            self.order.push(message.id.clone());
        }
        self.messages.insert(
            message.id,
            GemMsg {
                timestamp: message.timestamp,
                kind: message.kind,
                content: message.content,
                tool_calls: message.tool_calls,
            },
        );
    }

    fn quiet_reply(&self) -> bool {
        let Some(last_change) = self.last_change else {
            return false;
        };
        if last_change.elapsed() < std::time::Duration::from_secs(4) {
            return false;
        }
        let Some(id) = self.order.last() else {
            return false;
        };
        let Some(message) = self.messages.get(id) else {
            return false;
        };
        message.kind == "gemini"
            && !parts_text(&message.content).trim().is_empty()
            && message
                .tool_calls
                .iter()
                .all(|call| matches!(call.status.as_str(), "success" | "error" | "cancelled"))
    }
}

struct ParsedMsg {
    id: String,
    timestamp: String,
    kind: String,
    content: Value,
    tool_calls: Vec<GemTool>,
}

pub(crate) fn scan(watcher: &mut Watcher, options: &WatchOptions, cutoff: SystemTime) {
    if !options.gemini_tmp.is_dir() {
        return;
    }
    let hash = sha256_hex(&super::path_key(&options.root));
    let Ok(projects) = fs::read_dir(&options.gemini_tmp) else {
        watcher.note(
            "gemini",
            format!("gemini: could not read {}", options.gemini_tmp.display()),
        );
        return;
    };
    watcher.clear_note("gemini");
    for project in projects.flatten() {
        if !project.path().is_dir() {
            continue;
        }
        let directory = project.path();
        let root_file = fs::read_to_string(directory.join(".project_root"))
            .ok()
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
        let cwd = if let Some(root) = root_file {
            if !super::within(&options.root, Path::new(&root)) {
                continue;
            }
            PathBuf::from(root)
        } else if project.file_name().to_string_lossy() == hash {
            options.root.clone()
        } else {
            continue;
        };
        let chats = directory.join("chats");
        let mut files = Vec::new();
        if let Ok(entries) = fs::read_dir(&chats) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file()
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("session-"))
                    && matches!(
                        path.extension().and_then(|ext| ext.to_str()),
                        Some("json") | Some("jsonl")
                    )
                {
                    files.push(path);
                    continue;
                }
                if path.is_dir() {
                    if let Ok(nested) = fs::read_dir(&path) {
                        for child in nested.flatten() {
                            let child_path = child.path();
                            if child_path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                            {
                                files.push(child_path);
                            }
                        }
                    }
                }
            }
        }
        for file in files {
            let key = format!("gemini:{}", file.display());
            if watcher.has(&key) || !super::modified_after(&file, cutoff) {
                continue;
            }
            let mut data = SessionData::new(key, "gemini");
            data.cwd = cwd.clone();
            data.name = if file.parent() == Some(chats.as_path()) {
                "gemini".to_string()
            } else {
                "subagent".to_string()
            };
            watcher.insert(
                data,
                super::Extra::Gemini(GeminiMemory::new(file.clone())),
                Tail::new(file),
            );
        }
    }
}

fn apply_record(memory: &mut GeminiMemory, line: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return false;
    };
    if let Some(rewind) = value.get("$rewindTo").and_then(Value::as_str) {
        if let Some(index) = memory.order.iter().position(|id| id == rewind) {
            for id in memory.order.split_off(index) {
                memory.messages.remove(&id);
            }
        } else {
            memory.messages.clear();
            memory.order.clear();
        }
        return true;
    }
    if let Some(patch) = value.get("$patch") {
        apply_patch(memory, patch);
        return true;
    }
    if let Some(set) = value.get("$set") {
        if set.get("kind").and_then(Value::as_str) == Some("subagent") {
            // The session name is filled by the scanner; a subagent log stays labeled.
        }
        if let Some(messages) = set.get("messages").and_then(Value::as_array) {
            memory.replace_all(messages);
            return true;
        }
        return false;
    }
    if let Some(message) = parse_msg(&value) {
        memory.insert(message);
        return true;
    }
    false
}

fn apply_patch(memory: &mut GeminiMemory, patch: &Value) {
    if let Some(id) = patch.get("id").and_then(Value::as_str) {
        patch_message(memory, id, patch);
    }
    if let Some(updates) = patch.get("updates").and_then(Value::as_array) {
        for update in updates {
            if let Some(id) = update.get("id").and_then(Value::as_str) {
                patch_message(memory, id, update);
            }
        }
    }
    if let Some(remove) = patch.get("removeIds").and_then(Value::as_array) {
        for id in remove.iter().filter_map(Value::as_str) {
            memory.messages.remove(id);
            memory.order.retain(|existing| existing != id);
        }
    }
}

fn patch_message(memory: &mut GeminiMemory, id: &str, patch: &Value) {
    let Some(message) = memory.messages.get_mut(id) else {
        return;
    };
    if let Some(content) = patch.get("content") {
        message.content = content.clone();
    }
    if let Some(calls) = patch.get("toolCalls").and_then(Value::as_array) {
        for call in calls {
            let Some(call_id) = call.get("id").and_then(Value::as_str) else {
                continue;
            };
            if let Some(existing) = message
                .tool_calls
                .iter_mut()
                .find(|tool| tool.id == call_id)
            {
                if let Some(result) = call.get("result") {
                    existing.result = result.clone();
                }
                if let Some(status) = call.get("status").and_then(Value::as_str) {
                    existing.status = status.to_string();
                }
            }
        }
    }
}

fn parse_msg(value: &Value) -> Option<ParsedMsg> {
    let id = value.get("id").and_then(Value::as_str)?.to_string();
    if id.is_empty() {
        return None;
    }
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let content = if kind == "user" {
        value
            .get("displayContent")
            .cloned()
            .filter(|content| !content.is_null())
            .unwrap_or_else(|| value.get("content").cloned().unwrap_or(Value::Null))
    } else {
        value.get("content").cloned().unwrap_or(Value::Null)
    };
    let tool_calls = value
        .get("toolCalls")
        .and_then(Value::as_array)
        .map(|calls| calls.iter().filter_map(parse_tool).collect())
        .unwrap_or_default();
    Some(ParsedMsg {
        id,
        timestamp: value
            .get("timestamp")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        kind,
        content,
        tool_calls,
    })
}

fn parse_tool(value: &Value) -> Option<GemTool> {
    Some(GemTool {
        id: value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        name: value.get("name").and_then(Value::as_str)?.to_string(),
        args: value.get("args").cloned().unwrap_or(Value::Null),
        status: value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        result: value.get("result").cloned().unwrap_or(Value::Null),
    })
}

fn apply_tool_status(event: &mut super::Event, call: &GemTool) {
    match call.status.as_str() {
        "success" => event.outcome = super::Outcome::Succeeded,
        "error" | "cancelled" => event.outcome = super::Outcome::Failed,
        "awaiting_approval" => event.approval = true,
        _ => {}
    }
}

fn parts_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| match part {
                Value::String(text) => Some(text.clone()),
                Value::Object(_) => part
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|_| part.get("thought").and_then(Value::as_bool) != Some(true))
                    .map(str::to_string),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
