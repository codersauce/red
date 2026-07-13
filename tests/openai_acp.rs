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
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter},
    net::TcpListener,
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

struct Harness {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Harness {
    fn start(base_url: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_red_openai_acp"))
            .env("OPENAI_API_KEY", "test-secret-that-must-not-be-logged")
            .env("HTTP_PROXY", "http://127.0.0.1:1")
            .env("HTTPS_PROXY", "http://127.0.0.1:1")
            .env("ALL_PROXY", "http://127.0.0.1:1")
            .env("NO_PROXY", "")
            .arg("--base-url")
            .arg(base_url)
            .arg("--model")
            .arg("test-model")
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

    async fn initialize_and_create_session(&mut self, cwd: &Path) -> String {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": 1, "clientCapabilities": {"fs": {"readTextFile": true, "writeTextFile": true}}}
        }))
        .await;
        let initialized = self.next().await;
        assert_eq!(initialized["result"]["protocolVersion"], 1);
        assert_eq!(initialized["result"]["agentInfo"]["name"], "red-openai-acp");
        assert_eq!(
            initialized["result"]["agentCapabilities"]["sessionCapabilities"]["close"],
            json!({})
        );

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

    async fn finish(mut self) {
        self.stdin.shutdown().await.unwrap();
        drop(self.stdin);
        drop(self.stdout);
        let output = tokio::time::timeout(TEST_TIMEOUT, self.child.wait_with_output())
            .await
            .expect("ACP process did not stop")
            .unwrap();
        assert!(output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("test-secret-that-must-not-be-logged"));
        assert!(!stderr.contains("unsaved buffer contents"));
        assert!(!stderr.contains("staged proposal contents"));
    }
}

async fn start_mock_server(responses: Vec<Value>) -> (String, Arc<Mutex<Vec<Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    tokio::spawn(async move {
        for response in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (headers, body) = read_http_request(&mut socket).await;
            assert!(headers.starts_with("POST /v1/responses HTTP/1.1"));
            assert!(headers
                .to_ascii_lowercase()
                .contains("authorization: bearer test-secret-that-must-not-be-logged"));
            assert!(headers
                .to_ascii_lowercase()
                .contains("accept: text/event-stream"));
            let request: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(request["stream"], true);
            recorded.lock().await.push(request);
            let body = response_stream(&response);
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            socket.write_all(&body).await.unwrap();
            socket.shutdown().await.unwrap();
        }
    });
    (format!("http://{address}/v1"), requests)
}

async fn start_raw_stream(body: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (headers, request) = read_http_request(&mut socket).await;
        assert!(headers
            .to_ascii_lowercase()
            .contains("accept: text/event-stream"));
        assert_eq!(
            serde_json::from_slice::<Value>(&request).unwrap()["stream"],
            true
        );
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: Text/Event-Stream; charset=utf-8\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        socket.write_all(&body).await.unwrap();
        let _ = socket.shutdown().await;
    });
    format!("http://{address}/v1")
}

async fn read_http_request(socket: &mut tokio::net::TcpStream) -> (String, Vec<u8>) {
    let mut encoded = Vec::new();
    let header_end = loop {
        if let Some(end) = encoded.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
        let mut chunk = [0u8; 4096];
        let bytes = socket.read(&mut chunk).await.unwrap();
        assert_ne!(bytes, 0, "mock server received incomplete headers");
        encoded.extend_from_slice(&chunk[..bytes]);
        assert!(encoded.len() < 2 * 1024 * 1024);
    };
    let headers = String::from_utf8(encoded[..header_end].to_vec()).unwrap();
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.split_once(':').and_then(|(key, value)| {
                key.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
        })
        .unwrap();
    while encoded.len() - header_end < content_length {
        let mut chunk = [0u8; 4096];
        let bytes = socket.read(&mut chunk).await.unwrap();
        assert_ne!(bytes, 0, "mock server received incomplete body");
        encoded.extend_from_slice(&chunk[..bytes]);
    }
    (
        headers,
        encoded[header_end..header_end + content_length].to_vec(),
    )
}

fn function_call(call_id: &str, name: &str, arguments: Value) -> Value {
    json!({
        "output": [{
            "type": "function_call",
            "id": format!("fc-{call_id}"),
            "call_id": call_id,
            "name": name,
            "arguments": arguments.to_string()
        }]
    })
}

