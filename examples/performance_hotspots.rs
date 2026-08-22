//! Reproducible in-process measurements for editor-thread scaling bottlenecks.

use std::{hint::black_box, time::Instant};

use anyhow::Result;
use red::{
    agent_conversation::AgentConversationSnapshot,
    buffer::Buffer,
    config::Config,
    editing::MotionResolver,
    editor::{DetachedEditorCore, Editor, RenderBuffer, SearchDirection},
    highlighter::Highlighter,
    lsp::LspManager,
    plugin::{
        Decoration, DecorationAnchor, DecorationManager, GutterSign, GutterSignManager,
        PanelConfig, PanelManager, PanelRow, PanelRowKind, PanelSide, PluginRegistry, Runtime,
        TextPanelBlock, TextPanelBlockFormat, TextPanelBlockKind, TreePanelModel, WorkspaceConfig,
        WorkspaceManager, WorkspaceModel, WorkspaceRow,
    },
    preferences::PreferencesStore,
    theme::{parse_vscode_theme, Style, Theme},
    ui::{CompletionUI, Picker, PickerItem, PickerOptions},
    undo::{CursorSnapshot, TextPosition, TextRange, UndoHistory},
};
use serde_json::json;

const BACKGROUND_NAMESPACES: usize = 24;
const BACKGROUND_ENTRIES: usize = 128;
const HOT_ENTRIES: usize = 32;
const UPDATE_ITERATIONS: usize = 128;
const PICKER_ITEMS: usize = 24_000;
const PICKER_ROUNDS: usize = 4;
const AGENT_MESSAGES: usize = 128;
const AGENT_DELTAS: usize = 1_500;
const PANEL_LINES: usize = 12_000;
const PANEL_LOOKUPS: usize = 2_000;
const VIEWPORT_ROWS: usize = 64;
const VIEWPORT_UPDATES: usize = 512;
const PENDING_TIMERS: usize = 256;
const TIMER_POLLS: usize = 20_000;
const SEARCH_LINES: usize = 20_000;
const SEARCH_LOOKUPS: usize = 1_000;
const COMPLETION_ITEMS: usize = 18_000;
const COMPLETION_ROUNDS: usize = 4;
const ROW_PANEL_ITEMS: usize = 12_000;
const ROW_PANEL_LOOKUPS: usize = 1_500;
const JSON_CONVERSIONS: usize = 512;
const RENDER_ROWS: usize = 45;
const RENDER_FRAMES: usize = 160;
const PREFERENCES_UPDATES: usize = 128;
const DETACHED_FRAMES: usize = 256;
const WORD_MOTION_LOOKUPS: usize = 4_000;
const PARAGRAPH_LINES: usize = 512;
const PARAGRAPH_MOTIONS: usize = 512;
const SENTENCE_PARAGRAPHS: usize = 256;
const SENTENCE_MOTIONS: usize = 128;
const UNDO_HISTORY_CAPACITY: usize = 128;
const UNDO_TRANSACTIONS: usize = 512;
const HIGHLIGHT_REQUESTS: usize = 2_000;
const TREE_PANEL_ROWS: usize = 8_192;
const TREE_PANEL_LOOKUPS: usize = 256;
const WORKSPACE_ROWS: usize = 12_000;
const WORKSPACE_NAVIGATIONS: usize = 1_000;

