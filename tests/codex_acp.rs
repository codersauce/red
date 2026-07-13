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
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter},
    process::{Child, ChildStdin, ChildStdout, Command},
};

const TEST_TIMEOUT: Duration = Duration::from_secs(10);
const MOCK_APP_SERVER: &str = r#"#!/usr/bin/env python3
import json
import os
import pathlib
import sys

mode = os.environ['MOCK_MODE']
record = pathlib.Path(os.environ['MOCK_RECORD'])
thread_id = 'thread-red-codex'
thread_count = 0
turn_id = 'turn-red-codex'
turn_count = 0
pending_turn = None
seen = []

def save(event, value):
    seen.append({'event': event, 'value': value})
    temporary = record.with_suffix('.tmp')
    temporary.write_text(json.dumps(seen))
    temporary.replace(record)

def send(value):
    sys.stdout.write(json.dumps(value) + '\n')
    sys.stdout.flush()

def receive():
    line = sys.stdin.readline()
    if not line:
        raise SystemExit(0)
    return json.loads(line)

def call(tool, arguments, call_turn_id=None):
    call_id = 'tool-' + str(len(seen))
    send({'id': call_id, 'method': 'item/tool/call', 'params': {
        'threadId': thread_id, 'turnId': call_turn_id or turn_id, 'callId': call_id,
        'tool': tool, 'arguments': arguments,
    }})
    response = receive()
    save('tool:' + tool, response)
    return response

