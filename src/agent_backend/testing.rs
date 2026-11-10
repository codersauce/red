//! A scripted agent backend for tests.
//!
//! `ScriptedBackend` stands in for a real agent: it records every command as a
//! [`Call`], answers with whatever the test pushed through [`Script`], and
//! never spawns anything. [`open_scripted`] wraps it in the real [`Ledger`], so
//! editor tests see the same event stream the editor sees in production.

use std::{
    collections::VecDeque,
    future::Future,
    mem,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
};

use super::{
    backend::{AgentBackend, AgentError, CommitMessages, InlineAssist, Missing},
    events::{Event, Lost, LostScope, TurnEvent, TurnEventKind},
    ids::{BackendKind, Continuity, ContractStamp, QueryId, SavedSession, SessionId, TurnId},
    ledger::Ledger,
    types::{ModelSelection, Prompt, SessionQuery},
};

/// A command the editor sent to the backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Call {
    Ensure(PathBuf),
    NewSession {
        cwd: PathBuf,
        model: Option<ModelSelection>,
    },
    Restore {
        cwd: PathBuf,
        saved: SavedSession,
    },
    Prompt {
        session: SessionId,
        turn: TurnId,
        prompt: Prompt,
    },
    Stop {
        session: SessionId,
        turn: TurnId,
    },
    Close(SessionId),
    DefaultModel {
        id: QueryId,
        cwd: PathBuf,
    },
    ListModels(QueryId),
    SetModel {
        id: QueryId,
        session: SessionId,
        selection: ModelSelection,
    },
    ListSessions {
        id: QueryId,
        query: SessionQuery,
    },
    PreviewSession {
        id: QueryId,
        cwd: PathBuf,
        session: SessionId,
    },
    InlineStart {
        request: String,
        cwd: PathBuf,
        prompt: String,
        context: String,
    },
    InlineFollowup {
        request: String,
        session: SessionId,
        prompt: String,
        context: String,
    },
    CommitMessage {
        id: QueryId,
        cwd: PathBuf,
        prompt: String,
    },
    DetachForExit,
    Shutdown,
}

#[derive(Default)]
struct Shared {
    calls: Vec<Call>,
    inbox: VecDeque<Event>,
    failure: Option<AgentError>,
    stopped: bool,
    without_capabilities: bool,
    open_turns: Vec<(SessionId, TurnId)>,
    open_session: bool,
    ledger_queued: usize,
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().expect("scripted backend state is poisoned")
}

/// The backend half of a scripted pair. Running from the start; `Script::lose`
/// stops it, and `ensure`, `new_session` and `restore` start it again.
pub(crate) struct ScriptedBackend {
    shared: Arc<Mutex<Shared>>,
    root: Option<PathBuf>,
}

impl ScriptedBackend {
    /// A backend and the handle a test scripts it with.
    pub(crate) fn new() -> (Self, Script) {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let backend = Self {
            shared: Arc::clone(&shared),
            root: None,
        };
        (backend, Script { shared })
    }

