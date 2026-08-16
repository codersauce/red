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
async fn keyboard_edge_frame_matches_full_frame() {
    let mut failures = Vec::new();
    for down in [true, false] {
        let (mut editor, mut buffer, mut runtime) = workspace(false, false, false);
        editor.terminal_output_enabled = true;
        editor.config.scrolloff = Some(0);
        editor
            .config
            .keys
            .normal
            .insert("j".into(), KeyAction::Single(Action::MoveDown));
        editor
            .config
            .keys
            .normal
            .insert("k".into(), KeyAction::Single(Action::MoveUp));
        editor.vtop = 100;
        editor.cy = if down { editor.vheight() - 1 } else { 0 };
        editor.cx = 0;
        editor.sync_to_window();
        editor.render(&mut buffer).unwrap();
        assert!(editor.can_render_cursor_motion_delta());
        let old_vtop = editor.vtop;
        editor
            .process_editor_event(
                Event::Key(KeyEvent::new(
                    KeyCode::Char(if down { 'j' } else { 'k' }),
                    KeyModifiers::NONE,
                )),
                &mut buffer,
                &mut runtime,
                EventRenderMode::Immediate,
            )
            .await
            .unwrap();
        assert_ne!(old_vtop, editor.vtop);
        let mut full = buffer.clone();
        editor.render(&mut full).unwrap();
        let different_rows = buffer
            .cells
            .chunks(buffer.width)
            .zip(full.cells.chunks(full.width))
            .enumerate()
            .filter_map(|(row, (actual, expected))| (actual != expected).then_some(row))
            .collect::<Vec<_>>();
        eprintln!(
            "direction={} viewport={}→{} stale_rows={different_rows:?}",
            if down { "down" } else { "up" },
            old_vtop,
            editor.vtop
        );
        if !different_rows.is_empty() {
            failures.push((down, different_rows));
        }
    }
    assert!(
        failures.is_empty(),
        "keyboard edge frames differ from a full redraw: {failures:?}"
    );
}

fn key(character: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))
}

fn bind_keyboard_motions(editor: &mut Editor) {
    for (name, action) in [("j", Action::MoveDown), ("k", Action::MoveUp)] {
        editor
            .config
            .keys
            .normal
            .insert(name.into(), KeyAction::Single(action));
    }
}

async fn keyboard_batch(
    editor: &mut Editor,
    buffer: &mut RenderBuffer,
    runtime: &mut Runtime,
    pending: &mut VecDeque<Event>,
) -> usize {
    let before = pending.len();
    let event = pending.pop_front().unwrap();
    let processed = editor
        .process_editor_event(event, buffer, runtime, EventRenderMode::CoalescedNavigation)
        .await
        .unwrap();
    if processed.navigation_deferred {
        if processed.drain_repeated_motion {
            editor
                .drain_repeated_motion_with_reader(
                    processed.repeat_signature.unwrap(),
                    pending,
                    buffer,
                    runtime,
                    Duration::MAX,
                    |pending| Ok(pending.pop_front()),
                )
                .await
                .unwrap();
        } else {
            editor
                .finish_navigation_batch(buffer, runtime)
                .await
                .unwrap();
        }
    }
    before - pending.len()
}

#[tokio::test]
async fn keyboard_batches_preserve_every_key_and_stop_at_direction_boundaries() {
    let (mut editor, mut buffer, mut runtime) = workspace(false, false, false);
    bind_keyboard_motions(&mut editor);
    editor.vtop = 100;
    editor.cy = 3;
    editor.sync_to_window();
    editor.render(&mut buffer).unwrap();
    let start = editor.buffer_line();
    let mut pending = VecDeque::from([key('j'), key('j'), key('k'), key('j'), key('k')]);
    assert_eq!(
        keyboard_batch(&mut editor, &mut buffer, &mut runtime, &mut pending).await,
        2
    );
    assert_eq!(editor.buffer_line(), start + 2);
    assert_eq!(pending.front(), Some(&key('k')));
    while !pending.is_empty() {
        keyboard_batch(&mut editor, &mut buffer, &mut runtime, &mut pending).await;
    }
    assert_eq!(editor.buffer_line(), start + 1);
    assert_matches_full_frame(&mut editor, &buffer);

    let start = editor.buffer_line();
    let mut pending = VecDeque::from(vec![key('j'); KEY_EVENTS_PER_BATCH * 2 + 7]);
    let mut handled = 0;
    while !pending.is_empty() {
        let count = keyboard_batch(&mut editor, &mut buffer, &mut runtime, &mut pending).await;
        assert!((1..=KEY_EVENTS_PER_BATCH).contains(&count));
        handled += count;
    }
    assert_eq!(handled, KEY_EVENTS_PER_BATCH * 2 + 7);
    assert_eq!(editor.buffer_line(), start + handled);
    assert_matches_full_frame(&mut editor, &buffer);
}