while True:
    request = receive()
    method = request.get('method')
    if method == 'initialize':
        save('launch-args', sys.argv[1:])
        save('launch-env', {key: os.environ.get(key) for key in ('CODEX_APP_SERVER_MANAGED_CONFIG_PATH', 'CODEX_APP_SERVER_DISABLE_MANAGED_CONFIG', 'CODEX_APP_SERVER_TEST_USER_CONFIG_FILE')})
        save('auth-env', {key: os.environ.get(key) for key in ('CODEX_REFRESH_TOKEN_URL_OVERRIDE', 'CODEX_REVOKE_TOKEN_URL_OVERRIDE', 'CODEX_APP_SERVER_LOGIN_CLIENT_ID', 'CODEX_AUTHAPI_BASE_URL')})
        save('launch-cwd', {'cwd': os.path.realpath(os.getcwd()), 'codexHome': os.path.realpath(os.environ['CODEX_HOME']), 'sourceHome': os.path.realpath(os.environ['MOCK_SOURCE_HOME'])})
        save('launch-auth', {'isolatedAuthExists': (pathlib.Path(os.environ['CODEX_HOME']) / 'auth.json').exists(), 'sourceAuthExists': (pathlib.Path(os.environ['MOCK_SOURCE_HOME']) / 'auth.json').exists()})
        save('initialize', request['params'])
        if mode == 'incompatible':
            send({'id': request['id'], 'error': {'code': -32602, 'message': 'experimental API is unavailable'}})
            raise SystemExit(0)
        send({'id': request['id'], 'result': {'userAgent': 'mock-codex'}})
    elif method == 'initialized':
        save('initialized', request.get('params', {}))
    elif method == 'account/read':
        save('account', request['params'])
        if mode == 'hold-account' or (mode == 'saturated-cancel' and thread_count > 0):
            continue
        if mode == 'unauthenticated':
            send({'id': request['id'], 'result': {'account': None, 'requiresOpenaiAuth': True}})
        else:
            send({'id': request['id'], 'result': {'account': {'type': 'chatgpt', 'email': None, 'planType': 'pro'}, 'requiresOpenaiAuth': True}})
        if mode == 'delayed-start' and pending_turn is not None:
            send({'id': pending_turn, 'result': {'turn': {'id': turn_id, 'items': [], 'status': 'inProgress', 'error': None}}})
            pending_turn = None
    elif method == 'config/read':
        save('config', request['params'])
        if mode == 'remote-thread-config':
            snapshot = (pathlib.Path(os.environ['CODEX_HOME']) / 'config.toml').read_text()
            save('remote-config', {'snapshotHasEndpoint': 'experimental_thread_config_endpoint' in snapshot})
        if mode == 'config-error':
            send({'id': request['id'], 'error': {'code': -32603, 'message': 'config could not be read'}})
        elif mode == 'config-invalid':
            send({'id': request['id'], 'result': {'config': {}}})
        elif mode == 'config-origins-invalid':
            send({'id': request['id'], 'result': {'config': {'mcp_servers': {}}, 'origins': []}})
        elif mode in ('config-large-escaped', 'config-many-servers'):
            snapshot = (pathlib.Path(os.environ['CODEX_HOME']) / 'config.toml').read_text()
            instructions = snapshot.split("developer_instructions = '", 1)[1].split("'\n", 1)[0] if "developer_instructions = '" in snapshot else ''
            servers = {
                line[len('[mcp_servers.'):-1]: {'command': 'must-not-launch', 'enabled': True}
                for line in snapshot.splitlines()
                if line.startswith('[mcp_servers.')
            }
            response = {'id': request['id'], 'result': {
                'config': {'mcp_servers': servers, 'developer_instructions': instructions},
                'origins': {},
            }}
            save('large-config', {
                'snapshotBytes': len(snapshot.encode()),
                'responseBytes': len(json.dumps(response).encode()),
            })
            send(response)
        else:
            servers = {
                'filesystem': {'command': 'filesystem-server', 'enabled': True},
                'git.tools': {'url': 'https://example.invalid/mcp', 'enabled': True},
            }
            origins = {
                'mcp_servers.filesystem.enabled': {'name': {'type': 'user'}, 'version': 'user-version'},
                'mcp_servers.git.tools.enabled': {'name': {'type': 'project'}, 'version': 'project-version'},
                'notify.0': {'name': {'type': 'user'}, 'version': 'user-version'},
            }
            config = {'mcp_servers': servers, 'features': {}, 'orchestrator': {'mcp': {'enabled': False}}, 'notify': ['notify-command']}
            if mode == 'config-origin-invalid':
                origins['mcp_servers.git.tools.enabled'] = {'name': {}, 'version': 'invalid-version'}
            elif mode == 'config-origin-unknown':
                origins['mcp_servers.git.tools.enabled'] = {'name': {'type': 'futureManaged'}, 'version': 'unknown-version'}
            elif mode in ('managed-enabled-file', 'managed-enabled-mdm', 'managed-disabled'):
                server = 'filesystem' if mode == 'managed-enabled-file' else 'git.tools'
                source = 'legacyManagedConfigTomlFromFile' if mode == 'managed-enabled-file' else 'legacyManagedConfigTomlFromMdm'
                servers[server]['enabled'] = mode != 'managed-disabled'
                origins['mcp_servers.' + server + '.enabled'] = {'name': {'type': source}, 'version': 'managed-version'}
            elif mode in ('managed-feature-apps', 'managed-feature-connectors', 'managed-feature-plugins', 'managed-feature-skill', 'managed-feature-hooks', 'managed-feature-codex-hooks', 'managed-feature-orchestrator'):
                feature = {
                    'managed-feature-apps': 'apps',
                    'managed-feature-connectors': 'connectors',
                    'managed-feature-plugins': 'plugins',
                    'managed-feature-skill': 'skill_mcp_dependency_install',
                    'managed-feature-hooks': 'hooks',
                    'managed-feature-codex-hooks': 'codex_hooks',
                    'managed-feature-orchestrator': 'orchestrator.mcp.enabled',
                }[mode]
                if feature == 'orchestrator.mcp.enabled':
                    config['orchestrator']['mcp']['enabled'] = True
                    path = feature
                else:
                    config['features'][feature] = True
                    path = 'features.' + feature
                origins[path] = {'name': {'type': 'legacyManagedConfigTomlFromMdm'}, 'version': 'managed-version'}
            elif mode == 'managed-notify':
                origins['notify.0'] = {'name': {'type': 'legacyManagedConfigTomlFromFile'}, 'version': 'managed-version'}
            elif mode == 'managed-notify-origin-invalid':
                origins['notify.0'] = {'name': {}, 'version': 'invalid-version'}
            elif mode == 'managed-notify-origin-missing':
                del origins['notify.0']
            elif mode in ('external-endpoint-cloud', 'external-lockfile-system-load', 'external-lockfile-cloud-export'):
                source = 'enterpriseManaged' if 'cloud' in mode else 'system'
                if mode == 'external-endpoint-cloud':
                    config['experimental_thread_config_endpoint'] = 'http://127.0.0.1:9999/session'
                    path = 'experimental_thread_config_endpoint'
                elif mode == 'external-lockfile-system-load':
                    config['debug'] = {'config_lockfile': {'load_path': '/must-not-load/session.toml'}}
                    path = 'debug.config_lockfile.load_path'
                else:
                    config['debug'] = {'config_lockfile': {'export_dir': '/must-not-share/exports'}}
                    path = 'debug.config_lockfile.export_dir'
                origins[path] = {'name': {'type': source}, 'version': 'external-version'}
            send({'id': request['id'], 'result': {'config': config, 'origins': origins}})
            if mode == 'config-race':
                source_home = pathlib.Path(os.environ['MOCK_SOURCE_HOME'])
                (source_home / 'config.toml').write_text('[mcp_servers.raced]\ncommand = "must-not-launch"\nenabled = true\n')
    elif method == 'configRequirements/read':
        save('requirements', request.get('params'))
        if mode == 'requirements-error':
            send({'id': request['id'], 'error': {'code': -32603, 'message': 'requirements could not be read'}})
        elif mode == 'requirements-invalid':
            send({'id': request['id'], 'result': {'requirements': {'featureRequirements': []}}})
        elif mode in ('requirements-apps', 'requirements-connectors', 'requirements-plugins', 'requirements-skill', 'requirements-hooks', 'requirements-codex-hooks'):
            feature = {'requirements-apps': 'apps', 'requirements-connectors': 'connectors', 'requirements-plugins': 'plugins', 'requirements-skill': 'skill_mcp_dependency_install', 'requirements-hooks': 'hooks', 'requirements-codex-hooks': 'codex_hooks'}[mode]
            send({'id': request['id'], 'result': {'requirements': {'featureRequirements': {feature: True}}}})
        elif mode == 'managed-disabled':
            send({'id': request['id'], 'result': {'requirements': {'featureRequirements': {'apps': False, 'connectors': False, 'plugins': False, 'skill_mcp_dependency_install': False, 'hooks': False, 'codex_hooks': False}}}})
        else:
            send({'id': request['id'], 'result': {'requirements': None}})
    elif method == 'thread/start':
        save('thread', request['params'])
        if mode == 'config-race':
            source_home = pathlib.Path(os.environ['MOCK_SOURCE_HOME'])
            runtime_home = pathlib.Path(os.environ['CODEX_HOME'])
            cwd = request['params']['cwd']
            projects = request['params']['config'].get('projects', {})
            trust = projects.get(os.path.normcase(cwd), {}).get('trust_level')
            snapshot = (runtime_home / 'config.toml').read_text()
            save('isolation', {
                'sameHome': source_home == runtime_home,
                'snapshotHasRacedServer': 'mcp_servers.raced' in snapshot,
                'snapshotHasProjects': 'projects.' in snapshot,
                'snapshotHasSqliteHome': 'sqlite_home' in snapshot,
                'snapshotHasLogDir': 'log_dir' in snapshot,
                'snapshotHasConfigLockfile': 'config_lockfile' in snapshot,
                'sqliteHomeIsolated': pathlib.Path(os.environ.get('CODEX_SQLITE_HOME', '')) == runtime_home,
                'projectTrust': trust,
                'trustedAncestors': [str(path) for path in pathlib.Path(cwd).parents if projects.get(os.path.normcase(str(path)), {}).get('trust_level') != 'untrusted'],
            })
        thread_count += 1
        thread_id = 'thread-red-codex' if thread_count == 1 else 'thread-red-codex-' + str(thread_count)
        send({'id': request['id'], 'result': {'thread': {'id': thread_id}}})
    elif method == 'thread/unsubscribe':
        save('unsubscribe', request['params'])
        send({'id': request['id'], 'result': {'status': 'unsubscribed'}})
    elif method == 'thread/archive':
        save('archive', request['params'])
        if turn_count == 0:
            send({'id': request['id'], 'error': {'code': -32600, 'message': 'no rollout found for thread id'}})
        else:
            send({'id': request['id'], 'result': {}})
    elif method == 'turn/start':
        save('turn', request['params'])
        turn_count += 1
        if mode == 'stale-turns' and turn_count > 1:
            turn_id = 'turn-red-codex-' + str(turn_count)
        if mode == 'delayed-start':
            pending_turn = request['id']
            continue
        send({'id': request['id'], 'result': {'turn': {'id': turn_id, 'items': [], 'status': 'inProgress', 'error': None}}})
        if mode == 'stale-turns' and turn_count == 1:
            send({'method': 'item/agentMessage/delta', 'params': {'threadId': thread_id, 'turnId': 'wrong-turn', 'itemId': 'message', 'delta': 'must be ignored before cancellation'}})
            send({'method': 'item/agentMessage/delta', 'params': {'threadId': thread_id, 'turnId': turn_id, 'itemId': 'message', 'delta': 'working'}})
            continue
        if mode == 'stale-turns':
            send({'method': 'turn/completed', 'params': {'threadId': thread_id, 'turn': {'id': 'turn-red-codex', 'items': [], 'status': 'failed', 'error': {'message': 'stale failure'}}}})
            send({'method': 'item/agentMessage/delta', 'params': {'threadId': thread_id, 'turnId': 'turn-red-codex', 'itemId': 'message', 'delta': 'must be ignored after a new turn starts'}})
            call('read_file', {'path': 'stale-read.rs'}, 'turn-red-codex')
            call('write_file', {'path': 'stale-write.rs', 'content': 'must not be staged'}, 'turn-red-codex')
            send({'method': 'item/agentMessage/delta', 'params': {'threadId': thread_id, 'turnId': turn_id, 'itemId': 'message', 'delta': 'fresh output'}})
            send({'method': 'turn/completed', 'params': {'threadId': thread_id, 'turn': {'id': turn_id, 'items': [], 'status': 'completed', 'error': None}}})
            call('read_file', {'path': 'completed-read.rs'})
            call('write_file', {'path': 'completed-write.rs', 'content': 'must not be staged'})
            continue
        if mode == 'cancel' or mode == 'saturated-cancel':
            send({'method': 'item/agentMessage/delta', 'params': {'threadId': thread_id, 'turnId': turn_id, 'itemId': 'message', 'delta': 'working'}})
            continue
        if mode == 'close':
            save('closed', request['params'])
            raise SystemExit(0)
        if mode == 'invalid':
            save('invalid', request['params'])
            sys.stdout.write('{invalid app-server data}\n')
            sys.stdout.flush()
            continue
        if mode == 'failed':
            send({'method': 'turn/completed', 'params': {'threadId': thread_id, 'turn': {'id': turn_id, 'items': [], 'status': 'failed', 'error': {'message': 'secret backend details'}}}})
            continue
        if mode in ('large-ascii-delta', 'large-escaped-delta'):
            delta = 'x' * (960 * 1024 + 1) if mode == 'large-ascii-delta' else '\\' * 500000
            send({'method': 'item/agentMessage/delta', 'params': {'threadId': thread_id, 'turnId': turn_id, 'itemId': 'message', 'delta': delta}})
            send({'method': 'turn/completed', 'params': {'threadId': thread_id, 'turn': {'id': turn_id, 'items': [], 'status': 'completed', 'error': None}}})
            continue
        if mode == 'callback-cancel':
            call('read_file', {'path': 'example.rs'})
            continue
        if mode == 'proposal':
            call('list_files', {})
            if os.name == 'posix':
                call('search_files', {'query': 'disk contents'})
            call('read_file', {'path': 'existing.rs'})
            call('read_file', {'path': 'new.rs'})
            call('write_file', {'path': 'existing.rs', 'content': 'staged existing contents\n'})
            call('write_file', {'path': 'new.rs', 'content': 'staged new contents\n'})
            call('read_file', {'path': 'existing.rs'})
            call('read_file', {'path': 'new.rs'})
        elif mode == 'bounded-read':
            call('read_file', {'path': 'existing.rs'})
        elif mode == 'unsafe':
            call('write_file', {'path': '../outside.rs', 'content': 'must not be created'})
            if os.name == 'posix':
                call('write_file', {'path': 'linked.rs', 'content': 'must not follow link'})
            call('read_file', {'path': 'existing.rs', 'extra': 'must be rejected'})
            send({'id': 'native-write', 'method': 'item/fileChange/requestApproval', 'params': {'threadId': thread_id, 'turnId': turn_id, 'itemId': 'native-write'}})
            save('native-approval', receive())
            send({'id': 'native-command', 'method': 'item/commandExecution/requestApproval', 'params': {'threadId': thread_id, 'turnId': turn_id, 'itemId': 'native-command'}})
            save('command-approval', receive())
            send({'id': 'native-permissions', 'method': 'item/permissions/requestApproval', 'params': {'threadId': thread_id, 'turnId': turn_id, 'itemId': 'native-permissions', 'permissions': {'fileSystem': {'write': ['/']}}}})
            save('permissions-approval', receive())
        send({'method': 'item/agentMessage/delta', 'params': {'threadId': thread_id, 'turnId': turn_id, 'itemId': 'message', 'delta': 'Proposal is ready for review.'}})
        send({'method': 'turn/completed', 'params': {'threadId': thread_id, 'turn': {'id': turn_id, 'items': [], 'status': 'completed', 'error': None}}})
    elif method == 'turn/interrupt':
        save('interrupt', request['params'])
        send({'id': request['id'], 'result': {}})
        if mode == 'stale-turns':
            send({'method': 'item/agentMessage/delta', 'params': {'threadId': thread_id, 'turnId': turn_id, 'itemId': 'message', 'delta': 'must be ignored during cancellation'}})
            call('read_file', {'path': 'cancelled-read.rs'})
            call('write_file', {'path': 'cancelled-write.rs', 'content': 'must not be staged'})
        send({'method': 'turn/completed', 'params': {'threadId': thread_id, 'turn': {'id': turn_id, 'items': [], 'status': 'interrupted', 'error': None}}})
        if mode == 'stale-turns':
            send({'method': 'item/agentMessage/delta', 'params': {'threadId': thread_id, 'turnId': turn_id, 'itemId': 'message', 'delta': 'must be ignored after cancellation'}})
    else:
        save('unexpected', request)
        if 'id' in request:
            send({'id': request['id'], 'error': {'code': -32601, 'message': 'unexpected request'}})