fn message(text: &str) -> Value {
    json!({
        "output": [{"type": "message", "content": [{"type": "output_text", "text": text}]}]
    })
}

fn response_stream(response: &Value) -> Vec<u8> {
    let mut stream = Vec::new();
    if let Some(output) = response["output"].as_array() {
        for text in output
            .iter()
            .filter(|item| item["type"] == "message")
            .filter_map(|item| item["content"].as_array())
            .flatten()
            .filter(|content| content["type"] == "output_text")
            .filter_map(|content| content["text"].as_str())
        {
            stream.extend_from_slice(&sse_event(
                "response.output_text.delta",
                json!({"type": "response.output_text.delta", "delta": text}),
            ));
        }
    }
    stream.extend_from_slice(&sse_event(
        "response.completed",
        json!({"type": "response.completed", "response": response}),
    ));
    stream
}

fn sse_event(event: &str, data: Value) -> Vec<u8> {
    format!("event: {event}\r\ndata: {data}\r\n\r\n").into_bytes()
}

fn write_target(root: &Path) -> PathBuf {
    root.join("example.rs")
}

#[tokio::test]
async fn responses_text_deltas_reach_acp_before_the_stream_completes() {
    let workspace = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (first_delta_tx, first_delta_rx) = tokio::sync::oneshot::channel();
    let (complete_tx, complete_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (headers, body) = read_http_request(&mut socket).await;
        assert!(headers
            .to_ascii_lowercase()
            .contains("accept: text/event-stream"));
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["stream"],
            true
        );
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let first = sse_event(
            "response.output_text.delta",
            json!({"type": "response.output_text.delta", "delta": "first "}),
        );
        let split = first.len() / 2;
        socket.write_all(&first[..split]).await.unwrap();
        tokio::task::yield_now().await;
        socket.write_all(&first[split..]).await.unwrap();
        let _ = first_delta_tx.send(());
        complete_rx.await.unwrap();
        socket
            .write_all(&sse_event(
                "response.output_text.delta",
                json!({"type": "response.output_text.delta", "delta": "second"}),
            ))
            .await
            .unwrap();
        socket
            .write_all(&sse_event(
                "response.completed",
                json!({"type": "response.completed", "response": message("first second")}),
            ))
            .await
            .unwrap();
        socket.shutdown().await.unwrap();
    });

    let mut acp = Harness::start(&format!("http://{address}/v1"));
    let session = acp.initialize_and_create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "stream the answer"}]}
    }))
    .await;
    tokio::time::timeout(TEST_TIMEOUT, first_delta_rx)
        .await
        .expect("mock OpenAI server did not send its first delta")
        .unwrap();

    let first = acp.next().await;
    assert_eq!(first["method"], "session/update");
    assert_eq!(first["params"]["update"]["content"]["text"], "first ");
    complete_tx.send(()).unwrap();
    let second = acp.next().await;
    assert_eq!(second["method"], "session/update");
    assert_eq!(second["params"]["update"]["content"]["text"], "second");
    assert_eq!(acp.next().await["result"]["stopReason"], "end_turn");
    acp.finish().await;
}

#[tokio::test]
async fn cancellation_interrupts_a_delayed_responses_event_stream_after_a_delta() {
    let workspace = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (first_delta_tx, first_delta_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let _ = read_http_request(&mut socket).await;
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        socket
            .write_all(&sse_event(
                "response.output_text.delta",
                json!({"type": "response.output_text.delta", "delta": "working"}),
            ))
            .await
            .unwrap();
        let _ = first_delta_tx.send(());
        let mut closed = [0u8; 1];
        let _ = socket.read(&mut closed).await;
    });

    let mut acp = Harness::start(&format!("http://{address}/v1"));
    let session = acp.initialize_and_create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "start streaming"}]}
    }))
    .await;
    tokio::time::timeout(TEST_TIMEOUT, first_delta_rx)
        .await
        .expect("mock OpenAI server did not send its first delta")
        .unwrap();
    let update = acp.next().await;
    assert_eq!(update["method"], "session/update");
    assert_eq!(update["params"]["update"]["content"]["text"], "working");

    acp.send(json!({
        "jsonrpc": "2.0",
        "method": "session/cancel",
        "params": {"sessionId": session}
    }))
    .await;
    assert_eq!(acp.next().await["result"]["stopReason"], "cancelled");
    acp.finish().await;
}

