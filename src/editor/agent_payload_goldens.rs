//! Golden agent payloads. Each case feeds realistic Codex app-server messages
//! through the worker's handlers and pins the JSON the plugin receives. The
//! expected values were recorded from the untyped implementation and must not
//! change when the payloads gain types.

use super::*;
use crate::agent_backend::types::{ModelQuery, ModelSelection, SessionQuery, SessionScope};
use crate::codex::golden_support::Harness;
use crate::codex::{CodexCommand, CodexEvent};
use std::time::{SystemTime, UNIX_EPOCH};

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn model_payloads(events: Vec<CodexEvent>) -> Vec<Value> {
    events
        .into_iter()
        .map(|event| match event {
            CodexEvent::ModelRequestCompleted { result, .. } => {
                agent_models::model_result_payload(&result)
            }
            other => panic!("unexpected event {other:?}"),
        })
        .collect()
}

fn session_payloads(events: Vec<CodexEvent>) -> Vec<Value> {
    events
        .into_iter()
        .map(|event| match event {
            CodexEvent::SessionListCompleted { result, .. } => {
                agent_sessions::session_result_payload(&result)
            }
            CodexEvent::SessionPreviewCompleted { result, .. } => {
                agent_sessions::session_preview_payload(&result)
            }
            other => panic!("unexpected event {other:?}"),
        })
        .collect()
}

fn codex_model(model: &str, display: &str, default: bool, effort: &str) -> Value {
    json!({
        "id": model,
        "model": model,
        "upgrade": null,
        "upgradeInfo": null,
        "availabilityNux": null,
        "displayName": display,
        "description": format!("{display} for everyday coding."),
        "hidden": false,
        "supportedReasoningEfforts": [
            {"reasoningEffort": "low", "description": "Fast responses"},
            {"reasoningEffort": "high", "description": "Deeper reasoning"}
        ],
        "defaultReasoningEffort": effort,
        "inputModalities": ["text", "image"],
        "supportsPersonality": true,
        "additionalSpeedTiers": [],
        "isDefault": default
    })
}

fn model_request(request_id: i64, request: ModelQuery) -> CodexCommand {
    CodexCommand::ModelRequest {
        request_id,
        request,
    }
}

#[tokio::test]
async fn model_catalog_payload_merges_pages_and_drops_hidden_and_duplicates() {
    let first = codex_model("gpt-5.1-codex", "GPT-5.1 Codex", true, "medium");
    let second = codex_model("gpt-5.1-mini", "GPT-5.1 Mini", false, "low");
    let mut hidden = codex_model("secret", "Secret", false, "low");
    hidden["hidden"] = json!(true);
    let mut harness = Harness::new();
    harness.command(model_request(1, ModelQuery::List)).await;
    harness
        .serve(|method, params| {
            assert_eq!(method, "model/list");
            if params["cursor"].is_null() {
                json!({"result": {"data": [first.clone(), hidden.clone()], "nextCursor": "page-2"}})
            } else {
                json!({"result": {"data": [first.clone(), second.clone()], "nextCursor": null}})
            }
        })
        .await;

    assert_eq!(
        model_payloads(harness.events()),
        vec![json!({"models": [first, second], "error": ""})]
    );
}

async fn request(harness: &mut Harness, request_id: i64) {
    harness
        .command(model_request(
            request_id,
            ModelQuery::ReadDefault {
                cwd: PathBuf::from("/workspace"),
            },
        ))
        .await;
}

#[tokio::test]
async fn default_model_payload_reads_config_then_falls_back_to_the_catalog() {
    let config = |config: Value| json!({"result": {"config": config}});
    let mut harness = Harness::new();
    request(&mut harness, 1).await;
    harness
        .serve(|method, params| {
            assert_eq!(method, "config/read");
            assert_eq!(params["cwd"], "/workspace");
            config(json!({
                "model": "gpt-5.1-codex",
                "model_provider": "openai",
                "model_reasoning_effort": "high"
            }))
        })
        .await;
    request(&mut harness, 2).await;
    harness
        .serve(|_, _| config(json!({"model": "custom-model"})))
        .await;
    request(&mut harness, 3).await;
    harness
        .serve(|method, _| match method {
            "config/read" => config(json!({"model_provider": "openai"})),
            "model/list" => json!({"result": {"data": [
                codex_model("gpt-5.1-mini", "GPT-5.1 Mini", false, "low"),
                codex_model("gpt-5.1-codex", "GPT-5.1 Codex", true, "medium"),
            ], "nextCursor": null}}),
            other => panic!("unexpected {other}"),
        })
        .await;
    request(&mut harness, 4).await;
    harness
        .serve(|method, _| match method {
            "config/read" => config(json!({"model_reasoning_effort": "low"})),
            "model/list" => json!({"result": {"data": [
                codex_model("gpt-5.1-codex", "GPT-5.1 Codex", true, "medium"),
            ], "nextCursor": null}}),
            other => panic!("unexpected {other}"),
        })
        .await;
    request(&mut harness, 5).await;
    harness
        .serve(|_, _| json!({"result": {"config": null}}))
        .await;
    request(&mut harness, 6).await;
    harness
        .serve(|method, _| match method {
            "config/read" => config(json!({})),
            _ => json!({"result": {"data": [
                codex_model("gpt-5.1-mini", "GPT-5.1 Mini", false, "low"),
            ], "nextCursor": null}}),
        })
        .await;
    request(&mut harness, 7).await;
    harness
        .serve(|_, _| json!({"error": {"code": -32603, "message": "config unavailable"}}))
        .await;

    assert_eq!(
        model_payloads(harness.events()),
        vec![
            json!({"model_info": {"model": "gpt-5.1-codex", "provider": "openai", "effort": "high"}, "error": ""}),
            json!({"model_info": {"model": "custom-model", "provider": null, "effort": null}, "error": ""}),
            json!({"model_info": {"model": "gpt-5.1-codex", "provider": "openai", "effort": "medium"}, "error": ""}),
            json!({"model_info": {"model": "gpt-5.1-codex", "provider": null, "effort": "low"}, "error": ""}),
            json!({"error": "Codex returned invalid model configuration"}),
            json!({"error": "Codex did not return a default model"}),
            json!({"error": "config unavailable"}),
        ]
    );
}