"#;

struct Harness {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    record: PathBuf,
    _mock: tempfile::TempDir,
}

impl Harness {
    fn start(mode: &str) -> Self {
        let mock = tempfile::tempdir().unwrap();
        let codex_home = mock.path().join("codex-home");
        std::fs::create_dir(&codex_home).unwrap();
        let mut config = String::new();
        if mode == "ephemeral-auth" {
            config.push_str("cli_auth_credentials_store = \"ephemeral\"\n");
        }
        if mode == "remote-thread-config" {
            config.push_str(
                "experimental_thread_config_endpoint = \"http://127.0.0.1:9999/session\"\n",
            );
        }
        if mode == "config-large-escaped" {
            config.push_str("developer_instructions = '");
            config.push_str(&"\\".repeat(768 * 1024));
            config.push_str("'\n");
        }
        config.push_str(
            "model = \"test-model\"\nsqlite_home = \"/must-not-share/sqlite\"\nlog_dir = \"/must-not-share/log\"\n[debug.config_lockfile]\nexport_dir = \"/must-not-share/exports\"\nload_path = \"/must-not-load/session.toml\"\n[projects.\"/trusted/root\"]\ntrust_level = \"trusted\"\n[mcp_servers.existing]\ncommand = \"must-not-launch\"\nenabled = true\n",
        );
        if mode == "config-many-servers" {
            for index in 0..16_000 {
                config.push_str(&format!(
                    "[mcp_servers.server_{index:05}_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx]\ncommand = \"must-not-launch\"\nenabled = true\n"
                ));
            }
        }
        std::fs::write(codex_home.join("config.toml"), config).unwrap();
        if mode == "ephemeral-auth" {
            std::fs::write(codex_home.join("auth.json"), "stale file credentials").unwrap();
        }
        let script = mock.path().join("mock-codex.py");
        let record = mock.path().join("record.json");
        std::fs::write(&script, MOCK_APP_SERVER).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        #[cfg(windows)]
        let script = {
            let launcher = mock.path().join("mock-codex.cmd");
            std::fs::write(
                &launcher,
                "@echo off\r\npython \"%~dp0mock-codex.py\" %*\r\n",
            )
            .unwrap();
            launcher
        };
        let codex = if mode == "relative-codex" {
            PathBuf::from(".").join(script.file_name().unwrap())
        } else {
            script.clone()
        };
        let mut child = Command::new(env!("CARGO_BIN_EXE_red_codex_acp"))
            .arg("--codex")
            .arg(&codex)
            .current_dir(mock.path())
            .env("MOCK_MODE", mode)
            .env("MOCK_RECORD", &record)
            .env("MOCK_SOURCE_HOME", &codex_home)
            .env("CODEX_HOME", &codex_home)
            .env(
                "CODEX_ACCESS_TOKEN",
                if mode == "ephemeral-auth" {
                    "at-test-token"
                } else {
                    ""
                },
            )
            .env(
                "CODEX_APP_SERVER_MANAGED_CONFIG_PATH",
                "/must-not-load/managed.toml",
            )
            .env("CODEX_APP_SERVER_DISABLE_MANAGED_CONFIG", "1")
            .env(
                "CODEX_APP_SERVER_TEST_USER_CONFIG_FILE",
                "/must-not-load/config.toml",
            )
            .env(
                "CODEX_REFRESH_TOKEN_URL_OVERRIDE",
                "https://refresh.invalid",
            )
            .env("CODEX_REVOKE_TOKEN_URL_OVERRIDE", "https://revoke.invalid")
            .env("CODEX_APP_SERVER_LOGIN_CLIENT_ID", "must-not-use-client-id")
            .env("CODEX_AUTHAPI_BASE_URL", "https://auth.invalid")
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
            record,
            _mock: mock,
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

    async fn initialize(&mut self) {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": 1, "clientCapabilities": {"fs": {"readTextFile": true, "writeTextFile": true}}}
        }))
        .await;
        let initialized = self.next().await;
        assert_eq!(initialized["result"]["protocolVersion"], 1);
        assert_eq!(initialized["result"]["agentInfo"]["name"], "red-codex-acp");
        assert_eq!(
            initialized["result"]["agentCapabilities"]["sessionCapabilities"]["close"],
            json!({})
        );
    }

    async fn create_session(&mut self, cwd: &Path) -> String {
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

    fn available_events(&self) -> Vec<Value> {
        std::fs::read(&self.record)
            .ok()
            .and_then(|contents| serde_json::from_slice(&contents).ok())
            .unwrap_or_default()
    }

    async fn finish(mut self) -> Vec<Value> {
        self.stdin.shutdown().await.unwrap();
        drop(self.stdin);
        drop(self.stdout);
        let output = tokio::time::timeout(TEST_TIMEOUT, self.child.wait_with_output())
            .await
            .expect("ACP process did not stop")
            .unwrap();
        assert!(output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("unsaved existing contents"));
        assert!(!stderr.contains("staged existing contents"));
        assert!(!stderr.contains("must not"));
        serde_json::from_slice(&std::fs::read(&self.record).unwrap()).unwrap()
    }
}

