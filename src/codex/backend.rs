//! The Codex implementation of the agent backend interface.
//!
//! `CodexBackend` owns the Codex worker task and its channels. Commands become
//! `CodexCommand`s on an unbounded channel, so none of them waits. `poll`
//! turns the worker's `CodexEvent`s into `Event`s, so a `CodexEvent` never
//! leaves this file.
//!
//! How Codex's own events map onto the interface:
//!
//! | Codex                                  | Interface                                  |
//! | -------------------------------------- | ------------------------------------------ |
//! | `turn/start` response named the turn   | `Accepted`                                 |
//! | stop requested (also by `close`)       | `StopRequested`                            |
//! | `turn/interrupt` rejected              | `StopFailed`                               |
//! | turn status `completed`                | `End(Completed(EndTurn))`                  |
//! | turn status `interrupted`              | `End(Stopped)`                             |
//! | any other turn status                  | `End(Completed(Other(status)))`            |
//! | failure with a turn in flight          | `End(Failed)`                              |
//! | failure of a session with no turn      | `SessionEvent::Failed`                     |
//! | failure with no session                | `SessionEvent::CreateFailed`               |
//! | model, session and commit-message data | `Reply`                                    |
//! | the task ended                         | `Lost`, after every event it had sent      |

use std::{
    collections::HashMap,
    future::Future,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    pin::Pin,
};

use futures::FutureExt as _;
use path_absolutize::Absolutize as _;
use tokio::task::{JoinError, JoinHandle};

use super::{
    start_codex, AgentRuntimePolicy, CodexBridge, CodexCommand, CodexEvent, CodexProcessSpec,
    ToolHostAdapter,
};
use crate::{
    agent_backend::{
        backend::{AgentBackend, AgentError, CommitMessages, InlineAssist, Missing},
        events::{
            Completion, Delivery, Event, Failure, FailureCause, InlineEvent, Lost, LostScope,
            QueryKind, ReplyBody, SessionEvent, TurnEnd, TurnEvent, TurnEventKind,
        },
        ids::{BackendKind, Continuity, ContractStamp, QueryId, SavedSession, SessionId, TurnId},
        tool_host::ToolHost,
        types::{ModelQuery, ModelSelection, Prompt, SessionQuery},
    },
    agent_conversation::{AGENT_TOOL_CONTRACT_VERSION, RECAP_FOLLOW_UP, RECAP_PREFIX},
    config::AgentConfig,
};

/// Events the worker may have buffered before the editor reads them.
const AGENT_BRIDGE_CAPACITY: usize = 64;
const MAX_FAILURE_MESSAGE_CHARS: usize = 2048;
const NOT_RUNNING: &str = "No Codex app-server session is running";
const STOPPED_UNEXPECTEDLY: &str = "Codex app-server stopped unexpectedly";

type TaskResult = Result<anyhow::Result<()>, JoinError>;

/// How to launch the app-server, read once from the agent configuration.
struct Launch {
    command: Option<String>,
    args: Vec<String>,
    environment: HashMap<String, String>,
    policy: AgentRuntimePolicy,
}

/// A running app-server process and the channels to it.
struct Worker {
    bridge: CodexBridge,
    task: JoinHandle<anyhow::Result<()>>,
    /// The workspace the process was started in. One process serves one root.
    root: PathBuf,
}

/// The turn a session is running, which Codex's events do not name.
struct LiveTurn {
    turn: TurnId,
    /// The editor closed the session. The turn stays only to stamp the one
    /// stop the worker still reports.
    closing: bool,
}

/// Codex behind [`AgentBackend`], with inline assist and commit messages.
pub struct CodexBackend<H: ToolHost> {
    launch: Launch,
    host: H,
    /// `None` until a command needs a process, and again after the process is
    /// lost or shut down.
    worker: Option<Worker>,
    live: HashMap<SessionId, LiveTurn>,
    /// Which model query each request id is. A failed model query carries no
    /// result to tell them apart by.
    model_queries: HashMap<QueryId, QueryKind>,
}

impl<H: ToolHost> CodexBackend<H> {
    /// A backend that starts `config`'s Codex when a command first needs it.
    /// Nothing is started here. `host` serves the worker's tool calls and is
    /// cloned for each worker.
    pub fn new(config: &AgentConfig, host: H) -> Self {
        Self {
            launch: Launch {
                command: config.command.clone(),
                args: config.args.clone(),
                environment: config.env.clone(),
                policy: AgentRuntimePolicy::new(
                    config.allow_sensitive_paths,
                    config.enabled_mcp_servers.clone(),
                    config.enabled_codex_features.clone(),
                ),
            },
            host,
            worker: None,
            live: HashMap::new(),
            model_queries: HashMap::new(),
        }
    }

    /// A backend already talking to a worker the test drives by hand.
    #[cfg(test)]
    fn with_worker(
        config: &AgentConfig,
        host: H,
        bridge: CodexBridge,
        task: JoinHandle<anyhow::Result<()>>,
        root: &Path,
    ) -> Self {
        let mut backend = Self::new(config, host);
        backend.worker = Some(Worker {
            bridge,
            task,
            root: root.to_path_buf(),
        });
        backend
    }

    fn start(&mut self, root: PathBuf) -> Result<(), AgentError> {
        let configured = self.launch.command.as_deref().unwrap_or("codex");
        let command = crate::utils::find_executable(configured).ok_or_else(|| {
            AgentError::Unavailable(
                "Codex CLI was not found; install Codex, run `codex login`, and try again"
                    .to_string(),
            )
        })?;
        let mut spec = CodexProcessSpec::new(command, root.clone())
            .args(&self.launch.args)
            .policy(self.launch.policy.clone());
        spec.environment.extend(
            self.launch
                .environment
                .iter()
                .map(|(key, value)| (key.into(), value.into())),
        );
        let capacity =
            NonZeroUsize::new(AGENT_BRIDGE_CAPACITY).expect("agent bridge capacity is non-zero");
        let (bridge, task) = start_codex(spec, ToolHostAdapter(self.host.clone()), capacity)
            .map_err(|error| AgentError::Unavailable(error.to_string()))?;
        self.worker = Some(Worker { bridge, task, root });
        Ok(())
    }

    /// Starts a worker in `cwd` when none runs. An editor that wants the
    /// one-root rule checked calls `ensure` itself first.
    fn start_if_stopped(&mut self, cwd: &Path) -> Result<(), AgentError> {
        if self.worker.is_none() {
            self.ensure(cwd)?;
        }
        Ok(())
    }

    /// Queues `command`. A channel nobody reads from means the worker ended:
    /// it is dropped here and the error says why.
    fn send(&mut self, command: CodexCommand) -> Result<(), AgentError> {
        let worker = self
            .worker
            .as_ref()
            .ok_or_else(|| AgentError::Gone(NOT_RUNNING.to_string()))?;
        if worker.bridge.try_send(command).is_err() {
            return Err(AgentError::Gone(self.take_lost()));
        }
        Ok(())
    }

    /// Drops the worker that ended and everything tied to it, and describes
    /// how it ended. A task that has not finished yet counts as stopped
    /// unexpectedly; it is left to finish on its own.
    fn take_lost(&mut self) -> String {
        self.live.clear();
        self.model_queries.clear();
        let result = self
            .worker
            .take()
            .and_then(|worker| worker.task.now_or_never());
        lost_message(result)
    }

