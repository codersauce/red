//! Read-only follow mode for an external coding agent.
//!
//! `:agent-watch` and `--agent-watch` tail Claude Code, Codex CLI, and Gemini
//! CLI session logs. While follow is on, Red opens the file the agent is
//! reading or editing, moves to the changed lines, and tints them. `q` leaves
//! the mode and restores normal editing.

use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};

use crate::{
    agent_watch::{AgentView, TouchKind, WatchOptions, WatchView, Watcher},
    config::KeyAction,
    plugin::Runtime,
    utils::same_file_path,
};

use super::{Action, Editor, Mode, RenderBuffer};

const POLL_EVERY: Duration = Duration::from_millis(400);
const JUMP_GAP: Duration = Duration::from_millis(1500);
const HELP: &str = "agent watch is read-only — q leaves, f toggles follow, esc follows again, 1-9 pins, a follows every agent";

pub(super) struct FollowSession {
    watcher: Watcher,
    root: PathBuf,
    following: bool,
    pinned: Option<String>,
    pending: Option<HashMap<String, KeyAction>>,
    last_poll: Option<Instant>,
    last_jump_file: Option<PathBuf>,
    last_jump_at: Option<Instant>,
    highlight: Option<WatchHighlight>,
    activity: String,
    mode_label: String,
    roster: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WatchHighlight {
    pub path: PathBuf,
    pub start_line: usize,
    pub end_line: usize,
    pub kind: TouchKind,
}

impl Editor {
    /// Follow agents working in `path`, or in the current directory when absent.
    pub fn begin_agent_watch(&mut self, path: Option<PathBuf>) -> anyhow::Result<()> {
        let root = resolve_watch_root(path)?;
        self.begin_agent_watch_with(WatchOptions::discover(root));
        Ok(())
    }

    pub(super) fn begin_agent_watch_with(&mut self, options: WatchOptions) {
        let root = options.root.clone();
        self.mode = Mode::Normal;
        self.selection = None;
        self.selection_start = None;
        self.multi_cursor = None;
        self.splash_dismissed = true;
        self.welcome_startup_pending = false;
        self.suppress_startup_whats_new();
        self.waiting_key_action = None;
        self.waiting_command = None;
        self.agent_watch = Some(FollowSession {
            watcher: Watcher::new(options),
            root: root.clone(),
            following: true,
            pinned: None,
            pending: None,
            last_poll: None,
            last_jump_file: None,
            last_jump_at: None,
            highlight: None,
            activity: String::new(),
            mode_label: "FOLLOW".to_string(),
            roster: Vec::new(),
        });
        self.set_legacy_message(Some(format!(
            "following agents in {} — q leaves, f scrolls freely, 1-9 pins",
            root.display()
        )));
    }

    pub(super) fn stop_agent_watch(&mut self) {
        if self.agent_watch.is_none() {
            return;
        }
        self.agent_watch = None;
        self.set_legacy_message(Some("agent watch off — editing restored".to_string()));
    }

    pub(super) fn agent_watch_command(&mut self, args: &str) -> Vec<Action> {
        let args = args.trim();
        let result = if matches!(args, "stop" | "off" | "quit") {
            self.stop_agent_watch();
            Ok(())
        } else if args.is_empty() {
            self.begin_agent_watch(None)
        } else {
            self.begin_agent_watch(Some(PathBuf::from(args)))
        };
        if let Err(error) = result {
            self.set_legacy_message(Some(error.to_string()));
        }
        vec![Action::EnterMode(Mode::Normal)]
    }

    pub(super) fn agent_watch_mode_name(&self) -> Option<String> {
        self.agent_watch
            .as_ref()
            .map(|session| session.mode_label.clone())
    }

    pub(super) fn agent_watch_status_line(&self) -> Option<String> {
        self.agent_watch
            .as_ref()
            .and_then(|session| (!session.activity.is_empty()).then(|| session.activity.clone()))
    }

