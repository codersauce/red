//! Exercise the same pinned reference snapshots without Red's input or edit adapter.
use felix_core::{History, HostAction, InsertEvent, Range, Selections, Session};
use ropey::Rope;
use serde_json::{Value, json};

struct FixtureLines<'a> {
    text: ropey::RopeSlice<'a>,
    width: usize,
}

impl FixtureLines<'_> {
    fn rows(&self, line: std::ops::Range<usize>) -> Vec<Vec<(usize, usize)>> {
        use unicode_segmentation::UnicodeSegmentation;
        let source = std::borrow::Cow::<str>::from(self.text.slice(line.clone()));
        let mut rows = vec![Vec::new()];
        let mut col = 0;
        let mut offset = line.start;
        for grapheme in source.graphemes(true) {
            let mut cells = felix_core::cell_width(grapheme, col, 4);
            if col > 0 && col + cells > self.width {
                rows.push(Vec::new());
                col = 0;
                cells = felix_core::cell_width(grapheme, col, 4);
            }
            rows.last_mut().unwrap().push((offset, col));
            col += cells;
            offset += grapheme.chars().count();
        }
        rows.last_mut().unwrap().push((line.end, col));
        rows
    }
}

impl felix_core::VisualLines for FixtureLines<'_> {
    fn position(&self, line: std::ops::Range<usize>, pos: usize) -> (usize, usize) {
        let rows = self.rows(line);
        let row = rows.partition_point(|r| r[0].0 <= pos) - 1;
        let cell = rows[row].partition_point(|&(offset, _)| offset <= pos) - 1;
        (row, rows[row][cell].1)
    }

    fn offset(&self, line: std::ops::Range<usize>, row: usize, col: usize) -> usize {
        let rows = self.rows(line);
        let target = &rows[row.min(rows.len() - 1)];
        target[target.partition_point(|&(_, column)| column <= col) - 1].0
    }
}

#[test]
fn reference_sequences() {
    let source = std::env::var_os("FELIX_SCENARIOS").map_or_else(
        || include_str!("../../../scripts/felix/fixtures/motions.json").to_owned(),
        |path| std::fs::read_to_string(path).expect("live reference"),
    );
    replay_suite(&source);
}

#[test]
fn reference_registers() {
    replay_suite(include_str!(
        "../../../scripts/felix/fixtures/registers.json"
    ));
}