fn event<'a>(events: &'a [Value], name: &str) -> &'a Value {
    events
        .iter()
        .find(|entry| entry["event"] == name)
        .unwrap_or_else(|| panic!("missing recorded event {name}"))
}

#[cfg(unix)]
#[tokio::test]
async fn codex_rejects_a_workspace_root_below_a_symlinked_parent() {
    let workspace = tempfile::tempdir().unwrap();
    let real_parent = workspace.path().join("real-parent");
    std::fs::create_dir_all(real_parent.join("project")).unwrap();
    let linked_parent = workspace.path().join("linked-parent");
    std::os::unix::fs::symlink(&real_parent, &linked_parent).unwrap();
    let mut acp = Harness::start("proposal");
    acp.initialize().await;

    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "session/new",
        "params": {"cwd": linked_parent.join("project")}
    }))
    .await;

    let response = acp.next().await;
    assert_eq!(response["error"]["code"], -32_602);
    assert_eq!(
        response["error"]["message"],
        "Codex workspace root is invalid"
    );
    acp.finish().await;
}

#[cfg(unix)]
#[tokio::test]
async fn codex_rejects_workspace_tools_after_an_ancestor_is_replaced_by_a_symlink() {
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
    let mut acp = Harness::start("proposal");
    acp.initialize().await;
    let session = acp.create_session(&project).await;
    std::fs::rename(&parent, root.path().join("original-parent")).unwrap();
    std::os::unix::fs::symlink(&outside_parent, &parent).unwrap();

    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "inspect the workspace"}]}
    }))
    .await;

    assert_eq!(acp.next().await["method"], "session/update");
    assert_eq!(acp.next().await["result"]["stopReason"], "end_turn");
    let events = acp.finish().await;
    assert!(!outside.join("new.rs").exists());

    let tools = events
        .iter()
        .filter(|event| {
            event["event"]
                .as_str()
                .is_some_and(|name| name.starts_with("tool:"))
        })
        .collect::<Vec<_>>();
    assert!(!tools.is_empty());
    assert!(tools
        .iter()
        .all(|event| event["value"]["result"]["success"] == false));
    let recorded = serde_json::to_string(&tools).unwrap();
    assert!(!recorded.contains("secret-name.rs"));
    assert!(!recorded.contains("outside secret contents"));
}