    #[cfg(test)]
    pub(super) fn expire_agent_watch_poll(&mut self) {
        if let Some(session) = self.agent_watch.as_mut() {
            session.last_poll = None;
        }
    }

    pub(super) fn agent_watch_highlight(&self) -> Option<WatchHighlight> {
        self.agent_watch
            .as_ref()
            .and_then(|session| session.highlight.clone())
    }

    pub(super) async fn poll_agent_watch(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let due = self.agent_watch.as_ref().is_some_and(|session| {
            session
                .last_poll
                .map(|poll| poll.elapsed() >= POLL_EVERY)
                .unwrap_or(true)
        });
        if !due {
            return Ok(());
        }
        let view = {
            let session = self
                .agent_watch
                .as_mut()
                .expect("due check keeps the session");
            session.watcher.poll();
            session.last_poll = Some(Instant::now());
            let view = session.watcher.view();
            session.roster = view.agents.iter().map(|agent| agent.key.clone()).collect();
            view
        };
        self.apply_agent_watch_view(view, buffer, runtime).await
    }

    async fn apply_agent_watch_view(
        &mut self,
        view: WatchView,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let Some(session) = self.agent_watch.as_ref() else {
            return Ok(());
        };
        let pinned = session.pinned.clone();
        let following = session.following;
        let focus = choose_agent(&view.agents, pinned.as_deref()).cloned();
        let mut activity = status_text(&view, focus.as_ref());
        if view.agents.is_empty() {
            if let Some(session) = self.agent_watch.as_ref() {
                activity.push_str(" in ");
                activity.push_str(&session.root.display().to_string());
            }
        }
        let mut dirty = false;
        if let Some(session) = self.agent_watch.as_mut() {
            if session.activity != activity {
                session.activity = activity;
                dirty = true;
            }
        }

        let Some(focus) = focus else {
            if dirty {
                self.force_full_redraw = true;
                self.render(buffer)?;
            }
            return Ok(());
        };
        let Some(file) = focus.file.clone() else {
            if dirty {
                self.force_full_redraw = true;
                self.render(buffer)?;
            }
            return Ok(());
        };
        if !file.is_file() {
            if dirty {
                self.force_full_redraw = true;
                self.render(buffer)?;
            }
            return Ok(());
        }

        let showing = self
            .current_buffer()
            .file
            .as_deref()
            .is_some_and(|open| same_file_path(Path::new(open), &file));
        let allow_switch = following && self.agent_watch_jump_allowed(&focus.key, &file);
        if !showing && !allow_switch {
            if dirty {
                self.force_full_redraw = true;
                self.render(buffer)?;
            }
            return Ok(());
        }
        if self.is_command() || self.is_search() || self.current_dialog.is_some() {
            if dirty {
                self.force_full_redraw = true;
                self.render(buffer)?;
            }
            return Ok(());
        }

        let path = file.to_string_lossy().into_owned();
        let (index, _, _) = match self.load_or_reuse_path_buffer(&path).await {
            Ok(opened) => opened,
            Err(error) => {
                self.set_legacy_message(Some(error.to_string()));
                return Ok(());
            }
        };
        if !self.buffer_manager[index].is_dirty() {
            if let Ok((_, disk)) = self.buffer_manager[index].read_backing_file() {
                if disk != self.buffer_manager[index].contents() {
                    self.reload_changed_file(index, runtime).await?;
                    dirty = true;
                }
            }
        }
        if index != self.buffer_manager.active_index() {
            if !allow_switch {
                if dirty {
                    self.force_full_redraw = true;
                    self.render(buffer)?;
                }
                return Ok(());
            }
            self.set_current_buffer(buffer, index).await?;
            dirty = true;
        }

        let resolved = find_needle(self.current_buffer(), &focus.needle).unwrap_or_else(|| {
            focus
                .line
                .unwrap_or(0)
                .min(self.current_buffer().len().saturating_sub(1))
        });
        let end = resolved
            .saturating_add(focus.span.saturating_sub(1))
            .min(self.current_buffer().len().saturating_sub(1))
            .max(resolved);
        let highlight = focus.touch.map(|kind| WatchHighlight {
            path: file.clone(),
            start_line: resolved,
            end_line: end,
            kind,
        });
        if self.agent_watch.as_ref().map(|session| &session.highlight) != Some(&highlight) {
            if let Some(session) = self.agent_watch.as_mut() {
                session.highlight = highlight;
            }
            dirty = true;
        }

        if following && self.buffer_line() != resolved {
            self.go_to_line(
                resolved + 1,
                buffer,
                runtime,
                super::GoToLinePosition::Center,
            )
            .await?;
            self.execute(&Action::SetCursor(0, resolved), buffer, runtime)
                .await?;
            dirty = true;
        }
        if let Some(session) = self.agent_watch.as_mut() {
            let switched = session
                .last_jump_file
                .as_ref()
                .is_none_or(|previous| !same_file_path(previous, &file));
            if switched {
                session.last_jump_file = Some(file);
                session.last_jump_at = Some(Instant::now());
            }
        }
        if dirty {
            self.force_full_redraw = true;
            self.render(buffer)?;
        }
        Ok(())
    }

