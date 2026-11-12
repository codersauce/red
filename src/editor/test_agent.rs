//! Test seam between editor tests and the agent backend.
//!
//! Editor tests drive the agent exclusively through [`AgentTestHandle`]: they
//! install it on an editor, script backend events with `emit_*`, observe what
//! the editor asked of the backend with `commands`, and simulate backend loss
//! with `lose`. The backend is the scripted one inside the real ledger, so the
//! editor sees the event stream it sees in production.

use std::{cell::Cell, cell::RefCell, collections::HashMap};

use super::Editor;
use crate::{
    agent_backend::{
        backend::AgentError,
        events::{Completion, Event, InlineEvent, TurnEnd, TurnEventKind},
        ids::{SessionId, TurnId},
        testing::{open_scripted, Call, Script},
    },
    inline_assist::InlineAssistResult,
};

/// What the editor asked the backend to do, in backend-neutral terms.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AgentCommand {
    Prompt {
        session: String,
        text: String,
    },
    Stop {
        session: String,
    },
    Close {
        session: String,
    },
    ListModels,
    DefaultModel,
    /// A saved session was brought back, with the tool stamp the editor
    /// vouched for; the backend rejoins it only under the stamp it writes.
    Restore {
        session: String,
        contract: Option<u32>,
    },
    Other,
}

/// Test-side endpoint of the agent backend installed on an editor.
pub(crate) struct AgentTestHandle {
    script: Script,
    /// How many recorded calls `commands` already returned.
    reported: Cell<usize>,
    /// Turns opened for sessions the editor never prompted.
    fixture_turns: RefCell<HashMap<String, TurnId>>,
}

impl AgentTestHandle {
    /// Replaces the editor's agent with a scripted one that is running.
    pub(crate) fn install(editor: &mut Editor) -> Self {
        let (agents, script) = open_scripted();
        editor.agent_manager.replace_agents(agents);
        Self {
            script,
            reported: Cell::new(0),
            fixture_turns: RefCell::default(),
        }
    }

    /// The commands the editor sent since the last call.
    pub(crate) fn commands(&self) -> Vec<AgentCommand> {
        let calls = self.script.calls();
        let reported = self.reported.replace(calls.len());
        calls[reported..]
            .iter()
            .map(|call| match call {
                Call::Prompt {
                    session, prompt, ..
                } => AgentCommand::Prompt {
                    session: session.to_string(),
                    text: prompt.text.clone(),
                },
                Call::Stop { session, .. } => AgentCommand::Stop {
                    session: session.to_string(),
                },
                Call::Close(session) => AgentCommand::Close {
                    session: session.to_string(),
                },
                Call::ListModels(_) => AgentCommand::ListModels,
                Call::DefaultModel { .. } => AgentCommand::DefaultModel,
                Call::Restore { saved, .. } => AgentCommand::Restore {
                    session: saved.session.to_string(),
                    contract: saved.contract.map(|stamp| stamp.get()),
                },
                _ => AgentCommand::Other,
            })
            .collect()
    }

    /// How many emitted events the editor has not consumed yet.
    pub(crate) fn pending(&self) -> usize {
        self.script.pending()
    }

    /// Tells the backend a conversation exists, so losing it is reported as a
    /// lost conversation rather than a lost query.
    pub(crate) fn open_session(&self) {
        self.script.open_session();
    }

    pub(crate) fn emit_update(&self, session: &str, text: &str) {
        self.emit_turn(session, TurnEventKind::Delta(text.to_string()));
    }

    pub(crate) fn emit_message_completed(&self, session: &str, text: &str) {
        self.emit_turn(session, TurnEventKind::MessageDone(text.to_string()));
    }

    /// Ends the session's turn. `"interrupted"` is a stop that took effect.
    pub(crate) fn emit_completed(&self, session: &str, status: &str) {
        let end = match status {
            "completed" | "end_turn" => TurnEnd::Completed(Completion::EndTurn),
            "interrupted" => TurnEnd::Stopped,
            other => TurnEnd::Completed(Completion::Other(other.to_string())),
        };
        self.emit_turn(session, TurnEventKind::End(end));
        self.fixture_turns.borrow_mut().remove(session);
    }

    pub(crate) fn emit_cancelled(&self, session: &str) {
        self.emit_turn(session, TurnEventKind::StopRequested);
    }

    pub(crate) fn emit_cancel_failed(&self, session: &str, message: &str) {
        self.emit_turn(
            session,
            TurnEventKind::StopFailed {
                message: message.to_string(),
            },
        );
    }

    pub(crate) fn emit_inline_context_read(&self, request: &str, description: &str) {
        self.script.emit(Event::Inline(InlineEvent::ContextRead {
            request: request.to_string(),
            description: description.to_string(),
        }));
    }

    pub(crate) fn emit_inline_result(
        &self,
        request: &str,
        session: &str,
        result: InlineAssistResult,
    ) {
        self.script.emit(Event::Inline(InlineEvent::Result {
            request: request.to_string(),
            session: SessionId::from_raw(session.to_string()),
            result,
        }));
    }

    /// Ends the backend with exactly `message` after everything already
    /// emitted is delivered.
    pub(crate) fn lose(&self, message: &str) {
        self.script.lose(message);
    }

    /// Makes the next command fail as if the agent had just ended, with
    /// `message` as the reason it gave.
    pub(crate) fn fail_next_gone(&self, message: &str) {
        self.script.fail_next(AgentError::Gone(message.to_string()));
    }

    /// Ends one session of the backend, which keeps running.
    pub(crate) fn lose_session(&self, session: &str, message: &str) {
        self.script
            .lose_session(&SessionId::from_raw(session.to_string()), message);
    }

    /// Makes the editor's exit wait for the agent until the returned sender
    /// fires.
    pub(crate) fn hold_exit(&self) -> tokio::sync::oneshot::Sender<()> {
        self.script.hold_exit()
    }

    fn emit_turn(&self, session: &str, kind: TurnEventKind) {
        let turn = self.turn_of(session);
        self.script
            .emit_turn(&SessionId::from_raw(session.to_string()), turn, kind);
    }

    /// The turn the editor started for `session`, or a turn opened here for a
    /// session the editor never prompted.
    fn turn_of(&self, session: &str) -> TurnId {
        let prompted = self
            .script
            .calls()
            .into_iter()
            .rev()
            .find_map(|call| match call {
                Call::Prompt {
                    session: prompted,
                    turn,
                    ..
                } if prompted.as_str() == session => Some(turn),
                _ => None,
            });
        if let Some(turn) = prompted {
            return turn;
        }
        *self
            .fixture_turns
            .borrow_mut()
            .entry(session.to_string())
            .or_insert_with(|| {
                let turn = TurnId::mint();
                self.script
                    .open_turn(&SessionId::from_raw(session.to_string()), turn);
                turn
            })
    }
}