#[tokio::test]
async fn cancellation_interrupts_a_responses_stream_stalled_on_acp_output_backpressure() {
    let workspace = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stream_written_tx, stream_written_rx) = tokio::sync::oneshot::channel();
    let (stream_closed_tx, stream_closed_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let _ = read_http_request(&mut socket).await;
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let delta = "x".repeat(8 * 1024);
        let event = sse_event(
            "response.output_text.delta",
            json!({"type": "response.output_text.delta", "delta": delta}),
        );
        for _ in 0..120 {
            socket.write_all(&event).await.unwrap();
        }
        let _ = stream_written_tx.send(());
        let mut closed = [0u8; 1];
        let result = socket.read(&mut closed).await;
        let _ = stream_closed_tx.send(result);
    });

    let mut acp = Harness::start(&format!("http://{address}/v1"));
    let session = acp.initialize_and_create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "fill the ACP output pipe"}]}
    }))
    .await;
    tokio::time::timeout(TEST_TIMEOUT, stream_written_rx)
        .await
        .expect("mock OpenAI server did not send its deltas")
        .unwrap();

    acp.send(json!({
        "jsonrpc": "2.0",
        "method": "session/cancel",
        "params": {"sessionId": session}
    }))
    .await;
    let closed = tokio::time::timeout(TEST_TIMEOUT, stream_closed_rx)
        .await
        .expect("ACP cancellation did not interrupt a backpressured stream")
        .unwrap();
    assert!(
        closed.as_ref().is_ok_and(|bytes| *bytes == 0)
            || closed
                .as_ref()
                .is_err_and(|error| error.kind() == std::io::ErrorKind::ConnectionReset),
        "ACP cancellation did not close the OpenAI stream: {closed:?}"
    );

    let mut updates = 0usize;
    loop {
        let message = acp.next().await;
        if message["id"] == 3 {
            assert_eq!(message["result"]["stopReason"], "cancelled");
            break;
        }
        assert_eq!(message["method"], "session/update");
        updates += 1;
    }
    assert!(updates > 0);
    acp.finish().await;
}

#[tokio::test]
async fn responses_stream_surfaces_failures_and_enforces_output_and_body_bounds() {
    let cases = [
        (
            sse_event(
                "response.failed",
                json!({"type": "response.failed", "response": {"error": {"message": "model failed"}}}),
            ),
            "OpenAI response failed: model failed",
        ),
        (
            sse_event(
                "error",
                json!({"type": "error", "message": "stream transport failed"}),
            ),
            "OpenAI response failed: stream transport failed",
        ),
        (
            sse_event(
                "response.failed",
                json!({"type": "response.failed", "response": {"error": {"message": "x".repeat(1024 * 1024)}}}),
            ),
            "OpenAI response failed:",
        ),
        (
            sse_event(
                "response.output_text.delta",
                json!({"type": "response.output_text.delta", "delta": "x".repeat(960 * 1024 + 1)}),
            ),
            "OpenAI output exceeds",
        ),
        (vec![b'x'; 2 * 1024 * 1024 + 1], "OpenAI response exceeds"),
    ];

    for (stream, expected_error) in cases {
        let workspace = tempfile::tempdir().unwrap();
        let base_url = start_raw_stream(stream).await;
        let mut acp = Harness::start(&base_url);
        let session = acp.initialize_and_create_session(workspace.path()).await;
        acp.send(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/prompt",
            "params": {"sessionId": session, "prompt": [{"type": "text", "text": "test stream failure"}]}
        }))
        .await;

        let response = acp.next().await;
        assert_eq!(response["id"], 3);
        assert_eq!(response["error"]["code"], -32_000);
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains(expected_error));
        assert!(serde_json::to_vec(&response).unwrap().len() < 1024 * 1024);
        acp.finish().await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_a_symlinked_workspace_root() {
    let workspace = tempfile::tempdir().unwrap();
    let linked_root = workspace.path().join("linked-root");
    std::os::unix::fs::symlink(workspace.path(), &linked_root).unwrap();
    let mut acp = Harness::start("http://127.0.0.1:1/v1");

    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {"protocolVersion": 1, "clientCapabilities": {"fs": {"readTextFile": true, "writeTextFile": true}}}
    }))
    .await;
    assert_eq!(acp.next().await["result"]["protocolVersion"], 1);
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "session/new",
        "params": {"cwd": linked_root}
    }))
    .await;

    let response = acp.next().await;
    assert_eq!(response["error"]["code"], -32_602);
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("cannot be a symlink"));
    acp.finish().await;
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_a_workspace_root_below_a_symlinked_parent() {
    let workspace = tempfile::tempdir().unwrap();
    let real_parent = workspace.path().join("real-parent");
    std::fs::create_dir_all(real_parent.join("project")).unwrap();
    let linked_parent = workspace.path().join("linked-parent");
    std::os::unix::fs::symlink(&real_parent, &linked_parent).unwrap();
    let mut acp = Harness::start("http://127.0.0.1:1/v1");

    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {"protocolVersion": 1, "clientCapabilities": {"fs": {"readTextFile": true, "writeTextFile": true}}}
    }))
    .await;
    assert_eq!(acp.next().await["result"]["protocolVersion"], 1);
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "session/new",
        "params": {"cwd": linked_parent.join("project")}
    }))
    .await;

    let response = acp.next().await;
    assert_eq!(response["error"]["code"], -32_602);
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("cannot be a symlink"));
    acp.finish().await;
}

