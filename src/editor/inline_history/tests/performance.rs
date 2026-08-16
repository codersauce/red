//! Exercise the reported six-file rename through production History actions.

use super::*;
use crate::inline_history::{InlineAgentEdit, InlineAgentOutcome, InlineAgentState};

const SOURCES: [(&str, &str); 6] = [
    (
        "src/plugin/text_link.rs",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/plugin/text_link.rs"
        )),
    ),
    (
        "src/plugin/mod.rs",
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/plugin/mod.rs")),
    ),
    (
        "src/plugin/markdown.rs",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/plugin/markdown.rs"
        )),
    ),
    (
        "src/editor/inline_history.rs",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/editor/inline_history.rs"
        )),
    ),
    (
        "src/ui/inline_history.rs",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/ui/inline_history.rs"
        )),
    ),
    (
        "src/ui/inline_history/detail.rs",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/ui/inline_history/detail.rs"
        )),
    ),
];

#[tokio::test]
async fn inline_history_multifile_navigation_reuses_rendered_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let mut outcome = InlineAgentOutcome::new("benchmark".into(), "rename".into());
    let mut expected = Vec::new();
    for (index, (name, before)) in SOURCES.into_iter().enumerate() {
        let after = if before.contains("TextPanelFileLocation") {
            before.replace("TextPanelFileLocation", "SourceFileLocation")
        } else {
            // Keep the performance fixture useful if the real type is renamed.
            format!("// History benchmark edit {index}\n{before}")
        };
        let path = directory.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &after).unwrap();
        outcome.record(
            path.to_string_lossy().into_owned(),
            false,
            InlineAgentEdit::new(before.into(), after.clone(), format!("edit-{index}"), true),
        );
        expected.push((path, after));
    }
    outcome.state = InlineAgentState::Completed;
    outcome.validate().unwrap();
    assert_eq!(outcome.files.len(), SOURCES.len());

    let config = Config::default();
    let theme = crate::theme::parse_vscode_theme_contents(
        crate::assets::bundled_theme("red.json").unwrap(),
    )
    .unwrap();
    let mut editor = Editor::with_size(
        Box::new(LspManager::new(config.lsp.clone())),
        180,
        45,
        config,
        theme,
        vec![Buffer::new(
            Some(expected[0].0.to_string_lossy().into_owned()),
            expected[0].1.clone(),
        )],
    )
    .unwrap();
    editor.test_disable_terminal_output();
    begin(
        &mut editor,
        "rename",
        "request",
        line_range(0, 1),
        "Rename across files",
    );
    let turn = editor.inline_history.turn_mut("request").unwrap();
    turn.state = InlineTurnState::Completed;
    turn.result = Some(
        InlineAssistResult::from_tool("request_agent", json!({"reason": "Rename across files"}))
            .unwrap(),
    );
    turn.agent_outcomes.push(outcome);
    editor.inline_assist = None;
    editor
        .test_execute_production_action(Action::OpenInlineHistory)
        .await
        .unwrap();

    let mut cold = Duration::ZERO;
    while editor.inline_history_browser.as_ref().unwrap().view != HistoryView::Changes {
        let started = Instant::now();
        editor
            .test_execute_production_action(Action::InlineHistoryAction(HistoryAction::CycleView))
            .await
            .unwrap();
        if editor.inline_history_browser.as_ref().unwrap().view == HistoryView::Changes {
            cold = started.elapsed();
        }
    }
    let cache = Arc::clone(&editor.inline_history_browser.as_ref().unwrap().render_cache);
    let rendered = cache.cached_lines().unwrap();
    assert!(rendered.len() > 20, "expected a populated multi-file diff");

    let started = Instant::now();
    for _ in 0..2 {
        for action in [
            HistoryAction::Expand,
            HistoryAction::Collapse,
            HistoryAction::Next,
            HistoryAction::Previous,
            HistoryAction::ScrollDown,
            HistoryAction::ScrollUp,
        ] {
            editor
                .test_execute_production_action(Action::InlineHistoryAction(action))
                .await
                .unwrap();
            assert!(
                Arc::ptr_eq(&rendered, &cache.cached_lines().unwrap()),
                "unchanged History navigation rebuilt the six-file diff"
            );
        }
    }
    eprintln!(
        "six-file History: cold={cold:?}, 12 warm actions={:?}",
        started.elapsed()
    );
    for (path, contents) in expected {
        assert_eq!(std::fs::read_to_string(path).unwrap(), contents);
    }
}
