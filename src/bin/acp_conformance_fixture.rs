//! Live ACP fixture used by Red's transport conformance test.

use std::io::{self, BufRead, Write};

use agent_client_protocol_schema::{
    v1::{
        AgentCapabilities, AuthMethod, AuthMethodAgent, AuthenticateResponse, CloseSessionResponse,
        InitializeResponse, NewSessionResponse, PromptResponse, SessionCapabilities,
        SessionCloseCapabilities, StopReason,
    },
    ProtocolVersion,
};
use serde::Serialize;
use serde_json::{json, Value};

const READ_REQUEST_ID: i64 = 10_001;
const WRITE_REQUEST_ID: i64 = 10_002;
const PERMISSION_REQUEST_ID: i64 = 10_003;
const RECOVERY_READ_REQUEST_ID: i64 = 10_004;
const CANCELLED_WRITE_REQUEST_ID: i64 = 10_005;
const REPLACEMENT_WRITE_REQUEST_ID: i64 = 10_006;
const EDITOR_TOOL_REQUEST_ID: i64 = 10_007;
const DELAYED_RESPONSE_MILLIS: u64 = 1_200;

fn main() -> anyhow::Result<()> {
    if let Some(path) = std::env::var_os("RED_ACP_FIXTURE_PID_FILE") {
        std::fs::write(path, std::process::id().to_string())?;
    }
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    let mut prompt_request_id = None;
    let mode = std::env::var("RED_ACP_FIXTURE_MODE").unwrap_or_default();
    let mut delayed_setup = false;
    let mut authenticated = false;
    let mut session_closed = false;
    let mut session_attempts = 0usize;

    for line in stdin.lock().lines() {
        let message: Value = serde_json::from_str(&line?)?;
        match message.get("method").and_then(Value::as_str) {
            Some("initialize") => {
                let mut response = InitializeResponse::new(ProtocolVersion::V1);
                if mode == "require-auth" {
                    response = response.auth_methods(vec![AuthMethod::Agent(
                        AuthMethodAgent::new("fixture_api_key", "Fixture API key"),
                    )]);
                }
                if matches!(
                    mode.as_str(),
                    "close-supported"
                        | "ignore-close"
                        | "close-error"
                        | "reuse-after-close"
                        | "exhaust-cancellations"
                ) {
                    response =
                        response.agent_capabilities(AgentCapabilities::new().session_capabilities(
                            SessionCapabilities::new().close(SessionCloseCapabilities::new()),
                        ));
                }
                write_result(&mut stdout, request_id(&message)?, response)?;
            }
            Some("authenticate") => {
                anyhow::ensure!(
                    mode == "require-auth" && message["params"]["methodId"] == "fixture_api_key",
                    "fixture received an unexpected authentication request"
                );
                authenticated = true;
                write_result(
                    &mut stdout,
                    request_id(&message)?,
                    AuthenticateResponse::new(),
                )?;
            }
            Some("session/new") => {
                session_attempts += 1;
                if mode == "reject-second-session" && session_attempts == 2 {
                    write_value(
                        &mut stdout,
                        json!({
                            "jsonrpc": "2.0",
                            "id": request_id(&message)?,
                            "error": {"code": -32000, "message": "fixture rejected session"}
                        }),
                    )?;
                    continue;
                }
                if mode == "noisy-stderr" {
                    eprintln!("fixture-stderr-must-not-reach-the-terminal");
                }
                anyhow::ensure!(
                    mode != "require-auth" || authenticated,
                    "fixture session started before authentication"
                );
                if mode == "ignore-setup" {
                    continue;
                }
                if mode == "late-setup" && !delayed_setup {
                    delayed_setup = true;
                    std::thread::sleep(std::time::Duration::from_millis(DELAYED_RESPONSE_MILLIS));
                }
                write_result(
                    &mut stdout,
                    request_id(&message)?,
                    NewSessionResponse::new(if mode == "exhaust-cancellations" {
                        format!("fixture-session-{session_attempts}")
                    } else if matches!(
                        mode.as_str(),
                        "ignore-cancel" | "write-after-replacement-prompt"
                    ) && session_attempts > 1
                    {
                        "fixture-session-2".to_string()
                    } else {
                        "fixture-session".to_string()
                    }),
                )?;
                if mode == "stop-reading" {
                    std::thread::sleep(std::time::Duration::from_secs(30));
                }
            }
            Some("session/prompt") => {
                prompt_request_id = Some(request_id(&message)?);
                if mode == "editor-tools" {
                    write_value(
                        &mut stdout,
                        json!({
                            "jsonrpc": "2.0",
                            "id": EDITOR_TOOL_REQUEST_ID,
                            "method": "_red.dev/editor/tool",
                            "params": {
                                "sessionId": "fixture-session",
                                "tool": "get_editor_state"
                            }
                        }),
                    )?;
                    continue;
                }
                if mode == "editor-tool-after-prompt" {
                    write_result(
                        &mut stdout,
                        prompt_request_id
                            .take()
                            .expect("prompt id was just recorded"),
                        PromptResponse::new(StopReason::EndTurn),
                    )?;
                    write_value(
                        &mut stdout,
                        json!({
                            "jsonrpc": "2.0",
                            "id": EDITOR_TOOL_REQUEST_ID,
                            "method": "_red.dev/editor/tool",
                            "params": {
                                "sessionId": "fixture-session",
                                "tool": "get_editor_state"
                            }
                        }),
                    )?;
                    continue;
                }
                if mode == "exhaust-cancellations" {
                    write_value(
                        &mut stdout,
                        json!({
                            "jsonrpc": "2.0",
                            "id": CANCELLED_WRITE_REQUEST_ID,
                            "method": "fs/write_text_file",
                            "params": {
                                "sessionId": message["params"]["sessionId"],
                                "path": "/workspace/evicted.rs",
                                "content": "must not be staged"
                            }
                        }),
                    )?;
                    write_result(
                        &mut stdout,
                        prompt_request_id
                            .take()
                            .expect("prompt id was just recorded"),
                        PromptResponse::new(StopReason::EndTurn),
                    )?;
                    continue;
                }
                if mode == "reuse-after-close" {
                    anyhow::ensure!(
                        session_closed && message["params"]["sessionId"] == "fixture-session",
                        "fixture received a prompt before the reused session was closed"
                    );
                    write_result(
                        &mut stdout,
                        prompt_request_id
                            .take()
                            .expect("prompt id was just recorded"),
                        PromptResponse::new(StopReason::EndTurn),
                    )?;
                    continue;
                }
                if mode == "write-after-replacement-prompt" {
                    let session_id = message["params"]["sessionId"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("fixture prompt has no session id"))?;
                    if session_id == "fixture-session" {
                        continue;
                    }
                    anyhow::ensure!(
                        session_id == "fixture-session-2",
                        "fixture received an unexpected replacement session id"
                    );
                    write_value(
                        &mut stdout,
                        json!({
                            "jsonrpc": "2.0",
                            "id": CANCELLED_WRITE_REQUEST_ID,
                            "method": "fs/write_text_file",
                            "params": {
                                "sessionId": "fixture-session",
                                "path": "/workspace/cancelled.rs",
                                "content": "must not be staged"
                            }
                        }),
                    )?;
                    continue;
                }
                if mode == "ignore-cancel" || mode == "write-after-cancel" {
                    continue;
                }
                if mode == "delayed-prompt" {
                    std::thread::sleep(std::time::Duration::from_millis(DELAYED_RESPONSE_MILLIS));
                    write_result(
                        &mut stdout,
                        prompt_request_id
                            .take()
                            .expect("prompt id was just recorded"),
                        PromptResponse::new(StopReason::EndTurn),
                    )?;
                    continue;
                }
                let params = if mode == "invalid-params-recovery" {
                    json!({ "sessionId": "fixture-session" })
                } else if mode == "host-failure-recovery" {
                    json!({
                        "sessionId": "fixture-session",
                        "path": "/outside-workspace/private.txt"
                    })
                } else {
                    json!({
                        "sessionId": "fixture-session",
                        "path": "/workspace/example.rs"
                    })
                };
                write_value(
                    &mut stdout,
                    json!({
                        "jsonrpc": "2.0",
                        "id": READ_REQUEST_ID,
                        "method": "fs/read_text_file",
                        "params": params
                    }),
                )?;
            }
            Some("session/cancel") => {
                if mode == "ignore-cancel" {
                    continue;
                }
                if let Some(id) = prompt_request_id.take() {
                    write_result(&mut stdout, id, PromptResponse::new(StopReason::Cancelled))?;
                }
                if mode == "write-after-cancel" {
                    write_value(
                        &mut stdout,
                        json!({
                            "jsonrpc": "2.0",
                            "id": CANCELLED_WRITE_REQUEST_ID,
                            "method": "fs/write_text_file",
                            "params": {
                                "sessionId": "fixture-session",
                                "path": "/workspace/cancelled.rs",
                                "content": "must not be staged"
                            }
                        }),
                    )?;
                }
            }
            Some("session/close") => {
                if mode == "ignore-close" {
                    continue;
                }
                if mode == "close-error" || mode == "exhaust-cancellations" {
                    write_value(
                        &mut stdout,
                        json!({
                            "jsonrpc": "2.0",
                            "id": request_id(&message)?,
                            "error": {"code": -32000, "message": "fixture rejected close"}
                        }),
                    )?;
                    continue;
                }
                anyhow::ensure!(
                    mode == "close-supported" || mode == "reuse-after-close",
                    "fixture received an unadvertised session/close request"
                );
                anyhow::ensure!(
                    message["params"]["sessionId"] == "fixture-session",
                    "fixture received an unexpected session id"
                );
                session_closed = true;
                write_result(
                    &mut stdout,
                    request_id(&message)?,
                    CloseSessionResponse::new(),
                )?;
                if mode == "reuse-after-close" {
                    write_value(
                        &mut stdout,
                        json!({
                            "jsonrpc": "2.0",
                            "id": CANCELLED_WRITE_REQUEST_ID,
                            "method": "fs/write_text_file",
                            "params": {
                                "sessionId": "fixture-session",
                                "path": "/workspace/closed.rs",
                                "content": "must not be staged"
                            }
                        }),
                    )?;
                }
            }
            None if message.get("id") == Some(&json!(READ_REQUEST_ID)) => {
                if mode == "invalid-params-recovery" || mode == "host-failure-recovery" {
                    let expected_code = if mode == "invalid-params-recovery" {
                        -32_602
                    } else {
                        -32_000
                    };
                    anyhow::ensure!(
                        message["error"]["code"] == expected_code,
                        "client did not return the expected host error: {message}"
                    );
                    write_value(
                        &mut stdout,
                        json!({
                            "jsonrpc": "2.0",
                            "id": RECOVERY_READ_REQUEST_ID,
                            "method": "fs/read_text_file",
                            "params": {
                                "sessionId": "fixture-session",
                                "path": "/workspace/example.rs"
                            }
                        }),
                    )?;
                    continue;
                }
                anyhow::ensure!(
                    message["result"]["content"] == "unsaved buffer contents",
                    "client filesystem did not expose unsaved contents"
                );
                write_value(
                    &mut stdout,
                    json!({
                        "jsonrpc": "2.0",
                        "id": WRITE_REQUEST_ID,
                        "method": "fs/write_text_file",
                        "params": {
                            "sessionId": "fixture-session",
                            "path": "/workspace/example.rs",
                            "content": "proposed contents"
                        }
                    }),
                )?;
            }
            None if message.get("id") == Some(&json!(RECOVERY_READ_REQUEST_ID)) => {
                anyhow::ensure!(
                    message["result"]["content"] == "unsaved buffer contents",
                    "client did not recover after a rejected host request"
                );
                write_result(
                    &mut stdout,
                    prompt_request_id.take().ok_or_else(|| {
                        anyhow::anyhow!("recovery response arrived before prompt")
                    })?,
                    PromptResponse::new(StopReason::EndTurn),
                )?;
            }
            None if message.get("id") == Some(&json!(WRITE_REQUEST_ID)) => {
                write_value(
                    &mut stdout,
                    json!({
                        "jsonrpc": "2.0",
                        "id": PERMISSION_REQUEST_ID,
                        "method": "session/request_permission",
                        "params": {
                            "sessionId": "fixture-session",
                            "toolCall": { "toolCallId": "tool-1" },
                            "options": [{
                                "optionId": "allow-once",
                                "name": "Allow once",
                                "kind": "allow_once"
                            }]
                        }
                    }),
                )?;
            }
            None if message.get("id") == Some(&json!(CANCELLED_WRITE_REQUEST_ID)) => {
                anyhow::ensure!(
                    message["error"]["code"] == -32_000,
                    "client accepted a filesystem write after cancellation: {message}"
                );
                if mode == "write-after-replacement-prompt" {
                    write_value(
                        &mut stdout,
                        json!({
                            "jsonrpc": "2.0",
                            "id": REPLACEMENT_WRITE_REQUEST_ID,
                            "method": "fs/write_text_file",
                            "params": {
                                "sessionId": "fixture-session-2",
                                "path": "/workspace/replacement.rs",
                                "content": "replacement proposal"
                            }
                        }),
                    )?;
                }
            }
            None if message.get("id") == Some(&json!(REPLACEMENT_WRITE_REQUEST_ID)) => {
                anyhow::ensure!(
                    message.get("result").is_some(),
                    "client rejected a filesystem write from the replacement session: {message}"
                );
                write_result(
                    &mut stdout,
                    prompt_request_id.take().ok_or_else(|| {
                        anyhow::anyhow!("replacement write arrived before prompt")
                    })?,
                    PromptResponse::new(StopReason::EndTurn),
                )?;
            }
            None if message.get("id") == Some(&json!(PERMISSION_REQUEST_ID)) => {
                anyhow::ensure!(
                    message["result"]["outcome"]["outcome"] == "selected",
                    "client did not select a permission option"
                );
                write_value(
                    &mut stdout,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": "fixture-session",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {
                                    "type": "text",
                                    "text": "fixture streamed update"
                                }
                            }
                        }
                    }),
                )?;
            }
            None if message.get("id") == Some(&json!(EDITOR_TOOL_REQUEST_ID)) => {
                if mode == "editor-tool-after-prompt" {
                    anyhow::ensure!(
                        message["error"]["code"] == -32_000,
                        "client accepted an editor tool outside an active prompt: {message}"
                    );
                    continue;
                }
                anyhow::ensure!(
                    message["result"]["file"] == "example.rs",
                    "client did not return the expected editor-tool result: {message}"
                );
                write_result(
                    &mut stdout,
                    prompt_request_id
                        .take()
                        .ok_or_else(|| anyhow::anyhow!("editor tool returned before prompt"))?,
                    PromptResponse::new(StopReason::EndTurn),
                )?;
            }
            _ => anyhow::bail!("unexpected fixture message: {message}"),
        }
    }

    anyhow::ensure!(
        !matches!(mode.as_str(), "close-supported" | "reuse-after-close") || session_closed,
        "fixture did not receive the advertised session/close request"
    );

    Ok(())
}

fn request_id(message: &Value) -> anyhow::Result<Value> {
    message
        .get("id")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("request has no id"))
}

fn write_result(stdout: &mut impl Write, id: Value, result: impl Serialize) -> anyhow::Result<()> {
    write_value(
        stdout,
        json!({ "jsonrpc": "2.0", "id": id, "result": result }),
    )
}

fn write_value(stdout: &mut impl Write, value: Value) -> anyhow::Result<()> {
    serde_json::to_writer(&mut *stdout, &value)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}
