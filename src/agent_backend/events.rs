//! The single event stream an agent backend produces.

use super::{
    ids::{QueryId, SessionId, TurnId},
    types::{Activity, ModelInfo, ModelResult, RestoredHistory, SessionPage, SessionPreview},
};
use crate::inline_assist::InlineAssistResult;

/// Everything a backend reports, in the order it happened.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A conversation was created, restored, or changed models.
    Session(SessionEvent),
    /// Progress of one turn.
    Turn(TurnEvent),
    /// The answer to one query. Exactly one arrives per accepted query; the
    /// ledger synthesizes it when the backend is lost.
    Reply {
        id: QueryId,
        kind: QueryKind,
        result: Result<ReplyBody, String>,
    },
    /// Progress of an inline-assist request, which is not an agent turn.
    Inline(InlineEvent),
    /// The backend, or one session of it, ended. Everything open in that scope
    /// was already closed by earlier events in this stream.
    Lost(Lost),
}

/// A change to a conversation as a whole.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    Created {
        session: SessionId,
    },
    /// `new_session` failed before a conversation existed.
    CreateFailed {
        message: String,
    },
    /// `session` is the id to use from now on. With `Continuity::Fork` it
    /// differs from the requested one.
    Restored {
        session: SessionId,
        history: RestoredHistory,
    },
    RestoreFailed {
        requested: SessionId,
        message: String,
    },
    ModelChanged {
        session: SessionId,
        model: ModelInfo,
    },
    /// The backend ran the next turns on `model` instead of the one asked for.
    ModelRerouted {
        session: SessionId,
        model: String,
    },
}

/// A fact about one turn. Every turn event carries Red's session and turn id;
/// the backend's own ids never appear.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnEvent {
    pub session: SessionId,
    pub turn: TurnId,
    pub kind: TurnEventKind,
}

/// What happened to a turn.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnEventKind {
    /// The agent confirmed it received this turn's prompt, which is not the
    /// same as the editor having sent it. At most once per turn, before any
    /// `Delta`, `MessageDone` or `Activity`. `StopRequested` may precede it.
    Accepted,
    /// A piece of the agent's reply.
    Delta(String),
    /// The agent finished one message of the reply.
    MessageDone(String),
    Activity(Activity),
    /// A stop was issued and the agent has not ended the turn yet. Never
    /// closing.
    StopRequested,
    /// The agent refused the stop. The turn still runs and `stop` may be
    /// retried. Never closing.
    StopFailed {
        message: String,
    },
    /// The closing event. Exactly one per turn.
    End(TurnEnd),
}

/// How a turn ended.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnEnd {
    Completed(Completion),
    /// The user's stop took effect.
    Stopped,
    Failed(Failure),
}

/// How the agent said a turn finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completion {
    EndTurn,
    /// A status Red has no name for. The editor shows it as the agent's
    /// reason for stopping.
    Other(String),
}

/// Why a turn failed.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub message: String,
    pub cause: FailureCause,
    /// Backends emit `Unconfirmed`; the ledger overwrites it with what it
    /// actually saw.
    pub delivery: Delivery,
}

/// Where a failure came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCause {
    /// The agent reported it.
    Agent,
    /// The backend ended under the turn. Synthesized by the ledger.
    BackendLost,
}

/// Whether the backend ever confirmed it received a prompt. `Unconfirmed`
/// means it is safe to send the prompt again, except in one window: the agent
/// read it and died before acknowledging, so a restored session may already
/// contain it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Unconfirmed,
    Accepted,
}

/// Which request a reply answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueryKind {
    DefaultModel,
    ListModels,
    SetModel,
    ListSessions,
    PreviewSession,
    CommitMessage,
}

/// The successful answer to a query.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplyBody {
    Model(ModelResult),
    Sessions(SessionPage),
    Preview(SessionPreview),
    CommitMessage(String),
}

impl ReplyBody {
    /// The kind of query this body can answer.
    pub fn kind(&self) -> QueryKind {
        match self {
            Self::Model(ModelResult::Default(_)) => QueryKind::DefaultModel,
            Self::Model(ModelResult::Catalog(_)) => QueryKind::ListModels,
            Self::Model(ModelResult::Selected { .. }) => QueryKind::SetModel,
            Self::Sessions(_) => QueryKind::ListSessions,
            Self::Preview(_) => QueryKind::PreviewSession,
            Self::CommitMessage(_) => QueryKind::CommitMessage,
        }
    }
}

/// Progress of an inline-assist request. It keeps its editor-minted request
/// id and has no `TurnId`.
#[derive(Debug, Clone, PartialEq)]
pub enum InlineEvent {
    SessionCreated {
        request: String,
        session: SessionId,
    },
    ContextRead {
        request: String,
        description: String,
    },
    AnswerDelta {
        request: String,
        text: String,
    },
    Result {
        request: String,
        session: SessionId,
        result: InlineAssistResult,
    },
    Failed {
        request: Option<String>,
        session: Option<SessionId>,
        message: String,
    },
}

/// Which part of a backend ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LostScope {
    /// The whole backend. Codex runs one process, so this is its only scope.
    Backend,
    /// One session, for a backend that runs a process per session. Other
    /// sessions' turns and open queries are unaffected.
    Session(SessionId),
}

/// A backend or one of its sessions ended without the editor asking.
#[derive(Debug, Clone, PartialEq)]
pub struct Lost {
    pub scope: LostScope,
    /// User-facing description of what happened.
    pub message: String,
    /// Whether the editor should report the loss as a lost conversation. False
    /// only when the worker was started by a model or session query and no
    /// conversation was created or restored since. The ledger sets this
    /// itself.
    pub had_sessions: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_bodies_name_the_query_they_answer() {
        let model = ModelInfo {
            model: "m".to_string(),
            provider: None,
            effort: None,
        };
        let cases = [
            (
                ReplyBody::Model(ModelResult::Default(model.clone())),
                QueryKind::DefaultModel,
            ),
            (
                ReplyBody::Model(ModelResult::Catalog(Vec::new())),
                QueryKind::ListModels,
            ),
            (
                ReplyBody::Model(ModelResult::Selected {
                    confirmed: Some(model),
                }),
                QueryKind::SetModel,
            ),
            (
                ReplyBody::Sessions(SessionPage {
                    sessions: Vec::new(),
                    more: false,
                }),
                QueryKind::ListSessions,
            ),
            (
                ReplyBody::Preview(SessionPreview {
                    session_id: String::new(),
                    prompt: String::new(),
                    response: String::new(),
                }),
                QueryKind::PreviewSession,
            ),
            (
                ReplyBody::CommitMessage(String::new()),
                QueryKind::CommitMessage,
            ),
        ];
        for (body, kind) in cases {
            assert_eq!(body.kind(), kind);
        }
    }
}