#[cfg(unix)]
#[tokio::test]
async fn openai_rejects_workspace_tools_after_an_ancestor_is_replaced_by_a_symlink() {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("parent");
    let project = parent.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("existing.rs"), "workspace contents\n").unwrap();
    let outside_parent = root.path().join("outside-parent");
    let outside = outside_parent.join("project");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("existing.rs"), "outside secret contents\n").unwrap();
    std::fs::write(outside.join("secret-name.rs"), "outside secret contents\n").unwrap();
    let (base_url, requests) = start_mock_server(vec![
        function_call("list", "list_files", json!({})),
        function_call("search", "search_files", json!({"query": "outside secret"})),
        function_call("read", "read_file", json!({"path": "existing.rs"})),
        function_call(
            "write",
            "write_file",
            json!({"path": "new.rs", "content": "must not be staged"}),
        ),
        message("The workspace is no longer safe to inspect."),
    ])
    .await;
    let mut acp = Harness::start(&base_url);
    let session = acp.initialize_and_create_session(&project).await;
    std::fs::rename(&parent, root.path().join("original-parent")).unwrap();
    std::os::unix::fs::symlink(&outside_parent, &parent).unwrap();

    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "inspect the workspace"}]}
    }))
    .await;

    let update = acp.next().await;
    assert_eq!(update["method"], "session/update");
    assert_eq!(acp.next().await["result"]["stopReason"], "end_turn");
    acp.finish().await;
    assert!(!outside.join("new.rs").exists());

    let requests = requests.lock().await;
    assert_eq!(requests.len(), 5);
    for request in requests.iter().skip(1) {
        let input = request["input"].to_string();
        assert!(input.contains("workspace root cannot contain a symlink"));
        assert!(!input.contains("secret-name.rs"));
        assert!(!input.contains("outside secret contents"));
    }
}

#[tokio::test]
async fn tool_loop_uses_unsaved_reads_and_stages_writes_without_touching_disk() {
    let workspace = tempfile::tempdir().unwrap();
    let target = write_target(workspace.path());
    std::fs::write(&target, "disk contents").unwrap();
    let (base_url, requests) = start_mock_server(vec![
        function_call("read-1", "read_file", json!({"path": "example.rs"})),
        function_call(
            "write-1",
            "write_file",
            json!({"path": "example.rs", "content": "staged proposal contents"}),
        ),
        function_call("read-2", "read_file", json!({"path": "example.rs"})),
        message("Proposal is ready for review."),
        message("The earlier proposal remains in context."),
    ])
    .await;
    let mut acp = Harness::start(&base_url);
    let session = acp.initialize_and_create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "update the file"}]}
    }))
    .await;

    let read = acp.next().await;
    assert_eq!(read["method"], "fs/read_text_file");
    assert_eq!(read["params"]["path"], target.to_string_lossy().as_ref());
    acp.send(json!({"jsonrpc": "2.0", "id": read["id"], "result": {"content": "unsaved buffer contents"}}))
        .await;

    let write = acp.next().await;
    assert_eq!(write["method"], "fs/write_text_file");
    assert_eq!(write["params"]["content"], "staged proposal contents");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "disk contents");
    acp.send(json!({"jsonrpc": "2.0", "id": write["id"], "result": {}}))
        .await;

    let read_after_write = acp.next().await;
    assert_eq!(read_after_write["method"], "fs/read_text_file");
    acp.send(json!({"jsonrpc": "2.0", "id": read_after_write["id"], "result": {"content": "staged proposal contents"}}))
        .await;
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "disk contents");

    let update = acp.next().await;
    assert_eq!(update["method"], "session/update");
    assert_eq!(
        update["params"]["update"]["content"]["text"],
        "Proposal is ready for review."
    );
    assert_eq!(acp.next().await["result"]["stopReason"], "end_turn");
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "summarize the previous edit"}]}
    }))
    .await;
    let follow_up = acp.next().await;
    assert_eq!(follow_up["method"], "session/update");
    assert_eq!(acp.next().await["result"]["stopReason"], "end_turn");
    acp.finish().await;

    let requests = requests.lock().await;
    assert_eq!(requests.len(), 5);
    assert_eq!(requests[0]["model"], "test-model");
    assert_eq!(requests[0]["store"], false);
    assert_eq!(requests[0]["parallel_tool_calls"], false);
    assert!(requests[1].to_string().contains("unsaved buffer contents"));
    assert!(requests[3].to_string().contains("staged proposal contents"));
    let follow_up_input = requests[4]["input"].to_string();
    assert!(follow_up_input.contains("function_call"));
    assert!(follow_up_input.contains("function_call_output"));
    assert!(follow_up_input.contains("staged proposal contents"));
}

