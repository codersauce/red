use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use agent_client_protocol_schema::v1::{ReadTextFileRequest, WriteTextFileRequest};
use red::{
    acp::AcpHost,
    agent_workspace::{ProposalAcpHost, ProposalDisposition, ProposalWorkspace},
};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter},
    process::{Child, ChildStdin, ChildStdout, Command},
};

const TEST_TIMEOUT: Duration = Duration::from_secs(10);
const MOCK_APP_SERVER: &str = r#"#!/usr/bin/env python3
import json
import os
import pathlib
import sys

mode = os.environ['MOCK_MODE']
record = pathlib.Path(os.environ['MOCK_RECORD'])
thread_id = 'thread-red-codex'
turn_id = 'turn-red-codex'
seen = []

def save(event, value):
    seen.append({'event': event, 'value': value})
    record.write_text(json.dumps(seen))

def send(value):
    sys.stdout.write(json.dumps(value) + '\n')
    sys.stdout.flush()

def receive():
    line = sys.stdin.readline()
    if not line:
        raise SystemExit(0)
    return json.loads(line)

def call(tool, arguments):
    call_id = 'tool-' + str(len(seen))
    send({'id': call_id, 'method': 'item/tool/call', 'params': {
        'threadId': thread_id, 'turnId': turn_id, 'callId': call_id,
        'tool': tool, 'arguments': arguments,
    }})
    response = receive()
    save('tool:' + tool, response)
    return response

while True:
    request = receive()
    method = request.get('method')
    if method == 'initialize':
        save('initialize', request['params'])
        if mode == 'incompatible':
            send({'id': request['id'], 'error': {'code': -32602, 'message': 'experimental API is unavailable'}})
            raise SystemExit(0)
        send({'id': request['id'], 'result': {'userAgent': 'mock-codex'}})
    elif method == 'initialized':
        save('initialized', request.get('params', {}))
    elif method == 'account/read':
        save('account', request['params'])
        if mode == 'unauthenticated':
            send({'id': request['id'], 'result': {'account': None, 'requiresOpenaiAuth': True}})
        else:
            send({'id': request['id'], 'result': {'account': {'type': 'chatgpt', 'email': None, 'planType': 'pro'}, 'requiresOpenaiAuth': True}})
    elif method == 'thread/start':
        save('thread', request['params'])
        send({'id': request['id'], 'result': {'thread': {'id': thread_id}}})
    elif method == 'turn/start':
        save('turn', request['params'])
        send({'id': request['id'], 'result': {'turn': {'id': turn_id, 'items': [], 'status': 'inProgress', 'error': None}}})
        if mode == 'cancel':
            send({'method': 'item/agentMessage/delta', 'params': {'threadId': thread_id, 'turnId': turn_id, 'itemId': 'message', 'delta': 'working'}})
            continue
        if mode == 'close':
            save('closed', request['params'])
            raise SystemExit(0)
        if mode == 'failed':
            send({'method': 'turn/completed', 'params': {'threadId': thread_id, 'turn': {'id': turn_id, 'items': [], 'status': 'failed', 'error': {'message': 'secret backend details'}}}})
            continue
        if mode == 'proposal':
            call('list_files', {})
            if os.name == 'posix':
                call('search_files', {'query': 'disk contents'})
            call('read_file', {'path': 'existing.rs'})
            call('read_file', {'path': 'new.rs'})
            call('write_file', {'path': 'existing.rs', 'content': 'staged existing contents\n'})
            call('write_file', {'path': 'new.rs', 'content': 'staged new contents\n'})
            call('read_file', {'path': 'existing.rs'})
            call('read_file', {'path': 'new.rs'})
        elif mode == 'unsafe':
            call('write_file', {'path': '../outside.rs', 'content': 'must not be created'})
            if os.name == 'posix':
                call('write_file', {'path': 'linked.rs', 'content': 'must not follow link'})
            call('read_file', {'path': 'existing.rs', 'extra': 'must be rejected'})
            send({'id': 'native-write', 'method': 'item/fileChange/requestApproval', 'params': {'threadId': thread_id, 'turnId': turn_id, 'itemId': 'native-write'}})
            save('native-approval', receive())
            send({'id': 'native-command', 'method': 'item/commandExecution/requestApproval', 'params': {'threadId': thread_id, 'turnId': turn_id, 'itemId': 'native-command'}})
            save('command-approval', receive())
            send({'id': 'native-permissions', 'method': 'item/permissions/requestApproval', 'params': {'threadId': thread_id, 'turnId': turn_id, 'itemId': 'native-permissions', 'permissions': {'fileSystem': {'write': ['/']}}}})
            save('permissions-approval', receive())
        send({'method': 'item/agentMessage/delta', 'params': {'threadId': thread_id, 'turnId': turn_id, 'itemId': 'message', 'delta': 'Proposal is ready for review.'}})
        send({'method': 'turn/completed', 'params': {'threadId': thread_id, 'turn': {'id': turn_id, 'items': [], 'status': 'completed', 'error': None}}})
    elif method == 'turn/interrupt':
        save('interrupt', request['params'])
        send({'id': request['id'], 'result': {}})
        send({'method': 'turn/completed', 'params': {'threadId': thread_id, 'turn': {'id': turn_id, 'items': [], 'status': 'interrupted', 'error': None}}})
    else:
        save('unexpected', request)
        if 'id' in request:
            send({'id': request['id'], 'error': {'code': -32601, 'message': 'unexpected request'}})