    fn agent_watch_jump_allowed(&self, _agent: &str, file: &Path) -> bool {
        let Some(session) = &self.agent_watch else {
            return false;
        };
        !matches!(
            (&session.last_jump_file, session.last_jump_at),
            (Some(previous), Some(at))
                if !same_file_path(previous, file) && at.elapsed() < JUMP_GAP
        )
    }

    pub(super) fn agent_watch_captures(&mut self, event: &Event) -> bool {
        if self.agent_watch.is_none()
            || self.is_command()
            || self.is_search()
            || self.current_dialog.is_some()
        {
            return false;
        }
        match event {
            Event::Key(_) | Event::Paste(_) => {
                if !matches!(self.mode, Mode::Normal) {
                    self.mode = Mode::Normal;
                }
                true
            }
            Event::Mouse(mouse)
                if matches!(
                    mouse.kind,
                    MouseEventKind::Down(_)
                        | MouseEventKind::Drag(_)
                        | MouseEventKind::ScrollUp
                        | MouseEventKind::ScrollDown
                ) =>
            {
                self.set_agent_watch_following(false);
                false
            }
            _ => false,
        }
    }

    pub(super) fn handle_agent_watch_input(&mut self, event: &Event) -> Option<KeyAction> {
        let Event::Key(key) = event else {
            self.set_legacy_message(Some(HELP.to_string()));
            return Some(KeyAction::Single(Action::Refresh));
        };
        if matches!(key.kind, KeyEventKind::Release) {
            return Some(KeyAction::None);
        }

        if key.modifiers.intersects(
            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::META,
        ) {
            if let Some(action) = self.lookup_normal_key(event) {
                if contains_quit(&action) {
                    self.clear_agent_watch_prefix();
                    return Some(action);
                }
            }
        }

        if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
            if let Some(action) = self.agent_watch_mode_key(key.code) {
                return Some(action);
            }
        }

        if self
            .agent_watch
            .as_ref()
            .is_some_and(|session| session.pending.is_some())
        {
            return self.resolve_agent_watch_prefix(event);
        }

        let Some(action) = self.lookup_normal_key(event) else {
            self.set_legacy_message(Some(HELP.to_string()));
            return Some(KeyAction::Single(Action::Refresh));
        };
        self.dispatch_agent_watch_action(action)
    }

