//! Decorator that makes the turn, query and loss rules true for every backend.

use std::{collections::VecDeque, future::Future, path::Path, pin::Pin};

use super::{
    backend::{AgentBackend, AgentError, CommitMessages, InlineAssist, Missing},
    events::{
        Delivery, Event, Failure, FailureCause, Lost, LostScope, QueryKind, TurnEnd, TurnEvent,
        TurnEventKind,
    },
    ids::{BackendKind, Continuity, ContractStamp, QueryId, SavedSession, SessionId, TurnId},
    types::{ModelSelection, Prompt, SessionQuery},
};

/// Enforces the protocol rules at the read boundary, so they hold whichever
/// backend runs. Per-actor state stays inside each backend; the ledger only
/// merges what it sees.
///
/// It tracks, for the editor's benefit:
/// - open turns: inserted when `prompt` succeeds, removed on `End`, so even a
///   prompt that died in the command queue gets a closing event;
/// - open queries: inserted by each query call, removed on `Reply`;
/// - what the current worker was started for, which becomes
///   `Lost.had_sessions`: false only when a model or session query started it
///   and no conversation was created or restored since. Inline assist and
///   commit-message generation count as conversations, but a query-started
///   worker that later serves them stays query-only, as the editor has always
///   treated it.
///
/// `poll` applies these rules:
/// 1. A turn event for a turn that is not open is late or bogus and is dropped.
/// 2. `Accepted` marks the turn's delivery. `End` closes the turn, so a second
///    `End` falls under rule 1. An `End(Failed)` takes its delivery from what
///    the ledger saw, not from what the backend claimed.
/// 3. A reply for a query that is not open is dropped.
/// 4. An inner `Lost` becomes, in submission order, an `End(Failed)` for each
///    open turn in scope, a `Reply(Err)` for each open query when the whole
///    backend was lost, and then the `Lost`. Until a command succeeds again,
///    stray session and inline events are dropped and a second `Lost` is
///    ignored.
///
/// A command that fails with `Gone` is treated as that loss too: the events
/// are queued immediately and the error is still returned. The editor runs its
/// loss path only when it reads `Lost`.
///
/// `stop` for a turn that is not open is not forwarded. `close` marks the
/// session's open turns closing: exactly one `StopRequested` is let through
/// for each, then the turn is dropped without an `End`, because the editor has
/// already abandoned it. A later `Lost` does not fail closed turns.
///
/// Not enforced here: a live backend that forgets to end a turn.
pub struct Ledger<B: AgentBackend> {
    inner: B,
    turns: Vec<OpenTurn>,
    queries: Vec<(QueryId, QueryKind)>,
    worker_use: WorkerUse,
    lost: bool,
    backlog: VecDeque<Event>,
}

struct OpenTurn {
    session: SessionId,
    turn: TurnId,
    delivery: Delivery,
    phase: Phase,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Running,
    /// The editor closed the session. Only the stop it implies is reported.
    Closing,
}

/// What the current worker is known to have been used for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkerUse {
    /// No worker has served a command since the ledger was built, lost its
    /// worker, or was shut down.
    Idle,
    /// A model or session query started the worker, and no conversation was
    /// created or restored since.
    QueriesOnly,
    /// Something other than a plain query started the worker, or a
    /// conversation was created or restored on it.
    Conversations,
}

impl<B: AgentBackend> Ledger<B> {
    /// Wraps `inner`.
    pub fn new(inner: B) -> Self {
        Self {
            inner,
            turns: Vec::new(),
            queries: Vec::new(),
            worker_use: WorkerUse::Idle,
            lost: false,
            backlog: VecDeque::new(),
        }
    }

    /// Opens a turn the way a successful `prompt` does.
    pub(super) fn open_turn(&mut self, session: SessionId, turn: TurnId) {
        if self.turns.iter().all(|open| open.turn != turn) {
            self.turns.push(OpenTurn {
                session,
                turn,
                delivery: Delivery::Unconfirmed,
                phase: Phase::Running,
            });
        }
    }

    /// Notes that a conversation was created or restored on the worker, which
    /// stops it being query-only.
    pub(super) fn note_session(&mut self) {
        self.worker_use = WorkerUse::Conversations;
    }

    /// Notes what a command that succeeded used the worker for. Only the
    /// first one counts: it is what started the worker.
    fn note_use(&mut self, wanted: WorkerUse) {
        if self.worker_use == WorkerUse::Idle {
            self.worker_use = wanted;
        }
    }

    /// How many events `poll` can return without asking the backend.
    #[cfg(test)]
    pub(super) fn queued(&self) -> usize {
        self.backlog.len()
    }