fn main() -> Result<()> {
    let scenario = std::env::args().nth(1).unwrap_or_else(|| "all".into());
    let mut results = Vec::new();

    if scenario == "all" || scenario == "decorations" {
        results.push(benchmark_decorations());
    }
    if scenario == "all" || scenario == "gutters" {
        results.push(benchmark_gutters());
    }
    if scenario == "all" || scenario == "agent" {
        results.push(benchmark_agent_streaming());
    }
    if scenario == "all" || scenario == "picker" {
        results.push(benchmark_picker()?);
    }
    if scenario == "all" || scenario == "panel" {
        results.push(benchmark_text_panel()?);
    }
    if scenario == "all" || scenario == "viewport" {
        results.push(benchmark_viewport_snapshots()?);
    }
    if scenario == "all" || scenario == "timers" {
        results.push(benchmark_timer_polling()?);
    }
    if scenario == "all" || scenario == "search" {
        results.push(benchmark_search_navigation()?);
    }
    if scenario == "all" || scenario == "completion" {
        results.push(benchmark_completions()?);
    }
    if scenario == "all" || scenario == "rows" {
        results.push(benchmark_panel_rows()?);
    }
    if scenario == "all" || scenario == "json" {
        results.push(benchmark_json_conversion());
    }
    if scenario == "all" || scenario == "render" {
        results.push(benchmark_ascii_rendering());
    }
    if scenario == "all" || scenario == "preferences" {
        results.push(benchmark_preferences_persistence()?);
    }
    if scenario == "all" || scenario == "detached" {
        results.push(benchmark_detached_frame_serialization()?);
    }
    if scenario == "all" || scenario == "startup" {
        results.push(benchmark_bundled_plugin_startup()?);
    }
    if scenario == "all" || scenario == "word-motion" {
        results.push(benchmark_word_motion());
    }
    if scenario == "all" || scenario == "word-next" {
        results.push(benchmark_next_word_motion());
    }
    if scenario == "all" || scenario == "word-prev" {
        results.push(benchmark_previous_word_motion());
    }
    if scenario == "all" || scenario == "resolver-next" {
        results.push(benchmark_shared_forward_word_motion());
    }
    if scenario == "all" || scenario == "resolver-prev" {
        results.push(benchmark_shared_backward_word_motion());
    }
    if scenario == "all" || scenario == "resolver-range" {
        results.push(benchmark_shared_word_operator());
    }
    if scenario == "all" || scenario == "paragraph" {
        results.push(benchmark_paragraph_motion());
    }
    if scenario == "all" || scenario == "sentence" {
        results.push(benchmark_sentence_motion());
    }
    if scenario == "all" || scenario == "undo-prune" {
        results.push(benchmark_undo_history_pruning());
    }
    if scenario == "all" || scenario == "highlight" {
        results.push(benchmark_repeated_syntax_highlighting()?);
    }
    if scenario == "all" || scenario == "tree-selection" {
        results.push(benchmark_tree_panel_selection()?);
    }
    if scenario == "all" || scenario == "workspace-navigation" {
        results.push(benchmark_workspace_navigation()?);
    }

    anyhow::ensure!(
        !results.is_empty(),
        "unknown performance scenario `{scenario}`"
    );
    println!("{}", serde_json::to_string_pretty(&results)?);
    Ok(())
}

fn benchmark_decorations() -> serde_json::Value {
    let mut manager = DecorationManager::default();
    for namespace in 0..BACKGROUND_NAMESPACES {
        let decorations = (0..BACKGROUND_ENTRIES)
            .map(|line| decoration(namespace * BACKGROUND_ENTRIES + line, 10))
            .collect();
        manager.set(format!("background-{namespace}"), decorations);
    }
    let first = (0..HOT_ENTRIES)
        .map(|line| decoration(20_000 + line, 20))
        .collect::<Vec<_>>();
    let second = (0..HOT_ENTRIES)
        .map(|line| decoration(20_000 + line, 21))
        .collect::<Vec<_>>();

    let started = Instant::now();
    for iteration in 0..UPDATE_ITERATIONS {
        let payload = if iteration % 2 == 0 { &first } else { &second };
        black_box(manager.set("active".to_string(), payload.clone()));
    }
    report("decorations", started, UPDATE_ITERATIONS)
}

fn benchmark_gutters() -> serde_json::Value {
    let mut manager = GutterSignManager::default();
    for namespace in 0..BACKGROUND_NAMESPACES {
        let signs = (0..BACKGROUND_ENTRIES)
            .map(|line| gutter_sign(namespace * BACKGROUND_ENTRIES + line, 10))
            .collect();
        manager.set(format!("background-{namespace}"), signs);
    }
    let first = (0..HOT_ENTRIES)
        .map(|line| gutter_sign(20_000 + line, 20))
        .collect::<Vec<_>>();
    let second = (0..HOT_ENTRIES)
        .map(|line| gutter_sign(20_000 + line, 21))
        .collect::<Vec<_>>();

    let started = Instant::now();
    for iteration in 0..UPDATE_ITERATIONS {
        let payload = if iteration % 2 == 0 { &first } else { &second };
        black_box(manager.set("active".to_string(), payload.clone()));
    }
    report("gutters", started, UPDATE_ITERATIONS)
}

