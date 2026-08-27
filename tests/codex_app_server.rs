#![cfg(unix)]

use std::{
    num::NonZeroUsize,
    os::unix::fs::PermissionsExt as _,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use red::{
    agent_tools::EditorToolRequest,
    codex::{start_codex, CodexCommand, CodexEvent, CodexProcessSpec, CodexToolHost},
};
use serde_json::{json, Value};

static MOCK_CODEX_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone)]
struct RecordingHost {
    writes: Arc<Mutex<Vec<(String, String)>>>,
}

#[async_trait]
impl CodexToolHost for RecordingHost {
    async fn read_file(
        &mut self,
        _: &str,
        path: &str,
        _: usize,
        _: usize,
    ) -> anyhow::Result<Value> {
        Ok(json!({"content": format!("unsaved:{path}")}))
    }

    async fn write_file(
        &mut self,
        _: &str,
        path: &str,
        _expected_revision: u64,
        content: String,
    ) -> anyhow::Result<Value> {
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
for feature in ("apps", "connectors", "plugins", "remote_plugin", "skill_mcp_dependency_install"):
    assert f"features.{feature}=false" in sys.argv
assert "orchestrator.mcp.enabled=false" in sys.argv

def send(value):
    print(json.dumps(value), flush=True)

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
        send({"id": ident, "result": {"config": {"mcp_servers": {}}, "origins": {}}})
    elif method == "configRequirements/read":
        requirements = json.loads(os.environ.get("RED_MOCK_REQUIREMENTS", "null"))
        send({"id": ident, "result": {"requirements": requirements}})
    elif method == "thread/start":
        assert message["params"]["sandbox"] == "read-only"
        assert message["params"]["approvalPolicy"] == "never"
        assert message["params"]["ephemeral"] is False
        expected_tools = {
            "list_files", "search_files", "read_file", "write_file",
            "get_editor_state", "open_file", "select_text", "apply_edits",
            "run_editor_action", "create_directory", "add_annotations",
            "dismiss_annotations",
            "lsp_status", "lsp_diagnostics", "lsp_prepare_rename",
            "lsp_preview_rename", "lsp_apply_edit",
        }
        tool_names = [tool["name"] for tool in message["params"]["dynamicTools"]]
        assert len(tool_names) == len(expected_tools) and set(tool_names) == expected_tools, tool_names
        expected_hooks = os.environ.get("RED_MOCK_EXPECT_HOOKS") == "true"
        assert message["params"]["config"]["features"]["hooks"] is expected_hooks
        assert "codex_hooks" not in message["params"]["config"]["features"]
        send({"id": ident, "result": {"thread": {"id": "thread-red"}}})
    elif method == "thread/resume":
        assert message["params"]["threadId"] == "thread-red"
        assert message["params"]["sandbox"] == "read-only"
        assert message["params"]["approvalPolicy"] == "never"
        assert "dynamicTools" not in message["params"]
        if os.environ.get("RED_MOCK_RESUME_ERROR"):
            send({"id": ident, "error": {"code": -32602, "message": "stored thread is unavailable"}})
            continue
        send({"id": ident, "result": {"thread": {
            "id": "thread-red",
            "ephemeral": False,
            "turns": [{
                "id": "restored-turn",
                "status": "completed",
                "items": [
                    {"id": "restored-user", "type": "userMessage", "content": [{"type": "text", "text": "earlier question"}]},
                    {"id": "restored-agent", "type": "agentMessage", "text": "earlier answer"}
                ]
            }]
        }}})
    elif method == "turn/start":
        assert message["params"]["input"][0]["text"] == "update the file"
        context = message["params"]["input"][1]["text"]
        assert "Active editor context from red-buffer://active:" in context
        assert "unsaved editor text" in context
        send({"id": ident, "result": {"turn": {"id": "turn-red"}}})
        send({"method": "item/agentMessage/delta", "params": {
            "threadId": "thread-red", "turnId": "turn-red", "delta": "working"
        }})
        send({"id": "tool-write", "method": "item/tool/call", "params": {
            "threadId": "thread-red", "turnId": "turn-red",
            "tool": "write_file",
            "arguments": {"path": "src/main.rs", "expected_revision": 0, "content": "updated\n"}
        }})
    elif ident == "tool-write":
        assert message["result"]["success"] is True
        send({"method": "item/completed", "params": {
            "threadId": "thread-red", "turnId": "turn-red",
            "item": {"id": "agent-red", "type": "agentMessage", "text": "working"}
        }})
        send({"method": "turn/completed", "params": {
            "threadId": "thread-red",
            "turn": {"id": "turn-red", "status": "completed"}
        }})
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}

fn mock_inline_codex(directory: &std::path::Path) -> std::path::PathBuf {
    let path = directory.join("codex-inline");
    std::fs::write(
        &path,
        r#"#!/usr/bin/env python3
import json, sys

def send(value):
    print(json.dumps(value), flush=True)

turn = 0
for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    ident = message.get("id")
    if method == "initialize":
        send({"id": ident, "result": {"userAgent": "mock"}})
    elif method == "initialized":
        pass
    elif method == "account/read":
        send({"id": ident, "result": {"account": {"type": "chatgpt"}}})
    elif method == "config/read":
        send({"id": ident, "result": {"config": {"mcp_servers": {}}, "origins": {}}})
    elif method == "configRequirements/read":
        send({"id": ident, "result": {"requirements": None}})
    elif method == "thread/start":
        assert message["params"]["ephemeral"] is True
        assert message["params"]["sandbox"] == "read-only"
        tools = message["params"]["dynamicTools"]
        assert len(tools) == 8
        assert tools[0]["name"] == "submit_replacement"
        assert tools[1]["name"] == "submit_comments"
        assert {tool["name"] for tool in tools[2:]} == {"request_agent", "propose_expanded_replacement", "list_files", "search_files", "read_file", "read_git_diff"}
        assert "inline code editor" in message["params"]["baseInstructions"]
        send({"id": ident, "result": {"thread": {"id": "inline-red"}}})
    elif method == "turn/start":
        turn += 1
        text = message["params"]["input"][0]["text"]
        assert "Editor-owned target and context" in text
        assert "submit_replacement" in text
        turn_id = f"inline-turn-{turn}"
        send({"id": ident, "result": {"turn": {"id": turn_id}}})
        if turn == 1:
            send({"method": "item/agentMessage/delta", "params": {
                "threadId": "inline-red", "turnId": turn_id, "delta": "Renamed the value."
            }})
        tool = "submit_comments" if turn in (3, 5) else "submit_replacement"
        if turn == 3:
            arguments = {"comments": [{"start_line": 1, "end_line": 2, "message": "Review both lines"}]}
        elif turn == 4:
            arguments = {"replacement": "first();\nsecond();\n", "comments": [{"start_line": 2, "message": "Explain the second call"}]}
        elif turn == 5:
            arguments = {"comments": [{"start_line": 1, "message": "Must not be applied"}]}
        else:
            arguments = {"replacement": "let answer = 42;\n" if turn == 1 else "let answer: u64 = 42;\n"}
        send({"id": f"inline-tool-{turn}", "method": "item/tool/call", "params": {
            "threadId": "inline-red", "turnId": turn_id,
            "tool": tool, "arguments": arguments
        }})
    elif str(ident).startswith("inline-tool-"):
        assert message["result"]["success"] is True
        current = str(ident).split("-")[-1]
        send({"method": "turn/completed", "params": {
            "threadId": "inline-red",
            "turn": {"id": f"inline-turn-{current}", "status": "interrupted" if current == "5" else "completed"}
        }})
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}

async fn next_event(
    bridge: &mut red::codex::CodexBridge,
    task: &mut tokio::task::JoinHandle<anyhow::Result<()>>,
) -> CodexEvent {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::select! {
            event = async {
                loop {
                    if let Some(event) = bridge.try_recv() {
                        break event;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            } => event,
            result = &mut *task => match result {
                Ok(Ok(())) => panic!("Codex worker stopped before producing an event"),
                Ok(Err(error)) => panic!("Codex worker failed before producing an event: {error:#}"),
                Err(error) => panic!("Codex worker task failed before producing an event: {error}"),
            }
        }
    })
    .await
    .expect("Codex worker did not produce an event within ten seconds")
}

#[tokio::test]
async fn inline_app_server_is_ephemeral_tool_limited_and_supports_followups() {
    let _serial = MOCK_CODEX_TEST_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let codex = mock_inline_codex(directory.path());
    let writes = Arc::new(Mutex::new(Vec::new()));
    let host = RecordingHost {
        writes: Arc::clone(&writes),
    };
    let (mut bridge, mut task) = start_codex(
        CodexProcessSpec::new(codex, directory.path()),
        host,
        NonZeroUsize::new(32).unwrap(),
    )
    .unwrap();

    bridge
        .send(CodexCommand::InlineAssist {
            request_id: "request-1".to_string(),
            cwd: directory.path().to_path_buf(),
            prompt: "use a descriptive value".to_string(),
            context: "<target>let x = 1;</target>".to_string(),
        })
        .await
        .unwrap();
    assert!(matches!(
        next_event(&mut bridge, &mut task).await,
        CodexEvent::InlineSessionCreated { request_id, session_id }
            if request_id == "request-1" && session_id == "inline-red"
    ));
    assert!(matches!(
        next_event(&mut bridge, &mut task).await,
        CodexEvent::InlineAnswerDelta { request_id, text }
            if request_id == "request-1" && text == "Renamed the value."
    ));
    assert!(matches!(
        next_event(&mut bridge, &mut task).await,
        CodexEvent::InlineResult { request_id, session_id, result }
            if request_id == "request-1"
                && session_id == "inline-red"
                && result.replacement.as_deref() == Some("let answer = 42;\n")
    ));

    bridge
        .send(CodexCommand::InlineAssistFollowup {
            request_id: "request-2".to_string(),
            session_id: "inline-red".to_string(),
            prompt: "give it an explicit type".to_string(),
            context: "<target>let answer = 42;</target>".to_string(),
        })
        .await
        .unwrap();
    assert!(matches!(
        next_event(&mut bridge, &mut task).await,
        CodexEvent::InlineResult { request_id, result, .. }
            if request_id == "request-2" && result.replacement.as_deref() == Some("let answer: u64 = 42;\n")
    ));
    for turn in 3..=5 {
        bridge
            .send(CodexCommand::InlineAssistFollowup {
                request_id: format!("request-{turn}"),
                session_id: "inline-red".to_string(),
                prompt: "review the target".to_string(),
                context: "<target>first();\nsecond();\n</target>".to_string(),
            })
            .await
            .unwrap();
        let event = next_event(&mut bridge, &mut task).await;
        match (turn, event) {
            (3, CodexEvent::InlineResult { result, .. }) => {
                assert!(result.replacement.is_none());
                assert_eq!(result.comments[0].last_line(), 2);
            }
            (4, CodexEvent::InlineResult { result, .. }) => {
                assert_eq!(result.replacement.as_deref(), Some("first();\nsecond();\n"));
                assert_eq!(result.comments[0].start_line, 2);
            }
            (5, CodexEvent::InlineFailed { message, .. }) => {
                assert!(message.contains("interrupted"))
            }
            (_, event) => panic!("unexpected inline event: {event:?}"),
        }
    }
    assert!(writes.lock().unwrap().is_empty());

    drop(bridge);
    task.await.unwrap().unwrap();
}

fn mock_commit_codex(directory: &std::path::Path) -> std::path::PathBuf {
    let path = directory.join("codex-commit");
    std::fs::write(
        &path,
        r#"#!/usr/bin/env python3
import json, sys

def send(value):
    print(json.dumps(value), flush=True)

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
        send({"id": ident, "result": {"config": {"mcp_servers": {}}, "origins": {}}})
    elif method == "configRequirements/read":
        send({"id": ident, "result": {"requirements": None}})
    elif method == "thread/start":
        params = message["params"]
        assert params["sandbox"] == "read-only"
        assert params["approvalPolicy"] == "never"
        assert params["dynamicTools"] == []
        assert "Draft one Git commit message" in params["baseInstructions"]
        assert params["config"]["features"]["hooks"] is False
        send({"id": ident, "result": {"thread": {"id": "thread-commit"}}})
    elif method == "turn/start":
        text = message["params"]["input"][0]["text"]
        assert "staged_changes" in text
        assert "recent_commit_messages" in text
        send({"id": ident, "result": {"turn": {"id": "turn-commit"}}})
        send({"method": "item/agentMessage/delta", "params": {
            "threadId": "thread-commit", "turnId": "turn-commit",
            "delta": "```text\nfeat(git): generate commit messages\n```"
        }})
        send({"method": "turn/completed", "params": {
            "threadId": "thread-commit",
            "turn": {"id": "turn-commit", "status": "completed"}
        }})
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}

#[tokio::test]
async fn direct_app_server_streams_and_routes_writes_to_the_host() {
    let _serial = MOCK_CODEX_TEST_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let codex = mock_codex(directory.path());
    let writes = Arc::new(Mutex::new(Vec::new()));
    let host = RecordingHost {
        writes: Arc::clone(&writes),
    };
    let (mut bridge, mut task) = start_codex(
        CodexProcessSpec::new(codex, directory.path()),
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
    let session_id = match next_event(&mut bridge, &mut task).await {
        CodexEvent::SessionCreated { session_id } => session_id,
        other => panic!("expected created session, got {other:?}"),
    };
    assert_eq!(session_id, "thread-red");

    bridge
        .send(CodexCommand::PromptWithContext {
            session_id,
            text: "update the file".to_string(),
            uri: "red-buffer://active".to_string(),
            context: "unsaved editor text".to_string(),
        })
        .await
        .unwrap();
    let mut streamed = String::new();
    loop {
        match next_event(&mut bridge, &mut task).await {
            CodexEvent::Update { text, .. } => streamed.push_str(&text),
            CodexEvent::Completed { stop_reason, .. } => {
                assert_eq!(stop_reason, "completed");
                break;
            }
            CodexEvent::Failed { message, .. } => panic!("{message}"),
            _ => {}
        }
    }
    assert_eq!(streamed, "working");
    assert_eq!(
        *writes.lock().unwrap(),
        vec![("src/main.rs".to_string(), "updated\n".to_string())]
    );

    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn direct_app_server_generates_commit_messages_without_tools_or_agent_events() {
    let _serial = MOCK_CODEX_TEST_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let codex = mock_commit_codex(directory.path());
    let writes = Arc::new(Mutex::new(Vec::new()));
    let host = RecordingHost {
        writes: Arc::clone(&writes),
    };
    let (mut bridge, mut task) = start_codex(
        CodexProcessSpec::new(codex, directory.path()),
        host,
        NonZeroUsize::new(32).unwrap(),
    )
    .unwrap();

    bridge
        .send(CodexCommand::GenerateCommitMessage {
            request_id: 42,
            cwd: directory.path().to_path_buf(),
            prompt: "staged_changes recent_commit_messages".to_string(),
        })
        .await
        .unwrap();
    let event = next_event(&mut bridge, &mut task).await;

    assert!(matches!(
        event,
        CodexEvent::CommitMessageGenerated {
            request_id: 42,
            result: Ok(message),
        } if message == "feat(git): generate commit messages"
    ));
    assert!(writes.lock().unwrap().is_empty());
    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn direct_app_server_resumes_a_persisted_thread_with_history() {
    let _serial = MOCK_CODEX_TEST_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let codex = mock_codex(directory.path());
    let host = RecordingHost {
        writes: Arc::new(Mutex::new(Vec::new())),
    };
    let (mut bridge, mut task) = start_codex(
        CodexProcessSpec::new(codex, directory.path()),
        host,
        NonZeroUsize::new(32).unwrap(),
    )
    .unwrap();

    bridge
        .send(CodexCommand::ResumeSession {
            cwd: directory.path().to_path_buf(),
            session_id: "thread-red".to_string(),
        })
        .await
        .unwrap();
    let event = next_event(&mut bridge, &mut task).await;

    match event {
        CodexEvent::SessionRestored { session_id, thread } => {
            assert_eq!(session_id, "thread-red");
            assert_eq!(thread["turns"][0]["items"][1]["text"], "earlier answer");
        }
        other => panic!("expected restored session, got {other:?}"),
    }
    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn direct_app_server_reports_a_missing_persisted_thread_as_restore_failure() {
    let _serial = MOCK_CODEX_TEST_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let codex = mock_codex(directory.path());
    let host = RecordingHost {
        writes: Arc::new(Mutex::new(Vec::new())),
    };
    let mut spec = CodexProcessSpec::new(codex, directory.path());
    spec.environment
        .insert("RED_MOCK_RESUME_ERROR".into(), "true".into());
    let (mut bridge, mut task) = start_codex(spec, host, NonZeroUsize::new(32).unwrap()).unwrap();

    bridge
        .send(CodexCommand::ResumeSession {
            cwd: directory.path().to_path_buf(),
            session_id: "thread-red".to_string(),
        })
        .await
        .unwrap();
    let event = next_event(&mut bridge, &mut task).await;

    assert!(matches!(
        event,
        CodexEvent::SessionRestoreFailed { session_id, message }
            if session_id == "thread-red" && message.contains("unavailable")
    ));
    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn direct_app_server_starts_without_managed_feature_requirements() {
    let _serial = MOCK_CODEX_TEST_LOCK.lock().await;
    for requirements in [
        json!({"allowedSandboxModes": ["read-only"]}),
        json!({"allowedSandboxModes": ["read-only"], "featureRequirements": null}),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let codex = mock_codex(directory.path());
        let host = RecordingHost {
            writes: Arc::new(Mutex::new(Vec::new())),
        };
        let mut spec = CodexProcessSpec::new(codex, directory.path());
        spec.environment.insert(
            "RED_MOCK_REQUIREMENTS".into(),
            requirements.to_string().into(),
        );
        let (mut bridge, mut task) =
            start_codex(spec, host, NonZeroUsize::new(32).unwrap()).unwrap();

        bridge
            .send(CodexCommand::NewSession {
                cwd: directory.path().to_path_buf(),
            })
            .await
            .unwrap();
        let event = next_event(&mut bridge, &mut task).await;

        assert!(
            matches!(event, CodexEvent::SessionCreated { session_id } if session_id == "thread-red")
        );
        drop(bridge);
        task.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn direct_app_server_starts_with_required_hooks() {
    let _serial = MOCK_CODEX_TEST_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let codex = mock_codex(directory.path());
    let host = RecordingHost {
        writes: Arc::new(Mutex::new(Vec::new())),
    };
    let mut spec = CodexProcessSpec::new(codex, directory.path());
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
    let (mut bridge, mut task) = start_codex(spec, host, NonZeroUsize::new(32).unwrap()).unwrap();

    bridge
        .send(CodexCommand::NewSession {
            cwd: directory.path().to_path_buf(),
        })
        .await
        .unwrap();
    let event = next_event(&mut bridge, &mut task).await;

    assert!(
        matches!(event, CodexEvent::SessionCreated { session_id } if session_id == "thread-red")
    );
    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn direct_app_server_reports_live_startup_failure_and_stderr_availability() {
    let _serial = MOCK_CODEX_TEST_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let codex = directory.path().join("codex-fails");
    std::fs::write(
        &codex,
        "#!/bin/sh\necho 'workplace policy rejected app-server startup' >&2\nexit 23\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&codex).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&codex, permissions).unwrap();
    let host = RecordingHost {
        writes: Arc::new(Mutex::new(Vec::new())),
    };

    let (_bridge, task) = start_codex(
        CodexProcessSpec::new(codex, directory.path()),
        host,
        NonZeroUsize::new(32).unwrap(),
    )
    .unwrap();
    let error = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .expect("failing Codex worker should terminate")
        .unwrap()
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("Codex app-server initialization failed"),
        "{error}"
    );
    assert!(error.contains("status: 23"), "{error}");
    assert!(
        error.contains("diagnostic details to the Red log"),
        "{error}"
    );
}

fn mock_model_codex(directory: &std::path::Path) -> std::path::PathBuf {
    let path = directory.join("codex-models");
    std::fs::write(&path, r#"#!/usr/bin/env python3
import json, sys
def send(value):
    print(json.dumps(value), flush=True)
def info(model, effort):
    return {"model": model, "modelProvider": "test-provider", "reasoningEffort": effort}
for line in sys.stdin:
    message = json.loads(line)
    method, ident = message.get("method"), message.get("id")
    params = message.get("params", {})
    if method == "initialize":
        send({"id": ident, "result": {"userAgent": "mock"}})
    elif method == "account/read":
        send({"id": ident, "result": {"account": {"type": "chatgpt"}}})
    elif method == "config/read":
        send({"id": ident, "result": {"config": {"mcp_servers": {}}}})
    elif method == "configRequirements/read":
        send({"id": ident, "result": {"requirements": None}})
    elif method == "model/list":
        assert params["includeHidden"] is False
        if params.get("cursor") is None:
            send({"id": ident, "result": {"data": [{"model":"first","displayName":"First"},{"model":"secret","hidden":True}],"nextCursor":"page-2"}})
        else:
            assert params["cursor"] == "page-2"
            send({"id": ident, "result": {"data": [{"model":"first"},{"model":"second","displayName":"Second"}],"nextCursor":None}})
    elif method == "thread/start":
        assert params["model"] == "first"
        assert params["config"]["model_reasoning_effort"] == "high"
        assert params["sandbox"] == "read-only"
        send({"id": ident, "result": dict(info("first", "high"), thread={"id":"model-thread"})})
    elif method == "turn/start":
        assert params["threadId"] == "model-thread"
        assert "model" not in params
        send({"id": ident, "result": {"turn":{"id":"running-turn"}}})
    elif method == "thread/settings/update":
        assert set(params) == {"threadId","model","effort"}
        assert params["threadId"] == "model-thread"
        if params["model"] == "rejected":
            send({"id": ident, "error":{"code":-32602,"message":"model unavailable"}})
            continue
        assert params["model"] == "second" and params["effort"] == "low"
        send({"method":"thread/settings/updated","params":{"threadId":"foreign-thread","threadSettings":{"model":"wrong","effort":"max"}}})
        send({"method":"thread/settings/updated","params":{"threadId":"model-thread","threadSettings":{"model":"second","modelProvider":"test-provider","effort":"low"}}})
        send({"id":ident,"result":{}})
        send({"method":"turn/completed","params":{"threadId":"model-thread","turn":{"id":"running-turn","status":"completed"}}})
    elif method == "thread/resume":
        send({"id":ident,"result":dict(info("second","low"),thread={"id":"model-thread","turns":[]})})
    elif method == "turn/interrupt":
        raise AssertionError("changing the model must not interrupt the running turn")
"#).unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}

#[tokio::test]
async fn direct_app_server_lists_and_changes_conversation_models() {
    use red::codex::{AgentModelSelection, ModelRequest};
    let _serial = MOCK_CODEX_TEST_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let host = RecordingHost {
        writes: Arc::new(Mutex::new(Vec::new())),
    };
    let (mut bridge, mut task) = start_codex(
        CodexProcessSpec::new(mock_model_codex(directory.path()), directory.path()),
        host,
        NonZeroUsize::new(32).unwrap(),
    )
    .unwrap();
    bridge
        .send(CodexCommand::ModelRequest {
            request_id: 1,
            request: ModelRequest::List,
        })
        .await
        .unwrap();
    match next_event(&mut bridge, &mut task).await {
        CodexEvent::ModelRequestCompleted {
            request_id: 1,
            result: Ok(result),
        } => {
            assert_eq!(
                result["models"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|model| model["model"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                ["first", "second"]
            );
        }
        event => panic!("unexpected catalog result: {event:?}"),
    }
    bridge
        .send(CodexCommand::NewSessionWithModel {
            cwd: directory.path().to_path_buf(),
            selection: AgentModelSelection {
                model: "first".into(),
                effort: Some("high".into()),
            },
        })
        .await
        .unwrap();
    assert!(
        matches!(next_event(&mut bridge, &mut task).await, CodexEvent::SessionCreated { session_id } if session_id == "model-thread")
    );
    assert!(
        matches!(next_event(&mut bridge, &mut task).await, CodexEvent::SessionModelChanged { model_info, .. } if model_info.model == "first" && model_info.effort.as_deref() == Some("high"))
    );
    bridge
        .send(CodexCommand::Prompt {
            session_id: "model-thread".into(),
            text: "keep working".into(),
        })
        .await
        .unwrap();
    bridge
        .send(CodexCommand::ModelRequest {
            request_id: 2,
            request: ModelRequest::Set {
                session_id: "model-thread".into(),
                selection: AgentModelSelection {
                    model: "second".into(),
                    effort: Some("low".into()),
                },
            },
        })
        .await
        .unwrap();
    assert!(
        matches!(next_event(&mut bridge, &mut task).await, CodexEvent::SessionModelChanged { session_id, model_info } if session_id == "model-thread" && model_info.model == "second" && model_info.provider.as_deref() == Some("test-provider") && model_info.effort.as_deref() == Some("low"))
    );
    assert!(matches!(
        next_event(&mut bridge, &mut task).await,
        CodexEvent::ModelRequestCompleted {
            request_id: 2,
            result: Ok(_)
        }
    ));
    assert!(
        matches!(next_event(&mut bridge, &mut task).await, CodexEvent::Completed { session_id, .. } if session_id == "model-thread")
    );
    bridge
        .send(CodexCommand::ModelRequest {
            request_id: 3,
            request: ModelRequest::Set {
                session_id: "model-thread".into(),
                selection: AgentModelSelection {
                    model: "rejected".into(),
                    effort: Some("low".into()),
                },
            },
        })
        .await
        .unwrap();
    assert!(
        matches!(next_event(&mut bridge, &mut task).await, CodexEvent::ModelRequestCompleted { request_id: 3, result: Err(error) } if error == "model unavailable")
    );
    bridge
        .send(CodexCommand::ResumeSession {
            cwd: directory.path().to_path_buf(),
            session_id: "model-thread".into(),
        })
        .await
        .unwrap();
    assert!(matches!(
        next_event(&mut bridge, &mut task).await,
        CodexEvent::SessionRestored { .. }
    ));
    assert!(
        matches!(next_event(&mut bridge, &mut task).await, CodexEvent::SessionModelChanged { model_info, .. } if model_info.model == "second")
    );
    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn direct_app_server_previews_workspace_model_without_creating_a_thread() {
    use red::codex::ModelRequest;
    let _serial = MOCK_CODEX_TEST_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("codex-default-model");
    std::fs::write(&executable, r#"#!/usr/bin/env python3
import json, os, sys
mode = None
def send(ident, result):
    print(json.dumps({"id":ident,"result":result}), flush=True)
for line in sys.stdin:
    msg=json.loads(line)
    method, ident, params=msg.get("method"),msg.get("id"),msg.get("params",{})
    if method == "initialize": send(ident,{"userAgent":"mock"})
    elif method == "initialized": pass
    elif method == "account/read": send(ident,{"account":{"type":"chatgpt"}})
    elif method == "config/read":
        assert params["includeLayers"] is False
        mode=os.path.basename(params["cwd"])
        config={"model_provider":"configured-provider"}
        if mode == "configured": config.update(model="configured-model",model_reasoning_effort="high")
        if mode == "effort": config["model_reasoning_effort"]="low"
        if mode == "invalid": send(ident,{})
        else: send(ident,{"config":config})
    elif method == "model/list":
        assert mode in ["fallback","effort","missing"], "explicit model must not need the catalog"
        assert params["includeHidden"] is False
        if params.get("cursor") is None:
            send(ident,{"data":[{"model":"hidden","isDefault":True,"hidden":True},{"model":"other"}],"nextCursor":"page-2"})
        else:
            assert params["cursor"] == "page-2"
            send(ident,{"data":[{"model":"catalog-default","isDefault":mode!="missing","defaultReasoningEffort":"medium"}],"nextCursor":None})
    else: raise AssertionError("preview must be read-only: "+str(method))
"#).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (mut bridge, mut task) = start_codex(
        CodexProcessSpec::new(executable, directory.path()),
        RecordingHost {
            writes: Arc::new(Mutex::new(Vec::new())),
        },
        NonZeroUsize::new(8).unwrap(),
    )
    .unwrap();
    for (index, (mode, model, effort)) in [
        ("configured", "configured-model", "high"),
        ("fallback", "catalog-default", "medium"),
        ("effort", "catalog-default", "low"),
    ]
    .into_iter()
    .enumerate()
    {
        let request_id = index as i64;
        bridge
            .send(CodexCommand::ModelRequest {
                request_id,
                request: ModelRequest::ReadDefault {
                    cwd: directory.path().join(mode),
                },
            })
            .await
            .unwrap();
        match next_event(&mut bridge, &mut task).await {
            CodexEvent::ModelRequestCompleted {
                request_id: actual,
                result: Ok(result),
            } => {
                assert_eq!(actual, request_id);
                assert_eq!(
                    result["model_info"],
                    json!({"model":model,"provider":"configured-provider","effort":effort})
                );
            }
            event => panic!("unexpected preview result: {event:?}"),
        }
    }
    for mode in ["invalid", "missing"] {
        bridge
            .send(CodexCommand::ModelRequest {
                request_id: 4,
                request: ModelRequest::ReadDefault {
                    cwd: directory.path().join(mode),
                },
            })
            .await
            .unwrap();
        assert!(matches!(
            next_event(&mut bridge, &mut task).await,
            CodexEvent::ModelRequestCompleted {
                request_id: 4,
                result: Err(_)
            }
        ));
    }
    drop(bridge);
    task.await.unwrap().unwrap();
}