    /// Applies the loss rule to a command result.
    fn settle<T>(&mut self, result: Result<T, AgentError>) -> Result<T, AgentError> {
        match &result {
            Ok(_) => self.lost = false,
            Err(AgentError::Gone(message)) if !self.lost => {
                self.lose(LostScope::Backend, message.clone());
            }
            Err(_) => {}
        }
        result
    }

    /// Settles a query command and opens the query when it was accepted.
    fn settle_query(
        &mut self,
        id: QueryId,
        kind: QueryKind,
        result: Result<(), AgentError>,
    ) -> Result<(), AgentError> {
        self.settle(result)?;
        self.note_use(match kind {
            QueryKind::CommitMessage => WorkerUse::Conversations,
            QueryKind::DefaultModel
            | QueryKind::ListModels
            | QueryKind::SetModel
            | QueryKind::ListSessions
            | QueryKind::PreviewSession => WorkerUse::QueriesOnly,
        });
        self.queries.push((id, kind));
        Ok(())
    }

    /// Queues the closing events for a loss, then the loss itself.
    fn lose(&mut self, scope: LostScope, message: String) {
        let had_sessions = self.worker_use == WorkerUse::Conversations;
        let ended = match &scope {
            LostScope::Backend => std::mem::take(&mut self.turns),
            LostScope::Session(session) => {
                let (ended, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.turns)
                    .into_iter()
                    .partition(|open| open.session == *session);
                self.turns = kept;
                ended
            }
        };
        for open in ended
            .into_iter()
            .filter(|open| open.phase == Phase::Running)
        {
            self.backlog.push_back(Event::Turn(TurnEvent {
                session: open.session,
                turn: open.turn,
                kind: TurnEventKind::End(TurnEnd::Failed(Failure {
                    message: message.clone(),
                    cause: FailureCause::BackendLost,
                    delivery: open.delivery,
                })),
            }));
        }
        if scope == LostScope::Backend {
            for (id, kind) in std::mem::take(&mut self.queries) {
                self.backlog.push_back(Event::Reply {
                    id,
                    kind,
                    result: Err(message.clone()),
                });
            }
            self.worker_use = WorkerUse::Idle;
            self.lost = true;
        }
        self.backlog.push_back(Event::Lost(Lost {
            scope,
            message,
            had_sessions,
        }));
    }

    /// Applies rules 1 to 4 to one event from the backend.
    fn admit(&mut self, event: Event) {
        match event {
            Event::Turn(turn_event) => self.admit_turn(turn_event),
            Event::Reply { id, kind, result } => {
                match self.queries.iter().position(|(open, _)| *open == id) {
                    Some(index) => {
                        self.queries.remove(index);
                        self.backlog.push_back(Event::Reply { id, kind, result });
                    }
                    None => crate::log!("Dropped a reply to unknown query {id:?}"),
                }
            }
            Event::Lost(Lost { scope, message, .. }) => {
                if self.lost {
                    crate::log!("Dropped a second loss report: {message}");
                } else {
                    self.lose(scope, message);
                }
            }
            event @ (Event::Session(_) | Event::Inline(_)) => {
                if self.lost {
                    crate::log!("Dropped an event from a lost backend: {event:?}");
                } else {
                    self.backlog.push_back(event);
                }
            }
        }
    }

    fn admit_turn(&mut self, mut event: TurnEvent) {
        let Some(index) = self
            .turns
            .iter()
            .position(|open| open.turn == event.turn && open.session == event.session)
        else {
            crate::log!("Dropped an event for a turn that is not open: {event:?}");
            return;
        };
        let open = &mut self.turns[index];
        if open.phase == Phase::Closing {
            match event.kind {
                TurnEventKind::StopRequested => {
                    self.turns.remove(index);
                    self.backlog.push_back(Event::Turn(event));
                }
                TurnEventKind::End(_) => {
                    self.turns.remove(index);
                }
                _ => {}
            }
            return;
        }
        match &mut event.kind {
            TurnEventKind::Accepted => open.delivery = Delivery::Accepted,
            TurnEventKind::End(end) => {
                if let TurnEnd::Failed(failure) = end {
                    failure.delivery = open.delivery;
                }
                self.turns.remove(index);
            }
            _ => {}
        }
        self.backlog.push_back(Event::Turn(event));
    }

    fn inline(&mut self) -> Result<&mut dyn InlineAssist, AgentError> {
        Ok(self.inner.inline_assist()?)
    }

    fn commit(&mut self) -> Result<&mut dyn CommitMessages, AgentError> {
        Ok(self.inner.commit_messages()?)
    }
}

impl<B: AgentBackend> AgentBackend for Ledger<B> {
    fn kind(&self) -> BackendKind {
        self.inner.kind()
    }

    fn contract(&self) -> ContractStamp {
        self.inner.contract()
    }