#[tokio::test]
async fn model_selection_payload_reports_the_confirmed_server_settings() {
    let mut harness = Harness::new();
    harness.add_agent_session(
        "thread-1",
        "/workspace",
        Some("turn-1"),
        Some(("first", "high")),
    );
    let select = |model: &str, effort: Option<&str>| {
        model_request(
            9,
            ModelQuery::Set {
                session_id: "thread-1".into(),
                selection: ModelSelection {
                    model: model.into(),
                    effort: effort.map(str::to_string),
                },
            },
        )
    };

    // The server never confirmed the change, so there is no model to report.
    harness.command(select("second", Some("low"))).await;
    harness.serve(|_, _| json!({"result": {}})).await;
    // The server rejected it.
    harness.command(select("rejected", None)).await;
    harness
        .serve(|_, params| {
            assert_eq!(
                params,
                &json!({"threadId": "thread-1", "model": "rejected"})
            );
            json!({"error": {"code": -32602, "message": "model unavailable"}})
        })
        .await;
    // The conversation is gone.
    harness
        .command(model_request(
            10,
            ModelQuery::Set {
                session_id: "missing".into(),
                selection: ModelSelection {
                    model: "second".into(),
                    effort: None,
                },
            },
        ))
        .await;
    // Settings arrive before the acknowledgement.
    harness.command(select("second", Some("low"))).await;
    harness
        .message(json!({"method": "thread/settings/updated", "params": {
            "threadId": "thread-1",
            "threadSettings": {"model": "second", "modelProvider": "test-provider", "effort": "low"}
        }}))
        .await;
    harness.serve(|_, _| json!({"result": {}})).await;
    // A routed turn is announced separately.
    harness
        .message(json!({"method": "model/rerouted", "params": {
            "threadId": "thread-1", "turnId": "turn-1", "fromModel": "second", "toModel": "fallback", "reason": "highRiskCyberActivity"
        }}))
        .await;

    let mut payloads = Vec::new();
    for event in harness.events() {
        match event {
            CodexEvent::ModelRequestCompleted { result, .. } => {
                payloads.push(agent_models::model_result_payload(&result))
            }
            event => {
                let (name, payload) = agent_event_payload(event);
                payloads.push(json!({"event": name, "payload": payload}));
            }
        }
    }
    assert_eq!(
        payloads,
        vec![
            json!({"accepted": true, "model_info": null, "error": ""}),
            json!({"error": "model unavailable"}),
            json!({"error": "Agent conversation is no longer available"}),
            json!({"event": "agent:model_changed", "payload": {
                "session_id": "thread-1",
                "model_info": {"model": "second", "provider": "test-provider", "effort": "low"}
            }}),
            json!({"accepted": true, "model_info": {"model": "second", "provider": "test-provider", "effort": "low"}, "error": ""}),
            json!({"event": "agent:model_rerouted", "payload": {"session_id": "thread-1", "model": "fallback"}}),
        ]
    );
}

fn list_request(scope: SessionScope, query: &str, contents: bool) -> CodexCommand {
    CodexCommand::SessionList {
        request_id: 3,
        request: SessionQuery {
            cwd: PathBuf::from("/workspace"),
            scope,
            query: query.into(),
            exclude_session_id: Some("excluded".into()),
            search_contents: contents,
        },
    }
}

fn thread(id: &str, source: Value, name: Value, preview: &str, updated_at: i64) -> Value {
    json!({
        "id": id, "cwd": "/workspace", "ephemeral": false, "threadSource": "user",
        "source": source, "name": name, "preview": preview, "updatedAt": updated_at
    })
}