#[tokio::test]
async fn codex_dynamic_tools_round_trip_the_real_proposal_host_without_touching_disk() {
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
    let mut acp = Harness::start("proposal");
    acp.initialize().await;
    let session = acp.create_session(workspace.path()).await;
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
        let result = serde_json::to_value(host.read_text_file(request).await.unwrap()).unwrap();
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
        let result = serde_json::to_value(host.write_text_file(request).await.unwrap()).unwrap();
        acp.send(json!({"jsonrpc": "2.0", "id": write["id"], "result": result}))
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
        let result = serde_json::to_value(host.read_text_file(request).await.unwrap()).unwrap();
        assert_eq!(result["content"], contents);
        acp.send(json!({"jsonrpc": "2.0", "id": read["id"], "result": result}))
            .await;
    }
    let update = acp.next().await;
    assert_eq!(update["method"], "session/update");
    assert_eq!(
        update["params"]["update"]["content"]["text"],
        "Proposal is ready for review."
    );
    assert_eq!(acp.next().await["result"]["stopReason"], "end_turn");
    let events = acp.finish().await;

    let initialize = &event(&events, "initialize")["value"];
    assert_eq!(initialize["capabilities"]["experimentalApi"], true);
    assert_eq!(
        event(&events, "launch-args")["value"],
        json!([
            "app-server",
            "-c",
            "cli_auth_credentials_store=\"file\"",
            "-c",
            "mcp_oauth_credentials_store=\"file\"",
            "-c",
            "features.plugins=false",
            "-c",
            "features.remote_plugin=false"
        ])
    );
    assert_eq!(
        event(&events, "launch-env")["value"],
        json!({
            "CODEX_APP_SERVER_MANAGED_CONFIG_PATH": null,
            "CODEX_APP_SERVER_DISABLE_MANAGED_CONFIG": null,
            "CODEX_APP_SERVER_TEST_USER_CONFIG_FILE": null
        })
    );
    assert_eq!(
        event(&events, "auth-env")["value"],
        json!({
            "CODEX_REFRESH_TOKEN_URL_OVERRIDE": null,
            "CODEX_REVOKE_TOKEN_URL_OVERRIDE": null,
            "CODEX_APP_SERVER_LOGIN_CLIENT_ID": null,
            "CODEX_AUTHAPI_BASE_URL": null
        })
    );
    let bootstrap = &event(&events, "launch-cwd")["value"];
    assert_eq!(bootstrap["cwd"], bootstrap["codexHome"]);
    assert_ne!(bootstrap["cwd"], bootstrap["sourceHome"]);
    let thread = &event(&events, "thread")["value"];
    let config = &event(&events, "config")["value"];
    let requirements = &event(&events, "requirements")["value"];
    assert_eq!(config["includeLayers"], false);
    assert_eq!(config["cwd"], workspace.path().to_string_lossy().as_ref());
    assert!(requirements.is_null());
    assert_eq!(thread["environments"], json!([]));
    assert_eq!(thread["sandbox"], "read-only");
    assert_eq!(thread["approvalPolicy"], "never");
    let mut expected_config = json!({
        "mcp_servers": {
            "filesystem": {"enabled": false},
            "git.tools": {"enabled": false}
        },
        "features": {
            "apps": false,
            "connectors": false,
            "plugins": false,
            "skill_mcp_dependency_install": false,
            "hooks": false,
            "codex_hooks": false
        },
        "orchestrator": {"mcp": {"enabled": false}},
        "notify": []
    });
    let projects = thread["config"]["projects"].clone();
    expected_config["projects"] = projects.clone();
    assert_eq!(thread["config"], expected_config);
    for ancestor in workspace.path().ancestors() {
        let key = ancestor.to_string_lossy();
        #[cfg(windows)]
        let key = key.to_ascii_lowercase();
        assert_eq!(projects[&*key]["trust_level"], "untrusted");
    }
    let tools = thread["dynamicTools"].as_array().unwrap();
    assert_eq!(tools.len(), 4);
    assert_eq!(tools[0]["name"], "list_files");
    assert_eq!(tools[1]["name"], "search_files");
    assert_eq!(tools[2]["name"], "read_file");
    assert_eq!(tools[3]["name"], "write_file");
    let turn = &event(&events, "turn")["value"];
    assert_eq!(turn["environments"], json!([]));
    assert_eq!(turn["approvalPolicy"], "never");
    assert_eq!(turn["sandboxPolicy"]["type"], "readOnly");
    let list = &event(&events, "tool:list_files")["value"];
    let list_text = list["result"]["contentItems"][0]["text"].as_str().unwrap();
    assert!(list_text.contains("existing.rs"));
    #[cfg(unix)]
    {
        let search = &event(&events, "tool:search_files")["value"];
        let search_text = search["result"]["contentItems"][0]["text"]
            .as_str()
            .unwrap();
        assert!(search_text.contains("disk contents"));
    }

    let mut proposals = proposal_workspace.lock().unwrap();
    let disposition = proposals
        .accept_all(&session, &existing, 7, "unsaved existing contents\n")
        .unwrap();
    assert!(matches!(
        disposition,
        ProposalDisposition::Applied { contents, created: false, .. }
            if contents == "staged existing contents\n"
    ));
    proposals.reject_all(&session, &created, 0, "").unwrap();
    assert!(proposals.pending_files(&session).is_empty());
    assert_eq!(
        std::fs::read_to_string(existing).unwrap(),
        "disk contents\n"
    );
    assert!(!created.exists());
}

#[tokio::test]
async fn codex_bridge_rejects_unsafe_tools_and_native_file_approval_without_fallback() {
    let workspace = tempfile::tempdir().unwrap();
    let existing = workspace.path().join("existing.rs");
    let outside = workspace.path().parent().unwrap().join("outside.rs");
    std::fs::write(&existing, "disk contents\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&existing, workspace.path().join("linked.rs")).unwrap();
    let mut acp = Harness::start("unsafe");
    acp.initialize().await;
    let session = acp.create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "attempt unsafe writes"}]}
    }))
    .await;
    assert_eq!(acp.next().await["method"], "session/update");
    assert_eq!(acp.next().await["result"]["stopReason"], "end_turn");
    let events = acp.finish().await;
    assert_eq!(
        std::fs::read_to_string(&existing).unwrap(),
        "disk contents\n"
    );
    assert!(!outside.exists());
    let writes: Vec<_> = events
        .iter()
        .filter(|entry| entry["event"] == "tool:write_file")
        .collect();
    assert_eq!(writes.len(), if cfg!(unix) { 2 } else { 1 });
    assert!(writes
        .iter()
        .all(|entry| entry["value"]["result"]["success"] == false));
    assert_eq!(
        event(&events, "tool:read_file")["value"]["result"]["success"],
        false
    );
    assert_eq!(
        event(&events, "native-approval")["value"]["result"]["decision"],
        "decline"
    );
    assert_eq!(
        event(&events, "command-approval")["value"]["result"]["decision"],
        "decline"
    );
    assert_eq!(
        event(&events, "permissions-approval")["value"]["result"]["permissions"],
        json!({})
    );
}

