//! Reproducible in-process measurements for editor-thread scaling bottlenecks.

use std::{hint::black_box, time::Instant};

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use red::{
    agent_conversation::AgentConversationSnapshot,
    buffer::Buffer,
    config::Config,
    editing::{MotionResolver, TextArea, TextObjectKind, TextObjectScope},
    editor::{DetachedEditorCore, Editor, Mode, RenderBuffer, SearchDirection},
    highlighter::Highlighter,
    inline_history::{InlineConversation, InlineHistory, InlineHistoryTurn},
    lsp::{LspManager, RealLspClient},
    plugin::{
        Decoration, DecorationAnchor, DecorationManager, GutterSign, GutterSignManager,
        PanelConfig, PanelManager, PanelRow, PanelRowKind, PanelSide, PluginRegistry, PluginStatus,
        Runtime, TextPanelBlock, TextPanelBlockFormat, TextPanelBlockKind, TreePanelModel,
        WorkspaceConfig, WorkspaceManager, WorkspaceModel, WorkspaceRow,
    },
    preferences::PreferencesStore,
    session::SessionStore,
    text_layout::{LayoutOptions, TextLayout, WrapMode},
    theme::{parse_vscode_theme, parse_vscode_theme_contents, Style, Theme},
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
const INLINE_CONVERSATIONS: usize = 4_096;
const INLINE_ANSWER_DELTAS: usize = 1_000;
const STATUSLINE_FRAMES: usize = 1_000;
const LSP_DOCUMENT_RESOLVES: usize = 1_000;
const RECOVERY_BUFFERS: usize = 32;
const RECOVERY_RESTORES: usize = 8;
const WORKSPACE_SEARCH_DIRECTORIES: usize = 8;
const WORKSPACE_SEARCH_FILES_PER_DIRECTORY: usize = 32;
const WORKSPACE_SEARCH_LISTINGS: usize = 8;
const WORKSPACE_CONTENT_SEARCHES: usize = 4;
const PLUGIN_EVENT_BACKGROUND: usize = 64;
const PLUGIN_EVENT_DELIVERIES: usize = 4_096;
const SNAPSHOT_WRITE_BUFFERS: usize = 24;
const SNAPSHOT_WRITE_UNDO_NODES: usize = 48;
const SNAPSHOT_WRITES: usize = 6;
const FULL_FRAME_RENDERS: usize = 160;
const GIT_DISCOVERY_DEPTH: usize = 16;
const GIT_REPOSITORY_DISCOVERIES: usize = 512;
const STARTUP_FILE_COUNT: usize = 128;
const STARTUP_FILE_LOADS: usize = 4;
const LSP_DOCUMENT_LINES: usize = 4_096;
const LSP_INCREMENTAL_CHANGES: usize = 256;
const TEXTAREA_INITIAL_BYTES: usize = 32 * 1024;
const TEXTAREA_INSERTIONS: usize = 256;
const STARTUP_CONFIG_LOADS: usize = 24;
const STARTUP_THEME_LOADS: usize = 256;
const THEME_COLOR_PARSES: usize = 16_384;
const ASCII_GRAPHEME_COUNTS: usize = 1_024;
const TEXTAREA_DOCUMENT_LOADS: usize = 128;
const GIT_STATUS_FILES: usize = 2_048;
const GIT_STATUS_INDEX_BUILDS: usize = 32;
const GIT_STATUS_REFRESHES: usize = 32;
const BUFFER_LINE_LOOKUPS: usize = 4_096;
const SPARSE_REGEX_SEARCHES: usize = 128;
const WORD_OPERATOR_LOOKUPS: usize = 512;
const LAYOUT_CURSOR_LOOKUPS: usize = 2_048;
const TEXTAREA_VIM_MOTIONS: usize = 2_048;
const TEXTAREA_DELIMITER_MOTIONS: usize = 1_024;
const TEXTAREA_DELETE_EVENTS: usize = 256;
const TEXTAREA_UNDO_RESTORES: usize = 128;

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
    if scenario == "all" || scenario == "resolver-change-word" {
        results.push(benchmark_prefix_word_operator(/*change_word*/ true)?);
    }
    if scenario == "all" || scenario == "resolver-delete-word" {
        results.push(benchmark_prefix_word_operator(/*change_word*/ false)?);
    }
    if scenario == "all" || scenario == "resolver-count-change" {
        results.push(benchmark_counted_word_operator(/*change_word*/ true)?);
    }
    if scenario == "all" || scenario == "resolver-count-delete" {
        results.push(benchmark_counted_word_operator(/*change_word*/ false)?);
    }
    if scenario == "all" || scenario == "editor-word-end-backward" {
        results.push(benchmark_editor_word_end_operator(/*backward*/ true)?);
    }
    if scenario == "all" || scenario == "editor-word-end-forward" {
        results.push(benchmark_editor_word_end_operator(/*backward*/ false)?);
    }
    if scenario == "all" || scenario == "editor-cursor-reverse" {
        results.push(benchmark_editor_cursor_conversion(
            /*word_search*/ false,
        )?);
    }
    if scenario == "all" || scenario == "editor-cursor-word-search" {
        results.push(benchmark_editor_cursor_conversion(
            /*word_search*/ true,
        )?);
    }
    if scenario == "all" || scenario == "editor-line-length" {
        results.push(benchmark_editor_line_boundary(/*last_cell*/ false)?);
    }
    if scenario == "all" || scenario == "editor-line-last-cell" {
        results.push(benchmark_editor_line_boundary(/*last_cell*/ true)?);
    }
    if scenario == "all" || scenario == "editor-cursor-display" {
        results.push(benchmark_editor_cursor_position(
            /*language_server*/ false, /*window_snapshot*/ false,
        )?);
    }
    if scenario == "all" || scenario == "editor-lsp-position" {
        results.push(benchmark_editor_cursor_position(
            /*language_server*/ true, /*window_snapshot*/ false,
        )?);
    }
    if scenario == "all" || scenario == "editor-lsp-window" {
        results.push(benchmark_editor_cursor_position(
            /*language_server*/ true, /*window_snapshot*/ true,
        )?);
    }
    if scenario == "all" || scenario == "editor-line-scalar-ascii" {
        results.push(benchmark_editor_scalar_line_boundary(
            /*unicode*/ false, /*operator*/ false,
        )?);
    }
    if scenario == "all" || scenario == "editor-line-scalar-unicode" {
        results.push(benchmark_editor_scalar_line_boundary(
            /*unicode*/ true, /*operator*/ false,
        )?);
    }
    if scenario == "all" || scenario == "editor-line-end-ascii" {
        results.push(benchmark_editor_scalar_line_boundary(
            /*unicode*/ false, /*operator*/ true,
        )?);
    }
    if scenario == "all" || scenario == "editor-line-end-unicode" {
        results.push(benchmark_editor_scalar_line_boundary(
            /*unicode*/ true, /*operator*/ true,
        )?);
    }
    if scenario == "all" || scenario == "editor-rename-ascii" {
        results.push(benchmark_editor_rename_symbol(/*unicode*/ false)?);
    }
    if scenario == "all" || scenario == "editor-rename-unicode" {
        results.push(benchmark_editor_rename_symbol(/*unicode*/ true)?);
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
    if scenario == "all" || scenario == "inline-stream" {
        results.push(benchmark_inline_answer_streaming()?);
    }
    if scenario == "all" || scenario == "statusline" {
        results.push(benchmark_statusline_rendering()?);
    }
    if scenario == "all" || scenario == "lsp-routing" {
        results.push(benchmark_lsp_document_routing()?);
    }
    if scenario == "all" || scenario == "session-restore" {
        results.push(benchmark_session_buffer_restoration()?);
    }
    if scenario == "all" || scenario == "workspace-files" {
        results.push(benchmark_workspace_file_discovery()?);
    }
    if scenario == "all" || scenario == "workspace-search" {
        results.push(benchmark_workspace_content_search()?);
    }
    if scenario == "all" || scenario == "plugin-events" {
        results.push(benchmark_plugin_event_delivery()?);
    }
    if scenario == "all" || scenario == "session-write" {
        results.push(benchmark_session_snapshot_writes()?);
    }
    if scenario == "all" || scenario == "frame-full" {
        results.push(benchmark_full_editor_frames()?);
    }
    if scenario == "all" || scenario == "git-discovery" {
        results.push(benchmark_git_repository_discovery()?);
    }
    if scenario == "all" || scenario == "startup-files" {
        results.push(benchmark_startup_file_loading()?);
    }
    if scenario == "all" || scenario == "lsp-changes" {
        results.push(benchmark_lsp_incremental_changes()?);
    }
    if scenario == "all" || scenario == "textarea-typing" {
        results.push(benchmark_embedded_textarea_typing()?);
    }
    if scenario == "all" || scenario == "startup-config" {
        results.push(benchmark_startup_configuration_loading()?);
    }
    if scenario == "all" || scenario == "startup-theme" {
        results.push(benchmark_startup_theme_loading()?);
    }
    if scenario == "all" || scenario == "theme-colors" {
        results.push(benchmark_theme_color_parsing()?);
    }
    if scenario == "all" || scenario == "ascii-graphemes" {
        results.push(benchmark_ascii_grapheme_counting());
    }
    if scenario == "all" || scenario == "textarea-open" {
        results.push(benchmark_embedded_textarea_loading()?);
    }
    if scenario == "all" || scenario == "git-status-index" {
        results.push(benchmark_git_status_indexing()?);
    }
    if scenario == "all" || scenario == "git-status-refresh" {
        results.push(benchmark_git_status_refresh(/*repository*/ true)?);
    }
    if scenario == "all" || scenario == "git-status-outside" {
        results.push(benchmark_git_status_refresh(/*repository*/ false)?);
    }
    if scenario == "all" || scenario == "buffer-last-line" {
        results.push(benchmark_buffer_line_boundary(/*count*/ false)?);
    }
    if scenario == "all" || scenario == "buffer-line-count" {
        results.push(benchmark_buffer_line_boundary(/*count*/ true)?);
    }
    if scenario == "all" || scenario == "search-sparse-ascii" {
        results.push(benchmark_sparse_regex_matches(/*unicode*/ false)?);
    }
    if scenario == "all" || scenario == "search-sparse-unicode" {
        results.push(benchmark_sparse_regex_matches(/*unicode*/ true)?);
    }
    if scenario == "all" || scenario == "layout-grapheme-cursor" {
        results.push(benchmark_layout_cursor_lookup(WrapMode::Grapheme)?);
    }
    if scenario == "all" || scenario == "layout-word-cursor" {
        results.push(benchmark_layout_cursor_lookup(WrapMode::Word)?);
    }
    if scenario == "all" || scenario == "textarea-vim-word" {
        results.push(benchmark_embedded_vim_motion(/*line_motion*/ false)?);
    }
    if scenario == "all" || scenario == "textarea-vim-line" {
        results.push(benchmark_embedded_vim_motion(/*line_motion*/ true)?);
    }
    if scenario == "all" || scenario == "textarea-vim-match" {
        results.push(benchmark_embedded_vim_delimiter_motion()?);
    }
    if scenario == "all" || scenario == "textarea-delete" {
        results.push(benchmark_embedded_textarea_deletion(/*word*/ false)?);
    }
    if scenario == "all" || scenario == "textarea-word-delete" {
        results.push(benchmark_embedded_textarea_deletion(/*word*/ true)?);
    }
    if scenario == "all" || scenario == "textarea-home-end" {
        results.push(benchmark_embedded_home_end_navigation()?);
    }
    if scenario == "all" || scenario == "paragraph-operator" {
        results.push(benchmark_boundary_operator(/*sentence*/ false)?);
    }
    if scenario == "all" || scenario == "sentence-operator" {
        results.push(benchmark_boundary_operator(/*sentence*/ true)?);
    }
    if scenario == "all" || scenario == "vim-long-line-end" {
        results.push(benchmark_long_line_end_motion()?);
    }
    if scenario == "all" || scenario == "paragraph-long-line" {
        results.push(benchmark_long_line_paragraph_operator()?);
    }
    if scenario == "all" || scenario == "textarea-undo" {
        results.push(benchmark_embedded_undo_restoration(/*redo*/ false)?);
    }
    if scenario == "all" || scenario == "textarea-redo" {
        results.push(benchmark_embedded_undo_restoration(/*redo*/ true)?);
    }
    if scenario == "all" || scenario == "text-object-delimited" {
        results.push(benchmark_text_object_selection(/*quoted*/ false)?);
    }
    if scenario == "all" || scenario == "text-object-quoted" {
        results.push(benchmark_text_object_selection(/*quoted*/ true)?);
    }
    if scenario == "all" || scenario == "text-object-word" {
        results.push(benchmark_word_text_object(/*big_word*/ false)?);
    }
    if scenario == "all" || scenario == "text-object-big-word" {
        results.push(benchmark_word_text_object(/*big_word*/ true)?);
    }
    if scenario == "all" || scenario == "text-object-paragraph-inner" {
        results.push(benchmark_paragraph_text_object(TextObjectScope::Inner)?);
    }
    if scenario == "all" || scenario == "text-object-paragraph-around" {
        results.push(benchmark_paragraph_text_object(TextObjectScope::Around)?);
    }
    if scenario == "all" || scenario == "text-object-sentence-inner" {
        results.push(benchmark_sentence_text_object(TextObjectScope::Inner)?);
    }
    if scenario == "all" || scenario == "text-object-sentence-around" {
        results.push(benchmark_sentence_text_object(TextObjectScope::Around)?);
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

fn benchmark_prefix_word_operator(change_word: bool) -> Result<serde_json::Value> {
    let suffix = "ordinary_identifier and retained editor source ".repeat(512);
    let buffer = Buffer::new(None, format!("target_word  {suffix}"));
    let resolver = MotionResolver::new(&buffer, TextPosition::new(0, 0));
    let started = Instant::now();
    for _ in 0..WORD_OPERATOR_LOOKUPS {
        let range = resolver
            .word_range(/*count*/ 1, change_word, /*big_word*/ false)
            .ok_or_else(|| anyhow::anyhow!("Vim word operator lost its selected range"))?;
        anyhow::ensure!(
            range.end.character == if change_word { 11 } else { 13 },
            "Vim word operator changed its trailing-whitespace semantics"
        );
        black_box(range);
    }
    Ok(report(
        if change_word {
            "shared_vim_ascii_change_word_operator"
        } else {
            "shared_vim_ascii_delete_word_operator"
        },
        started,
        WORD_OPERATOR_LOOKUPS,
    ))
}

fn benchmark_counted_word_operator(change_word: bool) -> Result<serde_json::Value> {
    let suffix = "ordinary_identifier and retained editor source ".repeat(512);
    let buffer = Buffer::new(None, format!("first second third fourth  {suffix}"));
    let resolver = MotionResolver::new(&buffer, TextPosition::new(0, 0));
    let started = Instant::now();
    for _ in 0..WORD_OPERATOR_LOOKUPS {
        let range = resolver
            .word_range(/*count*/ 4, change_word, /*big_word*/ false)
            .ok_or_else(|| anyhow::anyhow!("counted Vim operator lost its selected range"))?;
        anyhow::ensure!(
            range.end.character == if change_word { 25 } else { 27 },
            "counted Vim operator changed its selected words or trailing whitespace"
        );
        black_box(range);
    }
    Ok(report(
        if change_word {
            "shared_vim_counted_change_word_operator"
        } else {
            "shared_vim_counted_delete_word_operator"
        },
        started,
        WORD_OPERATOR_LOOKUPS,
    ))
}

fn benchmark_editor_word_end_operator(backward: bool) -> Result<serde_json::Value> {
    let surrounding = "ordinary_identifier ".repeat(1_024);
    let contents = if backward {
        format!("target_identifier {surrounding}")
    } else {
        format!("{surrounding}target_identifier")
    };
    let cursor = if backward { 5 } else { contents.len() - 1 };
    let mut config = Config::default();
    config.lsp.enabled = false;
    let mut editor = Editor::with_size(
        Box::new(LspManager::new(config.lsp.clone())),
        120,
        40,
        config,
        Theme::default(),
        vec![Buffer::new(None, contents)],
    )?;
    editor.test_set_viewport_cursor(/*vtop*/ 0, cursor, /*cy*/ 0);
    let started = Instant::now();
    for _ in 0..WORD_OPERATOR_LOOKUPS {
        let range = editor
            .benchmark_word_end_operator(backward, /*big_word*/ false)
            .ok_or_else(|| anyhow::anyhow!("editor word-end operator lost its range"))?;
        anyhow::ensure!(
            range.end.character == cursor + 1 && (!backward || range.start.character == 0),
            "editor word-end operator changed its document boundary"
        );
        black_box(range);
    }
    Ok(report(
        if backward {
            "editor_backward_word_end_boundary_operator"
        } else {
            "editor_forward_word_end_boundary_operator"
        },
        started,
        WORD_OPERATOR_LOOKUPS,
    ))
}

fn benchmark_editor_cursor_conversion(word_search: bool) -> Result<serde_json::Value> {
    let contents = "ordinary_identifier ".repeat(1_024);
    let cursor = contents.len() - 1;
    let mut config = Config::default();
    config.lsp.enabled = false;
    let editor = Editor::with_size(
        Box::new(LspManager::new(config.lsp.clone())),
        120,
        40,
        config,
        Theme::default(),
        vec![Buffer::new(None, contents)],
    )?;
    let started = Instant::now();
    for _ in 0..TEXTAREA_VIM_MOTIONS {
        let converted = editor.benchmark_cursor_conversion(cursor, /*line*/ 0, word_search);
        anyhow::ensure!(
            converted == cursor - usize::from(word_search),
            "editor cursor conversion changed its scalar position or whitespace adjustment"
        );
        black_box(converted);
    }
    Ok(report(
        if word_search {
            "editor_next_word_search_cursor_conversion"
        } else {
            "editor_scalar_to_grapheme_cursor_conversion"
        },
        started,
        TEXTAREA_VIM_MOTIONS,
    ))
}

fn benchmark_editor_line_boundary(last_cell: bool) -> Result<serde_json::Value> {
    let contents = "ordinary_identifier ".repeat(1_024);
    let expected = contents.len() - usize::from(last_cell);
    let mut config = Config::default();
    config.lsp.enabled = false;
    let editor = Editor::with_size(
        Box::new(LspManager::new(config.lsp.clone())),
        120,
        40,
        config,
        Theme::default(),
        vec![Buffer::new(None, contents)],
    )?;
    let started = Instant::now();
    for _ in 0..TEXTAREA_VIM_MOTIONS {
        let boundary = editor.benchmark_line_boundary(/*line*/ 0, last_cell);
        anyhow::ensure!(
            boundary == expected,
            "editor line boundary changed its navigable width"
        );
        black_box(boundary);
    }
    Ok(report(
        if last_cell {
            "editor_ascii_final_cell_lookup"
        } else {
            "editor_ascii_logical_line_length"
        },
        started,
        TEXTAREA_VIM_MOTIONS,
    ))
}

fn benchmark_editor_cursor_position(
    language_server: bool,
    window_snapshot: bool,
) -> Result<serde_json::Value> {
    let contents = "ordinary_identifier ".repeat(1_024);
    let cursor = contents.len() - 1;
    let mut config = Config::default();
    config.lsp.enabled = false;
    let mut editor = Editor::with_size(
        Box::new(LspManager::new(config.lsp.clone())),
        120,
        40,
        config,
        Theme::default(),
        vec![Buffer::new(None, contents)],
    )?;
    editor.test_set_viewport_cursor(/*vtop*/ 0, cursor, /*cy*/ 0);
    let started = Instant::now();
    for _ in 0..TEXTAREA_VIM_MOTIONS {
        let position = if language_server {
            editor.benchmark_lsp_cursor_character(window_snapshot)
        } else {
            editor.benchmark_cursor_display_column()
        };
        anyhow::ensure!(
            position == cursor,
            "editor cursor position changed its display column or UTF-16 offset"
        );
        black_box(position);
    }
    Ok(report(
        if !language_server {
            "editor_ascii_cursor_display_column"
        } else if window_snapshot {
            "editor_ascii_window_lsp_cursor_character"
        } else {
            "editor_ascii_lsp_cursor_position"
        },
        started,
        TEXTAREA_VIM_MOTIONS,
    ))
}

fn benchmark_editor_scalar_line_boundary(
    unicode: bool,
    operator: bool,
) -> Result<serde_json::Value> {
    let contents = if unicode {
        "identifiant_λ👋 ".repeat(1_024)
    } else {
        "ordinary_identifier ".repeat(1_024)
    };
    let expected = contents.chars().count();
    let mut config = Config::default();
    config.lsp.enabled = false;
    let editor = Editor::with_size(
        Box::new(LspManager::new(config.lsp.clone())),
        120,
        40,
        config,
        Theme::default(),
        vec![Buffer::new(None, contents)],
    )?;
    let started = Instant::now();
    for _ in 0..TEXTAREA_VIM_MOTIONS {
        let boundary = editor.benchmark_scalar_line_boundary(operator);
        anyhow::ensure!(
            boundary == expected,
            "editor scalar boundary changed its Unicode position or Vim line-end range"
        );
        black_box(boundary);
    }
    Ok(report(
        match (unicode, operator) {
            (false, false) => "editor_ascii_scalar_line_length",
            (true, false) => "editor_unicode_scalar_line_length",
            (false, true) => "editor_ascii_vim_line_end_operator",
            (true, true) => "editor_unicode_vim_line_end_operator",
        },
        started,
        TEXTAREA_VIM_MOTIONS,
    ))
}

fn benchmark_editor_rename_symbol(unicode: bool) -> Result<serde_json::Value> {
    let symbol = if unicode {
        "λvariable終"
    } else {
        "ordinary_identifier"
    };
    let contents = format!("{symbol} ").repeat(1_024);
    let cursor = symbol.chars().count().saturating_sub(1);
    let mut config = Config::default();
    config.lsp.enabled = false;
    let mut editor = Editor::with_size(
        Box::new(LspManager::new(config.lsp.clone())),
        120,
        40,
        config,
        Theme::default(),
        vec![Buffer::new(None, contents)],
    )?;
    editor.test_set_viewport_cursor(/*vtop*/ 0, cursor, /*cy*/ 0);
    let started = Instant::now();
    for _ in 0..WORD_OPERATOR_LOOKUPS {
        let actual = editor.benchmark_rename_symbol();
        anyhow::ensure!(
            actual == symbol,
            "editor rename extraction changed its symbol or Unicode character boundaries"
        );
        black_box(actual);
    }
    Ok(report(
        if unicode {
            "editor_unicode_lsp_rename_symbol_extraction"
        } else {
            "editor_ascii_lsp_rename_symbol_extraction"
        },
        started,
        WORD_OPERATOR_LOOKUPS,
    ))
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

fn benchmark_inline_answer_streaming() -> Result<serde_json::Value> {
    let range = TextRange::insertion(TextPosition::default());
    let template: InlineHistoryTurn = serde_json::from_value(json!({
        "request_id": "request-0000",
        "created_at_ms": 0,
        "prompt": "Explain this function",
        "before": "fn example() {}",
        "original_range": range,
        "location": {
            "file": "src/main.rs",
            "range": range,
            "start_char": 0,
            "end_char": 0
        },
        "state": "pending",
        "result": null
    }))?;
    let mut history = InlineHistory {
        conversations: (0..INLINE_CONVERSATIONS)
            .map(|index| {
                let mut turn = template.clone();
                turn.request_id = format!("request-{index:04}");
                InlineConversation {
                    id: format!("conversation-{index:04}"),
                    cwd: "/workspace".to_string(),
                    file: "src/main.rs".to_string(),
                    turns: vec![turn],
                    resolved: false,
                    visible_request: None,
                }
            })
            .collect(),
        ..InlineHistory::default()
    };
    let request = format!("request-{:04}", INLINE_CONVERSATIONS - 1);

    let started = Instant::now();
    for _ in 0..INLINE_ANSWER_DELTAS {
        history.append_answer(black_box(&request), black_box("streamed response "));
    }
    anyhow::ensure!(
        history.turn(&request).is_some_and(|turn| {
            turn.answer.len() == INLINE_ANSWER_DELTAS * "streamed response ".len()
        }),
        "inline streaming benchmark did not retain every answer delta"
    );
    Ok(report(
        "inline_assistance_answer_streaming",
        started,
        INLINE_ANSWER_DELTAS,
    ))
}

fn benchmark_statusline_rendering() -> Result<serde_json::Value> {
    let directory = tempfile::tempdir()?;
    std::fs::create_dir(directory.path().join(".git"))?;
    std::fs::write(
        directory.path().join(".git/HEAD"),
        "ref: refs/heads/performance-benchmark\n",
    )?;
    let path = directory.path().join("main.rs");
    let contents = "fn example() { println!(\"statusline\"); }\n";
    std::fs::write(&path, contents)?;
    let config = Config::default();
    let mut editor = Editor::with_size(
        Box::new(LspManager::new(config.lsp.clone())),
        120,
        40,
        config,
        Theme::default(),
        vec![Buffer::new(
            Some(path.to_string_lossy().into_owned()),
            contents.to_string(),
        )],
    )?;
    let mut buffer = RenderBuffer::new(120, 40, &Style::default());
    editor.draw_statusline(&mut buffer);
    let frames = std::env::var("RED_STATUSLINE_BENCH_FRAMES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(STATUSLINE_FRAMES);

    let started = Instant::now();
    for _ in 0..frames {
        editor.draw_statusline(black_box(&mut buffer));
    }
    black_box(buffer.cells.len());
    Ok(report(
        "default_editor_statusline_rendering",
        started,
        frames,
    ))
}

fn benchmark_lsp_document_routing() -> Result<serde_json::Value> {
    let directory = tempfile::tempdir()?;
    std::fs::write(
        directory.path().join("Cargo.toml"),
        "[package]\nname = 'bench'\n",
    )?;
    let source = directory.path().join("src");
    std::fs::create_dir(&source)?;
    let path = source.join("main.rs");
    std::fs::write(&path, "fn main() {}\n")?;
    let path = path.to_string_lossy();
    let manager = LspManager::new(Config::default().lsp);
    anyhow::ensure!(
        manager.resolve_document(&path).is_some(),
        "LSP routing benchmark could not resolve its Rust document"
    );

    let started = Instant::now();
    for _ in 0..LSP_DOCUMENT_RESOLVES {
        black_box(manager.resolve_document(black_box(&path)));
    }
    Ok(report(
        "lsp_absolute_document_routing",
        started,
        LSP_DOCUMENT_RESOLVES,
    ))
}

fn benchmark_session_buffer_restoration() -> Result<serde_json::Value> {
    let directory = tempfile::tempdir()?;
    let mut buffers = Vec::with_capacity(RECOVERY_BUFFERS);
    for index in 0..RECOVERY_BUFFERS {
        let path = directory.path().join(format!("recovered-{index:03}.rs"));
        let contents = format!("fn recovered_{index}() {{}}\n");
        std::fs::write(&path, &contents)?;
        buffers.push(Buffer::new(
            Some(path.to_string_lossy().into_owned()),
            contents,
        ));
    }
    let mut config = Config::default();
    config.lsp.enabled = false;
    let mut editor = Editor::with_size(
        Box::new(LspManager::new(config.lsp.clone())),
        120,
        40,
        config,
        Theme::default(),
        buffers,
    )?;
    let snapshot = editor.test_session_snapshot();

    let started = Instant::now();
    for _ in 0..RECOVERY_RESTORES {
        let recovered = Editor::buffers_from_session_snapshot(black_box(&snapshot));
        anyhow::ensure!(
            recovered.len() == RECOVERY_BUFFERS,
            "session benchmark did not recover all file-backed buffers"
        );
        black_box(recovered);
    }
    Ok(report(
        "crash_recovery_buffer_restoration",
        started,
        RECOVERY_RESTORES,
    ))
}

fn benchmark_workspace_file_discovery() -> Result<serde_json::Value> {
    let directory = workspace_search_fixture()?;
    let root = directory.path();
    let expected_files = WORKSPACE_SEARCH_DIRECTORIES * WORKSPACE_SEARCH_FILES_PER_DIRECTORY + 1;
    let started = Instant::now();
    for _ in 0..WORKSPACE_SEARCH_LISTINGS {
        let listed = red::inline_context::benchmark_workspace_file_listing(black_box(root))?;
        let files = listed["files"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("workspace benchmark returned invalid files"))?;
        anyhow::ensure!(
            files.len() == expected_files,
            "workspace benchmark returned {} files instead of {expected_files}",
            files.len()
        );
        for forbidden in [
            "ignored.rs",
            ".env",
            "secrets/token.rs",
            ".git/config",
            "link.rs",
        ] {
            anyhow::ensure!(
                !files.iter().any(|file| file.as_str() == Some(forbidden)),
                "workspace benchmark disclosed restricted file {forbidden}"
            );
        }
        black_box(listed);
    }
    Ok(report(
        "workspace_inline_file_discovery",
        started,
        WORKSPACE_SEARCH_LISTINGS,
    ))
}

fn benchmark_workspace_content_search() -> Result<serde_json::Value> {
    let directory = workspace_search_fixture()?;
    let root = directory.path();
    let started = Instant::now();
    for _ in 0..WORKSPACE_CONTENT_SEARCHES {
        let result = red::inline_context::benchmark_workspace_content_search(
            black_box(root),
            black_box("source_7_31"),
        )?;
        let matches = result["matches"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("workspace search returned invalid matches"))?;
        anyhow::ensure!(
            matches.len() == 1
                && matches[0]["path"] == "module-07/source-031.rs"
                && matches[0]["source"] == "disk",
            "workspace search did not return its exact final source file"
        );
        black_box(result);
    }
    Ok(report(
        "workspace_inline_content_search",
        started,
        WORKSPACE_CONTENT_SEARCHES,
    ))
}

fn workspace_search_fixture() -> Result<tempfile::TempDir> {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    std::fs::create_dir(root.join(".git"))?;
    std::fs::write(root.join(".git/config"), "private = true\n")?;
    std::fs::write(root.join(".gitignore"), "ignored.rs\n")?;
    std::fs::write(root.join("ignored.rs"), "ignored\n")?;
    std::fs::write(root.join(".env"), "TOKEN=private\n")?;
    std::fs::create_dir(root.join("secrets"))?;
    std::fs::write(root.join("secrets/token.rs"), "private\n")?;
    for directory_index in 0..WORKSPACE_SEARCH_DIRECTORIES {
        let child = root.join(format!("module-{directory_index:02}"));
        std::fs::create_dir(&child)?;
        for file_index in 0..WORKSPACE_SEARCH_FILES_PER_DIRECTORY {
            std::fs::write(
                child.join(format!("source-{file_index:03}.rs")),
                format!("fn source_{directory_index}_{file_index}() {{}}\n"),
            )?;
        }
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("module-00/source-000.rs"), root.join("link.rs"))?;
    Ok(directory)
}

fn benchmark_plugin_event_delivery() -> Result<serde_json::Value> {
    let directory = tempfile::tempdir()?;
    let listener = directory.path().join("listener.hk");
    let background = directory.path().join("background.hk");
    std::fs::write(
        &listener,
        "pub fn activate() { red::on(\"cursor:moved\", observe); }\nfn observe(event: Json) {}\n",
    )?;
    std::fs::write(&background, "pub fn activate() {}\n")?;
    let mut plugins = PluginRegistry::new();
    plugins.add("listener", &listener.to_string_lossy());
    for index in 0..PLUGIN_EVENT_BACKGROUND {
        plugins.add(
            &format!("background-{index:03}"),
            &background.to_string_lossy(),
        );
    }
    let mut runtime = Runtime::try_new()?;
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    executor.block_on(plugins.initialize(&mut runtime))?;
    anyhow::ensure!(
        plugins
            .statuses()
            .values()
            .all(|status| matches!(status, PluginStatus::Active)),
        "plugin event benchmark failed to activate all plugins"
    );
    let payload = json!({ "x": 4, "y": 8 });

    let started = Instant::now();
    executor.block_on(async {
        for _ in 0..PLUGIN_EVENT_DELIVERIES {
            plugins
                .notify(
                    &mut runtime,
                    black_box("cursor:moved"),
                    black_box(payload.clone()),
                )
                .await?;
        }
        Ok::<(), anyhow::Error>(())
    })?;
    anyhow::ensure!(
        matches!(
            plugins.statuses().get("listener"),
            Some(PluginStatus::Active)
        ),
        "plugin event benchmark quarantined its listener"
    );
    Ok(report(
        "plugin_cursor_event_delivery",
        started,
        PLUGIN_EVENT_DELIVERIES,
    ))
}

fn benchmark_session_snapshot_writes() -> Result<serde_json::Value> {
    let directory = tempfile::tempdir()?;
    let mut buffers = Vec::with_capacity(SNAPSHOT_WRITE_BUFFERS);
    for buffer_index in 0..SNAPSHOT_WRITE_BUFFERS {
        let mut buffer = Buffer::new(
            None,
            format!("buffer {buffer_index} retained recovery contents\n").repeat(96),
        );
        for node_index in 0..SNAPSHOT_WRITE_UNDO_NODES {
            buffer
                .undo_history
                .begin_transaction("insert", CursorSnapshot::default());
            buffer.undo_history.record_replace(
                TextRange::insertion(TextPosition::new(0, node_index)),
                node_index,
                String::new(),
                format!("retained undo payload {buffer_index}:{node_index} ").repeat(8),
            );
            buffer
                .undo_history
                .commit_transaction(CursorSnapshot::default());
        }
        buffers.push(buffer);
    }
    let mut config = Config::default();
    config.lsp.enabled = false;
    let mut editor = Editor::with_size(
        Box::new(LspManager::new(config.lsp.clone())),
        120,
        40,
        config,
        Theme::default(),
        buffers,
    )?;
    let mut snapshot = editor.test_session_snapshot();
    let store = SessionStore::new(directory.path().join("recovery"));
    store.write(&mut snapshot)?;

    let started = Instant::now();
    for _ in 0..SNAPSHOT_WRITES {
        store.write(black_box(&mut snapshot))?;
    }
    let restored = store.load()?;
    anyhow::ensure!(
        restored.buffers.len() == SNAPSHOT_WRITE_BUFFERS
            && restored.generation == (SNAPSHOT_WRITES + 1) as u64,
        "snapshot benchmark failed to retain every buffer and generation"
    );
    Ok(report(
        "crash_recovery_snapshot_writes",
        started,
        SNAPSHOT_WRITES,
    ))
}

fn benchmark_full_editor_frames() -> Result<serde_json::Value> {
    let contents = (0..128)
        .map(|line| {
            format!(
                "fn editor_frame_line_{line:03}(value: usize) -> usize {{ value.saturating_add({line}) }} // complete frame composition"
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut config = Config::default();
    config.lsp.enabled = false;
    config.splash = Some(false);
    let mut editor = Editor::with_size(
        Box::new(LspManager::new(config.lsp.clone())),
        160,
        48,
        config,
        Theme::default(),
        vec![Buffer::new(None, contents)],
    )?;
    editor.test_disable_terminal_output();
    let mut buffer = RenderBuffer::new(160, 48, &Style::default());
    editor.render(&mut buffer)?;

    let started = Instant::now();
    for _ in 0..FULL_FRAME_RENDERS {
        editor.render(black_box(&mut buffer))?;
    }
    anyhow::ensure!(
        buffer.cells.iter().any(|cell| cell.text == "f"),
        "full-frame benchmark did not preserve source cells"
    );
    Ok(report(
        "complete_editor_frame_composition",
        started,
        FULL_FRAME_RENDERS,
    ))
}

fn benchmark_git_repository_discovery() -> Result<serde_json::Value> {
    let repository = tempfile::tempdir()?;
    std::fs::create_dir(repository.path().join(".git"))?;
    std::fs::write(
        repository.path().join(".git/HEAD"),
        "ref: refs/heads/performance-discovery\n",
    )?;
    let mut nested = repository.path().to_path_buf();
    for depth in 0..GIT_DISCOVERY_DEPTH {
        nested.push(format!("module-{depth:02}"));
    }
    std::fs::create_dir_all(&nested)?;
    let file = nested.join("main.rs");
    std::fs::write(&file, "fn main() {}\n")?;
    let file = file.to_string_lossy().into_owned();
    let mut config = Config::default();
    config.lsp.enabled = false;
    let mut editor = Editor::with_size(
        Box::new(LspManager::new(config.lsp.clone())),
        120,
        40,
        config,
        Theme::default(),
        vec![Buffer::new(Some(file.clone()), "fn main() {}\n".into())],
    )?;
    let expected = repository.path().canonicalize()?;
    let discovered = editor
        .benchmark_git_repository_discovery(&file)
        .ok_or_else(|| anyhow::anyhow!("Git discovery benchmark did not find its repository"))?;
    anyhow::ensure!(
        discovered.0 == expected && discovered.1 == "performance-discovery",
        "Git discovery benchmark found the wrong repository or branch"
    );

    let started = Instant::now();
    for _ in 0..GIT_REPOSITORY_DISCOVERIES {
        black_box(editor.benchmark_git_repository_discovery(black_box(&file)));
    }
    Ok(report(
        "git_repository_discovery_and_branch_refresh",
        started,
        GIT_REPOSITORY_DISCOVERIES,
    ))
}

fn benchmark_startup_file_loading() -> Result<serde_json::Value> {
    let directory = tempfile::tempdir()?;
    let mut files = Vec::with_capacity(STARTUP_FILE_COUNT);
    for index in 0..STARTUP_FILE_COUNT {
        let path = directory.path().join(format!("source-{index:03}.rs"));
        std::fs::write(
            &path,
            format!("fn source_{index}() -> usize {{ {index} }}\n"),
        )?;
        files.push(path.to_string_lossy().into_owned());
    }
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let started = Instant::now();
    for _ in 0..STARTUP_FILE_LOADS {
        let buffers = executor.block_on(red::buffer::load_startup_buffers(black_box(&files)))?;
        anyhow::ensure!(
            buffers.len() == STARTUP_FILE_COUNT,
            "startup file benchmark lost or duplicated a source buffer"
        );
        black_box(buffers);
    }
    Ok(report(
        "multi_file_process_startup_loading",
        started,
        STARTUP_FILE_LOADS,
    ))
}

fn benchmark_lsp_incremental_changes() -> Result<serde_json::Value> {
    let before = (0..LSP_DOCUMENT_LINES)
        .map(|line| {
            format!(
                "fn language_service_line_{line:04}(value: usize) -> usize {{ value.saturating_add({line}) }} // 👋 document synchronization\n"
            )
        })
        .collect::<String>();
    let target = format!("language_service_line_{:04}", LSP_DOCUMENT_LINES * 3 / 4);
    let insertion = before
        .find(&target)
        .ok_or_else(|| anyhow::anyhow!("LSP document benchmark target line was not found"))?
        + target.len();
    let mut after = before.clone();
    after.insert_str(insertion, "_λ👋");

    let started = Instant::now();
    for _ in 0..LSP_INCREMENTAL_CHANGES {
        let changes = RealLspClient::benchmark_incremental_document_change(
            black_box(&before),
            black_box(&after),
        );
        anyhow::ensure!(
            changes.len() == 1 && changes[0].text == "_λ👋",
            "LSP document benchmark produced an incorrect incremental change"
        );
        black_box(changes);
    }
    Ok(report(
        "lsp_incremental_large_document_changes",
        started,
        LSP_INCREMENTAL_CHANGES,
    ))
}

fn benchmark_embedded_textarea_typing() -> Result<serde_json::Value> {
    let mut area = TextArea::new("a".repeat(TEXTAREA_INITIAL_BYTES));
    anyhow::ensure!(
        area.cursor() == TEXTAREA_INITIAL_BYTES,
        "text-area benchmark did not position the insertion cursor"
    );

    let started = Instant::now();
    for _ in 0..TEXTAREA_INSERTIONS {
        anyhow::ensure!(
            area.insert(black_box("x")),
            "text-area benchmark unexpectedly rejected an insertion"
        );
    }
    anyhow::ensure!(
        area.cursor() == TEXTAREA_INITIAL_BYTES + TEXTAREA_INSERTIONS
            && area.text().len() == TEXTAREA_INITIAL_BYTES + TEXTAREA_INSERTIONS,
        "text-area benchmark lost inserted text or cursor state"
    );
    Ok(report(
        "embedded_text_area_ascii_typing",
        started,
        TEXTAREA_INSERTIONS,
    ))
}

fn benchmark_startup_configuration_loading() -> Result<serde_json::Value> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("config.toml");
    std::fs::write(
        &path,
        r#"wrap = false
scrolloff = 4

[search]
incsearch = true
hlsearch = true
wrapscan = true
ignorecase = true
smartcase = false

[completion]
enabled = true
auto_trigger = false
min_prefix_length = 3
debounce_ms = 40
buffer_words = true
max_buffer_words = 2048

[signature_help]
auto_trigger = false
debounce_ms = 120
show_documentation = false

[formatting]
on_save = false
trim_trailing_whitespace = false

[key_hints]
enabled = false
delay_ms = 75

[clipboard]
enabled = false
sync_on_yank = false
sync_on_paste = false
"#,
    )?;

    let started = Instant::now();
    for _ in 0..STARTUP_CONFIG_LOADS {
        let loaded = Config::load_user_file(black_box(&path), &[])?;
        anyhow::ensure!(
            loaded.is_clean()
                && loaded.config.scrolloff == Some(4)
                && !loaded.config.completion.auto_trigger
                && loaded.config.completion.max_buffer_words == 2048
                && !loaded.config.clipboard.enabled,
            "startup configuration benchmark lost a valid user setting"
        );
        black_box(loaded);
    }
    Ok(report(
        "startup_user_configuration_loading",
        started,
        STARTUP_CONFIG_LOADS,
    ))
}

fn benchmark_startup_theme_loading() -> Result<serde_json::Value> {
    let contents = red::assets::bundled_theme("red.json")
        .ok_or_else(|| anyhow::anyhow!("embedded default theme is missing"))?;

    let started = Instant::now();
    for _ in 0..STARTUP_THEME_LOADS {
        let theme = parse_vscode_theme_contents(black_box(contents))?;
        anyhow::ensure!(
            theme.name == "red" && !theme.token_styles.is_empty() && !theme.colors.is_empty(),
            "startup theme benchmark lost bundled workbench or syntax colors"
        );
        black_box(theme);
    }
    Ok(report(
        "startup_bundled_theme_loading",
        started,
        STARTUP_THEME_LOADS,
    ))
}

fn benchmark_theme_color_parsing() -> Result<serde_json::Value> {
    let colors = ["#101014", "#D8D8DE", "#E5484D", "#2B2B36C0"];

    let started = Instant::now();
    for index in 0..THEME_COLOR_PARSES {
        black_box(red::color::parse_rgb(black_box(
            colors[index % colors.len()],
        ))?);
    }
    Ok(report(
        "theme_hex_color_parsing",
        started,
        THEME_COLOR_PARSES,
    ))
}

fn benchmark_ascii_grapheme_counting() -> serde_json::Value {
    let contents = "ordinary ASCII editor line and cursor text\n".repeat(768);

    let started = Instant::now();
    for _ in 0..ASCII_GRAPHEME_COUNTS {
        black_box(red::unicode_utils::grapheme_len(black_box(&contents)));
    }
    report(
        "ascii_editor_grapheme_counting",
        started,
        ASCII_GRAPHEME_COUNTS,
    )
}

fn benchmark_embedded_textarea_loading() -> Result<serde_json::Value> {
    let contents = "ordinary ASCII composer draft line\n".repeat(768);

    let started = Instant::now();
    for _ in 0..TEXTAREA_DOCUMENT_LOADS {
        let area = TextArea::new(black_box(&contents));
        anyhow::ensure!(
            area.cursor() == contents.len() && area.buffer().contents().len() == contents.len(),
            "embedded textarea loading lost draft contents or cursor position"
        );
        black_box(area);
    }
    Ok(report(
        "embedded_text_area_document_loading",
        started,
        TEXTAREA_DOCUMENT_LOADS,
    ))
}

fn benchmark_git_status_indexing() -> Result<serde_json::Value> {
    const ROOT: &str = "/workspace/repository";
    let statuses = (0..GIT_STATUS_FILES)
        .map(|index| {
            let path = format!(
                "src/workspace/crate-{:02}/module/deep/source-{index:04}.rs",
                index % 16
            );
            let status = if index % 127 == 0 {
                "conflict"
            } else if index % 17 == 0 {
                "ignored"
            } else if index % 7 == 0 {
                "untracked"
            } else {
                "modified"
            };
            json!({
                "path": path,
                "absolute_path": format!("{ROOT}/{path}"),
                "status": status,
            })
        })
        .collect::<Vec<_>>();

    let started = Instant::now();
    for _ in 0..GIT_STATUS_INDEX_BUILDS {
        let index = red::editor::git_status_index(black_box(&statuses), black_box(ROOT));
        anyhow::ensure!(
            index["/workspace/repository/src"] == "conflict"
                && index["/workspace/repository/src/workspace/crate-00/module/deep/source-0000.rs"]
                    == "conflict",
            "Git status indexing lost conflict precedence or changed files"
        );
        black_box(index);
    }
    Ok(report(
        "git_workspace_status_directory_indexing",
        started,
        GIT_STATUS_INDEX_BUILDS,
    ))
}

fn benchmark_git_status_refresh(repository: bool) -> Result<serde_json::Value> {
    let directory = tempfile::tempdir()?;
    let nested = directory.path().join("src/workspace/nested");
    std::fs::create_dir_all(&nested)?;
    if repository {
        let initialized = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(directory.path())
            .output()?;
        anyhow::ensure!(
            initialized.status.success(),
            "Git fixture initialization failed"
        );
        std::fs::write(directory.path().join(".gitignore"), "ignored.log\n")?;
        std::fs::write(directory.path().join("tracked.rs"), "original contents\n")?;
        let staged = std::process::Command::new("git")
            .arg("-C")
            .arg(directory.path())
            .args(["add", ".gitignore", "tracked.rs"])
            .output()?;
        anyhow::ensure!(staged.status.success(), "Git fixture staging failed");
        std::fs::write(directory.path().join("tracked.rs"), "modified contents\n")?;
        std::fs::write(directory.path().join("ignored.log"), "ignored contents\n")?;
        std::fs::write(nested.join("untracked.rs"), "untracked contents\n")?;
    }
    let path = nested.to_string_lossy();
    let started = Instant::now();
    for _ in 0..GIT_STATUS_REFRESHES {
        let listing = red::editor::git_status_listing(black_box(&path));
        if repository {
            anyhow::ensure!(
                listing["root"].is_string()
                    && listing["statuses"]
                        .as_array()
                        .is_some_and(|statuses| statuses
                            .iter()
                            .any(|status| status["status"] == "ignored")),
                "Git refresh lost the repository root or ignored status entries"
            );
        } else {
            anyhow::ensure!(
                listing["root"].is_null()
                    && listing["statuses"].as_array().is_some_and(Vec::is_empty)
                    && listing["error"].is_null(),
                "Git refresh fabricated repository status outside a repository"
            );
        }
        black_box(listing);
    }
    Ok(report(
        if repository {
            "git_repository_subprocess_status_refresh"
        } else {
            "git_non_repository_status_refresh"
        },
        started,
        GIT_STATUS_REFRESHES,
    ))
}

fn benchmark_buffer_line_boundary(count: bool) -> Result<serde_json::Value> {
    let final_line = "ordinary source identifier and retained editor words ".repeat(1_024);
    let buffer = Buffer::new(None, format!("first line\n{final_line}"));
    let started = Instant::now();
    for _ in 0..BUFFER_LINE_LOOKUPS {
        let line = if count {
            black_box(&buffer).navigable_line_count()
        } else {
            black_box(&buffer).last_navigable_line()
        };
        anyhow::ensure!(
            line == if count { 2 } else { 1 },
            "buffer line lookup changed its final-line boundary"
        );
        black_box(line);
    }
    Ok(report(
        if count {
            "shared_buffer_navigable_line_count"
        } else {
            "shared_buffer_last_navigable_line"
        },
        started,
        BUFFER_LINE_LOOKUPS,
    ))
}

fn benchmark_sparse_regex_matches(unicode: bool) -> Result<serde_json::Value> {
    let gap = if unicode {
        "漢字 👋 e\u{301} retained ordinary editor source\r\n"
    } else {
        "ordinary ASCII retained editor source contents\n"
    };
    let contents = format!(
        "{}needle_target\n{}needle_target",
        gap.repeat(1_024),
        gap.repeat(1_024)
    );
    let buffer = Buffer::new(None, contents);
    let expression = regex::Regex::new("needle_target")?;
    let started = Instant::now();
    for _ in 0..SPARSE_REGEX_SEARCHES {
        let matches = buffer.regex_matches(black_box(&expression));
        anyhow::ensure!(
            matches.len() == 2 && matches[0].start_y == 1_024 && matches[1].start_y == 2_049,
            "sparse regex search changed match coordinates"
        );
        black_box(matches);
    }
    Ok(report(
        if unicode {
            "shared_buffer_sparse_unicode_regex_matches"
        } else {
            "shared_buffer_sparse_ascii_regex_matches"
        },
        started,
        SPARSE_REGEX_SEARCHES,
    ))
}

fn benchmark_layout_cursor_lookup(mode: WrapMode) -> Result<serde_json::Value> {
    let contents = "ordinary editor word and wrapped composer text ".repeat(512);
    let options = match mode {
        WrapMode::Grapheme => LayoutOptions::grapheme(64),
        WrapMode::Word => LayoutOptions::word(64),
    };
    let layout = TextLayout::new(&contents, options);
    anyhow::ensure!(layout.rows().len() > 256, "layout benchmark did not wrap");

    let started = Instant::now();
    for index in 0..LAYOUT_CURSOR_LOOKUPS {
        let row = (index * 17) % layout.rows().len();
        let column = (index * 13) % 64;
        let offset = layout
            .nearest_offset_on_row(black_box(row), black_box(column))
            .ok_or_else(|| anyhow::anyhow!("layout cursor lookup lost a visible row"))?;
        black_box(offset);
    }
    Ok(report(
        if mode == WrapMode::Grapheme {
            "grapheme_wrapped_layout_cursor_lookup"
        } else {
            "word_wrapped_layout_cursor_lookup"
        },
        started,
        LAYOUT_CURSOR_LOOKUPS,
    ))
}

fn benchmark_embedded_vim_motion(line_motion: bool) -> Result<serde_json::Value> {
    let line = "ordinary_identifier another_editor_word and more ascii content\n";
    let mut area = TextArea::new(line.repeat(512));
    let origin = line.len() * 256;
    area.set_mode(Mode::Normal);
    area.set_cursor(origin);
    let first = Event::Key(KeyEvent::new(
        KeyCode::Char(if line_motion { '$' } else { 'w' }),
        KeyModifiers::NONE,
    ));
    let second = Event::Key(KeyEvent::new(
        KeyCode::Char(if line_motion { '0' } else { 'b' }),
        KeyModifiers::NONE,
    ));

    let started = Instant::now();
    for index in 0..TEXTAREA_VIM_MOTIONS {
        let event = if index % 2 == 0 { &first } else { &second };
        anyhow::ensure!(
            area.handle_event(black_box(event), 120) == red::editing::TextAreaOutcome::Changed,
            "embedded Vim motion was not handled"
        );
        black_box(area.cursor());
    }
    anyhow::ensure!(
        area.cursor() == origin && area.mode() == Mode::Normal,
        "embedded Vim motion lost its cursor or Normal mode"
    );
    Ok(report(
        if line_motion {
            "embedded_vim_line_boundary_motions"
        } else {
            "embedded_vim_word_motions"
        },
        started,
        TEXTAREA_VIM_MOTIONS,
    ))
}

fn benchmark_embedded_vim_delimiter_motion() -> Result<serde_json::Value> {
    let prefix = "ordinary editor line and retained source text\n".repeat(512);
    let source = format!(
        "{prefix}fn (value (nested) [item] {{entry}}) tail\n{}",
        "remaining editor source and ordinary words\n".repeat(512)
    );
    let origin = prefix.len() + "fn ".len();
    let mut area = TextArea::new(source);
    area.set_mode(Mode::Normal);
    area.set_cursor(origin);
    let event = Event::Key(KeyEvent::new(KeyCode::Char('%'), KeyModifiers::NONE));

    let started = Instant::now();
    for _ in 0..TEXTAREA_DELIMITER_MOTIONS {
        anyhow::ensure!(
            area.handle_event(black_box(&event), 120) == red::editing::TextAreaOutcome::Changed,
            "embedded Vim delimiter motion was not handled"
        );
        black_box(area.cursor());
    }
    anyhow::ensure!(
        area.cursor() == origin && area.mode() == Mode::Normal,
        "embedded Vim delimiter motion lost its original cursor"
    );
    Ok(report(
        "embedded_vim_nested_delimiter_motions",
        started,
        TEXTAREA_DELIMITER_MOTIONS,
    ))
}

fn benchmark_embedded_textarea_deletion(word: bool) -> Result<serde_json::Value> {
    let source = "ordinary ".repeat(4_096);
    let original = source.len();
    let mut area = TextArea::new(source);
    if !word {
        area.set_cursor(0);
    }
    let event = if word {
        Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL))
    } else {
        Event::Key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE))
    };

    let started = Instant::now();
    for _ in 0..TEXTAREA_DELETE_EVENTS {
        anyhow::ensure!(
            area.handle_event(black_box(&event), 120) == red::editing::TextAreaOutcome::Changed,
            "embedded deletion event was not handled"
        );
        black_box(area.cursor());
    }
    let expected = if word {
        original - TEXTAREA_DELETE_EVENTS * "ordinary ".len()
    } else {
        original - TEXTAREA_DELETE_EVENTS
    };
    anyhow::ensure!(
        area.buffer().byte_len() == expected,
        "embedded deletion event removed an unexpected amount of text"
    );
    Ok(report(
        if word {
            "embedded_ctrl_backspace_word_deletion"
        } else {
            "embedded_forward_delete_key_events"
        },
        started,
        TEXTAREA_DELETE_EVENTS,
    ))
}

fn benchmark_embedded_home_end_navigation() -> Result<serde_json::Value> {
    let mut area = TextArea::new("ordinary editor draft contents\n".repeat(1_024));
    let home = Event::Key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
    let end = Event::Key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    let started = Instant::now();
    for index in 0..TEXTAREA_VIM_MOTIONS {
        let event = if index % 2 == 0 { &home } else { &end };
        anyhow::ensure!(
            area.handle_event(black_box(event), 120) == red::editing::TextAreaOutcome::Changed,
            "embedded Home/End event was not handled"
        );
        black_box(area.cursor());
    }
    anyhow::ensure!(
        area.cursor() == area.buffer().byte_len(),
        "embedded Home/End navigation lost the final document boundary"
    );
    Ok(report(
        "embedded_home_end_document_navigation",
        started,
        TEXTAREA_VIM_MOTIONS,
    ))
}

fn benchmark_boundary_operator(sentence: bool) -> Result<serde_json::Value> {
    let prefix = "ordinary sentence. another follows.\n\n".repeat(768);
    let ending = "final sentence with ordinary words";
    let buffer = Buffer::new(None, format!("{prefix}{ending}"));
    let cursor = buffer.char_idx_to_position(prefix.len());
    let resolver = MotionResolver::new(&buffer, cursor);
    let started = Instant::now();
    for _ in 0..TEXTAREA_VIM_MOTIONS {
        let (range, _) = if sentence {
            resolver.sentence_range(/*count*/ 1, /*backward*/ false)
        } else {
            resolver.paragraph_range(/*count*/ 1, /*backward*/ false)
        }
        .ok_or_else(|| anyhow::anyhow!("boundary operator did not reach the document end"))?;
        anyhow::ensure!(
            range.end == buffer.char_idx_to_position(prefix.len() + ending.len()),
            "boundary operator selected the wrong document end"
        );
        black_box(range);
    }
    Ok(report(
        if sentence {
            "shared_vim_sentence_operator_document_boundary"
        } else {
            "shared_vim_paragraph_operator_document_boundary"
        },
        started,
        TEXTAREA_VIM_MOTIONS,
    ))
}

fn benchmark_long_line_end_motion() -> Result<serde_json::Value> {
    let mut area = TextArea::new("ordinary_identifier ".repeat(2_048));
    area.set_mode(Mode::Normal);
    area.set_cursor(0);
    let end = Event::Key(KeyEvent::new(KeyCode::Char('$'), KeyModifiers::NONE));
    let start = Event::Key(KeyEvent::new(KeyCode::Char('0'), KeyModifiers::NONE));
    let started = Instant::now();
    for index in 0..TEXTAREA_VIM_MOTIONS {
        let event = if index % 2 == 0 { &end } else { &start };
        anyhow::ensure!(
            area.handle_event(black_box(event), 120) == red::editing::TextAreaOutcome::Changed,
            "long-line Vim motion was not handled"
        );
        black_box(area.cursor());
    }
    anyhow::ensure!(area.cursor() == 0, "long-line Vim motion lost its cursor");
    Ok(report(
        "embedded_vim_long_line_end_motions",
        started,
        TEXTAREA_VIM_MOTIONS,
    ))
}

fn benchmark_long_line_paragraph_operator() -> Result<serde_json::Value> {
    let line = "ordinary_identifier ".repeat(2_048);
    let buffer = Buffer::new(None, format!("{line}\n\nnext paragraph"));
    let resolver = MotionResolver::new(&buffer, TextPosition::new(0, line.len() / 2));
    let started = Instant::now();
    for _ in 0..TEXTAREA_VIM_MOTIONS {
        let (range, linewise) = resolver
            .paragraph_range(/*count*/ 1, /*backward*/ false)
            .ok_or_else(|| anyhow::anyhow!("long-line paragraph operator lost its range"))?;
        anyhow::ensure!(
            range.end == TextPosition::new(0, line.len()) && !linewise,
            "long-line paragraph operator changed its range or shape"
        );
        black_box(range);
    }
    Ok(report(
        "shared_vim_long_line_paragraph_operators",
        started,
        TEXTAREA_VIM_MOTIONS,
    ))
}

fn benchmark_embedded_undo_restoration(redo: bool) -> Result<serde_json::Value> {
    let source = "ordinary editor recovery line and retained cursor contents\n".repeat(768);
    let insertion = source.len() - 2;
    let mut areas = Vec::with_capacity(TEXTAREA_UNDO_RESTORES);
    for _ in 0..TEXTAREA_UNDO_RESTORES {
        let mut area = TextArea::new(&source);
        area.set_cursor(insertion);
        anyhow::ensure!(area.insert("x"), "undo benchmark insertion failed");
        if redo {
            anyhow::ensure!(area.undo(), "redo benchmark preparation failed");
        }
        areas.push(area);
    }

    let started = Instant::now();
    for area in &mut areas {
        let restored = if redo { area.redo() } else { area.undo() };
        anyhow::ensure!(restored, "embedded undo/redo restoration failed");
        black_box(area.cursor());
    }
    let expected = insertion + usize::from(redo);
    anyhow::ensure!(
        areas.iter().all(|area| area.cursor() == expected),
        "embedded undo/redo restoration lost its original cursor"
    );
    Ok(report(
        if redo {
            "embedded_redo_multiline_cursor_restoration"
        } else {
            "embedded_undo_multiline_cursor_restoration"
        },
        started,
        TEXTAREA_UNDO_RESTORES,
    ))
}

fn benchmark_text_object_selection(quoted: bool) -> Result<serde_json::Value> {
    let prefix = "ordinary_identifier ".repeat(1_024);
    let (target, kind) = if quoted {
        ("\"selected \\\" quote\"", TextObjectKind::Quote('"'))
    } else {
        (
            "(outer (selected) suffix)",
            TextObjectKind::Delimited {
                open: '(',
                close: ')',
            },
        )
    };
    let contents = format!("{prefix}{target}{}", " trailing_words".repeat(1_024));
    let cursor = prefix.len() + target.find("selected").unwrap();
    let buffer = Buffer::new(None, contents);
    let resolver = MotionResolver::new(&buffer, TextPosition::new(0, cursor));
    let started = Instant::now();
    for _ in 0..TEXTAREA_VIM_MOTIONS {
        let range = resolver
            .text_object(TextObjectScope::Inner, kind)
            .ok_or_else(|| anyhow::anyhow!("Vim text object was not found"))?;
        anyhow::ensure!(
            range.start.character <= cursor && range.end.character > cursor,
            "Vim text object did not contain its cursor"
        );
        black_box(range);
    }
    Ok(report(
        if quoted {
            "shared_vim_escaped_quote_text_objects"
        } else {
            "shared_vim_nested_delimiter_text_objects"
        },
        started,
        TEXTAREA_VIM_MOTIONS,
    ))
}

fn benchmark_word_text_object(big_word: bool) -> Result<serde_json::Value> {
    let prefix = "ordinary_identifier ".repeat(1_024);
    let target = "target_identifier,punctuation";
    let buffer = Buffer::new(
        None,
        format!("{prefix}{target}{}", " trailing_identifier".repeat(1_024)),
    );
    let cursor = prefix.len() + 4;
    let resolver = MotionResolver::new(&buffer, TextPosition::new(0, cursor));
    let kind = if big_word {
        TextObjectKind::BigWord
    } else {
        TextObjectKind::Word
    };
    let started = Instant::now();
    for _ in 0..TEXTAREA_VIM_MOTIONS {
        let range = resolver
            .text_object(TextObjectScope::Inner, kind)
            .ok_or_else(|| anyhow::anyhow!("Vim word text object was not found"))?;
        anyhow::ensure!(
            range.start.character <= cursor && range.end.character > cursor,
            "Vim word text object did not contain the cursor"
        );
        black_box(range);
    }
    Ok(report(
        if big_word {
            "shared_vim_whitespace_delimited_word_objects"
        } else {
            "shared_vim_keyword_word_text_objects"
        },
        started,
        TEXTAREA_VIM_MOTIONS,
    ))
}

fn benchmark_paragraph_text_object(scope: TextObjectScope) -> Result<serde_json::Value> {
    let line = "ordinary paragraph source line and retained editor content\n";
    let paragraph = line.repeat(768);
    let buffer = Buffer::new(None, format!("header\n\n{paragraph}\n\nfooter"));
    let resolver = MotionResolver::new(&buffer, TextPosition::new(386, 4));
    let started = Instant::now();
    for _ in 0..TEXTAREA_UNDO_RESTORES {
        let range = resolver
            .text_object(scope, TextObjectKind::Paragraph)
            .ok_or_else(|| anyhow::anyhow!("paragraph text object was not found"))?;
        anyhow::ensure!(
            range.start.line <= 386 && range.end.line > 386,
            "paragraph text object lost its source cursor"
        );
        black_box(range);
    }
    Ok(report(
        if scope == TextObjectScope::Inner {
            "shared_vim_inner_paragraph_text_objects"
        } else {
            "shared_vim_around_paragraph_text_objects"
        },
        started,
        TEXTAREA_UNDO_RESTORES,
    ))
}

fn benchmark_sentence_text_object(scope: TextObjectScope) -> Result<serde_json::Value> {
    let prefix = "ordinary sentence. another follows.\n\n".repeat(512);
    let ending = "final unterminated sentence with retained editor words";
    let buffer = Buffer::new(None, format!("{prefix}{ending}"));
    let cursor = buffer.char_idx_to_position(prefix.len() + 8);
    let resolver = MotionResolver::new(&buffer, cursor);
    let started = Instant::now();
    for _ in 0..TEXTAREA_VIM_MOTIONS {
        let range = resolver
            .text_object(scope, TextObjectKind::Sentence)
            .ok_or_else(|| anyhow::anyhow!("final sentence text object was not found"))?;
        anyhow::ensure!(
            range.start == buffer.char_idx_to_position(prefix.len())
                && range.end == buffer.char_idx_to_position(prefix.len() + ending.len()),
            "final sentence text object changed its source boundaries"
        );
        black_box(range);
    }
    Ok(report(
        if scope == TextObjectScope::Inner {
            "shared_vim_inner_final_sentence_text_objects"
        } else {
            "shared_vim_around_final_sentence_text_objects"
        },
        started,
        TEXTAREA_VIM_MOTIONS,
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
