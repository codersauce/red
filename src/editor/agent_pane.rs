//! Lifecycle of the Agent pane's conversation, owned by the editor core.
//!
//! The bundled Agent pane used to infer "what is the agent doing" from a dozen
//! plugin flags that every event handler patched by hand. This module is the
//! single source of truth instead: the pane sends intents (submit, stop, resume
//! the queue, reset) and renders the [`AgentPaneSnapshot`] the editor publishes
//! after every change. Methods only update state and return the effects the
//! editor must run, so every transition is unit-testable without Codex.

use std::collections::{HashMap, VecDeque};

use serde::Serialize;

/// Longest transcript tail replayed when a conversation continues in a new thread.
pub const MAX_RECAP_CHARS: usize = 6000;
/// Most follow-ups the pane may hold while a turn runs or the queue is paused.
pub const MAX_QUEUED_PROMPTS: usize = 16;
/// What the pane shows when the user asks Codex to pick up an interrupted reply.
pub const CONTINUE_PROMPT: &str = "Continue where you stopped.";

/// What the pane's conversation is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPanePhase {
    /// No turn is running; a session may or may not exist.
    Idle,
    /// A new Codex thread was requested and has not been created yet.
    Starting,
    /// A persisted thread is being resumed.
    Restoring,
    /// A turn is running.
    Working,
    /// A cancel was sent and Codex has not confirmed that the turn ended.
    Stopping,
}

/// A user message the pane shows before, or while, it is sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentPanePrompt {
    /// Stable block id the pane uses for the optimistic and committed message.
    pub id: String,
    pub text: String,
}

/// Everything the pane needs to render lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentPaneSnapshot {
    pub phase: AgentPanePhase,
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    /// The message of the running turn, so the pane commits it in the same
    /// update that removes it from the queue.
    pub in_flight: Option<AgentPanePrompt>,
    /// Prompts not yet sent, oldest first.
    pub queued: Vec<AgentPanePrompt>,
    /// Queued prompts wait for an explicit resume after a stop, failure, or loss.
    pub paused: bool,
    /// The last cancel was rejected, so the turn is still running.
    pub stop_failed: bool,
    /// The next prompt starts a new thread that is seeded with the transcript.
    pub recap_pending: bool,
    /// The message whose turn was cut short because Codex stopped. Nothing is
    /// sent until the user chooses what to do with it.
    pub interrupted: Option<AgentPanePrompt>,
    /// A saved thread is known but not resumed yet; it resumes when the pane
    /// opens or the next message is sent.
    pub resume_pending: bool,
}

/// What the user chose to do about a turn that Codex stopped in the middle of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentInterruptionChoice {
    /// Send the interrupted message again.
    Retry,
    /// Ask Codex to pick up where the reply stopped.
    Continue,
    /// Leave the reply as it is; queued follow-ups stay paused.
    Dismiss,
}

impl AgentInterruptionChoice {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "retry" => Some(Self::Retry),
            "continue" => Some(Self::Continue),
            "dismiss" => Some(Self::Dismiss),
            _ => None,
        }
    }
}

/// Work the editor performs for a lifecycle transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentPaneEffect {
    /// Start a new Codex thread in the workspace.
    StartSession,
    /// Resume a persisted Codex thread.
    ResumeSession { session_id: String, cwd: String },
    /// Send a prompt as a new turn. `wire_text` includes any transcript recap.
    Dispatch {
        session_id: String,
        prompt: AgentPanePrompt,
        wire_text: String,
    },
    /// Interrupt the running turn.
    Interrupt { session_id: String },
    /// Close a thread that the pane no longer uses.
    CloseSession { session_id: String },
    /// Close a thread and drop its persisted conversation.
    ForgetSession { session_id: String },
}

/// How a turn ended, from the editor's terminal Codex event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTurnOutcome {
    /// Codex finished the turn normally.
    Completed,
    /// The turn stopped early: interrupted, failed, or cut short.
    Stopped,
}

/// Result of submitting a prompt, for the pane's immediate feedback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSubmitResult {
    /// The prompt was sent or will be sent as soon as a session is ready.
    Accepted,
    /// The prompt waits behind the running turn or a paused queue.
    Queued,
    /// The queue is full and the prompt was not kept.
    QueueFull,
}

#[derive(Debug, Default)]
pub struct AgentPaneLifecycle {
    session_id: Option<String>,
    phase: Option<AgentPanePhase>,
    turn_id: Option<String>,
    in_flight: Option<AgentPanePrompt>,
    queue: VecDeque<AgentPanePrompt>,
    paused: bool,
    stop_requested: bool,
    stop_failed: bool,
    recap_pending: bool,
    /// Tail of the pane's visible transcript, replayed when a thread is replaced.
    transcript_tail: String,
    /// Transcript tail when the in-flight prompt was sent, before the pane
    /// committed it, so a retry does not replay the prompt or later errors.
    in_flight_tail: String,
    /// Recap for a retried prompt, taken from `in_flight_tail`.
    retry_tail: Option<String>,
    /// Whether `transcript_tail` already holds the running turn's reply header.
    reply_open: bool,
    /// Thread forgotten by a reset while it was still being resumed; a late
    /// restore of it must not be adopted.
    forgotten_session: Option<String>,
    /// Turn cut short when Codex stopped; see [`AgentPaneSnapshot::interrupted`].
    interrupted: Option<AgentPanePrompt>,
    /// Workspace of a saved thread that is resumed on first use.
    resume_cwd: Option<String>,
    /// Persisted thread selected in the session browser while its replacement loads.
    switch_target: Option<String>,
    /// Live thread retained until the selected replacement restores successfully.
    switch_previous_session: Option<String>,
    /// Wire text for queued prompts whose shown text is shorter than what Codex
    /// receives, keyed by prompt id.
    wire_overrides: HashMap<String, String>,
    next_prompt: u64,
}

impl AgentPaneLifecycle {
    pub fn snapshot(&self) -> AgentPaneSnapshot {
        AgentPaneSnapshot {
            phase: self.phase(),
            session_id: self.session_id.clone(),
            turn_id: self.turn_id.clone(),
            in_flight: self.in_flight.clone(),
            queued: self.queue.iter().cloned().collect(),
            paused: self.paused,
            stop_failed: self.stop_failed,
            recap_pending: self.recap_pending,
            interrupted: self.interrupted.clone(),
            resume_pending: self.resume_cwd.is_some(),
        }
    }

    pub fn phase(&self) -> AgentPanePhase {
        self.phase.unwrap_or(AgentPanePhase::Idle)
    }