#[tokio::test]
async fn codex_returns_bounded_failures_for_maximum_and_escape_heavy_reads() {
    for content in ["x".repeat(960 * 1024), "\\".repeat(300_000)] {
        let workspace = tempfile::tempdir().unwrap();
        let mut acp = Harness::start("bounded-read");
        acp.initialize().await;
        let session = acp.create_session(workspace.path()).await;
        acp.send(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/prompt",
            "params": {"sessionId": session, "prompt": [{"type": "text", "text": "read the file"}]}
        }))
        .await;

        let read = acp.next().await;
        assert_eq!(read["method"], "fs/read_text_file");
        acp.send(json!({
            "jsonrpc": "2.0",
            "id": read["id"],
            "result": {"content": content}
        }))
        .await;
        assert_eq!(acp.next().await["method"], "session/update");
        assert_eq!(acp.next().await["result"]["stopReason"], "end_turn");
        let events = acp.finish().await;
        let tool = &event(&events, "tool:read_file")["value"];
        assert!(tool["id"].as_str().unwrap().starts_with("tool-"));
        assert_eq!(tool["result"]["success"], false);
        assert_eq!(
            tool["result"]["contentItems"][0]["text"],
            "Codex dynamic-tool response exceeds the size limit"
        );
        assert!(serde_json::to_vec(tool).unwrap().len() < 1024 * 1024);
    }
}

#[tokio::test]
async fn codex_splits_large_message_deltas_into_bounded_acp_updates() {
    for (mode, expected) in [
        ("large-ascii-delta", "x".repeat(960 * 1024 + 1)),
        ("large-escaped-delta", "\\".repeat(500_000)),
    ] {
        let workspace = tempfile::tempdir().unwrap();
        let mut acp = Harness::start(mode);
        acp.initialize().await;
        let session = acp.create_session(workspace.path()).await;
        acp.send(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/prompt",
            "params": {"sessionId": session, "prompt": [{"type": "text", "text": "return a large response"}]}
        }))
        .await;

        let mut output = String::new();
        loop {
            let message = acp.next().await;
            assert!(serde_json::to_vec(&message).unwrap().len() < 1024 * 1024);
            if message["id"] == 3 {
                assert_eq!(message["result"]["stopReason"], "end_turn");
                break;
            }
            assert_eq!(message["method"], "session/update");
            let chunk = message["params"]["update"]["content"]["text"]
                .as_str()
                .unwrap();
            assert!(chunk.len() <= 128 * 1024);
            output.push_str(chunk);
        }
        assert_eq!(output, expected);
        acp.finish().await;
    }
}

#[tokio::test]
async fn codex_cancellation_interrupts_the_active_turn() {
    let workspace = tempfile::tempdir().unwrap();
    let mut acp = Harness::start("cancel");
    acp.initialize().await;
    let session = acp.create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "wait for cancellation"}]}
    }))
    .await;
    assert_eq!(acp.next().await["method"], "session/update");
    acp.send(json!({
        "jsonrpc": "2.0",
        "method": "session/cancel",
        "params": {"sessionId": session}
    }))
    .await;
    assert_eq!(acp.next().await["result"]["stopReason"], "cancelled");
    let events = acp.finish().await;
    let interrupt = &event(&events, "interrupt")["value"];
    assert_eq!(interrupt["threadId"], "thread-red-codex");
    assert_eq!(interrupt["turnId"], "turn-red-codex");
}

#[tokio::test]
async fn codex_ignores_cancelled_and_stale_turn_notifications() {
    let workspace = tempfile::tempdir().unwrap();
    let mut acp = Harness::start("stale-turns");
    acp.initialize().await;
    let session = acp.create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "wait for cancellation"}]}
    }))
    .await;
    let update = acp.next().await;
    assert_eq!(update["method"], "session/update");
    assert_eq!(update["params"]["update"]["content"]["text"], "working");

    acp.send(json!({
        "jsonrpc": "2.0",
        "method": "session/cancel",
        "params": {"sessionId": session}
    }))
    .await;
    let cancelled = acp.next().await;
    assert_eq!(cancelled["id"], 3);
    assert_eq!(cancelled["result"]["stopReason"], "cancelled");

    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "start a fresh turn"}]}
    }))
    .await;
    let update = acp.next().await;
    assert_eq!(update["method"], "session/update");
    assert_eq!(
        update["params"]["update"]["content"]["text"],
        "fresh output"
    );
    let completed = acp.next().await;
    assert_eq!(completed["id"], 4);
    assert_eq!(completed["result"]["stopReason"], "end_turn");
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            if acp
                .available_events()
                .iter()
                .filter(|event| {
                    event["event"] == "tool:read_file" || event["event"] == "tool:write_file"
                })
                .count()
                == 6
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Codex adapter did not reject the stale filesystem calls");
    acp.send(json!({"jsonrpc": "2.0", "id": 5, "method": "authenticate", "params": {}}))
        .await;
    let authenticated = acp.next().await;
    assert_eq!(authenticated["id"], 5);
    assert_eq!(authenticated["result"], json!({}));

    let events = acp.finish().await;
    let tools: Vec<_> = events
        .iter()
        .filter(|event| event["event"] == "tool:read_file" || event["event"] == "tool:write_file")
        .collect();
    assert_eq!(tools.len(), 6);
    assert!(tools
        .iter()
        .all(|event| event["value"]["result"]["success"] == false));
}

#[tokio::test]
async fn closing_a_codex_session_frees_capacity_and_rejects_the_old_session() {
    let workspace = tempfile::tempdir().unwrap();
    let mut acp = Harness::start("proposal");
    acp.initialize().await;
    let first = acp.create_session(workspace.path()).await;

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
        "Codex session capacity reached"
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
    assert_eq!(
        acp.next().await["error"]["message"],
        "Codex session was not found"
    );
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 69,
        "method": "session/new",
        "params": {"cwd": workspace.path()}
    }))
    .await;
    assert!(acp.next().await["result"]["sessionId"].is_string());
    let events = acp.finish().await;
    assert_eq!(
        events
            .iter()
            .filter(|event| event["event"] == "thread")
            .count(),
        65
    );
    assert_eq!(
        event(&events, "unsubscribe")["value"]["threadId"],
        "thread-red-codex"
    );
    assert_eq!(
        event(&events, "archive")["value"]["threadId"],
        "thread-red-codex"
    );
}