    /// Records `call`, then fails if the test asked the next command to fail.
    fn record(&self, call: Call) -> Result<(), AgentError> {
        let mut shared = lock(&self.shared);
        shared.calls.push(call);
        match shared.failure.take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn mark_running(&self) {
        lock(&self.shared).stopped = false;
    }

    fn offers(&self, feature: &'static str) -> Result<(), Missing> {
        if lock(&self.shared).without_capabilities {
            Err(Missing {
                feature,
                active: self.kind().name(),
            })
        } else {
            Ok(())
        }
    }
}

impl AgentBackend for ScriptedBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Codex
    }

    fn contract(&self) -> ContractStamp {
        ContractStamp::new(1)
    }

    fn continuity(&self, saved: &SavedSession) -> Continuity {
        if saved.contract == Some(self.contract()) {
            Continuity::Rejoin
        } else {
            Continuity::Fork
        }
    }

    fn is_running(&self) -> bool {
        !lock(&self.shared).stopped
    }

    fn ensure(&mut self, root: &Path) -> Result<(), AgentError> {
        self.record(Call::Ensure(root.to_path_buf()))?;
        if let Some(current) = self.root.as_deref().filter(|current| *current != root) {
            return Err(AgentError::Unavailable(format!(
                "agent root `{}` does not match the active agent workspace `{}`",
                root.display(),
                current.display()
            )));
        }
        self.root = Some(root.to_path_buf());
        self.mark_running();
        Ok(())
    }

    fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    fn new_session(&mut self, cwd: &Path, model: Option<ModelSelection>) -> Result<(), AgentError> {
        self.record(Call::NewSession {
            cwd: cwd.to_path_buf(),
            model,
        })?;
        self.mark_running();
        Ok(())
    }

    fn restore(&mut self, cwd: &Path, saved: &SavedSession) -> Result<(), AgentError> {
        self.record(Call::Restore {
            cwd: cwd.to_path_buf(),
            saved: saved.clone(),
        })?;
        self.mark_running();
        Ok(())
    }

    fn prompt(
        &mut self,
        session: &SessionId,
        turn: TurnId,
        prompt: Prompt,
    ) -> Result<(), AgentError> {
        self.record(Call::Prompt {
            session: session.clone(),
            turn,
            prompt,
        })
    }

    fn stop(&mut self, session: &SessionId, turn: TurnId) -> Result<(), AgentError> {
        self.record(Call::Stop {
            session: session.clone(),
            turn,
        })
    }

    fn close(&mut self, session: &SessionId) -> Result<(), AgentError> {
        self.record(Call::Close(session.clone()))
    }

    fn default_model(&mut self, id: QueryId, cwd: &Path) -> Result<(), AgentError> {
        self.record(Call::DefaultModel {
            id,
            cwd: cwd.to_path_buf(),
        })
    }

    fn list_models(&mut self, id: QueryId) -> Result<(), AgentError> {
        self.record(Call::ListModels(id))
    }

    fn set_model(
        &mut self,
        id: QueryId,
        session: &SessionId,
        selection: ModelSelection,
    ) -> Result<(), AgentError> {
        self.record(Call::SetModel {
            id,
            session: session.clone(),
            selection,
        })
    }

    fn list_sessions(&mut self, id: QueryId, query: SessionQuery) -> Result<(), AgentError> {
        self.record(Call::ListSessions { id, query })
    }

    fn preview_session(
        &mut self,
        id: QueryId,
        cwd: &Path,
        session: &SessionId,
    ) -> Result<(), AgentError> {
        self.record(Call::PreviewSession {
            id,
            cwd: cwd.to_path_buf(),
            session: session.clone(),
        })
    }

    fn poll(&mut self) -> Option<Event> {
        let event = lock(&self.shared).inbox.pop_front();
        if let Some(Event::Lost(Lost {
            scope: LostScope::Backend,
            ..
        })) = &event
        {
            self.root = None;
        }
        event
    }

    fn shutdown(&mut self) {
        let mut shared = lock(&self.shared);
        shared.calls.push(Call::Shutdown);
        shared.inbox.clear();
        shared.stopped = true;
        self.root = None;
    }

    fn detach_for_exit(&mut self) -> Option<Pin<Box<dyn Future<Output = ()> + Send>>> {
        let mut shared = lock(&self.shared);
        shared.calls.push(Call::DetachForExit);
        shared.stopped = true;
        None
    }

    fn inline_assist(&mut self) -> Result<&mut dyn InlineAssist, Missing> {
        self.offers("Inline assist")?;
        Ok(self)
    }

    fn commit_messages(&mut self) -> Result<&mut dyn CommitMessages, Missing> {
        self.offers("Commit message generation")?;
        Ok(self)
    }
}

impl InlineAssist for ScriptedBackend {
    fn start(
        &mut self,
        request: String,
        cwd: &Path,
        prompt: String,
        context: String,
    ) -> Result<(), AgentError> {
        self.record(Call::InlineStart {
            request,
            cwd: cwd.to_path_buf(),
            prompt,
            context,
        })
    }

    fn followup(
        &mut self,
        request: String,
        session: &SessionId,
        prompt: String,
        context: String,
    ) -> Result<(), AgentError> {
        self.record(Call::InlineFollowup {
            request,
            session: session.clone(),
            prompt,
            context,
        })
    }
}

impl CommitMessages for ScriptedBackend {
    fn generate(&mut self, id: QueryId, cwd: &Path, prompt: String) -> Result<(), AgentError> {
        self.record(Call::CommitMessage {
            id,
            cwd: cwd.to_path_buf(),
            prompt,
        })
    }
}

/// The test's handle on a [`ScriptedBackend`]. Every method is synchronous.
#[derive(Clone)]
pub(crate) struct Script {
    shared: Arc<Mutex<Shared>>,
}

impl Script {
    /// Queues an event for the next `poll` to return.
    pub(crate) fn emit(&self, event: Event) {
        lock(&self.shared).inbox.push_back(event);
    }

