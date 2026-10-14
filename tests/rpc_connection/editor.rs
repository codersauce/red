use super::*;
use red::{
    buffer::Buffer,
    config::{Config, KeyAction},
    editor::{Action, DetachedEditorCore, Editor},
    headless::{InputEvent, KeyCode, KeyKind},
    theme::Theme,
};
use std::path::{Path, PathBuf};

struct Harness {
    core: DetachedEditorCore,
    package: tempfile::TempDir,
}

impl Harness {
    async fn new(mapping: KeyAction) -> Result<Self> {
        let package = tempfile::tempdir()?;
        let slow = package.path().join("slow");
        let fast = package.path().join("fast");
        write_package(
            &slow,
            &["RpcWait", "RpcEdit", "RpcUndo", "RpcLegacy", "RpcPath"],
        )?;
        write_package(&fast, &["RpcRead"])?;
        let mut config = Config::from_user_toml_with_overrides("disable_ai = true", &[])?;
        config.plugins.clear();
        config.lsp.enabled = false;
        config.plugins.insert(
            "slow".into(),
            slow.join("red-plugin.toml").display().to_string(),
        );
        config.plugins.insert(
            "fast".into(),
            fast.join("red-plugin.toml").display().to_string(),
        );
        config.keys.normal.insert("F2".into(), mapping.clone());
        config.keys.normal.insert("Q".into(), mapping);
        config.keys.insert.insert(
            "F3".into(),
            KeyAction::Single(Action::PluginCommand("RpcUndo".into())),
        );
        let editor = Editor::test_with_size(
            Box::new(common::MockLsp),
            80,
            24,
            config,
            Theme::default(),
            vec![
                Buffer::new(None, "hello".into()),
                Buffer::new(None, "other".into()),
            ],
        )?;
        Ok(Self {
            core: DetachedEditorCore::new(editor).await?,
            package,
        })
    }

    fn marker(&self, name: &str, index: usize) -> PathBuf {
        self.package
            .path()
            .join("slow")
            .join(format!("{name}-{index}"))
    }

