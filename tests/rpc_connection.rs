use std::{
    env,
    io::{BufRead, Write},
    time::Duration,
};

use anyhow::{Context as _, Result};
use red::plugin::rpc::{RpcConnection, RpcIncoming, RpcInitializeParams};
use serde_json::{json, Value};
use tokio::process::Command;

const FIXTURE_MODE: &str = "RED_RPC_FIXTURE_MODE";

fn main() -> Result<()> {
    if let Ok(mode) = env::var(FIXTURE_MODE) {
        return run_fixture(&mode);
    }

    tokio::runtime::Runtime::new()?.block_on(async {
        exercises_bidirectional_requests_and_shutdown().await?;
        deadline_failure_reaps_the_plugin_and_fails_its_peers().await
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
