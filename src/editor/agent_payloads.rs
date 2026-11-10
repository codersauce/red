//! The plugin notification each agent event produces.

use super::plugin_json;
use crate::agent_backend::events::{Completion, SessionEvent, TurnEnd, TurnEvent, TurnEventKind};
use serde_json::{json, Value};

/// The notification plugins get for `event`, with its payload. `None` for events
/// the editor consumes without announcing them: replies, inline progress, a loss,
/// restored history (which becomes a conversation snapshot), and turn acceptance.
#[cfg(test)]
pub(crate) fn agent_event_payload(
    event: &crate::agent_backend::events::Event,
) -> Option<(&'static str, Value)> {
    use crate::agent_backend::events::Event;
    match event {
        Event::Session(event) => session_event_payload(event),
        Event::Turn(event) => turn_event_payload(event),
        Event::Reply { .. } | Event::Inline(_) | Event::Lost(_) => None,
    }
}

pub(super) fn session_event_payload(event: &SessionEvent) -> Option<(&'static str, Value)> {
    Some(match event {
        SessionEvent::Created { session } => (
            "agent:session_created",
            json!({ "session_id": session.as_str() }),
        ),
        SessionEvent::CreateFailed { message } => (
            "agent:error",
            json!({ "session_id": Value::Null, "message": message }),
        ),
        SessionEvent::Failed { session, message } => (
            "agent:error",
            json!({ "session_id": session.as_str(), "message": message }),
        ),
        SessionEvent::Restored { .. } => return None,
        SessionEvent::RestoreFailed { requested, message } => (
            "agent:session_restore_failed",
            json!({ "session_id": requested.as_str(), "message": message }),
        ),
        SessionEvent::ModelChanged { session, model } => (
            "agent:model_changed",
            json!({ "session_id": session.as_str(), "model_info": model }),
        ),
        SessionEvent::ModelRerouted { session, model } => (
            "agent:model_rerouted",
            json!({ "session_id": session.as_str(), "model": model }),
        ),
    })
}

pub(super) fn turn_event_payload(event: &TurnEvent) -> Option<(&'static str, Value)> {
    let session_id = event.session.as_str();
    Some(match &event.kind {
        TurnEventKind::Accepted => return None,
        TurnEventKind::Delta(text) => (
            "agent:update",
            json!({ "session_id": session_id, "text": text }),
        ),
        TurnEventKind::MessageDone(text) => (
            "agent:message_completed",
            json!({ "session_id": session_id, "text": text }),
        ),
        TurnEventKind::Activity(update) => (
            "agent:activity",
            json!({
                "session_id": session_id,
                "update": plugin_json(serde_json::to_value(update).expect("agent payloads serialize")),
            }),
        ),
        TurnEventKind::StopRequested => ("agent:cancelled", json!({ "session_id": session_id })),
        TurnEventKind::StopFailed { message } => (
            "agent:cancel_failed",
            json!({ "session_id": session_id, "message": message }),
        ),
        TurnEventKind::End(TurnEnd::Completed(completion)) => (
            "agent:completed",
            json!({
                "session_id": session_id,
                "stop_reason": match completion {
                    Completion::EndTurn => "completed",
                    Completion::Other(reason) => reason.as_str(),
                },
            }),
        ),
        TurnEventKind::End(TurnEnd::Stopped) => (
            "agent:completed",
            json!({ "session_id": session_id, "stop_reason": "interrupted" }),
        ),
        TurnEventKind::End(TurnEnd::Failed(failure)) => (
            "agent:error",
            json!({ "session_id": session_id, "message": failure.message }),
        ),
    })
}
