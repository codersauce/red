//! Live ACP fixture used by Red's transport conformance test.

use std::io::{self, BufRead, Write};

use agent_client_protocol_schema::{
    v1::{InitializeResponse, NewSessionResponse, PromptResponse, StopReason},
    ProtocolVersion,
};
use serde::Serialize;
use serde_json::{json, Value};

const READ_REQUEST_ID: i64 = 10_001;
const WRITE_REQUEST_ID: i64 = 10_002;
const PERMISSION_REQUEST_ID: i64 = 10_003;

fn main() -> anyhow::Result<()> {
    if let Some(path) = std::env::var_os("RED_ACP_FIXTURE_PID_FILE") {
        std::fs::write(path, std::process::id().to_string())?;
    }
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    let mut prompt_request_id = None;

    for line in stdin.lock().lines() {
        let message: Value = serde_json::from_str(&line?)?;
        match message.get("method").and_then(Value::as_str) {
            Some("initialize") => write_result(
                &mut stdout,
                request_id(&message)?,
                InitializeResponse::new(ProtocolVersion::V1),
            )?,
            Some("session/new") => write_result(
                &mut stdout,
                request_id(&message)?,
                NewSessionResponse::new("fixture-session"),
            )?,
            Some("session/prompt") => {
                prompt_request_id = Some(request_id(&message)?);
                write_value(
                    &mut stdout,
                    json!({
                        "jsonrpc": "2.0",
                        "id": READ_REQUEST_ID,
                        "method": "fs/read_text_file",
                        "params": {
                            "sessionId": "fixture-session",
                            "path": "/workspace/example.rs"
                        }
                    }),
                )?;
            }
            Some("session/cancel") => {
                let id = prompt_request_id
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("cancel received before prompt"))?;
                write_result(&mut stdout, id, PromptResponse::new(StopReason::Cancelled))?;
            }
            None if message.get("id") == Some(&json!(READ_REQUEST_ID)) => {
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
