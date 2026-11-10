//! Replay externally generated key-sequence cases against Red's Helix mode and
//! compare the final state with Helix 25.07.1 (`scripts/felix/cm6/run.sh`).
//!
//! The test is a no-op unless `FELIX_CM6_CASES` names a JSON file of
//! `{"id","in","keys","expected"}` entries. Each case starts from the text and
//! selections Helix parsed from the markup (`expected.start`). The report goes to
//! `FELIX_CM6_REPORT`; `FELIX_CM6_BASELINE` lists the ids expected to fail.
use super::macros::terminal_key;
use super::tests::fixture_editor;
use super::*;
use serde_json::{Value, json};

fn key_events(keys: &str) -> Result<Vec<Event>, String> {
    Ok(felix_core::parse_keys(keys)?
        .into_iter()
        .map(|key| {
            let mut event = terminal_key(key);
            // Terminals report Shift with uppercase letters.
            if matches!(event.code, KeyCode::Char(c) if c.is_uppercase()) {
                event.modifiers |= KeyModifiers::SHIFT;
            }
            Event::Key(event)
        })
        .collect())
}

/// Run `keys` from Helix's recorded starting state (`{"text","ranges","primary"}`).
async fn run_case(start: &Value, keys: &str) -> Value {
    let text = start["text"].as_str().unwrap_or_default().to_owned();
    let ranges: Vec<(usize, usize)> = serde_json::from_value(start["ranges"].clone()).unwrap();
    let primary = start["primary"].as_u64().unwrap() as usize;
    let events = match key_events(keys) {
        Ok(events) => events,
        Err(err) => return json!({"error": format!("keys: {err}")}),
    };
    let (mut editor, mut frame, mut runtime) = fixture_editor(&text);
    let shell_root = tempfile::tempdir().unwrap();
    editor.statusline_git_cache.working_directory = shell_root.path().into();
    let rope = ropey::Rope::from_str(&text);
    let ranges = ranges
        .into_iter()
        .map(|(anchor, head)| felix_core::Range { anchor, head })
        .collect();
    let Some(selections) = felix_core::Selections::new(rope.slice(..), ranges, primary) else {
        return json!({"error": "selection"});
    };
    editor.set_felix_active_selection(selections);
    for event in events {
        if let Err(err) = editor
            .process_editor_event(event, &mut frame, &mut runtime, EventRenderMode::Immediate)
            .await
        {
            return json!({"error": format!("event: {err}")});
        }
        while editor.felix.shell_job.is_some() {
            editor
                .service_felix_shell(&mut frame, &mut runtime)
                .await
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    }
    let Some(selections) = editor.felix_active_selection() else {
        return json!({"error": "no felix document"});
    };
    json!({
        "text": editor.current_buffer().contents(),
        "ranges": selections.ranges().iter().map(|r| [r.anchor, r.head]).collect::<Vec<_>>(),
        "primary": selections.primary_index(),
        "mode": format!("{:?}", editor.felix.session.mode).to_lowercase(),
        "register": editor.felix.registers.stored('"'),
    })
}

fn run_isolated(start: Value, keys: String) -> Value {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(run_case(&start, &keys))
        }));
        let _ = tx.send(outcome.unwrap_or_else(|panic| {
            let message = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            json!({"error": format!("panic: {message}")})
        }));
    });
    rx.recv_timeout(std::time::Duration::from_secs(10))
        .unwrap_or_else(|_| json!({"error": "timeout"}))
}

/// Fields compared with Helix, in reporting order.
const FIELDS: [&str; 5] = ["text", "ranges", "primary", "mode", "register"];

#[test]
fn cm6_cases_match_helix() {
    let Some(path) = std::env::var_os("FELIX_CM6_CASES") else {
        return;
    };
    let cases: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let baseline: std::collections::BTreeSet<String> = std::env::var_os("FELIX_CM6_BASELINE")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| {
            // `<id>  # note`; ids themselves may contain `#`.
            text.lines()
                .filter(|line| !line.starts_with('#'))
                .filter_map(|line| line.split_whitespace().next())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    std::panic::set_hook(Box::new(|_| {}));
    let mut failures = Vec::new();
    let (mut passed, mut skipped) = (0, 0);
    for case in &cases {
        let expected = &case["expected"];
        if expected.get("error").is_some() || expected.get("start").is_none() {
            skipped += 1;
            continue;
        }
        let actual = run_isolated(
            expected["start"].clone(),
            case["keys"].as_str().unwrap().into(),
        );
        let fields: Vec<&str> = if actual.get("error").is_some() {
            vec!["error"]
        } else {
            FIELDS
                .into_iter()
                .filter(|field| actual[field] != expected[field])
                .collect()
        };
        if fields.is_empty() {
            passed += 1;
        } else {
            failures.push(json!({
                "id": case["id"], "in": case["in"], "keys": case["keys"],
                "fields": fields, "expected": expected, "actual": actual,
            }));
        }
    }
    let _ = std::panic::take_hook();
    let failed: std::collections::BTreeSet<String> = failures
        .iter()
        .map(|f| f["id"].as_str().unwrap().to_owned())
        .collect();
    let ids: std::collections::BTreeSet<String> = cases
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_owned())
        .collect();
    let new: Vec<_> = failed.difference(&baseline).cloned().collect();
    let fixed: Vec<_> = baseline
        .iter()
        .filter(|id| ids.contains(*id) && !failed.contains(*id))
        .cloned()
        .collect();
    let report = json!({
        "total": cases.len(), "passed": passed, "failed": failures.len(), "skipped": skipped,
        "new": new, "fixed": fixed, "failures": failures,
    });
    if let Some(path) = std::env::var_os("FELIX_CM6_REPORT") {
        std::fs::write(path, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    }
    eprintln!(
        "cm6: {passed} passed, {} failed, {skipped} skipped, {} new, {} fixed",
        failures.len(),
        new.len(),
        fixed.len()
    );
    if std::env::var_os("FELIX_CM6_BASELINE").is_some() {
        assert!(new.is_empty(), "cases newly failing: {new:?}");
        assert!(
            fixed.is_empty(),
            "cases now passing; remove from baseline: {fixed:?}"
        );
    }
}
