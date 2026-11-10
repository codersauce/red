//! Model discovery and conversation-scoped settings. These requests never alter
//! the user's on-disk configuration or Red's tool and permission restrictions.

use super::translate;
use super::*;
use crate::agent_backend::types::{ModelOption, ModelResult};
use std::collections::HashSet;

pub(super) enum ModelListPurpose {
    Catalog,
    Default {
        provider: Option<String>,
        effort: Option<String>,
    },
}

pub(super) enum PendingModelRequest {
    ReadDefault {
        request_id: i64,
    },
    List {
        request_id: i64,
        purpose: ModelListPurpose,
        models: Vec<ModelOption>,
        cursors: HashSet<String>,
    },
    Set {
        request_id: i64,
        session_id: String,
        selection: ModelSelection,
        previous: Option<ModelInfo>,
    },
}

impl PendingModelRequest {
    fn request_id(&self) -> i64 {
        match self {
            Self::ReadDefault { request_id }
            | Self::List { request_id, .. }
            | Self::Set { request_id, .. } => *request_id,
        }
    }
}

async fn complete(
    events: &mpsc::Sender<CodexEvent>,
    request_id: i64,
    result: std::result::Result<ModelResult, String>,
) {
    events
        .send(CodexEvent::ModelRequestCompleted { request_id, result })
        .await
        .ok();
}

async fn list_page(
    state: PendingModelRequest,
    cursor: Option<String>,
    input: &mut (impl AsyncWrite + Unpin),
    pending: &mut HashMap<String, Pending>,
    next_id: &mut u64,
) -> Result<()> {
    let id = rpc_id(next_id);
    pending.insert(id.clone(), Pending::Model(state));
    write_message(input, &json!({"id": id, "method": "model/list", "params": {"limit": 100, "cursor": cursor, "includeHidden": false}})).await
}

pub(super) async fn handle_command(
    request_id: i64,
    request: ModelQuery,
    input: &mut (impl AsyncWrite + Unpin),
    events: &mpsc::Sender<CodexEvent>,
    pending: &mut HashMap<String, Pending>,
    sessions: &HashMap<String, Session>,
    next_id: &mut u64,
) -> Result<()> {
    match request {
        ModelQuery::ReadDefault { cwd } => {
            let id = rpc_id(next_id);
            pending.insert(
                id.clone(),
                Pending::Model(PendingModelRequest::ReadDefault { request_id }),
            );
            write_message(input, &json!({"id": id, "method": "config/read", "params": {"includeLayers": false, "cwd": cwd}})).await?;
        }
        ModelQuery::List => {
            list_page(
                PendingModelRequest::List {
                    request_id,
                    purpose: ModelListPurpose::Catalog,
                    models: Vec::new(),
                    cursors: HashSet::new(),
                },
                None,
                input,
                pending,
                next_id,
            )
            .await?
        }
        ModelQuery::Set {
            session_id,
            selection,
        } => {
            if selection.model.trim().is_empty()
                || !sessions
                    .get(&session_id)
                    .is_some_and(|session| matches!(session.kind, SessionKind::Agent))
            {
                complete(
                    events,
                    request_id,
                    Err("Agent conversation is no longer available".into()),
                )
                .await;
                return Ok(());
            }
            let id = rpc_id(next_id);
            let mut params = json!({"threadId": session_id, "model": selection.model});
            if let Some(effort) = &selection.effort {
                params["effort"] = json!(effort);
            }
            pending.insert(
                id.clone(),
                Pending::Model(PendingModelRequest::Set {
                    request_id,
                    previous: sessions
                        .get(&session_id)
                        .and_then(|session| session.model_info.clone()),
                    session_id,
                    selection,
                }),
            );
            write_message(
                input,
                &json!({"id": id, "method": "thread/settings/update", "params": params}),
            )
            .await?;
        }
    }
    Ok(())
}

