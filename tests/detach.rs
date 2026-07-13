#![cfg(unix)]

use std::{path::Path, time::Duration};

use red::{
    buffer::Buffer,
    config::Config,
    editor::{DetachedEditorCore, Editor, PluginRequest, ACTION_DISPATCHER},
    headless::{
        bind_session, connect_session, serve_editor_session, stop_session, InputEvent, KeyCode,
    },
    lsp::LspManager,
    theme::Theme,
};

async fn wait_for_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("ACP fixture did not start");
}

#[tokio::test(flavor = "current_thread")]
async fn running_agent_process_survives_disconnect_and_reattach() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("agent.pid");
    let mut config = Config::from_user_toml_with_overrides("", &[]).unwrap();
    config.agent.command = Some(env!("CARGO_BIN_EXE_acp_conformance_fixture").to_string());
    config.agent.env.insert(
        "RED_ACP_FIXTURE_PID_FILE".to_string(),
        pid_file.display().to_string(),
    );
    let lsp = Box::new(LspManager::new(config.lsp.clone()));
    let editor = Editor::with_size(
        lsp,
        80,
        24,
        config,
        Theme::default(),
        vec![Buffer::new(None, "agent-owned buffer\n".to_string())],
    )
    .unwrap();
    let core = DetachedEditorCore::new(editor).await.unwrap();
    let session = bind_session(directory.path(), "agent-work").unwrap();

    let server = serve_editor_session(&session, core);
    let client = async {
        let mut first = connect_session(directory.path(), "agent-work", None, (80, 24))
            .await
            .unwrap();
        ACTION_DISPATCHER.send_request(PluginRequest::AgentNewSession {
            cwd: directory.path().to_path_buf(),
        });
        wait_for_file(&pid_file).await;
        let original_pid = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();

        // Exercise the production input/edit path before dropping the transport.
        first
            .input(InputEvent::Key {
                code: KeyCode::Character('i'),
                modifiers: Vec::new(),
            })
            .await
            .unwrap();
        first
            .input(InputEvent::Paste {
                text: "kept ".to_string(),
            })
            .await
            .unwrap();
        drop(first); // Model an SSH transport disappearing without a detach handshake.
        tokio::time::sleep(Duration::from_millis(100)).await;

        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(original_pid),
            /*signal*/ None,
        )
        .expect("the original ACP adapter process must remain alive");
        let second = connect_session(directory.path(), "agent-work", None, (80, 24))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&pid_file).unwrap().trim(),
            original_pid.to_string(),
            "reattach must not restart the adapter"
        );
        stop_session(directory.path(), "agent-work").await.unwrap();
        drop(second);
    };

    let (server_result, ()) = tokio::join!(server, client);
    server_result.unwrap();
}