    fn continuity(&self, saved: &SavedSession) -> Continuity {
        self.inner.continuity(saved)
    }

    fn is_running(&self) -> bool {
        !self.lost && self.inner.is_running()
    }

    fn ensure(&mut self, root: &Path) -> Result<(), AgentError> {
        let result = self.inner.ensure(root);
        self.settle(result)
    }

    fn root(&self) -> Option<&Path> {
        self.inner.root()
    }

    fn new_session(&mut self, cwd: &Path, model: Option<ModelSelection>) -> Result<(), AgentError> {
        let result = self.inner.new_session(cwd, model);
        self.settle(result)?;
        self.note_session();
        Ok(())
    }

    fn restore(&mut self, cwd: &Path, saved: &SavedSession) -> Result<(), AgentError> {
        let result = self.inner.restore(cwd, saved);
        self.settle(result)?;
        self.note_session();
        Ok(())
    }

    fn prompt(
        &mut self,
        session: &SessionId,
        turn: TurnId,
        prompt: Prompt,
    ) -> Result<(), AgentError> {
        let result = self.inner.prompt(session, turn, prompt);
        self.settle(result)?;
        self.open_turn(session.clone(), turn);
        Ok(())
    }

    fn stop(&mut self, session: &SessionId, turn: TurnId) -> Result<(), AgentError> {
        let running = self.turns.iter().any(|open| {
            open.turn == turn && open.session == *session && open.phase == Phase::Running
        });
        if !running {
            return Ok(());
        }
        let result = self.inner.stop(session, turn);
        self.settle(result)
    }

    fn close(&mut self, session: &SessionId) -> Result<(), AgentError> {
        for open in self
            .turns
            .iter_mut()
            .filter(|open| open.session == *session)
        {
            open.phase = Phase::Closing;
        }
        let result = self.inner.close(session);
        self.settle(result)
    }

    fn default_model(&mut self, id: QueryId, cwd: &Path) -> Result<(), AgentError> {
        let result = self.inner.default_model(id, cwd);
        self.settle_query(id, QueryKind::DefaultModel, result)
    }

    fn list_models(&mut self, id: QueryId) -> Result<(), AgentError> {
        let result = self.inner.list_models(id);
        self.settle_query(id, QueryKind::ListModels, result)
    }

    fn set_model(
        &mut self,
        id: QueryId,
        session: &SessionId,
        selection: ModelSelection,
    ) -> Result<(), AgentError> {
        let result = self.inner.set_model(id, session, selection);
        self.settle_query(id, QueryKind::SetModel, result)
    }

    fn list_sessions(&mut self, id: QueryId, query: SessionQuery) -> Result<(), AgentError> {
        let result = self.inner.list_sessions(id, query);
        self.settle_query(id, QueryKind::ListSessions, result)
    }

    fn preview_session(
        &mut self,
        id: QueryId,
        cwd: &Path,
        session: &SessionId,
    ) -> Result<(), AgentError> {
        let result = self.inner.preview_session(id, cwd, session);
        self.settle_query(id, QueryKind::PreviewSession, result)
    }

    fn poll(&mut self) -> Option<Event> {
        loop {
            if let Some(event) = self.backlog.pop_front() {
                return Some(event);
            }
            let event = self.inner.poll()?;
            self.admit(event);
        }
    }

    fn shutdown(&mut self) {
        self.inner.shutdown();
        self.turns.clear();
        self.queries.clear();
        self.backlog.clear();
        self.worker_use = WorkerUse::Idle;
        self.lost = false;
    }

    fn detach_for_exit(&mut self) -> Option<Pin<Box<dyn Future<Output = ()> + Send>>> {
        self.inner.detach_for_exit()
    }

    /// Probes the backend, then hands out the ledger itself so a `Gone` from
    /// the capability is treated as a loss like any other command's.
    fn inline_assist(&mut self) -> Result<&mut dyn InlineAssist, Missing> {
        self.inner.inline_assist()?;
        Ok(self)
    }

    /// Probes the backend, then hands out the ledger itself so `generate` can
    /// open its query.
    fn commit_messages(&mut self) -> Result<&mut dyn CommitMessages, Missing> {
        self.inner.commit_messages()?;
        Ok(self)
    }
}

impl<B: AgentBackend> InlineAssist for Ledger<B> {
    fn start(
        &mut self,
        request: String,
        cwd: &Path,
        prompt: String,
        context: String,
    ) -> Result<(), AgentError> {
        let result = self
            .inline()
            .and_then(|inline| inline.start(request, cwd, prompt, context));
        self.settle(result)?;
        self.note_use(WorkerUse::Conversations);
        Ok(())
    }

