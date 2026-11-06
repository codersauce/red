//! Live view of external coding-agent sessions.
//!
//! Red tails the session logs that Claude Code, Codex CLI, and Gemini CLI
//! already write and turns tool calls into file positions. The session
//! locations, tool names, and activity model are adapted from
//! [shoulder](https://github.com/bjarneo/shoulder) by Bjarne Øverli,
//! Copyright (c) 2026 bjarneo, MIT License. This is a separate Rust
//! implementation for Red's editor; it does not include shoulder source.
//!
//! opencode keeps sessions in SQLite rather than an append-only log, so it
//! is not followed here.

mod claude;
mod codex;
mod gemini;
mod tools;

use std::{
    collections::{BTreeMap, HashMap},
    env, fs,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use path_absolutize::Absolutize;

const MAX_EVENTS: usize = 600;
const KEEP_EVENTS: usize = 400;
const TAIL_INITIAL: u64 = 8 << 20;
const MAX_LINE: usize = 32 << 20;
const SCAN_EVERY: Duration = Duration::from_secs(3);

/// Where to find agent logs for one project.
#[derive(Clone, Debug)]
pub struct WatchOptions {
    /// Git top level, or the directory the user asked to watch.
    pub root: PathBuf,
    /// Ignore session files that have not changed within this window.
    pub since: Duration,
    /// Claude Code `projects` directory.
    pub claude_projects: PathBuf,
    /// Codex `sessions` directory.
    pub codex_sessions: PathBuf,
    /// Gemini CLI `tmp` directory.
    pub gemini_tmp: PathBuf,
}

impl WatchOptions {
    /// Session directories for the project, honoring each agent's home override.
    pub fn discover(root: PathBuf) -> Self {
        let home = home_dir();
        let claude_base = env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude"));
        let codex_base = env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"));
        let gemini_base = match env::var_os("GEMINI_CLI_HOME") {
            Some(path) => PathBuf::from(path).join(".gemini"),
            None => home.join(".gemini"),
        };
        Self {
            root,
            since: Duration::from_secs(60 * 60),
            claude_projects: claude_base.join("projects"),
            codex_sessions: codex_base.join("sessions"),
            gemini_tmp: gemini_base.join("tmp"),
        }
    }
}

/// Git top level containing `start`, or `start` itself when it is not in a work tree.
pub fn project_root(start: &Path) -> PathBuf {
    let mut dir = canonical_or_lexical(start);
    let origin = dir.clone();
    loop {
        if dir.join(".git").exists() {
            return dir;
        }
        if !dir.pop() {
            return origin;
        }
    }
}

/// One polling pass over the session logs for a project.
pub struct Watcher {
    options: WatchOptions,
    slots: HashMap<String, Slot>,
    notes: BTreeMap<String, String>,
    last_scan: Option<Instant>,
}

impl Watcher {
    pub fn new(options: WatchOptions) -> Self {
        Self {
            options,
            slots: HashMap::new(),
            notes: BTreeMap::new(),
            last_scan: None,
        }
    }

    /// Reads new session bytes. Safe to call several times a second.
    pub fn poll(&mut self) {
        let due = self
            .last_scan
            .map(|scan| scan.elapsed() >= SCAN_EVERY)
            .unwrap_or(true);
        if due {
            self.last_scan = Some(Instant::now());
            let cutoff = SystemTime::now()
                .checked_sub(self.options.since)
                .unwrap_or(UNIX_EPOCH);
            let options = self.options.clone();
            claude::scan(self, &options, cutoff);
            codex::scan(self, &options, cutoff);
            gemini::scan(self, &options, cutoff);
        }
        let keys: Vec<String> = self.slots.keys().cloned().collect();
        for key in keys {
            let Some(slot) = self.slots.get_mut(&key) else {
                continue;
            };
            let source = slot.data.source;
            let polled = poll_slot(slot).map_err(|error| format!("{source}: {error}"));
            match polled {
                Ok(()) => {
                    self.notes.remove(&key);
                }
                Err(message) => {
                    self.notes.insert(key, message);
                }
            }
        }
    }

    /// Agents with at least one parsed event, oldest session first.
    pub fn view(&self) -> WatchView {
        let now = SystemTime::now();
        let mut agents: Vec<AgentView> = self
            .slots
            .values()
            .filter(|slot| !slot.data.events.is_empty())
            .map(|slot| summarize(&slot.data, now))
            .collect();
        agents.sort_by(|left, right| {
            left.started
                .cmp(&right.started)
                .then_with(|| left.key.cmp(&right.key))
        });
        for (index, agent) in agents.iter_mut().enumerate() {
            agent.label = format!("A{} {}", index + 1, agent.name);
        }
        WatchView {
            agents,
            notes: self.notes.values().cloned().collect(),
        }
    }

    pub(crate) fn has(&self, key: &str) -> bool {
        self.slots.contains_key(key)
    }

    pub(crate) fn note(&mut self, key: &str, message: String) {
        self.notes.insert(key.to_string(), message);
    }

    pub(crate) fn clear_note(&mut self, key: &str) {
        self.notes.remove(key);
    }

    pub(crate) fn insert(&mut self, data: SessionData, extra: Extra, tail: Tail) {
        let key = data.key.clone();
        self.slots.insert(key, Slot { data, tail, extra });
    }
}

/// A point-in-time picture of every matched agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchView {
    pub agents: Vec<AgentView>,
    pub notes: Vec<String>,
}

