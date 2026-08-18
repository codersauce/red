//! Resolve configured command-entry aliases before surface-local navigation.

use super::*;

impl Editor {
    pub(super) fn is_command_mode_entry(action: &KeyAction) -> bool {
        match action {
            KeyAction::Single(Action::EnterMode(Mode::Command)) => true,
            KeyAction::Multiple(actions) => {
                !actions.is_empty()
                    && actions
                        .iter()
                        .all(|action| matches!(action, Action::EnterMode(Mode::Command)))
            }
            KeyAction::Repeating(count, action) => {
                *count > 0 && Self::is_command_mode_entry(action)
            }
            _ => false,
        }
    }

    pub(super) fn command_mode_key_action(&self, event: &Event, mode: Mode) -> Option<KeyAction> {
        let key = Self::key_string_for_event(event)?;
        let visual;
        let mappings = match mode {
            Mode::Normal => &self.config.keys.normal,
            Mode::Visual | Mode::VisualLine | Mode::VisualBlock => {
                visual = self.visual_key_mappings_for_mode(mode);
                &visual
            }
            Mode::Insert | Mode::Command | Mode::Search => return None,
        };
        let action = mappings
            .get(&key)
            .or_else(|| (key == "Space").then(|| mappings.get(" ")).flatten())?;
        Self::is_command_mode_entry(action).then(|| action.clone())
    }

