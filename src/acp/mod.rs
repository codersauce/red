//! Agent Client Protocol foundations.
//!
//! Red pins the official stable schema crate and owns transport, lifecycle, buffer
//! integration, and policy around it. This module deliberately does not duplicate ACP
//! request or response structs.

mod transport;

use std::{num::NonZeroUsize, path::PathBuf, sync::Arc};

use agent_client_protocol_schema::{
    v1::{
        CancelNotification, ClientNotification, ClientRequest, CloseSessionRequest, ContentBlock,
        EmbeddedResource, EmbeddedResourceResource, InitializeRequest, JsonRpcMessage,
        NewSessionRequest, Notification, PermissionOption, PromptRequest, Request, RequestId,
        SessionId, TextContent, TextResourceContents,
    },
    ProtocolVersion,
};
use serde::{de::DeserializeOwned, Serialize};
use tokio::sync::mpsc;

/// Exact official schema artifact used to compile Red's ACP types.
pub const SCHEMA_ARTIFACT_VERSION: &str = "1.4.0";

/// Stable ACP wire protocol negotiated by this implementation.
pub const WIRE_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::V1;

/// Maximum encoded ACP message size, including its terminating newline.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

/// Encodes typed ACP messages as newline-delimited JSON-RPC.
#[derive(Debug)]
pub struct AcpCodec {
    next_request_id: i64,
}

impl Default for AcpCodec {
    fn default() -> Self {
        Self { next_request_id: 1 }
    }
}

impl AcpCodec {
    /// Encode a client-to-agent request and allocate its correlation ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the official schema value cannot be serialized.
    pub fn encode_request(&mut self, request: ClientRequest) -> anyhow::Result<String> {
        let method = Arc::<str>::from(request.method());
        let params = serde_json::to_value(request)?;
        let request = Request {
            id: RequestId::Number(self.next_request_id),
            method,
            params: Some(params),
        };
        self.next_request_id = self.next_request_id.saturating_add(1);
        encode_line(JsonRpcMessage::wrap(request))
    }

    /// Encode a client-to-agent notification.
    ///
    /// # Errors
    ///
    /// Returns an error if the official schema value cannot be serialized.
    pub fn encode_notification(&self, notification: ClientNotification) -> anyhow::Result<String> {
        let method = Arc::<str>::from(notification.method());
        let params = serde_json::to_value(notification)?;
        encode_line(JsonRpcMessage::wrap(Notification {
            method,
            params: Some(params),
        }))
    }

    /// Decode one NDJSON message into an official schema envelope.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty line or invalid JSON/schema payload.
    pub fn decode_line<T: DeserializeOwned>(&self, line: &str) -> anyhow::Result<T> {
        anyhow::ensure!(
            line.len() <= MAX_MESSAGE_BYTES,
            "agent protocol message exceeds {MAX_MESSAGE_BYTES} bytes"
        );
        let line = line.trim_end_matches(['\r', '\n']);
        anyhow::ensure!(!line.is_empty(), "agent protocol message line is empty");
        Ok(serde_json::from_str(line)?)
    }
}

fn encode_line(message: impl Serialize) -> anyhow::Result<String> {
    let mut line = serde_json::to_string(&message)?;
    line.push('\n');
    anyhow::ensure!(
        line.len() <= MAX_MESSAGE_BYTES,
        "agent protocol message exceeds {MAX_MESSAGE_BYTES} bytes"
    );
    Ok(line)
}

/// Commands sent from Red's editor/plugin surface to the ACP owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeCommand {
    NewSession {
        cwd: PathBuf,
    },
    Prompt {
        session_id: SessionId,
        text: String,
    },
    PromptWithContext {
        session_id: SessionId,
        text: String,
        uri: String,
        context: String,
    },
    Cancel {
        session_id: SessionId,
    },
    CloseSession {
        session_id: SessionId,
    },
    PermissionResponse {
        request_id: String,
        option_id: Option<String>,
    },
}

impl BridgeCommand {
    /// Convert the bridge command to its official stable ACP payload.
    #[must_use]
    pub fn into_wire(self) -> BridgeWireMessage {
        match self {
            Self::NewSession { cwd } => BridgeWireMessage::Request(Box::new(
                ClientRequest::NewSessionRequest(NewSessionRequest::new(cwd)),
            )),
            Self::Prompt { session_id, text } => BridgeWireMessage::Request(Box::new(
                ClientRequest::PromptRequest(PromptRequest::new(
                    session_id,
                    vec![ContentBlock::Text(TextContent::new(text))],
                )),
            )),
            Self::PromptWithContext {
                session_id,
                text,
                uri,
                context,
            } => BridgeWireMessage::Request(Box::new(ClientRequest::PromptRequest(
                PromptRequest::new(
                    session_id,
                    vec![
                        ContentBlock::Text(TextContent::new(text)),
                        ContentBlock::Resource(EmbeddedResource::new(
                            EmbeddedResourceResource::TextResourceContents(
                                TextResourceContents::new(context, uri)
                                    .mime_type("text/plain".to_string()),
                            ),
                        )),
                    ],
                ),
            ))),
            Self::Cancel { session_id } => BridgeWireMessage::Notification(
                ClientNotification::CancelNotification(CancelNotification::new(session_id)),
            ),
            Self::CloseSession { session_id } => BridgeWireMessage::Request(Box::new(
                ClientRequest::CloseSessionRequest(CloseSessionRequest::new(session_id)),
            )),
            Self::PermissionResponse { .. } => {
                unreachable!("permission responses are handled inside the bridge")
            }
        }
    }
}