#[tokio::test]
async fn escaping_heavy_read_returns_a_bounded_tool_error_and_continues_the_turn() {
    let workspace = tempfile::tempdir().unwrap();
    let (base_url, requests) = start_mock_server(vec![
        function_call("read-1", "read_file", json!({"path": "example.rs"})),
        message("The file was too large to inspect safely."),
    ])
    .await;
    let mut acp = Harness::start(&base_url);
    let session = acp.initialize_and_create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "inspect the file"}]}
    }))
    .await;

    let read = acp.next().await;
    assert_eq!(read["method"], "fs/read_text_file");
    let response = json!({
        "jsonrpc": "2.0",
        "id": read["id"],
        "result": {"content": "\\".repeat(524_000)}
    });
    assert!(serde_json::to_vec(&response).unwrap().len() < 1024 * 1024);
    acp.send(response).await;

    assert_eq!(acp.next().await["method"], "session/update");
    assert_eq!(acp.next().await["result"]["stopReason"], "end_turn");
    acp.finish().await;

    let requests = requests.lock().await;
    assert_eq!(requests.len(), 2);
    let output = requests[1]["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap()["output"]
        .as_str()
        .unwrap();
    let output: Value = serde_json::from_str(output).unwrap();
    assert_eq!(output["ok"], false);
    assert!(output["error"]
        .as_str()
        .unwrap()
        .contains("serialized ACP file content exceeds"));
}

#[tokio::test]
async fn aggregate_read_outputs_are_bounded_before_the_next_responses_request() {
    let workspace = tempfile::tempdir().unwrap();
    let reads = json!({
        "output": [
            {
                "type": "function_call",
                "id": "fc-read-1",
                "call_id": "read-1",
                "name": "read_file",
                "arguments": json!({"path": "first.rs"}).to_string()
            },
            {
                "type": "function_call",
                "id": "fc-read-2",
                "call_id": "read-2",
                "name": "read_file",
                "arguments": json!({"path": "second.rs"}).to_string()
            }
        ]
    });
    let (base_url, requests) = start_mock_server(vec![
        message(&"h".repeat(200 * 1024)),
        reads,
        message("The readable tool result remains available."),
    ])
    .await;
    let mut acp = Harness::start(&base_url);
    let session = acp.initialize_and_create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "remember this context"}]}
    }))
    .await;
    assert_eq!(acp.next().await["method"], "session/update");
    assert_eq!(acp.next().await["result"]["stopReason"], "end_turn");

    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "inspect both files"}]}
    }))
    .await;
    for expected_path in ["first.rs", "second.rs"] {
        let read = acp.next().await;
        assert_eq!(read["method"], "fs/read_text_file");
        assert!(read["params"]["path"]
            .as_str()
            .unwrap()
            .ends_with(expected_path));
        acp.send(json!({
            "jsonrpc": "2.0",
            "id": read["id"],
            "result": {"content": "x".repeat(940 * 1024)}
        }))
        .await;
    }
    assert_eq!(acp.next().await["method"], "session/update");
    assert_eq!(acp.next().await["result"]["stopReason"], "end_turn");
    acp.finish().await;

    let requests = requests.lock().await;
    assert_eq!(requests.len(), 3);
    assert!(serde_json::to_vec(&requests[2]).unwrap().len() <= 2 * 1024 * 1024);
    let outputs = requests[2]["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .map(|item| serde_json::from_str::<Value>(item["output"].as_str().unwrap()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 2);
    assert!(outputs.iter().any(|output| {
        output["ok"] == false
            && output["error"]
                .as_str()
                .unwrap()
                .contains("serialized tool outputs exceed")
    }));
    assert!(outputs.iter().any(|output| output["ok"] == true));
}

