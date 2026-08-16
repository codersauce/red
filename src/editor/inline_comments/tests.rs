use super::*;
use crate::{
    buffer::Buffer,
    color::{contrast_ratio, Color},
    config::{Config, KeyAction},
    editor::{
        display_layout::{
            inline_comment_block, layout_lines, BreakIndentOptions, DisplayLayout,
            InlineCommentContent, LayoutConfig,
        },
        Action, Mode, RenderBuffer,
    },
    inline_assist::{InlineAssistResult, InlineCommentInput},
    lsp::LspManager,
    plugin::Runtime,
    theme::{Style, Theme},
    undo::TextRange,
    unicode_utils::display_width,
};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

fn editor(text: &str, width: usize, height: usize, wrap: bool) -> Editor {
    let defaults: Config = toml::from_str(include_str!("../../../default_config.toml")).unwrap();
    let config = Config {
        wrap: Some(wrap),
        scrolloff: Some(0),
        ..defaults
    };
    let mut editor = Editor::with_size(
        Box::new(LspManager::new(config.lsp.clone())),
        width,
        height,
        config,
        Theme::default(),
        vec![Buffer::new(None, text.to_string())],
    )
    .unwrap();
    editor.test_disable_terminal_output();
    editor
}

fn layout(width: usize, height: usize) -> DisplayLayout {
    layout_lines(
        &[
            "alpha\n".to_string(),
            "beta\n".to_string(),
            "gamma\n".to_string(),
        ],
        3,
        LayoutConfig {
            content_width: width,
            height,
            wrap: false,
            vtop: 0,
            vleft: 0,
            skipcol: 0,
            break_indent: BreakIndentOptions::default(),
        },
    )
}

#[test]
fn inline_comment_rows_preserve_source_coordinates_and_offsets() {
    let original = layout(40, 8);
    let composed = layout(40, 8).with_inline_comments(&[(1, "Review this line")], 40, 8);
    assert_eq!(composed.inline_comment_row(1).unwrap().line, 1);
    assert!(composed.row(1).is_none());
    assert_eq!(composed.inline_comments.len(), 3);
    assert_eq!(
        composed.inline_comments[0].content,
        InlineCommentContent::TopEdge
    );
    assert_eq!(
        composed.inline_comments[2].content,
        InlineCommentContent::BottomEdge
    );
    assert_eq!(composed.row(4).unwrap().line, 1);
    assert_eq!(
        composed.row(4).unwrap().source_offset,
        original.row(1).unwrap().source_offset
    );
    assert_eq!(composed.segment_for_cursor(1, 0).unwrap().row, 4);
    assert_eq!(composed.screen_height(), 6);
}

#[test]
fn inline_comment_block_sizes_to_text_with_two_by_one_padding() {
    let block = inline_comment_block("short note", 60, 10);
    assert_eq!(
        block.rows,
        [
            InlineCommentContent::TopEdge,
            InlineCommentContent::Text("short note".into()),
            InlineCommentContent::BottomEdge
        ]
    );
    assert_eq!(block.text_offset, 2);
    assert_eq!(block.width, display_width("short note") + 4);
    let narrow = inline_comment_block("words stay together", 14, 10);
    assert_eq!(
        narrow
            .rows
            .iter()
            .map(InlineCommentContent::text)
            .collect::<Vec<_>>(),
        ["", "words stay", "together", ""]
    );
    assert_eq!(narrow.width, 14);
    assert_eq!(narrow.text_offset, 2);
}

#[test]
fn inline_comments_wrap_but_always_leave_the_source_visible() {
    assert_eq!(
        super::super::display_layout::wrap_inline_comment("words stay together", 11),
        vec!["words stay", "together"]
    );
    let composed = layout(4, 3).with_inline_comments(&[(0, "界界界界界界界界")], 4, 3);
    assert_eq!(composed.inline_comments.len(), 2);
    assert_eq!(composed.row(2).unwrap().line, 0);
    assert!(composed
        .inline_comments
        .iter()
        .all(|row| row.block_width <= 4
            && row.text_offset + display_width(row.content.text()) <= row.block_width));
    assert!(composed
        .inline_comments
        .last()
        .unwrap()
        .content
        .text()
        .ends_with('…'));
    let tiny = layout(1, 1).with_inline_comments(&[(0, "界")], 1, 1);
    assert!(tiny.inline_comments.is_empty());
    assert_eq!(tiny.row(0).unwrap().line, 0);
}