    fn agent_watch_mode_key(&mut self, code: KeyCode) -> Option<KeyAction> {
        match code {
            KeyCode::Char('q') => {
                self.clear_agent_watch_prefix();
                self.stop_agent_watch();
                Some(KeyAction::Single(Action::Refresh))
            }
            KeyCode::Char('f') => {
                self.clear_agent_watch_prefix();
                let following = self
                    .agent_watch
                    .as_ref()
                    .is_some_and(|session| !session.following);
                self.set_agent_watch_following(following);
                self.set_legacy_message(Some(if following {
                    "following the agent again".to_string()
                } else {
                    "free scroll — esc follows the agent again".to_string()
                }));
                Some(KeyAction::Single(Action::Refresh))
            }
            KeyCode::Esc => {
                let pending = self
                    .agent_watch
                    .as_ref()
                    .is_some_and(|session| session.pending.is_some());
                if pending {
                    self.clear_agent_watch_prefix();
                    return Some(KeyAction::None);
                }
                let following = self
                    .agent_watch
                    .as_ref()
                    .is_some_and(|session| session.following);
                if following {
                    self.stop_agent_watch();
                } else {
                    self.set_agent_watch_following(true);
                    self.set_legacy_message(Some("following the agent again".to_string()));
                }
                Some(KeyAction::Single(Action::Refresh))
            }
            KeyCode::Char('?') => {
                self.clear_agent_watch_prefix();
                self.set_legacy_message(Some(HELP.to_string()));
                Some(KeyAction::Single(Action::Refresh))
            }
            KeyCode::Char('a') => {
                self.clear_agent_watch_prefix();
                if let Some(session) = self.agent_watch.as_mut() {
                    session.pinned = None;
                }
                self.set_agent_watch_following(true);
                self.set_legacy_message(Some("following whichever agent is working".to_string()));
                Some(KeyAction::Single(Action::Refresh))
            }
            KeyCode::Char(digit) if digit.is_ascii_digit() && digit != '0' => {
                self.clear_agent_watch_prefix();
                let number = digit.to_digit(10).unwrap_or(0) as usize;
                let key = self
                    .agent_watch
                    .as_ref()
                    .and_then(|session| session.roster.get(number - 1).cloned());
                match key {
                    Some(key) => {
                        if let Some(session) = self.agent_watch.as_mut() {
                            session.pinned = Some(key);
                        }
                        self.set_agent_watch_following(true);
                        self.set_legacy_message(Some(format!("pinned agent {number}")));
                    }
                    None => {
                        self.set_legacy_message(Some(format!("no agent {number}")));
                    }
                }
                Some(KeyAction::Single(Action::Refresh))
            }
            _ => None,
        }
    }

    fn resolve_agent_watch_prefix(&mut self, event: &Event) -> Option<KeyAction> {
        let key = Self::key_string_for_event(event);
        let pending = self
            .agent_watch
            .as_mut()
            .and_then(|session| session.pending.take());
        let Some(map) = pending else {
            return Some(KeyAction::None);
        };
        let Some(key) = key else {
            self.set_legacy_message(Some(HELP.to_string()));
            return Some(KeyAction::Single(Action::Refresh));
        };
        let Some(action) = map.get(&key).cloned() else {
            self.set_legacy_message(Some(HELP.to_string()));
            return Some(KeyAction::Single(Action::Refresh));
        };
        self.dispatch_agent_watch_action(action)
    }

    fn dispatch_agent_watch_action(&mut self, action: KeyAction) -> Option<KeyAction> {
        if let KeyAction::Nested(map) = &action {
            if let Some(session) = self.agent_watch.as_mut() {
                session.pending = Some(map.clone());
            }
            return Some(KeyAction::None);
        }
        match key_effect(&action) {
            KeyEffect::Block => {
                self.set_legacy_message(Some(HELP.to_string()));
                Some(KeyAction::Single(Action::Refresh))
            }
            KeyEffect::Run { pause } => {
                if pause {
                    self.set_agent_watch_following(false);
                }
                Some(action)
            }
        }
    }

    fn lookup_normal_key(&self, event: &Event) -> Option<KeyAction> {
        let key = Self::key_string_for_event(event)?;
        self.config.keys.normal.get(&key).cloned()
    }

