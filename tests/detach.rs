#![cfg(unix)]

use std::{
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    time::Duration,
};

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

fn mock_codex(directory: &Path) -> PathBuf {
    let path = directory.join("codex");
    std::fs::write(
        &path,
        r#"#!/usr/bin/env python3
import json, os, sys

with open(os.environ["RED_CODEX_FIXTURE_PID_FILE"], "w") as pid:
    pid.write(str(os.getpid()))

def send(value):
    print(json.dumps(value), flush=True)

for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    ident = message.get("id")
    if method == "initialize":
        send({"id": ident, "result": {"userAgent": "detach-mock"}})
    elif method == "account/read":
        send({"id": ident, "result": {
            "account": {"type": "chatgpt"}, "requiresOpenaiAuth": True
        }})
    elif method == "config/read":
        send({"id": ident, "result": {
            "config": {"mcp_servers": {}}, "origins": {}
        }})
    elif method == "configRequirements/read":
        send({"id": ident, "result": {"requirements": None}})
    elif method == "thread/start":
        send({"id": ident, "result": {"thread": {"id": "detach-thread"}}})
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}

async fn wait_for_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Codex app-server fixture did not start");
}

#[tokio::test(flavor = "current_thread")]
async fn running_codex_process_survives_disconnect_and_reattach() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("agent.pid");
    let mut config = Config::from_user_toml_with_overrides("", &[]).unwrap();
    config.agent.command = Some(mock_codex(directory.path()).display().to_string());
    config.agent.env.insert(
        "RED_CODEX_FIXTURE_PID_FILE".to_string(),
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
        drop(first);
        tokio::time::sleep(Duration::from_millis(100)).await;

        nix::sys::signal::kill(nix::unistd::Pid::from_raw(original_pid), None)
            .expect("the original Codex app-server process must remain alive");
        let second = connect_session(directory.path(), "agent-work", None, (80, 24))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&pid_file).unwrap().trim(),
            original_pid.to_string(),
            "reattach must not restart Codex app-server"
        );
        stop_session(directory.path(), "agent-work").await.unwrap();
        drop(second);
    };

    let (server_result, ()) = tokio::join!(server, client);
    server_result.unwrap();
}