#[tokio::test]
async fn keyboard_batch_leaves_nonmatching_input_queued() {
    for boundary in [
        key('k'),
        Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL)),
        Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('j'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        )),
        Event::Resize(100, 25),
        key('i'),
    ] {
        let (mut editor, mut buffer, mut runtime) = workspace(false, false, false);
        bind_keyboard_motions(&mut editor);
        let mut pending = VecDeque::from([key('j'), boundary.clone(), key('j')]);
        assert_eq!(
            keyboard_batch(&mut editor, &mut buffer, &mut runtime, &mut pending).await,
            1
        );
        assert_eq!(pending, VecDeque::from([boundary, key('j')]));
    }
}

#[test]
fn keyboard_repeat_drain_excludes_counts_operators_and_macro_state() {
    let (mut editor, _, _) = workspace(false, false, false);
    let motion = KeyAction::Single(Action::MoveDown);
    assert!(editor.should_drain_repeated_motion(&key('j'), &motion));
    assert!(!editor.should_drain_repeated_motion(
        &key('j'),
        &KeyAction::Repeating(2, Box::new(motion.clone()))
    ));
    editor.pending_operator = Some(PendingOperator::new(EditOperator::Delete, 1));
    assert!(!editor.should_drain_repeated_motion(&key('j'), &motion));
    editor.pending_operator = None;
    editor.macro_recording = Some(MacroRecording {
        register: 'q',
        events: Vec::new(),
    });
    assert!(!editor.should_drain_repeated_motion(&key('j'), &motion));
    editor.macro_recording = None;
    editor.macro_replay_depth = 1;
    assert!(!editor.should_drain_repeated_motion(&key('j'), &motion));
    editor.macro_replay_depth = 0;
    editor.replaying_semantic_change = true;
    assert!(!editor.should_drain_repeated_motion(&key('j'), &motion));
}

#[tokio::test]
async fn counted_keyboard_motion_is_one_publication_and_not_a_repeat_run() {
    let (mut editor, mut buffer, mut runtime) = workspace(false, true, false);
    bind_keyboard_motions(&mut editor);
    editor
        .process_editor_event(
            key('2'),
            &mut buffer,
            &mut runtime,
            EventRenderMode::Immediate,
        )
        .await
        .unwrap();
    let generation = editor.render_generation;
    let processed = editor
        .process_editor_event(
            key('j'),
            &mut buffer,
            &mut runtime,
            EventRenderMode::CoalescedNavigation,
        )
        .await
        .unwrap();
    assert!(processed.navigation_deferred);
    assert!(!processed.drain_repeated_motion);
    assert_eq!(editor.render_generation, generation);
    editor
        .finish_navigation_batch(&mut buffer, &mut runtime)
        .await
        .unwrap();
    assert_eq!(editor.buffer_line(), 2);
    assert_eq!(editor.render_generation, generation + 1);
    assert_matches_full_frame(&mut editor, &buffer);
}

#[tokio::test]
async fn keyboard_batch_merges_pending_decoration_damage_before_painting() {
    for shared in [false, true] {
        let (mut editor, mut buffer, mut runtime) = workspace(shared, false, false);
        bind_keyboard_motions(&mut editor);
        let generation = editor.render_generation;
        let processed = editor
            .process_editor_event(
                key('j'),
                &mut buffer,
                &mut runtime,
                EventRenderMode::CoalescedNavigation,
            )
            .await
            .unwrap();
        assert!(processed.navigation_deferred);
        assert_eq!(editor.render_generation, generation);
        ACTION_DISPATCHER.send_request(PluginRequest::SetDecorations {
            namespace: "keyboard-test".into(),
            decorations: vec![decoration(0)],
        });
        editor
            .finish_navigation_batch(&mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(editor.render_generation, generation + 1);
        assert_matches_full_frame(&mut editor, &buffer);
    }
}

#[tokio::test]
async fn keyboard_scrolling_matches_full_frames_with_wrapping_and_comments() {
    for shared in [false, true] {
        for (relative, wrap, comments, scrolloff) in [
            (false, false, false, 0),
            (false, false, false, 3),
            (false, true, false, 3),
            (true, true, true, 0),
            (false, true, true, 3),
            (true, false, true, 3),
        ] {
            let (mut editor, mut buffer, mut runtime) = workspace(shared, relative, wrap);
            bind_keyboard_motions(&mut editor);
            editor.terminal_output_enabled = true;
            editor.config.scrolloff = Some(scrolloff);
            if comments {
                for (start, end) in [(95, 98), (104, 106), (114, 118)] {
                    let comment = editor.make_inline_comment(
                        start,
                        end,
                        "A long annotation spanning source lines and wrapping in the editor."
                            .into(),
                        inline_comments::InlineCommentOrigin::Sample,
                    );
                    editor.inline_comments.push(comment);
                }
                editor.layout_cache.borrow_mut().clear();
            }
            editor.vtop = 100;
            editor.cy = 0;
            editor.sync_to_window();
            editor.render(&mut buffer).unwrap();
            let source = editor.current_buffer().contents();
            for direction in ['j', 'k'] {
                for _ in 0..24 {
                    let mut pending = VecDeque::from([key(direction)]);
                    keyboard_batch(&mut editor, &mut buffer, &mut runtime, &mut pending).await;
                    assert_matches_full_frame(&mut editor, &buffer);
                }
            }
            assert_eq!(editor.current_buffer().contents(), source);
        }
    }
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