fn replay_suite(source: &str) {
    let suite: Value = serde_json::from_str(source).unwrap();
    for case in suite["cases"].as_array().unwrap() {
        let mut text = Rope::from_str(case["text"].as_str().unwrap());
        let indent = felix_core::detect_indent(text.slice(..)).unwrap_or_else(|| "    ".into());
        let mut sel = Selections::single(text.slice(..), Range::at(text.slice(..), 0));
        let mut session = Session::default();
        let mut registers = felix_core::Registers::default();
        let mut jumps = felix_core::Jumps::default();
        let mut viewport_start = 0;
        let mut before: Option<(Rope, Selections)> = None;
        let mut last_insert = felix_core::LastInsert::default();
        let mut undo = Vec::new();
        let mut redo = Vec::new();
        for (i, event) in case["events"].as_array().unwrap().iter().enumerate() {
            let key = event["key"].as_str().unwrap();
            let recipe = last_insert.clone();
            let mut replay = recipe.replay(0);
            let mut physical = Some(InsertEvent::Key(key.to_owned()));
            loop {
                let step = replay.next();
                let physical_event = physical.take();
                let Some(input) = step.map(|step| step.event).or(physical_event.as_ref()) else {
                    break;
                };
                // Plain unindented fixture layout, with no retained indent, word
                // wrapping, or annotations. Red independently uses its real rows.
                let wrap_width = case["wrap_width"].as_u64().map(|w| w as usize);
                let layout = FixtureLines {
                    text: text.slice(..),
                    width: wrap_width.unwrap_or(1),
                };
                let mut plan = match (input, event["paste"].as_str()) {
                    (_, Some(value)) => session.bracketed_paste(text.slice(..), &sel, value),
                    (InsertEvent::Key(token), None) => {
                        let token = if token == "<f12>" {
                            "<copy-between-registers>"
                        } else {
                            token
                        };
                        let values = session
                            .register_to_read(token)
                            .filter(|name| registers.exists(*name))
                            .map(|name| {
                                registers.read(name, text.slice(..), &sel, "~/example.txt")
                            });
                        let context = felix_core::Context {
                            search_history: registers.search_history(),
                            register: values.as_deref(),
                            visual_lines: wrap_width.map(|_| &layout as &_),
                            viewport_start,
                            indent: &indent,
                            ..Default::default()
                        };
                        if step.is_some() {
                            session.replay_key(text.slice(..), &sel, token, &context)
                        } else {
                            session.plan(text.slice(..), &sel, token, &context)
                        }
                    }
                    (InsertEvent::Completion(change), None) => {
                        session.replay_completion(text.slice(..), &sel, change)
                    }
                };
                if let Some(step) = step {
                    plan.history = step.history(plan.history);
                } else if event.get("paste").is_none() && !session.awaiting_register_argument() {
                    last_insert.record(key, session.mode, plan.next.mode);
                }
                viewport_start = plan
                    .viewport_start
                    .unwrap_or(viewport_start)
                    .min(text.len_chars());
                if let Some(saved) = &plan.save_jump {
                    jumps.save((), saved);
                }
                if plan.history == History::InsertPaste
                    && let Some(original) = before.take()
                    && text != original.0
                {
                    undo.push(original);
                    redo.clear();
                }
                if !plan.edits.is_empty() && before.is_none() {
                    before = Some((text.clone(), sel.clone()));
                }
                for edit in plan.edits.iter().rev() {
                    text.remove(edit.start..edit.end);
                    text.insert(edit.start, &edit.insert);
                    jumps.rebase(
                        (),
                        text.slice(..),
                        std::iter::once((edit.start, edit.end, edit.insert.chars().count())),
                    );
                }
                sel = plan.selections(text.slice(..));
                session = plan.next;
                if let Some(value) = plan.search {
                    registers.push_query(plan.search_name, value);
                }
                if let Some(value) = plan.register {
                    registers.write(plan.register_name, value);
                }
                if matches!(
                    plan.history,
                    History::Commit | History::Atomic | History::Checkpoint | History::InsertPaste
                ) && let Some(original) = before.take()
                    && text != original.0
                {
                    undo.push(original);
                    redo.clear();
                }
                match plan.host {
                    Some(HostAction::Undo(count)) => {
                        for _ in 0..count {
                            let Some(previous) = undo.pop() else { break };
                            redo.push((text, sel));
                            (text, sel) = previous;
                        }
                    }
                    Some(HostAction::Redo(count)) => {
                        for _ in 0..count {
                            let Some(next) = redo.pop() else { break };
                            undo.push((text, sel));
                            (text, sel) = next;
                        }
                    }
                    Some(HostAction::Jump { forward, count }) => {
                        if let Some(((), saved)) = jumps.navigate((), &sel, forward, count) {
                            sel = saved;
                        }
                    }
                    Some(HostAction::RepeatInsert(count)) => replay = recipe.replay(count),
                    None => {}
                    other => panic!("fixture requires additional host implementation: {other:?}"),
                }
            }
            if let Some(expected) = event.get("last_insert") {
                assert_eq!(
                    json!(last_insert.keys()),
                    *expected,
                    "{} input {} recipe",
                    case["name"],
                    i + 1
                );
            }
            let mut actual = json!({"text":text.to_string(),"mode":format!("{:?}",session.mode),
                "ranges": sel.ranges().iter().map(|r|[r.anchor,r.head]).collect::<Vec<_>>(),
                "primary":sel.primary_index(),"pending":session.pending_keys(),"count":session.count(),
                "search":registers.exists('/').then(||registers.search_history().iter().rev().collect::<Vec<_>>()),
                "unnamed":registers.stored('"')});
            if event.get("last_search_register").is_some() {
                actual["last_search_register"] = json!(session.last_search_register());
            }
            if let Some(slots) = event["registers"].as_object() {
                actual["selected_register"] = json!(session.selected_register());
                actual["register_prompt"] = json!(session.register_prompt());
                actual["registers"] = Value::Object(
                    slots
                        .keys()
                        .map(|slot| {
                            (
                                slot.clone(),
                                json!(registers.stored(slot.chars().next().unwrap())),
                            )
                        })
                        .collect(),
                );
            }
            if let Some(expected) = event.get("prompt") {
                let prompt = session.prompt().map(|(_, line, cursor)| {
                    let (candidates, selected) = session.prompt_completions().unwrap();
                    json!({"line":line, "cursor":cursor, "candidates":candidates, "selected":selected})
                });
                assert_eq!(
                    json!(prompt),
                    *expected,
                    "{} input {} ({key}) prompt",
                    case["name"],
                    i + 1
                );
            }
            if let Some(viewport) = event.get("viewport") {
                assert_eq!(
                    json!({"anchor":viewport_start, "height":felix_core::Context::default().page_height}),
                    *viewport,
                    "{} input {} viewport",
                    case["name"],
                    i + 1
                );
            }
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
