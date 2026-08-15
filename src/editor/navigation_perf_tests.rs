use super::*;

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn workspace(shared_buffer: bool, relative: bool, wrap: bool) -> (Editor, RenderBuffer, Runtime) {
    let mut config = Config {
        relative_line_numbers: Some(relative),
        wrap: Some(wrap),
        ..Config::default()
    };
    config.lsp.enabled = false;
    let source = (0..400)
        .map(|n| {
            format!("fn value_{n}() -> usize {{ {n} }} // a highlighted line with extra text\n")
        })
        .collect::<String>();
    let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
    let mut editor = Editor::with_size(
        lsp,
        120,
        30,
        config,
        Theme::default(),
        vec![
            Buffer::new(Some("fixture.rs".into()), source.clone()),
            Buffer::new(Some("other.rs".into()), source),
        ],
    )
    .unwrap();
    editor.test_disable_terminal_output();
    editor.panel_manager.create_text_panel(
        "agent".into(),
        plugin::PanelConfig {
            side: plugin::PanelSide::Right,
            width: 30,
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
            text: "An unchanged conversation".into(),
        }],
        30,
        120,
    );
    editor.apply_panel_layout();
    assert!(
        editor.update_window_layout(|windows| windows.split_horizontal(if shared_buffer {
            0
        } else {
            1
        }))
    );
    editor.set_active_window(0);
    let mut buffer = RenderBuffer::new(120, 30, &Style::default());
    editor.render(&mut buffer).unwrap();
    (editor, buffer, Runtime::new())
}

fn assert_matches_full_frame(editor: &mut Editor, buffer: &RenderBuffer) {
    let mut full = buffer.clone();
    editor.render(&mut full).unwrap();
    assert_eq!(buffer.cells, full.cells);
}

#[tokio::test]
async fn ignored_mouse_flood_does_not_render_or_mutate_editor_state() {
    let (mut editor, mut buffer, mut runtime) = workspace(false, false, false);
    let generation = editor.render_generation;
    let flushes = editor.terminal_flush_generation;
    let cells = buffer.cells.clone();
    let view = editor.editor_view_state();
    for index in 0..200 {
        editor
            .process_editor_event(
                mouse(MouseEventKind::Moved, 10 + index % 30, 3),
                &mut buffer,
                &mut runtime,
                EventRenderMode::Immediate,
            )
            .await
            .unwrap();
    }
    assert_eq!(editor.render_generation, generation);
    assert_eq!(editor.terminal_flush_generation, flushes);
    assert_eq!(editor.editor_view_state(), view);
    assert_eq!(buffer.cells, cells);
}

#[tokio::test]
async fn wheel_batch_preserves_scroll_distance_and_reuses_unchanged_surfaces() {
    for shared in [false, true] {
        for (relative, wrap) in [(false, false), (true, false), (true, true)] {
            let (mut editor, mut buffer, mut runtime) = workspace(shared, relative, wrap);
            let generation = editor.render_generation;
            let full_renders = editor.full_render_count;
            let mut events = Vec::new();
            for _ in 0..10 {
                events.push(mouse(MouseEventKind::ScrollDown, 12, 3));
                events.push(mouse(MouseEventKind::Moved, 16, 4));
            }
            assert!(!editor
                .process_scroll_batch(events, &mut buffer, &mut runtime)
                .await
                .unwrap());
            assert_eq!(editor.vtop, 30);
            assert_eq!(editor.render_generation, generation + 1);
            assert_eq!(editor.full_render_count, full_renders);
            assert_matches_full_frame(&mut editor, &buffer);
        }
    }
}

#[tokio::test]
async fn wheel_batches_keep_inline_comments_identical_to_a_full_frame() {
    for shared in [false, true] {
        for (relative, wrap) in [(false, false), (true, false), (true, true)] {
            let (mut editor, mut buffer, mut runtime) = workspace(shared, relative, wrap);
            let original = editor.current_buffer().contents();
            for (start, end) in [(2, 4), (9, 12), (27, 29), (55, 57)] {
                let comment = editor.make_inline_comment(
                    start,
                    end,
                    "A source-linked note that wraps in a narrow editor split.".to_string(),
                    inline_comments::InlineCommentOrigin::Sample,
                );
                editor.inline_comments.push(comment);
            }
            editor.layout_cache.borrow_mut().clear();
            editor.render(&mut buffer).unwrap();
            let window = editor.active_window_with_editor_view().unwrap();
            assert!(!editor.layout_for_window(&window).inline_comments.is_empty());
            let annotated_gutter = editor.gutter_width_for_window(&window);

            for kind in [
                MouseEventKind::ScrollDown,
                MouseEventKind::ScrollDown,
                MouseEventKind::ScrollUp,
            ] {
                let full_renders = editor.full_render_count;
                let events = (0..4)
                    .flat_map(|_| [mouse(kind, 12, 3), mouse(MouseEventKind::Moved, 16, 4)])
                    .collect();
                editor
                    .process_scroll_batch(events, &mut buffer, &mut runtime)
                    .await
                    .unwrap();
                assert_eq!(editor.full_render_count, full_renders);
                assert_matches_full_frame(&mut editor, &buffer);
            }

            editor.clear_inline_comments();
            editor.render(&mut buffer).unwrap();
            let window = editor.active_window_with_editor_view().unwrap();
            assert!(editor.gutter_width_for_window(&window) < annotated_gutter);
            assert_eq!(editor.current_buffer().contents(), original);
            assert_matches_full_frame(&mut editor, &buffer);
        }
    }
}