#[tokio::test]
async fn red_session_list_payload_labels_groups_and_orders_threads() {
    let now = now();
    let directory = tempfile::tempdir().unwrap();
    let rollout = directory.path().join("red-legacy.jsonl");
    std::fs::write(
        &rollout,
        format!(
            "{}\n",
            json!({"type": "session_meta", "payload": {"id": "red-legacy", "originator": "red"}})
        ),
    )
    .unwrap();
    let red = |id: &str, name: Value, preview: &str, updated_at: i64| {
        let mut value = thread(id, json!("appServer"), name, preview, updated_at);
        value["threadSource"] = json!("red");
        value
    };
    let mut legacy = thread(
        "red-legacy",
        json!("vscode"),
        json!("Legacy Red"),
        "",
        now - 30,
    );
    legacy["threadSource"] = Value::Null;
    legacy["path"] = json!(rollout);
    let mut originator = thread(
        "red-originator",
        json!("vscode"),
        Value::Null,
        "Fix the parser.Active editor context from file:///parser.rs:\n\n```text\nsource\n```",
        now - 7200,
    );
    originator["threadSource"] = Value::Null;
    originator["originator"] = json!("red");
    let mut elsewhere = red("red-elsewhere", json!("Elsewhere"), "", now - 5);
    elsewhere["cwd"] = json!("/other");
    let mut ephemeral = red("ephemeral", json!("Hidden"), "", now - 5);
    ephemeral["ephemeral"] = json!(true);
    let data = vec![
        red("current", json!("Current"), "", now),
        thread(
            "foreign",
            json!("appServer"),
            json!("Foreign"),
            "",
            now - 10,
        ),
        legacy,
        red("excluded", json!("Excluded"), "", now - 12),
        red(
            "recap",
            Value::Null,
            "Previous conversation (the last turn was interrupted):\nYou: cut",
            now - 100,
        ),
        red("untitled", Value::Null, "", now - 200),
        originator,
        red("red-old", Value::Null, "Older Red", now - 259_200),
        elsewhere,
        ephemeral,
    ];
    let mut harness = Harness::new();
    harness
        .command(list_request(SessionScope::Red, "", false))
        .await;
    harness
        .serve(|method, params| {
            assert_eq!(method, "thread/list");
            assert_eq!(params["sourceKinds"], json!(["appServer", "vscode"]));
            assert_eq!(params["cwd"], json!(["/workspace"]));
            json!({"result": {"data": data.clone(), "nextCursor": null}})
        })
        .await;

    let summary = |id: &str, label: &str, detail: &str, group: &str, updated_at: i64| {
        json!({
            "id": id, "cwd": "/workspace", "label": label, "detail": detail, "snippet": "",
            "source": "Red", "time_group": group, "is_red": true, "resumable": true,
            "updated_at": updated_at
        })
    };
    assert_eq!(
        session_payloads(harness.events()),
        vec![json!({
            "sessions": [
                summary("current", "Current", "just now · Red · workspace", "Recent", now),
                summary("red-legacy", "Legacy Red", "just now · Red · workspace", "Recent", now - 30),
                summary("recap", "Continued conversation", "1m ago · Red · workspace", "Recent", now - 100),
                summary("untitled", "Untitled session", "3m ago · Red · workspace", "Recent", now - 200),
                summary("red-originator", "Fix the parser.", "2h ago · Red · workspace", "Recent", now - 7200),
                summary("red-old", "Older Red", "3d ago · Red · workspace", "This week", now - 259_200),
            ],
            "more": false,
            "error": ""
        })]
    );
}

#[tokio::test]
async fn workspace_session_search_payload_adds_text_snippets() {
    let now = now();
    let data = vec![
        thread(
            "needle-title",
            json!("cli"),
            json!("Find the Needle"),
            "",
            now - 2 * 86_400,
        ),
        thread(
            "unrelated",
            json!("cli"),
            json!("Other"),
            "",
            now - 3 * 86_400,
        ),
        thread(
            "custom",
            json!({"custom": "my-tool"}),
            json!("Custom needle"),
            "",
            now - 5400,
        ),
    ];
    let mut harness = Harness::new();
    harness
        .command(list_request(SessionScope::Workspace, "needle", true))
        .await;
    harness
        .serve(|method, params| match method {
            "thread/list" => {
                assert_eq!(params["sourceKinds"], json!(["cli", "vscode", "appServer", "unknown"]));
                json!({"result": {"data": data.clone(), "nextCursor": null}})
            }
            "thread/search" => {
                assert_eq!(params["searchTerm"], "needle");
                json!({"result": {"data": [
                    {"thread": thread("workspace-hit", json!("vscode"), json!("Hit"), "", now - 10 * 86_400), "snippet": "  a matching\n needle "},
                    {"thread": thread("needle-title", json!("cli"), json!("Find the Needle"), "", now - 2 * 86_400), "snippet": "the needle itself"},
                    {"thread": thread("excluded", json!("cli"), json!("Excluded"), "", now), "snippet": "needle"}
                ], "nextCursor": null}})
            }
            other => panic!("unexpected {other}"),
        })
        .await;

    let summary =
        |id: &str, label: &str, snippet: &str, source: &str, detail: &str, group: &str, at: i64| {
            json!({
                "id": id, "cwd": "/workspace", "label": label, "detail": detail, "snippet": snippet,
                "source": source, "time_group": group, "is_red": false, "resumable": false,
                "updated_at": at
            })
        };
    assert_eq!(
        session_payloads(harness.events()),
        vec![json!({
            "sessions": [
                summary("custom", "Custom needle", "", "my-tool", "1h ago · my-tool · workspace", "Recent", now - 5400),
                summary("needle-title", "Find the Needle", "the needle itself", "Codex CLI", "2d ago · Codex CLI · workspace", "This week", now - 2 * 86_400),
                summary("workspace-hit", "Hit", "a matching needle", "VS Code", "10d ago · VS Code · workspace", "This month", now - 10 * 86_400),
            ],
            "more": false,
            "error": ""
        })]
    );
}

#[tokio::test]
async fn session_list_payload_reports_failures_and_partial_results() {
    let now = now();
    let mut harness = Harness::new();
    harness
        .command(list_request(SessionScope::All, "", false))
        .await;
    harness
        .serve(|_, _| json!({"error": {"code": -32000, "message": "state database is locked"}}))
        .await;
    harness
        .command(list_request(SessionScope::All, "", false))
        .await;
    harness
        .serve(|_, _| json!({"result": {"nextCursor": null}}))
        .await;
    harness
        .command(list_request(SessionScope::All, "", false))
        .await;
    harness
        .serve(|_, params| {
            if params["cursor"].is_null() {
                let mut other = thread("elsewhere", json!("cli"), json!("Elsewhere"), "", now - 30);
                other["cwd"] = json!("/other/project");
                json!({"result": {"data": [other], "nextCursor": "page-2"}})
            } else {
                json!({"error": {"code": -32000, "message": "stopped"}})
            }
        })
        .await;

    assert_eq!(
        session_payloads(harness.events()),
        vec![
            json!({"sessions": [], "error": "state database is locked"}),
            json!({"sessions": [], "error": "Codex returned an invalid session list"}),
            json!({
                "sessions": [{
                    "id": "elsewhere", "cwd": "/other/project", "label": "Elsewhere",
                    "detail": "just now · Codex CLI · project", "snippet": "", "source": "Codex CLI",
                    "time_group": "Recent", "is_red": false, "resumable": false, "updated_at": now - 30
                }],
                "more": true,
                "error": ""
            }),
        ]
    );
}

