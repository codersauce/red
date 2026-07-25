#![cfg(unix)]

use std::{
    num::NonZeroUsize,
    os::unix::fs::PermissionsExt as _,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use red::{
    agent_tools::EditorToolRequest,
    codex::{
        start_codex, CodexBridge, CodexCommand, CodexEvent, CodexExecutionMode, CodexProcessSpec,
        CodexToolHost,
    },
};
use serde_json::{json, Value};

#[derive(Clone)]
struct RecordingHost {
    writes: Arc<Mutex<Vec<(String, String)>>>,
}

#[async_trait]
impl CodexToolHost for RecordingHost {
    async fn read_file(&mut self, _: &str, path: &str) -> anyhow::Result<Value> {
        Ok(json!({"content": format!("unsaved:{path}")}))
    }

    async fn write_file(&mut self, _: &str, path: &str, content: String) -> anyhow::Result<Value> {
        self.writes
            .lock()
            .unwrap()
            .push((path.to_string(), content));
        Ok(json!({}))
    }

    async fn editor_tool(&mut self, _: EditorToolRequest) -> anyhow::Result<Value> {
        Ok(json!({}))
    }
}

fn mock_codex(directory: &std::path::Path) -> std::path::PathBuf {
    let path = directory.join("codex");
    std::fs::write(
        &path,
        r#"#!/usr/bin/env python3
import json, os, sys

assert "features.hooks=false" not in sys.argv
assert "features.codex_hooks=false" not in sys.argv
mode = os.environ.get("RED_MOCK_MODE", "review")
scenario = os.environ.get("RED_MOCK_SCENARIO", "write")
resume_attempts = 0
config_reads = 0

def send(value):
    print(json.dumps(value), flush=True)

def complete(status="completed", thread_id="thread-red"):
    send({"method": "turn/completed", "params": {
        "threadId": thread_id,
        "turn": {"id": "turn-red", "status": status}
    }})

for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    ident = message.get("id")
    if method == "initialize":
        send({"id": ident, "result": {"userAgent": "mock"}})
    elif method == "initialized":
        pass
    elif method == "account/read":
        send({"id": ident, "result": {"account": {"type": "chatgpt"}, "requiresOpenaiAuth": True}})
    elif method == "config/read":
        config_reads += 1
        send({"id": ident, "result": {"config": {"mcp_servers": {}}, "origins": {}}})
    elif method == "configRequirements/read":
        assert mode == "review"
        requirements = json.loads(os.environ.get("RED_MOCK_REQUIREMENTS", "null"))
        send({"id": ident, "result": {"requirements": requirements}})
    elif method == "thread/start":
        if os.environ.get("RED_MOCK_EXPECT_RECOVERY") == "true":
            assert resume_attempts == 1
            assert config_reads == 1
        assert os.environ.get("RED_MOCK_FORBID_RECOVERY") != "true"
        params = message["params"]
        assert len(params["dynamicTools"]) == 9
        if mode == "review":
            assert params["sandbox"] == "read-only"
            assert params["approvalPolicy"] == "never"
            expected_hooks = os.environ.get("RED_MOCK_EXPECT_HOOKS") == "true"
            assert params["config"]["features"]["hooks"] is expected_hooks
            assert "codex_hooks" not in params["config"]["features"]
        else:
            for overridden in ["sandbox", "approvalPolicy", "environments", "config", "baseInstructions"]:
                assert overridden not in params, overridden
            for disabled in ["features.apps=false", "features.connectors=false", "features.plugins=false", "features.remote_plugin=false"]:
                assert disabled not in sys.argv, disabled
            assert bool(params.get("ephemeral", False)) is (
                os.environ.get("RED_MOCK_EXPECT_EPHEMERAL") == "true"
            )
        send({"id": ident, "result": {
            "thread": {"id": "thread-red"}, "model": "mock-default-model"
        }})
    elif method == "thread/resume":
        resume_attempts += 1
        thread_id = message["params"]["threadId"]
        if thread_id == "missing-red":
            send({"id": ident, "error": {
                "code": -32000, "message": "persisted thread was not found"
            }})
        elif thread_id == "mismatched-red":
            send({"id": ident, "result": {
                "thread": {"id": "unexpected-red"}, "model": "mock-default-model"
            }})
        else:
            assert thread_id == "persisted-red"
            send({"id": ident, "result": {
                "thread": {"id": "persisted-red"}, "model": "mock-default-model"
            }})
    elif method == "model/list":
        send({"id": ident, "result": {"data": [{
            "id": "mock-selected-model",
            "model": "mock-selected-model",
            "displayName": "Mock Selected Model",
            "supportedReasoningEfforts": [{"reasoningEffort": "high"}],
            "defaultReasoningEffort": "medium"
        }], "nextCursor": None}})
    elif method == "thread/list":
        assert message["params"]["archived"] is False
        assert message["params"]["limit"] == 50
        send({"id": ident, "result": {"data": [{
            "id": "persisted-red", "preview": "Restored conversation"
        }], "nextCursor": None}})
    elif method == "turn/start":
        params = message["params"]
        if mode == "review":
            assert params["approvalPolicy"] == "never"
            assert params["sandboxPolicy"] == {"type": "readOnly"}
        else:
            assert "approvalPolicy" not in params
            assert "sandboxPolicy" not in params
        if scenario == "write":
            text = params["input"][0]["text"]
            assert "Active editor context from red-buffer://active:" in text
            assert "unsaved editor text" in text
        expected_model = os.environ.get("RED_MOCK_EXPECT_MODEL")
        if expected_model is not None:
            assert params["model"] == expected_model
        expected_effort = os.environ.get("RED_MOCK_EXPECT_EFFORT")
        if expected_effort is not None:
            assert params["effort"] == expected_effort
        send({"id": ident, "result": {"turn": {"id": "turn-red"}}})
        send({"method": "turn/started", "params": {
            "threadId": "thread-red", "turn": {"id": "turn-red"}
        }})
        if scenario == "write":
            send({"method": "item/agentMessage/delta", "params": {
                "threadId": "thread-red", "turnId": "turn-red", "delta": "working"
            }})
            send({"id": "tool-write", "method": "item/tool/call", "params": {
                "threadId": "thread-red", "turnId": "turn-red",
                "tool": "write_file",
                "arguments": {"path": "src/main.rs", "content": "proposed\n"}
            }})
        elif scenario == "approval":
            send({"id": "approve-command", "method": "item/commandExecution/requestApproval", "params": {
                "threadId": "thread-red", "turnId": "turn-red", "itemId": "command-1",
                "command": "cargo test", "availableDecisions": ["accept", "decline"]
            }})
        elif scenario == "activity":
            item = {"id": "command-1", "type": "commandExecution", "command": "cargo test"}
            send({"method": "item/started", "params": {
                "threadId": "thread-red", "turnId": "turn-red", "item": item
            }})
            send({"method": "item/reasoning/summaryTextDelta", "params": {
                "threadId": "thread-red", "turnId": "turn-red",
                "itemId": "reasoning-1", "delta": "public reasoning summary"
            }})
            send({"method": "item/reasoning/textDelta", "params": {
                "threadId": "thread-red", "turnId": "turn-red",
                "itemId": "reasoning-1", "delta": "private reasoning must stay hidden"
            }})
            send({"method": "item/completed", "params": {
                "threadId": "thread-red", "turnId": "turn-red",
                "item": {**item, "status": "completed"}
            }})
            send({"method": "item/agentMessage/delta", "params": {
                "threadId": "thread-red", "turnId": "turn-red", "delta": "working"
            }})
            complete()
    elif method == "turn/steer":
        assert message["params"]["expectedTurnId"] == "turn-red"
        send({"id": ident, "result": {"turnId": "turn-red"}})
    elif method == "turn/interrupt":
        assert message["params"]["turnId"] == "turn-red"
        send({"id": ident, "result": {}})
        complete("interrupted")
    elif ident == "tool-write":
        assert message["result"]["success"] is True
        complete()
    elif ident == "approve-command":
        assert message["result"]["decision"] == os.environ.get("RED_MOCK_EXPECT_DECISION", "accept")
        complete()
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}

async fn next_event(bridge: &mut CodexBridge) -> CodexEvent {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some(event) = bridge.try_recv() {
                return event;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("mock app-server did not produce an event")
}

fn recording_host() -> RecordingHost {
    RecordingHost {
        writes: Arc::new(Mutex::new(Vec::new())),
    }
}

async fn create_session(bridge: &mut CodexBridge, directory: &std::path::Path) -> String {
    bridge
        .send(CodexCommand::NewSession {
            cwd: directory.to_path_buf(),
        })
        .await
        .unwrap();
    match next_event(bridge).await {
        CodexEvent::SessionCreated { session_id } => session_id,
        CodexEvent::Failed { message, .. } => panic!("mock session failed: {message}"),
        other => panic!("unexpected session event: {other:?}"),
    }
}

fn mock_spec(
    codex: std::path::PathBuf,
    directory: &std::path::Path,
    mode: CodexExecutionMode,
    scenario: &str,
) -> CodexProcessSpec {
    let mut spec = CodexProcessSpec::new(codex, directory).execution_mode(mode);
    spec.environment.insert(
        "RED_MOCK_MODE".into(),
        match mode {
            CodexExecutionMode::Native => "native",
            CodexExecutionMode::ReviewSafe => "review",
        }
        .into(),
    );
    spec.environment
        .insert("RED_MOCK_SCENARIO".into(), scenario.into());
    spec
}

#[tokio::test]
async fn direct_app_server_streams_and_routes_writes_to_the_host() {
    let directory = tempfile::tempdir().unwrap();
    let codex = mock_codex(directory.path());
    let writes = Arc::new(Mutex::new(Vec::new()));
    let host = RecordingHost {
        writes: Arc::clone(&writes),
    };
    let (mut bridge, task) = start_codex(
        mock_spec(
            codex,
            directory.path(),
            CodexExecutionMode::ReviewSafe,
            "write",
        ),
        host,
        NonZeroUsize::new(32).unwrap(),
    )
    .unwrap();

    bridge
        .send(CodexCommand::NewSession {
            cwd: directory.path().to_path_buf(),
        })
        .await
        .unwrap();
    let session_id = loop {
        if let Some(CodexEvent::SessionCreated { session_id }) = bridge.try_recv() {
            break session_id;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    assert_eq!(session_id, "thread-red");

    bridge
        .send(CodexCommand::PromptWithContext {
            session_id,
            text: "make a proposal".to_string(),
            uri: "red-buffer://active".to_string(),
            context: "unsaved editor text".to_string(),
        })
        .await
        .unwrap();
    let mut streamed = String::new();
    loop {
        match bridge.try_recv() {
            Some(CodexEvent::Update { text, .. }) => streamed.push_str(&text),
            Some(CodexEvent::Completed { stop_reason, .. }) => {
                assert_eq!(stop_reason, "completed");
                break;
            }
            Some(CodexEvent::Failed { message, .. }) => panic!("{message}"),
            _ => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
        }
    }
    assert_eq!(streamed, "working");
    assert_eq!(
        *writes.lock().unwrap(),
        vec![("src/main.rs".to_string(), "proposed\n".to_string())]
    );

    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn direct_app_server_starts_with_required_hooks() {
    let directory = tempfile::tempdir().unwrap();
    let codex = mock_codex(directory.path());
    let host = RecordingHost {
        writes: Arc::new(Mutex::new(Vec::new())),
    };
    let mut spec = mock_spec(
        codex,
        directory.path(),
        CodexExecutionMode::ReviewSafe,
        "write",
    );
    spec.environment.insert(
        "RED_MOCK_REQUIREMENTS".into(),
        json!({
            "allowManagedHooksOnly": null,
            "featureRequirements": {"hooks": true}
        })
        .to_string()
        .into(),
    );
    spec.environment
        .insert("RED_MOCK_EXPECT_HOOKS".into(), "true".into());
    let (mut bridge, task) = start_codex(spec, host, NonZeroUsize::new(32).unwrap()).unwrap();

    bridge
        .send(CodexCommand::NewSession {
            cwd: directory.path().to_path_buf(),
        })
        .await
        .unwrap();
    let event = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(event) = bridge.try_recv() {
                break event;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    assert!(
        matches!(event, CodexEvent::SessionCreated { session_id } if session_id == "thread-red")
    );
    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn native_app_server_inherits_codex_policy_and_persists_threads() {
    let directory = tempfile::tempdir().unwrap();
    let codex = mock_codex(directory.path());
    let mut spec = CodexProcessSpec::new(codex, directory.path());
    assert_eq!(spec.execution_mode, CodexExecutionMode::Native);
    assert!(spec.persistent_threads);
    spec.environment
        .insert("RED_MOCK_MODE".into(), "native".into());
    spec.environment
        .insert("RED_MOCK_SCENARIO".into(), "idle".into());
    let (mut bridge, task) =
        start_codex(spec, recording_host(), NonZeroUsize::new(32).unwrap()).unwrap();

    assert_eq!(
        create_session(&mut bridge, directory.path()).await,
        "thread-red"
    );

    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn native_app_server_can_explicitly_request_ephemeral_threads() {
    let directory = tempfile::tempdir().unwrap();
    let codex = mock_codex(directory.path());
    let mut spec = mock_spec(codex, directory.path(), CodexExecutionMode::Native, "idle")
        .persistent_threads(false);
    spec.environment
        .insert("RED_MOCK_EXPECT_EPHEMERAL".into(), "true".into());
    let (mut bridge, task) =
        start_codex(spec, recording_host(), NonZeroUsize::new(32).unwrap()).unwrap();

    assert_eq!(
        create_session(&mut bridge, directory.path()).await,
        "thread-red"
    );

    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn native_app_server_surfaces_and_resolves_explicit_command_approvals() {
    let directory = tempfile::tempdir().unwrap();
    let codex = mock_codex(directory.path());
    let spec = mock_spec(
        codex,
        directory.path(),
        CodexExecutionMode::Native,
        "approval",
    );
    let (mut bridge, task) =
        start_codex(spec, recording_host(), NonZeroUsize::new(32).unwrap()).unwrap();
    let session_id = create_session(&mut bridge, directory.path()).await;
    bridge
        .send(CodexCommand::Prompt {
            session_id: session_id.clone(),
            text: "run the focused tests".to_string(),
        })
        .await
        .unwrap();

    let mut approved = false;
    loop {
        match next_event(&mut bridge).await {
            CodexEvent::PermissionRequested {
                request_id,
                session_id: owner,
                tool_call,
                options,
            } => {
                assert_eq!(owner, session_id);
                assert_eq!(request_id, "approve-command");
                assert_eq!(tool_call["command"], "cargo test");
                let options = options.as_array().unwrap();
                assert_eq!(options.len(), 2);
                assert!(options.iter().any(|option| option["option_id"] == "accept"));
                assert!(options
                    .iter()
                    .any(|option| option["option_id"] == "decline"));
                bridge
                    .send(CodexCommand::PermissionResponse {
                        request_id,
                        option_id: Some("accept".to_string()),
                    })
                    .await
                    .unwrap();
                approved = true;
            }
            CodexEvent::Completed { stop_reason, .. } => {
                assert_eq!(stop_reason, "completed");
                break;
            }
            CodexEvent::Failed { message, .. } => panic!("approval failed: {message}"),
            _ => {}
        }
    }
    assert!(approved, "native command approval was not presented");

    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn unavailable_command_approval_choices_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let codex = mock_codex(directory.path());
    let mut spec = mock_spec(
        codex,
        directory.path(),
        CodexExecutionMode::Native,
        "approval",
    );
    spec.environment
        .insert("RED_MOCK_EXPECT_DECISION".into(), "decline".into());
    let (mut bridge, task) =
        start_codex(spec, recording_host(), NonZeroUsize::new(32).unwrap()).unwrap();
    let session_id = create_session(&mut bridge, directory.path()).await;
    bridge
        .send(CodexCommand::Prompt {
            session_id,
            text: "run the focused tests".to_string(),
        })
        .await
        .unwrap();

    loop {
        match next_event(&mut bridge).await {
            CodexEvent::PermissionRequested { request_id, .. } => {
                bridge
                    .send(CodexCommand::PermissionResponse {
                        request_id,
                        option_id: Some("not-offered".to_string()),
                    })
                    .await
                    .unwrap();
            }
            CodexEvent::Completed { stop_reason, .. } => {
                assert_eq!(stop_reason, "completed");
                break;
            }
            CodexEvent::Failed { message, .. } => panic!("denial failed: {message}"),
            _ => {}
        }
    }

    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn review_safe_app_server_automatically_declines_native_approvals() {
    let directory = tempfile::tempdir().unwrap();
    let codex = mock_codex(directory.path());
    let mut spec = mock_spec(
        codex,
        directory.path(),
        CodexExecutionMode::ReviewSafe,
        "approval",
    );
    spec.environment
        .insert("RED_MOCK_EXPECT_DECISION".into(), "decline".into());
    let (mut bridge, task) =
        start_codex(spec, recording_host(), NonZeroUsize::new(32).unwrap()).unwrap();
    let session_id = create_session(&mut bridge, directory.path()).await;
    bridge
        .send(CodexCommand::Prompt {
            session_id,
            text: "inspect the focused tests".to_string(),
        })
        .await
        .unwrap();

    loop {
        match next_event(&mut bridge).await {
            CodexEvent::PermissionRequested { .. } => {
                panic!("review-safe mode surfaced a native command approval")
            }
            CodexEvent::Completed { stop_reason, .. } => {
                assert_eq!(stop_reason, "completed");
                break;
            }
            CodexEvent::Failed { message, .. } => panic!("review-safe turn failed: {message}"),
            _ => {}
        }
    }

    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn model_selection_applies_to_turns_and_hides_private_reasoning() {
    let directory = tempfile::tempdir().unwrap();
    let codex = mock_codex(directory.path());
    let mut spec = mock_spec(
        codex,
        directory.path(),
        CodexExecutionMode::Native,
        "activity",
    );
    spec.environment
        .insert("RED_MOCK_EXPECT_MODEL".into(), "mock-selected-model".into());
    spec.environment
        .insert("RED_MOCK_EXPECT_EFFORT".into(), "high".into());
    let (mut bridge, task) =
        start_codex(spec, recording_host(), NonZeroUsize::new(32).unwrap()).unwrap();
    let session_id = create_session(&mut bridge, directory.path()).await;

    bridge
        .send(CodexCommand::ListModels {
            session_id: session_id.clone(),
        })
        .await
        .unwrap();
    match next_event(&mut bridge).await {
        CodexEvent::Activity { update, .. } => {
            assert_eq!(update["session_update"], "models");
            assert_eq!(update["models"][0]["id"], "mock-selected-model");
            assert_eq!(
                update["models"][0]["supportedReasoningEfforts"][0]["reasoningEffort"],
                "high"
            );
        }
        other => panic!("unexpected model-catalog event: {other:?}"),
    }

    bridge
        .send(CodexCommand::SetModel {
            session_id: session_id.clone(),
            model: "mock-selected-model".to_string(),
            reasoning_effort: Some("high".to_string()),
        })
        .await
        .unwrap();
    match next_event(&mut bridge).await {
        CodexEvent::Activity { update, .. } => {
            assert_eq!(update["session_update"], "model_selected");
            assert_eq!(update["model"], "mock-selected-model");
            assert_eq!(update["reasoning_effort"], "high");
        }
        other => panic!("unexpected model-selection event: {other:?}"),
    }

    bridge
        .send(CodexCommand::Prompt {
            session_id,
            text: "explain the focused tests".to_string(),
        })
        .await
        .unwrap();
    let mut saw_public_reasoning = false;
    let mut saw_command_start = false;
    let mut saw_command_completion = false;
    let mut streamed = String::new();
    loop {
        match next_event(&mut bridge).await {
            CodexEvent::Activity { update, .. } => {
                assert!(
                    !update
                        .to_string()
                        .contains("private reasoning must stay hidden"),
                    "raw reasoning was disclosed"
                );
                match update["session_update"].as_str() {
                    Some("agent_thought_chunk") => {
                        assert_eq!(update["content"]["text"], "public reasoning summary");
                        saw_public_reasoning = true;
                    }
                    Some("tool_call") => {
                        assert_eq!(update["title"], "Running cargo test");
                        saw_command_start = true;
                    }
                    Some("tool_call_update") => {
                        assert_eq!(update["status"], "completed");
                        saw_command_completion = true;
                    }
                    _ => {}
                }
            }
            CodexEvent::Update { text, .. } => streamed.push_str(&text),
            CodexEvent::Completed { stop_reason, .. } => {
                assert_eq!(stop_reason, "completed");
                break;
            }
            CodexEvent::Failed { message, .. } => panic!("model turn failed: {message}"),
            _ => {}
        }
    }
    assert!(saw_public_reasoning);
    assert!(saw_command_start);
    assert!(saw_command_completion);
    assert_eq!(streamed, "working");

    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn durable_threads_can_be_listed_and_resumed() {
    let directory = tempfile::tempdir().unwrap();
    let codex = mock_codex(directory.path());
    let spec = mock_spec(codex, directory.path(), CodexExecutionMode::Native, "idle");
    let (mut bridge, task) =
        start_codex(spec, recording_host(), NonZeroUsize::new(32).unwrap()).unwrap();
    let session_id = create_session(&mut bridge, directory.path()).await;

    bridge
        .send(CodexCommand::ListSessions {
            session_id,
            cwd: directory.path().to_path_buf(),
        })
        .await
        .unwrap();
    match next_event(&mut bridge).await {
        CodexEvent::Activity { update, .. } => {
            assert_eq!(update["session_update"], "sessions");
            assert_eq!(update["sessions"][0]["id"], "persisted-red");
        }
        other => panic!("unexpected session-list event: {other:?}"),
    }

    bridge
        .send(CodexCommand::ResumeSession {
            session_id: "persisted-red".to_string(),
            cwd: directory.path().to_path_buf(),
        })
        .await
        .unwrap();
    match next_event(&mut bridge).await {
        CodexEvent::SessionCreated { session_id } => assert_eq!(session_id, "persisted-red"),
        other => panic!("unexpected resume event: {other:?}"),
    }

    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn active_turn_can_be_steered_and_cancelled_to_a_terminal_state() {
    let directory = tempfile::tempdir().unwrap();
    let codex = mock_codex(directory.path());
    let spec = mock_spec(
        codex,
        directory.path(),
        CodexExecutionMode::Native,
        "interrupt",
    );
    let (mut bridge, task) =
        start_codex(spec, recording_host(), NonZeroUsize::new(32).unwrap()).unwrap();
    let session_id = create_session(&mut bridge, directory.path()).await;
    bridge
        .send(CodexCommand::Prompt {
            session_id: session_id.clone(),
            text: "start the focused tests".to_string(),
        })
        .await
        .unwrap();

    match next_event(&mut bridge).await {
        CodexEvent::Activity { update, .. } => {
            assert_eq!(update["session_update"], "turn_started");
        }
        other => panic!("unexpected turn-start event: {other:?}"),
    }
    bridge
        .send(CodexCommand::Steer {
            session_id: session_id.clone(),
            text: "run only app-server tests".to_string(),
        })
        .await
        .unwrap();
    match next_event(&mut bridge).await {
        CodexEvent::Activity { update, .. } => {
            assert_eq!(update["session_update"], "steer");
            assert_eq!(update["turn_id"], "turn-red");
        }
        other => panic!("unexpected steering event: {other:?}"),
    }

    bridge
        .send(CodexCommand::Cancel { session_id })
        .await
        .unwrap();
    let mut acknowledged = false;
    loop {
        match next_event(&mut bridge).await {
            CodexEvent::Cancelled { .. } => acknowledged = true,
            CodexEvent::Completed { stop_reason, .. } => {
                assert_eq!(stop_reason, "interrupted");
                break;
            }
            CodexEvent::Failed { message, .. } => panic!("interrupt failed: {message}"),
            _ => {}
        }
    }
    assert!(acknowledged, "interrupt was not acknowledged");

    drop(bridge);
    task.await.unwrap().unwrap();
}

async fn assert_unavailable_session_recovers_once(
    mode: CodexExecutionMode,
    unavailable_thread: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let codex = mock_codex(directory.path());
    let mut spec = mock_spec(codex, directory.path(), mode, "idle");
    spec.environment
        .insert("RED_MOCK_EXPECT_RECOVERY".into(), "true".into());
    let (mut bridge, task) =
        start_codex(spec, recording_host(), NonZeroUsize::new(32).unwrap()).unwrap();

    bridge
        .send(CodexCommand::RecoverSession {
            session_id: unavailable_thread.to_string(),
            cwd: directory.path().to_path_buf(),
        })
        .await
        .unwrap();

    match next_event(&mut bridge).await {
        CodexEvent::SessionCreated { session_id } => assert_eq!(session_id, "thread-red"),
        CodexEvent::Failed { message, .. } => {
            panic!("automatic session recovery was not attempted: {message}")
        }
        other => panic!("unexpected recovery event: {other:?}"),
    }

    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn native_missing_session_recovers_with_inherited_codex_policy() {
    assert_unavailable_session_recovers_once(CodexExecutionMode::Native, "missing-red").await;
}

#[tokio::test]
async fn review_safe_missing_session_recovers_with_restricted_policy() {
    assert_unavailable_session_recovers_once(CodexExecutionMode::ReviewSafe, "missing-red").await;
}

#[tokio::test]
async fn mismatched_recovered_thread_starts_exactly_one_fresh_session() {
    assert_unavailable_session_recovers_once(CodexExecutionMode::Native, "mismatched-red").await;
}

#[tokio::test]
async fn explicit_resume_of_a_missing_thread_fails_without_creating_a_session() {
    let directory = tempfile::tempdir().unwrap();
    let codex = mock_codex(directory.path());
    let mut spec = mock_spec(codex, directory.path(), CodexExecutionMode::Native, "idle");
    spec.environment
        .insert("RED_MOCK_FORBID_RECOVERY".into(), "true".into());
    let (mut bridge, task) =
        start_codex(spec, recording_host(), NonZeroUsize::new(32).unwrap()).unwrap();

    bridge
        .send(CodexCommand::ResumeSession {
            session_id: "missing-red".to_string(),
            cwd: directory.path().to_path_buf(),
        })
        .await
        .unwrap();

    match next_event(&mut bridge).await {
        CodexEvent::Failed {
            session_id,
            message,
        } => {
            assert_eq!(session_id.as_deref(), Some("missing-red"));
            assert!(message.contains("not found"), "{message}");
        }
        other => panic!("explicit resume must fail rather than recover: {other:?}"),
    }

    drop(bridge);
    task.await.unwrap().unwrap();
}