pub(super) async fn handle_response(
    request: PendingModelRequest,
    message: Value,
    input: &mut (impl AsyncWrite + Unpin),
    events: &mpsc::Sender<CodexEvent>,
    pending: &mut HashMap<String, Pending>,
    sessions: &mut HashMap<String, Session>,
    next_id: &mut u64,
) -> Result<()> {
    let request_id = request.request_id();
    if let Some(error) = message.get("error") {
        complete(
            events,
            request_id,
            Err(error["message"]
                .as_str()
                .unwrap_or("Codex model request failed")
                .to_string()),
        )
        .await;
        return Ok(());
    }
    match request {
        PendingModelRequest::ReadDefault { .. } => {
            let Some(config) = message
                .pointer("/result/config")
                .filter(|value| value.is_object())
            else {
                complete(
                    events,
                    request_id,
                    Err("Codex returned invalid model configuration".into()),
                )
                .await;
                return Ok(());
            };
            let translate::ConfiguredModel {
                model,
                provider,
                effort,
            } = translate::configured_model(config);
            if let Some(model) = model {
                let info = ModelInfo {
                    model,
                    provider,
                    effort,
                };
                complete(events, request_id, Ok(ModelResult::Default(info))).await;
            } else {
                list_page(
                    PendingModelRequest::List {
                        request_id,
                        purpose: ModelListPurpose::Default { provider, effort },
                        models: Vec::new(),
                        cursors: HashSet::new(),
                    },
                    None,
                    input,
                    pending,
                    next_id,
                )
                .await?;
            }
        }
        PendingModelRequest::Set {
            session_id,
            selection,
            previous,
            ..
        } => {
            let confirmed = sessions
                .get(&session_id)
                .and_then(|session| session.model_info.as_ref())
                .filter(|info| {
                    Some(*info) != previous.as_ref()
                        || (info.model == selection.model
                            && (selection.effort.is_none() || info.effort == selection.effort))
                });
            complete(
                events,
                request_id,
                Ok(ModelResult::Selected {
                    confirmed: confirmed.cloned(),
                }),
            )
            .await
        }
        PendingModelRequest::List {
            purpose,
            mut models,
            mut cursors,
            ..
        } => {
            let Some(page) = message.pointer("/result/data").and_then(Value::as_array) else {
                complete(
                    events,
                    request_id,
                    Err("Codex returned an invalid model catalog".into()),
                )
                .await;
                return Ok(());
            };
            for model in page.iter().filter_map(translate::model_option) {
                if !models.iter().any(|entry| entry.model == model.model) {
                    models.push(model);
                }
            }
            if let Some(cursor) = message
                .pointer("/result/nextCursor")
                .and_then(Value::as_str)
                .filter(|cursor| !cursor.is_empty())
            {
                if models.len() > 4096
                    || cursors.len() >= 100
                    || !cursors.insert(cursor.to_string())
                {
                    complete(
                        events,
                        request_id,
                        Err("Codex model catalog pagination did not finish".into()),
                    )
                    .await;
                } else {
                    list_page(
                        PendingModelRequest::List {
                            request_id,
                            purpose,
                            models,
                            cursors,
                        },
                        Some(cursor.to_string()),
                        input,
                        pending,
                        next_id,
                    )
                    .await?;
                }
            } else {
                let result = match purpose {
                    ModelListPurpose::Catalog => Ok(ModelResult::Catalog(models)),
                    ModelListPurpose::Default { provider, effort } => models
                        .iter()
                        .find(|model| model.is_default == Some(true))
                        .map(|model| {
                            ModelResult::Default(ModelInfo {
                                model: model.model.clone(),
                                provider,
                                effort: effort.or_else(|| translate::catalog_default_effort(model)),
                            })
                        })
                        .ok_or_else(|| "Codex did not return a default model".to_string()),
                };
                complete(events, request_id, result).await;
            }
        }
    }
    Ok(())
}

pub(super) async fn settings_updated(
    params: &Value,
    events: &mpsc::Sender<CodexEvent>,
    sessions: &mut HashMap<String, Session>,
) {
    let Some(session_id) = params["threadId"].as_str() else {
        return;
    };
    let Some(session) = sessions
        .get_mut(session_id)
        .filter(|session| matches!(session.kind, SessionKind::Agent))
    else {
        return;
    };
    let Some(model_info) = translate::model_info(&params["threadSettings"]) else {
        return;
    };
    if session.model_info.as_ref() == Some(&model_info) {
        return;
    }
    session.model_info = Some(model_info.clone());
    events
        .send(CodexEvent::SessionModelChanged {
            session_id: session_id.to_string(),
            model_info,
        })
        .await
        .ok();
}