"#;

struct Harness {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    record: PathBuf,
    _mock: tempfile::TempDir,
}

impl Harness {
    fn start(mode: &str) -> Self {
        let mock = tempfile::tempdir().unwrap();
        let script = mock.path().join("mock-codex.py");
        let record = mock.path().join("record.json");
        std::fs::write(&script, MOCK_APP_SERVER).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        #[cfg(windows)]
        let script = {
            let launcher = mock.path().join("mock-codex.cmd");
            std::fs::write(
                &launcher,
                "@echo off\r\npython \"%~dp0mock-codex.py\" %*\r\n",
            )
            .unwrap();
            launcher
        };
        let mut child = Command::new(env!("CARGO_BIN_EXE_red_codex_acp"))
            .arg("--codex")
            .arg(&script)
            .env("MOCK_MODE", mode)
            .env("MOCK_RECORD", &record)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdin = BufWriter::new(child.stdin.take().unwrap());
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
            record,
            _mock: mock,
        }
    }

    async fn send(&mut self, message: Value) {
        let mut encoded = serde_json::to_vec(&message).unwrap();
        encoded.push(b'\n');
        self.stdin.write_all(&encoded).await.unwrap();
        self.stdin.flush().await.unwrap();
    }

    async fn next(&mut self) -> Value {
        let mut line = String::new();
        let bytes = tokio::time::timeout(TEST_TIMEOUT, self.stdout.read_line(&mut line))
            .await
            .expect("ACP response timed out")
            .unwrap();
        assert_ne!(bytes, 0, "ACP process closed stdout");
        serde_json::from_str(&line).unwrap()
    }

    async fn initialize(&mut self) {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": 1, "clientCapabilities": {"fs": {"readTextFile": true, "writeTextFile": true}}}
        }))
        .await;
        let initialized = self.next().await;
        assert_eq!(initialized["result"]["protocolVersion"], 1);
        assert_eq!(initialized["result"]["agentInfo"]["name"], "red-codex-acp");
    }

    async fn create_session(&mut self, cwd: &Path) -> String {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": {"cwd": cwd}
        }))
        .await;
        self.next().await["result"]["sessionId"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn events(&self) -> Vec<Value> {
        serde_json::from_slice(&std::fs::read(&self.record).unwrap()).unwrap()
    }

    async fn finish(mut self) -> Vec<Value> {
        let events = self.events();
        self.stdin.shutdown().await.unwrap();
        drop(self.stdin);
        drop(self.stdout);
        let output = tokio::time::timeout(TEST_TIMEOUT, self.child.wait_with_output())
            .await
            .expect("ACP process did not stop")
            .unwrap();
        assert!(output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("unsaved existing contents"));
        assert!(!stderr.contains("staged existing contents"));
        assert!(!stderr.contains("must not"));
        events
    }
}

fn event<'a>(events: &'a [Value], name: &str) -> &'a Value {
    events
        .iter()
        .find(|entry| entry["event"] == name)
        .unwrap_or_else(|| panic!("missing recorded event {name}"))
}