fn preview_command(request_id: i64) -> CodexCommand {
    CodexCommand::SessionPreview {
        request_id,
        session_id: "thread-1".into(),
    }
}

fn user_message(id: &str, parts: &[&str]) -> Value {
    json!({
        "id": id, "type": "userMessage",
        "content": parts.iter().map(|text| json!({"type": "text", "text": text, "text_elements": []})).collect::<Vec<_>>()
    })
}

fn agent_message(id: &str, text: &str) -> Value {
    json!({"id": id, "type": "agentMessage", "text": text, "phase": null})
}

#[tokio::test]
async fn session_preview_payload_shows_the_latest_exchange_without_red_markers() {
    let context =
        "Active editor context from file:///src/parser.rs:\n\n```text\nfn parse() {}\n```";
    let mut harness = Harness::new();
    harness.command(preview_command(1)).await;
    harness
        .serve(|method, params| {
            assert_eq!(method, "thread/turns/list");
            assert_eq!(params["itemsView"], "summary");
            json!({"result": {"data": [
                {"id": "t2", "status": "completed", "items": [
                    user_message("u2", &["Fix the parser", context]),
                    agent_message("a2", "  Fixed it.  ")
                ]},
                {"id": "t1", "status": "completed", "items": [
                    user_message("u1", &["Old question"]),
                    agent_message("a1", "Old answer")
                ]}
            ], "nextCursor": null}})
        })
        .await;
    harness.command(preview_command(2)).await;
    harness
        .serve(|_, _| {
            json!({"result": {"data": [{"id": "t", "items": [user_message(
                "u",
                &["Previous conversation (the last turn was interrupted):\nYou: earlier\n\nFollow-up:\nTry again\n\nActive editor context from file:///a.rs:\n\n```text\nx\n```"]
            )]}], "nextCursor": null}})
        })
        .await;
    harness.command(preview_command(3)).await;
    harness
        .serve(|_, _| json!({"result": {"data": [], "nextCursor": null}}))
        .await;
    harness.command(preview_command(4)).await;
    harness
        .serve(|_, _| json!({"error": {"code": -32000, "message": "thread is gone"}}))
        .await;

    assert_eq!(
        session_payloads(harness.events()),
        vec![
            json!({"session_id": "thread-1", "prompt": "Fix the parser", "response": "Fixed it.", "error": ""}),
            json!({"session_id": "thread-1", "prompt": "Try again", "response": "", "error": ""}),
            json!({"session_id": "thread-1", "prompt": "", "response": "", "error": ""}),
            json!({"session_id": "", "prompt": "", "response": "", "error": "thread is gone"}),
        ]
    );
}

#[tokio::test]
async fn session_preview_payload_falls_back_to_reading_the_whole_thread() {
    let thread = |id: &str| {
        json!({"result": {"thread": {"id": id, "turns": [{"id": "t", "items": [
            user_message("u", &["Question"]),
            agent_message("a", "Answer")
        ]}]}}})
    };
    let mut harness = Harness::new();
    harness.command(preview_command(1)).await;
    harness
        .serve(|method, params| match method {
            "thread/turns/list" => {
                json!({"error": {"code": -32601, "message": "method not found"}})
            }
            "thread/read" => {
                assert_eq!(params["includeTurns"], true);
                thread("thread-1")
            }
            other => panic!("unexpected {other}"),
        })
        .await;
    harness.command(preview_command(2)).await;
    harness
        .serve(|method, _| match method {
            "thread/turns/list" => {
                json!({"error": {"code": -32601, "message": "method not found"}})
            }
            _ => thread("another-thread"),
        })
        .await;

    assert_eq!(
        session_payloads(harness.events()),
        vec![
            json!({"session_id": "thread-1", "prompt": "Question", "response": "Answer", "error": ""}),
            json!({"session_id": "", "prompt": "", "response": "", "error": "Codex returned a preview for a different session"}),
        ]
    );
}

const ADD_A: &str = "00000000-0000-4000-8000-00000000000a";
const ADD_B: &str = "00000000-0000-4000-8000-00000000000b";
const ADD_C: &str = "00000000-0000-4000-8000-00000000000c";
const ADD_D: &str = "00000000-0000-4000-8000-00000000000d";

fn annotation_call(id: &str, tool: &str, arguments: Value, output: Value) -> Value {
    json!({
        "id": id, "type": "dynamicToolCall", "tool": tool, "arguments": arguments,
        "status": "completed", "success": true, "durationMs": 4,
        "contentItems": [{"type": "inputText", "text": output.to_string()}]
    })
}