    fn clear_agent_watch_prefix(&mut self) {
        if let Some(session) = self.agent_watch.as_mut() {
            session.pending = None;
        }
    }

    fn set_agent_watch_following(&mut self, following: bool) {
        if let Some(session) = self.agent_watch.as_mut() {
            session.following = following;
            session.mode_label = if following {
                "FOLLOW".to_string()
            } else {
                "WATCH".to_string()
            };
        }
    }
}

fn resolve_watch_root(path: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let start = match path {
        Some(path) => {
            let text = path.to_string_lossy();
            if text.starts_with('~') {
                crate::utils::expand_user_path(&text)?
            } else if path.is_absolute() {
                path
            } else {
                env::current_dir()?.join(path)
            }
        }
        None => env::current_dir()?,
    };
    if !start.exists() {
        anyhow::bail!("agent watch directory does not exist: {}", start.display());
    }
    let start = if start.is_file() {
        start.parent().map(Path::to_path_buf).unwrap_or(start)
    } else {
        start
    };
    Ok(crate::agent_watch::project_root(&start))
}

fn choose_agent<'a>(agents: &'a [AgentView], pinned: Option<&str>) -> Option<&'a AgentView> {
    if let Some(key) = pinned {
        return agents.iter().find(|agent| agent.key == key);
    }
    let mut busy = None;
    let mut any = None;
    for agent in agents {
        if agent.file.is_none() {
            continue;
        }
        if newer(agent, any) {
            any = Some(agent);
        }
        if agent.busy && newer(agent, busy) {
            busy = Some(agent);
        }
    }
    busy.or(any).or_else(|| {
        agents
            .iter()
            .max_by(|left, right| left.touched.cmp(&right.touched))
    })
}

fn newer(agent: &AgentView, current: Option<&AgentView>) -> bool {
    current
        .map(|current| agent.touched >= current.touched)
        .unwrap_or(true)
}

fn status_text(view: &WatchView, focus: Option<&AgentView>) -> String {
    let Some(agent) = focus else {
        return view.notes.first().cloned().unwrap_or_else(|| {
            "waiting for a Claude Code, Codex, or Gemini CLI session".to_string()
        });
    };
    let mut text = format!("{} · {} · {}", agent.label, agent.activity, agent.detail);
    if !agent.task.is_empty() && agent.task != "no prompt yet" && !text.contains(&agent.task) {
        text.push_str(" · ");
        text.push_str(&agent.task);
    }
    if view.agents.len() > 1 {
        text.push_str(&format!(" · {} agents", view.agents.len()));
    }
    text
}

fn find_needle(buffer: &crate::buffer::Buffer, needle: &str) -> Option<usize> {
    let needle = needle.lines().next()?.trim();
    if needle.is_empty() {
        return None;
    }
    (0..buffer.len()).find(|line| buffer.get(*line).is_some_and(|text| text.contains(needle)))
}

enum KeyEffect {
    Block,
    Run { pause: bool },
}

fn key_effect(action: &KeyAction) -> KeyEffect {
    match action {
        KeyAction::None => KeyEffect::Run { pause: false },
        KeyAction::Nested(_) => KeyEffect::Block,
        KeyAction::Single(action) => action_effect(action),
        KeyAction::Repeating(_, action) => key_effect(action),
        KeyAction::Multiple(actions) => {
            let mut pause = false;
            for action in actions {
                match action_effect(action) {
                    KeyEffect::Block => return KeyEffect::Block,
                    KeyEffect::Run { pause: this } => pause |= this,
                }
            }
            KeyEffect::Run { pause }
        }
    }
}