#[tokio::test]
async fn codex_dynamic_tools_round_trip_the_real_proposal_host_without_touching_disk() {
    let workspace = tempfile::tempdir().unwrap();
    let existing = workspace.path().join("existing.rs");
    let created = workspace.path().join("new.rs");
    std::fs::write(&existing, "disk contents\n").unwrap();
    let proposal_workspace = Arc::new(StdMutex::new(
        ProposalWorkspace::new(workspace.path()).unwrap(),
    ));
    proposal_workspace
        .lock()
        .unwrap()
        .sync_visible_file(&existing, 7, "unsaved existing contents\n".to_string())
        .unwrap();
    let mut host = ProposalAcpHost::new(Arc::clone(&proposal_workspace));
    let mut acp = Harness::start("proposal");
    acp.initialize().await;
    let session = acp.create_session(workspace.path()).await;
    proposal_workspace
        .lock()
        .unwrap()
        .begin_turn(&session, "turn-1".to_string());
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "stage the edit"}]}
    }))
    .await;

    for (path, contents) in [(&existing, "unsaved existing contents\n"), (&created, "")] {
        let read = acp.next().await;
        assert_eq!(read["method"], "fs/read_text_file");
        assert_eq!(read["params"]["path"], path.to_string_lossy().as_ref());
        let request: ReadTextFileRequest = serde_json::from_value(read["params"].clone()).unwrap();
        let result = serde_json::to_value(host.read_text_file(request).await.unwrap()).unwrap();
        assert_eq!(result["content"], contents);
        acp.send(json!({"jsonrpc": "2.0", "id": read["id"], "result": result}))
            .await;
    }
    for (path, contents) in [
        (&existing, "staged existing contents\n"),
        (&created, "staged new contents\n"),
    ] {
        let write = acp.next().await;
        assert_eq!(write["method"], "fs/write_text_file");
        assert_eq!(write["params"]["path"], path.to_string_lossy().as_ref());
        assert_eq!(write["params"]["content"], contents);
        let request: WriteTextFileRequest =
            serde_json::from_value(write["params"].clone()).unwrap();
        let result = serde_json::to_value(host.write_text_file(request).await.unwrap()).unwrap();
        acp.send(json!({"jsonrpc": "2.0", "id": write["id"], "result": result}))
            .await;
    }
    assert_eq!(
        std::fs::read_to_string(&existing).unwrap(),
        "disk contents\n"
    );
    assert!(!created.exists());
    assert_eq!(
        proposal_workspace.lock().unwrap().pending_files(&session),
        vec![existing.clone(), created.clone()]
    );
    for (path, contents) in [
        (&existing, "staged existing contents\n"),
        (&created, "staged new contents\n"),
    ] {
        let read = acp.next().await;
        assert_eq!(read["method"], "fs/read_text_file");
        assert_eq!(read["params"]["path"], path.to_string_lossy().as_ref());
        let request: ReadTextFileRequest = serde_json::from_value(read["params"].clone()).unwrap();
        let result = serde_json::to_value(host.read_text_file(request).await.unwrap()).unwrap();
        assert_eq!(result["content"], contents);
        acp.send(json!({"jsonrpc": "2.0", "id": read["id"], "result": result}))
            .await;
    }
    let update = acp.next().await;
    assert_eq!(update["method"], "session/update");
    assert_eq!(
        update["params"]["update"]["content"]["text"],
        "Proposal is ready for review."
    );
    assert_eq!(acp.next().await["result"]["stopReason"], "end_turn");
    let events = acp.finish().await;

    let initialize = &event(&events, "initialize")["value"];
    assert_eq!(initialize["capabilities"]["experimentalApi"], true);
    let thread = &event(&events, "thread")["value"];
    assert_eq!(thread["environments"], json!([]));
    assert_eq!(thread["sandbox"], "read-only");
    assert_eq!(thread["approvalPolicy"], "never");
    let tools = thread["dynamicTools"].as_array().unwrap();
    assert_eq!(tools.len(), 4);
    assert_eq!(tools[0]["name"], "list_files");
    assert_eq!(tools[1]["name"], "search_files");
    assert_eq!(tools[2]["name"], "read_file");
    assert_eq!(tools[3]["name"], "write_file");
    let turn = &event(&events, "turn")["value"];
    assert_eq!(turn["environments"], json!([]));
    assert_eq!(turn["approvalPolicy"], "never");
    assert_eq!(turn["sandboxPolicy"]["type"], "readOnly");
    let list = &event(&events, "tool:list_files")["value"];
    let list_text = list["result"]["contentItems"][0]["text"].as_str().unwrap();
    assert!(list_text.contains("existing.rs"));
    #[cfg(unix)]
    {
        let search = &event(&events, "tool:search_files")["value"];
        let search_text = search["result"]["contentItems"][0]["text"]
            .as_str()
            .unwrap();
        assert!(search_text.contains("disk contents"));
    }

    let mut proposals = proposal_workspace.lock().unwrap();
    let disposition = proposals
        .accept_all(&session, &existing, 7, "unsaved existing contents\n")
        .unwrap();
    assert!(matches!(
        disposition,
        ProposalDisposition::Applied { contents, created: false, .. }
            if contents == "staged existing contents\n"
    ));
    proposals.reject_all(&session, &created, 0, "").unwrap();
    assert!(proposals.pending_files(&session).is_empty());
    assert_eq!(
        std::fs::read_to_string(existing).unwrap(),
        "disk contents\n"
    );
    assert!(!created.exists());
}