#[tokio::test]
async fn oversized_tool_round_drops_old_history_and_preserves_current_reasoning_and_call() {
    let workspace = tempfile::tempdir().unwrap();
    let read = json!({
        "output": [
            {
                "type": "reasoning",
                "id": "rs-read-1",
                "summary": [],
                "encrypted_content": "r".repeat(1850 * 1024)
            },
            {
                "type": "function_call",
                "id": "fc-read-1",
                "call_id": "read-1",
                "name": "read_file",
                "arguments": json!({"path": "first.rs"}).to_string()
            }
        ]
    });
    let (base_url, requests) = start_mock_server(vec![
        message(&"h".repeat(200 * 1024)),
        read,
        message("The current tool round completed."),
    ])
    .await;
    let mut acp = Harness::start(&base_url);
    let session = acp.initialize_and_create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "remember this context"}]}
    }))
    .await;
    assert_eq!(acp.next().await["method"], "session/update");
    assert_eq!(acp.next().await["result"]["stopReason"], "end_turn");

    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "inspect the current file"}]}
    }))
    .await;
    let callback = acp.next().await;
    assert_eq!(callback["method"], "fs/read_text_file");
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": callback["id"],
        "result": {"content": "x".repeat(940 * 1024)}
    }))
    .await;
    assert_eq!(acp.next().await["method"], "session/update");
    assert_eq!(acp.next().await["result"]["stopReason"], "end_turn");
    acp.finish().await;

    let requests = requests.lock().await;
    assert_eq!(requests.len(), 3);
    assert!(serde_json::to_vec(&requests[2]).unwrap().len() <= 2 * 1024 * 1024);
    let input = requests[2]["input"].as_array().unwrap();
    assert!(input
        .iter()
        .any(|item| item["role"] == "user" && item["content"] == "inspect the current file"));
    assert!(input
        .iter()
        .all(|item| item["content"] != "remember this context"));
    let reasoning = input
        .iter()
        .find(|item| item["type"] == "reasoning")
        .unwrap();
    assert_eq!(
        reasoning["encrypted_content"].as_str().unwrap().len(),
        1850 * 1024
    );
    assert!(input
        .iter()
        .any(|item| item["type"] == "function_call" && item["call_id"] == "read-1"));
    let output = input
        .iter()
        .find(|item| item["type"] == "function_call_output" && item["call_id"] == "read-1")
        .unwrap()["output"]
        .as_str()
        .unwrap();
    let output: Value = serde_json::from_str(output).unwrap();
    assert_eq!(output["ok"], false);
    assert!(output["error"]
        .as_str()
        .unwrap()
        .contains("serialized tool outputs exceed"));
}

#[tokio::test]
async fn rejected_and_traversal_writes_have_no_local_fallback() {
    let workspace = tempfile::tempdir().unwrap();
    let target = write_target(workspace.path());
    let outside_name = format!("red-acp-outside-{}.txt", uuid::Uuid::new_v4());
    let outside = workspace.path().parent().unwrap().join(&outside_name);
    std::fs::write(&target, "disk contents").unwrap();
    let (base_url, requests) = start_mock_server(vec![
        function_call(
            "write-rejected",
            "write_file",
            json!({"path": "example.rs", "content": "must not reach disk"}),
        ),
        function_call(
            "write-outside",
            "write_file",
            json!({"path": format!("../{outside_name}"), "content": "must not be created"}),
        ),
        message("The editor rejected the edit."),
    ])
    .await;
    let mut acp = Harness::start(&base_url);
    let session = acp.initialize_and_create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "try the edit"}]}
    }))
    .await;

    let rejected = acp.next().await;
    assert_eq!(rejected["method"], "fs/write_text_file");
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": rejected["id"],
        "error": {"code": -32000, "message": "write rejected by editor"}
    }))
    .await;
    let update = acp.next().await;
    assert_eq!(update["method"], "session/update");
    assert_eq!(acp.next().await["result"]["stopReason"], "end_turn");
    assert_eq!(std::fs::read_to_string(target).unwrap(), "disk contents");
    assert!(!outside.exists());
    acp.finish().await;

    let requests = requests.lock().await;
    assert_eq!(requests.len(), 3);
    assert!(requests[1].to_string().contains("write rejected by editor"));
    assert!(requests[2]
        .to_string()
        .contains("workspace path contains parent traversal"));
}

