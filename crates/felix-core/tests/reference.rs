//! Exercise the same pinned reference snapshots without Red's input or edit adapter.
use felix_core::{History, HostAction, Range, Selections, Session};
use ropey::Rope;
use serde_json::{Value, json};

#[test]
fn reference_sequences() {
    let source = std::env::var_os("FELIX_SCENARIOS").map_or_else(
        || include_str!("../../../scripts/felix/fixtures/motions.json").to_owned(),
        |path| std::fs::read_to_string(path).expect("live reference"),
    );
    let suite: Value = serde_json::from_str(&source).unwrap();
    for case in suite["cases"].as_array().unwrap() {
        let mut text = Rope::from_str(case["text"].as_str().unwrap());
        let indent = felix_core::detect_indent(text.slice(..)).unwrap_or_else(|| "    ".into());
        let mut sel = Selections::single(text.slice(..), Range::at(text.slice(..), 0));
        let mut session = Session::default();
        let mut search = Vec::new();
        let mut register = Vec::new();
        let mut before = None;
        let mut undo = Vec::new();
        let mut redo = Vec::new();
        for (i, event) in case["events"].as_array().unwrap().iter().enumerate() {
            let key = event["key"].as_str().unwrap();
            let plan = session.plan(
                text.slice(..),
                &sel,
                key,
                &felix_core::Context {
                    search: search.last().map(String::as_str),
                    register: &register,
                    indent: &indent,
                    ..Default::default()
                },
            );
            if !plan.edits.is_empty() && before.is_none() {
                before = Some((text.clone(), sel.clone()));
            }
            for edit in plan.edits.iter().rev() {
                text.remove(edit.start..edit.end);
                text.insert(edit.start, &edit.insert);
            }
            sel = plan.selections(text.slice(..));
            session = plan.next;
            if let Some(value) = plan.search {
                search.push(value);
            }
            if let Some(value) = plan.register {
                register = value;
            }
            if matches!(
                plan.history,
                History::Commit | History::Atomic | History::Checkpoint
            ) {
                if let Some(original) = before.take() {
                    if text != original.0 {
                        undo.push(original);
                        redo.clear();
                    }
                }
            }
            match plan.host {
                Some(HostAction::Undo) => {
                    if let Some(previous) = undo.pop() {
                        redo.push((text, sel));
                        (text, sel) = previous;
                    }
                }
                Some(HostAction::Redo) => {
                    if let Some(next) = redo.pop() {
                        undo.push((text, sel));
                        (text, sel) = next;
                    }
                }
                None => {}
                other => panic!("fixture requires additional host implementation: {other:?}"),
            }
            let actual = json!({"text":text.to_string(),"mode":format!("{:?}",session.mode),
                "ranges": sel.ranges().iter().map(|r|[r.anchor,r.head]).collect::<Vec<_>>(),
                "primary":sel.primary_index(),"pending":session.pending_keys(),"count":session.count(),
                "search":(!search.is_empty()).then(||search.iter().rev().collect::<Vec<_>>()),
                "unnamed":(!register.is_empty()).then_some(&register)});
            for (field, value) in actual.as_object().unwrap() {
                assert_eq!(
                    value,
                    &event[field],
                    "{} input {} ({key}), {field}",
                    case["name"],
                    i + 1
                );
            }
        }
    }
}