fn benchmark_agent_streaming() -> serde_json::Value {
    let mut conversation = AgentConversationSnapshot::new("benchmark", "/workspace");
    for index in 0..AGENT_MESSAGES {
        conversation.append_user(index.to_string(), "existing discussion ".repeat(32));
    }

    let started = Instant::now();
    for _ in 0..AGENT_DELTAS {
        conversation.append_agent_delta("stream", black_box("streamed assistant token "));
    }
    black_box(conversation.items.len());
    report("agent_streaming", started, AGENT_DELTAS)
}

fn benchmark_picker() -> Result<serde_json::Value> {
    let mut config = Config::default();
    config.lsp.enabled = false;
    let lsp = Box::new(LspManager::new(config.lsp.clone()));
    let editor = Editor::with_size(
        lsp,
        120,
        40,
        config,
        Theme::default(),
        vec![Buffer::new(None, String::new())],
    )?;
    let items = (0..PICKER_ITEMS)
        .map(|index| {
            let label = if index % 12 == 0 {
                format!("needle_pipeline_{index:05}.rs")
            } else {
                format!("third_party_module_{index:05}.rs")
            };
            PickerItem {
                id: label.clone(),
                icon: None,
                label,
                kind: None,
                annotation: None,
                detail: None,
                data: serde_json::Value::Null,
                matches: Vec::new(),
                detail_matches: Vec::new(),
                preview: None,
            }
        })
        .collect();
    let mut picker = Picker::new_dynamic(None, &editor, items, 1, PickerOptions::default());
    let queries = [
        "n", "ne", "nee", "need", "needl", "needle", "needle_", "needle_p",
    ];

    let started = Instant::now();
    for _ in 0..PICKER_ROUNDS {
        picker.filter("");
        for query in queries {
            picker.filter(black_box(query));
        }
    }
    Ok(report(
        "picker_filter",
        started,
        PICKER_ROUNDS * queries.len(),
    ))
}

fn benchmark_text_panel() -> Result<serde_json::Value> {
    let mut panels = PanelManager::default();
    panels.create_text_panel(
        "agent".to_string(),
        PanelConfig {
            side: PanelSide::Right,
            width: 48,
            ..PanelConfig::default()
        },
    );
    let transcript = (0..PANEL_LINES)
        .map(|line| format!("Conversation line {line:05} with useful content"))
        .collect::<Vec<_>>()
        .join("\n");
    panels.update_text_panel(
        "agent",
        vec![TextPanelBlock {
            id: "answer".to_string(),
            kind: TextPanelBlockKind::Agent,
            format: TextPanelBlockFormat::Plain,
            text: transcript,
        }],
        38,
        120,
    );
    anyhow::ensure!(
        panels.focus_panel("agent"),
        "could not focus benchmark panel"
    );
    anyhow::ensure!(
        panels.focused_text_panel_cursor_position(120, 40).is_some(),
        "benchmark panel has no visible cursor"
    );

    let started = Instant::now();
    for _ in 0..PANEL_LOOKUPS {
        black_box(panels.focused_text_panel_cursor_position(120, 40));
    }
    Ok(report("panel_cursor_lookup", started, PANEL_LOOKUPS))
}

fn benchmark_viewport_snapshots() -> Result<serde_json::Value> {
    let mut runtime = Runtime::try_new()?;
    let rows = (0..VIEWPORT_ROWS)
        .map(|line| {
            json!({
                "screen_row": line,
                "line": line,
                "start_col": 0,
                "end_col": 80,
                "start_grapheme": 0,
                "end_grapheme": 80,
                "first_segment": true,
                "indent_width": line % 8 * 4,
                "visual_offset": 0,
                "text": format!("{}fn function_{line}() {{ value(); }}", " ".repeat(line % 8 * 4)),
            })
        })
        .collect::<Vec<_>>();
    let viewport = json!({
        "buffer_index": 0,
        "window_id": 1,
        "width": 120,
        "height": VIEWPORT_ROWS,
        "vtop": 0,
        "vleft": 0,
        "skipcol": 0,
        "wrap": false,
        "revision": 1,
        "cursor": { "x": 8, "y": 0, "lsp_character": 8, "screen_row": 0 },
        "rows": rows,
    });
    runtime.set_snapshot("viewport_layout", viewport);

    let started = Instant::now();
    for iteration in 0..VIEWPORT_UPDATES {
        let line = iteration % VIEWPORT_ROWS;
        let cursor = json!({
            "x": 8,
            "y": line,
            "lsp_character": 8,
            "screen_row": line,
        });
        anyhow::ensure!(
            runtime.update_viewport_cursor(black_box(cursor)),
            "viewport cursor snapshot was unavailable"
        );
    }
    Ok(report(
        "viewport_cursor_snapshot",
        started,
        VIEWPORT_UPDATES,
    ))
}