/// Turns as `thread/turns/list` returns them: newest first.
fn restored_turns() -> Vec<Value> {
    let add_first = annotation_call(
        "call-add-1",
        "add_annotations",
        json!({"path": "src/parser.rs", "expected_revision": 3, "annotations": []}),
        json!({"ok": true, "path": "src/parser.rs", "annotations": [
            {"id": ADD_A, "start_line": 1, "end_line": 2, "message": "First note"},
            {"id": ADD_B, "start_line": 4, "end_line": 4, "message": "Second note"}
        ]}),
    );
    let dismiss = annotation_call(
        "call-dismiss",
        "dismiss_annotations",
        json!({"annotation_ids": [ADD_A]}),
        json!({"ok": true, "dismissed": [ADD_A]}),
    );
    let mut failed = annotation_call(
        "call-failed",
        "add_annotations",
        json!({"path": "ignored.rs"}),
        json!({"ok": true, "path": "ignored.rs", "annotations": [
            {"id": ADD_D, "start_line": 1, "end_line": 1, "message": "Never applied"}
        ]}),
    );
    failed["success"] = json!(false);
    let rejected = annotation_call(
        "call-rejected",
        "add_annotations",
        json!({"path": "rejected.rs"}),
        json!({"ok": false, "path": "rejected.rs", "annotations": [
            {"id": ADD_D, "start_line": 1, "end_line": 1, "message": "Rejected"}
        ]}),
    );
    let add_later = annotation_call(
        "call-add-2",
        "add_annotations",
        json!({"path": "src/lib.rs"}),
        json!({"ok": true, "annotations": [
            {"id": ADD_A, "start_line": 9, "end_line": 9, "message": "Re-added after its dismissal"},
            {"id": ADD_C, "start_line": 7, "message": "No end line"},
            {"id": ADD_D, "message": "No start line"},
            {"start_line": 3, "message": "No id"}
        ]}),
    );
    let recap = "Previous conversation (the last turn was interrupted):\nYou: earlier\nAgent: partial\n\nFollow-up:\nAdd notes to the parser\n\nActive editor context from file:///src/parser.rs:\n\n```text\nfn parse() {}\n```";
    vec![
        json!({"items": [
            user_message("u4", &["Hello"]),
            json!({"id": "other", "type": "commandExecution", "aggregatedOutput": "ignored"})
        ]}),
        json!({"id": "turn-3", "status": "completed", "items": [
            json!({"id": "u3", "type": "userMessage", "content": [{"type": "text", "text": "Active editor context from file:///a.rs:\n\nctx"}]}),
            add_later,
            json!({"type": "agentMessage", "text": "Done."})
        ]}),
        json!({"id": "turn-2", "status": "completed", "items": [
            json!({"id": "user-2", "type": "userMessage", "content": [
                {"type": "image", "url": "file:///x.png"},
                {"type": "text", "text": "Dismiss the first note"}
            ]}),
            dismiss, failed, rejected,
            agent_message("agent-2", "")
        ]}),
        json!({"id": "turn-1", "status": "completed", "items": [
            user_message("user-1", &[recap]),
            json!({"id": "reasoning-1", "type": "reasoning", "summary": [], "content": []}),
            add_first,
            agent_message("agent-1a", "Added notes."),
            agent_message("agent-1b", "Anything else?")
        ]}),
    ]
}

fn expected_restored_items() -> Value {
    json!([
        {"id": "user-1", "turn_id": "turn-1", "role": "user", "text": "Add notes to the parser"},
        {"id": "agent-1a", "turn_id": "turn-1", "role": "agent", "text": "Added notes.\n\nAnything else?"},
        {"id": "user-2", "turn_id": "turn-2", "role": "user", "text": "Dismiss the first note"},
        {"id": "u3", "turn_id": "turn-3", "role": "user", "text": "Active editor context from file:///a.rs:\n\nctx"},
        {"id": "codex-agent-turn-3", "turn_id": "turn-3", "role": "agent", "text": "Done."},
        {"id": "u4", "role": "user", "text": "Hello"}
    ])
}

async fn restore(history: impl Fn(&str) -> Value) -> (Vec<CodexEvent>, Harness) {
    let mut harness = Harness::new();
    harness
        .command(CodexCommand::ResumeSession {
            cwd: PathBuf::from("/workspace"),
            session_id: "thread-red".into(),
        })
        .await;
    harness
        .serve(|method, params| match method {
            "config/read" => json!({"result": {"config": {"mcp_servers": {}}, "origins": {}}}),
            "configRequirements/read" => json!({"result": {"requirements": null}}),
            "thread/resume" => json!({"result": {
                "thread": {"id": "thread-red", "ephemeral": false, "turns": []},
                "model": "gpt-5.1-codex", "modelProvider": "openai", "reasoningEffort": "high"
            }}),
            "thread/turns/list" => history(params["itemsView"].as_str().unwrap()),
            other => panic!("unexpected {other}"),
        })
        .await;
    let events = harness.events();
    (events, harness)
}

fn restored_thread(events: Vec<CodexEvent>) -> (String, CodexEvent) {
    let mut restored = None;
    for event in events {
        match event {
            CodexEvent::SessionRestored { ref session_id, .. } => {
                restored = Some((session_id.clone(), event));
            }
            CodexEvent::SessionModelChanged { .. } => {}
            other => panic!("unexpected event {other:?}"),
        }
    }
    restored.expect("session was restored")
}

#[tokio::test]
async fn restored_conversation_snapshot_reconciles_transcript_and_annotations() {
    let (events, _) = restore(|view| {
        assert_eq!(view, "full");
        json!({"result": {"data": restored_turns(), "nextCursor": null}})
    })
    .await;
    let model_changed = events
        .iter()
        .find(|event| matches!(event, CodexEvent::SessionModelChanged { .. }))
        .cloned()
        .unwrap();
    assert_eq!(
        agent_event_payload(model_changed),
        (
            "agent:model_changed",
            json!({"session_id": "thread-red", "model_info": {
                "model": "gpt-5.1-codex", "provider": "openai", "effort": "high"
            }})
        )
    );
    let (session_id, CodexEvent::SessionRestored { thread, .. }) = restored_thread(events) else {
        unreachable!()
    };
    assert_eq!(session_id, "thread-red");

    let mut manager = agent_manager::AgentManager::default();
    let snapshot = manager
        .reconcile_conversation(&session_id, Path::new("/workspace"), &thread)
        .cloned()
        .unwrap();

    assert_eq!(
        plugin_json(serde_json::to_value(snapshot).unwrap()),
        json!({
            "thread_id": "thread-red",
            "cwd": "/workspace",
            "tool_contract_version": 1,
            "items": expected_restored_items(),
            "annotations": [
                {"id": ADD_B, "session_id": "thread-red", "turn_id": "turn-1", "path": "src/parser.rs", "start_line": 4, "end_line": 4, "message": "Second note"},
                {"id": ADD_A, "session_id": "thread-red", "turn_id": "turn-3", "path": "src/lib.rs", "start_line": 9, "end_line": 9, "message": "Re-added after its dismissal"},
                {"id": ADD_C, "session_id": "thread-red", "turn_id": "turn-3", "path": "src/lib.rs", "start_line": 7, "end_line": 7, "message": "No end line"}
            ]
        })
    );
}