#[test]
fn inline_comment_rendering_is_not_a_text_edit() {
    let mut editor = editor("alpha\nbeta\ngamma\n", 60, 12, false);
    editor.cy = 1;
    editor.set_inline_comment("A review note");
    editor.sync_to_window();
    let window = editor.active_window_with_editor_view().unwrap();
    let layout = editor.layout_for_window(&window);
    assert_eq!(layout.inline_comments[0].row, 1);
    assert_eq!(editor.buffer_to_window_coords(&window, 0, 1).unwrap().1, 4);
    let mut frame = RenderBuffer::new(60, 12, &Style::default());
    editor.render(&mut frame).unwrap();
    let x = editor.gutter_width_for_window(&window) + 1;
    let row = frame.cells[120..180]
        .iter()
        .map(|cell| cell.c)
        .collect::<String>();
    assert!(row
        .chars()
        .skip(x + 2)
        .collect::<String>()
        .starts_with("A review note"));
    assert_eq!(
        row.chars()
            .take(x)
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>(),
        "╭───"
    );
    let comment_style = editor.theme.inline_comment_style();
    assert_ne!(comment_style.bg, editor.theme.style.bg);
    for comment in &layout.inline_comments {
        let cells = &frame.cells[comment.row * 60..(comment.row + 1) * 60];
        assert!(cells[..x]
            .iter()
            .all(|cell| cell.style.bg == editor.theme.style.bg));
        assert!(cells[x + comment.block_width..]
            .iter()
            .all(|cell| cell.style.bg == editor.theme.style.bg));
        if let InlineCommentContent::Text(_) = &comment.content {
            assert!(cells[x..x + comment.block_width]
                .iter()
                .all(|cell| cell.style.bg == comment_style.bg));
            assert!(cells[x..x + 2].iter().all(|cell| cell.c == ' '));
            assert!(cells[x + comment.block_width - 2..x + comment.block_width]
                .iter()
                .all(|cell| cell.c == ' '));
        } else {
            let glyph = if comment.content == InlineCommentContent::TopEdge {
                '▄'
            } else {
                '▀'
            };
            assert!(cells[x..x + comment.block_width]
                .iter()
                .all(|cell| cell.c == glyph
                    && cell.style.fg == comment_style.bg
                    && cell.style.bg == editor.theme.style.bg
                    && !cell.style.italic));
        }
    }
    assert_eq!(editor.current_buffer().contents(), "alpha\nbeta\ngamma\n");
    assert!(!editor.current_buffer().is_dirty());
    assert!(editor
        .current_buffer()
        .undo_history
        .latest_transaction()
        .is_none());
    editor.set_inline_comment("Replacement note");
    assert_eq!(editor.inline_comments.len(), 1);
    assert_eq!(
        editor.layout_for_window(&window).inline_comments[1]
            .content
            .text(),
        "Replacement note"
    );
    editor.clear_inline_comments();
    assert!(editor.layout_for_window(&window).inline_comments.is_empty());
}