fn benchmark_timer_polling() -> Result<serde_json::Value> {
    let mut runtime = Runtime::try_new()?;
    let source = format!(
        "pub fn activate() {{ let index = 0; while index < {PENDING_TIMERS} {{ red::execute(\"SetTimeout\", 60000); index = index + 1; }} }}"
    );
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    executor.block_on(runtime.load_plugin("timer_benchmark", &source))?;

    let started = Instant::now();
    for _ in 0..TIMER_POLLS {
        black_box(runtime.poll_timer_callbacks());
    }
    Ok(report("idle_timer_polling", started, TIMER_POLLS))
}

fn benchmark_search_navigation() -> Result<serde_json::Value> {
    let mut config = Config::default();
    config.lsp.enabled = false;
    let lsp = Box::new(LspManager::new(config.lsp.clone()));
    let contents = "needle in a searchable haystack\n".repeat(SEARCH_LINES);
    let mut editor = Editor::with_size(
        lsp,
        120,
        40,
        config,
        Theme::default(),
        vec![Buffer::new(None, contents)],
    )?;
    editor.test_search_match_from_origin("needle", 0, 0, SearchDirection::Forward, true)?;

    let started = Instant::now();
    for iteration in 0..SEARCH_LOOKUPS {
        let (line, direction) = if iteration % 2 == 0 {
            (SEARCH_LINES - 1, SearchDirection::Forward)
        } else {
            (0, SearchDirection::Backward)
        };
        black_box(editor.test_search_match_from_origin("needle", 0, line, direction, true)?);
    }
    Ok(report("search_match_navigation", started, SEARCH_LOOKUPS))
}