    /// Queues a turn event.
    pub(crate) fn emit_turn(&self, session: &SessionId, turn: TurnId, kind: TurnEventKind) {
        self.emit(Event::Turn(TurnEvent {
            session: session.clone(),
            turn,
            kind,
        }));
    }

    /// Every command the backend received so far, failed ones included.
    pub(crate) fn calls(&self) -> Vec<Call> {
        lock(&self.shared).calls.clone()
    }

    /// Like [`Script::calls`], and forgets them.
    pub(crate) fn take_calls(&self) -> Vec<Call> {
        mem::take(&mut lock(&self.shared).calls)
    }

    /// Ends the backend: it stops running and, after the events already
    /// queued, reports `Lost` with exactly `message`.
    pub(crate) fn lose(&self, message: &str) {
        let mut shared = lock(&self.shared);
        shared.stopped = true;
        shared.inbox.push_back(Event::Lost(Lost {
            scope: LostScope::Backend,
            message: message.to_string(),
            had_sessions: false,
        }));
    }

    /// Ends one session of the backend, which keeps running.
    pub(crate) fn lose_session(&self, session: &SessionId, message: &str) {
        self.emit(Event::Lost(Lost {
            scope: LostScope::Session(session.clone()),
            message: message.to_string(),
            had_sessions: false,
        }));
    }

    /// Makes the next command fail with `error` after it is recorded.
    pub(crate) fn fail_next(&self, error: AgentError) {
        lock(&self.shared).failure = Some(error);
    }

    /// How many events were queued and not yet delivered, counting those the
    /// ledger is holding back.
    pub(crate) fn pending(&self) -> usize {
        let shared = lock(&self.shared);
        shared.inbox.len() + shared.ledger_queued
    }

    /// Makes the backend stop offering inline assist and commit messages.
    pub(crate) fn withhold_capabilities(&self) {
        lock(&self.shared).without_capabilities = true;
    }

    /// Opens `turn` in the ledger as a successful `prompt` would, without
    /// sending a command. Only meaningful for [`open_scripted`], and takes
    /// effect at its next call.
    pub(crate) fn open_turn(&self, session: &SessionId, turn: TurnId) {
        lock(&self.shared).open_turns.push((session.clone(), turn));
    }

    /// Tells the ledger a conversation exists, as a successful `new_session`
    /// would, without sending a command. Only meaningful for
    /// [`open_scripted`], and takes effect at its next call.
    pub(crate) fn open_session(&self) {
        lock(&self.shared).open_session = true;
    }
}

/// The scripted backend inside the real ledger, plus the hooks that let a
/// [`Script`] register state the editor would have created with commands.
struct ScriptedLedger {
    ledger: Ledger<ScriptedBackend>,
    shared: Arc<Mutex<Shared>>,
}

impl ScriptedLedger {
    /// Applies what the script registered since the last call, runs `f`, and
    /// publishes how many events the ledger holds back.
    fn with<T>(&mut self, f: impl FnOnce(&mut Ledger<ScriptedBackend>) -> T) -> T {
        let (turns, session) = {
            let mut shared = lock(&self.shared);
            (
                mem::take(&mut shared.open_turns),
                mem::take(&mut shared.open_session),
            )
        };
        if session {
            self.ledger.note_session();
        }
        for (session, turn) in turns {
            self.ledger.open_turn(session, turn);
        }
        let result = f(&mut self.ledger);
        lock(&self.shared).ledger_queued = self.ledger.queued();
        result
    }
}

impl AgentBackend for ScriptedLedger {
    fn kind(&self) -> BackendKind {
        self.ledger.kind()
    }

    fn contract(&self) -> ContractStamp {
        self.ledger.contract()
    }

    fn continuity(&self, saved: &SavedSession) -> Continuity {
        self.ledger.continuity(saved)
    }

    fn is_running(&self) -> bool {
        self.ledger.is_running()
    }

    fn ensure(&mut self, root: &Path) -> Result<(), AgentError> {
        self.with(|ledger| ledger.ensure(root))
    }

    fn root(&self) -> Option<&Path> {
        self.ledger.root()
    }

    fn new_session(&mut self, cwd: &Path, model: Option<ModelSelection>) -> Result<(), AgentError> {
        self.with(|ledger| ledger.new_session(cwd, model))
    }

    fn restore(&mut self, cwd: &Path, saved: &SavedSession) -> Result<(), AgentError> {
        self.with(|ledger| ledger.restore(cwd, saved))
    }

    fn prompt(
        &mut self,
        session: &SessionId,
        turn: TurnId,
        prompt: Prompt,
    ) -> Result<(), AgentError> {
        self.with(|ledger| ledger.prompt(session, turn, prompt))
    }

