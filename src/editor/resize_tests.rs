use super::*;
use crate::terminal_input::RESIZE_EVENTS_PER_BATCH;

#[derive(Default)]
struct BenchOutput {
    bytes: Vec<u8>,
    flushes: usize,
}

struct BenchSink(Arc<std::sync::Mutex<BenchOutput>>);

impl std::io::Write for BenchSink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().flushes += 1;
        Ok(())
    }
}

/// Runs the production resize/render/encode path without a terminal process.
/// Keep this ignored: timings are workstation evidence, not a CI assertion.
#[tokio::test]
#[ignore = "manual release-mode resize measurement"]
async fn resize_output_benchmark() {
    use sha2::{Digest as _, Sha256};

    for scenario in ["editor", "dense", "workspace"] {
        let mut config = Config::default();
        config.lsp.enabled = false;
        config.statusline.left = vec![crate::config::StatuslineSection::Filename];
        config.statusline.right = vec![crate::config::StatuslineSection::Position];
        let source = (0..400)
            .map(|line| {
                if scenario == "dense" {
                    format!("let value_{line} = \"{}\";\n", "abcdefghij".repeat(28))
                } else {
                    format!("fn value_{line}() -> usize {{ {line} }} // 界 👩‍💻\n")
                }
            })
            .collect::<String>();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            lsp,
            314,
            64,
            config,
            Theme::default(),
            vec![Buffer::new(Some("resize-fixture.rs".into()), source)],
        )
        .unwrap();
        if scenario == "workspace" {
            editor.panel_manager.create_text_panel(
                "agent".into(),
                plugin::PanelConfig {
                    side: plugin::PanelSide::Right,
                    width: 60,
                    title: Some("Agent".into()),
                    ..Default::default()
                },
            );
            editor.panel_manager.update_text_panel(
                "agent",
                vec![plugin::TextPanelBlock {
                    id: "answer".into(),
                    kind: Default::default(),
                    format: Default::default(),
                    text:
                        "An unchanged conversation with several lines.\nSecond line.\nThird line."
                            .into(),
                }],
                60,
                314,
            );
            editor.apply_panel_layout();
            editor.update_window_layout(|windows| windows.split_horizontal(0));
            editor.set_active_window(0);
        }
        let captured = Arc::new(std::sync::Mutex::new(BenchOutput::default()));
        editor.stdout = std::io::BufWriter::with_capacity(
            1 << 20,
            Box::new(BenchSink(captured.clone())) as TerminalOutput,
        );
        let mut buffer = RenderBuffer::new(314, 64, &Style::default());
        let mut runtime = Runtime::new();
        editor.render(&mut buffer).unwrap();
        let mut elapsed = Vec::new();
        let mut bytes = 0;
        let mut flushes = 0;
        let mut frame_hash = Sha256::new();
        for index in 0..68 {
            let size = [(157, 64), (314, 64), (200, 48), (314, 64)][index % 4];
            *captured.lock().unwrap() = BenchOutput::default();
            let started = Instant::now();
            editor
                .process_editor_event(
                    Event::Resize(size.0, size.1),
                    &mut buffer,
                    &mut runtime,
                    EventRenderMode::Immediate,
                )
                .await
                .unwrap();
            editor
                .service_background(&mut buffer, &mut runtime)
                .await
                .unwrap();
            let micros = started.elapsed().as_micros();
            if index >= 8 {
                elapsed.push(micros);
                let output = captured.lock().unwrap();
                bytes += output.bytes.len();
                flushes += output.flushes;
                frame_hash.update(format!("{:?}", buffer.cells).as_bytes());
            }
        }
        elapsed.sort_unstable();
        eprintln!(
            "RESIZE_BENCH {}",
            serde_json::json!({
                "scenario": scenario, "samples": elapsed.len(), "bytes": bytes,
                "flushes": flushes, "p50_us": elapsed[(elapsed.len()-1)*50/100],
                "p95_us": elapsed[(elapsed.len()-1)*95/100], "max_us": elapsed.last(),
                "screen_sha256": format!("{:x}", frame_hash.finalize()),
            })
        );
    }
}

fn resize_editor(width: usize, height: usize) -> (Editor, RenderBuffer) {
    let config = Config::default();
    let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
    let contents = (0..160)
        .map(|line| format!("line {line:03}: abcdefghijklmnopqrstuvwxyz\n"))
        .collect::<String>();
    let mut editor = Editor::with_size(
        lsp,
        width,
        height,
        config,
        Theme::default(),
        vec![Buffer::new(None, contents)],
    )
    .unwrap();
    editor.test_disable_terminal_output();
    let buffer = RenderBuffer::new(width, height, &Style::default());
    (editor, buffer)
}