fn benchmark_completions() -> Result<serde_json::Value> {
    let items = (0..COMPLETION_ITEMS)
        .map(|index| {
            let label = if index % 12 == 0 {
                format!("needle_pipeline_{index:05}")
            } else {
                format!("third_party_module_{index:05}")
            };
            serde_json::from_value(json!({ "label": label }))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut completion = CompletionUI::new();
    completion.show(items, 8, 4);
    let queries = ["n", "ne", "nee", "need", "needl", "needle", "needle_"];

    let started = Instant::now();
    for _ in 0..COMPLETION_ROUNDS {
        completion.set_filter("");
        for query in queries {
            completion.set_filter(black_box(query));
        }
    }
    Ok(report(
        "lsp_completion_filter",
        started,
        COMPLETION_ROUNDS * queries.len(),
    ))
}

fn benchmark_panel_rows() -> Result<serde_json::Value> {
    let mut panels = PanelManager::default();
    panels.create_panel("files".to_string(), PanelConfig::default());
    let rows = (0..ROW_PANEL_ITEMS)
        .map(|index| PanelRow {
            id: format!("file-{index:05}"),
            path: None,
            expanded: None,
            kind: PanelRowKind::File,
            segments: Vec::new(),
            right_segments: Vec::new(),
        })
        .collect();
    panels.update_panel("files", rows);
    let target = format!("file-{:05}", ROW_PANEL_ITEMS - 1);
    anyhow::ensure!(
        panels.select_row_by_id("files", &target, 40),
        "row panel benchmark target is missing"
    );

    let started = Instant::now();
    for _ in 0..ROW_PANEL_LOOKUPS {
        black_box(panels.select_row_by_id("files", &target, 40));
    }
    Ok(report("row_panel_selection", started, ROW_PANEL_LOOKUPS))
}

fn benchmark_json_conversion() -> serde_json::Value {
    let payload = json!({
        "cursor": { "x": 8, "y": 20 },
        "rows": (0..VIEWPORT_ROWS).map(|line| json!({
            "line": line,
            "kind": "source",
            "text": format!("fn function_{line}() {{ value(); }}"),
            "indentation": { "width": line % 8 * 4, "tabs": false },
        })).collect::<Vec<_>>()
    });
    let payloads = (0..JSON_CONVERSIONS)
        .map(|_| payload.clone())
        .collect::<Vec<_>>();

    let started = Instant::now();
    for payload in payloads {
        black_box(husk_runtime::Value::from_json(black_box(payload)));
    }
    report("husk_json_conversion", started, JSON_CONVERSIONS)
}

fn benchmark_ascii_rendering() -> serde_json::Value {
    let style = Style::default();
    let mut buffer = RenderBuffer::new(120, RENDER_ROWS, &style);
    let lines = (0..RENDER_ROWS)
        .map(|line| {
            format!(
                "fn render_line_{line:03}(value: usize) -> usize {{ value.saturating_add({line}) }} // editor source"
            )
        })
        .collect::<Vec<_>>();

    let started = Instant::now();
    for _ in 0..RENDER_FRAMES {
        for (row, line) in lines.iter().enumerate() {
            buffer.set_text(0, row, black_box(line), &style);
        }
    }
    black_box(buffer.cells.len());
    report(
        "ascii_frame_rendering",
        started,
        RENDER_FRAMES * RENDER_ROWS,
    )
}

fn benchmark_preferences_persistence() -> Result<serde_json::Value> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("preferences.json");
    let mut preferences = PreferencesStore::load(&path);
    let value = json!({
        "thread": "performance-benchmark",
        "transcript": "Agent conversation context. ".repeat(256),
    });
    preferences.set_plugin_storage("agent", "conversation", value.clone())?;

    let started = Instant::now();
    for _ in 0..PREFERENCES_UPDATES {
        preferences.set_plugin_storage("agent", "conversation", black_box(value.clone()))?;
    }
    Ok(report(
        "plugin_preferences_persistence",
        started,
        PREFERENCES_UPDATES,
    ))
}

fn benchmark_detached_frame_serialization() -> Result<serde_json::Value> {
    let mut config = Config::default();
    config.lsp.enabled = false;
    let lsp = Box::new(LspManager::new(config.lsp.clone()));
    let contents = (0..80)
        .map(|line| {
            format!("fn detached_line_{line:03}(value: usize) -> usize {{ value + {line} }}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let editor = Editor::with_size(
        lsp,
        120,
        45,
        config,
        Theme::default(),
        vec![Buffer::new(None, contents)],
    )?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut core = runtime.block_on(DetachedEditorCore::new(editor))?;

    let started = Instant::now();
    for iteration in 0..DETACHED_FRAMES {
        black_box(core.benchmark_incremental_frame(black_box(iteration))?);
    }
    Ok(report(
        "detached_frame_serialization",
        started,
        DETACHED_FRAMES,
    ))
}

fn benchmark_bundled_plugin_startup() -> Result<serde_json::Value> {
    let config = toml::from_str::<Config>(include_str!("../default_config.toml"))?;
    let mut plugins = PluginRegistry::new();
    for (name, path) in &config.plugins {
        plugins.add(name, &Config::resolve_plugin_path(path));
    }
    let plugin_count = config.plugins.len();
    let mut runtime = Runtime::try_new()?;
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let started = Instant::now();
    executor.block_on(plugins.initialize(&mut runtime))?;
    anyhow::ensure!(
        plugins.statuses().len() == plugin_count,
        "bundled plugin initialization lost a configured plugin"
    );
    Ok(report("bundled_plugin_startup", started, plugin_count))
}

fn benchmark_word_motion() -> serde_json::Value {
    let prefix = "ordinary_identifier ".repeat(512);
    let offset = prefix.len();
    let buffer = Buffer::new(None, format!("{prefix}target_identifier remaining"));

    let started = Instant::now();
    for _ in 0..WORD_MOTION_LOOKUPS {
        black_box(buffer.find_word_end((black_box(offset), 0)));
    }
    report("buffer_word_end_motion", started, WORD_MOTION_LOOKUPS)
}

fn benchmark_next_word_motion() -> serde_json::Value {
    let prefix = "ordinary_identifier ".repeat(512);
    let offset = prefix.len();
    let buffer = Buffer::new(None, format!("{prefix}target_identifier remaining"));

    let started = Instant::now();
    for _ in 0..WORD_MOTION_LOOKUPS {
        black_box(buffer.find_next_word((black_box(offset), 0)));
    }
    report("buffer_next_word_motion", started, WORD_MOTION_LOOKUPS)
}

fn benchmark_previous_word_motion() -> serde_json::Value {
    let prefix = "ordinary_identifier ".repeat(512);
    let offset = prefix.len() + "target_identifier".len();
    let buffer = Buffer::new(None, format!("{prefix}target_identifier remaining"));

    let started = Instant::now();
    for _ in 0..WORD_MOTION_LOOKUPS {
        black_box(buffer.find_prev_word((black_box(offset), 0)));
    }
    report("buffer_previous_word_motion", started, WORD_MOTION_LOOKUPS)
}

fn benchmark_shared_forward_word_motion() -> serde_json::Value {
    let prefix = "ordinary_identifier ".repeat(512);
    let offset = prefix.len();
    let buffer = Buffer::new(None, format!("{prefix}target_identifier remaining"));
    let resolver = MotionResolver::new(&buffer, TextPosition::new(0, offset));

    let started = Instant::now();
    for _ in 0..WORD_MOTION_LOOKUPS {
        black_box(resolver.word_target(
            /*count*/ 1, /*backward*/ false, /*end*/ false, /*big_word*/ false,
        ));
    }
    report("shared_forward_word_motion", started, WORD_MOTION_LOOKUPS)
}

fn benchmark_shared_backward_word_motion() -> serde_json::Value {
    let prefix = "ordinary_identifier ".repeat(512);
    let offset = prefix.len() + "target_identifier".len();
    let buffer = Buffer::new(None, format!("{prefix}target_identifier remaining"));
    let resolver = MotionResolver::new(&buffer, TextPosition::new(0, offset));

    let started = Instant::now();
    for _ in 0..WORD_MOTION_LOOKUPS {
        black_box(resolver.word_target(
            /*count*/ 1, /*backward*/ true, /*end*/ false, /*big_word*/ false,
        ));
    }
    report("shared_backward_word_motion", started, WORD_MOTION_LOOKUPS)
}

fn benchmark_shared_word_operator() -> serde_json::Value {
    let prefix = "ordinary_identifier ".repeat(512);
    let offset = prefix.len();
    let buffer = Buffer::new(None, format!("{prefix}target_identifier remaining"));
    let resolver = MotionResolver::new(&buffer, TextPosition::new(0, offset));

    let started = Instant::now();
    for _ in 0..WORD_MOTION_LOOKUPS {
        black_box(resolver.word_range(
            /*count*/ 1, /*change_word*/ false, /*big_word*/ false,
        ));
    }
    report("shared_word_operator_motion", started, WORD_MOTION_LOOKUPS)
}

fn benchmark_paragraph_motion() -> serde_json::Value {
    let line = "ordinary_identifier ".repeat(12);
    let contents = format!(
        "{}\nnext paragraph",
        format!("{line}\n").repeat(PARAGRAPH_LINES)
    );
    let buffer = Buffer::new(None, contents);
    let resolver = MotionResolver::new(&buffer, TextPosition::new(0, 0));

    let started = Instant::now();
    for _ in 0..PARAGRAPH_MOTIONS {
        black_box(resolver.paragraph_target(/*count*/ 1, /*backward*/ false));
    }
    report("vim_paragraph_motion", started, PARAGRAPH_MOTIONS)
}

fn benchmark_sentence_motion() -> serde_json::Value {
    let paragraph = "One sentence. Another follows! Third closes?\n\n";
    let buffer = Buffer::new(None, paragraph.repeat(SENTENCE_PARAGRAPHS));
    let cursor = TextPosition::new((SENTENCE_PARAGRAPHS - 1) * 2, 0);
    let resolver = MotionResolver::new(&buffer, cursor);

    let started = Instant::now();
    for _ in 0..SENTENCE_MOTIONS {
        black_box(resolver.sentence_target(/*count*/ 1, /*backward*/ false));
    }
    report("vim_sentence_motion", started, SENTENCE_MOTIONS)
}

fn benchmark_undo_history_pruning() -> serde_json::Value {
    let mut history = UndoHistory::default();
    history.set_max_nodes(UNDO_HISTORY_CAPACITY);
    let text = "ordinary unicode edit payload 世界 ".repeat(24);

    let started = Instant::now();
    for index in 0..UNDO_TRANSACTIONS {
        history.begin_transaction("insert", CursorSnapshot::default());
        history.record_replace(
            TextRange::insertion(TextPosition::new(0, index)),
            index,
            String::new(),
            text.clone(),
        );
        black_box(history.commit_transaction(CursorSnapshot::default()));
    }
    report("undo_history_capacity_pruning", started, UNDO_TRANSACTIONS)
}

fn benchmark_repeated_syntax_highlighting() -> Result<serde_json::Value> {
    let theme = parse_vscode_theme("themes/mocha.json")?;
    let mut highlighter = Highlighter::new(&theme)?;
    let source = concat!(
        "fn greeting(name: &str) -> String {\n",
        "    let prefix = \"Olá\";\n",
        "    format!(\"{prefix}, {name}!\")\n",
        "}\n",
    );
    black_box(highlighter.highlight("rust", source)?);

    let started = Instant::now();
    for _ in 0..HIGHLIGHT_REQUESTS {
        black_box(highlighter.highlight(black_box("rust"), black_box(source))?);
    }
    Ok(report(
        "repeated_syntax_highlighting",
        started,
        HIGHLIGHT_REQUESTS,
    ))
}

fn benchmark_tree_panel_selection() -> Result<serde_json::Value> {
    let entries = (0..TREE_PANEL_ROWS)
        .map(|index| {
            json!({
                "name": format!("file-{index:04}.rs"),
                "path": format!("./file-{index:04}.rs"),
                "kind": "file",
            })
        })
        .collect::<Vec<_>>();
    let model = TreePanelModel::from_husk_values(&[
        husk_runtime::Value::String("/repo".to_string()),
        husk_runtime::Value::from_json(json!([{ "path": ".", "entries": entries }])),
        husk_runtime::Value::from_json(json!(["."])),
        husk_runtime::Value::from_json(json!([])),
        husk_runtime::Value::from_json(json!([])),
        husk_runtime::Value::String(String::new()),
        husk_runtime::Value::from_json(json!([])),
    ])?;
    let mut panels = PanelManager::default();
    panels.create_panel("tree".to_string(), PanelConfig::default());
    panels.update_tree_panel("tree", model);
    let target = format!("./file-{:04}.rs", TREE_PANEL_ROWS - 1);
    anyhow::ensure!(
        panels.select_row_by_id("tree", &target, 40),
        "tree panel benchmark target is missing"
    );

    let started = Instant::now();
    for _ in 0..TREE_PANEL_LOOKUPS {
        black_box(panels.select_row_by_id("tree", black_box(&target), 40));
    }
    Ok(report(
        "neotree_repeated_row_selection",
        started,
        TREE_PANEL_LOOKUPS,
    ))
}

fn benchmark_workspace_navigation() -> Result<serde_json::Value> {
    let mut workspaces = WorkspaceManager::default();
    workspaces.open("git".to_string(), WorkspaceConfig::default());
    let model = WorkspaceModel {
        rows: (0..WORKSPACE_ROWS)
            .map(|index| WorkspaceRow {
                id: format!("file-{index:05}"),
                selectable: true,
                depth: 0,
                path: Some(format!("src/file-{index:05}.rs")),
                segments: Vec::new(),
                right_segments: Vec::new(),
                data: serde_json::Value::Null,
            })
            .collect(),
        ..WorkspaceModel::default()
    };
    anyhow::ensure!(
        workspaces.update("git", model, &Theme::default()),
        "workspace benchmark failed to initialize rows"
    );
    workspaces.handle_action("last".to_string(), 40, 120);

    let started = Instant::now();
    for index in 0..WORKSPACE_NAVIGATIONS {
        let action = if index % 2 == 0 { "up" } else { "down" };
        black_box(workspaces.handle_action(action.to_string(), 40, 120));
    }
    Ok(report(
        "git_workspace_row_navigation",
        started,
        WORKSPACE_NAVIGATIONS,
    ))
}

fn decoration(line: usize, priority: i32) -> Decoration {
    Decoration {
        buffer_index: Some(0),
        anchor: DecorationAnchor::Column,
        line,
        column: 4,
        text: "│".to_string(),
        style: Style::default(),
        priority,
        repeat_linebreak: false,
        only_whitespace: true,
    }
}

fn gutter_sign(line: usize, priority: i32) -> GutterSign {
    GutterSign {
        buffer_index: 0,
        line,
        text: "+".to_string(),
        style: Style::default(),
        priority,
    }
}

fn report(scenario: &str, started: Instant, iterations: usize) -> serde_json::Value {
    let elapsed = started.elapsed();
    json!({
        "scenario": scenario,
        "iterations": iterations,
        "elapsed_us": elapsed.as_micros(),
        "mean_us": elapsed.as_micros() / u128::try_from(iterations).unwrap_or(1).max(1),
        "background_namespaces": BACKGROUND_NAMESPACES,
        "background_entries": BACKGROUND_ENTRIES,
        "picker_items": PICKER_ITEMS,
    })
}