    fn followup(
        &mut self,
        request: String,
        session: &SessionId,
        prompt: String,
        context: String,
    ) -> Result<(), AgentError> {
        let result = self
            .inline()
            .and_then(|inline| inline.followup(request, session, prompt, context));
        self.settle(result)?;
        self.note_use(WorkerUse::Conversations);
        Ok(())
    }
}

impl<B: AgentBackend> CommitMessages for Ledger<B> {
    fn generate(&mut self, id: QueryId, cwd: &Path, prompt: String) -> Result<(), AgentError> {
        let result = self
            .commit()
            .and_then(|commit| commit.generate(id, cwd, prompt));
        self.settle_query(id, QueryKind::CommitMessage, result)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::agent_backend::{
        events::{Completion, ReplyBody, SessionEvent},
        testing::{assert_turn_discipline, Call, Script, ScriptedBackend},
    };

    fn ledger() -> (Ledger<ScriptedBackend>, Script) {
        let (backend, script) = ScriptedBackend::new();
        (Ledger::new(backend), script)
    }

    fn session(id: &str) -> SessionId {
        SessionId::from_raw(id.to_string())
    }

    fn prompt(text: &str) -> Prompt {
        Prompt {
            text: text.to_string(),
            context: None,
            recap: None,
        }
    }

    fn start_turn(ledger: &mut Ledger<ScriptedBackend>, session: &SessionId) -> TurnId {
        let turn = TurnId::mint();
        ledger.prompt(session, turn, prompt("hello")).unwrap();
        turn
    }

    fn drain(ledger: &mut Ledger<ScriptedBackend>) -> Vec<Event> {
        std::iter::from_fn(|| ledger.poll()).collect()
    }

    fn turn_event(session: &SessionId, turn: TurnId, kind: TurnEventKind) -> Event {
        Event::Turn(TurnEvent {
            session: session.clone(),
            turn,
            kind,
        })
    }

    fn completed() -> TurnEventKind {
        TurnEventKind::End(TurnEnd::Completed(Completion::EndTurn))
    }

    fn failed(message: &str, cause: FailureCause, delivery: Delivery) -> TurnEventKind {
        TurnEventKind::End(TurnEnd::Failed(Failure {
            message: message.to_string(),
            cause,
            delivery,
        }))
    }

    fn lost(message: &str, had_sessions: bool) -> Event {
        Event::Lost(Lost {
            scope: LostScope::Backend,
            message: message.to_string(),
            had_sessions,
        })
    }

    fn query_failed(id: i64, kind: QueryKind, message: &str) -> Event {
        Event::Reply {
            id: QueryId(id),
            kind,
            result: Err(message.to_string()),
        }
    }

    fn catalog_reply(id: i64) -> Event {
        Event::Reply {
            id: QueryId(id),
            kind: QueryKind::ListModels,
            result: Ok(ReplyBody::Model(
                crate::agent_backend::types::ModelResult::Catalog(Vec::new()),
            )),
        }
    }

    fn gone(message: &str) -> AgentError {
        AgentError::Gone(message.to_string())
    }

    #[test]
    fn events_for_a_turn_that_ended_are_dropped() {
        let (mut ledger, script) = ledger();
        let session = session("s1");
        let turn = start_turn(&mut ledger, &session);
        script.emit_turn(&session, turn, TurnEventKind::Delta("a".into()));
        script.emit_turn(&session, turn, completed());
        script.emit_turn(&session, turn, TurnEventKind::Delta("late".into()));
        script.emit_turn(&session, turn, TurnEventKind::MessageDone("late".into()));
        script.emit_turn(&session, turn, TurnEventKind::StopRequested);

        assert_eq!(
            drain(&mut ledger),
            vec![
                turn_event(&session, turn, TurnEventKind::Delta("a".into())),
                turn_event(&session, turn, completed()),
            ]
        );
    }

    #[test]
    fn a_second_end_is_dropped() {
        let (mut ledger, script) = ledger();
        let session = session("s1");
        let turn = start_turn(&mut ledger, &session);
        script.emit_turn(&session, turn, completed());
        script.emit_turn(&session, turn, TurnEventKind::End(TurnEnd::Stopped));

        let events = drain(&mut ledger);
        assert_eq!(events, vec![turn_event(&session, turn, completed())]);
        assert_turn_discipline(&events);
    }

    #[test]
    fn events_for_turns_that_were_never_opened_are_dropped() {
        let (mut ledger, script) = ledger();
        let one = session("s1");
        let other = session("s2");
        let turn = start_turn(&mut ledger, &one);
        script.emit_turn(&one, TurnId::mint(), TurnEventKind::Delta("stray".into()));
        script.emit_turn(&other, turn, TurnEventKind::Delta("wrong session".into()));
        script.emit_turn(&one, turn, TurnEventKind::Delta("ok".into()));

        assert_eq!(
            drain(&mut ledger),
            vec![turn_event(&one, turn, TurnEventKind::Delta("ok".into()))]
        );
    }

    #[test]
    fn a_prompt_that_fails_does_not_open_a_turn() {
        let (mut ledger, script) = ledger();
        let session = session("s1");
        let turn = TurnId::mint();
        script.fail_next(AgentError::Unavailable("no".into()));
        assert!(ledger.prompt(&session, turn, prompt("x")).is_err());
        script.emit_turn(&session, turn, TurnEventKind::Delta("a".into()));

        assert_eq!(drain(&mut ledger), Vec::new());
    }

    #[test]
    fn loss_ends_every_open_turn_and_query_before_it_reports_the_loss() {
        let (mut ledger, script) = ledger();
        let (one, two) = (session("s1"), session("s2"));
        ledger.new_session(&PathBuf::from("/w"), None).unwrap();
        let accepted = start_turn(&mut ledger, &one);
        let unconfirmed = start_turn(&mut ledger, &two);
        ledger.list_models(QueryId(1)).unwrap();
        ledger
            .default_model(QueryId(2), &PathBuf::from("/w"))
            .unwrap();
        script.emit_turn(&one, accepted, TurnEventKind::Accepted);
        script.lose("boom");

        let events = drain(&mut ledger);
        assert_eq!(
            events,
            vec![
                turn_event(&one, accepted, TurnEventKind::Accepted),
                turn_event(
                    &one,
                    accepted,
                    failed("boom", FailureCause::BackendLost, Delivery::Accepted)
                ),
                turn_event(
                    &two,
                    unconfirmed,
                    failed("boom", FailureCause::BackendLost, Delivery::Unconfirmed)
                ),
                query_failed(1, QueryKind::ListModels, "boom"),
                query_failed(2, QueryKind::DefaultModel, "boom"),
                lost("boom", true),
            ]
        );
        assert_turn_discipline(&events);
        assert!(!ledger.is_running());
        assert_eq!(drain(&mut ledger), Vec::new());
    }

    #[test]
    fn a_failed_turn_reports_the_delivery_the_ledger_saw() {
        let (mut ledger, script) = ledger();
        let session = session("s1");
        let unseen = start_turn(&mut ledger, &session);
        let seen = start_turn(&mut ledger, &session);
        script.emit_turn(
            &session,
            unseen,
            failed("agent", FailureCause::Agent, Delivery::Accepted),
        );
        script.emit_turn(&session, seen, TurnEventKind::Accepted);
        script.emit_turn(
            &session,
            seen,
            failed("agent", FailureCause::Agent, Delivery::Unconfirmed),
        );

        assert_eq!(
            drain(&mut ledger),
            vec![
                turn_event(
                    &session,
                    unseen,
                    failed("agent", FailureCause::Agent, Delivery::Unconfirmed)
                ),
                turn_event(&session, seen, TurnEventKind::Accepted),
                turn_event(
                    &session,
                    seen,
                    failed("agent", FailureCause::Agent, Delivery::Accepted)
                ),
            ]
        );
    }

    #[test]
    fn a_stop_for_an_ended_or_unknown_turn_is_not_forwarded() {
        let (mut ledger, script) = ledger();
        let session = session("s1");
        let ended = start_turn(&mut ledger, &session);
        let running = start_turn(&mut ledger, &session);
        script.emit_turn(&session, ended, completed());
        drain(&mut ledger);
        script.take_calls();

        ledger.stop(&session, ended).unwrap();
        ledger.stop(&session, TurnId::mint()).unwrap();
        assert_eq!(script.calls(), Vec::new());

        ledger.stop(&session, running).unwrap();
        assert_eq!(
            script.calls(),
            vec![Call::Stop {
                session,
                turn: running
            }]
        );
    }

    #[test]
    fn closing_a_session_mid_turn_reports_one_stop_and_never_an_end() {
        let (mut ledger, script) = ledger();
        let session = session("s1");
        let turn = start_turn(&mut ledger, &session);
        script.take_calls();

        ledger.close(&session).unwrap();
        ledger.stop(&session, turn).unwrap();
        assert_eq!(script.calls(), vec![Call::Close(session.clone())]);

        script.emit_turn(&session, turn, TurnEventKind::StopRequested);
        script.emit_turn(&session, turn, TurnEventKind::StopRequested);
        script.emit_turn(&session, turn, TurnEventKind::Delta("late".into()));
        script.emit_turn(&session, turn, TurnEventKind::End(TurnEnd::Stopped));
        assert_eq!(
            drain(&mut ledger),
            vec![turn_event(&session, turn, TurnEventKind::StopRequested)]
        );
    }

    #[test]
    fn a_loss_after_a_close_does_not_fail_the_closed_turn() {
        let (mut ledger, script) = ledger();
        let (closed, open) = (session("s1"), session("s2"));
        let closed_turn = start_turn(&mut ledger, &closed);
        let open_turn = start_turn(&mut ledger, &open);
        ledger.close(&closed).unwrap();
        script.emit_turn(&closed, closed_turn, TurnEventKind::StopRequested);
        script.lose("boom");

        assert_eq!(
            drain(&mut ledger),
            vec![
                turn_event(&closed, closed_turn, TurnEventKind::StopRequested),
                turn_event(
                    &open,
                    open_turn,
                    failed("boom", FailureCause::BackendLost, Delivery::Unconfirmed)
                ),
                lost("boom", false),
            ]
        );

        let (mut ledger, script) = ledger_with_closed_turn_that_never_stopped();
        script.lose("boom");
        assert_eq!(drain(&mut ledger), vec![lost("boom", false)]);
    }

    fn ledger_with_closed_turn_that_never_stopped() -> (Ledger<ScriptedBackend>, Script) {
        let (mut ledger, script) = ledger();
        let session = session("s1");
        start_turn(&mut ledger, &session);
        ledger.close(&session).unwrap();
        (ledger, script)
    }

    #[test]
    fn a_worker_that_only_served_queries_reports_no_sessions_lost() {
        let (mut ledger, script) = ledger();
        let cwd = PathBuf::from("/w");
        ledger.list_models(QueryId(1)).unwrap();
        ledger.default_model(QueryId(2), &cwd).unwrap();
        ledger
            .set_model(
                QueryId(3),
                &session("s1"),
                ModelSelection {
                    model: "m".into(),
                    effort: None,
                },
            )
            .unwrap();
        ledger
            .list_sessions(
                QueryId(4),
                SessionQuery {
                    cwd: cwd.clone(),
                    scope: crate::agent_backend::types::SessionScope::All,
                    query: String::new(),
                    exclude_session_id: None,
                    search_contents: false,
                },
            )
            .unwrap();
        ledger
            .preview_session(QueryId(5), &cwd, &session("s1"))
            .unwrap();
        script.lose("boom");
        let events = drain(&mut ledger);
        assert_eq!(events.last(), Some(&lost("boom", false)));
        assert_eq!(events.len(), 6);
    }

    #[test]
    fn a_worker_started_by_an_inline_start_is_not_query_only() {
        let (mut ledger, script) = ledger();
        ledger
            .inline_assist()
            .expect("offered")
            .start("r".into(), &PathBuf::from("/w"), "p".into(), "c".into())
            .unwrap();
        script.lose("boom");
        assert_eq!(drain(&mut ledger), vec![lost("boom", true)]);
    }

    #[test]
    fn a_worker_started_by_an_inline_followup_is_not_query_only() {
        let (mut ledger, script) = ledger();
        ledger
            .inline_assist()
            .expect("offered")
            .followup("r".into(), &session("s1"), "p".into(), "c".into())
            .unwrap();
        script.lose("boom");
        assert_eq!(drain(&mut ledger), vec![lost("boom", true)]);
    }

    #[test]
    fn a_worker_started_by_a_commit_message_is_not_query_only() {
        let (mut ledger, script) = ledger();
        ledger
            .commit_messages()
            .expect("offered")
            .generate(QueryId(1), &PathBuf::from("/w"), "p".into())
            .unwrap();
        script.lose("boom");
        assert_eq!(
            drain(&mut ledger),
            vec![
                query_failed(1, QueryKind::CommitMessage, "boom"),
                lost("boom", true)
            ]
        );
    }

    #[test]
    fn inline_assist_and_commit_messages_do_not_end_a_query_started_workers_query_only_status() {
        let cwd = PathBuf::from("/w");
        let (mut ledger, script) = ledger();
        ledger.list_models(QueryId(1)).unwrap();
        ledger
            .inline_assist()
            .expect("offered")
            .start("r".into(), &cwd, "p".into(), "c".into())
            .unwrap();
        ledger
            .commit_messages()
            .expect("offered")
            .generate(QueryId(2), &cwd, "p".into())
            .unwrap();
        ledger.list_models(QueryId(3)).unwrap();
        script.lose("boom");
        assert_eq!(drain(&mut ledger).last(), Some(&lost("boom", false)));
    }

    #[test]
    fn a_new_session_ends_a_query_started_workers_query_only_status() {
        let (mut ledger, script) = ledger();
        ledger.list_models(QueryId(1)).unwrap();
        ledger.new_session(&PathBuf::from("/w"), None).unwrap();
        script.lose("boom");
        assert_eq!(drain(&mut ledger).last(), Some(&lost("boom", true)));
    }

    #[test]
    fn a_restore_ends_a_query_started_workers_query_only_status() {
        let (mut ledger, script) = ledger();
        let saved = SavedSession {
            backend: BackendKind::Codex,
            session: session("old"),
            contract: None,
        };
        ledger.list_models(QueryId(1)).unwrap();
        ledger.restore(&PathBuf::from("/w"), &saved).unwrap();
        script.lose("boom");
        assert_eq!(drain(&mut ledger).last(), Some(&lost("boom", true)));
    }

    #[test]
    fn a_query_to_a_worker_with_conversations_does_not_make_it_query_only() {
        let (mut ledger, script) = ledger();
        ledger.new_session(&PathBuf::from("/w"), None).unwrap();
        ledger.list_models(QueryId(1)).unwrap();
        script.lose("boom");
        assert_eq!(drain(&mut ledger).last(), Some(&lost("boom", true)));
    }

    #[test]
    fn closing_a_session_does_not_undo_that_one_existed() {
        let (mut ledger, script) = ledger();
        ledger.new_session(&PathBuf::from("/w"), None).unwrap();
        ledger.close(&session("s1")).unwrap();
        script.lose("boom");
        assert_eq!(drain(&mut ledger), vec![lost("boom", true)]);
    }

    #[test]
    fn restoring_counts_and_the_backends_own_claim_is_ignored() {
        let (mut ledger, script) = ledger();
        let saved = SavedSession {
            backend: BackendKind::Codex,
            session: session("old"),
            contract: None,
        };
        ledger.restore(&PathBuf::from("/w"), &saved).unwrap();
        script.emit(lost("boom", false));
        assert_eq!(drain(&mut ledger), vec![lost("boom", true)]);

        script.emit(lost("again", true));
        assert_eq!(drain(&mut ledger), Vec::new(), "a second loss is ignored");
    }

    #[test]
    fn the_next_worker_starts_without_sessions() {
        let (mut ledger, script) = ledger();
        ledger.new_session(&PathBuf::from("/w"), None).unwrap();
        script.lose("first");
        assert_eq!(drain(&mut ledger), vec![lost("first", true)]);

        ledger.list_models(QueryId(2)).unwrap();
        script.lose("second");
        assert_eq!(
            drain(&mut ledger),
            vec![
                query_failed(2, QueryKind::ListModels, "second"),
                lost("second", false)
            ]
        );
    }

    #[test]
    fn a_session_command_that_failed_created_nothing() {
        let (mut ledger, script) = ledger();
        script.fail_next(AgentError::Unavailable("no".into()));
        assert!(ledger.new_session(&PathBuf::from("/w"), None).is_err());
        script.lose("boom");
        assert_eq!(drain(&mut ledger), vec![lost("boom", false)]);
    }

    #[test]
    fn a_gone_command_queues_the_loss_once_and_still_fails() {
        let (mut ledger, script) = ledger();
        let (one, two) = (session("s1"), session("s2"));
        ledger.new_session(&PathBuf::from("/w"), None).unwrap();
        let turn = start_turn(&mut ledger, &two);
        ledger.list_models(QueryId(7)).unwrap();

        script.fail_next(gone("the agent session is not running"));
        let failed_turn = TurnId::mint();
        let error = ledger.prompt(&one, failed_turn, prompt("x")).unwrap_err();
        assert!(matches!(&error, AgentError::Gone(m) if m == "the agent session is not running"));
        assert!(!ledger.is_running());

        script.fail_next(gone("still gone"));
        assert!(matches!(
            ledger.list_models(QueryId(8)),
            Err(AgentError::Gone(_))
        ));
        script.lose("the worker's own report");

        let events = drain(&mut ledger);
        assert_eq!(
            events,
            vec![
                turn_event(
                    &two,
                    turn,
                    failed(
                        "the agent session is not running",
                        FailureCause::BackendLost,
                        Delivery::Unconfirmed
                    )
                ),
                query_failed(7, QueryKind::ListModels, "the agent session is not running"),
                lost("the agent session is not running", true),
            ]
        );
        assert_turn_discipline(&events);
    }

    #[test]
    fn events_from_a_lost_backend_are_dropped_until_a_command_succeeds() {
        let (mut ledger, script) = ledger();
        script.lose("boom");
        assert_eq!(drain(&mut ledger), vec![lost("boom", false)]);

        let created = Event::Session(SessionEvent::Created {
            session: session("late"),
        });
        script.emit(created.clone());
        assert_eq!(drain(&mut ledger), Vec::new());

        ledger.new_session(&PathBuf::from("/w"), None).unwrap();
        assert!(ledger.is_running());
        script.emit(created.clone());
        assert_eq!(drain(&mut ledger), vec![created]);

        script.lose("again");
        assert_eq!(drain(&mut ledger), vec![lost("again", true)]);
    }

    #[test]
    fn gone_from_a_capability_is_a_loss_too() {
        let (mut ledger, script) = ledger();
        let cwd = PathBuf::from("/w");
        script.fail_next(gone("gone"));
        let result =
            ledger
                .commit_messages()
                .expect("offered")
                .generate(QueryId(3), &cwd, "p".into());
        assert!(matches!(result, Err(AgentError::Gone(_))));
        assert_eq!(drain(&mut ledger), vec![lost("gone", false)]);

        script.fail_next(gone("gone"));
        ledger.new_session(&cwd, None).unwrap_err();
        script.take_calls();
        ledger.new_session(&cwd, None).unwrap();
        script.fail_next(gone("gone again"));
        let result = ledger.inline_assist().expect("offered").start(
            "r".into(),
            &cwd,
            "p".into(),
            "c".into(),
        );
        assert!(matches!(result, Err(AgentError::Gone(_))));
        assert_eq!(drain(&mut ledger), vec![lost("gone again", true)]);
    }

    #[test]
    fn a_reply_is_delivered_once_and_only_for_a_query_that_is_open() {
        let (mut ledger, script) = ledger();
        script.emit(catalog_reply(9));
        assert_eq!(drain(&mut ledger), Vec::new(), "unknown id");

        ledger.list_models(QueryId(1)).unwrap();
        script.emit(catalog_reply(1));
        script.emit(catalog_reply(1));
        assert_eq!(drain(&mut ledger), vec![catalog_reply(1)]);
    }

    #[test]
    fn commit_messages_open_a_query_that_loss_answers() {
        let (mut ledger, script) = ledger();
        let cwd = PathBuf::from("/w");
        ledger
            .commit_messages()
            .expect("offered")
            .generate(QueryId(4), &cwd, "prompt".into())
            .unwrap();
        assert_eq!(
            script.calls(),
            vec![Call::CommitMessage {
                id: QueryId(4),
                cwd,
                prompt: "prompt".into()
            }]
        );
        script.lose("boom");
        assert_eq!(
            drain(&mut ledger),
            vec![
                query_failed(4, QueryKind::CommitMessage, "boom"),
                lost("boom", true)
            ]
        );
    }

    #[test]
    fn a_capability_the_backend_lacks_is_missing() {
        let (mut ledger, script) = ledger();
        script.withhold_capabilities();

        let inline = ledger.inline_assist().err().expect("withheld");
        assert_eq!(
            inline.to_string(),
            "Inline assist requires Codex (active agent: Codex). Install Codex and run `codex login`."
        );
        let commit = ledger.commit_messages().err().expect("withheld");
        assert_eq!(commit.feature, "Commit message generation");
    }

    #[test]
    fn losing_one_session_leaves_the_rest_of_the_backend_alone() {
        let (mut ledger, script) = ledger();
        let (one, two) = (session("s1"), session("s2"));
        ledger.new_session(&PathBuf::from("/w"), None).unwrap();
        let lost_turn = start_turn(&mut ledger, &one);
        let kept_turn = start_turn(&mut ledger, &two);
        ledger.list_models(QueryId(1)).unwrap();
        script.lose_session(&one, "session died");

        assert_eq!(
            drain(&mut ledger),
            vec![
                turn_event(
                    &one,
                    lost_turn,
                    failed(
                        "session died",
                        FailureCause::BackendLost,
                        Delivery::Unconfirmed
                    )
                ),
                Event::Lost(Lost {
                    scope: LostScope::Session(one),
                    message: "session died".into(),
                    had_sessions: true,
                }),
            ]
        );
        assert!(ledger.is_running());

        script.emit_turn(&two, kept_turn, completed());
        script.emit(catalog_reply(1));
        assert_eq!(
            drain(&mut ledger),
            vec![turn_event(&two, kept_turn, completed()), catalog_reply(1)]
        );
    }

    #[test]
    fn shutdown_forgets_everything_without_events() {
        let (mut ledger, script) = ledger();
        let cwd = PathBuf::from("/w");
        let session = session("s1");
        ledger.new_session(&cwd, None).unwrap();
        let turn = start_turn(&mut ledger, &session);
        ledger.list_models(QueryId(1)).unwrap();
        script.fail_next(gone("gone"));
        ledger.list_models(QueryId(2)).unwrap_err();
        script.emit_turn(&session, turn, TurnEventKind::Delta("queued".into()));

        ledger.shutdown();
        assert_eq!(drain(&mut ledger), Vec::new());
        assert_eq!(script.calls().last(), Some(&Call::Shutdown));

        script.emit_turn(&session, turn, TurnEventKind::Delta("late".into()));
        script.emit(catalog_reply(1));
        script.lose("after shutdown");
        assert_eq!(drain(&mut ledger), vec![lost("after shutdown", false)]);
    }
}
