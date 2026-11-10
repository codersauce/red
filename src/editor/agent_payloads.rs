//! The plugin notification each agent event produces.

use super::plugin_json;
use crate::agent_backend::events::{Completion, SessionEvent, TurnEnd, TurnEvent, TurnEventKind};
use crate::agent_backend::types::Activity;
use crate::agent_conversation::AgentConversationSnapshot;
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

/// A conversation as the pane-restore events carry it. `session_id` repeats
/// `thread_id` for plugins that should not depend on Codex vocabulary.
pub(crate) fn conversation_payload(
    conversation: AgentConversationSnapshot,
) -> serde_json::Result<Value> {
    let session_id = Value::String(conversation.thread_id.clone());
    let mut payload = plugin_json(serde_json::to_value(conversation)?);
    payload["session_id"] = session_id;
    Ok(payload)
}

/// The `update` of `agent:activity`: the `session_update` form plugins have
/// always read, plus `activity` (`thought` or `tool`) and `phase` (`started`,
/// `updated`, `finished`), which do not depend on ACP tags.
fn activity_update_payload(activity: &Activity) -> Value {
    let (kind, phase) = match activity {
        Activity::Thought => ("thought", "updated"),
        Activity::ToolStarted(_) => ("tool", "started"),
        Activity::ToolFinished(_) => ("tool", "finished"),
    };
    let mut update = plugin_json(serde_json::to_value(activity).expect("agent payloads serialize"));
    update["activity"] = json!(kind);
    update["phase"] = json!(phase);
    update
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
                "update": activity_update_payload(update),
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