/// What one agent is doing, and the file Red should show for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentView {
    pub key: String,
    pub label: String,
    pub source: &'static str,
    pub name: String,
    pub task: String,
    pub activity: String,
    pub detail: String,
    pub file: Option<PathBuf>,
    pub line: Option<usize>,
    pub needle: String,
    pub span: usize,
    pub touch: Option<TouchKind>,
    pub busy: bool,
    pub started: SystemTime,
    pub touched: SystemTime,
}

/// Whether the file position came from a read or an edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchKind {
    Read,
    Edit,
}

#[derive(Clone, Debug)]
pub(crate) struct Event {
    pub time: SystemTime,
    pub kind: Kind,
    pub tool: String,
    pub call_id: String,
    pub file: PathBuf,
    pub line: Option<usize>,
    pub end_line: Option<usize>,
    pub needle: String,
    pub query: String,
    pub cmd: String,
    pub text: String,
    pub add: i32,
    pub del: i32,
    pub outcome: Outcome,
    pub approval: bool,
    pub deleted: bool,
}

impl Event {
    pub(crate) fn bare(kind: Kind) -> Self {
        Self {
            time: UNIX_EPOCH,
            kind,
            tool: String::new(),
            call_id: String::new(),
            file: PathBuf::new(),
            line: None,
            end_line: None,
            needle: String::new(),
            query: String::new(),
            cmd: String::new(),
            text: String::new(),
            add: 0,
            del: 0,
            outcome: Outcome::Pending,
            approval: false,
            deleted: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    You,
    Say,
    Read,
    Grep,
    List,
    Edit,
    Run,
    Web,
    Task,
    Plan,
    Ask,
    Tool,
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Pending,
    Succeeded,
    Failed,
}

pub(crate) struct SessionData {
    pub key: String,
    pub source: &'static str,
    pub name: String,
    pub title: String,
    pub prompt: String,
    pub cwd: PathBuf,
    pub started: Option<SystemTime>,
    pub updated: Option<SystemTime>,
    pub events: Vec<Event>,
    calls: HashMap<String, Vec<usize>>,
    pub turn_ended: bool,
}

impl SessionData {
    pub(crate) fn new(key: String, source: &'static str) -> Self {
        Self {
            key,
            source,
            name: source.to_string(),
            title: String::new(),
            prompt: String::new(),
            cwd: PathBuf::new(),
            started: None,
            updated: None,
            events: Vec::new(),
            calls: HashMap::new(),
            turn_ended: false,
        }
    }

    pub(crate) fn add(&mut self, time: Option<SystemTime>, events: Vec<Event>) {
        for mut event in events {
            if event.time == UNIX_EPOCH {
                if let Some(time) = time {
                    event.time = time;
                }
            }
            let stamp = (event.time != UNIX_EPOCH).then_some(event.time).or(time);
            if !event.call_id.is_empty() {
                let index = self.events.len();
                self.calls
                    .entry(event.call_id.clone())
                    .or_default()
                    .push(index);
            }
            self.events.push(event);
            self.touch(stamp);
        }
        if self.events.len() > MAX_EVENTS {
            let drop_count = self.events.len() - KEEP_EVENTS;
            self.events.drain(..drop_count);
            self.reindex();
        }
    }

    pub(crate) fn prompt(&mut self, time: Option<SystemTime>, text: &str) {
        let text = one_line(text, 400);
        if text.is_empty() {
            return;
        }
        if self.prompt.is_empty() {
            self.prompt = one_line(&text, 120);
        }
        self.turn_ended = false;
        let mut event = Event::bare(Kind::You);
        event.text = text;
        self.add(time, vec![event]);
    }

    pub(crate) fn say(&mut self, time: Option<SystemTime>, text: &str) {
        let text = one_line(text, 400);
        if text.is_empty() {
            return;
        }
        let mut event = Event::bare(Kind::Say);
        event.text = text;
        self.add(time, vec![event]);
    }

    pub(crate) fn end_turn(&mut self, time: Option<SystemTime>, text: &str) {
        if self.turn_ended {
            return;
        }
        self.turn_ended = true;
        let text = if text.trim().is_empty() {
            last_say(self).unwrap_or_else(|| "turn ended".to_string())
        } else {
            one_line(text, 200)
        };
        let mut event = Event::bare(Kind::Done);
        event.text = text;
        self.add(time, vec![event]);
    }

    pub(crate) fn touch(&mut self, time: Option<SystemTime>) {
        let Some(time) = time else {
            return;
        };
        if self.updated.is_none_or(|updated| time > updated) {
            self.updated = Some(time);
        }
        if self.started.is_none_or(|started| time < started) {
            self.started = Some(time);
        }
    }

    pub(crate) fn call_indexes(&self, id: &str) -> Vec<usize> {
        self.calls.get(id).cloned().unwrap_or_default()
    }

    pub(crate) fn pending(&self) -> Option<usize> {
        for (index, event) in self.events.iter().enumerate().rev() {
            if matches!(event.kind, Kind::You | Kind::Done) {
                return None;
            }
            if is_tool(event.kind) && event.outcome == Outcome::Pending {
                return Some(index);
            }
        }
        None
    }

    fn reindex(&mut self) {
        self.calls.clear();
        for (index, event) in self.events.iter().enumerate() {
            if !event.call_id.is_empty() {
                self.calls
                    .entry(event.call_id.clone())
                    .or_default()
                    .push(index);
            }
        }
    }

    fn apply_gemini(&mut self, rebuild: gemini::GeminiRebuild) {
        self.events = rebuild.events;
        self.reindex();
        if !rebuild.name.is_empty() {
            self.name = rebuild.name;
        }
        if self.prompt.is_empty() {
            self.prompt = rebuild.prompt;
        }
        self.turn_ended = rebuild.turn_ended;
        self.started = rebuild.started.or(self.started);
        self.updated = rebuild.updated.or(self.updated);
    }
}

struct Slot {
    data: SessionData,
    tail: Tail,
    extra: Extra,
}

pub(crate) enum Extra {
    Claude(claude::ClaudeState),
    Codex(codex::CodexState),
    Gemini(gemini::GeminiMemory),
}

fn poll_slot(slot: &mut Slot) -> io::Result<()> {
    if matches!(slot.extra, Extra::Gemini(_)) {
        let refresh = {
            let Slot { tail, extra, .. } = &mut *slot;
            let Extra::Gemini(memory) = extra else {
                unreachable!("gemini session");
            };
            memory.read(tail)?
        };
        if refresh {
            let rebuilt = {
                let Extra::Gemini(memory) = &mut slot.extra else {
                    unreachable!("gemini session");
                };
                memory.rebuild(&slot.data.cwd, &slot.data.name)
            };
            slot.data.apply_gemini(rebuilt);
        }
        return Ok(());
    }

    let lines = slot.tail.read_lines()?;
    let Slot { data, extra, .. } = slot;
    match extra {
        Extra::Claude(state) => {
            for line in lines {
                claude::ingest(data, state, &line);
            }
        }
        Extra::Codex(state) => {
            for line in lines {
                codex::ingest(data, state, &line);
            }
        }
        Extra::Gemini(_) => unreachable!("gemini sessions are polled above"),
    }
    Ok(())
}

pub(crate) struct Tail {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
    started: bool,
}

impl Tail {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            offset: 0,
            partial: Vec::new(),
            started: false,
        }
    }

