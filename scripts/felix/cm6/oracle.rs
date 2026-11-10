//! Helix 25.07.1 integration test that records the final editor state for
//! externally generated key-sequence cases. `run.sh` copies it into a pinned
//! Helix checkout as `helix-term/tests/felix_oracle.rs`.
//!
//! Input (`FELIX_ORACLE_IN`): `[{"id", "in", "keys", "lang"?, "file"?}]`, where
//! `in` uses Helix selection markup (`#[a|]#`, `#(b|)#`) and `file` names the
//! opened document (`example.txt` by default).
//! Output (`FELIX_ORACLE_OUT`): `[{"id", "start", "text", "ranges", "primary",
//! "mode", "register", "out"} | {"id", "error"}]`, where `start` holds the text
//! and selections before the keys.
#![cfg(feature = "integration")]

#[path = "test/helpers.rs"]
#[allow(dead_code)]
mod helpers;

use helix_core::test;
use helix_term::application::Application;
use helix_view::{current, current_ref, document::Mode};
use serde_json::{json, Value};

use self::helpers::*;

fn snapshot(app: &Application) -> Value {
    let (view, doc) = current_ref!(app.editor);
    let mode = match app.editor.mode {
        Mode::Normal => "normal",
        Mode::Insert => "insert",
        Mode::Select => "select",
    };
    let register = app
        .editor
        .registers
        .read('"', &app.editor)
        .map(|values| values.map(|v| v.to_string()).collect::<Vec<_>>());
    let selection = doc.selection(view.id);
    let ranges: Vec<[usize; 2]> = selection
        .ranges()
        .iter()
        .map(|r| [r.anchor, r.head])
        .collect();
    json!({
        "text": doc.text().to_string(),
        "ranges": ranges,
        "primary": selection.primary_index(),
        "out": test::plain(doc.text().slice(..), selection),
        "mode": mode,
        "register": register,
    })
}

fn lang_loader(name: &str) -> helix_core::syntax::Loader {
    let keep: Vec<&str> = if name == "markdown" {
        vec!["markdown", "markdown.inline"]
    } else {
        vec![name]
    };
    let mut config = helix_loader::config::default_lang_config();
    if let Some(languages) = config.get_mut("language").and_then(|v| v.as_array_mut()) {
        languages.retain(|l| {
            l.get("name")
                .and_then(|n| n.as_str())
                .is_some_and(|n| keep.contains(&n))
        });
        for language in languages.iter_mut() {
            language
                .as_table_mut()
                .unwrap()
                .insert("language-servers".into(), toml::Value::Array(Vec::new()));
        }
    }
    helix_core::syntax::Loader::new(config.try_into().unwrap()).unwrap()
}

async fn run_case(input: &str, keys: &str, lang: Option<&str>, file: &str) -> Value {
    let (text, selection) = test::print(input);
    // Open the text as a file, like a user would: no undo step for the initial
    // content and the same document name Red's runner uses.
    let dir = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(err) => return json!({ "error": err.to_string() }),
    };
    let path = dir.path().join(file);
    if let Err(err) = std::fs::write(&path, &text) {
        return json!({ "error": err.to_string() });
    }
    let builder = match lang {
        Some(name) => AppBuilder::new().with_lang_loader(lang_loader(name)),
        None => AppBuilder::new(),
    }
    .with_file(&path, None);
    let mut app = match builder.build() {
        Ok(app) => app,
        Err(err) => return json!({ "error": err.to_string() }),
    };
    {
        let loader = app.editor.syn_loader.load_full();
        let (view, doc) = current!(app.editor);
        if doc.text() != &helix_core::Rope::from(text.as_str()) {
            return json!({ "error": "file text differs from the case" });
        }
        doc.set_selection(view.id, selection);
        if let Some(lang) = lang {
            if let Err(err) = doc.set_language_by_language_id(lang, &loader) {
                return json!({ "error": err.to_string() });
            }
            if doc.syntax().is_none() {
                return json!({ "error": "no syntax" });
            }
        }
    }
    // Red starts from this state instead of parsing the markup itself.
    let start = {
        let (view, doc) = current_ref!(app.editor);
        let selection = doc.selection(view.id);
        json!({
            "text": doc.text().to_string(),
            "ranges": selection.ranges().iter().map(|r| [r.anchor, r.head]).collect::<Vec<_>>(),
            "primary": selection.primary_index(),
        })
    };
    let captured = std::cell::RefCell::new(Value::Null);
    let outcome = test_key_sequence(
        &mut app,
        Some(keys),
        Some(&|app: &Application| *captured.borrow_mut() = snapshot(app)),
        false,
    )
    .await;
    let mut result = captured.into_inner();
    if result.is_null() {
        result = json!({});
    }
    result["start"] = start;
    if let Err(err) = outcome {
        if result.get("out").is_none() {
            result["error"] = json!(err.to_string());
        }
    }
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn felix_oracle() -> anyhow::Result<()> {
    let Ok(input_path) = std::env::var("FELIX_ORACLE_IN") else {
        return Ok(());
    };
    let output_path = std::env::var("FELIX_ORACLE_OUT")?;
    let cases: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(input_path)?)?;
    let mut results = Vec::with_capacity(cases.len());
    for case in cases {
        let input = case["in"].as_str().unwrap_or_default().to_string();
        let keys = case["keys"].as_str().unwrap_or_default().to_string();
        let lang = case["lang"].as_str().map(str::to_string);
        let file = case["file"].as_str().unwrap_or("example.txt").to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(run_case(&input, &keys, lang.as_deref(), &file))
            }));
            let _ = tx.send(outcome.unwrap_or_else(|_| json!({ "error": "panic" })));
        });
        let mut result = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap_or_else(|_| json!({ "error": "timeout" }));
        result["id"] = case["id"].clone();
        results.push(result);
    }
    std::fs::write(output_path, serde_json::to_string(&results)?)?;
    Ok(())
}