    /// Queues a model request and remembers what it asked for.
    fn model_query(
        &mut self,
        id: QueryId,
        kind: QueryKind,
        request: ModelQuery,
    ) -> Result<(), AgentError> {
        self.send(CodexCommand::ModelRequest {
            request_id: id.0,
            request,
        })?;
        self.model_queries.insert(id, kind);
        Ok(())
    }

    /// Stamps a turn event with the turn the session is running. Without one
    /// the event is late: its turn already ended and was reported.
    fn turn_event(&mut self, session: SessionId, kind: TurnEventKind) -> Option<Event> {
        let Some(live) = self.live.get(&session) else {
            crate::log!("Dropped a Codex event for session {session}, which has no turn in flight");
            return None;
        };
        let turn = live.turn;
        let finished = match kind {
            TurnEventKind::End(_) => true,
            TurnEventKind::StopRequested => live.closing,
            _ => false,
        };
        if finished {
            self.live.remove(&session);
        }
        Some(Event::Turn(TurnEvent {
            session,
            turn,
            kind,
        }))
    }

    /// The interface's view of one Codex event, or `None` when it has none.
    fn translate(&mut self, event: CodexEvent) -> Option<Event> {
        let session = SessionId::from_raw;
        Some(match event {
            CodexEvent::ModelRequestCompleted { request_id, result } => {
                let id = QueryId(request_id);
                let result = result.map(ReplyBody::Model);
                let known = self.model_queries.remove(&id);
                let Some(kind) = known.or_else(|| result.as_ref().ok().map(ReplyBody::kind)) else {
                    crate::log!("Dropped a failed Codex model reply to unknown query {id:?}");
                    return None;
                };
                Event::Reply { id, kind, result }
            }
            CodexEvent::SessionListCompleted { request_id, result } => Event::Reply {
                id: QueryId(request_id),
                kind: QueryKind::ListSessions,
                result: result.map(ReplyBody::Sessions),
            },
            CodexEvent::SessionPreviewCompleted { request_id, result } => Event::Reply {
                id: QueryId(request_id),
                kind: QueryKind::PreviewSession,
                result: result.map(ReplyBody::Preview),
            },
            CodexEvent::CommitMessageGenerated { request_id, result } => Event::Reply {
                id: QueryId(request_id),
                kind: QueryKind::CommitMessage,
                result: result.map(ReplyBody::CommitMessage),
            },
            CodexEvent::SessionModelRerouted { session_id, model } => {
                Event::Session(SessionEvent::ModelRerouted {
                    session: session(session_id),
                    model,
                })
            }
            CodexEvent::SessionModelChanged {
                session_id,
                model_info,
            } => Event::Session(SessionEvent::ModelChanged {
                session: session(session_id),
                model: model_info,
            }),
            CodexEvent::SessionCreated { session_id } => Event::Session(SessionEvent::Created {
                session: session(session_id),
            }),
            CodexEvent::SessionRestored { session_id, thread } => {
                Event::Session(SessionEvent::Restored {
                    session: session(session_id),
                    history: thread,
                })
            }
            CodexEvent::SessionRestoreFailed {
                session_id,
                message,
            } => Event::Session(SessionEvent::RestoreFailed {
                requested: session(session_id),
                message,
            }),
            CodexEvent::InlineContextRead {
                request_id,
                description,
            } => Event::Inline(InlineEvent::ContextRead {
                request: request_id,
                description,
            }),
            CodexEvent::InlineAnswerDelta { request_id, text } => {
                Event::Inline(InlineEvent::AnswerDelta {
                    request: request_id,
                    text,
                })
            }
            CodexEvent::InlineSessionCreated {
                request_id,
                session_id,
            } => Event::Inline(InlineEvent::SessionCreated {
                request: request_id,
                session: session(session_id),
            }),
            CodexEvent::InlineResult {
                request_id,
                session_id,
                result,
            } => Event::Inline(InlineEvent::Result {
                request: request_id,
                session: session(session_id),
                result,
            }),
            CodexEvent::InlineFailed {
                request_id,
                session_id,
                message,
            } => Event::Inline(InlineEvent::Failed {
                request: request_id,
                session: session_id.map(SessionId::from_raw),
                message,
            }),
            CodexEvent::TurnAccepted { session_id } => {
                return self.turn_event(session(session_id), TurnEventKind::Accepted);
            }
            CodexEvent::Update { session_id, text } => {
                return self.turn_event(session(session_id), TurnEventKind::Delta(text));
            }
            CodexEvent::MessageCompleted { session_id, text } => {
                return self.turn_event(session(session_id), TurnEventKind::MessageDone(text));
            }
            CodexEvent::Activity { session_id, update } => {
                return self.turn_event(session(session_id), TurnEventKind::Activity(update));
            }
            CodexEvent::Completed {
                session_id,
                stop_reason,
            } => {
                let end = match stop_reason.as_str() {
                    "completed" => TurnEnd::Completed(Completion::EndTurn),
                    "interrupted" => TurnEnd::Stopped,
                    _ => TurnEnd::Completed(Completion::Other(stop_reason)),
                };
                return self.turn_event(session(session_id), TurnEventKind::End(end));
            }
            CodexEvent::Cancelled { session_id } => {
                return self.turn_event(session(session_id), TurnEventKind::StopRequested);
            }
            CodexEvent::CancelFailed {
                session_id,
                message,
            } => {
                return self.turn_event(session(session_id), TurnEventKind::StopFailed { message });
            }
            CodexEvent::Failed {
                session_id: Some(session_id),
                message,
            } => {
                let failed = session(session_id);
                if !self.live.contains_key(&failed) {
                    return Some(Event::Session(SessionEvent::Failed {
                        session: failed,
                        message,
                    }));
                }
                return self.turn_event(
                    failed,
                    TurnEventKind::End(TurnEnd::Failed(Failure {
                        message,
                        cause: FailureCause::Agent,
                        delivery: Delivery::Unconfirmed,
                    })),
                );
            }
            CodexEvent::Failed {
                session_id: None,
                message,
            } => Event::Session(SessionEvent::CreateFailed { message }),
        })
    }
}

impl<H: ToolHost> AgentBackend for CodexBackend<H> {
    fn kind(&self) -> BackendKind {
        BackendKind::Codex
    }

    fn contract(&self) -> ContractStamp {
        ContractStamp::new(AGENT_TOOL_CONTRACT_VERSION)
    }

    fn continuity(&self, saved: &SavedSession) -> Continuity {
        match saved.backend {
            BackendKind::Codex if saved.contract == Some(self.contract()) => Continuity::Rejoin,
            BackendKind::Codex => Continuity::Fork,
        }
    }

    fn is_running(&self) -> bool {
        self.worker.is_some()
    }

    fn ensure(&mut self, root: &Path) -> Result<(), AgentError> {
        let root = root
            .absolutize()
            .map_err(|error| AgentError::Unavailable(error.to_string()))?;
        match &self.worker {
            Some(worker) if worker.root == *root => Ok(()),
            Some(worker) => Err(AgentError::Unavailable(format!(
                "Codex session root `{}` does not match the active agent workspace `{}`",
                root.display(),
                worker.root.display()
            ))),
            None => self.start(root.into_owned()),
        }
    }

    fn root(&self) -> Option<&Path> {
        self.worker.as_ref().map(|worker| worker.root.as_path())
    }