    pub(crate) fn read_lines(&mut self) -> io::Result<Vec<Vec<u8>>> {
        let mut file = fs::File::open(&self.path)?;
        let size = file.metadata()?.len();
        if size < self.offset {
            self.offset = 0;
            self.partial.clear();
            self.started = false;
        }
        let mut skip_first = false;
        if !self.started {
            self.started = true;
            if size > TAIL_INITIAL {
                self.offset = size - TAIL_INITIAL;
                skip_first = true;
            }
        }
        if size == self.offset {
            return Ok(Vec::new());
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut buf = Vec::new();
        file.take(size - self.offset).read_to_end(&mut buf)?;
        self.offset += buf.len() as u64;
        if skip_first {
            if let Some(index) = buf.iter().position(|byte| *byte == b'\n') {
                buf.drain(..=index);
            } else {
                return Ok(Vec::new());
            }
        }
        if !self.partial.is_empty() {
            let mut combined = std::mem::take(&mut self.partial);
            combined.append(&mut buf);
            buf = combined;
        }
        let mut lines = Vec::new();
        while let Some(index) = buf.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = buf.drain(..=index).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.iter().any(|byte| !byte.is_ascii_whitespace()) {
                lines.push(line);
            }
        }
        if !buf.is_empty() && buf.len() < MAX_LINE {
            self.partial = buf;
        }
        Ok(lines)
    }
}

