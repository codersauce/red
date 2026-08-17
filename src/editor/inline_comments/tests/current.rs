use super::*;
use std::sync::Arc;

fn press(editor: &mut Editor, code: KeyCode) -> Option<KeyAction> {
    editor
        .current_dialog
        .as_mut()
        .unwrap()
        .handle_event(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

#[tokio::test]
async fn inline_popups_do_not_cover_their_card_near_the_viewport_bottom() {
    for height in [14, 22, 32] {
        let source = (0..60)
            .map(|line| format!("source {line}\n"))
            .collect::<String>();
        let mut editor = editor(&source, 110, height, false);
        editor.replace_inline_comment_group(
            "first",
            "provider",
            "one",
            0,
            &[note(
                35,
                52,
                "This annotation owns a range extending beyond the viewport.",
            )],
        );
        let first = editor.inline_comments[0].id;
        editor.replace_inline_comment_group(
            "second",
            "provider",
            "two",
            0,
            &[note(35, 52, "A second explanation on the same range.")],
        );
        let second = editor.inline_comments[1].id;
        editor.vtop = 28;
        editor.cy = 6;
        editor.sync_to_window();
        for action in [
            Action::FocusInlineComment(second),
            Action::OpenInlineComment(second),
            Action::NavigateOverlappingInlineComment {
                id: second,
                backwards: true,
                open: true,
            },
        ] {
            editor.test_execute_production_action(action).await.unwrap();
            let id = editor
                .current_dialog
                .as_ref()
                .unwrap()
                .inline_comment_id()
                .unwrap();
            assert!(id == first || id == second);
            let layout = editor.inline_comment_overlay_layout(id).unwrap();
            let (card_start, card_end) = layout.protected_rows.expect("visible annotation card");
            let (_, y, _, popup_height) = full_comment_rect(&editor, 110, height);
            assert!(
                y + popup_height <= card_start || y > card_end,
                "height={height}: popup {y}..{} covers card {card_start}..={card_end}",
                y + popup_height
            );
            assert!(
                y >= layout.viewport.y
                    && y + popup_height <= layout.viewport.y + layout.viewport.height
            );
        }
    }
}

#[tokio::test]
async fn inline_current_selection_is_pane_local_and_survives_arrivals_and_edits() {
    let mut editor = editor("alpha\nbeta\ngamma\ndelta\n", 160, 32, false);
    editor.replace_inline_comment_group(
        "notes",
        "provider",
        "request",
        0,
        &[
            note(1, 2, "First"),
            note(2, 3, "Second"),
            note(3, 3, "Third"),
        ],
    );
    let ids = editor
        .inline_comments
        .iter()
        .map(|comment| comment.id)
        .collect::<Vec<_>>();
    let first = editor.window_manager.active_stable_window_id().unwrap();
    let buffer = editor.current_buffer().id();
    editor.select_inline_comment_by_id(ids[0]);
    editor
        .test_execute_production_action(Action::SplitVertical)
        .await
        .unwrap();
    let second = editor.window_manager.active_stable_window_id().unwrap();
    assert_ne!(first, second);
    editor.select_inline_comment_by_id(ids[1]);
    assert_eq!(editor.inline_comment_selection(first, buffer), Some(ids[0]));
    assert_eq!(
        editor.inline_comment_selection(second, buffer),
        Some(ids[1])
    );
    let first_window = editor.window_manager.window(first).unwrap().clone();
    let second_window = editor.window_manager.window(second).unwrap().clone();
    let first_layout = editor.layout_for_window(&first_window);
    let second_layout = editor.layout_for_window(&second_window);
    assert!(first_layout
        .inline_comments
        .iter()
        .any(|row| row.content.text() == "First"));
    assert!(second_layout
        .inline_comments
        .iter()
        .any(|row| row.content.text() == "Second"));
    assert!(!Arc::ptr_eq(&first_layout, &second_layout));
    assert!(editor.inline_comment_is_current_at(&first_window, 0, false));
    assert!(!editor.inline_comment_is_current_at(&second_window, 0, false));
    assert_ne!(
        editor.theme.current_inline_comment_style().bg,
        editor.theme.inline_comment_style().bg
    );
    assert_ne!(
        editor.theme.current_inline_comment_guide_style().fg,
        editor.theme.inline_comment_guide_style().fg
    );

    editor.replace_inline_comment_group_in_buffer(
        editor.buffer_manager.active_index(),
        "arriving",
        "provider",
        "new",
        0,
        &[note(1, 3, "New answer")],
    );
    assert_eq!(editor.inline_comment_selection(first, buffer), Some(ids[0]));
    assert_eq!(editor.active_inline_comment(), Some(ids[1]));
    editor.begin_transaction("insert above");
    editor.replace_range(TextRange::insertion(TextPosition::new(0, 0)), "new\n");
    editor.commit_transaction(editor.cursor_snapshot());
    assert_eq!(editor.active_inline_comment(), Some(ids[1]));
    editor
        .test_execute_production_action(Action::DismissInlineCommentById(ids[1]))
        .await
        .unwrap();
    assert_eq!(editor.active_inline_comment(), Some(ids[2]));
    assert_eq!(editor.inline_comment_selection(first, buffer), Some(ids[0]));
    assert_eq!(editor.buffer_line(), 3);
}

#[tokio::test]
async fn inline_card_chooser_and_navigation_are_explicit_and_id_bound() {
    let mut editor = editor("alpha\nbeta\ngamma\n", 100, 28, false);
    editor.replace_inline_comment_group(
        "notes",
        "provider",
        "request",
        0,
        &[note(1, 2, "First"), note(2, 3, "Second")],
    );
    let first = editor.inline_comments[0].id;
    let second = editor.inline_comments[1].id;
    editor
        .test_execute_production_action(Action::FocusInlineComment(first))
        .await
        .unwrap();
    assert_eq!(
        press(&mut editor, KeyCode::Enter),
        Some(KeyAction::Single(Action::OpenInlineComment(first)))
    );
    for code in [KeyCode::Right, KeyCode::Char('l'), KeyCode::Char(']')] {
        assert_eq!(
            press(&mut editor, code),
            Some(KeyAction::Single(Action::NavigateInlineCommentCard {
                id: first,
                backwards: false
            }))
        );
    }
    assert_eq!(
        press(&mut editor, KeyCode::Char('x')),
        Some(KeyAction::Single(Action::DismissInlineCommentById(first)))
    );
    assert_eq!(
        press(&mut editor, KeyCode::Char('r')),
        Some(KeyAction::Single(Action::RefineInlineComment(first)))
    );
    assert_eq!(
        press(&mut editor, KeyCode::Char('d')),
        Some(KeyAction::Single(Action::ResolveInlineComment(first)))
    );
    assert_eq!(
        editor
            .current_dialog
            .as_mut()
            .unwrap()
            .activate_surface_action("expand-inline"),
        Some(KeyAction::Single(Action::OpenInlineComment(first)))
    );
    editor
        .test_execute_production_action(Action::ChooseInlineComment(first))
        .await
        .unwrap();
    assert_eq!(editor.active_inline_comment(), Some(first));
    assert_eq!(
        press(&mut editor, KeyCode::Enter),
        Some(KeyAction::Multiple(vec![
            Action::CloseDialog,
            Action::FocusInlineComment(first)
        ]))
    );
    press(&mut editor, KeyCode::Down);
    assert_eq!(editor.active_inline_comment(), Some(first));
    assert_eq!(
        press(&mut editor, KeyCode::Enter),
        Some(KeyAction::Multiple(vec![
            Action::CloseDialog,
            Action::FocusInlineComment(second)
        ]))
    );
    editor
        .test_execute_production_action(Action::OpenInlineComment(second))
        .await
        .unwrap();
    assert_eq!(
        press(&mut editor, KeyCode::Left),
        Some(KeyAction::Single(
            Action::NavigateOverlappingInlineComment {
                id: second,
                backwards: true,
                open: true
            }
        ))
    );
    assert_eq!(editor.current_buffer().contents(), "alpha\nbeta\ngamma\n");
}