#[test]
fn inline_comment_surfaces_and_faded_guides_follow_dark_and_light_themes() {
    for background in [
        Color::Rgb {
            r: 32,
            g: 32,
            b: 36,
        },
        Color::Rgb {
            r: 255,
            g: 255,
            b: 255,
        },
    ] {
        let mut editor = editor("alpha\nbeta\n", 40, 10, false);
        editor.theme.colors.clear();
        editor.theme.style.bg = Some(background);
        editor.cy = 1;
        editor.set_inline_comment("This comment wraps onto several rows in a narrow editor.");
        let mut frame = RenderBuffer::new(40, 10, &Style::default());
        editor.render(&mut frame).unwrap();
        let window = editor.active_window_with_editor_view().unwrap();
        let layout = editor.layout_for_window(&window);
        let style = editor.theme.inline_comment_style();
        let guide = editor.theme.inline_comment_guide_style();
        let comment_background = style.bg.unwrap();
        assert_ne!(comment_background, background);
        assert!(contrast_ratio(style.fg.unwrap(), comment_background) >= 4.5);
        assert!(
            contrast_ratio(guide.fg.unwrap(), background)
                < contrast_ratio(style.fg.unwrap(), background)
        );
        assert!(layout.inline_comments.len() > 1);
        for comment in &layout.inline_comments {
            let y = editor.window_to_terminal_y(&window, comment.row);
            let cells = &frame.cells[y * 40..(y + 1) * 40];
            let gutter_width = editor.gutter_width_for_window(&window) + 1;
            assert!(cells[..gutter_width]
                .iter()
                .all(|cell| cell.style.bg == Some(background)));
            let block_cells = &cells[gutter_width..gutter_width + comment.block_width];
            match &comment.content {
                InlineCommentContent::Text(_) => assert!(block_cells
                    .iter()
                    .all(|cell| cell.style.bg == Some(comment_background))),
                InlineCommentContent::TopEdge | InlineCommentContent::BottomEdge => {
                    let glyph = if comment.content == InlineCommentContent::TopEdge {
                        '▄'
                    } else {
                        '▀'
                    };
                    assert!(block_cells.iter().all(|cell| cell.c == glyph
                        && cell.style.fg == Some(comment_background)
                        && cell.style.bg == Some(background)));
                }
            }
            assert!(cells[gutter_width + comment.block_width..]
                .iter()
                .all(|cell| cell.style.bg == Some(background)));
            let guide_x = gutter_width - editor.inline_comment_lane_width(&window);
            let expected = if comment.starts_connection {
                "╭"
            } else if comment.content == InlineCommentContent::TopEdge {
                " "
            } else {
                "┆"
            };
            assert_eq!(cells[guide_x].text, expected);
            if expected != " " {
                assert_eq!(cells[guide_x].style, guide);
            }
            assert!(cells[..guide_x].iter().all(|cell| cell.c == ' '));
        }
    }
}

#[test]
fn inline_comment_half_height_edges_fall_back_to_solid_padding_in_ascii_mode() {
    let mut editor = editor("alpha\nbeta\n", 40, 10, false);
    editor.config.window_borders_ascii = true;
    editor.cy = 1;
    editor.set_inline_comment("note");
    let mut frame = RenderBuffer::new(40, 10, &Style::default());
    editor.render(&mut frame).unwrap();
    let window = editor.active_window_with_editor_view().unwrap();
    let layout = editor.layout_for_window(&window);
    let x = editor.gutter_width_for_window(&window) + 1;
    for comment in &layout.inline_comments {
        let y = editor.window_to_terminal_y(&window, comment.row);
        let cells = &frame.cells[y * 40..(y + 1) * 40];
        let expected = if comment.starts_connection {
            '+'
        } else if comment.content == InlineCommentContent::TopEdge {
            ' '
        } else {
            ':'
        };
        assert_eq!(
            cells[x - editor.inline_comment_lane_width(&window)].c,
            expected
        );
        assert!(cells[x..x + comment.block_width]
            .iter()
            .all(|cell| cell.style.bg == editor.theme.inline_comment_style().bg));
        if !matches!(comment.content, InlineCommentContent::Text(_)) {
            assert!(cells[x..x + comment.block_width]
                .iter()
                .all(|cell| cell.c == ' '));
        }
    }
}

#[tokio::test]
async fn inline_comment_anchors_follow_edits_and_undo_redo() {
    let mut editor = editor("alpha\nbeta\ngamma\n", 60, 12, false);
    editor.cy = 1;
    editor.set_inline_comment("note");
    editor.begin_transaction("insert above");
    editor.replace_range(TextRange::insertion(TextPosition::new(0, 0)), "new\n");
    assert!(editor.commit_transaction(editor.cursor_snapshot()));
    assert_eq!(
        editor
            .current_buffer()
            .char_idx_to_position(editor.inline_comments[0].anchor.char_index)
            .line,
        2
    );
    editor
        .test_execute_production_action(Action::Undo)
        .await
        .unwrap();
    assert_eq!(
        editor
            .current_buffer()
            .char_idx_to_position(editor.inline_comments[0].anchor.char_index)
            .line,
        1
    );
    editor
        .test_execute_production_action(Action::Redo)
        .await
        .unwrap();
    assert_eq!(
        editor
            .current_buffer()
            .char_idx_to_position(editor.inline_comments[0].anchor.char_index)
            .line,
        2
    );
}

