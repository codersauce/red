use std::{
    num::NonZeroUsize,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use agent_client_protocol_schema::v1::{
    CancelNotification, ClientNotification, ClientRequest, CloseSessionRequest,
    CloseSessionResponse, ContentBlock, InitializeResponse, NewSessionRequest, NewSessionResponse,
    PermissionOptionId, PromptRequest, PromptResponse, ReadTextFileRequest, ReadTextFileResponse,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionNotification, StopReason, TextContent, WriteTextFileRequest,
    WriteTextFileResponse,
};
use async_trait::async_trait;
use red::acp::{
    initialize_request, start_bridge, AcpHost, AcpProcessSpec, AcpSpawn, BridgeCommand,
    BridgeEvent, MAX_MESSAGE_BYTES, WIRE_PROTOCOL_VERSION,
};
use red::agent_tools::{EditorToolCall, EditorToolRequest};
use tokio::sync::mpsc;

#[derive(Debug, Default)]
struct HostState {
    reads: Vec<PathBuf>,
    writes: Vec<(PathBuf, String)>,
    permission_requests: usize,
    editor_tools: Vec<String>,
}

struct RecordingHost {
    state: Arc<Mutex<HostState>>,
    updates: mpsc::UnboundedSender<SessionNotification>,
    reject_outside_workspace: bool,
    reject_updates: bool,
}

#[async_trait]
impl AcpHost for RecordingHost {
    async fn read_text_file(
        &mut self,
        request: ReadTextFileRequest,
    ) -> anyhow::Result<ReadTextFileResponse> {
        anyhow::ensure!(
            !self.reject_outside_workspace || request.path.starts_with("/workspace"),
            "agent path {} is outside workspace /workspace",
            request.path.display()
        );
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

    async fn editor_tool(
        &mut self,
        request: EditorToolRequest,
    ) -> anyhow::Result<serde_json::Value> {
        let name = match request.call {
            EditorToolCall::GetEditorState {} => "get_editor_state",
            _ => anyhow::bail!("unexpected conformance editor tool"),
        };
        self.state
            .lock()
            .unwrap()
            .editor_tools
            .push(name.to_string());
        Ok(serde_json::json!({"ok": true, "file": "example.rs"}))
    }

    async fn session_update(&mut self, notification: SessionNotification) -> anyhow::Result<()> {
        anyhow::ensure!(!self.reject_updates, "session updates are unavailable");
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
        reject_outside_workspace: false,
        reject_updates: false,
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
async fn live_fixture_routes_an_active_editor_tool_request() {
    let state = Arc::new(Mutex::new(HostState::default()));
    let (update_tx, _update_rx) = mpsc::unbounded_channel();
    let host = RecordingHost {
        state: Arc::clone(&state),
        updates: update_tx,
        reject_outside_workspace: false,
        reject_updates: false,
    };
    let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
    let mut spec = AcpProcessSpec::new(executable);
    spec.environment
        .insert("RED_ACP_FIXTURE_MODE".into(), "editor-tools".into());
    let spawned = AcpSpawn::start(spec, host).unwrap();
    let client = spawned.client.clone();
    let _: InitializeResponse = client.request(initialize_request()).await.unwrap();
    let session: NewSessionResponse = client
        .request(ClientRequest::NewSessionRequest(NewSessionRequest::new(
            "/workspace",
        )))
        .await
        .unwrap();

    let response: PromptResponse = client
        .request(ClientRequest::PromptRequest(PromptRequest::new(
            session.session_id,
            vec![ContentBlock::Text(TextContent::new("inspect the editor"))],
        )))
        .await
        .unwrap();

    assert_eq!(response.stop_reason, StopReason::EndTurn);
    assert_eq!(
        state.lock().unwrap().editor_tools,
        ["get_editor_state".to_string()]
    );
    client.shutdown().await.unwrap();
    spawned.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn inactive_editor_tool_requests_never_reach_the_host() {
    let state = Arc::new(Mutex::new(HostState::default()));
    let (update_tx, _update_rx) = mpsc::unbounded_channel();
    let host = RecordingHost {
        state: Arc::clone(&state),
        updates: update_tx,
        reject_outside_workspace: false,
        reject_updates: false,
    };
    let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
    let mut spec = AcpProcessSpec::new(executable);
    spec.environment.insert(
        "RED_ACP_FIXTURE_MODE".into(),
        "editor-tool-after-prompt".into(),
    );
    let spawned = AcpSpawn::start(spec, host).unwrap();
    let client = spawned.client.clone();
    let _: InitializeResponse = client.request(initialize_request()).await.unwrap();
    let session: NewSessionResponse = client
        .request(ClientRequest::NewSessionRequest(NewSessionRequest::new(
            "/workspace",
        )))
        .await
        .unwrap();

    let response: PromptResponse = client
        .request(ClientRequest::PromptRequest(PromptRequest::new(
            session.session_id,
            vec![ContentBlock::Text(TextContent::new("finish immediately"))],
        )))
        .await
        .unwrap();
    assert_eq!(response.stop_reason, StopReason::EndTurn);
    client.shutdown().await.unwrap();
    spawned.task.await.unwrap().unwrap();
    assert!(state.lock().unwrap().editor_tools.is_empty());
}

#[tokio::test]
async fn bridge_surfaces_editor_tool_activity_to_the_agent_panel() {
    let state = Arc::new(Mutex::new(HostState::default()));
    let (update_tx, _update_rx) = mpsc::unbounded_channel();
    let host = RecordingHost {
        state,
        updates: update_tx,
        reject_outside_workspace: false,
        reject_updates: false,
    };
    let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
    let mut spec = AcpProcessSpec::new(executable);
    spec.environment
        .insert("RED_ACP_FIXTURE_MODE".into(), "editor-tools".into());
    let capacity = NonZeroUsize::new(8).unwrap();
    let (mut bridge, task) = start_bridge(spec, host, capacity).unwrap();
    bridge
        .send(BridgeCommand::NewSession {
            cwd: PathBuf::from("/workspace"),
        })
        .await
        .unwrap();
    let session_id = match bridge.recv().await {
        Some(BridgeEvent::SessionCreated { session_id }) => session_id,
        event => panic!("expected session creation, got {event:?}"),
    };
    bridge
        .send(BridgeCommand::Prompt {
            session_id: session_id.clone(),
            text: "inspect the editor".to_string(),
        })
        .await
        .unwrap();

    match bridge.recv().await {
        Some(BridgeEvent::Activity {
            session_id: activity_session,
            update,
        }) => {
            assert_eq!(activity_session, session_id);
            assert_eq!(update["session_update"], "editor_tool");
            assert_eq!(update["status"], "in_progress");
            assert_eq!(update["title"], "Inspecting editor state");
        }
        event => panic!("expected editor-tool activity, got {event:?}"),
    }
    assert!(matches!(
        bridge.recv().await,
        Some(BridgeEvent::Completed { session_id: completed, stop_reason })
            if completed == session_id && stop_reason == "end_turn"
    ));
    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn bounded_bridge_drives_a_live_session_from_husk_shaped_commands() {
    const BRIDGE_CAPACITY: usize = 8;
    let state = Arc::new(Mutex::new(HostState::default()));
    let (update_tx, _update_rx) = mpsc::unbounded_channel();
    let host = RecordingHost {
        state,
        updates: update_tx,
        reject_outside_workspace: false,
        reject_updates: false,
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
    assert!(matches!(
        bridge.recv().await,
        Some(BridgeEvent::ProposalsChanged { session_id: changed }) if changed == session_id
    ));
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

#[tokio::test]
async fn bridge_closes_an_idle_session_when_the_adapter_advertises_support() {
    let state = Arc::new(Mutex::new(HostState::default()));
    let (update_tx, _update_rx) = mpsc::unbounded_channel();
    let host = RecordingHost {
        state,
        updates: update_tx,
        reject_outside_workspace: false,
        reject_updates: false,
    };
    let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
    let mut spec = AcpProcessSpec::new(executable);
    spec.environment
        .insert("RED_ACP_FIXTURE_MODE".into(), "close-supported".into());
    let capacity = NonZeroUsize::new(2).expect("bridge capacity is non-zero");
    let (mut bridge, task) = start_bridge(spec, host, capacity).unwrap();
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
        .send(BridgeCommand::CloseSession { session_id })
        .await
        .unwrap();
    bridge
        .send(BridgeCommand::NewSession {
            cwd: PathBuf::from("/workspace"),
        })
        .await
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), bridge.recv()).await,
        Ok(Some(BridgeEvent::SessionCreated { .. }))
    ));

    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn successful_close_retires_a_cancelled_session_without_accepting_late_writes() {
    let state = Arc::new(Mutex::new(HostState::default()));
    let (update_tx, _update_rx) = mpsc::unbounded_channel();
    let host = RecordingHost {
        state: Arc::clone(&state),
        updates: update_tx,
        reject_outside_workspace: false,
        reject_updates: false,
    };
    let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
    let mut spec = AcpProcessSpec::new(executable);
    spec.environment
        .insert("RED_ACP_FIXTURE_MODE".into(), "reuse-after-close".into());
    let spawned = AcpSpawn::start(spec, host).unwrap();
    let client = spawned.client.clone();

    let _: InitializeResponse = client.request(initialize_request()).await.unwrap();
    let original: NewSessionResponse = client
        .request(ClientRequest::NewSessionRequest(NewSessionRequest::new(
            "/workspace",
        )))
        .await
        .unwrap();
    client
        .notify(ClientNotification::CancelNotification(
            CancelNotification::new(original.session_id.clone()),
        ))
        .await
        .unwrap();
    let _: CloseSessionResponse = client
        .request(ClientRequest::CloseSessionRequest(
            CloseSessionRequest::new(original.session_id.clone()),
        ))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(state.lock().unwrap().writes.is_empty());

    let reused: NewSessionResponse = client
        .request(ClientRequest::NewSessionRequest(NewSessionRequest::new(
            "/workspace",
        )))
        .await
        .unwrap();
    assert_eq!(reused.session_id, original.session_id);
    let response: PromptResponse = client
        .request(ClientRequest::PromptRequest(PromptRequest::new(
            reused.session_id,
            vec![ContentBlock::Text(TextContent::new(
                "reuse the closed session id",
            ))],
        )))
        .await
        .unwrap();
    assert_eq!(response.stop_reason, StopReason::EndTurn);
    assert!(state.lock().unwrap().writes.is_empty());

    client.shutdown().await.unwrap();
    spawned.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn failed_or_timed_out_close_keeps_the_cancelled_session_unusable() {
    for mode in ["close-error", "ignore-close"] {
        let state = Arc::new(Mutex::new(HostState::default()));
        let (update_tx, _update_rx) = mpsc::unbounded_channel();
        let host = RecordingHost {
            state: Arc::clone(&state),
            updates: update_tx,
            reject_outside_workspace: false,
            reject_updates: false,
        };
        let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
        let mut spec = AcpProcessSpec::new(executable);
        spec.request_timeout = Duration::from_secs(1);
        spec.environment
            .insert("RED_ACP_FIXTURE_MODE".into(), mode.into());
        let spawned = AcpSpawn::start(spec, host).unwrap();
        let client = spawned.client.clone();

        let _: InitializeResponse = client.request(initialize_request()).await.unwrap();
        let session: NewSessionResponse = client
            .request(ClientRequest::NewSessionRequest(NewSessionRequest::new(
                "/workspace",
            )))
            .await
            .unwrap();
        client
            .notify(ClientNotification::CancelNotification(
                CancelNotification::new(session.session_id.clone()),
            ))
            .await
            .unwrap();
        assert!(client
            .request::<CloseSessionResponse>(ClientRequest::CloseSessionRequest(
                CloseSessionRequest::new(session.session_id.clone()),
            ))
            .await
            .is_err());

        let error = client
            .request::<PromptResponse>(ClientRequest::PromptRequest(PromptRequest::new(
                session.session_id,
                vec![ContentBlock::Text(TextContent::new(
                    "must remain cancelled",
                ))],
            )))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("start a new session"));
        assert!(state.lock().unwrap().writes.is_empty());

        client.shutdown().await.unwrap();
        spawned.task.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn exhausted_cancelled_session_capacity_rejects_evicted_and_new_session_prompts() {
    const CANCELLED_SESSION_CAPACITY: usize = 32;

    let state = Arc::new(Mutex::new(HostState::default()));
    let (update_tx, _update_rx) = mpsc::unbounded_channel();
    let host = RecordingHost {
        state: Arc::clone(&state),
        updates: update_tx,
        reject_outside_workspace: false,
        reject_updates: false,
    };
    let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
    let mut spec = AcpProcessSpec::new(executable);
    spec.queue_capacity = 1;
    spec.environment.insert(
        "RED_ACP_FIXTURE_MODE".into(),
        "exhaust-cancellations".into(),
    );
    let spawned = AcpSpawn::start(spec, host).unwrap();
    let client = spawned.client.clone();

    let _: InitializeResponse = client.request(initialize_request()).await.unwrap();
    let mut first_session = None;
    for index in 0..=CANCELLED_SESSION_CAPACITY {
        let session: NewSessionResponse = client
            .request(ClientRequest::NewSessionRequest(NewSessionRequest::new(
                "/workspace",
            )))
            .await
            .unwrap();
        if index == 0 {
            first_session = Some(session.session_id.clone());
        }
        client
            .notify(ClientNotification::CancelNotification(
                CancelNotification::new(session.session_id.clone()),
            ))
            .await
            .unwrap();
        assert!(client
            .request::<CloseSessionResponse>(ClientRequest::CloseSessionRequest(
                CloseSessionRequest::new(session.session_id),
            ))
            .await
            .is_err());
    }

    let fresh: NewSessionResponse = client
        .request(ClientRequest::NewSessionRequest(NewSessionRequest::new(
            "/workspace",
        )))
        .await
        .unwrap();
    for session_id in [
        first_session.expect("first session was recorded"),
        fresh.session_id,
    ] {
        let error = client
            .request::<PromptResponse>(ClientRequest::PromptRequest(PromptRequest::new(
                session_id,
                vec![ContentBlock::Text(TextContent::new("must fail closed"))],
            )))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cancelled-session capacity"));
    }
    assert!(state.lock().unwrap().writes.is_empty());

    client.shutdown().await.unwrap();
    spawned.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn bridge_stays_alive_when_a_new_session_request_fails() {
    let state = Arc::new(Mutex::new(HostState::default()));
    let (update_tx, _update_rx) = mpsc::unbounded_channel();
    let host = RecordingHost {
        state,
        updates: update_tx,
        reject_outside_workspace: false,
        reject_updates: false,
    };
    let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
    let mut spec = AcpProcessSpec::new(executable);
    spec.environment.insert(
        "RED_ACP_FIXTURE_MODE".into(),
        "reject-second-session".into(),
    );
    let capacity = NonZeroUsize::new(2).expect("bridge capacity is non-zero");
    let (mut bridge, task) = start_bridge(spec, host, capacity).unwrap();

    bridge
        .send(BridgeCommand::NewSession {
            cwd: PathBuf::from("/workspace"),
        })
        .await
        .unwrap();
    assert!(matches!(
        bridge.recv().await,
        Some(BridgeEvent::SessionCreated { .. })
    ));
    bridge
        .send(BridgeCommand::NewSession {
            cwd: PathBuf::from("/workspace"),
        })
        .await
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), bridge.recv()).await,
        Ok(Some(BridgeEvent::Failed {
            session_id: None,
            message,
        })) if message.contains("fixture rejected session")
    ));
    bridge
        .send(BridgeCommand::NewSession {
            cwd: PathBuf::from("/workspace"),
        })
        .await
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), bridge.recv()).await,
        Ok(Some(BridgeEvent::SessionCreated { .. }))
    ));

    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn bridge_releases_a_prompt_slot_when_session_close_is_not_supported() {
    let state = Arc::new(Mutex::new(HostState::default()));
    let (update_tx, _update_rx) = mpsc::unbounded_channel();
    let host = RecordingHost {
        state,
        updates: update_tx,
        reject_outside_workspace: false,
        reject_updates: false,
    };
    let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
    let mut spec = AcpProcessSpec::new(executable);
    spec.queue_capacity = 1;
    spec.environment
        .insert("RED_ACP_FIXTURE_MODE".into(), "ignore-cancel".into());
    let capacity = NonZeroUsize::new(2).expect("bridge capacity is non-zero");
    let (mut bridge, task) = start_bridge(spec, host, capacity).unwrap();
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
            text: "wait for close".to_string(),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;

    bridge
        .send(BridgeCommand::CloseSession {
            session_id: session_id.clone(),
        })
        .await
        .unwrap();
    bridge
        .send(BridgeCommand::NewSession {
            cwd: PathBuf::from("/workspace"),
        })
        .await
        .unwrap();
    let mut completed = false;
    let mut created = false;
    while !completed || !created {
        match tokio::time::timeout(Duration::from_secs(5), bridge.recv()).await {
            Ok(Some(BridgeEvent::Completed {
                session_id: completed_session,
                stop_reason,
            })) => {
                assert_eq!(completed_session, session_id);
                assert_eq!(stop_reason, "cancelled");
                completed = true;
            }
            Ok(Some(BridgeEvent::SessionCreated { .. })) => created = true,
            event => panic!("unexpected bridge event: {event:?}"),
        }
    }

    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_stalled_session_close_cannot_block_the_replacement_session() {
    let state = Arc::new(Mutex::new(HostState::default()));
    let (update_tx, _update_rx) = mpsc::unbounded_channel();
    let host = RecordingHost {
        state,
        updates: update_tx,
        reject_outside_workspace: false,
        reject_updates: false,
    };
    let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
    let mut spec = AcpProcessSpec::new(executable);
    spec.request_timeout = Duration::from_secs(30);
    spec.environment
        .insert("RED_ACP_FIXTURE_MODE".into(), "ignore-close".into());
    let capacity = NonZeroUsize::new(2).expect("bridge capacity is non-zero");
    let (mut bridge, task) = start_bridge(spec, host, capacity).unwrap();
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
        .send(BridgeCommand::CloseSession {
            session_id: session_id.clone(),
        })
        .await
        .unwrap();
    bridge
        .send(BridgeCommand::NewSession {
            cwd: PathBuf::from("/workspace"),
        })
        .await
        .unwrap();
    let mut failed = false;
    let mut created = false;
    while !failed || !created {
        match tokio::time::timeout(Duration::from_secs(5), bridge.recv()).await {
            Ok(Some(BridgeEvent::Failed {
                session_id: failed_session,
                message,
            })) => {
                assert_eq!(failed_session, Some(session_id.clone()));
                assert!(message.contains("timed out"));
                failed = true;
            }
            Ok(Some(BridgeEvent::SessionCreated { .. })) => created = true,
            event => panic!("unexpected bridge event: {event:?}"),
        }
    }

    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn bridge_authenticates_with_the_advertised_method_before_starting_a_session() {
    let state = Arc::new(Mutex::new(HostState::default()));
    let (update_tx, _update_rx) = mpsc::unbounded_channel();
    let host = RecordingHost {
        state,
        updates: update_tx,
        reject_outside_workspace: false,
        reject_updates: false,
    };
    let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
    let mut spec = AcpProcessSpec::new(executable).authentication_method("fixture_api_key");
    spec.environment
        .insert("RED_ACP_FIXTURE_MODE".into(), "require-auth".into());
    let capacity = NonZeroUsize::new(2).expect("bridge capacity is non-zero");
    let (mut bridge, task) = start_bridge(spec, host, capacity).unwrap();

    bridge
        .send(BridgeCommand::NewSession {
            cwd: PathBuf::from("/workspace"),
        })
        .await
        .unwrap();

    assert!(matches!(
        bridge.recv().await,
        Some(BridgeEvent::SessionCreated { session_id })
            if session_id.to_string() == "fixture-session"
    ));
    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn bridge_rejects_an_unadvertised_authentication_method() {
    let state = Arc::new(Mutex::new(HostState::default()));
    let (update_tx, _update_rx) = mpsc::unbounded_channel();
    let host = RecordingHost {
        state,
        updates: update_tx,
        reject_outside_workspace: false,
        reject_updates: false,
    };
    let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
    let mut spec = AcpProcessSpec::new(executable).authentication_method("unknown");
    spec.environment
        .insert("RED_ACP_FIXTURE_MODE".into(), "require-auth".into());
    let capacity = NonZeroUsize::new(2).expect("bridge capacity is non-zero");
    let (bridge, task) = start_bridge(spec, host, capacity).unwrap();

    let error = task.await.unwrap().unwrap_err().to_string();

    assert!(error.contains("did not advertise ACP authentication method `unknown`"));
    drop(bridge);
}

#[tokio::test]
async fn adapter_stderr_is_isolated_from_the_terminal() {
    const CHILD_FLAG: &str = "RED_ACP_STDERR_CAPTURE_CHILD";
    const MARKER: &str = "fixture-stderr-must-not-reach-the-terminal";
    if std::env::var_os(CHILD_FLAG).is_some() {
        let state = Arc::new(Mutex::new(HostState::default()));
        let (update_tx, _update_rx) = mpsc::unbounded_channel();
        let host = RecordingHost {
            state,
            updates: update_tx,
            reject_outside_workspace: false,
            reject_updates: false,
        };
        let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
        let mut spec = AcpProcessSpec::new(executable);
        spec.environment
            .insert("RED_ACP_FIXTURE_MODE".into(), "noisy-stderr".into());
        let capacity = NonZeroUsize::new(2).expect("bridge capacity is non-zero");
        let (mut bridge, task) = start_bridge(spec, host, capacity).unwrap();
        bridge
            .send(BridgeCommand::NewSession {
                cwd: PathBuf::from("/workspace"),
            })
            .await
            .unwrap();
        assert!(matches!(
            bridge.recv().await,
            Some(BridgeEvent::SessionCreated { .. })
        ));
        drop(bridge);
        task.await.unwrap().unwrap();
        return;
    }

    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "adapter_stderr_is_isolated_from_the_terminal",
            "--nocapture",
        ])
        .env(CHILD_FLAG, "1")
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", output.status);
    assert!(!String::from_utf8_lossy(&output.stderr).contains(MARKER));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(MARKER));
}

#[tokio::test]
async fn host_policy_failure_and_invalid_params_are_request_scoped() {
    for (mode, reject_outside_workspace) in [
        ("host-failure-recovery", true),
        ("invalid-params-recovery", false),
    ] {
        let state = Arc::new(Mutex::new(HostState::default()));
        let (update_tx, _update_rx) = mpsc::unbounded_channel();
        let host = RecordingHost {
            state: Arc::clone(&state),
            updates: update_tx,
            reject_outside_workspace,
            reject_updates: false,
        };
        let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
        let mut spec = AcpProcessSpec::new(executable);
        spec.environment
            .insert("RED_ACP_FIXTURE_MODE".into(), mode.into());
        let spawned = AcpSpawn::start(spec, host).unwrap();

        let _: InitializeResponse = spawned.client.request(initialize_request()).await.unwrap();
        let session: NewSessionResponse = spawned
            .client
            .request(ClientRequest::NewSessionRequest(NewSessionRequest::new(
                "/workspace",
            )))
            .await
            .unwrap();
        let response: PromptResponse = spawned
            .client
            .request(ClientRequest::PromptRequest(PromptRequest::new(
                session.session_id,
                vec![ContentBlock::Text(TextContent::new("recover after denial"))],
            )))
            .await
            .unwrap();
        assert_eq!(response.stop_reason, StopReason::EndTurn);
        assert_eq!(
            state.lock().unwrap().reads,
            [PathBuf::from("/workspace/example.rs")]
        );

        spawned.client.shutdown().await.unwrap();
        spawned.task.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn prompts_outlive_the_control_request_timeout() {
    let state = Arc::new(Mutex::new(HostState::default()));
    let (update_tx, _update_rx) = mpsc::unbounded_channel();
    let host = RecordingHost {
        state,
        updates: update_tx,
        reject_outside_workspace: false,
        reject_updates: false,
    };
    let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
    let mut spec = AcpProcessSpec::new(executable);
    spec.request_timeout = Duration::from_millis(500);
    spec.environment
        .insert("RED_ACP_FIXTURE_MODE".into(), "delayed-prompt".into());
    let spawned = AcpSpawn::start(spec, host).unwrap();

    let _: InitializeResponse = spawned.client.request(initialize_request()).await.unwrap();
    let session: NewSessionResponse = spawned
        .client
        .request(ClientRequest::NewSessionRequest(NewSessionRequest::new(
            "/workspace",
        )))
        .await
        .unwrap();
    let response: PromptResponse = spawned
        .client
        .request(ClientRequest::PromptRequest(PromptRequest::new(
            session.session_id,
            vec![ContentBlock::Text(TextContent::new("take your time"))],
        )))
        .await
        .unwrap();
    assert_eq!(response.stop_reason, StopReason::EndTurn);

    spawned.client.shutdown().await.unwrap();
    spawned.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_late_control_response_does_not_terminate_the_actor() {
    let state = Arc::new(Mutex::new(HostState::default()));
    let (update_tx, _update_rx) = mpsc::unbounded_channel();
    let host = RecordingHost {
        state,
        updates: update_tx,
        reject_outside_workspace: false,
        reject_updates: false,
    };
    let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
    let mut spec = AcpProcessSpec::new(executable);
    spec.request_timeout = Duration::from_secs(1);
    spec.environment
        .insert("RED_ACP_FIXTURE_MODE".into(), "late-setup".into());
    let spawned = AcpSpawn::start(spec, host).unwrap();
    let _: InitializeResponse = spawned.client.request(initialize_request()).await.unwrap();

    let error = spawned
        .client
        .request::<NewSessionResponse>(ClientRequest::NewSessionRequest(NewSessionRequest::new(
            "/workspace",
        )))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("timed out"));
    tokio::time::sleep(Duration::from_millis(1_400)).await;

    let session: NewSessionResponse = spawned
        .client
        .request(ClientRequest::NewSessionRequest(NewSessionRequest::new(
            "/workspace",
        )))
        .await
        .unwrap();
    assert_eq!(session.session_id.to_string(), "fixture-session");
    spawned.client.shutdown().await.unwrap();
    spawned.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn pending_requests_are_bounded_when_an_adapter_never_responds() {
    let state = Arc::new(Mutex::new(HostState::default()));
    let (update_tx, _update_rx) = mpsc::unbounded_channel();
    let host = RecordingHost {
        state,
        updates: update_tx,
        reject_outside_workspace: false,
        reject_updates: false,
    };
    let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
    let mut spec = AcpProcessSpec::new(executable);
    spec.queue_capacity = 2;
    spec.request_timeout = Duration::from_secs(5);
    spec.environment
        .insert("RED_ACP_FIXTURE_MODE".into(), "ignore-setup".into());
    let spawned = AcpSpawn::start(spec, host).unwrap();
    let _: InitializeResponse = spawned.client.request(initialize_request()).await.unwrap();

    let first_client = spawned.client.clone();
    let first = tokio::spawn(async move {
        first_client
            .request::<NewSessionResponse>(ClientRequest::NewSessionRequest(
                NewSessionRequest::new("/workspace"),
            ))
            .await
    });
    let second_client = spawned.client.clone();
    let second = tokio::spawn(async move {
        second_client
            .request::<NewSessionResponse>(ClientRequest::NewSessionRequest(
                NewSessionRequest::new("/workspace"),
            ))
            .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let error = spawned
        .client
        .request::<NewSessionResponse>(ClientRequest::NewSessionRequest(NewSessionRequest::new(
            "/workspace",
        )))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("capacity"));
    first.abort();
    second.abort();

    spawned.client.shutdown().await.unwrap();
    spawned.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_non_reading_adapter_cannot_stall_prompt_control_or_shutdown() {
    let state = Arc::new(Mutex::new(HostState::default()));
    let (update_tx, _update_rx) = mpsc::unbounded_channel();
    let host = RecordingHost {
        state,
        updates: update_tx,
        reject_outside_workspace: false,
        reject_updates: false,
    };
    let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
    let mut spec = AcpProcessSpec::new(executable);
    spec.request_timeout = Duration::from_secs(2);
    spec.write_timeout = Duration::from_millis(250);
    spec.shutdown_timeout = Duration::from_millis(250);
    spec.environment
        .insert("RED_ACP_FIXTURE_MODE".into(), "stop-reading".into());
    let spawned = AcpSpawn::start(spec, host).unwrap();
    let _: InitializeResponse = spawned.client.request(initialize_request()).await.unwrap();
    let session: NewSessionResponse = spawned
        .client
        .request(ClientRequest::NewSessionRequest(NewSessionRequest::new(
            "/workspace",
        )))
        .await
        .unwrap();

    let prompt_client = spawned.client.clone();
    let session_id = session.session_id.clone();
    let prompt = tokio::spawn(async move {
        prompt_client
            .request::<PromptResponse>(ClientRequest::PromptRequest(PromptRequest::new(
                session_id,
                vec![ContentBlock::Text(TextContent::new(
                    "x".repeat(MAX_MESSAGE_BYTES - 64 * 1024),
                ))],
            )))
            .await
    });
    tokio::time::sleep(Duration::from_millis(25)).await;

    let control = tokio::time::timeout(
        Duration::from_secs(2),
        spawned
            .client
            .request::<NewSessionResponse>(ClientRequest::NewSessionRequest(
                NewSessionRequest::new("/workspace"),
            )),
    )
    .await
    .expect("control request must not remain blocked behind a non-reading adapter");
    assert!(control.is_err());
    let prompt = tokio::time::timeout(Duration::from_secs(2), prompt)
        .await
        .expect("prompt must not remain blocked behind a non-reading adapter")
        .unwrap();
    assert!(prompt.is_err());

    let shutdown = tokio::time::timeout(Duration::from_secs(2), spawned.client.shutdown())
        .await
        .expect("shutdown must not remain blocked behind a non-reading adapter");
    assert!(shutdown.is_err());
    let actor = tokio::time::timeout(Duration::from_secs(2), spawned.task)
        .await
        .expect("ACP actor must terminate after the stdin-write timeout")
        .unwrap()
        .unwrap_err();
    assert!(actor.to_string().contains("stdin write timed out"));
}

#[tokio::test]
async fn cancellation_releases_a_never_responding_prompt_slot() {
    let state = Arc::new(Mutex::new(HostState::default()));
    let (update_tx, _update_rx) = mpsc::unbounded_channel();
    let host = RecordingHost {
        state,
        updates: update_tx,
        reject_outside_workspace: false,
        reject_updates: false,
    };
    let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
    let mut spec = AcpProcessSpec::new(executable);
    spec.queue_capacity = 1;
    spec.environment
        .insert("RED_ACP_FIXTURE_MODE".into(), "ignore-cancel".into());
    let spawned = AcpSpawn::start(spec, host).unwrap();
    let _: InitializeResponse = spawned.client.request(initialize_request()).await.unwrap();
    let mut session: NewSessionResponse = spawned
        .client
        .request(ClientRequest::NewSessionRequest(NewSessionRequest::new(
            "/workspace",
        )))
        .await
        .unwrap();

    for (index, text) in ["first prompt", "second prompt"].into_iter().enumerate() {
        let client = spawned.client.clone();
        let session_id = session.session_id.clone();
        let prompt = tokio::spawn(async move {
            client
                .request::<PromptResponse>(ClientRequest::PromptRequest(PromptRequest::new(
                    session_id,
                    vec![ContentBlock::Text(TextContent::new(text))],
                )))
                .await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        spawned
            .client
            .notify(ClientNotification::CancelNotification(
                CancelNotification::new(session.session_id.clone()),
            ))
            .await
            .unwrap();
        assert_eq!(
            prompt.await.unwrap().unwrap().stop_reason,
            StopReason::Cancelled
        );
        if index == 0 {
            let previous = session.session_id;
            session = spawned
                .client
                .request(ClientRequest::NewSessionRequest(NewSessionRequest::new(
                    "/workspace",
                )))
                .await
                .unwrap();
            assert_ne!(session.session_id, previous);
        }
    }

    spawned.client.shutdown().await.unwrap();
    spawned.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn cancellation_rejects_a_late_filesystem_write_without_staging_a_proposal() {
    let state = Arc::new(Mutex::new(HostState::default()));
    let (update_tx, _update_rx) = mpsc::unbounded_channel();
    let host = RecordingHost {
        state: Arc::clone(&state),
        updates: update_tx,
        reject_outside_workspace: false,
        reject_updates: false,
    };
    let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
    let mut spec = AcpProcessSpec::new(executable);
    spec.environment
        .insert("RED_ACP_FIXTURE_MODE".into(), "write-after-cancel".into());
    let (mut bridge, task) = start_bridge(
        spec,
        host,
        NonZeroUsize::new(4).expect("bridge capacity is non-zero"),
    )
    .unwrap();

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
            text: "start a turn".to_string(),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    bridge
        .send(BridgeCommand::Cancel {
            session_id: session_id.clone(),
        })
        .await
        .unwrap();

    let mut saw_cancelled = false;
    let mut saw_completed = false;
    while !saw_cancelled || !saw_completed {
        match tokio::time::timeout(Duration::from_secs(5), bridge.recv()).await {
            Ok(Some(BridgeEvent::Cancelled { session_id: id })) => {
                assert_eq!(id, session_id);
                saw_cancelled = true;
            }
            Ok(Some(BridgeEvent::Completed {
                session_id: id,
                stop_reason,
            })) => {
                assert_eq!(id, session_id);
                assert_eq!(stop_reason, "cancelled");
                saw_completed = true;
            }
            event => panic!("unexpected bridge event: {event:?}"),
        }
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(100), bridge.recv())
            .await
            .is_err()
    );
    assert!(state.lock().unwrap().writes.is_empty());

    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn cancellation_rejects_session_reuse_and_stale_writes_during_a_replacement_prompt() {
    let state = Arc::new(Mutex::new(HostState::default()));
    let (update_tx, _update_rx) = mpsc::unbounded_channel();
    let host = RecordingHost {
        state: Arc::clone(&state),
        updates: update_tx,
        reject_outside_workspace: false,
        reject_updates: false,
    };
    let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
    let mut spec = AcpProcessSpec::new(executable);
    spec.environment.insert(
        "RED_ACP_FIXTURE_MODE".into(),
        "write-after-replacement-prompt".into(),
    );
    let (mut bridge, task) = start_bridge(
        spec,
        host,
        NonZeroUsize::new(4).expect("bridge capacity is non-zero"),
    )
    .unwrap();

    bridge
        .send(BridgeCommand::NewSession {
            cwd: PathBuf::from("/workspace"),
        })
        .await
        .unwrap();
    let cancelled_session = match bridge.recv().await {
        Some(BridgeEvent::SessionCreated { session_id }) => session_id,
        event => panic!("expected session-created event, got {event:?}"),
    };
    bridge
        .send(BridgeCommand::Prompt {
            session_id: cancelled_session.clone(),
            text: "start a turn".to_string(),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    bridge
        .send(BridgeCommand::Cancel {
            session_id: cancelled_session.clone(),
        })
        .await
        .unwrap();

    let mut saw_cancelled = false;
    let mut saw_completed = false;
    while !saw_cancelled || !saw_completed {
        match tokio::time::timeout(Duration::from_secs(5), bridge.recv()).await {
            Ok(Some(BridgeEvent::Cancelled { session_id })) => {
                assert_eq!(session_id, cancelled_session);
                saw_cancelled = true;
            }
            Ok(Some(BridgeEvent::Completed {
                session_id,
                stop_reason,
            })) => {
                assert_eq!(session_id, cancelled_session);
                assert_eq!(stop_reason, "cancelled");
                saw_completed = true;
            }
            event => panic!("unexpected bridge event: {event:?}"),
        }
    }

    bridge
        .send(BridgeCommand::Prompt {
            session_id: cancelled_session.clone(),
            text: "must not reuse the cancelled session".to_string(),
        })
        .await
        .unwrap();
    match tokio::time::timeout(Duration::from_secs(5), bridge.recv()).await {
        Ok(Some(BridgeEvent::Failed {
            session_id: Some(session_id),
            message,
        })) => {
            assert_eq!(session_id, cancelled_session);
            assert!(message.contains("start a new session"));
        }
        event => panic!("expected a scoped session-reuse failure, got {event:?}"),
    }

    bridge
        .send(BridgeCommand::NewSession {
            cwd: PathBuf::from("/workspace"),
        })
        .await
        .unwrap();
    let replacement_session = match bridge.recv().await {
        Some(BridgeEvent::SessionCreated { session_id }) => session_id,
        event => panic!("expected replacement-session event, got {event:?}"),
    };
    assert_ne!(replacement_session, cancelled_session);
    bridge
        .send(BridgeCommand::Prompt {
            session_id: replacement_session.clone(),
            text: "start a replacement turn".to_string(),
        })
        .await
        .unwrap();

    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), bridge.recv()).await,
        Ok(Some(BridgeEvent::ProposalsChanged { session_id })) if session_id == replacement_session
    ));
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), bridge.recv()).await,
        Ok(Some(BridgeEvent::Completed { session_id, stop_reason }))
            if session_id == replacement_session && stop_reason == "end_turn"
    ));
    assert_eq!(
        state.lock().unwrap().writes,
        vec![(
            PathBuf::from("/workspace/replacement.rs"),
            "replacement proposal".to_string(),
        )]
    );

    drop(bridge);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_failed_session_update_callback_does_not_terminate_the_actor() {
    let state = Arc::new(Mutex::new(HostState::default()));
    let (update_tx, _update_rx) = mpsc::unbounded_channel();
    let host = RecordingHost {
        state,
        updates: update_tx,
        reject_outside_workspace: false,
        reject_updates: true,
    };
    let executable = env!("CARGO_BIN_EXE_acp_conformance_fixture");
    let spawned = AcpSpawn::start(AcpProcessSpec::new(executable), host).unwrap();
    let _: InitializeResponse = spawned.client.request(initialize_request()).await.unwrap();
    let session: NewSessionResponse = spawned
        .client
        .request(ClientRequest::NewSessionRequest(NewSessionRequest::new(
            "/workspace",
        )))
        .await
        .unwrap();
    let prompt_client = spawned.client.clone();
    let session_id = session.session_id.clone();
    let prompt = tokio::spawn(async move {
        prompt_client
            .request::<PromptResponse>(ClientRequest::PromptRequest(PromptRequest::new(
                session_id,
                vec![ContentBlock::Text(TextContent::new("stream an update"))],
            )))
            .await
    });
    tokio::time::sleep(Duration::from_millis(80)).await;
    spawned
        .client
        .notify(ClientNotification::CancelNotification(
            CancelNotification::new(session.session_id),
        ))
        .await
        .unwrap();
    assert_eq!(
        prompt.await.unwrap().unwrap().stop_reason,
        StopReason::Cancelled
    );
    spawned.client.shutdown().await.unwrap();
    spawned.task.await.unwrap().unwrap();
}