#[tokio::test]
async fn cancellation_interrupts_an_in_flight_openai_request() {
    let workspace = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (request_seen_tx, request_seen_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let _ = read_http_request(&mut socket).await;
        let _ = request_seen_tx.send(());
        let mut closed = [0u8; 1];
        let _ = socket.read(&mut closed).await;
    });
    let mut acp = Harness::start(&format!("http://{address}/v1"));
    let session = acp.initialize_and_create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "wait for cancellation"}]}
    }))
    .await;
    tokio::time::timeout(TEST_TIMEOUT, request_seen_rx)
        .await
        .expect("mock OpenAI server did not receive a request")
        .unwrap();
    acp.send(json!({
        "jsonrpc": "2.0",
        "method": "session/cancel",
        "params": {"sessionId": session}
    }))
    .await;

    assert_eq!(acp.next().await["result"]["stopReason"], "cancelled");
    acp.finish().await;
}

#[tokio::test]
async fn closing_an_openai_session_frees_capacity_and_rejects_the_old_session() {
    let workspace = tempfile::tempdir().unwrap();
    let mut acp = Harness::start("http://127.0.0.1:1/v1");
    let first = acp.initialize_and_create_session(workspace.path()).await;

    for id in 3..=65 {
        acp.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/new",
            "params": {"cwd": workspace.path()}
        }))
        .await;
        assert!(acp.next().await["result"]["sessionId"].is_string());
    }
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 66,
        "method": "session/new",
        "params": {"cwd": workspace.path()}
    }))
    .await;
    assert_eq!(
        acp.next().await["error"]["message"],
        "ACP session capacity reached"
    );

    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 67,
        "method": "session/close",
        "params": {"sessionId": first}
    }))
    .await;
    assert_eq!(acp.next().await["result"], json!({}));
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 68,
        "method": "session/prompt",
        "params": {"sessionId": first, "prompt": [{"type": "text", "text": "must fail"}]}
    }))
    .await;
    assert_eq!(acp.next().await["error"]["message"], "unknown ACP session");
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 69,
        "method": "session/new",
        "params": {"cwd": workspace.path()}
    }))
    .await;
    assert!(acp.next().await["result"]["sessionId"].is_string());
    acp.finish().await;
}

#[tokio::test]
async fn closing_an_openai_session_cancels_an_in_flight_prompt() {
    let workspace = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (request_seen_tx, request_seen_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let _ = read_http_request(&mut socket).await;
        let _ = request_seen_tx.send(());
        let mut closed = [0u8; 1];
        let _ = socket.read(&mut closed).await;
    });
    let mut acp = Harness::start(&format!("http://{address}/v1"));
    let session = acp.initialize_and_create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "wait for close"}]}
    }))
    .await;
    tokio::time::timeout(TEST_TIMEOUT, request_seen_rx)
        .await
        .expect("mock OpenAI server did not receive a request")
        .unwrap();

    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "session/close",
        "params": {"sessionId": session}
    }))
    .await;
    let responses = [acp.next().await, acp.next().await];

    assert!(responses
        .iter()
        .any(|response| response["id"] == 3 && response["result"]["stopReason"] == "cancelled"));
    assert!(responses
        .iter()
        .any(|response| response["id"] == 4 && response["result"] == json!({})));
    acp.finish().await;
}

#[tokio::test]
async fn closing_an_openai_session_cancels_a_stalled_response_body() {
    let workspace = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (body_started_tx, body_started_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let _ = read_http_request(&mut socket).await;
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 1024\r\n\r\n{\"output\":[",
            )
            .await
            .unwrap();
        let _ = body_started_tx.send(());
        let mut closed = [0u8; 1];
        let _ = socket.read(&mut closed).await;
    });
    let mut acp = Harness::start(&format!("http://{address}/v1"));
    let session = acp.initialize_and_create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "wait for body"}]}
    }))
    .await;
    tokio::time::timeout(TEST_TIMEOUT, body_started_rx)
        .await
        .expect("mock OpenAI server did not start the response body")
        .unwrap();

    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "session/close",
        "params": {"sessionId": session}
    }))
    .await;
    let responses = [acp.next().await, acp.next().await];
    assert!(responses
        .iter()
        .any(|response| response["id"] == 3 && response["result"]["stopReason"] == "cancelled"));
    assert!(responses
        .iter()
        .any(|response| response["id"] == 4 && response["result"] == json!({})));
    acp.finish().await;
}