    fn stop(&mut self, session: &SessionId, turn: TurnId) -> Result<(), AgentError> {
        self.with(|ledger| ledger.stop(session, turn))
    }

    fn close(&mut self, session: &SessionId) -> Result<(), AgentError> {
        self.with(|ledger| ledger.close(session))
    }

    fn default_model(&mut self, id: QueryId, cwd: &Path) -> Result<(), AgentError> {
        self.with(|ledger| ledger.default_model(id, cwd))
    }

    fn list_models(&mut self, id: QueryId) -> Result<(), AgentError> {
        self.with(|ledger| ledger.list_models(id))
    }

    fn set_model(
        &mut self,
        id: QueryId,
        session: &SessionId,
        selection: ModelSelection,
    ) -> Result<(), AgentError> {
        self.with(|ledger| ledger.set_model(id, session, selection))
    }

    fn list_sessions(&mut self, id: QueryId, query: SessionQuery) -> Result<(), AgentError> {
        self.with(|ledger| ledger.list_sessions(id, query))
    }

    fn preview_session(
        &mut self,
        id: QueryId,
        cwd: &Path,
        session: &SessionId,
    ) -> Result<(), AgentError> {
        self.with(|ledger| ledger.preview_session(id, cwd, session))
    }

    fn poll(&mut self) -> Option<Event> {
        self.with(Ledger::poll)
    }

    fn shutdown(&mut self) {
        self.with(Ledger::shutdown);
    }

    fn detach_for_exit(&mut self) -> Option<Pin<Box<dyn Future<Output = ()> + Send>>> {
        self.with(Ledger::detach_for_exit)
    }

    fn inline_assist(&mut self) -> Result<&mut dyn InlineAssist, Missing> {
        self.with(|ledger| ledger.inline_assist().map(|_| ()))?;
        Ok(self)
    }

    fn commit_messages(&mut self) -> Result<&mut dyn CommitMessages, Missing> {
        self.with(|ledger| ledger.commit_messages().map(|_| ()))?;
        Ok(self)
    }
}

impl InlineAssist for ScriptedLedger {
    fn start(
        &mut self,
        request: String,
        cwd: &Path,
        prompt: String,
        context: String,
    ) -> Result<(), AgentError> {
        self.with(|ledger| {
            ledger
                .inline_assist()
                .map_err(AgentError::from)?
                .start(request, cwd, prompt, context)
        })
    }

    fn followup(
        &mut self,
        request: String,
        session: &SessionId,
        prompt: String,
        context: String,
    ) -> Result<(), AgentError> {
        self.with(|ledger| {
            ledger
                .inline_assist()
                .map_err(AgentError::from)?
                .followup(request, session, prompt, context)
        })
    }
}

impl CommitMessages for ScriptedLedger {
    fn generate(&mut self, id: QueryId, cwd: &Path, prompt: String) -> Result<(), AgentError> {
        self.with(|ledger| {
            ledger
                .commit_messages()
                .map_err(AgentError::from)?
                .generate(id, cwd, prompt)
        })
    }
}

/// A scripted backend inside the real [`Ledger`], as the editor holds it.
pub(crate) fn open_scripted() -> (Box<dyn AgentBackend>, Script) {
    let (backend, script) = ScriptedBackend::new();
    let backend = ScriptedLedger {
        ledger: Ledger::new(backend),
        shared: Arc::clone(&script.shared),
    };
    (Box::new(backend), script)
}