#[tokio::test]
async fn inline_comment_keys_and_mouse_keep_source_cursor_semantics() {
    let mut editor = editor("alpha\nbeta\ngamma\n", 60, 12, false);
    let Some(KeyAction::Nested(leader)) = editor.config.keys.normal.get(" ") else {
        panic!("missing normal-mode leader bindings");
    };
    assert_eq!(
        leader.get("C"),
        Some(&KeyAction::Single(Action::AddSampleInlineComment))
    );
    assert_eq!(
        leader.get("X"),
        Some(&KeyAction::Single(Action::ClearInlineComments))
    );
    editor.cy = 1;
    for key in [' ', 'C'] {
        editor
            .test_execute_event(Event::Key(KeyEvent::new(
                KeyCode::Char(key),
                KeyModifiers::NONE,
            )))
            .await
            .unwrap();
    }
    assert_eq!(editor.inline_comments.len(), 1);
    editor.set_inline_comment("note");
    editor.sync_to_window();
    let window = editor.active_window_with_editor_view().unwrap();
    let row = editor.layout_for_window(&window).inline_comments[0].row;
    editor
        .test_execute_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: (window.position.x + editor.gutter_width_for_window(&window) + 2) as u16,
            row: editor.window_to_terminal_y(&window, row) as u16,
            modifiers: KeyModifiers::NONE,
        }))
        .await
        .unwrap();
    assert_eq!(editor.buffer_line(), 1);
    editor
        .test_execute_production_action(Action::MoveDown)
        .await
        .unwrap();
    assert_eq!(editor.buffer_line(), 2);
    editor
        .test_execute_production_action(Action::MoveScreenLineUp)
        .await
        .unwrap();
    assert_eq!(editor.buffer_line(), 1);
    for key in [' ', 'X'] {
        editor
            .test_execute_event(Event::Key(KeyEvent::new(
                KeyCode::Char(key),
                KeyModifiers::NONE,
            )))
            .await
            .unwrap();
    }
    assert!(editor.inline_comments.is_empty());
    assert_eq!(editor.mode, Mode::Normal);
    assert!(!editor.current_buffer().is_dirty());
}

#[test]
fn inline_comments_keep_bottom_cursor_visible_in_both_wrap_modes() {
    let text = (0..30)
        .map(|line| format!("line {line}\n"))
        .collect::<String>();
    for wrap in [false, true] {
        let mut editor = editor(&text, 40, 8, wrap);
        for line in 0..6 {
            editor.cy = line;
            editor.set_inline_comment("A long comment that takes more than one screen row.");
        }
        editor.cy = 5;
        editor.check_bounds();
        assert_eq!(editor.buffer_line(), 5);
        assert!(editor.visible_cursor_segment(5, 0));
        let window = editor.active_window_with_editor_view().unwrap();
        assert!(editor
            .layout_for_window(&window)
            .inline_comments
            .iter()
            .any(|comment| comment.line == 5));
        assert!(editor.vtop > 0);
        editor.test_set_size(24, 6);
        editor.check_bounds();
        assert_eq!(editor.buffer_line(), 5);
        assert!(editor.visible_cursor_segment(5, 0));
    }
}