#[tokio::test]
async fn wheel_focus_change_repaints_the_previous_active_window() {
    let (mut editor, mut buffer, mut runtime) = workspace(false, false, false);
    let old_window = editor.window_manager.active_stable_window_id();
    let full_renders = editor.full_render_count;
    let lower = editor.window_manager.window_at_index(1).unwrap();
    let event = mouse(
        MouseEventKind::ScrollDown,
        12,
        (lower.position.y + 2) as u16,
    );
    editor
        .process_scroll_batch(vec![event], &mut buffer, &mut runtime)
        .await
        .unwrap();
    assert_ne!(editor.window_manager.active_stable_window_id(), old_window);
    assert_eq!(editor.full_render_count, full_renders + 1);
    assert_matches_full_frame(&mut editor, &buffer);
}

#[test]
fn wheel_batch_stops_at_direction_target_modifier_and_ordering_boundaries() {
    let first = mouse(MouseEventKind::ScrollDown, 12, 3);
    assert!(Editor::scroll_batch_accepts(&first, &first));
    assert!(Editor::scroll_batch_accepts(
        &first,
        &mouse(MouseEventKind::Moved, 80, 20)
    ));
    for next in [
        mouse(MouseEventKind::ScrollUp, 12, 3),
        mouse(MouseEventKind::ScrollDown, 13, 3),
        mouse(MouseEventKind::Down(MouseButton::Left), 12, 3),
        mouse(MouseEventKind::Drag(MouseButton::Left), 12, 3),
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 12,
            row: 3,
            modifiers: KeyModifiers::SHIFT,
        }),
        Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
        Event::Resize(100, 20),
    ] {
        assert!(!Editor::scroll_batch_accepts(&first, &next), "{next:?}");
    }
}

#[tokio::test]
async fn shared_buffer_decoration_updates_repaint_both_editor_windows() {
    let (mut editor, mut buffer, mut runtime) = workspace(true, false, false);
    let full_renders = editor.full_render_count;
    ACTION_DISPATCHER.send_request(PluginRequest::SetGutterSigns {
        namespace: "perf-test".into(),
        signs: vec![plugin::GutterSign {
            buffer_index: 0,
            line: 0,
            text: "!".into(),
            style: Style::default(),
            priority: 100,
        }],
    });
    editor
        .service_background(&mut buffer, &mut runtime)
        .await
        .unwrap();
    assert_eq!(editor.full_render_count, full_renders);
    assert_matches_full_frame(&mut editor, &buffer);
}

fn decoration(buffer_index: usize) -> plugin::Decoration {
    plugin::Decoration {
        buffer_index: Some(buffer_index),
        anchor: plugin::DecorationAnchor::Column,
        line: 0,
        column: 3,
        text: "!".into(),
        style: Style::default(),
        priority: 100,
        repeat_linebreak: false,
        only_whitespace: false,
    }
}

#[tokio::test]
async fn replacing_decorations_clears_the_previous_inactive_buffer() {
    let (mut editor, mut buffer, mut runtime) = workspace(false, false, false);
    editor
        .decoration_manager
        .set("moving".into(), vec![decoration(1)]);
    editor.render(&mut buffer).unwrap();
    let full_renders = editor.full_render_count;
    ACTION_DISPATCHER.send_request(PluginRequest::SetDecorations {
        namespace: "moving".into(),
        decorations: vec![decoration(0)],
    });
    editor
        .service_background(&mut buffer, &mut runtime)
        .await
        .unwrap();
    assert_eq!(editor.full_render_count, full_renders);
    assert_matches_full_frame(&mut editor, &buffer);
}

#[tokio::test]
async fn streamed_panel_changes_still_invalidate_the_full_surface() {
    let (mut editor, mut buffer, mut runtime) = workspace(false, false, false);
    let full_renders = editor.full_render_count;
    ACTION_DISPATCHER.send_request(PluginRequest::AppendTextPanel {
        id: "agent".into(),
        block_id: "answer".into(),
        delta: "\nNew streamed content".into(),
    });
    editor
        .service_background(&mut buffer, &mut runtime)
        .await
        .unwrap();
    assert_eq!(editor.full_render_count, full_renders + 1);
    assert!(render_text_rows(&buffer)
        .join("\n")
        .contains("New streamed content"));
    assert_matches_full_frame(&mut editor, &buffer);
}

struct HoverDialog(bool);

impl crate::ui::Component for HoverDialog {
    fn draw(&self, _buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        Ok(())
    }
    fn handle_event(&mut self, event: &Event) -> Option<KeyAction> {
        (self.0
            && matches!(
                event,
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Moved,
                    ..
                })
            ))
        .then_some(KeyAction::Single(Action::Refresh))
    }
}

#[tokio::test]
async fn dialogs_may_request_hover_redraws_but_ignored_hover_is_free() {
    let (mut editor, mut buffer, mut runtime) = workspace(false, false, false);
    for redraw in [false, true] {
        editor.current_dialog = Some(Box::new(HoverDialog(redraw)));
        let generation = editor.render_generation;
        editor
            .process_editor_event(
                mouse(MouseEventKind::Moved, 10, 3),
                &mut buffer,
                &mut runtime,
                EventRenderMode::Immediate,
            )
            .await
            .unwrap();
        assert_eq!(editor.render_generation != generation, redraw);
    }
}