#[tokio::test]
async fn truncated_history_keeps_saved_annotations_and_only_fills_empty_records() {
    let (events, _) = restore(|view| match view {
        "full" => json!({"error": {"code": -32099, "message": "frame too large"}}),
        "summary" => json!({"result": {"data": restored_turns(), "nextCursor": null}}),
        other => panic!("unexpected {other}"),
    })
    .await;
    let (_, CodexEvent::SessionRestored { thread, .. }) = restored_thread(events) else {
        unreachable!()
    };

    let mut saved =
        crate::agent_conversation::AgentConversationSnapshot::new("thread-red", "/workspace");
    saved.append_user("local-turn", "Local prompt");
    saved
        .annotations
        .push(crate::agent_conversation::AgentAnnotationRecord {
            id: ADD_B.to_string(),
            session_id: "thread-red".to_string(),
            turn_id: "turn-1".to_string(),
            path: "renamed.rs".to_string(),
            start_line: 8,
            end_line: 9,
            message: "Saved note".to_string(),
            expected_fingerprint: Some([7; 32]),
        });
    let kept = saved.reconciled_with_thread(&thread);
    let filled =
        crate::agent_conversation::AgentConversationSnapshot::new("thread-red", "/workspace")
            .reconciled_with_thread(&thread);

    assert_eq!(
        serde_json::to_value(kept).unwrap(),
        json!({
            "thread_id": "thread-red",
            "cwd": "/workspace",
            "tool_contract_version": 1,
            "items": expected_restored_items(),
            "annotations": [{
                "id": ADD_B, "session_id": "thread-red", "turn_id": "turn-1", "path": "renamed.rs",
                "start_line": 8, "end_line": 9, "message": "Saved note",
                "expected_fingerprint": vec![7; 32]
            }]
        })
    );
    assert_eq!(
        serde_json::to_value(filled).unwrap()["annotations"],
        json!([
            {"id": ADD_B, "session_id": "thread-red", "turn_id": "turn-1", "path": "src/parser.rs", "start_line": 4, "end_line": 4, "message": "Second note"},
            {"id": ADD_A, "session_id": "thread-red", "turn_id": "turn-3", "path": "src/lib.rs", "start_line": 9, "end_line": 9, "message": "Re-added after its dismissal"},
            {"id": ADD_C, "session_id": "thread-red", "turn_id": "turn-3", "path": "src/lib.rs", "start_line": 7, "end_line": 7, "message": "No end line"}
        ])
    );
}

#[tokio::test]
async fn saved_annotations_win_over_replayed_ones_for_the_same_id() {
    let (events, _) =
        restore(|_| json!({"result": {"data": restored_turns(), "nextCursor": null}})).await;
    let (_, CodexEvent::SessionRestored { thread, .. }) = restored_thread(events) else {
        unreachable!()
    };
    let mut saved =
        crate::agent_conversation::AgentConversationSnapshot::new("thread-red", "/workspace");
    saved
        .annotations
        .push(crate::agent_conversation::AgentAnnotationRecord {
            id: ADD_B.to_string(),
            session_id: "thread-red".to_string(),
            turn_id: "turn-1".to_string(),
            path: "renamed.rs".to_string(),
            start_line: 8,
            end_line: 9,
            message: "Saved note".to_string(),
            expected_fingerprint: Some([7; 32]),
        });

    assert_eq!(
        serde_json::to_value(saved.reconciled_with_thread(&thread)).unwrap()["annotations"],
        json!([
            {"id": ADD_B, "session_id": "thread-red", "turn_id": "turn-1", "path": "renamed.rs", "start_line": 8, "end_line": 9, "message": "Saved note", "expected_fingerprint": vec![7; 32]},
            {"id": ADD_A, "session_id": "thread-red", "turn_id": "turn-3", "path": "src/lib.rs", "start_line": 9, "end_line": 9, "message": "Re-added after its dismissal"},
            {"id": ADD_C, "session_id": "thread-red", "turn_id": "turn-3", "path": "src/lib.rs", "start_line": 7, "end_line": 7, "message": "No end line"}
        ])
    );
}

#[tokio::test]
async fn threads_without_messages_keep_the_saved_transcript() {
    let (events, _) = restore(|_| json!({"result": {"data": [], "nextCursor": null}})).await;
    let (_, CodexEvent::SessionRestored { thread, .. }) = restored_thread(events) else {
        unreachable!()
    };
    let mut saved =
        crate::agent_conversation::AgentConversationSnapshot::new("thread-red", "/workspace");
    saved.append_user("turn", "Saved prompt");

    assert_eq!(
        serde_json::to_value(saved.reconciled_with_thread(&thread)).unwrap(),
        json!({
            "thread_id": "thread-red",
            "cwd": "/workspace",
            "tool_contract_version": 1,
            "items": [{"id": "red-user-turn", "turn_id": "turn", "role": "user", "text": "Saved prompt"}],
            "annotations": []
        })
    );
}