fn summarize(data: &SessionData, now: SystemTime) -> AgentView {
    let (activity, detail, busy) = describe(data, now);
    let position = file_position(data);
    let touch = position.map(|event| match event.kind {
        Kind::Edit => TouchKind::Edit,
        _ => TouchKind::Read,
    });
    let span = position
        .map(|event| match (event.line, event.end_line) {
            (Some(start), Some(end)) if end >= start => (end - start + 1).clamp(1, 12),
            _ if event.add > 1 => (event.add as usize).clamp(1, 12),
            _ => 1,
        })
        .unwrap_or(1);
    AgentView {
        key: data.key.clone(),
        label: data.name.clone(),
        source: data.source,
        name: data.name.clone(),
        task: task(data),
        activity,
        detail,
        file: position
            .filter(|event| !event.file.as_os_str().is_empty())
            .map(|event| event.file.clone()),
        line: position.and_then(|event| event.line),
        needle: position
            .map(|event| event.needle.clone())
            .unwrap_or_default(),
        span,
        touch,
        busy,
        started: data.started.unwrap_or(UNIX_EPOCH),
        touched: position
            .map(|event| event.time)
            .or(data.updated)
            .unwrap_or(UNIX_EPOCH),
    }
}

fn describe(data: &SessionData, now: SystemTime) -> (String, String, bool) {
    if data.turn_ended {
        let detail = last_say(data).unwrap_or_else(|| "turn ended".to_string());
        return ("done".to_string(), detail, false);
    }
    if let Some(index) = data.pending() {
        let event = &data.events[index];
        if event.approval || (approval_guess(event.kind) && age(now, event.time).as_secs() >= 20) {
            return (
                "waiting".to_string(),
                format!("approve {}", event.tool),
                true,
            );
        }
        return (act_of(event.kind), detail_of(event, &data.cwd), true);
    }
    if age(now, data.updated.unwrap_or(UNIX_EPOCH)).as_secs() >= 10 * 60 {
        return ("idle".to_string(), "quiet".to_string(), false);
    }
    if let Some(event) = data.events.last() {
        if is_tool(event.kind) && age(now, event.time).as_secs() < 2 {
            return (act_of(event.kind), detail_of(event, &data.cwd), true);
        }
    }
    let detail = file_position(data)
        .map(|event| short_path(&event.file, &data.cwd))
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| "…".to_string());
    ("thinking".to_string(), detail, true)
}