fn action_effect(action: &Action) -> KeyEffect {
    let pause = match action {
        Action::EnterMode(Mode::Command) | Action::Quit(_) | Action::Refresh => false,
        Action::MoveUp
        | Action::MoveDown
        | Action::MoveLeft
        | Action::MoveRight
        | Action::MoveToLineEnd
        | Action::MoveToLineStart
        | Action::MoveToFirstLineChar
        | Action::MoveToLastLineChar
        | Action::MoveToViewportTop(_)
        | Action::MoveToViewportMiddle
        | Action::MoveToViewportBottom(_)
        | Action::MoveScreenLineUp
        | Action::MoveScreenLineDown
        | Action::MoveToScreenLineEnd
        | Action::MoveToScreenLineStart
        | Action::MoveToScreenLineFirstNonBlank
        | Action::MoveLineToViewportCenter
        | Action::MoveLineToViewportBottom
        | Action::MoveToBottom
        | Action::MoveToTop
        | Action::MoveTo(_, _)
        | Action::MoveToFilePos(_, _, _)
        | Action::MoveToNextWord
        | Action::MoveToPreviousWord
        | Action::MoveToNextBigWord
        | Action::MoveToNextWordEnd
        | Action::MoveToPreviousWordEnd
        | Action::MoveToNextBigWordEnd
        | Action::MoveToPreviousBigWord
        | Action::MoveToPreviousBigWordEnd
        | Action::MoveToNextParagraph
        | Action::MoveToPreviousParagraph
        | Action::MoveToNextSentence
        | Action::MoveToPreviousSentence
        | Action::MoveToFilePercent(_)
        | Action::MatchitForward
        | Action::MatchitBackward
        | Action::MatchitPreviousUnmatched
        | Action::MatchitNextUnmatched
        | Action::PageDown
        | Action::PageUp
        | Action::HalfPageDown(_)
        | Action::HalfPageUp(_)
        | Action::ScrollUp
        | Action::ScrollDown
        | Action::ScrollViewLeft
        | Action::ScrollViewRight
        | Action::ScrollViewHalfPageLeft
        | Action::ScrollViewHalfPageRight
        | Action::ScrollCursorToViewStart
        | Action::ScrollCursorToViewEnd
        | Action::GoToLine(_)
        | Action::GoToDefinition
        | Action::JumpBack
        | Action::JumpForward
        | Action::EnterSearch(_)
        | Action::RepeatSearch
        | Action::RepeatSearchOpposite
        | Action::SearchWordUnderCursor
        | Action::ClearSearchHighlight
        | Action::ToggleWrap
        | Action::SetWrap(_)
        | Action::SetRelativeLineNumbers(_)
        | Action::NextBuffer
        | Action::PreviousBuffer
        | Action::AlternateBuffer
        | Action::Yank
        | Action::YankCurrentLine
        | Action::YankCurrentLines(_)
        | Action::YankToLineEnd(_)
        | Action::CommandPalette
        | Action::KeyboardShortcuts => true,
        _ => return KeyEffect::Block,
    };
    KeyEffect::Run { pause }
}