#[tokio::test]
async fn closing_an_openai_session_releases_a_pending_filesystem_callback() {
    let workspace = tempfile::tempdir().unwrap();
    let (base_url, _requests) = start_mock_server(vec![function_call(
        "read-pending",
        "read_file",
        json!({"path": "example.rs"}),
    )])
    .await;
    let mut acp = Harness::start(&base_url);
    let session = acp.initialize_and_create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "read a file"}]}
    }))
    .await;
    let callback = acp.next().await;
    assert_eq!(callback["method"], "fs/read_text_file");

    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "session/close",
        "params": {"sessionId": session}
    }))
    .await;
    let responses = [acp.next().await, acp.next().await];
    assert!(responses
        .iter()
        .any(|response| response["id"] == 3 && response["result"]["stopReason"] == "cancelled"));
    assert!(responses
        .iter()
        .any(|response| response["id"] == 4 && response["result"] == json!({})));

    acp.send(json!({"jsonrpc": "2.0", "id": callback["id"], "result": {"content": "late"}}))
        .await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "session/new",
        "params": {"cwd": workspace.path()}
    }))
    .await;
    assert!(acp.next().await["result"]["sessionId"].is_string());
    acp.finish().await;
}

#[tokio::test]
async fn first_party_adapter_round_trips_the_real_proposal_host() {
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
    let (base_url, requests) = start_mock_server(vec![
        function_call("read-existing", "read_file", json!({"path": "existing.rs"})),
        function_call("read-new", "read_file", json!({"path": "new.rs"})),
        function_call(
            "write-existing",
            "write_file",
            json!({"path": "existing.rs", "content": "staged existing contents\n"}),
        ),
        function_call(
            "write-new",
            "write_file",
            json!({"path": "new.rs", "content": "staged new contents\n"}),
        ),
        function_call(
            "read-existing-after-write",
            "read_file",
            json!({"path": "existing.rs"}),
        ),
        function_call(
            "read-new-after-write",
            "read_file",
            json!({"path": "new.rs"}),
        ),
        message("The proposal is ready."),
    ])
    .await;
    let mut acp = Harness::start(&base_url);
    let session = acp.initialize_and_create_session(workspace.path()).await;
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
        let result = host.read_text_file(request).await.unwrap();
        let result = serde_json::to_value(result).unwrap();
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
        let result = host.write_text_file(request).await.unwrap();
        acp.send(json!({"jsonrpc": "2.0", "id": write["id"], "result": serde_json::to_value(result).unwrap()}))
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
        let result = host.read_text_file(request).await.unwrap();
        let result = serde_json::to_value(result).unwrap();
        assert_eq!(result["content"], contents);
        acp.send(json!({"jsonrpc": "2.0", "id": read["id"], "result": result}))
            .await;
    }

    assert_eq!(acp.next().await["method"], "session/update");
    assert_eq!(acp.next().await["result"]["stopReason"], "end_turn");
    acp.finish().await;
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 7);
    assert!(requests[1]
        .to_string()
        .contains("unsaved existing contents"));
    assert!(requests[5].to_string().contains("staged existing contents"));
    assert!(requests[6].to_string().contains("staged new contents"));

    let mut proposals = proposal_workspace.lock().unwrap();
    let disposition = proposals
        .accept_all(&session, &existing, 7, "unsaved existing contents\n")
        .unwrap();
    assert!(matches!(
        disposition,
        ProposalDisposition::Applied { contents, created: false, .. }
            if contents == "staged existing contents\n"
    ));
    assert_eq!(proposals.pending_files(&session), vec![created.clone()]);
    proposals.reject_all(&session, &created, 0, "").unwrap();
    assert!(proposals.pending_files(&session).is_empty());
    assert_eq!(
        std::fs::read_to_string(existing).unwrap(),
        "disk contents\n"
    );
    assert!(!created.exists());
}
