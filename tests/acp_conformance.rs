use std::{
    num::NonZeroUsize,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use agent_client_protocol_schema::v1::{
    CancelNotification, ClientNotification, ClientRequest, ContentBlock, InitializeResponse,
    NewSessionRequest, NewSessionResponse, PermissionOptionId, PromptRequest, PromptResponse,
    ReadTextFileRequest, ReadTextFileResponse, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionNotification, StopReason,
    TextContent, WriteTextFileRequest, WriteTextFileResponse,
};
use async_trait::async_trait;
use red::acp::{
    initialize_request, start_bridge, AcpHost, AcpProcessSpec, AcpSpawn, BridgeCommand,
    BridgeEvent, WIRE_PROTOCOL_VERSION,
};
use tokio::sync::mpsc;

#[derive(Debug, Default)]
struct HostState {
    reads: Vec<PathBuf>,
    writes: Vec<(PathBuf, String)>,
    permission_requests: usize,
}

struct RecordingHost {
    state: Arc<Mutex<HostState>>,
    updates: mpsc::UnboundedSender<SessionNotification>,
}

#[async_trait]
impl AcpHost for RecordingHost {
    async fn read_text_file(
        &mut self,
        request: ReadTextFileRequest,
    ) -> anyhow::Result<ReadTextFileResponse> {
        self.state.lock().unwrap().reads.push(request.path);
        Ok(ReadTextFileResponse::new("unsaved buffer contents"))
    }

    async fn write_text_file(
        &mut self,
        request: WriteTextFileRequest,
    ) -> anyhow::Result<WriteTextFileResponse> {
        self.state
            .lock()
            .unwrap()
            .writes
            .push((request.path, request.content));
        Ok(WriteTextFileResponse::new())
    }

    async fn request_permission(
        &mut self,
        request: RequestPermissionRequest,
    ) -> anyhow::Result<RequestPermissionResponse> {
        self.state.lock().unwrap().permission_requests += 1;
        let option = request
            .options
            .first()
            .map(|option| option.option_id.clone())
            .unwrap_or_else(|| PermissionOptionId::new("allow-once"));
        Ok(RequestPermissionResponse::new(
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option)),
        ))
    }

    async fn session_update(&mut self, notification: SessionNotification) -> anyhow::Result<()> {
        self.updates
            .send(notification)
            .map_err(|_| anyhow::anyhow!("conformance update receiver stopped"))
    }
}

#[tokio::test]
async fn live_fixture_covers_stable_vertical_slice() {
    let state = Arc::new(Mutex::new(HostState::default()));
    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    let host = RecordingHost {
        state: Arc::clone(&state),
        updates: update_tx,
    };
    let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
    let spawned = AcpSpawn::start(AcpProcessSpec::new(executable), host).unwrap();
    let client = spawned.client.clone();

    let initialized: InitializeResponse = client.request(initialize_request()).await.unwrap();
    assert_eq!(initialized.protocol_version, WIRE_PROTOCOL_VERSION);

    let session: NewSessionResponse = client
        .request(ClientRequest::NewSessionRequest(NewSessionRequest::new(
            "/workspace",
        )))
        .await
        .unwrap();
    let session_id = session.session_id;
    let prompt_client = client.clone();
    let prompt_session_id = session_id.clone();
    let prompt = tokio::spawn(async move {
        prompt_client
            .request::<PromptResponse>(ClientRequest::PromptRequest(PromptRequest::new(
                prompt_session_id,
                vec![ContentBlock::Text(TextContent::new("inspect the buffer"))],
            )))
            .await
    });

    let update = tokio::time::timeout(Duration::from_secs(5), update_rx.recv())
        .await
        .unwrap()
        .expect("fixture must stream a session update");
    assert_eq!(update.session_id, session_id);
    client
        .notify(ClientNotification::CancelNotification(
            CancelNotification::new(session_id),
        ))
        .await
        .unwrap();

    let prompt_response = prompt.await.unwrap().unwrap();
    assert_eq!(prompt_response.stop_reason, StopReason::Cancelled);
    {
        let state = state.lock().unwrap();
        assert_eq!(state.reads, [PathBuf::from("/workspace/example.rs")]);
        assert_eq!(
            state.writes,
            [(
                PathBuf::from("/workspace/example.rs"),
                "proposed contents".to_string()
            )]
        );
        assert_eq!(state.permission_requests, 1);
    }

    client.shutdown().await.unwrap();
    spawned.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn bounded_bridge_drives_a_live_session_from_husk_shaped_commands() {
    const BRIDGE_CAPACITY: usize = 8;
    let state = Arc::new(Mutex::new(HostState::default()));
    let (update_tx, _update_rx) = mpsc::unbounded_channel();
    let host = RecordingHost {
        state,
        updates: update_tx,
    };
    let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
    let capacity = NonZeroUsize::new(BRIDGE_CAPACITY).expect("bridge capacity is non-zero");
    let (mut bridge, task) = start_bridge(AcpProcessSpec::new(executable), host, capacity).unwrap();

    bridge
        .send(BridgeCommand::NewSession {
            cwd: PathBuf::from("/workspace"),
        })
        .await
        .unwrap();
    let session_id = match bridge.recv().await {
        Some(BridgeEvent::SessionCreated { session_id }) => session_id,
        event => panic!("expected session-created event, got {event:?}"),
    };
    bridge
        .send(BridgeCommand::Prompt {
            session_id: session_id.clone(),
            text: "inspect the buffer".to_string(),
        })
        .await
        .unwrap();
    let permission_request_id = match bridge.recv().await {
        Some(BridgeEvent::PermissionRequested {
            request_id,
            session_id: requested_session,
            options,
            ..
        }) => {
            assert_eq!(requested_session, session_id);
            assert_eq!(options[0].option_id.to_string(), "allow-once");
            request_id
        }
        event => panic!("expected permission request, got {event:?}"),
    };
    bridge
        .send(BridgeCommand::PermissionResponse {
            request_id: permission_request_id,
            option_id: Some("allow-once".to_string()),
        })
        .await
        .unwrap();
    assert!(matches!(
        bridge.recv().await,
        Some(BridgeEvent::Update { text, .. }) if text == "fixture streamed update"
    ));
    bridge
        .send(BridgeCommand::Cancel {
            session_id: session_id.clone(),
        })
        .await
        .unwrap();

    let mut saw_cancelled = false;
    let mut saw_completed = false;
    while !saw_cancelled || !saw_completed {
        match bridge.recv().await {
            Some(BridgeEvent::Cancelled {
                session_id: cancelled,
            }) => {
                assert_eq!(cancelled, session_id);
                saw_cancelled = true;
            }
            Some(BridgeEvent::Completed {
                session_id: completed,
                stop_reason,
            }) => {
                assert_eq!(completed, session_id);
                assert_eq!(stop_reason, "cancelled");
                saw_completed = true;
            }
            event => panic!("unexpected bridge event: {event:?}"),
        }
    }

    drop(bridge);
    task.await.unwrap().unwrap();
}