#[tokio::test]
async fn closing_a_codex_session_cancels_the_active_turn_once() {
    let workspace = tempfile::tempdir().unwrap();
    let mut acp = Harness::start("cancel");
    acp.initialize().await;
    let session = acp.create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "wait for close"}]}
    }))
    .await;
    assert_eq!(acp.next().await["method"], "session/update");

    acp.send(json!({
        "jsonrpc": "2.0",
        "method": "session/cancel",
        "params": {"sessionId": session}
    }))
    .await;
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
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            if acp
                .available_events()
                .iter()
                .any(|event| event["event"] == "interrupt")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Codex adapter did not interrupt the closed turn");
    let events = acp.finish().await;
    assert_eq!(
        events
            .iter()
            .filter(|event| event["event"] == "interrupt")
            .count(),
        1
    );
}

#[tokio::test]
async fn closing_a_codex_session_interrupts_a_late_turn_start() {
    let workspace = tempfile::tempdir().unwrap();
    let mut acp = Harness::start("delayed-start");
    acp.initialize().await;
    let session = acp.create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "wait for close"}]}
    }))
    .await;
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            if acp
                .available_events()
                .iter()
                .any(|event| event["event"] == "turn")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Codex adapter did not send the delayed turn request");

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

    acp.send(json!({"jsonrpc": "2.0", "id": 5, "method": "authenticate", "params": {}}))
        .await;
    assert_eq!(acp.next().await["result"], json!({}));
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            if acp
                .available_events()
                .iter()
                .any(|event| event["event"] == "interrupt")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Codex adapter did not interrupt the late turn");

    let events = acp.finish().await;
    let interrupt = &event(&events, "interrupt")["value"];
    assert_eq!(interrupt["threadId"], "thread-red-codex");
    assert_eq!(interrupt["turnId"], "turn-red-codex");
}

#[tokio::test]
async fn closing_a_codex_session_cancels_when_request_capacity_is_full() {
    let workspace = tempfile::tempdir().unwrap();
    let mut acp = Harness::start("saturated-cancel");
    acp.initialize().await;
    let session = acp.create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "wait for close"}]}
    }))
    .await;
    assert_eq!(acp.next().await["method"], "session/update");

    for id in 10..74 {
        acp.send(json!({"jsonrpc": "2.0", "id": id, "method": "authenticate", "params": {}}))
            .await;
    }
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if acp
                .available_events()
                .iter()
                .filter(|event| event["event"] == "account")
                .count()
                == 65
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Codex adapter did not fill its pending-request capacity");

    acp.send(json!({
        "jsonrpc": "2.0",
        "method": "session/cancel",
        "params": {"sessionId": session}
    }))
    .await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 74,
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
        .any(|response| response["id"] == 74 && response["result"] == json!({})));

    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            if acp
                .available_events()
                .iter()
                .any(|event| event["event"] == "interrupt")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Codex adapter did not interrupt at request capacity");
    let events = acp.finish().await;
    assert_eq!(
        events
            .iter()
            .filter(|event| event["event"] == "interrupt")
            .count(),
        1
    );
}

#[tokio::test]
async fn codex_counts_pending_session_starts_toward_the_session_limit() {
    let workspace = tempfile::tempdir().unwrap();
    let mut acp = Harness::start("hold-account");
    acp.initialize().await;

    for id in 2..=66 {
        acp.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/new",
            "params": {"cwd": workspace.path()}
        }))
        .await;
    }

    let response = acp.next().await;
    assert_eq!(response["id"], 66);
    assert_eq!(
        response["error"]["message"],
        "Codex session capacity reached"
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let events = std::fs::read(&acp.record)
                .ok()
                .and_then(|contents| serde_json::from_slice::<Vec<Value>>(&contents).ok())
                .unwrap_or_default();
            if events
                .iter()
                .filter(|event| event["event"] == "account")
                .count()
                == 64
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Codex adapter did not send the reserved account requests");
    acp.finish().await;
}

#[tokio::test]
async fn closing_a_codex_session_releases_a_pending_filesystem_callback() {
    let workspace = tempfile::tempdir().unwrap();
    let mut acp = Harness::start("callback-cancel");
    acp.initialize().await;
    let session = acp.create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "read a file"}]}
    }))
    .await;
    assert_eq!(acp.next().await["method"], "fs/read_text_file");

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

    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            let events = std::fs::read(&acp.record)
                .ok()
                .and_then(|contents| serde_json::from_slice::<Vec<Value>>(&contents).ok())
                .unwrap_or_default();
            if events.iter().any(|event| event["event"] == "interrupt") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Codex adapter did not cancel the callback turn");
    let events = acp.finish().await;
    assert_eq!(
        event(&events, "tool:read_file")["value"]["result"]["success"],
        false
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["event"] == "interrupt")
            .count(),
        1
    );
}

#[tokio::test]
async fn codex_authentication_failure_is_actionable_and_does_not_start_a_thread() {
    let workspace = tempfile::tempdir().unwrap();
    let mut acp = Harness::start("unauthenticated");
    acp.initialize().await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "session/new",
        "params": {"cwd": workspace.path()}
    }))
    .await;
    let response = acp.next().await;
    assert_eq!(response["error"]["code"], -32_001);
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("codex login"));
    let events = acp.finish().await;
    assert!(events.iter().all(|entry| entry["event"] != "thread"));
}

#[tokio::test]
async fn codex_refuses_to_start_when_mcp_configuration_cannot_be_inspected() {
    for mode in [
        "config-error",
        "config-invalid",
        "config-origins-invalid",
        "config-origin-invalid",
        "config-origin-unknown",
        "managed-enabled-file",
        "managed-enabled-mdm",
        "managed-feature-apps",
        "managed-feature-connectors",
        "managed-feature-plugins",
        "managed-feature-skill",
        "managed-feature-hooks",
        "managed-feature-codex-hooks",
        "managed-feature-orchestrator",
        "managed-notify",
        "managed-notify-origin-invalid",
        "managed-notify-origin-missing",
        "external-endpoint-cloud",
        "external-lockfile-system-load",
        "external-lockfile-cloud-export",
        "requirements-error",
        "requirements-invalid",
        "requirements-apps",
        "requirements-connectors",
        "requirements-plugins",
        "requirements-skill",
        "requirements-hooks",
        "requirements-codex-hooks",
    ] {
        let workspace = tempfile::tempdir().unwrap();
        let mut acp = Harness::start(mode);
        acp.initialize().await;
        acp.send(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": {"cwd": workspace.path()}
        }))
        .await;
        let response = acp.next().await;
        assert_eq!(response["id"], 2);
        assert_eq!(response["error"]["code"], -32_000);
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("configured MCP tools"));
        let events = acp.finish().await;
        assert!(events.iter().any(|entry| entry["event"] == "config"));
        assert!(events.iter().all(|entry| entry["event"] != "thread"));
    }
}

