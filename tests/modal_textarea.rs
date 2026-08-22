mod common;

use common::EditorHarness;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use red::{
    buffer::Buffer,
    config::Config,
    editing::{TextArea, TextAreaOutcome},
    editor::Mode,
    text_layout::{LayoutOptions, TextLayout},
};

fn event(character: char) -> Event {
    let code = if character == '\u{1b}' {
        KeyCode::Esc
    } else {
        KeyCode::Char(character)
    };
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

#[test]
fn word_wrapped_vertical_motion_uses_the_display_projection() {
    let text = "one two three";
    let options = LayoutOptions::word(7);
    let layout = TextLayout::new(text, options);
    let mut area = TextArea::new(text);
    area.set_cursor(0);
    let down = Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let up = Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    area.handle_event_with_layout_options(&down, options);
    assert_eq!(area.cursor(), 8);
    assert_eq!(layout.position(area.cursor()).unwrap().row, 1);
    area.handle_event_with_layout_options(&up, options);
    assert_eq!(area.cursor(), 0);
    assert_eq!(area.text(), text);

    // The old API remains character-wrapped for every caller that has not opted in.
    area.handle_event(&down, 7);
    assert_eq!(area.cursor(), 7);
}

#[test]
fn word_backspace_is_grapheme_safe_undoable_and_ignores_release() {
    for modifiers in [KeyModifiers::ALT, KeyModifiers::CONTROL] {
        let original = "one 👨‍👩‍👧e\u{301} rest";
        let mut area = TextArea::new(original);
        area.set_cursor(6);
        let press = Event::Key(KeyEvent::new(KeyCode::Backspace, modifiers));
        let release = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Backspace,
            modifiers,
            KeyEventKind::Release,
        ));
        area.handle_event(&release, 80);
        assert_eq!(area.text(), original);
        area.handle_event(&press, 80);
        assert_eq!(area.text(), "one  rest");
        assert_eq!(area.cursor(), 4);
        assert!(area.undo());
        assert_eq!(area.text(), original);
        assert_eq!(area.cursor(), 6);
        assert!(area.redo());
        assert_eq!(area.text(), "one  rest");
        area.handle_event(
            &Event::Key(KeyEvent::new_with_kind(
                KeyCode::Backspace,
                modifiers,
                KeyEventKind::Repeat,
            )),
            80,
        );
        assert_eq!(area.text(), " rest");
        area.set_cursor(0);
        area.handle_event(&press, 80);
        assert_eq!(area.text(), " rest");
    }
}

#[test]
fn word_wrap_does_not_turn_visual_rows_into_logical_lines() {
    let mut area = TextArea::new("one two three\nlast");
    let options = LayoutOptions::word(7);
    area.set_mode(Mode::Normal);
    area.set_cursor(0);
    for character in ['d', 'd'] {
        area.handle_event_with_layout_options(&event(character), options);
    }
    assert_eq!(area.text(), "last");
    area.handle_event_with_layout_options(&event('u'), options);
    assert_eq!(area.text(), "one two three\nlast");
}

#[test]
fn embedded_textarea_formats_current_and_visual_lines_as_one_undoable_change() {
    let mut area = TextArea::new("alpha beta gamma delta\nepsilon zeta eta theta");
    area.set_cursor(0);
    area.set_mode(Mode::Normal);

    for character in "gqq".chars() {
        assert_eq!(
            area.handle_event(&event(character), 12),
            TextAreaOutcome::Changed
        );
    }
    assert_eq!(
        area.text(),
        "alpha beta\ngamma delta\nepsilon zeta eta theta"
    );
    assert_eq!(area.buffer().pos, (0, 1));
    assert!(area.undo());
    assert_eq!(
        area.text(),
        "alpha beta gamma delta\nepsilon zeta eta theta"
    );

    for character in "VGgq".chars() {
        assert_eq!(
            area.handle_event(&event(character), 12),
            TextAreaOutcome::Changed
        );
    }
    assert_eq!(
        area.text(),
        "alpha beta\ngamma delta\nepsilon zeta\neta theta"
    );
    assert_eq!(area.mode(), Mode::Normal);
}

