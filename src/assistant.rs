//! Codex-backed assistant sessions exposed to Red features and plugins.

use std::{
    collections::{HashMap, VecDeque},
    process::Stdio,
};

use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::mpsc,
};

const PROMPT_REQUEST_BEGIN: &str = "## My request for Codex:";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AssistantConversationOptions {
    pub cwd: String,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AssistantTurnOptions {
    pub conversation_id: String,
    pub prompt: String,
    #[serde(default)]
    pub sink: Option<AssistantTextSink>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AssistantTextSink {
    pub panel_id: String,
    pub block_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct AssistantConversation {
    pub id: String,
    pub preview: String,
    pub name: Option<String>,
    pub cwd: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct AssistantTurn {
    pub id: String,
    pub conversation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct AssistantHistory {
    pub conversation: AssistantConversation,
    pub blocks: Vec<AssistantHistoryBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct AssistantHistoryBlock {
    pub id: String,
    pub kind: AssistantHistoryBlockKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantHistoryBlockKind {
    User,
    Agent,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantEventKind {
    Delta { delta: String },
    Completed,
    Error { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct AssistantEvent {
    pub conversation_id: String,
    pub turn_id: String,
    pub kind: AssistantEventKind,
    #[serde(skip_serializing)]
    pub sink: Option<AssistantTextSink>,
}

struct ConversationState {
    thread_id: String,
    cwd: String,
    loaded: bool,
}

struct TurnState {
    conversation_id: String,
    sink: Option<AssistantTextSink>,
}

/// Owns one lazy Codex app-server connection and its normalized session state.
pub struct AssistantService {
    client: CodexAppServerClient,
    conversations: HashMap<String, ConversationState>,
    turns: HashMap<String, TurnState>,
}

impl AssistantService {
    pub async fn start() -> anyhow::Result<Self> {
        Ok(Self {
            client: CodexAppServerClient::start().await?,
            conversations: HashMap::new(),
            turns: HashMap::new(),
        })
    }

    pub async fn create_conversation(
        &mut self,
        owner: &str,
        options: AssistantConversationOptions,
    ) -> anyhow::Result<AssistantConversation> {
        let result = self
            .client
            .request(
                "thread/start",
                json!({
                    "cwd": options.cwd,
                    "approvalPolicy": "never",
                    "sandbox": "read-only",
                    "ephemeral": false,
                    "threadSource": format!("red:{owner}"),
                }),
            )
            .await?;
        let thread = result
            .get("thread")
            .ok_or_else(|| anyhow!("thread/start response omitted thread"))?;
        let conversation = conversation_from_thread(thread)?;
        self.conversations.insert(
            conversation.id.clone(),
            ConversationState {
                thread_id: conversation.id.clone(),
                cwd: conversation.cwd.clone(),
                loaded: true,
            },
        );
        if let Some(title) = options.title.filter(|title| !title.trim().is_empty()) {
            let _ = self
                .client
                .request(
                    "thread/name/set",
                    json!({ "threadId": conversation.id, "name": title }),
                )
                .await;
        }
        Ok(conversation)
    }

    pub async fn list_conversations(
        &mut self,
        owner: &str,
        cwd: &str,
    ) -> anyhow::Result<Vec<AssistantConversation>> {
        let result = self
            .client
            .request(
                "thread/list",
                json!({
                    "limit": 100,
                    "sortKey": "recency_at",
                    "sortDirection": "desc",
                    "sourceKinds": ["appServer"],
                    "cwd": cwd,
                }),
            )
            .await?;
        let expected_source = format!("red:{owner}");
        let threads = result
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("thread/list response omitted data"))?;
        threads
            .iter()
            .filter(|thread| {
                thread
                    .get("threadSource")
                    .and_then(Value::as_str)
                    .is_some_and(|source| source == expected_source)
            })
            .map(conversation_from_thread)
            .collect()
    }

    pub async fn read_conversation(
        &mut self,
        conversation_id: &str,
    ) -> anyhow::Result<AssistantHistory> {
        let result = self
            .client
            .request(
                "thread/read",
                json!({ "threadId": conversation_id, "includeTurns": true }),
            )
            .await?;
        let thread = result
            .get("thread")
            .ok_or_else(|| anyhow!("thread/read response omitted thread"))?;
        let conversation = conversation_from_thread(thread)?;
        self.conversations.insert(
            conversation.id.clone(),
            ConversationState {
                thread_id: conversation.id.clone(),
                cwd: conversation.cwd.clone(),
                loaded: false,
            },
        );
        let blocks = history_blocks(thread);
        Ok(AssistantHistory {
            conversation,
            blocks,
        })
    }

    pub async fn start_turn(
        &mut self,
        options: AssistantTurnOptions,
    ) -> anyhow::Result<AssistantTurn> {
        let Some(conversation) = self.conversations.get_mut(&options.conversation_id) else {
            return Err(anyhow!("unknown assistant conversation"));
        };
        if !conversation.loaded {
            self.client
                .request(
                    "thread/resume",
                    json!({
                        "threadId": conversation.thread_id,
                        "cwd": conversation.cwd,
                        "approvalPolicy": "never",
                        "sandbox": "read-only",
                    }),
                )
                .await?;
            conversation.loaded = true;
        }

        let result = self
            .client
            .request(
                "turn/start",
                json!({
                    "threadId": conversation.thread_id,
                    "input": [{
                        "type": "text",
                        "text": options.prompt,
                        "textElements": [],
                    }],
                }),
            )
            .await?;
        let turn_id = result
            .get("turn")
            .and_then(|turn| turn.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("turn/start response omitted turn id"))?
            .to_string();
        self.turns.insert(
            turn_id.clone(),
            TurnState {
                conversation_id: options.conversation_id.clone(),
                sink: options.sink,
            },
        );
        Ok(AssistantTurn {
            id: turn_id,
            conversation_id: options.conversation_id,
        })
    }

    pub async fn interrupt_turn(&mut self, turn_id: &str) -> anyhow::Result<()> {
        let Some(turn) = self.turns.get(turn_id) else {
            return Ok(());
        };
        let Some(conversation) = self.conversations.get(&turn.conversation_id) else {
            return Ok(());
        };
        self.client
            .request(
                "turn/interrupt",
                json!({ "threadId": conversation.thread_id, "turnId": turn_id }),
            )
            .await?;
        Ok(())
    }

    pub fn poll_events(&mut self) -> Vec<AssistantEvent> {
        let mut events = Vec::new();
        for message in self.client.poll_messages() {
            let Some(method) = message.get("method").and_then(Value::as_str) else {
                continue;
            };
            let params = message.get("params").unwrap_or(&Value::Null);
            match method {
                "item/agentMessage/delta" => {
                    let Some(turn_id) = params.get("turnId").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(delta) = params.get("delta").and_then(Value::as_str) else {
                        continue;
                    };
                    if let Some(turn) = self.turns.get(turn_id) {
                        events.push(AssistantEvent {
                            conversation_id: turn.conversation_id.clone(),
                            turn_id: turn_id.to_string(),
                            kind: AssistantEventKind::Delta {
                                delta: delta.to_string(),
                            },
                            sink: turn.sink.clone(),
                        });
                    }
                }
                "turn/completed" => {
                    let Some(turn_id) = params
                        .get("turn")
                        .and_then(|turn| turn.get("id"))
                        .and_then(Value::as_str)
                    else {
                        continue;
                    };
                    if let Some(turn) = self.turns.remove(turn_id) {
                        let kind = match params
                            .get("turn")
                            .and_then(|turn| turn.get("status"))
                            .and_then(Value::as_str)
                        {
                            Some("failed") => AssistantEventKind::Error {
                                message: params
                                    .get("turn")
                                    .and_then(|turn| turn.get("error"))
                                    .and_then(|error| error.get("message"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("assistant turn failed")
                                    .to_string(),
                            },
                            _ => AssistantEventKind::Completed,
                        };
                        events.push(AssistantEvent {
                            conversation_id: turn.conversation_id,
                            turn_id: turn_id.to_string(),
                            kind,
                            sink: turn.sink,
                        });
                    }
                }
                "error" => {
                    let message = params
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("assistant error")
                        .to_string();
                    for (turn_id, turn) in self.turns.drain() {
                        events.push(AssistantEvent {
                            conversation_id: turn.conversation_id,
                            turn_id,
                            kind: AssistantEventKind::Error {
                                message: message.clone(),
                            },
                            sink: turn.sink,
                        });
                    }
                }
                _ => {}
            }
        }
        events
    }
}

struct CodexAppServerClient {
    _child: Child,
    stdin: ChildStdin,
    receiver: mpsc::Receiver<Value>,
    pending_notifications: VecDeque<Value>,
    next_id: u64,
}

impl CodexAppServerClient {
    async fn start() -> anyhow::Result<Self> {
        let mut child = Command::new("codex")
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to start codex app-server")?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("codex app-server stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("codex app-server stdout unavailable"))?;
        let (sender, receiver) = mpsc::channel(256);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(message) = serde_json::from_str::<Value>(&line) {
                    if sender.send(message).await.is_err() {
                        break;
                    }
                }
            }
        });

        let mut client = Self {
            _child: child,
            stdin,
            receiver,
            pending_notifications: VecDeque::new(),
            next_id: 0,
        };
        client
            .request(
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "red",
                        "title": "Red",
                        "version": env!("CARGO_PKG_VERSION"),
                    }
                }),
            )
            .await?;
        client.notify("initialized", json!({})).await?;
        Ok(client)
    }

    async fn request(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.write_message(json!({ "method": method, "id": id, "params": params }))
            .await?;
        loop {
            let message = self
                .receiver
                .recv()
                .await
                .ok_or_else(|| anyhow!("codex app-server closed the transport"))?;
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(error) = message.get("error") {
                    return Err(anyhow!("codex app-server request failed: {error}"));
                }
                return message
                    .get("result")
                    .cloned()
                    .ok_or_else(|| anyhow!("codex app-server response omitted result"));
            }
            self.pending_notifications.push_back(message);
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> anyhow::Result<()> {
        self.write_message(json!({ "method": method, "params": params }))
            .await
    }

    async fn write_message(&mut self, message: Value) -> anyhow::Result<()> {
        let mut line = serde_json::to_vec(&message)?;
        line.push(b'\n');
        self.stdin
            .write_all(&line)
            .await
            .context("failed to write codex app-server request")?;
        self.stdin
            .flush()
            .await
            .context("failed to flush codex app-server request")
    }

    fn poll_messages(&mut self) -> Vec<Value> {
        let mut messages = self.pending_notifications.drain(..).collect::<Vec<_>>();
        while let Ok(message) = self.receiver.try_recv() {
            messages.push(message);
        }
        messages
    }
}

fn conversation_from_thread(thread: &Value) -> anyhow::Result<AssistantConversation> {
    Ok(AssistantConversation {
        id: required_string(thread, "id")?,
        preview: thread
            .get("preview")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        name: thread
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string),
        cwd: thread
            .get("cwd")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

fn history_blocks(thread: &Value) -> Vec<AssistantHistoryBlock> {
    let mut blocks = Vec::new();
    let Some(turns) = thread.get("turns").and_then(Value::as_array) else {
        return blocks;
    };
    for turn in turns {
        let turn_id = turn.get("id").and_then(Value::as_str).unwrap_or("turn");
        let Some(items) = turn.get("items").and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            match item.get("type").and_then(Value::as_str) {
                Some("userMessage") => {
                    if let Some(text) = user_message_text(item) {
                        blocks.push(AssistantHistoryBlock {
                            id,
                            kind: AssistantHistoryBlockKind::User,
                            text: visible_prompt_request(&text).to_string(),
                        });
                    }
                }
                Some("agentMessage") => {
                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                        blocks.push(AssistantHistoryBlock {
                            id,
                            kind: AssistantHistoryBlockKind::Agent,
                            text: text.to_string(),
                        });
                    }
                }
                _ => {}
            }
        }
        if turn.get("status").and_then(Value::as_str) == Some("failed") {
            if let Some(message) = turn
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
            {
                blocks.push(AssistantHistoryBlock {
                    id: format!("{turn_id}:error"),
                    kind: AssistantHistoryBlockKind::Error,
                    text: message.to_string(),
                });
            }
        }
    }
    blocks
}

fn user_message_text(item: &Value) -> Option<String> {
    let content = item.get("content")?.as_array()?;
    let mut text = String::new();
    for input in content {
        if input.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(value) = input.get("text").and_then(Value::as_str) {
                text.push_str(value);
            }
        }
    }
    (!text.is_empty()).then_some(text)
}

fn visible_prompt_request(prompt: &str) -> &str {
    prompt
        .rsplit_once(PROMPT_REQUEST_BEGIN)
        .map_or(prompt, |(_, request)| request.trim())
}

fn required_string(value: &Value, key: &str) -> anyhow::Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("assistant response omitted {key}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_prompt_request_strips_editor_context() {
        let prompt = "# Context from my IDE setup:\n\n## Active file: src/lib.rs\n\n## My request for Codex:\nWhy?";

        assert_eq!(visible_prompt_request(prompt), "Why?");
    }

    #[test]
    fn history_blocks_keep_only_user_and_agent_messages() {
        let thread = json!({
            "turns": [{
                "items": [
                    {
                        "id": "user-1",
                        "type": "userMessage",
                        "content": [{
                            "type": "text",
                            "text": "## My request for Codex:\nQuestion",
                            "textElements": []
                        }]
                    },
                    {
                        "id": "agent-1",
                        "type": "agentMessage",
                        "text": "Answer"
                    }
                ],
                "status": "completed"
            }]
        });

        let blocks = history_blocks(&thread);

        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].text, "Question");
        assert_eq!(blocks[1].text, "Answer");
    }
}
