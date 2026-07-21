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
        assert len(message["params"]["dynamicTools"]) == 9
        expected_hooks = os.environ.get("RED_MOCK_EXPECT_HOOKS") == "true"
        assert message["params"]["config"]["features"]["hooks"] is expected_hooks
        assert "codex_hooks" not in message["params"]["config"]["features"]
        send({"id": ident, "result": {"thread": {"id": "thread-red"}}})
    elif method == "turn/start":
        text = message["params"]["input"][0]["text"]
        assert "Active editor context from red-buffer://active:" in text
        assert "unsaved editor text" in text
        send({"id": ident, "result": {"turn": {"id": "turn-red"}}})
        send({"method": "item/agentMessage/delta", "params": {
            "threadId": "thread-red", "turnId": "turn-red", "delta": "working"
        }})
        send({"id": "tool-write", "method": "item/tool/call", "params": {
            "threadId": "thread-red", "turnId": "turn-red",
            "tool": "write_file",
            "arguments": {"path": "src/main.rs", "content": "proposed\n"}
        }})
    elif ident == "tool-write":
        assert message["result"]["success"] is True
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

#[tokio::test]
async fn direct_app_server_streams_and_routes_writes_to_the_host() {
    let directory = tempfile::tempdir().unwrap();
    let codex = mock_codex(directory.path());
    let writes = Arc::new(Mutex::new(Vec::new()));
    let host = RecordingHost {
        writes: Arc::clone(&writes),
    };
    let (mut bridge, task) = start_codex(
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