/// Typed wire direction produced by a bridge command.
#[derive(Debug, Clone)]
pub enum BridgeWireMessage {
    Request(Box<ClientRequest>),
    Notification(ClientNotification),
}

/// Events streamed from the ACP owner to the editor/plugin surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeEvent {
    SessionCreated {
        session_id: SessionId,
    },
    Update {
        session_id: SessionId,
        text: String,
    },
    Activity {
        session_id: SessionId,
        update: serde_json::Value,
    },
    ProposalsChanged {
        session_id: SessionId,
    },
    Completed {
        session_id: SessionId,
        stop_reason: String,
    },
    Cancelled {
        session_id: SessionId,
    },
    PermissionRequested {
        request_id: String,
        session_id: SessionId,
        tool_call: serde_json::Value,
        options: Vec<PermissionOption>,
    },
    Failed {
        session_id: Option<SessionId>,
        message: String,
    },
}

/// Editor/plugin half of the bounded ACP bridge.
#[derive(Debug)]
pub struct AcpBridge {
    commands: mpsc::Sender<BridgeCommand>,
    events: mpsc::Receiver<BridgeEvent>,
}

/// ACP-owner half of the bounded bridge.
#[derive(Debug)]
pub struct AcpBridgeWorker {
    commands: mpsc::Receiver<BridgeCommand>,
    events: mpsc::Sender<BridgeEvent>,
}

impl AcpBridge {
    /// Create a bridge with explicit backpressure.
    #[must_use]
    pub fn channel(capacity: NonZeroUsize) -> (Self, AcpBridgeWorker) {
        let (command_tx, command_rx) = mpsc::channel(capacity.get());
        let (event_tx, event_rx) = mpsc::channel(capacity.get());
        (
            Self {
                commands: command_tx,
                events: event_rx,
            },
            AcpBridgeWorker {
                commands: command_rx,
                events: event_tx,
            },
        )
    }

    /// Queue a command without allowing an unbounded producer to exhaust memory.
    ///
    /// # Errors
    ///
    /// Returns the unsent command if the ACP owner has stopped.
    pub async fn send(
        &self,
        command: BridgeCommand,
    ) -> Result<(), mpsc::error::SendError<BridgeCommand>> {
        self.commands.send(command).await
    }

    /// Queue a command only when bridge capacity is immediately available.
    ///
    /// # Errors
    ///
    /// Returns the unsent command if the ACP owner has stopped or is backpressured.
    pub fn try_send(
        &self,
        command: BridgeCommand,
    ) -> Result<(), mpsc::error::TrySendError<BridgeCommand>> {
        self.commands.try_send(command)
    }

    /// Receive the next streamed event, or `None` after the ACP owner exits.
    pub async fn recv(&mut self) -> Option<BridgeEvent> {
        self.events.recv().await
    }

    /// Receive an already-buffered event without blocking the editor input loop.
    pub fn try_recv(&mut self) -> Option<BridgeEvent> {
        self.events.try_recv().ok()
    }

    /// Return whether streamed events remain buffered for the editor input loop.
    #[must_use]
    pub fn has_pending_events(&self) -> bool {
        !self.events.is_empty()
    }
}

impl AcpBridgeWorker {
    /// Receive the next editor/plugin command.
    pub async fn recv(&mut self) -> Option<BridgeCommand> {
        self.commands.recv().await
    }

    /// Stream one event back to the editor/plugin surface.
    ///
    /// # Errors
    ///
    /// Returns the unsent event if the editor/plugin surface has stopped.
    pub async fn send(
        &self,
        event: BridgeEvent,
    ) -> Result<(), mpsc::error::SendError<BridgeEvent>> {
        self.events.send(event).await
    }
}

/// Construct the stable initialization payload Red sends first.
#[must_use]
pub fn initialize_request() -> ClientRequest {
    use agent_client_protocol_schema::v1::{ClientCapabilities, FileSystemCapabilities};

    let file_system = FileSystemCapabilities::new()
        .read_text_file(/*read_text_file*/ true)
        .write_text_file(/*write_text_file*/ true);
    ClientRequest::InitializeRequest(
        InitializeRequest::new(WIRE_PROTOCOL_VERSION)
            .client_capabilities(ClientCapabilities::new().fs(file_system)),
    )
}