fn file_position(data: &SessionData) -> Option<&Event> {
    data.events.iter().rev().find(|event| {
        !event.file.as_os_str().is_empty()
            && matches!(event.kind, Kind::Read | Kind::Edit)
            && !event.deleted
    })
}

fn last_say(data: &SessionData) -> Option<String> {
    for event in data.events.iter().rev() {
        match event.kind {
            Kind::Say if !event.text.is_empty() => return Some(event.text.clone()),
            Kind::You => return None,
            _ => {}
        }
    }
    None
}

fn task(data: &SessionData) -> String {
    if !data.title.is_empty() {
        return one_line(&data.title, 120);
    }
    if !data.prompt.is_empty() {
        return one_line(&data.prompt, 120);
    }
    "no prompt yet".to_string()
}

fn is_tool(kind: Kind) -> bool {
    !matches!(kind, Kind::You | Kind::Say | Kind::Done)
}

fn approval_guess(kind: Kind) -> bool {
    !matches!(
        kind,
        Kind::Run | Kind::Task | Kind::Web | Kind::Ask | Kind::You | Kind::Say | Kind::Done
    )
}

fn act_of(kind: Kind) -> String {
    match kind {
        Kind::Read => "reading",
        Kind::Grep | Kind::List => "searching",
        Kind::Edit => "editing",
        Kind::Run => "running",
        Kind::Web => "browsing",
        Kind::Task => "delegating",
        Kind::Plan => "planning",
        Kind::Ask => "waiting",
        _ => "working",
    }
    .to_string()
}

fn detail_of(event: &Event, cwd: &Path) -> String {
    match event.kind {
        Kind::Read | Kind::Edit => {
            let mut text = short_path(&event.file, cwd);
            if text.is_empty() {
                text = event.tool.clone();
            }
            if event.add > 0 || event.del > 0 {
                text.push_str(&format!(" +{} -{}", event.add.max(0), event.del.max(0)));
            }
            if let Some(line) = event.line {
                text.push(':');
                text.push_str(&(line + 1).to_string());
            }
            text
        }
        Kind::Run => format!("$ {}", one_line(&event.cmd, 80)),
        Kind::Grep => format!("grep \"{}\"", one_line(&event.query, 60)),
        _ => one_line(&event.text, 80),
    }
}

fn short_path(path: &Path, cwd: &Path) -> String {
    if path.as_os_str().is_empty() {
        return String::new();
    }
    path.strip_prefix(cwd)
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .map(|relative| relative.display().to_string())
        .or_else(|| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| path.display().to_string())
}

pub(crate) fn one_line(text: &str, max: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = text.chars().count();
    if count <= max {
        return text;
    }
    if max == 0 {
        return String::new();
    }
    let mut clipped: String = text.chars().take(max - 1).collect();
    clipped.push('…');
    clipped
}