    pub(super) fn panel_command_mode_key_action(&self, event: &Event) -> Option<KeyAction> {
        self.command_mode_key_action(event, self.panel_manager.command_mode_keymap()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command_entry() -> KeyAction {
        KeyAction::Single(Action::EnterMode(Mode::Command))
    }

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn editor() -> Editor {
        let mut config: Config = toml::from_str(include_str!("../../default_config.toml")).unwrap();
        config.keys.normal.insert(";".into(), command_entry());
        config.keys.normal.insert("!".into(), command_entry());
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size_and_preferences(
            lsp,
            80,
            24,
            config,
            Theme::default(),
            vec![Buffer::new(None, "one; two; three\n".into())],
            PreferencesStore::in_memory(),
        )
        .unwrap();
        editor.test_disable_terminal_output();
        editor
    }

    fn text_panel(editor: &mut Editor) {
        editor.test_create_text_panel(
            "test",
            plugin::PanelConfig {
                side: plugin::PanelSide::Right,
                width: 32,
                composer: Some(plugin::TextPanelComposerConfig {
                    placeholder: "Ask".into(),
                    rows: 3,
                }),
                ..plugin::PanelConfig::default()
            },
        );
        editor.test_update_text_panel(
            "test",
            vec![plugin::TextPanelBlock {
                id: "answer".into(),
                kind: plugin::TextPanelBlockKind::Text,
                format: plugin::TextPanelBlockFormat::Plain,
                text: "one; two; three".into(),
            }],
        );
        assert!(editor.test_focus_panel("test"));
    }

    fn dispatch(editor: &mut Editor, code: KeyCode) -> Option<KeyAction> {
        editor.handle_event(&key(code)).unwrap()
    }

    fn draft(editor: &Editor) -> String {
        editor
            .panel_manager
            .snapshot(80)
            .panels
            .into_iter()
            .find(|panel| panel.id == "test")
            .unwrap()
            .text
            .unwrap()
            .composer
            .unwrap()
            .text
    }

    #[test]
    fn command_aliases_are_inherited_by_all_visual_modes() {
        let mut editor = editor();
        for mode in [
            Mode::Normal,
            Mode::Visual,
            Mode::VisualLine,
            Mode::VisualBlock,
        ] {
            editor.mode = mode;
            for character in [':', ';', '!'] {
                assert_eq!(
                    dispatch(&mut editor, KeyCode::Char(character)),
                    Some(command_entry()),
                    "{mode:?} {character}"
                );
            }
        }
        editor
            .config
            .keys
            .visual
            .insert(";".into(), KeyAction::None);
        for mode in [Mode::Visual, Mode::VisualLine, Mode::VisualBlock] {
            editor.mode = mode;
            assert_eq!(
                dispatch(&mut editor, KeyCode::Char(';')),
                Some(KeyAction::None)
            );
        }
        editor
            .config
            .keys
            .visual_line
            .insert(";".into(), command_entry());
        editor.mode = Mode::VisualLine;
        assert_eq!(
            dispatch(&mut editor, KeyCode::Char(';')),
            Some(command_entry())
        );
    }

    #[test]
    fn command_aliases_do_not_steal_editor_text_or_find_targets() {
        let mut editor = editor();
        editor.mode = Mode::Insert;
        assert_eq!(
            dispatch(&mut editor, KeyCode::Char(';')),
            Some(KeyAction::Single(Action::InsertCharAtCursorPos(';')))
        );
        editor.mode = Mode::Normal;
        dispatch(&mut editor, KeyCode::Char('f'));
        assert_eq!(
            dispatch(&mut editor, KeyCode::Char(';')),
            Some(KeyAction::Single(Action::FindCharForward {
                target: ';',
                count: 1
            }))
        );
        editor.mode = Mode::Command;
        assert_eq!(dispatch(&mut editor, KeyCode::Char(';')), None);
        assert_eq!(editor.command, ";");
        editor.begin_search(SearchDirection::Forward);
        assert_eq!(dispatch(&mut editor, KeyCode::Char(';')), None);
        assert_eq!(editor.active_search_text(), Some(";"));
    }

    #[test]
    fn command_aliases_precede_row_panel_navigation_without_hardcoded_keys() {
        let mut editor = editor();
        editor
            .config
            .keys
            .normal
            .insert("j".into(), command_entry());
        editor.test_create_panel("tree", plugin::PanelConfig::default());
        assert!(editor.test_focus_panel("tree"));
        for character in [':', ';', '!', 'j'] {
            assert_eq!(
                dispatch(&mut editor, KeyCode::Char(character)),
                Some(command_entry())
            );
        }
        editor
            .config
            .keys
            .normal
            .insert(":".into(), KeyAction::None);
        assert_ne!(
            dispatch(&mut editor, KeyCode::Char(':')),
            Some(command_entry())
        );
    }

    #[test]
    fn command_aliases_preserve_composer_input_and_pending_commands() {
        let mut editor = editor();
        text_panel(&mut editor);
        assert!(editor.test_focus_text_panel_composer("test"));
        editor
            .handle_event(&Event::Paste("one; two; three".into()))
            .unwrap();
        dispatch(&mut editor, KeyCode::Char(';'));
        assert_eq!(draft(&editor), "one; two; three;");
        dispatch(&mut editor, KeyCode::Esc);
        assert_eq!(
            editor.panel_manager.focused_text_panel_cursor_mode(),
            Some(Mode::Normal)
        );
        for character in [':', ';', '!'] {
            assert_eq!(
                dispatch(&mut editor, KeyCode::Char(character)),
                Some(command_entry())
            );
        }
        for prefix in ['f', 'r', 'd'] {
            dispatch(&mut editor, KeyCode::Char(prefix));
            assert_ne!(
                dispatch(&mut editor, KeyCode::Char(';')),
                Some(command_entry()),
                "{prefix};"
            );
            dispatch(&mut editor, KeyCode::Esc);
            // Escape from idle Normal blurs the composer; focus it for the next case.
            assert!(editor.test_focus_text_panel_composer("test"));
        }
        dispatch(&mut editor, KeyCode::Char('/'));
        assert_eq!(
            editor.panel_manager.focused_text_panel_cursor_mode(),
            Some(Mode::Search)
        );
        assert_ne!(
            dispatch(&mut editor, KeyCode::Char(';')),
            Some(command_entry())
        );
        dispatch(&mut editor, KeyCode::Esc);
        assert_eq!(
            dispatch(&mut editor, KeyCode::Char(';')),
            Some(command_entry())
        );
    }

    #[test]
    fn command_aliases_preserve_transcript_search_and_find_targets() {
        let mut editor = editor();
        text_panel(&mut editor);
        for character in [':', ';', '!'] {
            assert_eq!(
                dispatch(&mut editor, KeyCode::Char(character)),
                Some(command_entry())
            );
        }
        dispatch(&mut editor, KeyCode::Char('f'));
        assert_ne!(
            dispatch(&mut editor, KeyCode::Char(';')),
            Some(command_entry())
        );
        assert_eq!(
            dispatch(&mut editor, KeyCode::Char(';')),
            Some(command_entry())
        );
        dispatch(&mut editor, KeyCode::Char('/'));
        assert_eq!(
            editor.panel_manager.focused_text_panel_cursor_mode(),
            Some(Mode::Search)
        );
        assert_ne!(
            dispatch(&mut editor, KeyCode::Char(';')),
            Some(command_entry())
        );
        dispatch(&mut editor, KeyCode::Esc);
        assert_eq!(
            dispatch(&mut editor, KeyCode::Char(';')),
            Some(command_entry())
        );
        dispatch(&mut editor, KeyCode::Char('v'));
        assert_eq!(
            editor.panel_manager.focused_text_panel_cursor_mode(),
            Some(Mode::Visual)
        );
        assert_eq!(
            dispatch(&mut editor, KeyCode::Char(';')),
            Some(command_entry())
        );
        editor
            .config
            .keys
            .visual
            .insert(";".into(), KeyAction::None);
        editor
            .config
            .keys
            .visual
            .insert(":".into(), KeyAction::None);
        for character in [':', ';'] {
            assert_ne!(
                dispatch(&mut editor, KeyCode::Char(character)),
                Some(command_entry())
            );
        }
    }

    #[tokio::test]
    async fn command_alias_round_trip_keeps_composer_draft_and_focus() {
        let mut editor = editor();
        text_panel(&mut editor);
        assert!(editor.test_focus_text_panel_composer("test"));
        editor
            .handle_event(&Event::Paste("keep this draft".into()))
            .unwrap();
        dispatch(&mut editor, KeyCode::Esc);
        let before = editor.panel_manager.snapshot(80);
        let mut frame = RenderBuffer::new(80, 24, &Style::default());
        let mut runtime = Runtime::new();
        for code in [KeyCode::Char(';'), KeyCode::Char(';'), KeyCode::Esc] {
            editor
                .process_editor_event(
                    key(code),
                    &mut frame,
                    &mut runtime,
                    EventRenderMode::Immediate,
                )
                .await
                .unwrap();
            if code == KeyCode::Char(';') {
                assert!(editor.is_command());
            }
        }
        assert_eq!(editor.mode, Mode::Normal);
        assert_eq!(editor.panel_manager.focused_panel_id(), Some("test"));
        assert_eq!(editor.panel_manager.snapshot(80), before);
    }

    #[tokio::test]
    async fn command_aliases_work_in_workspaces_without_stealing_filters() {
        let mut editor = editor();
        editor
            .workspace_manager
            .open("test".into(), plugin::WorkspaceConfig::default());
        let mut frame = RenderBuffer::new(80, 24, &Style::default());
        let mut runtime = Runtime::new();
        editor
            .process_editor_event(
                key(KeyCode::Char(';')),
                &mut frame,
                &mut runtime,
                EventRenderMode::Immediate,
            )
            .await
            .unwrap();
        assert!(editor.is_command());
        dispatch(&mut editor, KeyCode::Char('x'));
        assert_eq!(editor.command, "x");
        editor
            .process_editor_event(
                key(KeyCode::Esc),
                &mut frame,
                &mut runtime,
                EventRenderMode::Immediate,
            )
            .await
            .unwrap();
        assert!(editor.workspace_manager.is_active());
        assert_eq!(editor.mode, Mode::Normal);
        dispatch(&mut editor, KeyCode::Char('/'));
        assert!(editor.workspace_manager.is_filtering());
        assert_ne!(
            dispatch(&mut editor, KeyCode::Char(';')),
            Some(command_entry())
        );
        assert!(editor.workspace_manager.is_filtering());
        dispatch(&mut editor, KeyCode::Esc);
        assert_eq!(
            dispatch(&mut editor, KeyCode::Char(';')),
            Some(command_entry())
        );
        dispatch(&mut editor, KeyCode::Char('g'));
        assert_ne!(
            dispatch(&mut editor, KeyCode::Char(';')),
            Some(command_entry())
        );
    }

    #[test]
    fn default_semicolon_keeps_local_character_search() {
        let mut editor = editor();
        editor
            .config
            .keys
            .normal
            .insert(";".into(), KeyAction::Single(Action::RepeatCharSearch(1)));
        assert_eq!(
            dispatch(&mut editor, KeyCode::Char(';')),
            Some(KeyAction::Single(Action::RepeatCharSearch(1)))
        );
        text_panel(&mut editor);
        assert_ne!(
            dispatch(&mut editor, KeyCode::Char(';')),
            Some(command_entry())
        );
        assert!(editor.test_focus_text_panel_composer("test"));
        dispatch(&mut editor, KeyCode::Esc);
        assert_ne!(
            dispatch(&mut editor, KeyCode::Char(';')),
            Some(command_entry())
        );
        assert_eq!(
            dispatch(&mut editor, KeyCode::Char(':')),
            Some(command_entry())
        );
    }

    #[tokio::test]
    async fn visual_command_alias_preserves_the_selected_command_range() {
        let mut editor = editor();
        let mut frame = RenderBuffer::new(80, 24, &Style::default());
        let mut runtime = Runtime::new();
        for mode in [Mode::Visual, Mode::VisualLine, Mode::VisualBlock] {
            editor
                .execute(&Action::EnterMode(mode), &mut frame, &mut runtime)
                .await
                .unwrap();
            editor
                .process_editor_event(
                    key(KeyCode::Char(';')),
                    &mut frame,
                    &mut runtime,
                    EventRenderMode::Immediate,
                )
                .await
                .unwrap();
            assert_eq!(editor.mode, Mode::Command);
            assert_eq!(editor.command, "'<,'>");
            editor
                .process_editor_event(
                    key(KeyCode::Esc),
                    &mut frame,
                    &mut runtime,
                    EventRenderMode::Immediate,
                )
                .await
                .unwrap();
        }
    }
}