#[tokio::test]
async fn resize_experience_preserves_logical_cursor_through_shrink_and_grow() {
    for wrap in [false, true] {
        for scrolloff in [0, 3] {
            let (mut editor, mut buffer) = resize_editor(100, 40);
            editor.wrap = wrap;
            editor.config.scrolloff = Some(scrolloff);
            editor.vtop = 40;
            editor.cy = 25;
            editor.cx = 18;
            editor.refresh_cursor_goal();
            editor.render(&mut buffer).unwrap();
            let logical_cursor = (editor.cx, editor.buffer_line());
            let cursor_goal = editor.cursor_goal;
            let mut runtime = Runtime::new();

            for (width, height) in [(25, 10), (12, 4), (0, 0), (1, 2), (100, 40)] {
                editor
                    .process_editor_event(
                        Event::Resize(width, height),
                        &mut buffer,
                        &mut runtime,
                        EventRenderMode::Immediate,
                    )
                    .await
                    .unwrap();

                assert_eq!(
                    (editor.cx, editor.buffer_line()),
                    logical_cursor,
                    "wrap={wrap}, scrolloff={scrolloff}, size={width}x{height}"
                );
                assert_eq!(editor.cursor_goal, cursor_goal);
                assert_eq!(
                    (buffer.width, buffer.height),
                    (width as usize, height as usize)
                );
            }
        }
    }
}

#[tokio::test]
async fn resize_experience_preserves_inactive_split_cursor_lines() {
    let (mut editor, mut buffer) = resize_editor(120, 60);
    editor.window_manager.split_horizontal(0).unwrap();
    for (index, window) in editor.window_manager.windows_mut().into_iter().enumerate() {
        window.wrap = false;
        window.vtop = 30 + index * 40;
        window.cy = 20;
        window.cx = 5;
    }
    editor.sync_with_window();
    editor.refresh_cursor_goal();
    editor.render(&mut buffer).unwrap();
    let positions = |editor: &Editor| {
        editor
            .window_manager
            .windows()
            .into_iter()
            .map(|window| (window.id, window.vtop + window.cy, window.cx))
            .collect::<Vec<_>>()
    };
    let expected = positions(&editor);
    let mut runtime = Runtime::new();

    for (width, height) in [(80, 12), (2, 1), (120, 60)] {
        editor
            .process_editor_event(
                Event::Resize(width, height),
                &mut buffer,
                &mut runtime,
                EventRenderMode::Immediate,
            )
            .await
            .unwrap();
        assert_eq!(positions(&editor), expected);
    }
}

#[tokio::test]
async fn resize_experience_detached_cursor_tracks_mode_and_focus() {
    let (editor, _) = resize_editor(80, 24);
    let mut core = DetachedEditorCore::new(editor).await.unwrap();
    assert_eq!(core.snapshot(None).cursor_state.unwrap().position, None);
    core.editor.mode = Mode::Insert;
    core.editor.render(&mut core.render_buffer).unwrap();
    let insert = core.finish_render().unwrap();
    assert!(insert.cursor_state.unwrap().position.is_some());
    assert_eq!(
        insert.cursor_state.unwrap().shape,
        crate::config::CursorShape::SteadyBar
    );
    let unfocused = core.focus(false).await.unwrap();
    assert!(unfocused.revision > insert.revision);
    assert_eq!(unfocused.cursor_state.unwrap().position, None);
}

#[tokio::test]
async fn resize_experience_duplicate_size_does_not_repaint() {
    let (mut editor, mut buffer) = resize_editor(80, 24);
    editor.render(&mut buffer).unwrap();
    let generation = editor.render_generation;
    let full_renders = editor.full_render_count;
    let mut runtime = Runtime::new();

    editor
        .process_editor_event(
            Event::Resize(80, 24),
            &mut buffer,
            &mut runtime,
            EventRenderMode::Immediate,
        )
        .await
        .unwrap();

    assert_eq!(editor.render_generation, generation);
    assert_eq!(editor.full_render_count, full_renders);
    assert!(!editor.force_full_redraw);
}