    fn new_session(&mut self, cwd: &Path, model: Option<ModelSelection>) -> Result<(), AgentError> {
        self.start_if_stopped(cwd)?;
        let cwd = cwd.to_path_buf();
        self.send(match model {
            Some(selection) => CodexCommand::NewSessionWithModel { cwd, selection },
            None => CodexCommand::NewSession { cwd },
        })
    }

    fn restore(&mut self, cwd: &Path, saved: &SavedSession) -> Result<(), AgentError> {
        let cwd_owned = cwd.to_path_buf();
        let session_id = saved.session.as_str().to_string();
        let command = match self.continuity(saved) {
            Continuity::Rejoin => CodexCommand::ResumeSession {
                cwd: cwd_owned,
                session_id,
            },
            Continuity::Fork => CodexCommand::ContinueSession {
                cwd: cwd_owned,
                session_id,
            },
            Continuity::Foreign => {
                return Err(AgentError::Unavailable(format!(
                    "This conversation belongs to another agent and {} cannot restore it",
                    self.kind().name()
                )));
            }
        };
        self.start_if_stopped(cwd)?;
        self.send(command)
    }

    fn prompt(
        &mut self,
        session: &SessionId,
        turn: TurnId,
        prompt: Prompt,
    ) -> Result<(), AgentError> {
        let session_id = session.as_str().to_string();
        let text = match prompt.recap {
            Some(recap) => format!("{RECAP_PREFIX}\n{recap}{RECAP_FOLLOW_UP}{}", prompt.text),
            None => prompt.text,
        };
        self.send(match prompt.context {
            Some(context) => CodexCommand::PromptWithContext {
                session_id,
                turn,
                text,
                uri: context.uri,
                context: context.text,
            },
            None => CodexCommand::Prompt {
                session_id,
                turn,
                text,
            },
        })?;
        self.live.insert(
            session.clone(),
            LiveTurn {
                turn,
                closing: false,
            },
        );
        Ok(())
    }

    fn stop(&mut self, session: &SessionId, turn: TurnId) -> Result<(), AgentError> {
        // The worker interrupts whatever the session runs, so a stop for a turn
        // that already ended must not reach it.
        let running = self
            .live
            .get(session)
            .is_some_and(|live| live.turn == turn && !live.closing);
        if !running {
            return Ok(());
        }
        self.send(CodexCommand::Cancel {
            session_id: session.as_str().to_string(),
        })
    }

    fn close(&mut self, session: &SessionId) -> Result<(), AgentError> {
        self.send(CodexCommand::CloseSession {
            session_id: session.as_str().to_string(),
        })?;
        if let Some(live) = self.live.get_mut(session) {
            live.closing = true;
        }
        Ok(())
    }

    fn default_model(&mut self, id: QueryId, cwd: &Path) -> Result<(), AgentError> {
        self.model_query(
            id,
            QueryKind::DefaultModel,
            ModelQuery::ReadDefault {
                cwd: cwd.to_path_buf(),
            },
        )
    }

    fn list_models(&mut self, id: QueryId) -> Result<(), AgentError> {
        self.model_query(id, QueryKind::ListModels, ModelQuery::List)
    }

    fn set_model(
        &mut self,
        id: QueryId,
        session: &SessionId,
        selection: ModelSelection,
    ) -> Result<(), AgentError> {
        self.model_query(
            id,
            QueryKind::SetModel,
            ModelQuery::Set {
                session_id: session.as_str().to_string(),
                selection,
            },
        )
    }

    fn list_sessions(&mut self, id: QueryId, query: SessionQuery) -> Result<(), AgentError> {
        self.send(CodexCommand::SessionList {
            request_id: id.0,
            request: query,
        })
    }

    fn preview_session(
        &mut self,
        id: QueryId,
        _cwd: &Path,
        session: &SessionId,
    ) -> Result<(), AgentError> {
        self.send(CodexCommand::SessionPreview {
            request_id: id.0,
            session_id: session.as_str().to_string(),
        })
    }

    fn poll(&mut self) -> Option<Event> {
        loop {
            let worker = self.worker.as_mut()?;
            let Some(event) = worker.bridge.try_recv() else {
                // Read `is_finished` first: once it is true nothing more can
                // arrive, so an empty buffer afterwards is final.
                if !worker.task.is_finished() || worker.bridge.has_pending_events() {
                    return None;
                }
                let message = self.take_lost();
                return Some(Event::Lost(Lost {
                    scope: LostScope::Backend,
                    message,
                    had_sessions: false,
                }));
            };
            if let Some(event) = self.translate(event) {
                return Some(event);
            }
        }
    }

    fn shutdown(&mut self) {
        self.live.clear();
        self.model_queries.clear();
        if let Some(worker) = self.worker.take() {
            drop(worker.bridge);
            worker.task.abort();
        }
    }

    fn detach_for_exit(&mut self) -> Option<Pin<Box<dyn Future<Output = ()> + Send>>> {
        self.live.clear();
        self.model_queries.clear();
        let Worker { bridge, task, .. } = self.worker.take()?;
        // Closing the command channel starts the worker's own shutdown.
        drop(bridge);
        Some(Box::pin(async move {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => crate::log!("Codex app-server shutdown failed: {error}"),
                Err(error) => crate::log!("Codex app-server task failed: {error}"),
            }
        }))
    }

    fn inline_assist(&mut self) -> Result<&mut dyn InlineAssist, Missing> {
        Ok(self)
    }

    fn commit_messages(&mut self) -> Result<&mut dyn CommitMessages, Missing> {
        Ok(self)
    }
}

