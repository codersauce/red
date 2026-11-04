//! Exercise the same pinned reference snapshots without Red's input or edit adapter.
use felix_core::{History, HostAction, InsertEvent, Range, Selections, Session};
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
                let mut rows: Vec<Vec<(usize, usize)>> = Vec::new();
                if let Some(width) = wrap_width {
                    use unicode_segmentation::UnicodeSegmentation;
                    assert!(width > 0);
                    for line in 0..text.len_lines() {
                        let mut offset = text.line_to_char(line);
                        let source = std::borrow::Cow::<str>::from(text.line(line));
                        let mut row = Vec::new();
                        let mut col = 0;
                        for grapheme in source
                            .graphemes(true)
                            .chain((line + 1 == text.len_lines()).then_some(""))
                        {
                            let mut cells = felix_core::cell_width(grapheme, col, 4);
                            // A line terminator or the EOF caret belongs to the last
                            // visual row even when its text fills every cell.
                            if !matches!(grapheme, "" | "\n" | "\r\n")
                                && col > 0
                                && col + cells > width
                            {
                                rows.push(std::mem::take(&mut row));
                                col = 0;
                                cells = felix_core::cell_width(grapheme, col, 4);
                            }
                            row.push((offset, col));
                            col += cells;
                            offset += grapheme.chars().count();
                        }
                        rows.push(row);
                    }
                }
                let visual_motion = |pos: usize, delta: isize, goal: Option<usize>| {
                    let row = rows.partition_point(|r| r[0].0 <= pos) - 1;
                    let cursor = rows[row].partition_point(|&(offset, _)| offset <= pos) - 1;
                    let goal = goal.unwrap_or(rows[row][cursor].1);
                    let dest = &rows[row.saturating_add_signed(delta).min(rows.len() - 1)];
                    let cell = dest.partition_point(|&(_, col)| col <= goal) - 1;
                    (dest[cell].0, goal)
                };
                let mut plan = match (input, event["paste"].as_str()) {
                    (_, Some(value)) => session.bracketed_paste(text.slice(..), &sel, value),
                    (InsertEvent::Key(token), None) => session.plan(
                        text.slice(..),
                        &sel,
                        token,
                        &felix_core::Context {
                            search_history: &search,
                            register: &register,
                            visual_motion: wrap_width.map(|_| &visual_motion as &_),
                            viewport_start,
                            indent: &indent,
                            ..Default::default()
                        },
                    ),
                    (InsertEvent::Completion(change), None) => {
                        session.replay_completion(text.slice(..), &sel, change)
                    }
                };
                if let Some(step) = step {
                    plan.history = step.history(plan.history);
                } else if event.get("paste").is_none() {
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
                    search.push(value);
                }
                if let Some(value) = plan.register {
                    register = value;
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
            let actual = json!({"text":text.to_string(),"mode":format!("{:?}",session.mode),
                "ranges": sel.ranges().iter().map(|r|[r.anchor,r.head]).collect::<Vec<_>>(),
                "primary":sel.primary_index(),"pending":session.pending_keys(),"count":session.count(),
                "search":(!search.is_empty()).then(||search.iter().rev().collect::<Vec<_>>()),
                "unnamed":(!register.is_empty()).then_some(&register)});
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