#[tokio::test]
async fn resize_experience_publishes_plugin_effects_in_one_frame() {
    let directory = tempfile::tempdir().unwrap();
    let plugin_path = directory.path().join("resize-probe.hk");
    std::fs::write(
        &plugin_path,
        r#"
        pub fn activate() { red::on("editor:resize", resized); }
        fn resized(event: Json) { red::execute("Print", "resize observed"); }
    "#,
    )
    .unwrap();
    let (mut editor, mut buffer) = resize_editor(80, 24);
    let mut runtime = Runtime::new();
    editor
        .plugin_registry
        .add("resize_probe", plugin_path.to_str().unwrap());
    editor
        .plugin_registry
        .initialize(&mut runtime)
        .await
        .unwrap();
    editor
        .service_background(&mut buffer, &mut runtime)
        .await
        .unwrap();
    editor.render(&mut buffer).unwrap();
    let full_renders = editor.full_render_count;

    editor
        .process_editor_event(
            Event::Resize(100, 30),
            &mut buffer,
            &mut runtime,
            EventRenderMode::Immediate,
        )
        .await
        .unwrap();
    editor
        .service_background(&mut buffer, &mut runtime)
        .await
        .unwrap();

    assert_eq!(editor.full_render_count, full_renders + 1);
    assert!(render_text_rows(&buffer)
        .last()
        .unwrap()
        .contains("resize observed"));
    assert!(!editor.defer_motion_render);
    assert_eq!(editor.deferred_motion_render, MotionRender::None);
}

#[test]
fn resize_experience_batch_collects_arrivals_and_preserves_input_boundary() {
    let key = Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    let mut pending = VecDeque::from([Event::Resize(90, 25)]);
    let mut arrivals =
        VecDeque::from([Event::Resize(100, 30), key.clone(), Event::Resize(120, 40)]);
    let latest = crate::terminal_input::read_resize_batch(
        Event::Resize(80, 24),
        &mut pending,
        Duration::from_secs(1),
        |timeout| {
            assert!(!timeout.is_zero());
            assert!(timeout <= Duration::from_millis(2));
            Ok(arrivals.pop_front())
        },
    )
    .unwrap();

    assert_eq!(latest, Event::Resize(100, 30));
    assert_eq!(pending.pop_front(), Some(key));
    assert_eq!(arrivals.pop_front(), Some(Event::Resize(120, 40)));
}

#[tokio::test]
async fn resize_experience_same_size_still_repaints_an_invalidated_surface() {
    let (mut editor, mut buffer) = resize_editor(80, 24);
    editor.render(&mut buffer).unwrap();
    let generation = editor.render_generation;
    let mut runtime = Runtime::new();
    let mut pending = VecDeque::from([Event::Resize(80, 24)]);
    let event = crate::terminal_input::read_resize_batch(
        Event::Resize(40, 12),
        &mut pending,
        Duration::ZERO,
        |_| panic!("must not read after the batch deadline"),
    )
    .unwrap();

    // Native resize delivery invalidates the physical screen independently
    // of whether the final dimensions differ from the last rendered frame.
    editor.force_full_redraw = true;
    editor
        .process_editor_event(event, &mut buffer, &mut runtime, EventRenderMode::Immediate)
        .await
        .unwrap();

    assert_eq!(editor.render_generation, generation + 1);
    assert!(!editor.force_full_redraw);
}

#[test]
fn resize_experience_batch_respects_queued_boundary_and_time_budget() {
    let paste = Event::Paste("keep me".into());
    let mut pending = VecDeque::from([Event::Resize(90, 25), paste.clone()]);
    let latest = crate::terminal_input::read_resize_batch(
        Event::Resize(80, 24),
        &mut pending,
        Duration::from_secs(1),
        |_| panic!("must not read past a queued input boundary"),
    )
    .unwrap();
    assert_eq!(latest, Event::Resize(90, 25));
    assert_eq!(pending.pop_front(), Some(paste));

    let latest = crate::terminal_input::read_resize_batch(
        Event::Resize(80, 24),
        &mut pending,
        Duration::ZERO,
        |_| panic!("must not read after the batch deadline"),
    )
    .unwrap();
    assert_eq!(latest, Event::Resize(80, 24));
}

#[test]
fn resize_experience_batch_has_a_fixed_event_limit() {
    let mut count = 1;
    let latest = crate::terminal_input::read_resize_batch(
        Event::Resize(1, 24),
        &mut VecDeque::new(),
        Duration::from_secs(1),
        |_| {
            count += 1;
            Ok(Some(Event::Resize(count as u16, 24)))
        },
    )
    .unwrap();
    assert_eq!(count, RESIZE_EVENTS_PER_BATCH);
    assert_eq!(latest, Event::Resize(RESIZE_EVENTS_PER_BATCH as u16, 24));
}