#[tokio::test]
async fn codex_bridge_rejects_unsafe_tools_and_native_file_approval_without_fallback() {
    let workspace = tempfile::tempdir().unwrap();
    let existing = workspace.path().join("existing.rs");
    let outside = workspace.path().parent().unwrap().join("outside.rs");
    std::fs::write(&existing, "disk contents\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&existing, workspace.path().join("linked.rs")).unwrap();
    let mut acp = Harness::start("unsafe");
    acp.initialize().await;
    let session = acp.create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "attempt unsafe writes"}]}
    }))
    .await;
    assert_eq!(acp.next().await["method"], "session/update");
    assert_eq!(acp.next().await["result"]["stopReason"], "end_turn");
    let events = acp.finish().await;
    assert_eq!(
        std::fs::read_to_string(&existing).unwrap(),
        "disk contents\n"
    );
    assert!(!outside.exists());
    let writes: Vec<_> = events
        .iter()
        .filter(|entry| entry["event"] == "tool:write_file")
        .collect();
    assert_eq!(writes.len(), if cfg!(unix) { 2 } else { 1 });
    assert!(writes
        .iter()
        .all(|entry| entry["value"]["result"]["success"] == false));
    assert_eq!(
        event(&events, "tool:read_file")["value"]["result"]["success"],
        false
    );
    assert_eq!(
        event(&events, "native-approval")["value"]["result"]["decision"],
        "decline"
    );
    assert_eq!(
        event(&events, "command-approval")["value"]["result"]["decision"],
        "decline"
    );
    assert_eq!(
        event(&events, "permissions-approval")["value"]["result"]["permissions"],
        json!({})
    );
}

#[tokio::test]
async fn codex_cancellation_interrupts_the_active_turn() {
    let workspace = tempfile::tempdir().unwrap();
    let mut acp = Harness::start("cancel");
    acp.initialize().await;
    let session = acp.create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "wait for cancellation"}]}
    }))
    .await;
    assert_eq!(acp.next().await["method"], "session/update");
    acp.send(json!({
        "jsonrpc": "2.0",
        "method": "session/cancel",
        "params": {"sessionId": session}
    }))
    .await;
    assert_eq!(acp.next().await["result"]["stopReason"], "cancelled");
    let events = acp.finish().await;
    let interrupt = &event(&events, "interrupt")["value"];
    assert_eq!(interrupt["threadId"], "thread-red-codex");
    assert_eq!(interrupt["turnId"], "turn-red-codex");
}

#[tokio::test]
async fn codex_authentication_failure_is_actionable_and_does_not_start_a_thread() {
    let workspace = tempfile::tempdir().unwrap();
    let mut acp = Harness::start("unauthenticated");
    acp.initialize().await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "session/new",
        "params": {"cwd": workspace.path()}
    }))
    .await;
    let response = acp.next().await;
    assert_eq!(response["error"]["code"], -32_001);
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("codex login"));
    let events = acp.finish().await;
    assert!(events.iter().all(|entry| entry["event"] != "thread"));
}

#[tokio::test]
async fn codex_failed_turn_returns_a_content_free_error() {
    let workspace = tempfile::tempdir().unwrap();
    let mut acp = Harness::start("failed");
    acp.initialize().await;
    let session = acp.create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "trigger a failure"}]}
    }))
    .await;
    let response = acp.next().await;
    assert_eq!(response["error"]["code"], -32_000);
    assert_eq!(response["error"]["message"], "Codex turn failed");
    assert!(!response.to_string().contains("secret backend details"));
    acp.finish().await;
}

#[tokio::test]
async fn codex_app_server_close_completes_the_pending_prompt_without_hanging() {
    let workspace = tempfile::tempdir().unwrap();
    let mut acp = Harness::start("close");
    acp.initialize().await;
    let session = acp.create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "wait for app-server close"}]}
    }))
    .await;
    let response = acp.next().await;
    assert_eq!(response["error"]["message"], "Codex app-server stopped");
    acp.finish().await;
}

#[tokio::test]
async fn codex_incompatible_app_server_fails_closed_before_acp_handshake() {
    let acp = Harness::start("incompatible");
    let output = tokio::time::timeout(TEST_TIMEOUT, acp.child.wait_with_output())
        .await
        .expect("ACP process did not stop after incompatible handshake")
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("required experimental API"));
    assert!(!stderr.contains("experimental API is unavailable"));
}