#[tokio::test]
async fn codex_starts_when_a_managed_mcp_server_is_already_disabled() {
    let workspace = tempfile::tempdir().unwrap();
    let mut acp = Harness::start("managed-disabled");
    acp.initialize().await;

    let session = acp.create_session(workspace.path()).await;

    assert!(!session.is_empty());
    let events = acp.finish().await;
    let thread = &event(&events, "thread")["value"];
    assert_eq!(
        thread["config"]["mcp_servers"]["git.tools"]["enabled"],
        false
    );
}

#[tokio::test]
async fn codex_strips_the_remote_thread_config_endpoint_before_configuration_is_read() {
    let workspace = tempfile::tempdir().unwrap();
    let mut acp = Harness::start("remote-thread-config");
    acp.initialize().await;

    let session = acp.create_session(workspace.path()).await;

    assert!(!session.is_empty());
    let events = acp.finish().await;
    assert_eq!(
        event(&events, "remote-config")["value"]["snapshotHasEndpoint"],
        false
    );
    assert!(events.iter().any(|entry| entry["event"] == "thread"));
}

#[tokio::test]
async fn codex_starts_with_a_relative_executable_after_isolating_the_bootstrap_cwd() {
    let workspace = tempfile::tempdir().unwrap();
    let mut acp = Harness::start("relative-codex");
    acp.initialize().await;

    let session = acp.create_session(workspace.path()).await;

    assert!(!session.is_empty());
    let events = acp.finish().await;
    assert!(events.iter().any(|entry| entry["event"] == "thread"));
}

#[tokio::test]
async fn codex_honors_ephemeral_authentication_and_ignores_stale_file_credentials() {
    let workspace = tempfile::tempdir().unwrap();
    let mut acp = Harness::start("ephemeral-auth");
    acp.initialize().await;

    let session = acp.create_session(workspace.path()).await;

    assert!(!session.is_empty());
    let events = acp.finish().await;
    let args = event(&events, "launch-args")["value"].as_array().unwrap();
    assert!(args
        .iter()
        .any(|value| value == "cli_auth_credentials_store=\"ephemeral\""));
    assert!(!args
        .iter()
        .any(|value| value == "cli_auth_credentials_store=\"file\""));
    assert_eq!(
        event(&events, "launch-auth")["value"],
        json!({"isolatedAuthExists": false, "sourceAuthExists": true})
    );
}

#[tokio::test]
async fn codex_accepts_a_large_but_bounded_configuration_response() {
    let workspace = tempfile::tempdir().unwrap();
    let mut acp = Harness::start("config-large-escaped");
    acp.initialize().await;

    let session = acp.create_session(workspace.path()).await;

    assert!(!session.is_empty());
    let events = acp.finish().await;
    assert!(events.iter().any(|entry| entry["event"] == "config"));
    assert!(events.iter().any(|entry| entry["event"] == "thread"));
    let config = &event(&events, "large-config")["value"];
    assert!(config["snapshotBytes"].as_u64().unwrap() < 2 * 1024 * 1024);
    assert!(config["responseBytes"].as_u64().unwrap() > 1024 * 1024);
}

#[tokio::test]
async fn codex_accepts_many_mcp_servers_in_the_restricted_configuration() {
    let workspace = tempfile::tempdir().unwrap();
    let mut acp = Harness::start("config-many-servers");
    acp.initialize().await;

    let session = acp.create_session(workspace.path()).await;

    assert!(!session.is_empty());
    let events = acp.finish().await;
    let thread = &event(&events, "thread")["value"];
    assert_eq!(
        thread["config"]["mcp_servers"].as_object().unwrap().len(),
        16_001
    );
    assert!(serde_json::to_vec(thread).unwrap().len() > 1024 * 1024);
}

#[tokio::test]
async fn codex_ignores_mcp_servers_added_after_configuration_inspection() {
    let workspace = tempfile::tempdir().unwrap();
    let nested = workspace.path().join("nested").join("project");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::create_dir(workspace.path().join(".codex")).unwrap();
    std::fs::write(
        workspace.path().join(".codex").join("config.toml"),
        "[mcp_servers.project]\ncommand = \"must-not-launch\"\nenabled = true\n",
    )
    .unwrap();
    let mut acp = Harness::start("config-race");
    acp.initialize().await;

    let session = acp.create_session(&nested).await;

    assert!(!session.is_empty());
    let events = acp.finish().await;
    let isolation = &event(&events, "isolation")["value"];
    assert_eq!(isolation["sameHome"], false);
    assert_eq!(isolation["snapshotHasRacedServer"], false);
    assert_eq!(isolation["snapshotHasProjects"], false);
    assert_eq!(isolation["snapshotHasSqliteHome"], false);
    assert_eq!(isolation["snapshotHasLogDir"], false);
    assert_eq!(isolation["snapshotHasConfigLockfile"], false);
    assert_eq!(isolation["sqliteHomeIsolated"], true);
    assert_eq!(isolation["projectTrust"], "untrusted");
    assert_eq!(isolation["trustedAncestors"], json!([]));
}

#[tokio::test]
async fn codex_failed_turn_returns_a_content_free_error() {
    let workspace = tempfile::tempdir().unwrap();
    let mut acp = Harness::start("failed");
    acp.initialize().await;
    let session = acp.create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "trigger a failure"}]}
    }))
    .await;
    let response = acp.next().await;
    assert_eq!(response["error"]["code"], -32_000);
    assert_eq!(response["error"]["message"], "Codex turn failed");
    assert!(!response.to_string().contains("secret backend details"));
    acp.finish().await;
}

#[tokio::test]
async fn codex_app_server_close_completes_the_pending_prompt_without_hanging() {
    let workspace = tempfile::tempdir().unwrap();
    let mut acp = Harness::start("close");
    acp.initialize().await;
    let session = acp.create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "wait for app-server close"}]}
    }))
    .await;
    let response = acp.next().await;
    assert_eq!(response["error"]["message"], "Codex app-server stopped");
    acp.finish().await;
}

#[tokio::test]
async fn invalid_codex_app_server_output_completes_the_pending_prompt_without_hanging() {
    let workspace = tempfile::tempdir().unwrap();
    let mut acp = Harness::start("invalid");
    acp.initialize().await;
    let session = acp.create_session(workspace.path()).await;
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "wait for invalid app-server data"}]}
    }))
    .await;
    let response = acp.next().await;
    assert_eq!(
        response["error"]["message"],
        "Codex app-server returned invalid data"
    );
    acp.finish().await;
}

#[tokio::test]
async fn codex_incompatible_app_server_fails_closed_before_acp_handshake() {
    let acp = Harness::start("incompatible");
    let output = tokio::time::timeout(TEST_TIMEOUT, acp.child.wait_with_output())
        .await
        .expect("ACP process did not stop after incompatible handshake")
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("required experimental API"));
    assert!(!stderr.contains("experimental API is unavailable"));
}