fn age(now: SystemTime, then: SystemTime) -> Duration {
    now.duration_since(then).unwrap_or_default()
}

pub(crate) fn parse_time(value: Option<&str>) -> Option<SystemTime> {
    let value = value?;
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| SystemTime::from(time.to_utc()))
}

pub(crate) fn modified_after(path: &Path, cutoff: SystemTime) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .map(|modified| modified >= cutoff)
        .unwrap_or(false)
}

pub(crate) fn within(root: &Path, dir: &Path) -> bool {
    if dir.as_os_str().is_empty() {
        return false;
    }
    let roots = path_forms(root);
    let dirs = path_forms(dir);
    dirs.iter().any(|dir| {
        roots
            .iter()
            .any(|root| dir == root || dir.starts_with(root))
    })
}

pub(crate) fn path_key(path: &Path) -> String {
    let text = lexical(path).to_string_lossy().into_owned();
    if text.len() > 1 {
        text.trim_end_matches(['/', '\\']).to_string()
    } else {
        text
    }
}

fn path_forms(path: &Path) -> Vec<PathBuf> {
    let lexical = lexical(path);
    let mut forms = vec![lexical.clone()];
    if let Ok(canonical) = path.canonicalize() {
        if canonical != lexical {
            forms.push(canonical);
        }
    }
    forms
}

pub(crate) fn lexical(path: &Path) -> PathBuf {
    path.absolutize()
        .map(|path| path.into_owned())
        .unwrap_or_else(|_| path.to_path_buf())
}

fn canonical_or_lexical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| lexical(path))
}