/// Asserts the turn protocol over a recorded event stream: for every turn,
/// at most one `Accepted`, before any content event; exactly one `End`; and
/// nothing after it.
///
/// A turn whose session was closed ends silently by design, so streams that
/// close sessions mid-turn do not satisfy the "exactly one `End`" rule.
pub(crate) fn assert_turn_discipline(events: &[Event]) {
    struct Seen {
        session: SessionId,
        turn: TurnId,
        accepted: bool,
        content: bool,
        ended: bool,
    }

    let mut seen: Vec<Seen> = Vec::new();
    for event in events {
        let Event::Turn(TurnEvent {
            session,
            turn,
            kind,
        }) = event
        else {
            continue;
        };
        let index = match seen
            .iter()
            .position(|seen| seen.turn == *turn && seen.session == *session)
        {
            Some(index) => index,
            None => {
                seen.push(Seen {
                    session: session.clone(),
                    turn: *turn,
                    accepted: false,
                    content: false,
                    ended: false,
                });
                seen.len() - 1
            }
        };
        let this = &mut seen[index];
        assert!(
            !this.ended,
            "turn {turn} of session {session} got {kind:?} after its End"
        );
        match kind {
            TurnEventKind::Accepted => {
                assert!(
                    !this.accepted,
                    "turn {turn} of session {session} was accepted twice"
                );
                assert!(
                    !this.content,
                    "turn {turn} of session {session} was accepted after content"
                );
                this.accepted = true;
            }
            TurnEventKind::Delta(_)
            | TurnEventKind::MessageDone(_)
            | TurnEventKind::Activity(_) => this.content = true,
            TurnEventKind::StopRequested | TurnEventKind::StopFailed { .. } => {}
            TurnEventKind::End(_) => this.ended = true,
        }
    }
    for this in &seen {
        assert!(
            this.ended,
            "turn {} of session {} never ended",
            this.turn, this.session
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_backend::events::{Completion, TurnEnd};

    fn sid(id: &str) -> SessionId {
        SessionId::from_raw(id.to_string())
    }

    fn prompt(text: &str) -> Prompt {
        Prompt {
            text: text.to_string(),
            context: None,
            recap: None,
        }
    }

    fn drain(backend: &mut dyn AgentBackend) -> Vec<Event> {
        std::iter::from_fn(|| backend.poll()).collect()
    }

    fn turn_event(session: &SessionId, turn: TurnId, kind: TurnEventKind) -> Event {
        Event::Turn(TurnEvent {
            session: session.clone(),
            turn,
            kind,
        })
    }

    fn end() -> TurnEventKind {
        TurnEventKind::End(TurnEnd::Completed(Completion::EndTurn))
    }

    #[test]
    fn scripted_backend_records_every_command_in_order() {
        let (mut backend, script) = ScriptedBackend::new();
        let cwd = PathBuf::from("/w");
        let (session, turn) = (sid("s1"), TurnId::mint());
        let selection = ModelSelection {
            model: "m".into(),
            effort: None,
        };
        let saved = SavedSession {
            backend: BackendKind::Codex,
            session: sid("old"),
            contract: Some(ContractStamp::new(1)),
        };
        let query = SessionQuery {
            cwd: cwd.clone(),
            scope: crate::agent_backend::types::SessionScope::Red,
            query: "q".into(),
            exclude_session_id: None,
            search_contents: false,
        };

        backend.ensure(&cwd).unwrap();
        backend.new_session(&cwd, Some(selection.clone())).unwrap();
        backend.restore(&cwd, &saved).unwrap();
        backend.prompt(&session, turn, prompt("hi")).unwrap();
        backend.stop(&session, turn).unwrap();
        backend.close(&session).unwrap();
        backend.default_model(QueryId(1), &cwd).unwrap();
        backend.list_models(QueryId(2)).unwrap();
        backend
            .set_model(QueryId(3), &session, selection.clone())
            .unwrap();
        backend.list_sessions(QueryId(4), query.clone()).unwrap();
        backend.preview_session(QueryId(5), &cwd, &session).unwrap();
        backend
            .inline_assist()
            .unwrap()
            .start("r1".into(), &cwd, "p".into(), "c".into())
            .unwrap();
        backend
            .inline_assist()
            .unwrap()
            .followup("r1".into(), &session, "p2".into(), "c2".into())
            .unwrap();
        backend
            .commit_messages()
            .unwrap()
            .generate(QueryId(6), &cwd, "diff".into())
            .unwrap();
        assert!(backend.detach_for_exit().is_none());
        backend.shutdown();

        assert_eq!(
            script.calls(),
            vec![
                Call::Ensure(cwd.clone()),
                Call::NewSession {
                    cwd: cwd.clone(),
                    model: Some(selection.clone())
                },
                Call::Restore {
                    cwd: cwd.clone(),
                    saved
                },
                Call::Prompt {
                    session: session.clone(),
                    turn,
                    prompt: prompt("hi")
                },
                Call::Stop {
                    session: session.clone(),
                    turn
                },
                Call::Close(session.clone()),
                Call::DefaultModel {
                    id: QueryId(1),
                    cwd: cwd.clone()
                },
                Call::ListModels(QueryId(2)),
                Call::SetModel {
                    id: QueryId(3),
                    session: session.clone(),
                    selection
                },
                Call::ListSessions {
                    id: QueryId(4),
                    query
                },
                Call::PreviewSession {
                    id: QueryId(5),
                    cwd: cwd.clone(),
                    session: session.clone()
                },
                Call::InlineStart {
                    request: "r1".into(),
                    cwd: cwd.clone(),
                    prompt: "p".into(),
                    context: "c".into()
                },
                Call::InlineFollowup {
                    request: "r1".into(),
                    session,
                    prompt: "p2".into(),
                    context: "c2".into()
                },
                Call::CommitMessage {
                    id: QueryId(6),
                    cwd,
                    prompt: "diff".into()
                },
                Call::DetachForExit,
                Call::Shutdown,
            ]
        );
        assert_eq!(script.take_calls().len(), 16);
        assert_eq!(script.calls(), Vec::new());
    }

    #[test]
    fn fail_next_fails_one_command_after_recording_it() {
        let (mut backend, script) = ScriptedBackend::new();
        let session = sid("s1");
        script.fail_next(AgentError::Unavailable("nope".into()));

        let error = backend.close(&session).unwrap_err();
        assert!(matches!(error, AgentError::Unavailable(m) if m == "nope"));
        backend.close(&session).unwrap();
        assert_eq!(
            script.calls(),
            vec![Call::Close(session.clone()), Call::Close(session)]
        );
    }

    #[test]
    fn emitted_events_come_back_in_order_and_are_counted() {
        let (mut backend, script) = ScriptedBackend::new();
        let (session, turn) = (sid("s1"), TurnId::mint());
        script.emit_turn(&session, turn, TurnEventKind::Accepted);
        script.emit(Event::Turn(TurnEvent {
            session: session.clone(),
            turn,
            kind: end(),
        }));
        assert_eq!(script.pending(), 2);

        assert_eq!(
            drain(&mut backend),
            vec![
                turn_event(&session, turn, TurnEventKind::Accepted),
                turn_event(&session, turn, end()),
            ]
        );
        assert_eq!(script.pending(), 0);
    }

    #[test]
    fn scripted_backend_runs_until_it_is_lost_or_shut_down() {
        let (mut backend, script) = ScriptedBackend::new();
        let cwd = PathBuf::from("/w");
        assert!(backend.is_running());
        assert_eq!(backend.root(), None);

        backend.ensure(&cwd).unwrap();
        assert_eq!(backend.root(), Some(cwd.as_path()));
        script.emit_turn(&sid("s"), TurnId::mint(), TurnEventKind::Accepted);
        script.lose("boom");
        assert!(!backend.is_running());
        let events = drain(&mut backend);
        assert_eq!(events.len(), 2, "queued events, then the loss");
        assert!(matches!(
            events.last(),
            Some(Event::Lost(Lost { scope: LostScope::Backend, message, .. })) if message == "boom"
        ));
        assert_eq!(backend.root(), None, "a lost worker has no root");

        backend.new_session(&cwd, None).unwrap();
        assert!(backend.is_running());

        script.emit_turn(&sid("s"), TurnId::mint(), TurnEventKind::Accepted);
        backend.shutdown();
        assert!(!backend.is_running());
        assert_eq!(drain(&mut backend), Vec::new(), "shutdown is silent");
        backend
            .restore(
                &cwd,
                &SavedSession {
                    backend: BackendKind::Codex,
                    session: sid("s"),
                    contract: None,
                },
            )
            .unwrap();
        assert!(backend.is_running());
    }

    #[test]
    fn one_session_can_be_lost_while_the_backend_keeps_running() {
        let (mut backend, script) = ScriptedBackend::new();
        let session = sid("s1");
        script.lose_session(&session, "session died");

        assert!(backend.is_running());
        assert_eq!(
            drain(&mut backend),
            vec![Event::Lost(Lost {
                scope: LostScope::Session(session),
                message: "session died".into(),
                had_sessions: false,
            })]
        );
    }

    #[test]
    fn ensure_refuses_a_second_root() {
        let (mut backend, _script) = ScriptedBackend::new();
        backend.ensure(Path::new("/a")).unwrap();
        backend.ensure(Path::new("/a")).unwrap();

        let error = backend.ensure(Path::new("/b")).unwrap_err();
        assert!(matches!(error, AgentError::Unavailable(_)), "{error}");
        assert_eq!(backend.root(), Some(Path::new("/a")));
    }

    #[test]
    fn continuity_rejoins_only_the_stamp_the_backend_writes() {
        let (backend, _script) = ScriptedBackend::new();
        assert_eq!(backend.kind(), BackendKind::Codex);
        assert_eq!(backend.contract(), ContractStamp::new(1));

        let saved = |contract| SavedSession {
            backend: BackendKind::Codex,
            session: sid("s"),
            contract,
        };
        assert_eq!(
            backend.continuity(&saved(Some(ContractStamp::new(1)))),
            Continuity::Rejoin
        );
        for stamp in [
            None,
            Some(ContractStamp::new(0)),
            Some(ContractStamp::new(2)),
        ] {
            assert_eq!(backend.continuity(&saved(stamp)), Continuity::Fork);
        }
    }

    #[test]
    fn a_backend_can_withhold_its_capabilities() {
        let (mut backend, script) = ScriptedBackend::new();
        script.withhold_capabilities();
        assert!(backend.inline_assist().is_err());
        assert!(backend.commit_messages().is_err());
    }

    #[test]
    fn open_scripted_runs_the_scripted_backend_inside_the_ledger() {
        let (mut backend, script) = open_scripted();
        let (session, turn) = (sid("s1"), TurnId::mint());
        let stray = TurnId::mint();
        backend.new_session(Path::new("/w"), None).unwrap();
        backend.prompt(&session, turn, prompt("hi")).unwrap();
        script.emit_turn(&session, turn, TurnEventKind::Delta("a".into()));
        script.emit_turn(&session, stray, TurnEventKind::Delta("dropped".into()));
        script.emit_turn(&session, turn, end());
        script.emit_turn(&session, turn, end());

        let events = drain(backend.as_mut());
        assert_eq!(
            events,
            vec![
                turn_event(&session, turn, TurnEventKind::Delta("a".into())),
                turn_event(&session, turn, end()),
            ]
        );
        assert_turn_discipline(&events);
        assert_eq!(script.pending(), 0);
        assert_eq!(backend.kind(), BackendKind::Codex);
        assert_eq!(backend.contract(), ContractStamp::new(1));
        assert!(backend.is_running());
    }

    #[test]
    fn open_turn_registers_a_turn_without_a_command() {
        let (mut backend, script) = open_scripted();
        let (one, two) = (sid("s1"), sid("s2"));
        let (first, second) = (TurnId::mint(), TurnId::mint());
        script.open_turn(&one, first);
        script.open_turn(&two, second);
        script.open_session();
        script.emit_turn(&one, first, TurnEventKind::Delta("a".into()));
        script.lose("boom");

        assert_eq!(script.pending(), 2);
        assert_eq!(
            backend.poll(),
            Some(turn_event(&one, first, TurnEventKind::Delta("a".into())))
        );
        assert_eq!(script.calls(), Vec::new(), "no command was sent");
        assert_eq!(script.pending(), 1);

        let rest = drain(backend.as_mut());
        assert_eq!(
            rest.len(),
            3,
            "an End for each turn, then the loss: {rest:?}"
        );
        assert!(matches!(
            rest.last(),
            Some(Event::Lost(Lost {
                had_sessions: true,
                ..
            }))
        ));
        assert_eq!(script.pending(), 0);
    }

    #[test]
    fn pending_counts_events_the_ledger_holds_back() {
        let (mut backend, script) = open_scripted();
        let (one, two) = (sid("s1"), sid("s2"));
        script.open_turn(&one, TurnId::mint());
        script.open_turn(&two, TurnId::mint());
        script.lose("boom");
        assert_eq!(script.pending(), 1);

        assert!(backend.poll().is_some());
        assert_eq!(script.pending(), 2, "the second End and the loss");
    }

    #[test]
    fn open_scripted_forwards_every_command_to_the_scripted_backend() {
        let (mut backend, script) = open_scripted();
        let cwd = PathBuf::from("/w");
        let session = sid("s1");
        let turn = TurnId::mint();
        let saved = SavedSession {
            backend: BackendKind::Codex,
            session: session.clone(),
            contract: Some(ContractStamp::new(1)),
        };

        backend.ensure(&cwd).unwrap();
        assert_eq!(backend.root(), Some(cwd.as_path()));
        backend.restore(&cwd, &saved).unwrap();
        assert_eq!(backend.continuity(&saved), Continuity::Rejoin);
        backend.prompt(&session, turn, prompt("hi")).unwrap();
        backend.stop(&session, turn).unwrap();
        backend.close(&session).unwrap();
        backend.default_model(QueryId(1), &cwd).unwrap();
        backend.list_models(QueryId(2)).unwrap();
        backend
            .set_model(
                QueryId(3),
                &session,
                ModelSelection {
                    model: "m".into(),
                    effort: None,
                },
            )
            .unwrap();
        backend.preview_session(QueryId(4), &cwd, &session).unwrap();
        backend
            .inline_assist()
            .unwrap()
            .start("r".into(), &cwd, "p".into(), "c".into())
            .unwrap();
        backend
            .inline_assist()
            .unwrap()
            .followup("r".into(), &session, "p".into(), "c".into())
            .unwrap();
        backend
            .commit_messages()
            .unwrap()
            .generate(QueryId(5), &cwd, "diff".into())
            .unwrap();
        assert!(backend.detach_for_exit().is_none());
        backend.shutdown();

        let calls = script.calls();
        assert_eq!(calls.len(), 14, "{calls:?}");
        assert!(matches!(calls[2], Call::Prompt { .. }));
        assert!(matches!(calls[3], Call::Stop { .. }), "the turn was open");
        assert_eq!(calls.last(), Some(&Call::Shutdown));
    }

    #[test]
    fn open_scripted_withholds_capabilities_on_request() {
        let (mut backend, script) = open_scripted();
        assert!(backend.inline_assist().is_ok());
        script.withhold_capabilities();
        assert!(backend.inline_assist().is_err());
        assert!(backend.commit_messages().is_err());
    }

    #[test]
    fn open_scripted_runs_the_loss_path_for_a_failed_capability_call() {
        let (mut backend, script) = open_scripted();
        let cwd = PathBuf::from("/w");
        backend.default_model(QueryId(1), &cwd).unwrap();
        script.fail_next(AgentError::Gone("gone".into()));
        let result = backend
            .commit_messages()
            .unwrap()
            .generate(QueryId(2), &cwd, "diff".into());
        assert!(matches!(result, Err(AgentError::Gone(_))));
        assert_eq!(script.pending(), 2, "the open query's reply and the loss");

        let events = drain(backend.as_mut());
        assert!(matches!(
            events.as_slice(),
            [
                Event::Reply {
                    id: QueryId(1),
                    result: Err(_),
                    ..
                },
                Event::Lost(_)
            ]
        ));
    }

    #[test]
    fn discipline_accepts_interleaved_turns_that_each_end_once() {
        let (one, two) = (sid("s1"), sid("s2"));
        let (first, second) = (TurnId::mint(), TurnId::mint());
        assert_turn_discipline(&[
            turn_event(&one, first, TurnEventKind::StopRequested),
            turn_event(&one, first, TurnEventKind::Accepted),
            turn_event(&two, second, TurnEventKind::Accepted),
            turn_event(&one, first, TurnEventKind::Delta("a".into())),
            turn_event(
                &two,
                second,
                TurnEventKind::StopFailed {
                    message: "no".into(),
                },
            ),
            turn_event(&two, second, end()),
            turn_event(&one, first, TurnEventKind::End(TurnEnd::Stopped)),
            Event::Lost(Lost {
                scope: LostScope::Backend,
                message: "after".into(),
                had_sessions: false,
            }),
        ]);
        assert_turn_discipline(&[]);
    }

    #[test]
    #[should_panic(expected = "after its End")]
    fn discipline_rejects_a_second_end() {
        let (session, turn) = (sid("s1"), TurnId::mint());
        assert_turn_discipline(&[
            turn_event(&session, turn, end()),
            turn_event(&session, turn, end()),
        ]);
    }

    #[test]
    #[should_panic(expected = "after its End")]
    fn discipline_rejects_content_after_end() {
        let (session, turn) = (sid("s1"), TurnId::mint());
        assert_turn_discipline(&[
            turn_event(&session, turn, end()),
            turn_event(&session, turn, TurnEventKind::Delta("late".into())),
        ]);
    }

    #[test]
    #[should_panic(expected = "accepted twice")]
    fn discipline_rejects_a_second_accepted() {
        let (session, turn) = (sid("s1"), TurnId::mint());
        assert_turn_discipline(&[
            turn_event(&session, turn, TurnEventKind::Accepted),
            turn_event(&session, turn, TurnEventKind::Accepted),
            turn_event(&session, turn, end()),
        ]);
    }

    #[test]
    #[should_panic(expected = "accepted after content")]
    fn discipline_rejects_accepted_after_content() {
        let (session, turn) = (sid("s1"), TurnId::mint());
        assert_turn_discipline(&[
            turn_event(&session, turn, TurnEventKind::MessageDone("a".into())),
            turn_event(&session, turn, TurnEventKind::Accepted),
            turn_event(&session, turn, end()),
        ]);
    }

    #[test]
    #[should_panic(expected = "never ended")]
    fn discipline_rejects_a_turn_that_never_ends() {
        let (session, turn) = (sid("s1"), TurnId::mint());
        assert_turn_discipline(&[turn_event(&session, turn, TurnEventKind::Accepted)]);
    }
}