#[tokio::test]
async fn continuing_a_foreign_session_restores_its_history_into_the_new_thread() {
    let mut harness = Harness::new();
    harness
        .command(CodexCommand::ContinueSession {
            cwd: PathBuf::from("/workspace"),
            session_id: "foreign".into(),
        })
        .await;
    harness
        .serve(|method, params| match method {
            "thread/turns/list" => {
                assert_eq!(params["itemsView"], "summary");
                json!({"result": {"data": [
                    {"id": "new-turn", "items": [
                        user_message("new-user", &["Later question", "Active editor context from file:///b.rs:\n\nctx"]),
                        agent_message("new-agent", "Later answer")
                    ]},
                    {"id": "old-turn", "items": [
                        user_message("old-user", &["Earlier question"]),
                        agent_message("old-agent", "Earlier answer")
                    ]}
                ], "nextCursor": null}})
            }
            "config/read" => json!({"result": {"config": {"mcp_servers": {}}}}),
            "configRequirements/read" => json!({"result": {"requirements": null}}),
            "thread/start" => json!({"result": {"thread": {"id": "red-fork", "turns": []}}}),
            "thread/inject_items" => json!({"result": {}}),
            other => panic!("unexpected {other}"),
        })
        .await;

    let (session_id, CodexEvent::SessionRestored { thread, .. }) =
        restored_thread(harness.events())
    else {
        unreachable!()
    };
    assert_eq!(session_id, "red-fork");
    let snapshot =
        crate::agent_conversation::AgentConversationSnapshot::new("red-fork", "/workspace")
            .reconciled_with_thread(&thread);
    assert_eq!(
        serde_json::to_value(snapshot).unwrap(),
        json!({
            "thread_id": "red-fork",
            "cwd": "/workspace",
            "tool_contract_version": 1,
            "items": [
                {"id": "old-user", "turn_id": "old-turn", "role": "user", "text": "Earlier question"},
                {"id": "old-agent", "turn_id": "old-turn", "role": "agent", "text": "Earlier answer"},
                {"id": "new-user", "turn_id": "new-turn", "role": "user", "text": "Later question"},
                {"id": "new-agent", "turn_id": "new-turn", "role": "agent", "text": "Later answer"}
            ],
            "annotations": []
        })
    );
}

fn item_notification(method: &str, turn: &str, item: Value) -> Value {
    json!({"method": method, "params": {"threadId": "agent", "turnId": turn, "item": item}})
}

fn tool_item(id: &str, tool: &str, arguments: Value) -> Value {
    json!({"id": id, "type": "dynamicToolCall", "tool": tool, "arguments": arguments})
}

#[tokio::test]
async fn agent_activity_payloads_cover_every_shape_the_worker_emits() {
    let mut harness = Harness::new();
    harness.add_agent_session("agent", "/workspace", Some("turn"), None);
    let long_path = format!("/workspace/{}/{}/main.rs", "a".repeat(30), "b".repeat(30));
    let finished = |mut item: Value, status: &str| {
        item["status"] = json!(status);
        item
    };
    let mut write_failure = tool_item(
        "call-write",
        "write_file",
        json!({"path": "src/lib.rs", "content": "private"}),
    );
    write_failure["status"] = json!("completed");
    write_failure["success"] = json!(false);
    write_failure["contentItems"] =
        json!([{"type": "inputText", "text": "{\"ok\":false,\n\"status\":\"stale_revision\"}"}]);
    let mut read_done = finished(
        tool_item(
            "call-read",
            "read_file",
            json!({"path": "/workspace/src/main.rs"}),
        ),
        "completed",
    );
    read_done["success"] = json!(true);
    read_done["durationMs"] = json!(12);
    let messages = vec![
        item_notification(
            "item/started",
            "turn",
            json!({"id": "r1", "type": "reasoning", "summary": [], "content": []}),
        ),
        item_notification(
            "item/completed",
            "turn",
            json!({"id": "r1", "type": "reasoning", "summary": [], "content": []}),
        ),
        item_notification(
            "item/started",
            "turn",
            tool_item(
                "call-read",
                "read_file",
                json!({"path": "/workspace/src/main.rs"}),
            ),
        ),
        item_notification("item/completed", "turn", read_done),
        item_notification("item/completed", "turn", write_failure),
        item_notification(
            "item/started",
            "turn",
            tool_item("call-search", "search_files", json!({"query": "TODO"})),
        ),
        item_notification(
            "item/started",
            "turn",
            tool_item("call-list", "list_files", json!({})),
        ),
        item_notification(
            "item/started",
            "turn",
            tool_item(
                "call-dir",
                "create_directory",
                json!({"path": "/workspace/go/"}),
            ),
        ),
        item_notification(
            "item/completed",
            "turn",
            finished(
                tool_item(
                    "call-diag",
                    "lsp_diagnostics",
                    json!({"scope": "open_buffers"}),
                ),
                "cancelled",
            ),
        ),
        item_notification(
            "item/completed",
            "turn",
            finished(
                tool_item("call-edit", "apply_edits", json!({"path": "src/main.rs"})),
                "declined",
            ),
        ),
        item_notification(
            "item/completed",
            "turn",
            finished(
                tool_item("call-fail", "lsp_apply_edit", json!({"plan_id": "p"})),
                "failed",
            ),
        ),
        item_notification(
            "item/started",
            "turn",
            tool_item("call-custom", "custom_tool", json!({})),
        ),
        item_notification(
            "item/started",
            "turn",
            tool_item("call-long", "read_file", json!({"path": long_path})),
        ),
        item_notification(
            "item/completed",
            "turn",
            json!({"id": "m", "type": "agentMessage", "text": "hidden"}),
        ),
        item_notification(
            "item/started",
            "stale-turn",
            tool_item("call-stale", "read_file", json!({"path": "x"})),
        ),
        item_notification(
            "item/started",
            "turn",
            tool_item("", "read_file", json!({"path": "x"})),
        ),
    ];
    for message in messages {
        harness.message(message).await;
    }

    let update = |value: Value| json!({"session_id": "agent", "update": value});
    let payloads = harness
        .events()
        .into_iter()
        .map(agent_event_payload)
        .collect::<Vec<_>>();
    assert_eq!(
        payloads,
        vec![
            (
                "agent:activity",
                update(json!({"session_update": "agent_thought_chunk"}))
            ),
            (
                "agent:activity",
                update(json!({
                    "session_update": "tool_call", "tool_call_id": "call-read", "kind": "read",
                    "title": "Reading src/main.rs", "full_title": "Reading /workspace/src/main.rs",
                    "status": "in_progress", "detail": ""
                }))
            ),
            (
                "agent:activity",
                update(json!({
                    "session_update": "tool_call_update", "tool_call_id": "call-read", "kind": "read",
                    "title": "Reading src/main.rs", "full_title": "Reading /workspace/src/main.rs",
                    "status": "completed", "detail": "12 ms"
                }))
            ),
            (
                "agent:activity",
                update(json!({
                    "session_update": "tool_call_update", "tool_call_id": "call-write", "kind": "edit",
                    "title": "Editing src/lib.rs", "full_title": "Editing src/lib.rs",
                    "status": "failed", "detail": "{\"ok\":false, \"status\":\"stale_revision\"}"
                }))
            ),
            (
                "agent:activity",
                update(json!({
                    "session_update": "tool_call", "tool_call_id": "call-search", "kind": "search",
                    "title": "Searching for TODO", "full_title": "Searching for TODO",
                    "status": "in_progress", "detail": ""
                }))
            ),
            (
                "agent:activity",
                update(json!({
                    "session_update": "tool_call", "tool_call_id": "call-list", "kind": "list",
                    "title": "Listing workspace files", "full_title": "Listing workspace files",
                    "status": "in_progress", "detail": ""
                }))
            ),
            (
                "agent:activity",
                update(json!({
                    "session_update": "tool_call", "tool_call_id": "call-dir", "kind": "create",
                    "title": "Creating go/", "full_title": "Creating /workspace/go/",
                    "status": "in_progress", "detail": ""
                }))
            ),
            (
                "agent:activity",
                update(json!({
                    "session_update": "tool_call_update", "tool_call_id": "call-diag", "kind": "diagnostics",
                    "title": "Checking diagnostics for open buffers",
                    "full_title": "Checking diagnostics for open buffers",
                    "status": "cancelled", "detail": ""
                }))
            ),
            (
                "agent:activity",
                update(json!({
                    "session_update": "tool_call_update", "tool_call_id": "call-edit", "kind": "edit",
                    "title": "Editing src/main.rs", "full_title": "Editing src/main.rs",
                    "status": "cancelled", "detail": ""
                }))
            ),
            (
                "agent:activity",
                update(json!({
                    "session_update": "tool_call_update", "tool_call_id": "call-fail", "kind": "edit",
                    "title": "Applying language-server edits", "full_title": "Applying language-server edits",
                    "status": "failed", "detail": "Tool failed"
                }))
            ),
            (
                "agent:activity",
                update(json!({
                    "session_update": "tool_call", "tool_call_id": "call-custom", "kind": "tool",
                    "title": "Running custom_tool", "full_title": "Running custom_tool",
                    "status": "in_progress", "detail": ""
                }))
            ),
            (
                "agent:activity",
                update(json!({
                    "session_update": "tool_call", "tool_call_id": "call-long", "kind": "read",
                    "title": format!("Reading …{}/{}/main.rs", "a".repeat(10), "b".repeat(30)),
                    "full_title": format!("Reading /workspace/{}/{}/main.rs", "a".repeat(30), "b".repeat(30)),
                    "status": "in_progress", "detail": ""
                }))
            ),
            (
                "agent:message_completed",
                json!({"session_id": "agent", "text": "hidden"})
            ),
        ]
    );
}