pub(super) async fn model_rerouted(
    params: &Value,
    events: &mpsc::Sender<CodexEvent>,
    sessions: &HashMap<String, Session>,
) {
    let (Some(session_id), Some(turn_id), Some(model)) = (
        params["threadId"].as_str(),
        params["turnId"].as_str(),
        params["toModel"].as_str(),
    ) else {
        return;
    };
    if model.is_empty()
        || !sessions.get(session_id).is_some_and(|session| {
            matches!(session.kind, SessionKind::Agent)
                && session.active_turn.as_deref() == Some(turn_id)
        })
    {
        return;
    }
    events
        .send(CodexEvent::SessionModelRerouted {
            session_id: session_id.to_string(),
            model: model.to_string(),
        })
        .await
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(model: &str, effort: &str, kind: SessionKind) -> Session {
        Session {
            model_info: Some(ModelInfo {
                model: model.into(),
                provider: None,
                effort: Some(effort.into()),
            }),
            cwd: PathBuf::from("/workspace"),
            active_turn: Some("turn-1".into()),
            red_turn: None,
            pending_interrupt_turn_id: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            allow_sensitive_paths: false,
            kind,
        }
    }

    #[tokio::test]
    async fn repeated_catalog_cursor_fails_without_another_request() {
        let (events, mut received) = mpsc::channel(2);
        let mut output = Vec::new();
        handle_response(
            PendingModelRequest::List {
                request_id: 7,
                purpose: ModelListPurpose::Catalog,
                models: Vec::new(),
                cursors: HashSet::from(["again".into()]),
            },
            json!({"result":{"data":[],"nextCursor":"again"}}),
            &mut output,
            &events,
            &mut HashMap::new(),
            &mut HashMap::new(),
            &mut 1,
        )
        .await
        .unwrap();
        assert!(output.is_empty());
        assert!(matches!(
            received.recv().await,
            Some(CodexEvent::ModelRequestCompleted {
                request_id: 7,
                result: Err(_)
            })
        ));
    }

    #[tokio::test]
    async fn settings_ack_uses_normalized_server_values() {
        let (events, mut received) = mpsc::channel(2);
        let previous = session("first", "high", SessionKind::Agent).model_info;
        let mut sessions = HashMap::from([(
            "thread-1".into(),
            session("second", "low", SessionKind::Agent),
        )]);
        handle_response(
            PendingModelRequest::Set {
                request_id: 8,
                session_id: "thread-1".into(),
                selection: ModelSelection {
                    model: "second".into(),
                    effort: Some("high".into()),
                },
                previous,
            },
            json!({"result":{}}),
            &mut Vec::new(),
            &events,
            &mut HashMap::new(),
            &mut sessions,
            &mut 1,
        )
        .await
        .unwrap();
        assert!(
            matches!(received.recv().await, Some(CodexEvent::ModelRequestCompleted { result: Ok(ModelResult::Selected { confirmed: Some(info) }), .. }) if info.effort.as_deref() == Some("low"))
        );
    }

    #[tokio::test]
    async fn hidden_and_stale_turns_cannot_change_the_visible_model() {
        let (events, mut received) = mpsc::channel(4);
        let mut sessions = HashMap::from([
            ("agent".into(), session("first", "high", SessionKind::Agent)),
            (
                "inline".into(),
                session(
                    "hidden",
                    "low",
                    SessionKind::Inline {
                        request_id: "inline-request".into(),
                        result: None,
                    },
                ),
            ),
        ]);
        settings_updated(
            &json!({"threadId":"inline","threadSettings":{"model":"wrong"}}),
            &events,
            &mut sessions,
        )
        .await;
        model_rerouted(
            &json!({"threadId":"agent","turnId":"old-turn","toModel":"wrong"}),
            &events,
            &sessions,
        )
        .await;
        assert!(received.try_recv().is_err());
        model_rerouted(
            &json!({"threadId":"agent","turnId":"turn-1","toModel":"routed"}),
            &events,
            &sessions,
        )
        .await;
        assert!(
            matches!(received.recv().await, Some(CodexEvent::SessionModelRerouted { session_id, model }) if session_id == "agent" && model == "routed")
        );
        assert_eq!(
            sessions["agent"].model_info.as_ref().unwrap().model,
            "first"
        );
    }
}