    fn screen(&self) -> String {
        self.core
            .snapshot(None)
            .lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    async fn key(&mut self, code: KeyCode) -> Result<()> {
        tokio::time::timeout(
            Duration::from_secs(2),
            self.core.input(InputEvent::Key {
                code,
                modifiers: vec![],
                key_kind: KeyKind::Press,
            }),
        )
        .await
        .context("editor input blocked on RPC")??;
        Ok(())
    }

    async fn text(&mut self, text: &str) -> Result<()> {
        for character in text.chars() {
            self.key(KeyCode::Character(character)).await?;
        }
        Ok(())
    }

    async fn command(&mut self, command: &str) -> Result<()> {
        self.key(KeyCode::Escape).await?;
        self.text(&format!(":{command}")).await?;
        self.key(KeyCode::Enter).await
    }

    async fn until(&mut self, predicate: impl Fn(&Self) -> bool) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                self.core.tick().await?;
                if predicate(self) {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .with_context(|| format!("RPC editor fixture did not progress:\n{}", self.screen()))?
    }

    fn release(&self, index: usize) -> Result<()> {
        fs::write(self.marker("release", index), b"")?;
        Ok(())
    }
}

fn write_package(root: &Path, commands: &[&str]) -> Result<()> {
    fs::create_dir_all(root)?;
    let executable = env::current_exe()?;
    let executable = executable
        .file_name()
        .context("missing executable name")?
        .to_string_lossy();
    let args = std::iter::once("--editor-fixture")
        .chain(commands.iter().copied())
        .collect::<Vec<_>>();
    fs::write(root.join("red-plugin.toml"), format!(
        "schema_version = 1\n[plugin]\nid = \"editor-fixture\"\nname = \"Editor Fixture\"\nversion = \"0.1.0\"\nred_api = \"^0.20.0\"\n[rpc]\ncommand = {executable:?}\nargs = {}\n[activation]\ncommands = {}\n",
        serde_json::to_string(&args)?, serde_json::to_string(commands)?,
    ))?;
    Ok(())
}

pub(super) async fn run() -> Result<()> {
    responsive_commands_and_undo()
        .await
        .context("responsive commands and undo")?;
    legacy_mutation_rejects_changed_document()
        .await
        .context("legacy target guard")?;
    ordered_action_tails_and_user_interruption()
        .await
        .context("ordered action tails")?;
    macro_tail_waits_for_completion()
        .await
        .context("macro continuation")?;
    nested_macro_keeps_its_recursion_limit()
        .await
        .context("nested macro budget")?;
    nested_macro_events_survive_a_second_wait()
        .await
        .context("nested macro queue")?;
    dot_replay_waits_for_its_action_tail()
        .await
        .context("dot continuation")?;
    replacement_discards_the_retired_command_tail()
        .await
        .context("retired command completion")?;
    explicit_path_edit_preserves_the_active_view_and_insert()
        .await
        .context("off-screen document edit")?;
    Ok(())
}

async fn responsive_commands_and_undo() -> Result<()> {
    let mut h = Harness::new(KeyAction::Single(Action::PluginCommand("RpcWait".into()))).await?;
    h.key(KeyCode::Function(2)).await?;
    h.until(|h| h.marker("started", 1).exists()).await?;
    h.key(KeyCode::Function(2)).await?;
    h.until(|h| h.screen().contains("is busy")).await?;
    assert!(!h.marker("started", 2).exists());
    h.text("iuser").await?;
    assert!(h.screen().contains("userhello"));
    h.core.resize(90, 30).await?;
    h.command("RpcRead").await?;
    h.until(|h| h.screen().contains("RpcRead done")).await?;
    h.release(1)?;
    h.until(|h| h.screen().contains("RpcWait done")).await?;

    h.command("RpcEdit").await?;
    h.until(|h| h.marker("started", 2).exists()).await?;
    h.text("iX").await?;
    h.release(2)?;
    h.until(|h| h.marker("done", 2).exists() && !h.core.test_rpc_command_pending("slow"))
        .await?;
    let plugin_text = h.screen().lines().next().unwrap().to_string();
    assert!(plugin_text.contains("plugin-"));
    h.text("Y").await?;
    let user_text = h.screen().lines().next().unwrap().to_string();
    h.key(KeyCode::Function(3)).await?;
    h.until(|h| h.marker("done", 3).exists() && !h.core.test_rpc_command_pending("slow"))
        .await?;
    let rejected: Value = serde_json::from_slice(&fs::read(h.marker("done", 3))?)?;
    assert!(rejected["error"]["message"]
        .as_str()
        .unwrap()
        .contains("no longer the document's latest change"));
    assert_eq!(h.screen().lines().next().unwrap(), user_text);
    h.key(KeyCode::Escape).await?;
    h.text("u").await?;
    assert_eq!(h.screen().lines().next().unwrap(), plugin_text);
    h.text("u").await?;
    assert!(!h.screen().contains("plugin-"));
    assert!(h.screen().contains('X'));
    h.text("u").await?;
    assert!(h.screen().contains("userhello"));
    Ok(())
}

async fn legacy_mutation_rejects_changed_document() -> Result<()> {
    let mut h = Harness::new(KeyAction::Single(Action::PluginCommand("RpcLegacy".into()))).await?;
    h.key(KeyCode::Function(2)).await?;
    h.until(|h| h.marker("started", 1).exists()).await?;
    h.command("bnext").await?;
    h.release(1)?;
    h.until(|h| h.screen().contains("current document changed"))
        .await?;
    assert!(h.screen().contains("other"));
    assert!(!h.screen().contains("plugin-"));
    Ok(())
}

async fn ordered_action_tails_and_user_interruption() -> Result<()> {
    let mut h = Harness::new(KeyAction::Multiple(vec![
        Action::Command("RpcWait".into()),
        Action::PluginCommand("RpcWait".into()),
        Action::InsertString("tail".into()),
    ]))
    .await?;
    h.text("2").await?;
    h.key(KeyCode::Function(2)).await?;
    for index in 1..=4 {
        h.until(|h| h.marker("started", index).exists()).await?;
        assert_eq!(h.screen().matches("tail").count(), usize::from(index > 2));
        h.core.resize(81, 25).await?;
        h.release(index)?;
    }
    h.until(|h| h.screen().contains("tailtailhello")).await?;

    h.key(KeyCode::Function(2)).await?;
    h.until(|h| h.marker("started", 5).exists()).await?;
    h.text("iuser").await?;
    h.release(5)?;
    h.until(|h| h.screen().contains("RpcWait done")).await?;
    assert!(!h.marker("started", 6).exists());
    assert_eq!(h.screen().matches("tail").count(), 2);
    assert!(h.screen().contains("user"));
    Ok(())
}

async fn macro_tail_waits_for_completion() -> Result<()> {
    let mut h = Harness::new(KeyAction::Single(Action::PluginCommand("RpcWait".into()))).await?;
    h.text("qa").await?;
    h.command("RpcWait").await?;
    h.text("iZ").await?;
    h.key(KeyCode::Escape).await?;
    h.text("q").await?;
    h.release(1)?;
    h.until(|h| h.screen().contains("RpcWait done")).await?;
    h.text("u@a").await?;
    h.until(|h| h.marker("started", 2).exists()).await?;
    assert!(!h.screen().contains("Zhello"));
    h.release(2)?;
    h.until(|h| h.screen().contains("Zhello")).await?;
    Ok(())
}

async fn nested_macro_keeps_its_recursion_limit() -> Result<()> {
    let mut h = Harness::new(KeyAction::Multiple(vec![
        Action::PluginCommand("RpcWait".into()),
        Action::PlayMacro('a'),
    ]))
    .await?;
    h.command("register a Q").await?;
    h.text("@a").await?;
    for index in 1..=20 {
        h.until(|h| h.marker("started", index).exists()).await?;
        h.release(index)?;
    }
    h.until(|h| h.screen().contains("macro recursion limit") || h.marker("started", 21).exists())
        .await?;
    assert!(
        h.screen().contains("macro recursion limit"),
        "{}",
        h.screen()
    );
    assert!(!h.marker("started", 21).exists());
    Ok(())
}

async fn replacement_discards_the_retired_command_tail() -> Result<()> {
    let mut h = Harness::new(KeyAction::Multiple(vec![
        Action::PluginCommand("RpcWait".into()),
        Action::InsertString("stale".into()),
    ]))
    .await?;
    h.key(KeyCode::Function(2)).await?;
    h.until(|h| h.marker("started", 1).exists()).await?;
    let process_path = h.package.path().join("slow/process-id");
    let previous = fs::read_to_string(&process_path)?;
    let manifest = h.package.path().join("slow/red-plugin.toml");
    let source = fs::read_to_string(&manifest)?;
    fs::write(
        manifest,
        format!("{source}\n# Reload while a command is pending.\n"),
    )?;
    h.until(|_| fs::read_to_string(&process_path).is_ok_and(|pid| pid != previous))
        .await?;
    assert!(!h.screen().contains("stale"));
    h.command("RpcWait").await?;
    h.release(1)?;
    h.until(|h| h.screen().contains("RpcWait done")).await?;
    assert!(!h.screen().contains("stale"));
    Ok(())
}

async fn nested_macro_events_survive_a_second_wait() -> Result<()> {
    let mut h = Harness::new(KeyAction::Multiple(vec![
        Action::PluginCommand("RpcWait".into()),
        Action::PlayMacro('b'),
        Action::PluginCommand("RpcWait".into()),
    ]))
    .await?;
    h.command("register b iB<Esc>").await?;
    h.command("register a QiA<Esc>").await?;
    h.text("@a").await?;
    for index in 1..=2 {
        h.until(|h| h.marker("started", index).exists()).await?;
        h.release(index)?;
    }
    h.until(|h| h.marker("done", 2).exists() && !h.core.test_rpc_command_pending("slow"))
        .await?;
    assert!(h.screen().contains("ABhello"), "{}", h.screen());
    Ok(())
}

async fn dot_replay_waits_for_its_action_tail() -> Result<()> {
    let mut h = Harness::new(KeyAction::Multiple(vec![
        Action::InsertString("a".into()),
        Action::PluginCommand("RpcWait".into()),
        Action::InsertString("b".into()),
    ]))
    .await?;
    h.text("Q").await?;
    h.until(|h| h.marker("started", 1).exists()).await?;
    assert!(h.screen().contains("ahello"));
    h.release(1)?;
    h.until(|h| h.screen().contains("abhello")).await?;
    h.text(".").await?;
    h.until(|h| h.marker("started", 2).exists()).await?;
    assert!(h.screen().contains("abahello"));
    h.release(2)?;
    h.until(|h| h.screen().contains("ababhello")).await?;
    Ok(())
}

async fn explicit_path_edit_preserves_the_active_view_and_insert() -> Result<()> {
    let mut h = Harness::new(KeyAction::Single(Action::PluginCommand("RpcPath".into()))).await?;
    h.command("bnext").await?;
    h.command("file second.txt").await?;
    h.command("bprevious").await?;
    h.key(KeyCode::Function(2)).await?;
    h.until(|h| h.marker("started", 1).exists()).await?;
    h.text("iU").await?;
    h.release(1)?;
    h.until(|h| h.marker("done", 1).exists() && !h.core.test_rpc_command_pending("slow"))
        .await?;
    assert!(h.screen().contains("Uhello"));
    assert!(!h.screen().contains("plugin-"));
    h.text("V").await?;
    h.key(KeyCode::Escape).await?;
    assert!(h.screen().contains("UVhello"));
    h.text("u").await?;
    assert!(h.screen().contains("hello"));
    assert!(!h.screen().contains("UVhello"));
    h.command("bnext").await?;
    assert!(h.screen().contains("plugin-other"));
    Ok(())
}

pub(super) fn fixture() -> Result<()> {
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    let initialize = read_message(&mut input)?;
    let commands = env::args()
        .skip(2)
        .map(|name| json!({"name": name}))
        .collect::<Vec<_>>();
    write_message(
        &mut output,
        json!({"jsonrpc":"2.0", "id":initialize["id"], "result":{"protocol_version":1,"commands":commands}}),
    )?;
    let _initialized = read_message(&mut input)?;
    fs::write("process-id", std::process::id().to_string())?;
    let mut count = 0;
    loop {
        let request = read_message(&mut input)?;
        match request["method"].as_str() {
            Some("command/execute") => {
                let name = request["params"]["name"].as_str().context("command name")?;
                let result = if name == "RpcRead" {
                    Ok(())
                } else {
                    count += 1;
                    fixture_command(name, count, &mut input, &mut output)
                };
                let response = match result {
                    Ok(()) => {
                        json!({"jsonrpc":"2.0", "id":request["id"], "result":{"message":format!("{name} done")}})
                    }
                    Err(error) => {
                        json!({"jsonrpc":"2.0", "id":request["id"], "error":{"code":-32000,"message":error.to_string()}})
                    }
                };
                write_message(&mut output, response.clone())?;
                fs::write(format!("done-{count}"), serde_json::to_vec(&response)?)?;
            }
            Some("shutdown") => write_message(
                &mut output,
                json!({"jsonrpc":"2.0", "id":request["id"], "result":null}),
            )?,
            Some("exit") => return Ok(()),
            _ => {}
        }
    }
}

fn fixture_command(
    name: &str,
    count: usize,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<()> {
    if name == "RpcUndo" {
        let params = serde_json::from_slice(&fs::read("last-edit")?)?;
        host_request(input, output, "red/DocumentUndo", params)?;
        return Ok(());
    }
    let mut document = Value::Null;
    if name != "RpcEdit" {
        let params = if name == "RpcPath" {
            json!({"path":"second.txt"})
        } else {
            json!({"include_document_id":true})
        };
        document = host_request(input, output, "red/DocumentSnapshot", params)?;
    }
    fs::write(format!("started-{count}"), b"")?;
    while !Path::new(&format!("release-{count}")).exists() {
        std::thread::sleep(Duration::from_millis(1));
    }
    if name == "RpcEdit" {
        document = host_request(
            input,
            output,
            "red/DocumentSnapshot",
            json!({"include_document_id":true}),
        )?;
    }
    if name != "RpcWait" {
        let mut params = json!({"expected_revision":document["revision"], "edits":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},"text":"plugin-"}]});
        if name == "RpcEdit" {
            params["document_id"] = document["document_id"].clone();
        }
        if name == "RpcPath" {
            params["path"] = json!("second.txt");
        }
        let applied = host_request(input, output, "red/DocumentApply", params)?;
        fs::write(
            "last-edit",
            serde_json::to_vec(
                &json!({"document_id":document["document_id"], "transaction_id":applied["transaction_id"]}),
            )?,
        )?;
    }
    Ok(())
}

fn host_request(
    input: &mut impl BufRead,
    output: &mut impl Write,
    method: &str,
    params: Value,
) -> Result<Value> {
    write_message(
        output,
        json!({"jsonrpc":"2.0","id":"host","method":method,"params":params}),
    )?;
    let response = read_message(input)?;
    anyhow::ensure!(response["id"] == "host", "unexpected message: {response}");
    anyhow::ensure!(
        response.get("error").is_none(),
        "{}",
        response["error"]["message"]
    );
    Ok(response["result"].clone())
}