#[tokio::test]
async fn inline_comments_preserve_viewport_at_insert_line_end() {
    let text = (0..80)
        .map(|line| format!("    line_{line};\n"))
        .collect::<String>();
    let cases: &[(&str, &[KeyCode], usize)] = &[
        ("o", &[KeyCode::Char('o')], 33),
        ("O", &[KeyCode::Char('O')], 32),
        ("A", &[KeyCode::Char('A')], 32),
        ("Enter", &[KeyCode::Char('A'), KeyCode::Enter], 33),
    ];

    for wrap in [false, true] {
        for comment_line in [None, Some(32), Some(34)] {
            for &(name, keys, target_line) in cases {
                let mut editor = editor(&text, 80, 24, wrap);
                if let Some(line) = comment_line {
                    editor.test_set_viewport_cursor(20, 4, line - 20);
                    editor.set_inline_comment("Review this line");
                }
                editor.test_set_viewport_cursor(20, 4, 12);

                for &key in keys {
                    editor
                        .test_execute_event(Event::Key(KeyEvent::new(key, KeyModifiers::NONE)))
                        .await
                        .unwrap();
                    assert_eq!(
                        editor.vtop, 20,
                        "{name}: wrap={wrap}, comment_line={comment_line:?}, key={key:?}"
                    );
                }

                assert_eq!(editor.mode, Mode::Insert);
                assert_eq!(editor.buffer_line(), target_line);
                let expected = if name == "A" {
                    "    line_32;\n"
                } else {
                    "    \n"
                };
                assert_eq!(editor.current_line_contents().as_deref(), Some(expected));
                assert_eq!(editor.cx, expected.trim_end_matches('\n').len());
                assert!(editor.visible_cursor_segment(target_line, editor.cx));

                editor
                    .test_execute_event(Event::Key(KeyEvent::new(
                        KeyCode::Char('x'),
                        KeyModifiers::NONE,
                    )))
                    .await
                    .unwrap();
                assert_eq!(editor.vtop, 20, "typing after {name}");
                assert_eq!(editor.buffer_line(), target_line);
                assert!(editor.visible_cursor_segment(target_line, editor.cx));
            }
        }
    }
}

#[test]
fn inline_comments_reveal_the_actual_wrapped_cursor_segment() {
    let text = (0..40)
        .map(|line| {
            if line == 26 {
                format!("{}\n", "x".repeat(300))
            } else {
                format!("line {line}\n")
            }
        })
        .collect::<String>();
    let mut editor = editor(&text, 40, 8, true);
    editor.test_set_viewport_cursor(20, 0, 15);
    editor.set_inline_comment("Review a later line");
    editor.mode = Mode::Insert;
    let width = editor.active_content_width();
    let bottom_row = editor.vheight() - 1;
    let initial_top = 26 - bottom_row;
    editor.test_set_viewport_cursor(initial_top, width, bottom_row);

    assert!(!editor.visible_cursor_segment(26, width));
    editor.ensure_inline_comment_cursor_visible();
    assert_eq!(editor.vtop, initial_top + 1);
    assert_eq!(editor.buffer_line(), 26);
    assert!(editor.visible_cursor_segment(26, width));

    // An end-of-line caret beyond a full screen must use the final segment,
    // even when the fallback has to scroll within the logical line.
    editor.test_set_viewport_cursor(initial_top, 300, bottom_row);
    assert!(!editor.visible_cursor_segment(26, 300));
    editor.ensure_inline_comment_cursor_visible();
    assert_eq!(editor.buffer_line(), 26);
    assert!(editor.skipcol > 0);
    assert!(editor.visible_cursor_segment(26, 300));
}

#[test]
fn clearing_inline_comments_preserves_cursor_near_eof() {
    let mut editor = editor("alpha\nbeta\ngamma\ndelta\nepsilon\n", 40, 6, false);
    editor.vtop = 3;
    editor.cy = 1;
    editor.set_inline_comment("A comment near the end of the file.");
    editor.check_bounds();
    assert_eq!(editor.buffer_line(), 4);
    editor.clear_inline_comments();
    editor.check_bounds();
    assert_eq!(editor.buffer_line(), 4);
}

#[test]
fn inline_comment_navigation_preserves_treesitter_class_motions() {
    let editor = editor("class Example {}\n", 60, 12, false);
    let nested =
        |map: &std::collections::HashMap<String, KeyAction>, prefix: &str, key: &str| match map
            .get(prefix)
        {
            Some(KeyAction::Nested(keys)) => keys.get(key).cloned(),
            _ => None,
        };
    assert_eq!(
        nested(&editor.config.keys.normal, "]", "c"),
        Some(KeyAction::Single(Action::MoveToNextClass))
    );
    assert_eq!(
        nested(&editor.config.keys.normal, "[", "c"),
        Some(KeyAction::Single(Action::MoveToPreviousClass))
    );
    let Some(KeyAction::Nested(leader)) = editor.config.keys.normal.get(" ") else {
        panic!("missing leader");
    };
    assert_eq!(
        nested(leader, "]", "c"),
        Some(KeyAction::Single(Action::NextInlineComment))
    );
    assert_eq!(
        nested(leader, "[", "c"),
        Some(KeyAction::Single(Action::PreviousInlineComment))
    );
}