fn contains_quit(action: &KeyAction) -> bool {
    match action {
        KeyAction::Single(Action::Quit(_)) => true,
        KeyAction::Multiple(actions) => actions
            .iter()
            .any(|action| matches!(action, Action::Quit(_))),
        KeyAction::Repeating(_, action) => contains_quit(action),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use serde_json::json;

    use crate::{
        agent_watch::WatchOptions, buffer::Buffer, config::Config, editor::Editor, lsp::LspManager,
        plugin::Runtime, theme::Theme,
    };

    use super::*;

    fn editor() -> Editor {
        let config: Config = toml::from_str(include_str!("../../default_config.toml")).unwrap();
        let mut editor = Editor::test_with_size(
            Box::new(LspManager::new(config.lsp.clone())),
            80,
            24,
            config,
            Theme::default(),
            vec![Buffer::new(None, String::new())],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        editor
    }

    fn claude_fixture(root: &Path) -> WatchOptions {
        let options = WatchOptions {
            root: root.to_path_buf(),
            since: Duration::from_secs(60 * 60),
            claude_projects: root.join("logs/claude"),
            codex_sessions: root.join("logs/codex"),
            gemini_tmp: root.join("logs/gemini"),
        };
        let canonical = root.canonicalize().unwrap();
        let encoded: String = canonical
            .to_string_lossy()
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
            .collect();
        let file = root.join("snake.js");
        fs::write(&file, "alpha\nfn brand_new_line() {}\nomega\n").unwrap();
        let transcript = options.claude_projects.join(encoded).join("session.jsonl");
        fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        let stamp = chrono::Utc::now().to_rfc3339();
        let lines = format!(
            "{}\n{}\n",
            json!({
                "type": "user",
                "timestamp": stamp,
                "cwd": canonical,
                "message": {"content": "Simplify snake.js"}
            }),
            json!({
                "type": "assistant",
                "timestamp": stamp,
                "cwd": canonical,
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
        );
        fs::write(transcript, lines).unwrap();
        options
    }

    #[tokio::test]
    async fn follow_opens_the_edited_file_and_keys_stay_read_only() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("proj");
        fs::create_dir_all(&root).unwrap();
        let mut editor = editor();
        editor.begin_agent_watch_with(claude_fixture(&root));
        let mut buffer = RenderBuffer::new(80, 24, &crate::theme::Style::default());
        let mut runtime = Runtime::new();
        editor
            .poll_agent_watch(&mut buffer, &mut runtime)
            .await
            .unwrap();

        assert!(editor
            .current_buffer()
            .file
            .as_deref()
            .is_some_and(|file| file.ends_with("snake.js")));
        assert_eq!(editor.buffer_line(), 1);
        assert_eq!(editor.agent_watch_mode_name().as_deref(), Some("FOLLOW"));
        let highlight = editor.agent_watch_highlight().unwrap();
        assert_eq!(highlight.start_line, 1);
        assert_eq!(highlight.kind, TouchKind::Edit);
        assert!(editor
            .agent_watch_status_line()
            .unwrap()
            .contains("editing"));

        editor
            .test_execute_event(Event::Key(KeyEvent::new(
                KeyCode::Char('i'),
                KeyModifiers::NONE,
            )))
            .await
            .unwrap();
        assert_eq!(editor.mode, Mode::Normal);
        assert!(editor.agent_watch.is_some());

        editor
            .test_execute_event(Event::Key(KeyEvent::new(
                KeyCode::Char('j'),
                KeyModifiers::NONE,
            )))
            .await
            .unwrap();
        assert_eq!(editor.buffer_line(), 2);
        assert_eq!(editor.agent_watch_mode_name().as_deref(), Some("WATCH"));

        editor
            .test_execute_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
            .await
            .unwrap();
        assert_eq!(editor.agent_watch_mode_name().as_deref(), Some("FOLLOW"));
        editor.expire_agent_watch_poll();
        editor
            .poll_agent_watch(&mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(editor.buffer_line(), 1);

        editor
            .test_execute_event(Event::Key(KeyEvent::new(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
            )))
            .await
            .unwrap();
        assert!(editor.agent_watch.is_none());
        assert_eq!(editor.mode, Mode::Normal);
    }

    #[test]
    fn command_reports_a_missing_directory_and_stop_leaves_the_mode() {
        let mut editor = editor();
        let runtime = Runtime::new();
        let actions = editor.handle_command("agent-watch /no/such/red-agent-watch", &runtime);
        assert_eq!(actions, vec![Action::EnterMode(Mode::Normal)]);
        assert!(editor.agent_watch.is_none());
        assert!(editor
            .transient_message
            .as_ref()
            .unwrap()
            .text
            .contains("does not exist"));

        let temp = tempfile::tempdir().unwrap();
        editor
            .begin_agent_watch(Some(temp.path().to_path_buf()))
            .unwrap();
        assert!(editor.agent_watch.is_some());
        editor.handle_command("agent-watch stop", &runtime);
        assert!(editor.agent_watch.is_none());
    }
}