    /// Accepts a user message. `transcript` is the pane's visible conversation,
    /// replayed only when the message must continue in a new thread.
    pub fn submit(
        &mut self,
        text: String,
        transcript: &str,
    ) -> (AgentSubmitResult, Vec<AgentPaneEffect>) {
        self.set_transcript(transcript);
        if self.queue.len() >= MAX_QUEUED_PROMPTS {
            return (AgentSubmitResult::QueueFull, Vec::new());
        }
        // Writing a new message is an answer too: the user moved on.
        self.interrupted = None;
        let prompt = self.new_prompt(text);
        let busy = matches!(
            self.phase(),
            AgentPanePhase::Working | AgentPanePhase::Stopping
        );
        if busy || (self.paused && !self.queue.is_empty()) {
            self.queue.push_back(prompt);
            return (AgentSubmitResult::Queued, Vec::new());
        }
        // A fresh submission is an explicit request to continue.
        self.paused = false;
        self.queue.push_back(prompt);
        (AgentSubmitResult::Accepted, self.advance())
    }

    /// Stops the running turn. Before a session exists it pauses the start.
    pub fn stop(&mut self) -> Vec<AgentPaneEffect> {
        match (self.phase(), self.session_id.clone()) {
            (AgentPanePhase::Working | AgentPanePhase::Stopping, Some(session_id)) => {
                self.phase = Some(AgentPanePhase::Stopping);
                self.stop_requested = true;
                self.stop_failed = false;
                vec![AgentPaneEffect::Interrupt { session_id }]
            }
            (AgentPanePhase::Starting, _) => {
                // The thread still starts, but nothing is sent to it unattended.
                self.paused = !self.queue.is_empty();
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Resumes paused follow-ups.
    pub fn send_queued(&mut self, transcript: &str) -> Vec<AgentPaneEffect> {
        self.set_transcript(transcript);
        // Sending the queue is an answer to an interrupted turn: move on.
        self.interrupted = None;
        self.paused = false;
        self.advance()
    }

    /// Drops every prompt that has not been sent.
    pub fn discard_queued(&mut self) -> usize {
        let dropped = self.queue.len();
        self.queue.clear();
        self.paused = false;
        dropped
    }

    /// Starts over: the current thread is forgotten and nothing stays queued.
    /// A thread that is still being created becomes the fresh conversation's
    /// thread, so a reset never starts a second one alongside it.
    pub fn reset(&mut self) -> Vec<AgentPaneEffect> {
        let starting = self.phase() == AgentPanePhase::Starting;
        let restoring = self.phase() == AgentPanePhase::Restoring;
        let forgotten = self.session_id.take();
        let effects = forgotten
            .clone()
            .map(|session_id| AgentPaneEffect::ForgetSession { session_id })
            .into_iter()
            .collect();
        *self = Self {
            phase: starting.then_some(AgentPanePhase::Starting),
            forgotten_session: if restoring {
                forgotten
            } else {
                self.forgotten_session.take()
            },
            next_prompt: self.next_prompt,
            ..Self::default()
        };
        effects
    }

    /// Starts a thread ahead of the first prompt so the reply arrives sooner.
    pub fn start(&mut self) -> Vec<AgentPaneEffect> {
        if self.session_id.is_some() || self.phase() != AgentPanePhase::Idle {
            return Vec::new();
        }
        self.phase = Some(AgentPanePhase::Starting);
        vec![AgentPaneEffect::StartSession]
    }

    /// A Codex thread exists. A different previous thread is closed.
    pub fn session_created(&mut self, session_id: &str) -> Vec<AgentPaneEffect> {
        self.forgotten_session = None;
        let mut effects = Vec::new();
        if let Some(previous) = self.session_id.replace(session_id.to_string()) {
            if previous != session_id {
                effects.push(AgentPaneEffect::CloseSession {
                    session_id: previous,
                });
            }
        }
        if self.phase() == AgentPanePhase::Starting {
            self.phase = Some(AgentPanePhase::Idle);
        }
        effects.extend(self.advance());
        effects
    }

    /// A new thread could not be started. Unsent prompts wait for a retry.
    pub fn start_failed(&mut self) {
        if self.phase() != AgentPanePhase::Starting {
            return;
        }
        self.phase = Some(AgentPanePhase::Idle);
        self.paused = !self.queue.is_empty();
    }

    /// Streamed reply text for the running turn. The tail mirrors the pane's
    /// transcript, so a follow-up sent automatically after this turn recaps the
    /// finished answer instead of the transcript from when it was queued.
    pub fn reply_delta(&mut self, session_id: &str, text: &str) {
        let running = matches!(
            self.phase(),
            AgentPanePhase::Working | AgentPanePhase::Stopping
        );
        if !running || text.is_empty() || self.session_id.as_deref() != Some(session_id) {
            return;
        }
        if !self.reply_open {
            self.reply_open = true;
            self.end_transcript_line();
            self.transcript_tail.push_str("Agent: ");
        }
        self.push_transcript(text);
    }

    /// The editor confirmed the turn the pane just dispatched.
    pub fn turn_started(&mut self, session_id: &str, turn_id: &str) {
        if self.session_id.as_deref() == Some(session_id) {
            self.turn_id = Some(turn_id.to_string());
        }
    }

    /// A turn ended. Returns `None` when the event belongs to another session or
    /// an earlier turn, which the pane must ignore.
    pub fn turn_ended(
        &mut self,
        session_id: &str,
        turn_id: Option<&str>,
        outcome: AgentTurnOutcome,
    ) -> Option<Vec<AgentPaneEffect>> {
        if self.session_id.as_deref() != Some(session_id) {
            return None;
        }
        if let (Some(ended), Some(current)) = (turn_id, self.turn_id.as_deref()) {
            if ended != current {
                return None;
            }
        }
        if !matches!(
            self.phase(),
            AgentPanePhase::Working | AgentPanePhase::Stopping
        ) {
            return None;
        }
        // A stop that lost the race to completion still means "stop here".
        let hold = self.stop_requested || outcome == AgentTurnOutcome::Stopped;
        self.close_reply();
        let in_flight = self.in_flight.take();
        if outcome == AgentTurnOutcome::Stopped && !self.stop_requested {
            // Cut short without the user asking: offer Retry and Continue.
            self.interrupted = in_flight;
        }
        self.phase = Some(AgentPanePhase::Idle);
        self.turn_id = None;
        self.stop_requested = false;
        self.stop_failed = false;
        if hold {
            self.paused = !self.queue.is_empty();
            return Some(Vec::new());
        }
        Some(self.advance())
    }

    /// A cancel for this session arrived from outside the pane, such as the
    /// per-session `AgentCancel` host call; record it without a second interrupt.
    pub fn stopping(&mut self, session_id: &str) {
        if self.session_id.as_deref() == Some(session_id) && self.phase() == AgentPanePhase::Working
        {
            self.phase = Some(AgentPanePhase::Stopping);
            self.stop_requested = true;
            self.stop_failed = false;
        }
    }

    /// Codex refused the interrupt; the turn keeps running.
    pub fn stop_failed(&mut self, session_id: &str) {
        if self.session_id.as_deref() == Some(session_id)
            && self.phase() == AgentPanePhase::Stopping
        {
            self.phase = Some(AgentPanePhase::Working);
            self.stop_failed = true;
        }
    }

    /// The app-server went away. When the in-flight prompt never reached Codex it
    /// is retried in a new thread; a prompt that already ran is not replayed,
    /// because its edits may have applied.
    pub fn session_lost(&mut self, unsent: bool) -> Vec<AgentPaneEffect> {
        let running = self.is_running();
        let in_flight = self.in_flight.take();
        let (retry, ran) = if unsent {
            (in_flight, None)
        } else {
            (None, in_flight.filter(|_| running))
        };
        if ran.is_some() {
            // The reply was cut short; ask instead of replaying or dropping it.
            self.interrupted = ran;
        }
        self.resume_cwd = None;
        let had_session = self.session_id.take().is_some();
        self.close_reply();
        self.recap_pending |= had_session || retry.is_some();
        self.phase = Some(AgentPanePhase::Idle);
        self.turn_id = None;
        self.stop_requested = false;
        self.stop_failed = false;
        if let Some(prompt) = retry {
            self.retry_tail = Some(std::mem::take(&mut self.in_flight_tail));
            self.queue.push_front(prompt);
            self.paused = false;
            return self.advance();
        }
        self.paused = !self.queue.is_empty();
        Vec::new()
    }

    /// The editor found a persisted thread to resume.
    /// A turn that was running when Codex stopped is held as
    /// [`AgentPaneSnapshot::interrupted`] and nothing queued is sent until the
    /// user decides; the thread itself resumes in the background.
    pub fn restore_pending(&mut self, session_id: &str, cwd: &str) -> Vec<AgentPaneEffect> {
        // A bridge restart resumes the persisted pane conversation. Any
        // user-requested switch that was still in flight is abandoned.
        self.switch_target = None;
        self.switch_previous_session = None;
        if self.is_running() {
            if let Some(prompt) = self.in_flight.take() {
                self.interrupted = Some(prompt);
            }
            self.paused = !self.queue.is_empty();
        }
        self.close_reply();
        self.session_id = Some(session_id.to_string());
        self.phase = Some(AgentPanePhase::Restoring);
        self.turn_id = None;
        self.in_flight = None;
        self.stop_requested = false;
        self.stop_failed = false;
        self.resume_cwd = None;
        vec![AgentPaneEffect::ResumeSession {
            session_id: session_id.to_string(),
            cwd: cwd.to_string(),
        }]
    }

    /// A thread saved by an earlier Red session in this workspace. It is resumed
    /// lazily, when the pane opens or the next message is sent, so a launch that
    /// never uses the agent does not start Codex.
    pub fn resumable(&mut self, session_id: &str, cwd: &str) {
        if self.session_id.is_some() || self.phase() != AgentPanePhase::Idle {
            return;
        }
        self.session_id = Some(session_id.to_string());
        self.resume_cwd = Some(cwd.to_string());
        self.recap_pending = false;
    }

    /// The pane opened: resume a saved thread now so the first reply is quick.
    pub fn prepare(&mut self) -> Vec<AgentPaneEffect> {
        if self.phase() != AgentPanePhase::Idle {
            return Vec::new();
        }
        self.resume_now().into_iter().collect()
    }

    /// Begins a user-requested switch while retaining the current thread until
    /// the selected conversation has restored successfully.
    pub fn switch_pending(&mut self, session_id: &str) -> bool {
        if self.phase() != AgentPanePhase::Idle
            || session_id.is_empty()
            || self.session_id.as_deref() == Some(session_id)
            || self.in_flight.is_some()
            || !self.queue.is_empty()
            || self.interrupted.is_some()
        {
            return false;
        }
        self.switch_previous_session.clone_from(&self.session_id);
        self.switch_target = Some(session_id.to_string());
        self.phase = Some(AgentPanePhase::Restoring);
        self.turn_id = None;
        true
    }

    /// Applies the user's answer about an interrupted turn.
    pub fn resolve_interruption(
        &mut self,
        choice: AgentInterruptionChoice,
        transcript: &str,
    ) -> Vec<AgentPaneEffect> {
        let Some(interrupted) = self.interrupted.take() else {
            return Vec::new();
        };
        self.set_transcript(transcript);
        let (shown, wire) = match choice {
            AgentInterruptionChoice::Dismiss => {
                self.paused = !self.queue.is_empty();
                return Vec::new();
            }
            AgentInterruptionChoice::Retry => (
                interrupted.text.clone(),
                format!(
                    "Codex stopped unexpectedly while you were answering this message, so your reply was cut off. Edits you already made are still applied: check the current files before redoing any work. The message again:\n{}",
                    interrupted.text
                ),
            ),
            AgentInterruptionChoice::Continue => (
                CONTINUE_PROMPT.to_string(),
                "Codex stopped unexpectedly while you were answering my last message. Continue from where you stopped. Edits you already made are still applied: check the current files before redoing anything.".to_string(),
            ),
        };
        let prompt = self.new_prompt(shown);
        self.wire_overrides.insert(prompt.id.clone(), wire);
        self.queue.push_front(prompt);
        self.paused = false;
        self.advance()
    }

    /// The pane's thread was closed outside the lifecycle, for example when the
    /// plugin deactivates; the next prompt starts a new thread.
    pub fn session_closed(&mut self, session_id: &str) {
        if self.session_id.as_deref() != Some(session_id) {
            return;
        }
        self.session_id = None;
        self.resume_cwd = None;
        self.phase = Some(AgentPanePhase::Idle);
        self.turn_id = None;
        self.in_flight = None;
        self.stop_requested = false;
        self.stop_failed = false;
        self.paused = !self.queue.is_empty();
    }

    /// The persisted thread is live again.
    pub fn restored(&mut self, session_id: &str) -> Vec<AgentPaneEffect> {
        if self.switch_target.take().is_some() {
            let previous = self.switch_previous_session.take();
            self.forgotten_session = None;
            self.session_id = Some(session_id.to_string());
            self.phase = Some(AgentPanePhase::Idle);
            self.recap_pending = false;
            self.resume_cwd = None;
            let mut effects = previous
                .filter(|previous| previous != session_id)
                .map(|session_id| AgentPaneEffect::CloseSession { session_id })
                .into_iter()
                .collect::<Vec<_>>();
            effects.extend(self.advance());
            return effects;
        }
        if self.forgotten_session.as_deref() == Some(session_id) {
            return Vec::new();
        }
        // Adopt a live thread the editor restored without a pane request, such as
        // after the plugin reloads, but never replace a different thread.
        match self.session_id.as_deref() {
            Some(current) if current != session_id => return Vec::new(),
            Some(_) => {}
            None => self.session_id = Some(session_id.to_string()),
        }
        self.phase = Some(AgentPanePhase::Idle);
        self.recap_pending = false;
        self.resume_cwd = None;
        self.advance()
    }

    /// The persisted thread cannot be resumed; the transcript is replayed instead.
    pub fn restore_failed(&mut self, session_id: &str) -> Vec<AgentPaneEffect> {
        if self.switch_target.as_deref() == Some(session_id) {
            self.switch_target = None;
            self.session_id = self.switch_previous_session.take();
            self.phase = Some(AgentPanePhase::Idle);
            return Vec::new();
        }
        if self.session_id.as_deref() != Some(session_id) {
            return Vec::new();
        }
        self.session_id = None;
        self.resume_cwd = None;
        self.phase = Some(AgentPanePhase::Idle);
        self.recap_pending = true;
        let mut effects = vec![AgentPaneEffect::ForgetSession {
            session_id: session_id.to_string(),
        }];
        // Prompts that waited for the resume continue in a new thread.
        effects.extend(self.advance());
        effects
    }

    /// Only an archived transcript survived; the next prompt replays it.
    pub fn archived_transcript_restored(&mut self, has_transcript: bool) {
        if self.session_id.is_none() {
            self.recap_pending = has_transcript;
        }
    }

    /// Updates the lifecycle from an agent notification the editor sends plugins,
    /// so the pane follows exactly the events plugins observe.
    pub fn observe(&mut self, event: &str, payload: &serde_json::Value) -> Vec<AgentPaneEffect> {
        let text = |key: &str| {
            payload
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let session_id = text("session_id");
        let turn_id = payload
            .get("turn_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        match event {
            // Adopt the thread the pane asked for, or one started for an idle pane.
            "agent:session_created"
                if self.phase() == AgentPanePhase::Starting
                    || (self.phase() == AgentPanePhase::Idle && self.session_id.is_none()) =>
            {
                self.session_created(&session_id)
            }
            "agent:update" => {
                self.reply_delta(&session_id, &text("text"));
                Vec::new()
            }
            "agent:turn_started" => {
                if let Some(turn_id) = turn_id {
                    self.turn_started(&session_id, &turn_id);
                }
                Vec::new()
            }
            "agent:completed" => {
                let outcome = match text("stop_reason").as_str() {
                    "end_turn" | "completed" | "" => AgentTurnOutcome::Completed,
                    _ => AgentTurnOutcome::Stopped,
                };
                self.turn_ended(&session_id, turn_id.as_deref(), outcome)
                    .unwrap_or_default()
            }
            "agent:error" if session_id.is_empty() => {
                self.start_failed();
                Vec::new()
            }
            "agent:error" => self
                .turn_ended(&session_id, turn_id.as_deref(), AgentTurnOutcome::Stopped)
                .unwrap_or_default(),
            "agent:cancelled" => {
                self.stopping(&session_id);
                Vec::new()
            }
            "agent:cancel_failed" => {
                self.stop_failed(&session_id);
                Vec::new()
            }
            // A prompt in the payload never reached Codex and is safe to retry.
            "agent:session_lost" => self.session_lost(!text("prompt").is_empty()),
            "agent:conversation_restore_pending" => {
                self.restore_pending(&text("thread_id"), &text("cwd"))
            }
            "agent:session_restored" => self.restored(&text("thread_id")),
            "agent:conversation_resumable" => {
                self.resumable(&text("thread_id"), &text("cwd"));
                Vec::new()
            }
            "agent:session_restore_failed" => self.restore_failed(&session_id),
            "agent:transcript_restored" => {
                self.archived_transcript_restored(!text("transcript").trim().is_empty());
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Sends the next queued prompt when the conversation can take a turn.
    fn advance(&mut self) -> Vec<AgentPaneEffect> {
        if self.paused || self.queue.is_empty() || self.interrupted.is_some() {
            return Vec::new();
        }
        if let Some(effect) = self.resume_now() {
            return vec![effect];
        }
        match (self.phase(), self.session_id.clone()) {
            (AgentPanePhase::Idle, None) => {
                self.phase = Some(AgentPanePhase::Starting);
                vec![AgentPaneEffect::StartSession]
            }
            (AgentPanePhase::Idle, Some(session_id)) => {
                let Some(prompt) = self.queue.pop_front() else {
                    return Vec::new();
                };
                let text = self
                    .wire_overrides
                    .remove(&prompt.id)
                    .unwrap_or_else(|| prompt.text.clone());
                let wire_text = self.wire_text(&text);
                self.in_flight_tail = self.transcript_tail.clone();
                // Mirror the pane committing the message to its transcript.
                self.end_transcript_line();
                self.push_transcript(&format!("You: {}\n", prompt.text));
                self.phase = Some(AgentPanePhase::Working);
                self.turn_id = None;
                self.in_flight = Some(prompt.clone());
                vec![AgentPaneEffect::Dispatch {
                    session_id,
                    prompt,
                    wire_text,
                }]
            }
            _ => Vec::new(),
        }
    }

    fn is_running(&self) -> bool {
        matches!(
            self.phase(),
            AgentPanePhase::Working | AgentPanePhase::Stopping
        )
    }

    /// Starts resuming a saved thread that has not been resumed yet.
    fn resume_now(&mut self) -> Option<AgentPaneEffect> {
        let session_id = self.session_id.clone()?;
        if self.phase() != AgentPanePhase::Idle {
            return None;
        }
        let cwd = self.resume_cwd.take()?;
        self.phase = Some(AgentPanePhase::Restoring);
        Some(AgentPaneEffect::ResumeSession { session_id, cwd })
    }

    fn set_transcript(&mut self, transcript: &str) {
        // The pane's transcript is authoritative; streamed deltas extend it.
        self.transcript_tail = recap_tail(transcript);
    }

    fn push_transcript(&mut self, text: &str) {
        self.transcript_tail.push_str(text);
        // Trim in batches so streaming deltas do not recount the tail every time.
        if self.transcript_tail.len() > MAX_RECAP_CHARS * 8 {
            self.transcript_tail = recap_tail(&self.transcript_tail);
        }
    }

    fn end_transcript_line(&mut self) {
        if !self.transcript_tail.is_empty() && !self.transcript_tail.ends_with('\n') {
            self.transcript_tail.push('\n');
        }
    }

    fn close_reply(&mut self) {
        if std::mem::take(&mut self.reply_open) {
            self.end_transcript_line();
        }
    }

    fn wire_text(&mut self, text: &str) -> String {
        let retry_tail = self.retry_tail.take();
        let recap = recap_tail(retry_tail.as_ref().unwrap_or(&self.transcript_tail));
        if !std::mem::take(&mut self.recap_pending) || recap.trim().is_empty() {
            return text.to_string();
        }
        use crate::agent_conversation::{RECAP_FOLLOW_UP, RECAP_PREFIX};
        format!("{RECAP_PREFIX}\n{recap}{RECAP_FOLLOW_UP}{text}")
    }

    fn new_prompt(&mut self, text: String) -> AgentPanePrompt {
        self.next_prompt += 1;
        AgentPanePrompt {
            id: format!("prompt:{}", self.next_prompt),
            text,
        }
    }
}

/// The last [`MAX_RECAP_CHARS`] characters of a transcript.
fn recap_tail(transcript: &str) -> String {
    let skip = transcript.chars().count().saturating_sub(MAX_RECAP_CHARS);
    transcript.chars().skip(skip).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dispatched(effects: &[AgentPaneEffect]) -> Vec<String> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                AgentPaneEffect::Dispatch { wire_text, .. } => Some(wire_text.clone()),
                _ => None,
            })
            .collect()
    }

    fn live(session_id: &str) -> AgentPaneLifecycle {
        let mut pane = AgentPaneLifecycle::default();
        pane.session_created(session_id);
        pane
    }

    #[test]
    fn first_prompt_starts_a_session_and_is_sent_once_it_exists() {
        let mut pane = AgentPaneLifecycle::default();
        let (result, effects) = pane.submit("hello".into(), "");
        assert_eq!(result, AgentSubmitResult::Accepted);
        assert_eq!(effects, [AgentPaneEffect::StartSession]);
        assert_eq!(pane.phase(), AgentPanePhase::Starting);
        assert_eq!(pane.snapshot().queued.len(), 1, "shown while starting");

        let effects = pane.session_created("s1");
        assert_eq!(dispatched(&effects), ["hello"]);
        assert_eq!(pane.phase(), AgentPanePhase::Working);
        assert!(pane.snapshot().queued.is_empty());
    }

    #[test]
    fn follow_ups_queue_behind_a_turn_and_drain_in_order() {
        let mut pane = live("s1");
        pane.submit("one".into(), "");
        pane.turn_started("s1", "t1");
        assert_eq!(pane.submit("two".into(), "").0, AgentSubmitResult::Queued);
        assert_eq!(pane.submit("three".into(), "").0, AgentSubmitResult::Queued);

        let effects = pane
            .turn_ended("s1", Some("t1"), AgentTurnOutcome::Completed)
            .unwrap();
        assert_eq!(dispatched(&effects), ["two"]);
        pane.turn_started("s1", "t2");
        let effects = pane
            .turn_ended("s1", Some("t2"), AgentTurnOutcome::Completed)
            .unwrap();
        assert_eq!(dispatched(&effects), ["three"]);
    }

    #[test]
    fn stopping_keeps_the_thread_and_pauses_follow_ups() {
        let mut pane = live("s1");
        pane.submit("one".into(), "");
        pane.turn_started("s1", "t1");
        pane.submit("held".into(), "");
        assert_eq!(
            pane.stop(),
            [AgentPaneEffect::Interrupt {
                session_id: "s1".into()
            }]
        );
        assert_eq!(pane.phase(), AgentPanePhase::Stopping);
        assert_eq!(
            pane.submit("also held".into(), "").0,
            AgentSubmitResult::Queued
        );

        // A cancel that lost the race still holds the queue.
        let effects = pane
            .turn_ended("s1", Some("t1"), AgentTurnOutcome::Completed)
            .unwrap();
        assert!(effects.is_empty());
        let snapshot = pane.snapshot();
        assert_eq!(snapshot.phase, AgentPanePhase::Idle);
        assert_eq!(snapshot.session_id.as_deref(), Some("s1"));
        assert!(snapshot.paused);
        assert_eq!(snapshot.queued.len(), 2);

        let effects = pane.send_queued("");
        assert_eq!(dispatched(&effects), ["held"], "same thread, no recap");
    }

    #[test]
    fn a_new_message_while_paused_joins_the_queue_instead_of_jumping_it() {
        let mut pane = live("s1");
        pane.submit("one".into(), "");
        pane.turn_started("s1", "t1");
        pane.submit("held".into(), "");
        pane.turn_ended("s1", Some("t1"), AgentTurnOutcome::Stopped);
        assert_eq!(pane.submit("newer".into(), "").0, AgentSubmitResult::Queued);
        assert_eq!(pane.discard_queued(), 2);
        let (_, effects) = pane.submit("fresh".into(), "");
        assert_eq!(dispatched(&effects), ["fresh"]);
    }

    #[test]
    fn rejected_stop_returns_to_working_and_can_be_retried() {
        let mut pane = live("s1");
        pane.submit("one".into(), "");
        pane.turn_started("s1", "t1");
        pane.stop();
        pane.stop_failed("s1");
        let snapshot = pane.snapshot();
        assert_eq!(snapshot.phase, AgentPanePhase::Working);
        assert!(snapshot.stop_failed);
        assert_eq!(pane.stop().len(), 1, "retry sends another interrupt");
    }

    #[test]
    fn events_for_earlier_turns_or_other_sessions_are_ignored() {
        let mut pane = live("s1");
        pane.submit("one".into(), "");
        pane.turn_started("s1", "t2");
        assert!(pane
            .turn_ended("s1", Some("t1"), AgentTurnOutcome::Stopped)
            .is_none());
        assert!(pane
            .turn_ended("other", Some("t2"), AgentTurnOutcome::Stopped)
            .is_none());
        assert_eq!(pane.phase(), AgentPanePhase::Working);
    }

    #[test]
    fn failed_start_holds_the_prompt_until_retried() {
        let mut pane = AgentPaneLifecycle::default();
        pane.submit("hello".into(), "");
        pane.start_failed();
        let snapshot = pane.snapshot();
        assert_eq!(snapshot.phase, AgentPanePhase::Idle);
        assert!(snapshot.paused);
        assert_eq!(pane.send_queued(""), [AgentPaneEffect::StartSession]);
        assert_eq!(dispatched(&pane.session_created("s1")), ["hello"]);
    }

    #[test]
    fn unsent_prompt_is_retried_with_a_recap_after_the_app_server_dies() {
        let mut pane = live("s1");
        pane.submit("lost in transit".into(), "You: earlier\n");
        let effects = pane.session_lost(/*unsent*/ true);
        assert_eq!(effects, [AgentPaneEffect::StartSession]);
        let effects = pane.session_created("s2");
        let wire = dispatched(&effects);
        assert!(wire[0].starts_with("Previous conversation"));
        assert!(wire[0].ends_with("Follow-up:\nlost in transit"));
    }

    #[test]
    fn a_retried_prompt_recaps_the_conversation_from_before_it_was_sent() {
        let mut pane = live("s1");
        pane.submit("first".into(), "");
        pane.turn_started("s1", "t1");
        pane.turn_ended("s1", Some("t1"), AgentTurnOutcome::Completed);
        pane.submit("retry me".into(), "You: first\nAgent: done\n");
        pane.session_lost(/*unsent*/ true);
        // The pane has since committed the prompt and shown an error.
        pane.start_failed();
        pane.send_queued("You: first\nAgent: done\nYou: retry me\nError: lost\n");
        let wire = dispatched(&pane.session_created("s2"));
        assert!(wire[0].contains("Agent: done"));
        assert!(!wire[0].contains("Error: lost"));
        assert!(wire[0].ends_with("Follow-up:\nretry me"));
        assert_eq!(wire[0].matches("retry me").count(), 1);
    }

    #[test]
    fn an_auto_sent_follow_up_recaps_the_answer_that_finished_before_it() {
        let mut pane = live("s1");
        pane.submit("first".into(), "");
        pane.turn_started("s1", "t1");
        pane.observe(
            "agent:update",
            &serde_json::json!({ "session_id": "s1", "text": "partial " }),
        );
        // Queued mid-stream: the pane's transcript has only part of the answer.
        pane.submit("second".into(), "You: first\nAgent: partial ");
        pane.observe(
            "agent:update",
            &serde_json::json!({ "session_id": "s1", "text": "and the rest" }),
        );
        pane.turn_ended("s1", Some("t1"), AgentTurnOutcome::Completed);
        pane.session_lost(/*unsent*/ true);
        let wire = dispatched(&pane.session_created("s2"));
        assert!(wire[0].contains("You: first\nAgent: partial and the rest\n"));
        assert!(wire[0].ends_with("Follow-up:\nsecond"));
        assert_eq!(wire[0].matches("second").count(), 1);
    }

    #[test]
    fn reset_while_a_thread_is_starting_adopts_it_instead_of_starting_another() {
        let mut pane = AgentPaneLifecycle::default();
        assert_eq!(pane.start(), [AgentPaneEffect::StartSession]);
        let mut effects = pane.reset();
        effects.extend(pane.start());
        assert!(effects.is_empty(), "no second thread: {effects:?}");
        assert_eq!(pane.phase(), AgentPanePhase::Starting);
        pane.session_created("s1");
        assert_eq!(pane.snapshot().session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn a_thread_forgotten_while_restoring_is_not_adopted_when_it_comes_back() {
        let mut pane = AgentPaneLifecycle::default();
        pane.restore_pending("old", "/tmp");
        let effects = pane.reset();
        assert_eq!(
            effects,
            [AgentPaneEffect::ForgetSession {
                session_id: "old".into()
            }]
        );
        assert!(pane.restored("old").is_empty());
        assert_eq!(pane.snapshot().session_id, None);
        assert_eq!(pane.start(), [AgentPaneEffect::StartSession]);
    }

    #[test]
    fn a_turn_that_already_ran_is_not_replayed_when_the_app_server_dies() {
        let mut pane = live("s1");
        pane.submit("edits files".into(), "");
        pane.turn_started("s1", "t1");
        pane.submit("held".into(), "");
        assert!(pane.session_lost(/*unsent*/ false).is_empty());
        let snapshot = pane.snapshot();
        assert_eq!(snapshot.session_id, None);
        assert!(snapshot.paused && snapshot.recap_pending);
        assert_eq!(snapshot.queued.len(), 1);
    }

    #[test]
    fn restore_resumes_the_thread_and_a_failure_falls_back_to_a_recap() {
        let mut pane = AgentPaneLifecycle::default();
        assert_eq!(
            pane.restore_pending("s1", "/workspace"),
            [AgentPaneEffect::ResumeSession {
                session_id: "s1".into(),
                cwd: "/workspace".into()
            }]
        );
        assert_eq!(pane.phase(), AgentPanePhase::Restoring);
        assert_eq!(
            pane.submit("waits".into(), "").0,
            AgentSubmitResult::Accepted
        );
        assert_eq!(dispatched(&pane.restored("s1")), ["waits"]);

        let mut pane = AgentPaneLifecycle::default();
        pane.restore_pending("s1", "/workspace");
        assert_eq!(
            pane.restore_failed("s1"),
            [AgentPaneEffect::ForgetSession {
                session_id: "s1".into()
            }]
        );
        assert!(pane.snapshot().recap_pending);
        assert_eq!(
            pane.submit("next".into(), "You: old\n").1,
            [AgentPaneEffect::StartSession]
        );
    }

    #[test]
    fn an_already_live_restored_thread_is_adopted_but_never_replaces_another() {
        let mut pane = AgentPaneLifecycle::default();
        pane.restored("s1");
        assert_eq!(pane.snapshot().session_id.as_deref(), Some("s1"));
        let (_, effects) = pane.submit("continue".into(), "");
        assert_eq!(
            dispatched(&effects),
            ["continue"],
            "no new thread is started"
        );

        let mut pane = live("s1");
        pane.restored("other");
        assert_eq!(pane.snapshot().session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn switching_threads_keeps_the_previous_thread_until_the_replacement_restores() {
        let mut pane = live("old");
        assert!(pane.switch_pending("selected"));
        assert_eq!(pane.phase(), AgentPanePhase::Restoring);
        assert_eq!(pane.snapshot().session_id.as_deref(), Some("old"));

        assert_eq!(
            pane.restored("new"),
            [AgentPaneEffect::CloseSession {
                session_id: "old".into()
            }]
        );
        assert_eq!(pane.phase(), AgentPanePhase::Idle);
        assert_eq!(pane.snapshot().session_id.as_deref(), Some("new"));
    }

    #[test]
    fn a_failed_thread_switch_returns_to_the_previous_thread() {
        let mut pane = live("old");
        assert!(pane.switch_pending("selected"));

        assert!(pane.restore_failed("selected").is_empty());
        let snapshot = pane.snapshot();
        assert_eq!(snapshot.phase, AgentPanePhase::Idle);
        assert_eq!(snapshot.session_id.as_deref(), Some("old"));
        assert!(!snapshot.recap_pending);
    }

    #[test]
    fn switching_threads_does_not_move_held_prompts_to_another_conversation() {
        let mut pane = live("old");
        pane.submit("running".into(), "");
        pane.turn_started("old", "turn");
        pane.submit("held".into(), "");
        assert_eq!(
            pane.stop(),
            [AgentPaneEffect::Interrupt {
                session_id: "old".into()
            }]
        );
        assert!(pane
            .turn_ended("old", Some("turn"), AgentTurnOutcome::Stopped)
            .unwrap()
            .is_empty());

        assert_eq!(pane.phase(), AgentPanePhase::Idle);
        assert!(pane.snapshot().paused);
        assert!(!pane.switch_pending("selected"));
        assert_eq!(pane.snapshot().session_id.as_deref(), Some("old"));
    }

    #[test]
    fn a_bridge_restart_abandons_an_in_flight_thread_switch() {
        let mut pane = live("old");
        assert!(pane.switch_pending("selected"));

        assert_eq!(
            pane.restore_pending("old", "/workspace"),
            [AgentPaneEffect::ResumeSession {
                session_id: "old".into(),
                cwd: "/workspace".into()
            }]
        );
        assert!(pane.restored("new").is_empty());
        assert_eq!(pane.snapshot().session_id.as_deref(), Some("old"));
        assert_eq!(pane.phase(), AgentPanePhase::Restoring);
        assert!(pane.restored("old").is_empty());
        assert_eq!(pane.phase(), AgentPanePhase::Idle);
    }

    #[test]
    fn reset_forgets_the_thread_and_clears_everything() {
        let mut pane = live("s1");
        pane.submit("one".into(), "");
        pane.submit("two".into(), "");
        assert_eq!(
            pane.reset(),
            [AgentPaneEffect::ForgetSession {
                session_id: "s1".into()
            }]
        );
        let snapshot = pane.snapshot();
        assert_eq!(snapshot.phase, AgentPanePhase::Idle);
        assert!(snapshot.queued.is_empty() && snapshot.session_id.is_none());
        let (_, effects) = pane.submit("again".into(), "");
        assert_eq!(effects, [AgentPaneEffect::StartSession]);
        assert_ne!(pane.snapshot().queued[0].id, "prompt:1", "ids stay unique");
    }

    #[test]
    fn eager_start_creates_one_thread_and_a_waiting_prompt_uses_it() {
        let mut pane = AgentPaneLifecycle::default();
        assert_eq!(pane.start(), [AgentPaneEffect::StartSession]);
        assert!(pane.start().is_empty(), "already starting");
        let (result, effects) = pane.submit("hello".into(), "");
        assert_eq!(result, AgentSubmitResult::Accepted);
        assert!(effects.is_empty(), "waits for the thread being started");
        assert_eq!(dispatched(&pane.session_created("s1")), ["hello"]);
        assert!(pane.start().is_empty(), "a live thread needs no start");
    }

    #[test]
    fn the_queue_is_bounded() {
        let mut pane = live("s1");
        pane.submit("running".into(), "");
        for index in 0..MAX_QUEUED_PROMPTS {
            assert_eq!(
                pane.submit(format!("{index}"), "").0,
                AgentSubmitResult::Queued
            );
        }
        assert_eq!(
            pane.submit("overflow".into(), "").0,
            AgentSubmitResult::QueueFull
        );
    }

    fn crashed_mid_turn() -> AgentPaneLifecycle {
        let mut pane = live("s1");
        pane.submit("write an essay".into(), "");
        pane.turn_started("s1", "t1");
        pane.observe(
            "agent:update",
            &serde_json::json!({ "session_id": "s1", "text": "The first half" }),
        );
        pane.submit("held follow-up".into(), "");
        let effects = pane.restore_pending("s1", "/workspace");
        assert_eq!(
            effects,
            [AgentPaneEffect::ResumeSession {
                session_id: "s1".into(),
                cwd: "/workspace".into()
            }],
            "the thread still resumes in the background"
        );
        pane
    }

    #[test]
    fn a_crash_mid_turn_holds_everything_and_asks_what_to_do() {
        let mut pane = crashed_mid_turn();
        let snapshot = pane.snapshot();
        assert_eq!(
            snapshot
                .interrupted
                .as_ref()
                .map(|prompt| prompt.text.as_str()),
            Some("write an essay")
        );
        assert!(snapshot.in_flight.is_none());
        assert!(snapshot.paused);
        assert!(
            pane.restored("s1").is_empty(),
            "nothing is sent automatically after the resume"
        );
        assert_eq!(pane.snapshot().queued.len(), 1);
    }

    #[test]
    fn retrying_an_interrupted_turn_resends_it_with_a_warning_then_drains_the_queue() {
        let mut pane = crashed_mid_turn();
        pane.restored("s1");
        let effects = pane.resolve_interruption(AgentInterruptionChoice::Retry, "");
        let wire = dispatched(&effects);
        assert!(wire[0].contains("still applied"), "{}", wire[0]);
        assert!(wire[0].ends_with("write an essay"));
        let snapshot = pane.snapshot();
        assert!(snapshot.interrupted.is_none());
        assert_eq!(
            snapshot.in_flight.map(|prompt| prompt.text).as_deref(),
            Some("write an essay"),
            "the pane shows the plain message"
        );
        pane.turn_started("s1", "t2");
        let effects = pane
            .turn_ended("s1", Some("t2"), AgentTurnOutcome::Completed)
            .unwrap();
        assert_eq!(dispatched(&effects), ["held follow-up"]);
    }

    #[test]
    fn continuing_an_interrupted_turn_waits_for_the_resume_to_finish() {
        let mut pane = crashed_mid_turn();
        let effects = pane.resolve_interruption(AgentInterruptionChoice::Continue, "");
        assert!(effects.is_empty(), "still restoring: {effects:?}");
        assert_eq!(pane.snapshot().queued[0].text, CONTINUE_PROMPT);
        let wire = dispatched(&pane.restored("s1"));
        assert!(wire[0].starts_with("Codex stopped unexpectedly"));
        assert!(wire[0].contains("Continue from where you stopped"));
    }

    #[test]
    fn dismissing_an_interrupted_turn_keeps_follow_ups_paused() {
        let mut pane = crashed_mid_turn();
        pane.restored("s1");
        assert!(pane
            .resolve_interruption(AgentInterruptionChoice::Dismiss, "")
            .is_empty());
        let snapshot = pane.snapshot();
        assert!(snapshot.interrupted.is_none());
        assert!(snapshot.paused);
        assert_eq!(dispatched(&pane.send_queued("")), ["held follow-up"]);
    }

    #[test]
    fn typing_a_new_message_answers_the_interruption() {
        let mut pane = live("s1");
        pane.submit("long".into(), "");
        pane.turn_started("s1", "t1");
        pane.restore_pending("s1", "/workspace");
        pane.restored("s1");
        let (_, effects) = pane.submit("something else".into(), "");
        assert_eq!(dispatched(&effects), ["something else"]);
        assert!(pane.snapshot().interrupted.is_none());
    }

    #[test]
    fn an_idle_crash_resumes_quietly() {
        let mut pane = live("s1");
        pane.submit("done".into(), "");
        pane.turn_started("s1", "t1");
        pane.turn_ended("s1", Some("t1"), AgentTurnOutcome::Completed);
        pane.restore_pending("s1", "/workspace");
        assert!(pane.snapshot().interrupted.is_none());
        pane.restored("s1");
        let (_, effects) = pane.submit("next".into(), "");
        assert_eq!(dispatched(&effects), ["next"]);
    }

    #[test]
    fn a_turn_lost_with_its_app_server_is_never_dropped_silently() {
        let mut pane = live("s1");
        pane.submit("edits files".into(), "You: earlier\n");
        pane.turn_started("s1", "t1");
        assert!(pane.session_lost(/*unsent*/ false).is_empty());
        assert_eq!(
            pane.snapshot()
                .interrupted
                .map(|prompt| prompt.text)
                .as_deref(),
            Some("edits files")
        );
        let effects = pane.resolve_interruption(AgentInterruptionChoice::Retry, "You: earlier\n");
        assert_eq!(effects, [AgentPaneEffect::StartSession]);
        let wire = dispatched(&pane.session_created("s2"));
        assert!(wire[0].starts_with("Previous conversation"), "{}", wire[0]);
        assert!(wire[0].ends_with("edits files"));
    }

    #[test]
    fn a_saved_thread_resumes_when_the_pane_opens_or_on_the_first_message() {
        let mut pane = AgentPaneLifecycle::default();
        pane.observe(
            "agent:conversation_resumable",
            &serde_json::json!({ "thread_id": "saved", "cwd": "/workspace" }),
        );
        let snapshot = pane.snapshot();
        assert!(snapshot.resume_pending);
        assert_eq!(snapshot.phase, AgentPanePhase::Idle);
        assert_eq!(
            pane.prepare(),
            [AgentPaneEffect::ResumeSession {
                session_id: "saved".into(),
                cwd: "/workspace".into()
            }]
        );
        assert!(pane.prepare().is_empty(), "resumes once");
        assert!(!pane.snapshot().resume_pending);

        let mut pane = AgentPaneLifecycle::default();
        pane.resumable("saved", "/workspace");
        let (_, effects) = pane.submit("codeword?".into(), "");
        assert_eq!(
            effects,
            [AgentPaneEffect::ResumeSession {
                session_id: "saved".into(),
                cwd: "/workspace".into()
            }]
        );
        assert_eq!(dispatched(&pane.restored("saved")), ["codeword?"]);
    }

    #[test]
    fn a_saved_thread_that_cannot_resume_continues_in_a_new_one_with_a_recap() {
        let mut pane = AgentPaneLifecycle::default();
        pane.resumable("saved", "/workspace");
        pane.submit("codeword?".into(), "You: remember HERON\n");
        let effects = pane.restore_failed("saved");
        assert_eq!(
            effects,
            [
                AgentPaneEffect::ForgetSession {
                    session_id: "saved".into()
                },
                AgentPaneEffect::StartSession
            ]
        );
        let wire = dispatched(&pane.session_created("fresh"));
        assert!(wire[0].contains("remember HERON"));
        assert!(wire[0].ends_with("codeword?"));
    }

    #[test]
    fn reset_forgets_a_saved_thread_before_it_is_resumed() {
        let mut pane = AgentPaneLifecycle::default();
        pane.resumable("saved", "/workspace");
        assert_eq!(
            pane.reset(),
            [AgentPaneEffect::ForgetSession {
                session_id: "saved".into()
            }]
        );
        assert!(!pane.snapshot().resume_pending);
        assert!(pane.prepare().is_empty());
    }

    #[test]
    fn a_turn_that_fails_on_its_own_asks_but_a_requested_stop_does_not() {
        let mut pane = live("s1");
        pane.submit("long task".into(), "");
        pane.turn_started("s1", "t1");
        pane.turn_ended("s1", Some("t1"), AgentTurnOutcome::Stopped);
        assert_eq!(
            pane.snapshot()
                .interrupted
                .map(|prompt| prompt.text)
                .as_deref(),
            Some("long task")
        );
        let wire = dispatched(&pane.resolve_interruption(AgentInterruptionChoice::Retry, ""));
        assert!(wire[0].ends_with("long task"));

        pane.turn_started("s1", "t2");
        pane.stop();
        pane.turn_ended("s1", Some("t2"), AgentTurnOutcome::Stopped);
        assert!(
            pane.snapshot().interrupted.is_none(),
            "the user chose to stop"
        );
    }

    #[test]
    fn recap_uses_only_the_transcript_tail() {
        let mut pane = AgentPaneLifecycle::default();
        pane.archived_transcript_restored(true);
        let transcript = format!("{}{}", "x".repeat(MAX_RECAP_CHARS), "TAIL");
        pane.submit("next".into(), &transcript);
        let wire = dispatched(&pane.session_created("s1"));
        assert!(wire[0].contains("TAIL"));
        assert!(!wire[0].contains(&"x".repeat(MAX_RECAP_CHARS)));
    }
}
