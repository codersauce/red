use std::{
    env, fs,
    io::{BufRead, Write},
    time::Duration,
};

use anyhow::{Context as _, Result};
use red::plugin::rpc::{RpcConnection, RpcIncoming, RpcInitializeParams};
use red::plugin::{PluginRegistry, PluginStatus, Runtime};
use serde_json::{json, Value};
use tokio::process::Command;

const FIXTURE_MODE: &str = "RED_RPC_FIXTURE_MODE";

fn main() -> Result<()> {
    if env::args().any(|argument| argument == "--registry-fixture") {
        return run_registry_fixture();
    }
    if let Ok(mode) = env::var(FIXTURE_MODE) {
        return run_fixture(&mode);
    }

    tokio::runtime::Runtime::new()?.block_on(async {
        exercises_bidirectional_requests_and_shutdown().await?;
        deadline_failure_reaps_the_plugin_and_fails_its_peers().await?;
        registry_discovers_and_lazily_executes_an_rpc_command().await
    })
}

async fn exercises_bidirectional_requests_and_shutdown() -> Result<()> {
    let (connection, mut incoming) = spawn_fixture("normal")?;
    let initialized = connection
        .initialize(initialize_params(), Duration::from_secs(2))
        .await?;
    assert_eq!(initialized["protocol_version"], 1);

    let host = tokio::spawn(async move {
        let Some(RpcIncoming::Request(request)) = incoming.recv().await else {
            anyhow::bail!("fixture did not call the host");
        };
        assert_eq!(request.method, "red/Test");
        let result = json!(request.params["value"].as_u64().unwrap() + 1);
        request.respond(Ok(result)).await
    });
    let nested = connection
        .request("test/nested", json!({"value": 2}), Duration::from_secs(2))
        .await?;
    assert_eq!(nested, json!({"callback": 3}));
    host.await??;

    let (first, second) = tokio::join!(
        connection.request("test/first", Value::Null, Duration::from_secs(2)),
        connection.request("test/second", Value::Null, Duration::from_secs(2)),
    );
    assert_eq!(first?["method"], "test/first");
    assert_eq!(second?["method"], "test/second");

    let shutdown = connection
        .shutdown(Duration::from_secs(2), Duration::from_secs(2))
        .await?;
    assert!(!shutdown.forced);
    assert!(shutdown.diagnostics.contains("fixture exited"));
    Ok(())
}

async fn deadline_failure_reaps_the_plugin_and_fails_its_peers() -> Result<()> {
    let (connection, _incoming) = spawn_fixture("hang")?;
    connection
        .initialize(initialize_params(), Duration::from_secs(2))
        .await?;

    let (timed_out, peer) = tokio::join!(
        connection.request("test/timeout", Value::Null, Duration::from_millis(100)),
        connection.request("test/peer", Value::Null, Duration::from_secs(10)),
    );
    let timed_out = timed_out.unwrap_err().to_string();
    let peer = peer.unwrap_err().to_string();
    assert!(timed_out.contains("timed out after 100 ms"));
    assert!(peer.contains("timed out after 100 ms"));
    assert!(timed_out.contains("fixture stalled"));
    assert!(peer.contains("fixture stalled"));
    Ok(())
}

async fn registry_discovers_and_lazily_executes_an_rpc_command() -> Result<()> {
    let package = tempfile::tempdir()?;
    let marker = package.path().join("executed.json");
    let initialized_marker = package.path().join("initialized");
    let executable = env::current_exe()?;
    let executable_name = executable
        .file_name()
        .context("test executable has no file name")?
        .to_string_lossy();
    let executable_dir = executable
        .parent()
        .context("test executable has no parent")?;
    let path = env::join_paths(
        std::iter::once(executable_dir.to_path_buf())
            .chain(env::split_paths(&env::var_os("PATH").unwrap_or_default())),
    )?;
    env::set_var("PATH", path);
    env::set_var("RED_RPC_REGISTRY_MARKER", &marker);
    fs::write(
        package.path().join("red-plugin.toml"),
        format!(
            r#"schema_version = 1

[plugin]
id = "rpc-test"
name = "RPC Test"
version = "0.1.0"
red_api = "^0.19.0"

[rpc]
command = "{executable_name}"
args = ["--registry-fixture"]

[activation]
commands = ["RpcFail", "RpcHello"]
"#
        ),
    )?;
    let manifest = package.path().join("red-plugin.toml");
    let mut registry = PluginRegistry::new();
    registry.add(
        "rpc-alias",
        manifest.to_str().context("non-UTF-8 manifest path")?,
    );
    let mut runtime = Runtime::new();

    registry.initialize(&mut runtime).await?;
    registry.reload(&mut runtime).await?;
    assert!(matches!(
        registry.statuses().get("rpc-alias"),
        Some(PluginStatus::Pending)
    ));
    registry.execute(&mut runtime, "RpcHello one two").await?;
    assert!(matches!(
        registry.statuses().get("rpc-alias"),
        Some(PluginStatus::Active)
    ));
    let invocation: Value = serde_json::from_slice(&fs::read(&marker)?)?;
    assert_eq!(invocation["name"], "RpcHello");
    assert_eq!(invocation["args"], json!(["one", "two"]));
    assert_eq!(invocation["raw_args"], "one two");
    assert!(initialized_marker.is_file());
    registry.execute(&mut runtime, "RpcFail").await?;
    assert!(matches!(
        registry.statuses().get("rpc-alias"),
        Some(PluginStatus::Active)
    ));
    registry.deactivate_all(&mut runtime).await?;

    fs::remove_file(&marker)?;
    fs::remove_file(&initialized_marker)?;
    let source = fs::read_to_string(&manifest)?;
    fs::write(&manifest, source.replace("RpcHello", "RpcMismatch"))?;
    let mut registry = PluginRegistry::new();
    registry.add(
        "rpc-alias",
        manifest.to_str().context("non-UTF-8 manifest path")?,
    );
    registry.initialize(&mut runtime).await?;
    registry.execute(&mut runtime, "RpcMismatch").await?;
    assert!(matches!(
        registry.statuses().get("rpc-alias"),
        Some(PluginStatus::Quarantined { diagnostic, .. })
            if diagnostic.contains("commands do not match")
    ));
    assert!(!marker.exists());
    assert!(!initialized_marker.exists());
    Ok(())
}