#[tokio::test]
async fn turn_end_payloads_carry_the_app_server_status() {
    let mut harness = Harness::new();
    for (index, status) in [Some("completed"), Some("interrupted"), Some("failed"), None]
        .into_iter()
        .enumerate()
    {
        let turn = format!("turn-{index}");
        harness.add_agent_session("agent", "/workspace", Some(&turn), None);
        let mut turn_body = json!({"id": turn});
        if let Some(status) = status {
            turn_body["status"] = json!(status);
        }
        harness
            .message(json!({"method": "turn/completed", "params": {
                "threadId": "agent", "turn": turn_body
            }}))
            .await;
    }
    let mut payloads = harness
        .events()
        .into_iter()
        .map(agent_event_payload)
        .collect::<Vec<_>>();
    payloads.extend(
        [
            CodexEvent::Cancelled {
                session_id: "agent".into(),
            },
            CodexEvent::CancelFailed {
                session_id: "agent".into(),
                message: "no active turn to interrupt".into(),
            },
            CodexEvent::Failed {
                session_id: None,
                message: "Codex app-server exited".into(),
            },
            CodexEvent::Failed {
                session_id: Some("agent".into()),
                message: "Codex could not start the turn".into(),
            },
        ]
        .into_iter()
        .map(agent_event_payload),
    );

    assert_eq!(
        payloads,
        vec![
            (
                "agent:completed",
                json!({"session_id": "agent", "stop_reason": "completed"})
            ),
            (
                "agent:completed",
                json!({"session_id": "agent", "stop_reason": "interrupted"})
            ),
            (
                "agent:completed",
                json!({"session_id": "agent", "stop_reason": "failed"})
            ),
            (
                "agent:completed",
                json!({"session_id": "agent", "stop_reason": "completed"})
            ),
            ("agent:cancelled", json!({"session_id": "agent"})),
            (
                "agent:cancel_failed",
                json!({"session_id": "agent", "message": "no active turn to interrupt"})
            ),
            (
                "agent:error",
                json!({"session_id": null, "message": "Codex app-server exited"})
            ),
            (
                "agent:error",
                json!({"session_id": "agent", "message": "Codex could not start the turn"})
            ),
        ]
    );
}