fn begin_assist(editor: &mut Editor, range: TextRange, request: &str, group: &str) {
    editor.inline_assist = Some(super::super::InlineAssistSession {
        buffer_id: editor.current_buffer().id(),
        window_id: editor.window_manager.active_stable_window_id().unwrap(),
        expected_revision: editor.current_buffer().revision(),
        expected_text: editor.current_buffer().text_in_range(range),
        range,
        scope: "test selection".into(),
        request_id: Some(request.into()),
        session_id: Some("test-session".into()),
        transaction_id: None,
        annotation_group_id: group.into(),
        has_result: false,
        result_request_id: None,
    });
}

fn note(start: usize, end: usize, message: &str) -> InlineCommentInput {
    InlineCommentInput {
        start_line: start,
        end_line: Some(end),
        message: message.into(),
    }
}

#[tokio::test]
async fn inline_comment_only_result_is_kept_without_a_text_transaction() {
    let mut editor = editor("alpha\nbeta\ngamma\n", 70, 16, false);
    begin_assist(
        &mut editor,
        TextRange::new(TextPosition::new(0, 0), TextPosition::new(2, 0)),
        "review",
        "group",
    );
    let mut frame = RenderBuffer::new(70, 16, &Style::default());
    let mut runtime = Runtime::new();
    editor
        .apply_inline_result(
            "review",
            "test-session",
            &InlineAssistResult {
                needs_agent: None,
                replacement: None,
                comments: vec![note(1, 2, "Both lines")],
            },
            &mut frame,
            &mut runtime,
        )
        .await
        .unwrap();
    assert_eq!(
        editor.inline_comments[0].lines(editor.current_buffer()),
        (0, 1)
    );
    assert!(!editor.current_buffer().is_dirty());
    assert!(editor
        .current_buffer()
        .undo_history
        .latest_transaction()
        .is_none());
    assert_eq!(
        editor.inline_assist_result_state(),
        crate::ui::InlineAssistPopupState::Applied {
            edited: false,
            comments: 1
        }
    );
    let accepted_id = editor.inline_comments[0].id;
    assert!(editor
        .apply_inline_result(
            "review",
            "test-session",
            &InlineAssistResult {
                needs_agent: None,
                replacement: None,
                comments: vec![note(1, 1, "Duplicate")]
            },
            &mut frame,
            &mut runtime
        )
        .await
        .is_err());
    assert_eq!(editor.inline_comments[0].id, accepted_id);
    editor
        .execute(&Action::KeepInlineAssist, &mut frame, &mut runtime)
        .await
        .unwrap();
    assert!(editor.inline_assist.is_none());
    assert_eq!(editor.inline_comments.len(), 1);
    editor.dismiss_inline_comment();
    assert!(editor.inline_comments.is_empty());
}

#[tokio::test]
async fn inline_mixed_result_validates_before_editing_and_undo_removes_its_group() {
    let mut editor = editor("old\ntail\n", 70, 16, false);
    begin_assist(
        &mut editor,
        TextRange::new(TextPosition::new(0, 0), TextPosition::new(1, 0)),
        "edit",
        "group",
    );
    let mut frame = RenderBuffer::new(70, 16, &Style::default());
    let mut runtime = Runtime::new();
    let mut result = InlineAssistResult {
        needs_agent: None,
        replacement: Some("one\ntwo\n".into()),
        comments: vec![note(3, 3, "Outside")],
    };
    assert!(editor
        .apply_inline_result("edit", "test-session", &result, &mut frame, &mut runtime)
        .await
        .is_err());
    assert_eq!(editor.current_buffer().contents(), "old\ntail\n");
    assert!(editor.inline_comments.is_empty());
    result.comments = vec![note(2, 2, "Second new line")];
    editor
        .apply_inline_result("edit", "test-session", &result, &mut frame, &mut runtime)
        .await
        .unwrap();
    assert_eq!(editor.current_buffer().contents(), "one\ntwo\ntail\n");
    assert_eq!(
        editor.inline_comments[0].lines(editor.current_buffer()),
        (1, 1)
    );
    editor
        .execute(&Action::UndoInlineAssist, &mut frame, &mut runtime)
        .await
        .unwrap();
    assert_eq!(editor.current_buffer().contents(), "old\ntail\n");
    assert!(editor.inline_comments.is_empty());
}

