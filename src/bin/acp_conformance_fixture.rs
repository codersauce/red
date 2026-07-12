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
                if mode == "close-supported" || mode == "ignore-close" {
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
                    NewSessionResponse::new("fixture-session"),
                )?;
                if mode == "stop-reading" {
                    std::thread::sleep(std::time::Duration::from_secs(30));
                }
            }
            Some("session/prompt") => {
                prompt_request_id = Some(request_id(&message)?);
                if mode == "ignore-cancel" {
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
            }
            Some("session/close") => {
                if mode == "ignore-close" {
                    continue;
                }
                anyhow::ensure!(
                    mode == "close-supported",
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
            _ => anyhow::bail!("unexpected fixture message: {message}"),
        }
    }

    anyhow::ensure!(
        mode != "close-supported" || session_closed,
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