pub use transport::{
    start_bridge, AcpClient, AcpHost, AcpProcessSpec, AcpRpcError, AcpSpawn, NoopAcpHost,
};

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol_schema::v1::{
        ReadTextFileRequest, WriteTextFileRequest, CLIENT_METHOD_NAMES,
    };
    use serde_json::Value;

    #[test]
    fn recorded_stable_requests_match_official_method_names() {
        const FIXTURE_REQUEST_ID: i64 = 1;
        let session_id = SessionId::new("session-1");
        let requests = [
            initialize_request(),
            BridgeCommand::NewSession {
                cwd: PathBuf::from("/workspace"),
            }
            .into_wire()
            .into_request(),
            BridgeCommand::Prompt {
                session_id: session_id.clone(),
                text: "inspect this file".to_string(),
            }
            .into_wire()
            .into_request(),
            BridgeCommand::CloseSession {
                session_id: session_id.clone(),
            }
            .into_wire()
            .into_request(),
        ];
        let expected_methods = [
            "initialize",
            "session/new",
            "session/prompt",
            "session/close",
        ];

        let mut codec = AcpCodec::default();
        for (request, expected_method) in requests.into_iter().zip(expected_methods) {
            let line = codec.encode_request(request).unwrap();
            let value: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(value["jsonrpc"], "2.0");
            assert_eq!(value["method"], expected_method);
        }

        let filesystem_requests = [
            (
                CLIENT_METHOD_NAMES.fs_read_text_file,
                serde_json::to_value(ReadTextFileRequest::new(
                    session_id.clone(),
                    "/workspace/src/main.rs",
                ))
                .unwrap(),
            ),
            (
                CLIENT_METHOD_NAMES.fs_write_text_file,
                serde_json::to_value(WriteTextFileRequest::new(
                    session_id,
                    "/workspace/src/main.rs",
                    "fn main() {}",
                ))
                .unwrap(),
            ),
        ];
        for (method, params) in filesystem_requests {
            let message = JsonRpcMessage::wrap(Request {
                id: RequestId::Number(FIXTURE_REQUEST_ID),
                method: Arc::from(method),
                params: Some(params),
            });
            let line = encode_line(message).unwrap();
            let value: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(value["method"], method);
        }
    }

    #[test]
    fn prompt_with_context_encodes_an_embedded_text_resource() {
        let request = BridgeCommand::PromptWithContext {
            session_id: SessionId::new("session-1"),
            text: "explain the selection".to_string(),
            uri: "file:///workspace/src/main.rs".to_string(),
            context: "fn main() {}".to_string(),
        }
        .into_wire()
        .into_request();

        let mut codec = AcpCodec::default();
        let line = codec.encode_request(request).unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();

        assert_eq!(value["method"], "session/prompt");
        assert_eq!(value["params"]["prompt"][0]["type"], "text");
        assert_eq!(
            value["params"]["prompt"][0]["text"],
            "explain the selection"
        );
        assert_eq!(value["params"]["prompt"][1]["type"], "resource");
        assert_eq!(
            value["params"]["prompt"][1]["resource"],
            serde_json::json!({
                "uri": "file:///workspace/src/main.rs",
                "mimeType": "text/plain",
                "text": "fn main() {}"
            })
        );
    }

    #[test]
    fn cancellation_is_an_ndjson_notification() {
        let command = BridgeCommand::Cancel {
            session_id: SessionId::new("session-1"),
        };
        let BridgeWireMessage::Notification(notification) = command.into_wire() else {
            panic!("cancel must be a notification");
        };

        let line = AcpCodec::default()
            .encode_notification(notification)
            .unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["method"], "session/cancel");
        assert!(value.get("id").is_none());
        assert!(line.ends_with('\n'));
    }

    #[test]
    fn codec_rejects_oversized_messages_in_both_directions() {
        let oversized = "x".repeat(MAX_MESSAGE_BYTES);
        let decode_error = AcpCodec::default()
            .decode_line::<Value>(&format!("{oversized}\n"))
            .unwrap_err();
        assert!(decode_error.to_string().contains("exceeds"));

        let encode_error = encode_line(serde_json::json!({ "content": oversized })).unwrap_err();
        assert!(encode_error.to_string().contains("exceeds"));
    }

    #[tokio::test]
    async fn bridge_is_bounded_and_streams_updates() {
        const BRIDGE_CAPACITY: usize = 1;
        let capacity = NonZeroUsize::new(BRIDGE_CAPACITY).expect("one is non-zero");
        let (mut bridge, mut worker) = AcpBridge::channel(capacity);
        let session_id = SessionId::new("session-1");

        bridge
            .send(BridgeCommand::Prompt {
                session_id: session_id.clone(),
                text: "hello".to_string(),
            })
            .await
            .unwrap();
        assert!(matches!(
            worker.recv().await,
            Some(BridgeCommand::Prompt { .. })
        ));

        worker
            .send(BridgeEvent::Update {
                session_id,
                text: "world".to_string(),
            })
            .await
            .unwrap();
        assert!(matches!(
            bridge.recv().await,
            Some(BridgeEvent::Update { .. })
        ));
    }

    impl BridgeWireMessage {
        fn into_request(self) -> ClientRequest {
            match self {
                Self::Request(request) => *request,
                Self::Notification(_) => panic!("expected ACP request"),
            }
        }
    }
}