fn spawn_fixture(mode: &str) -> Result<(RpcConnection, tokio::sync::mpsc::Receiver<RpcIncoming>)> {
    let mut command = Command::new(env::current_exe()?);
    command.env(FIXTURE_MODE, mode);
    RpcConnection::spawn(command)
}

fn initialize_params() -> RpcInitializeParams {
    RpcInitializeParams {
        protocol_versions: vec![1],
        red_api_version: "0.19.0".to_string(),
        plugin_id: "test-plugin".to_string(),
        host_methods: vec!["red/Test".to_string()],
    }
}

fn run_fixture(mode: &str) -> Result<()> {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut output = stdout.lock();

    let initialize = read_message(&mut input)?;
    assert_eq!(initialize["method"], "initialize");
    write_message(
        &mut output,
        json!({
            "jsonrpc": "2.0",
            "id": initialize["id"],
            "result": {"protocol_version": 1},
        }),
    )?;
    assert_eq!(read_message(&mut input)?["method"], "initialized");

    if mode == "hang" {
        eprintln!("fixture stalled");
        std::thread::sleep(Duration::from_secs(60));
        return Ok(());
    }
    anyhow::ensure!(mode == "normal", "unknown fixture mode `{mode}`");

    let nested = read_message(&mut input)?;
    assert_eq!(nested["method"], "test/nested");
    write_message(
        &mut output,
        json!({
            "jsonrpc": "2.0",
            "id": "plugin-1",
            "method": "red/Test",
            "params": nested["params"],
        }),
    )?;
    let callback = read_message(&mut input)?;
    assert_eq!(callback["id"], "plugin-1");
    write_message(
        &mut output,
        json!({
            "jsonrpc": "2.0",
            "id": nested["id"],
            "result": {"callback": callback["result"]},
        }),
    )?;

    let concurrent = [read_message(&mut input)?, read_message(&mut input)?];
    for request in concurrent.into_iter().rev() {
        write_message(
            &mut output,
            json!({
                "jsonrpc": "2.0",
                "id": request["id"],
                "result": {"method": request["method"]},
            }),
        )?;
    }

    let shutdown = read_message(&mut input)?;
    assert_eq!(shutdown["method"], "shutdown");
    write_message(
        &mut output,
        json!({"jsonrpc": "2.0", "id": shutdown["id"], "result": null}),
    )?;
    assert_eq!(read_message(&mut input)?["method"], "exit");
    eprintln!("fixture exited");
    Ok(())
}

fn run_registry_fixture() -> Result<()> {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut output = stdout.lock();

    let initialize = read_message(&mut input)?;
    assert_eq!(initialize["params"]["plugin_id"], "rpc-test");
    assert_eq!(
        initialize["params"]["host_methods"],
        json!(["red/DocumentSnapshot"])
    );
    write_message(
        &mut output,
        json!({
            "jsonrpc": "2.0",
            "id": initialize["id"],
            "result": {
                "protocol_version": 1,
                "commands": [
                    {"name": "RpcFail"},
                    {
                        "name": "RpcHello",
                        "metadata": {"title": "RPC hello", "arguments": true}
                    }
                ]
            },
        }),
    )?;
    assert_eq!(read_message(&mut input)?["method"], "initialized");
    let marker = env::var_os("RED_RPC_REGISTRY_MARKER").context("missing marker path")?;
    let marker = std::path::PathBuf::from(marker);
    fs::write(marker.with_file_name("initialized"), [])?;
    let execute = read_message(&mut input)?;
    assert_eq!(execute["method"], "command/execute");
    fs::write(marker, serde_json::to_vec(&execute["params"])?)?;
    write_message(
        &mut output,
        json!({
            "jsonrpc": "2.0",
            "id": "plugin-document",
            "method": "red/DocumentSnapshot",
            "params": {},
        }),
    )?;
    let host_response = read_message(&mut input)?;
    assert_eq!(host_response["id"], "plugin-document");
    assert_eq!(host_response["error"]["code"], -32601);
    write_message(
        &mut output,
        json!({
            "jsonrpc": "2.0",
            "id": execute["id"],
            "result": {"message": "hello from RPC"},
        }),
    )?;
    let execute = read_message(&mut input)?;
    assert_eq!(execute["method"], "command/execute");
    assert_eq!(execute["params"]["name"], "RpcFail");
    write_message(
        &mut output,
        json!({
            "jsonrpc": "2.0",
            "id": execute["id"],
            "error": {"code": -32001, "message": "fixture failure"},
        }),
    )?;
    let shutdown = read_message(&mut input)?;
    assert_eq!(shutdown["method"], "shutdown");
    write_message(
        &mut output,
        json!({"jsonrpc": "2.0", "id": shutdown["id"], "result": null}),
    )?;
    assert_eq!(read_message(&mut input)?["method"], "exit");
    Ok(())
}

fn read_message(reader: &mut impl BufRead) -> Result<Value> {
    let mut line = String::new();
    anyhow::ensure!(reader.read_line(&mut line)? != 0, "host closed stdin");
    serde_json::from_str(&line).context("host wrote invalid JSON")
}

fn write_message(writer: &mut impl Write, value: Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, &value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}