#[tokio::test]
async fn embedded_textareas_match_file_editor_for_shared_vim_sequences() {
    let cases = [
        ("counted word delete", "one two three four", "2dw"),
        ("operator motion count", "one two three four", "d2w"),
        ("change word", "first second", "cwnew\u{1b}"),
        ("change word undo grouping", "first second", "cwnew\u{1b}u"),
        ("insert undo grouping", "first second", "inew\u{1b}u"),
        ("open line undo grouping", "first second", "onew\u{1b}u"),
        ("inner word", "first second", "diw"),
        ("around parentheses", "(first) tail", "da("),
        ("inside quotes", "\"first second\" tail", "di\""),
        ("character find delete", "one: two: three", "df:"),
        ("backward word", "one two three", "web"),
        ("end word", "one two three", "2e"),
        ("dot repeat", "one two three", "dw."),
        ("visual deletion", "one two three", "vwx"),
        ("next paragraph", "one\n\ntwo\n\nthree", "}"),
        ("counted paragraphs", "one\n\ntwo\n\nthree", "2}"),
        ("previous paragraph", "one\n\ntwo\n\nthree", "G{"),
        (
            "whitespace-only lines do not split paragraphs",
            "one\n   \ntwo\n\nthree",
            "}",
        ),
        ("linewise paragraph deletion", "one\n\ntwo", "d}"),
        ("characterwise paragraph deletion", "alpha\n\nbeta", "2ld}"),
        ("counted paragraph deletion", "one\n\ntwo\n\nthree", "d2}"),
        ("paragraph yank shape", "one\n\ntwo", "y}p"),
        ("paragraph visual deletion", "one\n\ntwo", "v}x"),
        ("next sentence", "One.  Two! Three?", ")"),
        ("counted sentence", "One.  Two! Three?", "2)"),
        ("previous sentence", "One.  Two! Three?", "2)("),
        ("sentence deletion", "One.  Two! Three?", "d)"),
        ("counted sentence deletion", "One.  Two! Three?", "d2)"),
        ("inner sentence", "One.  Two! Three?", "dis"),
        ("around sentence", "One.  Two! Three?", "das"),
        ("counted inner sentence", "One.  Two! Three?", "d3is"),
        ("counted around sentence", "One.  Two! Three?", "d2as"),
        ("sentence whitespace object", "One.  Two! Three?", "4ldis"),
        ("sentence around whitespace", "One.  Two! Three?", "4ldas"),
        ("visual sentence", "One.  Two! Three?", "v)x"),
        ("visual inner sentence", "One.  Two! Three?", "visx"),
        ("counted visual sentence", "One.  Two! Three?", "v2isx"),
        ("unicode sentence", "Olá! 👨‍👩‍👧 e\u{301}lan. Fim", "das"),
        ("sentence dot repeat", "One.  Two! Three?", "das."),
        ("sentence undo grouping", "One.  Two!", "cisnew\u{1b}u"),
    ];

    for (name, initial, raw_keys) in cases {
        let keys = raw_keys.to_string();
        let config = toml::from_str::<Config>(include_str!("../default_config.toml")).unwrap();
        let mut editor = EditorHarness::with_config(Buffer::new(None, initial.to_string()), config);
        let mut area = TextArea::new(initial);
        area.set_cursor(0);
        area.set_mode(Mode::Normal);

        for character in keys.chars() {
            let input = event(character);
            editor.execute_event(input.clone()).await.unwrap();
            assert_eq!(
                area.handle_event(&input, 80),
                TextAreaOutcome::Changed,
                "{name}: textarea rejected {character:?}"
            );
        }

        assert_eq!(area.text(), editor.buffer_contents(), "{name}: text");
        assert_eq!(area.mode(), editor.mode(), "{name}: mode");
        assert_eq!(
            area.buffer().pos,
            editor.cursor_position(),
            "{name}: cursor"
        );
    }
}

#[test]
fn embedded_paragraph_operators_match_neovim_register_shapes() {
    for (keys, expected_text, expected_register, expected_linewise) in [
        ("d}", "\nbeta", "alpha\n", true),
        ("2ld}", "al\n\nbeta", "pha", false),
    ] {
        let mut area = TextArea::new("alpha\n\nbeta");
        area.set_cursor(0);
        area.set_mode(Mode::Normal);

        for character in keys.chars() {
            assert_eq!(
                area.handle_event(&event(character), 80),
                TextAreaOutcome::Changed
            );
        }

        assert_eq!(area.text(), expected_text);
        assert_eq!(area.register().text, expected_register);
        assert_eq!(area.register().linewise, expected_linewise);
    }
}

#[test]
fn embedded_textareas_do_not_claim_editor_owned_structural_objects() {
    let mut area = TextArea::new("fn first() { value(); }");
    area.set_mode(Mode::Normal);

    for character in "dif".chars() {
        assert_eq!(
            area.handle_event(&event(character), 80),
            TextAreaOutcome::Changed
        );
    }

    assert_eq!(area.text(), "fn first() { value(); }");
    assert_eq!(area.mode(), Mode::Normal);
    assert_eq!(area.register().text, "");
}

#[test]
fn separate_textareas_keep_mode_history_registers_and_undo_independent() {
    let mut first = TextArea::new("one two");
    let mut second = TextArea::new("alpha beta");
    first.set_cursor(0);
    first.set_mode(Mode::Normal);
    for character in "dw".chars() {
        assert_eq!(
            first.handle_event(&event(character), 80),
            TextAreaOutcome::Changed
        );
    }

    assert_eq!(first.text(), "two");
    assert_eq!(first.register().text, "one ");
    assert_eq!(second.text(), "alpha beta");
    assert_eq!(second.mode(), Mode::Insert);
    assert_eq!(second.register().text, "");

    second.set_mode(Mode::Normal);
    second.handle_event(&event('u'), 80);
    assert_eq!(second.text(), "alpha beta");
    assert_eq!(first.text(), "two");
}

#[tokio::test]
async fn embedded_changes_match_editor_undo_and_redo_transactions() {
    let config = toml::from_str::<Config>(include_str!("../default_config.toml")).unwrap();
    let mut editor = EditorHarness::with_config(Buffer::new(None, "first second".into()), config);
    let mut area = TextArea::new("first second");
    area.set_cursor(0);
    area.set_mode(Mode::Normal);

    let mut events = "cwnew\u{1b}u".chars().map(event).collect::<Vec<_>>();
    events.push(Event::Key(KeyEvent::new(
        KeyCode::Char('r'),
        KeyModifiers::CONTROL,
    )));

    for input in events {
        editor.execute_event(input.clone()).await.unwrap();
        assert_eq!(area.handle_event(&input, 80), TextAreaOutcome::Changed);
        assert_eq!(area.text(), editor.buffer_contents());
        assert_eq!(area.buffer().pos, editor.cursor_position());
    }

    assert_eq!(area.text(), "new second");
}