pub(crate) fn abs_path(path: &str, cwd: &Path) -> PathBuf {
    if path.is_empty() {
        return PathBuf::new();
    }
    let expanded = if let Some(rest) = path.strip_prefix("~/") {
        home_dir().join(rest)
    } else if path == "~" {
        home_dir()
    } else {
        PathBuf::from(path)
    };
    let joined = if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    };
    lexical(&joined)
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;
    use serde_json::json;

    fn stamp() -> String {
        chrono::Utc::now().to_rfc3339()
    }

    fn options(root: &Path) -> WatchOptions {
        WatchOptions {
            root: root.to_path_buf(),
            since: Duration::from_secs(60 * 60),
            claude_projects: root.join("homes/claude/projects"),
            codex_sessions: root.join("homes/codex/sessions"),
            gemini_tmp: root.join("homes/gemini/tmp"),
        }
    }

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    #[test]
    fn project_root_stops_at_git_and_partial_lines_wait_for_newline() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join("sub")).unwrap();
        fs::create_dir(repo.join(".git")).unwrap();
        assert_eq!(
            project_root(&repo.join("sub")),
            repo.canonicalize().unwrap()
        );

        let root = repo;
        let file = root.join("snake.js");
        write(&file, "alpha\nfn brand_new_line() {}\nomega\n");
        let encoded: String = root
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
            .collect();
        let transcript = options(&root)
            .claude_projects
            .join(&encoded)
            .join("session.jsonl");
        let cwd = root.canonicalize().unwrap();
        write(
            &transcript,
            &format!(
                "{}\n",
                json!({
                    "type": "user",
                    "timestamp": stamp(),
                    "cwd": cwd,
                    "message": {"content": "Simplify snake.js"}
                })
            ),
        );
        let mut watcher = Watcher::new(options(&root));
        watcher.poll();
        assert_eq!(watcher.view().agents.len(), 1);
        assert!(watcher.view().agents[0].file.is_none());

        let edit = json!({
            "type": "assistant",
            "timestamp": stamp(),
            "cwd": cwd,
            "message": {
                "content": [{
                    "type": "tool_use",
                    "id": "edit-1",
                    "name": "Edit",
                    "input": {
                        "file_path": file,
                        "old_string": "old",
                        "new_string": "fn brand_new_line() {}"
                    }
                }]
            }
        })
        .to_string();
        let (partial, rest) = edit.split_at(edit.len() / 2);
        let mut contents = fs::read_to_string(&transcript).unwrap();
        contents.push_str(partial);
        fs::write(&transcript, &contents).unwrap();
        watcher.poll();
        assert!(watcher.view().agents[0].file.is_none());

        contents.push_str(rest);
        contents.push('\n');
        fs::write(&transcript, contents).unwrap();
        watcher.poll();
        let agent = &watcher.view().agents[0];
        assert_eq!(agent.activity, "editing");
        assert_eq!(agent.touch, Some(TouchKind::Edit));
        assert_eq!(agent.needle, "fn brand_new_line() {}");
        assert_eq!(agent.file.as_deref(), Some(lexical(&file).as_path()));
    }

    #[test]
    fn codex_and_gemini_reads_follow_the_project_and_ignore_other_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("proj");
        fs::create_dir_all(&root).unwrap();
        let watched = options(&root);
        let day = chrono::Local::now().date_naive();
        let rollout = watched.codex_sessions.join(format!(
            "{:04}/{:02}/{:02}/rollout-test.jsonl",
            day.year(),
            day.month(),
            day.day()
        ));
        write(
            &rollout,
            &format!(
                "{}\n{}\n{}\n",
                json!({
                    "timestamp": stamp(),
                    "type": "session_meta",
                    "payload": {"cwd": root, "type": "session_meta"}
                }),
                json!({
                    "timestamp": stamp(),
                    "type": "event_msg",
                    "payload": {"type": "user_message", "message": "Read the snake"}
                }),
                json!({
                    "timestamp": stamp(),
                    "type": "response_item",
                    "payload": {
                        "type": "function_call",
                        "name": "shell_command",
                        "call_id": "shell-1",
                        "arguments": json!({"command": "head -n 2 snake.js"}).to_string()
                    }
                })
            ),
        );
        let outside = watched.codex_sessions.join(format!(
            "{:04}/{:02}/{:02}/rollout-outside.jsonl",
            day.year(),
            day.month(),
            day.day()
        ));
        write(
            &outside,
            &format!(
                "{}\n",
                json!({
                    "timestamp": stamp(),
                    "type": "session_meta",
                    "payload": {"cwd": "/somewhere/else", "type": "session_meta"}
                })
            ),
        );

        let gemini_dir = watched.gemini_tmp.join("snake-proj");
        write(
            &gemini_dir.join(".project_root"),
            &format!("{}\n", root.display()),
        );
        write(
            &gemini_dir.join("chats/session-1.jsonl"),
            &format!(
                "{}\n",
                json!({
                    "id": "m1",
                    "timestamp": stamp(),
                    "type": "gemini",
                    "content": [{"text": "Looking"}],
                    "toolCalls": [{
                        "id": "g1",
                        "name": "read_file",
                        "status": "success",
                        "args": {"file_path": root.join("snake.js"), "offset": 1, "limit": 2}
                    }]
                })
            ),
        );

        let mut watcher = Watcher::new(watched);
        watcher.poll();
        let view = watcher.view();
        assert_eq!(view.agents.len(), 2);
        let codex = view
            .agents
            .iter()
            .find(|agent| agent.source == "codex")
            .unwrap();
        assert_eq!(codex.touch, Some(TouchKind::Read));
        assert_eq!(codex.line, Some(0));
        assert_eq!(codex.end_line_span(), 2);
        assert_eq!(codex.file.as_deref(), Some(root.join("snake.js").as_path()));
        let gemini = view
            .agents
            .iter()
            .find(|agent| agent.source == "gemini")
            .unwrap();
        assert_eq!(gemini.touch, Some(TouchKind::Read));
        assert_eq!(gemini.line, Some(1));
    }

    impl AgentView {
        fn end_line_span(&self) -> usize {
            self.span
        }
    }
}