impl<H: ToolHost> InlineAssist for CodexBackend<H> {
    fn start(
        &mut self,
        request: String,
        cwd: &Path,
        prompt: String,
        context: String,
    ) -> Result<(), AgentError> {
        self.start_if_stopped(cwd)?;
        self.send(CodexCommand::InlineAssist {
            request_id: request,
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
        self.send(CodexCommand::InlineAssistFollowup {
            request_id: request,
            session_id: session.as_str().to_string(),
            prompt,
            context,
        })
    }
}

impl<H: ToolHost> CommitMessages for CodexBackend<H> {
    fn generate(&mut self, id: QueryId, cwd: &Path, prompt: String) -> Result<(), AgentError> {
        self.start_if_stopped(cwd)?;
        self.send(CodexCommand::GenerateCommitMessage {
            request_id: id.0,
            cwd: cwd.to_path_buf(),
            prompt,
        })
    }
}

/// What the user is told when a worker's task ended, given its result when one
/// could be read.
fn lost_message(result: Option<TaskResult>) -> String {
    let message = match result {
        Some(Ok(Err(error))) => {
            let details = format!("{error:#}");
            crate::log!("Codex app-server task failed: {details}");
            format!("Codex app-server failed: {details}")
        }
        Some(Err(error)) if !error.is_cancelled() => {
            crate::log!("Codex app-server task failed: {error}");
            format!("Codex app-server task failed: {error}")
        }
        Some(Ok(Ok(()))) | Some(Err(_)) | None => STOPPED_UNEXPECTEDLY.to_string(),
    };
    bounded_failure_message(&message)
}

fn bounded_failure_message(message: &str) -> String {
    if message.chars().count() <= MAX_FAILURE_MESSAGE_CHARS {
        return message.to_string();
    }
    let mut bounded = message
        .chars()
        .take(MAX_FAILURE_MESSAGE_CHARS)
        .collect::<String>();
    bounded.push_str("… (full details are in the Red log)");
    bounded
}

/// What tests need to run the backend without an editor.
#[cfg(test)]
pub(super) mod test_support {
    use async_trait::async_trait;
    use serde_json::Value;

    use super::*;
    use crate::{agent_backend::tool_host::ToolCtx, agent_tools::EditorToolCall};

    /// A host for tests that never call a tool.
    #[derive(Clone)]
    pub(in crate::codex) struct NoHost;

    #[async_trait]
    impl ToolHost for NoHost {
        async fn read_file(
            &mut self,
            _: &ToolCtx,
            _: &str,
            _: usize,
            _: usize,
        ) -> anyhow::Result<Value> {
            anyhow::bail!("this test has no tools")
        }

        async fn write_file(
            &mut self,
            _: &ToolCtx,
            _: &str,
            _: u64,
            _: String,
        ) -> anyhow::Result<Value> {
            anyhow::bail!("this test has no tools")
        }

        async fn editor_tool(&mut self, _: &ToolCtx, _: EditorToolCall) -> anyhow::Result<Value> {
            anyhow::bail!("this test has no tools")
        }
    }

    /// The session a worker event belongs to when it is part of a turn.
    fn turn_session(event: &CodexEvent) -> Option<&str> {
        match event {
            CodexEvent::TurnAccepted { session_id }
            | CodexEvent::Update { session_id, .. }
            | CodexEvent::MessageCompleted { session_id, .. }
            | CodexEvent::Activity { session_id, .. }
            | CodexEvent::Completed { session_id, .. }
            | CodexEvent::Cancelled { session_id }
            | CodexEvent::CancelFailed { session_id, .. } => Some(session_id),
            _ => None,
        }
    }

    /// What the backend makes of `events` as the worker reports them. Every
    /// session counts as running a turn, so its turn events are stamped
    /// instead of dropped as late.
    pub(in crate::codex) fn translated(events: Vec<CodexEvent>) -> Vec<Event> {
        let mut backend = CodexBackend::new(&AgentConfig::default(), NoHost);
        events
            .into_iter()
            .filter_map(|event| {
                if let Some(session) = turn_session(&event) {
                    backend
                        .live
                        .entry(SessionId::from_raw(session.to_string()))
                        .or_insert_with(|| LiveTurn {
                            turn: TurnId::mint(),
                            closing: false,
                        });
                }
                backend.translate(event)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::{sync::oneshot, time::timeout};

    use super::test_support::NoHost;
    use super::*;
    use crate::{
        agent_backend::{
            ledger::Ledger,
            testing::assert_turn_discipline,
            types::{
                Activity, EditorContext, ModelInfo, ModelResult, RestoredHistory, SessionPage,
                SessionPreview, SessionScope,
            },
        },
        codex::CodexBridgeWorker,
        inline_assist::InlineAssistResult,
    };

    const ROOT: &str = "/workspace";

    type Ended = oneshot::Sender<anyhow::Result<()>>;

    /// A backend over a worker the test drives by hand. Sending on the third
    /// value ends the worker's task with that result.
    fn direct() -> (CodexBackend<NoHost>, CodexBridgeWorker, Ended) {
        let (bridge, worker) = CodexBridge::channel(NonZeroUsize::new(64).unwrap());
        let (ended, finish) = oneshot::channel();
        let task = tokio::spawn(async move { finish.await.unwrap_or(Ok(())) });
        let backend = CodexBackend::with_worker(
            &AgentConfig::default(),
            NoHost,
            bridge,
            task,
            Path::new(ROOT),
        );
        (backend, worker, ended)
    }

    /// Lets a finished task become visible to `JoinHandle::is_finished`.
    async fn settle() {
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    /// The backend inside the real ledger, as the editor holds it, with the
    /// worker's end of the channels in the test's hands.
    struct Rig {
        ledger: Ledger<CodexBackend<NoHost>>,
        worker: CodexBridgeWorker,
        ended: Option<Ended>,
    }

    impl Rig {
        fn new() -> Self {
            let (backend, worker, ended) = direct();
            Self {
                ledger: Ledger::new(backend),
                worker,
                ended: Some(ended),
            }
        }

        async fn emit(&self, event: CodexEvent) {
            self.worker.send(event).await.unwrap();
        }

        async fn command(&mut self) -> CodexCommand {
            timeout(Duration::from_secs(1), self.worker.recv())
                .await
                .expect("the backend sent a command")
                .expect("the command channel is open")
        }

        async fn assert_no_command(&mut self) {
            assert!(
                timeout(Duration::from_millis(20), self.worker.recv())
                    .await
                    .is_err(),
                "the backend sent a command nobody expected"
            );
        }

        fn events(&mut self) -> Vec<Event> {
            std::iter::from_fn(|| self.ledger.poll()).collect()
        }

        async fn end_task(&mut self, result: anyhow::Result<()>) {
            let ended = self.ended.take().expect("the task ends once");
            ended.send(result).ok();
            settle().await;
        }
    }

    fn sid(id: &str) -> SessionId {
        SessionId::from_raw(id.to_string())
    }

    fn say(text: &str) -> Prompt {
        Prompt {
            text: text.to_string(),
            context: None,
            recap: None,
        }
    }

    fn cwd() -> &'static Path {
        Path::new(ROOT)
    }

    fn on_turn(session: &SessionId, turn: TurnId, kind: TurnEventKind) -> Event {
        Event::Turn(TurnEvent {
            session: session.clone(),
            turn,
            kind,
        })
    }

    fn update(session: &str, text: &str) -> CodexEvent {
        CodexEvent::Update {
            session_id: session.to_string(),
            text: text.to_string(),
        }
    }

    fn finished(session: &str, status: &str) -> CodexEvent {
        CodexEvent::Completed {
            session_id: session.to_string(),
            stop_reason: status.to_string(),
        }
    }

    fn accepted(session: &str) -> CodexEvent {
        CodexEvent::TurnAccepted {
            session_id: session.to_string(),
        }
    }

    fn lost(message: &str, had_sessions: bool) -> Event {
        Event::Lost(Lost {
            scope: LostScope::Backend,
            message: message.to_string(),
            had_sessions,
        })
    }

    fn model(name: &str) -> ModelInfo {
        ModelInfo {
            model: name.to_string(),
            provider: None,
            effort: None,
        }
    }

    /// Session `s1` with a turn in flight, after the two commands that made
    /// them were read from the worker's side.
    async fn started_turn() -> (Rig, SessionId, TurnId) {
        let mut rig = Rig::new();
        rig.ledger.new_session(cwd(), None).unwrap();
        assert!(matches!(
            rig.command().await,
            CodexCommand::NewSession { cwd } if cwd == Path::new(ROOT)
        ));
        rig.emit(CodexEvent::SessionCreated {
            session_id: "s1".to_string(),
        })
        .await;
        let session = sid("s1");
        assert_eq!(
            rig.events(),
            [Event::Session(SessionEvent::Created {
                session: session.clone()
            })]
        );
        let turn = TurnId::mint();
        rig.ledger.prompt(&session, turn, say("hello")).unwrap();
        assert!(matches!(
            rig.command().await,
            CodexCommand::Prompt { session_id, turn: sent, text }
                if session_id == "s1" && sent == turn && text == "hello"
        ));
        (rig, session, turn)
    }

    #[tokio::test]
    async fn a_turn_streams_and_ends_exactly_once() {
        let (mut rig, session, turn) = started_turn().await;
        rig.emit(accepted("s1")).await;
        rig.emit(update("s1", "Hel")).await;
        rig.emit(update("s1", "lo")).await;
        rig.emit(CodexEvent::MessageCompleted {
            session_id: "s1".to_string(),
            text: "Hello".to_string(),
        })
        .await;
        rig.emit(CodexEvent::Activity {
            session_id: "s1".to_string(),
            update: Activity::Thought,
        })
        .await;
        rig.emit(finished("s1", "completed")).await;
        // Whatever follows the end belongs to no turn.
        rig.emit(update("s1", "late")).await;

        let events = rig.events();
        assert_eq!(
            events,
            [
                on_turn(&session, turn, TurnEventKind::Accepted),
                on_turn(&session, turn, TurnEventKind::Delta("Hel".to_string())),
                on_turn(&session, turn, TurnEventKind::Delta("lo".to_string())),
                on_turn(
                    &session,
                    turn,
                    TurnEventKind::MessageDone("Hello".to_string())
                ),
                on_turn(&session, turn, TurnEventKind::Activity(Activity::Thought)),
                on_turn(
                    &session,
                    turn,
                    TurnEventKind::End(TurnEnd::Completed(Completion::EndTurn))
                ),
            ]
        );
        assert_turn_discipline(&events);
    }

    #[tokio::test]
    async fn each_turn_status_maps_to_one_ending() {
        let (mut rig, session, first) = started_turn().await;
        let statuses = [
            ("completed", TurnEnd::Completed(Completion::EndTurn)),
            ("interrupted", TurnEnd::Stopped),
            (
                "failed",
                TurnEnd::Completed(Completion::Other("failed".to_string())),
            ),
        ];
        let mut turn = first;
        for (index, (status, end)) in statuses.into_iter().enumerate() {
            if index > 0 {
                turn = TurnId::mint();
                rig.ledger.prompt(&session, turn, say("again")).unwrap();
                rig.command().await;
            }
            rig.emit(finished("s1", status)).await;
            assert_eq!(
                rig.events(),
                [on_turn(&session, turn, TurnEventKind::End(end))],
                "status {status}"
            );
        }
    }

    #[tokio::test]
    async fn a_stop_reaches_the_worker_and_is_reported_before_the_turn_ends() {
        let (mut rig, session, turn) = started_turn().await;
        rig.emit(accepted("s1")).await;
        rig.ledger.stop(&session, turn).unwrap();
        assert!(matches!(
            rig.command().await,
            CodexCommand::Cancel { session_id } if session_id == "s1"
        ));
        rig.emit(CodexEvent::Cancelled {
            session_id: "s1".to_string(),
        })
        .await;
        rig.emit(finished("s1", "interrupted")).await;

        let events = rig.events();
        assert_eq!(
            events,
            [
                on_turn(&session, turn, TurnEventKind::Accepted),
                on_turn(&session, turn, TurnEventKind::StopRequested),
                on_turn(&session, turn, TurnEventKind::End(TurnEnd::Stopped)),
            ]
        );
        assert_turn_discipline(&events);
    }

    #[tokio::test]
    async fn a_cancel_after_the_turn_completed_is_dropped_and_a_late_stop_is_not_sent() {
        let (mut rig, session, turn) = started_turn().await;
        rig.emit(finished("s1", "completed")).await;
        rig.emit(CodexEvent::Cancelled {
            session_id: "s1".to_string(),
        })
        .await;

        let events = rig.events();
        assert_eq!(
            events,
            [on_turn(
                &session,
                turn,
                TurnEventKind::End(TurnEnd::Completed(Completion::EndTurn))
            )]
        );
        assert_turn_discipline(&events);
        rig.ledger.stop(&session, turn).unwrap();
        rig.assert_no_command().await;
    }

    #[tokio::test]
    async fn a_rejected_interrupt_leaves_the_turn_running_and_can_be_retried() {
        let (mut rig, session, turn) = started_turn().await;
        rig.ledger.stop(&session, turn).unwrap();
        assert!(matches!(rig.command().await, CodexCommand::Cancel { .. }));
        rig.emit(CodexEvent::Cancelled {
            session_id: "s1".to_string(),
        })
        .await;
        rig.emit(CodexEvent::CancelFailed {
            session_id: "s1".to_string(),
            message: "turn/interrupt was rejected".to_string(),
        })
        .await;
        rig.ledger.stop(&session, turn).unwrap();
        assert!(matches!(rig.command().await, CodexCommand::Cancel { .. }));
        rig.emit(finished("s1", "interrupted")).await;

        let events = rig.events();
        assert_eq!(
            events,
            [
                on_turn(&session, turn, TurnEventKind::StopRequested),
                on_turn(
                    &session,
                    turn,
                    TurnEventKind::StopFailed {
                        message: "turn/interrupt was rejected".to_string()
                    }
                ),
                on_turn(&session, turn, TurnEventKind::End(TurnEnd::Stopped)),
            ]
        );
        assert_turn_discipline(&events);
    }

    #[tokio::test]
    async fn closing_a_session_mid_turn_reports_one_stop_and_no_end() {
        let (mut rig, session, turn) = started_turn().await;
        rig.ledger.close(&session).unwrap();
        assert!(matches!(
            rig.command().await,
            CodexCommand::CloseSession { session_id } if session_id == "s1"
        ));
        rig.emit(CodexEvent::Cancelled {
            session_id: "s1".to_string(),
        })
        .await;
        assert_eq!(
            rig.events(),
            [on_turn(&session, turn, TurnEventKind::StopRequested)]
        );

        // The closed turn is not failed again when the worker is lost.
        rig.end_task(Err(anyhow::anyhow!("boom"))).await;
        assert_eq!(rig.events(), [lost("Codex app-server failed: boom", true)]);
    }

    #[tokio::test]
    async fn a_closed_session_forgets_its_turn_after_the_one_stop_the_worker_reports() {
        let (mut backend, worker, _ended) = direct();
        let session = sid("s1");
        let turn = TurnId::mint();
        backend.prompt(&session, turn, say("hello")).unwrap();
        backend.close(&session).unwrap();
        assert!(backend.live.get(&session).is_some_and(|live| live.closing));
        // Closing already asked the worker to cancel, so a stop is not sent.
        backend.stop(&session, turn).unwrap();

        worker
            .send(CodexEvent::Cancelled {
                session_id: "s1".to_string(),
            })
            .await
            .unwrap();
        worker.send(finished("s1", "interrupted")).await.unwrap();
        assert_eq!(
            backend.poll(),
            Some(on_turn(&session, turn, TurnEventKind::StopRequested))
        );
        assert!(backend.live.is_empty());
        assert_eq!(backend.poll(), None);
    }

    #[tokio::test]
    async fn losing_the_worker_mid_turn_ends_the_turn_and_then_reports_the_loss() {
        let (mut rig, session, turn) = started_turn().await;
        rig.emit(accepted("s1")).await;
        rig.emit(update("s1", "partial")).await;
        rig.end_task(Err(anyhow::anyhow!("fixture adapter exited")))
            .await;

        let message = "Codex app-server failed: fixture adapter exited";
        let events = rig.events();
        assert_eq!(
            events,
            [
                on_turn(&session, turn, TurnEventKind::Accepted),
                on_turn(&session, turn, TurnEventKind::Delta("partial".to_string())),
                on_turn(
                    &session,
                    turn,
                    TurnEventKind::End(TurnEnd::Failed(Failure {
                        message: message.to_string(),
                        cause: FailureCause::BackendLost,
                        delivery: Delivery::Accepted,
                    }))
                ),
                lost(message, true),
            ]
        );
        assert_turn_discipline(&events);
        assert!(!rig.ledger.is_running());
    }

    #[tokio::test]
    async fn a_loss_before_the_prompt_was_accepted_leaves_delivery_unconfirmed() {
        let (mut rig, session, turn) = started_turn().await;
        rig.end_task(Ok(())).await;

        let message = "Codex app-server stopped unexpectedly";
        assert_eq!(
            rig.events(),
            [
                on_turn(
                    &session,
                    turn,
                    TurnEventKind::End(TurnEnd::Failed(Failure {
                        message: message.to_string(),
                        cause: FailureCause::BackendLost,
                        delivery: Delivery::Unconfirmed,
                    }))
                ),
                lost(message, true),
            ]
        );
    }

    #[tokio::test]
    async fn events_buffered_before_the_loss_are_delivered_before_it() {
        let (mut rig, session, turn) = started_turn().await;
        for index in 0..5 {
            rig.emit(update("s1", &index.to_string())).await;
        }
        rig.emit(finished("s1", "completed")).await;
        rig.end_task(Err(anyhow::anyhow!("gone"))).await;

        let events = rig.events();
        let mut expected = (0..5)
            .map(|index| on_turn(&session, turn, TurnEventKind::Delta(index.to_string())))
            .collect::<Vec<_>>();
        expected.push(on_turn(
            &session,
            turn,
            TurnEventKind::End(TurnEnd::Completed(Completion::EndTurn)),
        ));
        expected.push(lost("Codex app-server failed: gone", true));
        assert_eq!(events, expected);
        assert_turn_discipline(&events);
    }

    #[tokio::test]
    async fn a_worker_that_only_answered_queries_is_lost_without_conversations() {
        let mut rig = Rig::new();
        rig.ledger.list_models(QueryId(1)).unwrap();
        rig.command().await;
        rig.end_task(Err(anyhow::anyhow!("gone"))).await;
        assert_eq!(
            rig.events(),
            [
                Event::Reply {
                    id: QueryId(1),
                    kind: QueryKind::ListModels,
                    result: Err("Codex app-server failed: gone".to_string()),
                },
                lost("Codex app-server failed: gone", false),
            ]
        );
    }

    #[tokio::test]
    async fn a_long_failure_is_bounded() {
        let mut rig = Rig::new();
        rig.end_task(Err(anyhow::anyhow!("x".repeat(3000)))).await;
        let events = rig.events();
        let [Event::Lost(Lost { message, .. })] = events.as_slice() else {
            panic!("expected one loss");
        };
        let suffix = "… (full details are in the Red log)";
        assert!(message.starts_with("Codex app-server failed: xxx"));
        assert!(message.ends_with(suffix));
        assert_eq!(
            message.chars().count(),
            MAX_FAILURE_MESSAGE_CHARS + suffix.chars().count()
        );
    }

    #[tokio::test]
    async fn a_panicking_task_is_reported_as_a_task_failure() {
        let (bridge, _worker) = CodexBridge::channel(NonZeroUsize::new(64).unwrap());
        let task = tokio::spawn(async {
            if true {
                panic!("fixture panic");
            }
            Ok(())
        });
        let mut backend = CodexBackend::with_worker(
            &AgentConfig::default(),
            NoHost,
            bridge,
            task,
            Path::new(ROOT),
        );
        settle().await;
        let Some(Event::Lost(Lost { message, .. })) = backend.poll() else {
            panic!("expected a loss");
        };
        assert!(message.starts_with("Codex app-server task failed: "));
        assert!(message.contains("fixture panic"));
    }

    #[tokio::test]
    async fn a_failure_with_no_session_is_a_failed_creation() {
        let mut rig = Rig::new();
        rig.emit(CodexEvent::Failed {
            session_id: None,
            message: "boom".to_string(),
        })
        .await;
        assert_eq!(
            rig.events(),
            [Event::Session(SessionEvent::CreateFailed {
                message: "boom".to_string()
            })]
        );
    }

    #[tokio::test]
    async fn a_failure_ends_the_turn_in_flight_or_reports_the_idle_session() {
        let (mut rig, session, turn) = started_turn().await;
        rig.emit(CodexEvent::Failed {
            session_id: Some("s1".to_string()),
            message: "Codex session was not found".to_string(),
        })
        .await;
        rig.emit(CodexEvent::Failed {
            session_id: Some("s1".to_string()),
            message: "late".to_string(),
        })
        .await;

        let events = rig.events();
        assert_eq!(
            events,
            [
                on_turn(
                    &session,
                    turn,
                    TurnEventKind::End(TurnEnd::Failed(Failure {
                        message: "Codex session was not found".to_string(),
                        cause: FailureCause::Agent,
                        delivery: Delivery::Unconfirmed,
                    }))
                ),
                Event::Session(SessionEvent::Failed {
                    session: session.clone(),
                    message: "late".to_string(),
                }),
            ]
        );
    }

    #[tokio::test]
    async fn a_saved_conversation_rejoins_only_under_the_same_contract() {
        let (backend, _worker, _ended) = direct();
        let saved = |contract| SavedSession {
            backend: BackendKind::Codex,
            session: sid("old"),
            contract,
        };
        let current = backend.contract();
        assert_eq!(backend.continuity(&saved(None)), Continuity::Fork);
        assert_eq!(
            backend.continuity(&saved(Some(ContractStamp::new(0)))),
            Continuity::Fork
        );
        assert_eq!(
            backend.continuity(&saved(Some(current))),
            Continuity::Rejoin
        );
    }

    #[tokio::test]
    async fn a_forked_restore_continues_the_thread_and_reports_the_new_id() {
        let mut rig = Rig::new();
        let saved = SavedSession {
            backend: BackendKind::Codex,
            session: sid("old"),
            contract: None,
        };
        rig.ledger.restore(cwd(), &saved).unwrap();
        assert!(matches!(
            rig.command().await,
            CodexCommand::ContinueSession { session_id, cwd }
                if session_id == "old" && cwd == Path::new(ROOT)
        ));
        rig.emit(CodexEvent::SessionRestored {
            session_id: "new".to_string(),
            thread: RestoredHistory::default(),
        })
        .await;
        assert_eq!(
            rig.events(),
            [Event::Session(SessionEvent::Restored {
                session: sid("new"),
                history: RestoredHistory::default(),
            })]
        );
    }

    #[tokio::test]
    async fn a_rejoined_restore_resumes_the_session_and_a_failed_one_names_the_request() {
        let mut rig = Rig::new();
        let saved = SavedSession {
            backend: BackendKind::Codex,
            session: sid("old"),
            contract: Some(rig.ledger.contract()),
        };
        rig.ledger.restore(cwd(), &saved).unwrap();
        assert!(matches!(
            rig.command().await,
            CodexCommand::ResumeSession { session_id, .. } if session_id == "old"
        ));
        rig.emit(CodexEvent::SessionRestoreFailed {
            session_id: "old".to_string(),
            message: "missing".to_string(),
        })
        .await;
        assert_eq!(
            rig.events(),
            [Event::Session(SessionEvent::RestoreFailed {
                requested: sid("old"),
                message: "missing".to_string(),
            })]
        );
    }

    #[tokio::test]
    async fn a_new_session_carries_the_chosen_model() {
        let mut rig = Rig::new();
        let selection = ModelSelection {
            model: "gpt-test".to_string(),
            effort: Some("high".to_string()),
        };
        rig.ledger
            .new_session(cwd(), Some(selection.clone()))
            .unwrap();
        assert!(matches!(
            rig.command().await,
            CodexCommand::NewSessionWithModel { selection: sent, .. } if sent == selection
        ));
    }

    #[tokio::test]
    async fn model_events_name_the_session_they_belong_to() {
        let mut rig = Rig::new();
        rig.emit(CodexEvent::SessionModelChanged {
            session_id: "s1".to_string(),
            model_info: model("gpt-test"),
        })
        .await;
        rig.emit(CodexEvent::SessionModelRerouted {
            session_id: "s1".to_string(),
            model: "gpt-other".to_string(),
        })
        .await;
        assert_eq!(
            rig.events(),
            [
                Event::Session(SessionEvent::ModelChanged {
                    session: sid("s1"),
                    model: model("gpt-test"),
                }),
                Event::Session(SessionEvent::ModelRerouted {
                    session: sid("s1"),
                    model: "gpt-other".to_string(),
                }),
            ]
        );
    }

    #[tokio::test]
    async fn model_queries_are_answered_with_their_own_kind_even_when_they_fail() {
        let mut rig = Rig::new();
        let selection = ModelSelection {
            model: "gpt-test".to_string(),
            effort: None,
        };
        rig.ledger.default_model(QueryId(1), cwd()).unwrap();
        rig.ledger.list_models(QueryId(2)).unwrap();
        rig.ledger
            .set_model(QueryId(3), &sid("s1"), selection.clone())
            .unwrap();
        rig.ledger.list_models(QueryId(4)).unwrap();
        assert!(matches!(
            rig.command().await,
            CodexCommand::ModelRequest {
                request_id: 1,
                request: ModelQuery::ReadDefault { cwd },
            } if cwd == Path::new(ROOT)
        ));
        assert!(matches!(
            rig.command().await,
            CodexCommand::ModelRequest {
                request_id: 2,
                request: ModelQuery::List,
            }
        ));
        assert!(matches!(
            rig.command().await,
            CodexCommand::ModelRequest {
                request_id: 3,
                request: ModelQuery::Set { session_id, selection: sent },
            } if session_id == "s1" && sent == selection
        ));
        rig.command().await;

        let reply = |request_id, result| CodexEvent::ModelRequestCompleted { request_id, result };
        rig.emit(reply(1, Ok(ModelResult::Default(model("gpt-test")))))
            .await;
        rig.emit(reply(2, Ok(ModelResult::Catalog(Vec::new()))))
            .await;
        rig.emit(reply(3, Err("busy".to_string()))).await;
        rig.emit(reply(4, Ok(ModelResult::Selected { confirmed: None })))
            .await;
        assert_eq!(
            rig.events(),
            [
                Event::Reply {
                    id: QueryId(1),
                    kind: QueryKind::DefaultModel,
                    result: Ok(ReplyBody::Model(ModelResult::Default(model("gpt-test")))),
                },
                Event::Reply {
                    id: QueryId(2),
                    kind: QueryKind::ListModels,
                    result: Ok(ReplyBody::Model(ModelResult::Catalog(Vec::new()))),
                },
                Event::Reply {
                    id: QueryId(3),
                    kind: QueryKind::SetModel,
                    result: Err("busy".to_string()),
                },
                Event::Reply {
                    id: QueryId(4),
                    kind: QueryKind::ListModels,
                    result: Ok(ReplyBody::Model(ModelResult::Selected { confirmed: None })),
                },
            ]
        );
    }

    #[tokio::test]
    async fn session_queries_and_commit_messages_are_answered_with_their_kinds() {
        let mut rig = Rig::new();
        let query = SessionQuery {
            cwd: cwd().to_path_buf(),
            scope: SessionScope::Red,
            query: "fix".to_string(),
            exclude_session_id: None,
            search_contents: false,
        };
        rig.ledger.list_sessions(QueryId(1), query.clone()).unwrap();
        rig.ledger
            .preview_session(QueryId(2), cwd(), &sid("s9"))
            .unwrap();
        rig.ledger
            .commit_messages()
            .unwrap()
            .generate(QueryId(3), cwd(), "diff".to_string())
            .unwrap();
        assert!(matches!(
            rig.command().await,
            CodexCommand::SessionList { request_id: 1, request } if request == query
        ));
        assert!(matches!(
            rig.command().await,
            CodexCommand::SessionPreview { request_id: 2, session_id } if session_id == "s9"
        ));
        assert!(matches!(
            rig.command().await,
            CodexCommand::GenerateCommitMessage { request_id: 3, cwd, prompt }
                if cwd == Path::new(ROOT) && prompt == "diff"
        ));

        let page = SessionPage {
            sessions: Vec::new(),
            more: true,
        };
        let preview = SessionPreview {
            session_id: "s9".to_string(),
            prompt: "p".to_string(),
            response: "r".to_string(),
        };
        rig.emit(CodexEvent::SessionListCompleted {
            request_id: 1,
            result: Ok(page.clone()),
        })
        .await;
        rig.emit(CodexEvent::SessionPreviewCompleted {
            request_id: 2,
            result: Ok(preview.clone()),
        })
        .await;
        rig.emit(CodexEvent::CommitMessageGenerated {
            request_id: 3,
            result: Err("timed out".to_string()),
        })
        .await;
        assert_eq!(
            rig.events(),
            [
                Event::Reply {
                    id: QueryId(1),
                    kind: QueryKind::ListSessions,
                    result: Ok(ReplyBody::Sessions(page)),
                },
                Event::Reply {
                    id: QueryId(2),
                    kind: QueryKind::PreviewSession,
                    result: Ok(ReplyBody::Preview(preview)),
                },
                Event::Reply {
                    id: QueryId(3),
                    kind: QueryKind::CommitMessage,
                    result: Err("timed out".to_string()),
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_prompt_carries_its_context_and_the_recap_in_front_of_the_text() {
        let (mut rig, session, _turn) = started_turn().await;
        rig.emit(finished("s1", "completed")).await;
        rig.events();

        let turn = TurnId::mint();
        rig.ledger
            .prompt(
                &session,
                turn,
                Prompt {
                    text: "next".to_string(),
                    context: Some(EditorContext {
                        uri: "file:///workspace/a.rs".to_string(),
                        text: "fn a() {}".to_string(),
                    }),
                    recap: Some("earlier".to_string()),
                },
            )
            .unwrap();
        let expected = format!("{RECAP_PREFIX}\nearlier{RECAP_FOLLOW_UP}next");
        assert!(matches!(
            rig.command().await,
            CodexCommand::PromptWithContext { session_id, turn: sent, text, uri, context }
                if session_id == "s1"
                    && sent == turn
                    && text == expected
                    && uri == "file:///workspace/a.rs"
                    && context == "fn a() {}"
        ));
    }

    #[tokio::test]
    async fn inline_assist_requests_and_events_keep_the_editor_request_id() {
        let mut rig = Rig::new();
        let inline = rig.ledger.inline_assist().unwrap();
        inline
            .start(
                "r1".to_string(),
                cwd(),
                "rename".to_string(),
                "ctx".to_string(),
            )
            .unwrap();
        inline
            .followup(
                "r2".to_string(),
                &sid("i1"),
                "again".to_string(),
                "ctx2".to_string(),
            )
            .unwrap();
        assert!(matches!(
            rig.command().await,
            CodexCommand::InlineAssist { request_id, cwd, prompt, context }
                if request_id == "r1"
                    && cwd == Path::new(ROOT)
                    && prompt == "rename"
                    && context == "ctx"
        ));
        assert!(matches!(
            rig.command().await,
            CodexCommand::InlineAssistFollowup { request_id, session_id, prompt, context }
                if request_id == "r2"
                    && session_id == "i1"
                    && prompt == "again"
                    && context == "ctx2"
        ));

        let result = InlineAssistResult {
            expanded_scope: None,
            replacement: Some("x".to_string()),
            comments: Vec::new(),
            needs_agent: None,
        };
        rig.emit(CodexEvent::InlineSessionCreated {
            request_id: "r1".to_string(),
            session_id: "i1".to_string(),
        })
        .await;
        rig.emit(CodexEvent::InlineContextRead {
            request_id: "r1".to_string(),
            description: "read a.rs".to_string(),
        })
        .await;
        rig.emit(CodexEvent::InlineAnswerDelta {
            request_id: "r1".to_string(),
            text: "ok".to_string(),
        })
        .await;
        rig.emit(CodexEvent::InlineResult {
            request_id: "r1".to_string(),
            session_id: "i1".to_string(),
            result: result.clone(),
        })
        .await;
        rig.emit(CodexEvent::InlineFailed {
            request_id: Some("r2".to_string()),
            session_id: None,
            message: "nope".to_string(),
        })
        .await;
        assert_eq!(
            rig.events(),
            [
                Event::Inline(InlineEvent::SessionCreated {
                    request: "r1".to_string(),
                    session: sid("i1"),
                }),
                Event::Inline(InlineEvent::ContextRead {
                    request: "r1".to_string(),
                    description: "read a.rs".to_string(),
                }),
                Event::Inline(InlineEvent::AnswerDelta {
                    request: "r1".to_string(),
                    text: "ok".to_string(),
                }),
                Event::Inline(InlineEvent::Result {
                    request: "r1".to_string(),
                    session: sid("i1"),
                    result,
                }),
                Event::Inline(InlineEvent::Failed {
                    request: Some("r2".to_string()),
                    session: None,
                    message: "nope".to_string(),
                }),
            ]
        );
    }

    #[tokio::test]
    async fn a_backend_with_no_worker_refuses_commands_and_has_nothing_to_poll() {
        let mut backend = CodexBackend::new(&AgentConfig::default(), NoHost);
        assert!(!backend.is_running());
        assert_eq!(backend.root(), None);
        assert_eq!(backend.poll(), None);
        let gone = |result: Result<(), AgentError>| {
            assert!(matches!(
                result,
                Err(AgentError::Gone(message)) if message == NOT_RUNNING
            ));
        };
        gone(backend.prompt(&sid("s1"), TurnId::mint(), say("hi")));
        gone(backend.close(&sid("s1")));
        gone(backend.list_models(QueryId(1)));
        gone(backend.preview_session(QueryId(2), cwd(), &sid("s1")));
        gone(backend.inline_assist().unwrap().followup(
            "r".to_string(),
            &sid("i"),
            String::new(),
            String::new(),
        ));
        // A stop for a turn nobody is running has nothing to cancel.
        assert!(backend.stop(&sid("s1"), TurnId::mint()).is_ok());
    }

    #[tokio::test]
    async fn a_command_to_an_ended_worker_is_refused_with_the_reason_it_ended() {
        let (mut backend, worker, ended) = direct();
        ended
            .send(Err(anyhow::anyhow!("fixture adapter exited")))
            .ok();
        settle().await;
        drop(worker);
        assert!(matches!(
            backend.list_models(QueryId(1)),
            Err(AgentError::Gone(message))
                if message == "Codex app-server failed: fixture adapter exited"
        ));
        assert!(!backend.is_running());
        assert!(matches!(
            backend.list_models(QueryId(2)),
            Err(AgentError::Gone(message)) if message == NOT_RUNNING
        ));
    }

    #[tokio::test]
    async fn a_closed_command_channel_with_a_task_still_running_is_a_generic_loss() {
        let (mut backend, worker, _ended) = direct();
        drop(worker);
        assert!(matches!(
            backend.list_models(QueryId(1)),
            Err(AgentError::Gone(message)) if message == STOPPED_UNEXPECTEDLY
        ));
        assert!(!backend.is_running());
    }

    #[tokio::test]
    async fn one_worker_serves_one_root() {
        let (mut backend, _worker, _ended) = direct();
        backend.ensure(Path::new(ROOT)).unwrap();
        assert_eq!(backend.root(), Some(Path::new(ROOT)));
        assert!(matches!(
            backend.ensure(Path::new("/other")),
            Err(AgentError::Unavailable(message))
                if message
                    == "Codex session root `/other` does not match the active agent workspace `/workspace`"
        ));
        assert_eq!(backend.root(), Some(Path::new(ROOT)));
    }

    #[tokio::test]
    async fn a_missing_executable_is_unavailable_and_starts_nothing() {
        let config = AgentConfig {
            command: Some("/nonexistent/codex".to_string()),
            ..AgentConfig::default()
        };
        let mut backend = CodexBackend::new(&config, NoHost);
        let expected = "Codex CLI was not found; install Codex, run `codex login`, and try again";
        assert!(matches!(
            backend.ensure(cwd()),
            Err(AgentError::Unavailable(message)) if message == expected
        ));
        assert!(matches!(
            backend.new_session(cwd(), None),
            Err(AgentError::Unavailable(message)) if message == expected
        ));
        assert!(!backend.is_running());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_started_worker_that_dies_is_reported_lost() {
        let config = AgentConfig {
            command: Some("false".to_string()),
            ..AgentConfig::default()
        };
        let mut backend = CodexBackend::new(&config, NoHost);
        let root = std::env::temp_dir();
        backend.ensure(&root).unwrap();
        assert!(backend.is_running());
        assert_eq!(backend.root(), Some(root.as_path()));

        let event = timeout(Duration::from_secs(10), async {
            loop {
                if let Some(event) = backend.poll() {
                    break event;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the worker's end is reported");
        assert!(matches!(
            event,
            Event::Lost(Lost { scope: LostScope::Backend, message, .. })
                if message.starts_with("Codex app-server failed: ")
        ));
        assert!(!backend.is_running());
        assert_eq!(backend.root(), None);
    }

    #[tokio::test]
    async fn shutting_down_drops_the_worker_without_reporting_a_loss() {
        let (mut backend, mut worker, _ended) = direct();
        let session = sid("s1");
        backend.prompt(&session, TurnId::mint(), say("hi")).unwrap();
        backend.shutdown();
        assert!(!backend.is_running());
        assert!(backend.live.is_empty());
        assert_eq!(backend.poll(), None);
        assert!(matches!(
            worker.recv().await,
            Some(CodexCommand::Prompt { .. })
        ));
        assert!(worker.recv().await.is_none());
        assert!(backend.detach_for_exit().is_none());
    }

    #[tokio::test]
    async fn detaching_for_exit_closes_the_commands_and_resolves_when_the_task_ends() {
        let (mut backend, mut worker, ended) = direct();
        let exit = backend.detach_for_exit().expect("a worker was running");
        assert!(!backend.is_running());
        // The worker sees its commands close, which is what ends it.
        assert!(worker.recv().await.is_none());
        ended.send(Ok(())).ok();
        timeout(Duration::from_secs(1), exit)
            .await
            .expect("the exit future resolves once the task ended");
        assert!(backend.detach_for_exit().is_none());
    }
}