#[tokio::test]
async fn inline_comment_refinement_replaces_only_its_own_group() {
    let mut editor = editor("alpha\nbeta\n", 70, 16, false);
    let range = TextRange::new(TextPosition::new(0, 0), TextPosition::new(2, 0));
    let mut frame = RenderBuffer::new(70, 16, &Style::default());
    let mut runtime = Runtime::new();
    for (request, group, message) in [
        ("one", "first", "First review"),
        ("two", "second", "Second review"),
    ] {
        begin_assist(&mut editor, range, request, group);
        editor
            .apply_inline_result(
                request,
                "test-session",
                &InlineAssistResult {
                    needs_agent: None,
                    replacement: None,
                    comments: vec![note(1, 2, message)],
                },
                &mut frame,
                &mut runtime,
            )
            .await
            .unwrap();
    }
    assert_eq!(editor.inline_comments.len(), 2);
    assert_eq!(
        editor
            .inline_comment_display_messages(editor.current_buffer())
            .len(),
        1
    );
    assert!(
        editor.inline_comment_display_messages(editor.current_buffer())[0]
            .1
            .contains("[2/2]")
    );
    editor.inline_assist.as_mut().unwrap().request_id = Some("refine".into());
    editor
        .apply_inline_result(
            "refine",
            "test-session",
            &InlineAssistResult {
                needs_agent: None,
                replacement: None,
                comments: vec![note(1, 1, "Refined")],
            },
            &mut frame,
            &mut runtime,
        )
        .await
        .unwrap();
    assert_eq!(editor.inline_comments.len(), 2);
    assert!(editor
        .inline_comments
        .iter()
        .any(|comment| comment.message == "First review"));
    assert!(!editor
        .inline_comments
        .iter()
        .any(|comment| comment.message == "Second review"));
    editor.navigate_inline_comment(true);
    assert!(
        editor.inline_comment_display_messages(editor.current_buffer())[0]
            .1
            .contains("First review")
    );
    editor
        .execute(&Action::UndoInlineAssist, &mut frame, &mut runtime)
        .await
        .unwrap();
    assert_eq!(editor.inline_comments.len(), 1);
    assert_eq!(editor.inline_comments[0].message, "First review");
}

#[tokio::test]
async fn inline_annotations_survive_source_edits_as_outdated() {
    let mut editor = editor("alpha\nbeta\n", 70, 16, false);
    editor.set_inline_comment("Review alpha");
    let id = editor.inline_comments[0].id;
    editor.begin_transaction("change annotated line");
    editor.replace_range(
        TextRange::new(TextPosition::new(0, 1), TextPosition::new(0, 2)),
        "X",
    );
    editor.commit_transaction(editor.cursor_snapshot());
    assert_eq!(editor.inline_comments[0].id, id);
    assert!(editor.inline_comments[0].stale);
    editor
        .test_execute_production_action(Action::Undo)
        .await
        .unwrap();
    assert!(!editor.inline_comments[0].stale);
}

#[tokio::test]
async fn inline_annotation_range_survives_whole_target_replacement_and_undo() {
    let original = "fn example() {\n    let total = 0;\n    use_value(total);\n}\n";
    let replacement = "fn example() {\n    let sum = 0;\n    use_value(sum);\n}\n";
    let mut editor = editor(original, 70, 16, false);
    editor.replace_inline_comment_group(
        "kept",
        "session",
        "review",
        0,
        &[note(2, 3, "Accumulator")],
    );
    let id = editor.inline_comments[0].id;
    editor.begin_transaction("replace function");
    editor.replace_range(
        TextRange::new(TextPosition::new(0, 0), TextPosition::new(4, 0)),
        replacement,
    );
    editor.commit_transaction(editor.cursor_snapshot());
    assert_eq!(
        editor.inline_comments[0].lines(editor.current_buffer()),
        (1, 2)
    );
    assert!(editor.inline_comments[0].stale);
    editor
        .test_execute_production_action(Action::Undo)
        .await
        .unwrap();
    assert_eq!(editor.inline_comments[0].id, id);
    assert_eq!(
        editor.inline_comments[0].lines(editor.current_buffer()),
        (1, 2)
    );
    assert!(!editor.inline_comments[0].stale);
}
