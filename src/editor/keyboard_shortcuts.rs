//! Context capture and dispatch for the non-destructive shortcut explorer.

use super::*;
use crate::ui::{
    KeyboardShortcuts, ShortcutEntry, ShortcutEvent, ShortcutHelpRegion, ShortcutTarget,
};

impl Editor {
    pub(super) fn open_keyboard_shortcuts(
        &mut self,
        runtime: Option<&Runtime>,
        clicked: Option<ShortcutHelpRegion>,
    ) {
        let commands = runtime
            .map(Runtime::registered_commands)
            .unwrap_or_default();
        let mut all = command_palette::shortcut_entries(&self.config.keys, &commands, None);
        all.extend(plugin::panel::all_text_panel_shortcuts());
        all.extend(crate::ui::common_shortcut_entries());
        for region in &self.shortcut_help_regions {
            all.extend(
                ShortcutEntry::from_actions(&region.context, &region.actions)
                    .into_iter()
                    .map(|entry| entry.in_context(&region.context)),
            );
        }
        let (context, current) = if let Some(region) = clicked {
            let entries = ShortcutEntry::from_actions(&region.context, &region.actions);
            (region.context, entries)
        } else if let Some(dialog) = &self.current_dialog {
            let context = dialog.shortcut_context().to_owned();
            let actions = dialog.surface_actions();
            let mut entries = ShortcutEntry::from_actions(&context, &actions);
            for (entry, action) in entries
                .iter_mut()
                .zip(actions.iter().filter(|action| action.enabled))
            {
                if action.priority != crate::ui::ActionPriority::Reference
                    && action.event().is_some()
                {
                    entry.target = Some(ShortcutTarget::Surface(action.id.clone()));
                }
            }
            (context, entries)
        } else if self.workspace_manager.is_active() {
            let (context, actions) = self.workspace_manager.shortcut_actions();
            let mut entries = ShortcutEntry::from_actions(&context, &actions);
            for (entry, action) in entries
                .iter_mut()
                .zip(actions.iter().filter(|action| action.enabled))
            {
                if action.priority != crate::ui::ActionPriority::Reference
                    && action.id != "?"
                    && action.id != "F1"
                {
                    entry.target = Some(ShortcutTarget::Workspace(action.id.clone()));
                }
            }
            (context, entries)
        } else if self.panel_manager.has_focused_panel() {
            let context = self.panel_manager.shortcut_context();
            let actions = self.panel_manager.surface_actions();
            let mut entries = ShortcutEntry::from_actions(&context, &actions);
            for (entry, action) in entries
                .iter_mut()
                .zip(actions.iter().filter(|action| action.enabled))
            {
                if action.priority != crate::ui::ActionPriority::Reference
                    && action.event().is_some()
                {
                    entry.target = Some(ShortcutTarget::Surface(action.id.clone()));
                }
            }
            (context, entries)
        } else {
            (
                format!("Editor · {:?}", self.mode),
                command_palette::shortcut_entries(&self.config.keys, &commands, Some(self.mode)),
            )
        };
        self.keyboard_shortcuts = Some(KeyboardShortcuts::new(context, current, all));
    }

    /// Captures help before the underlying surface can consume a key or mouse click.
    pub(super) fn handle_keyboard_shortcuts_event(
        &mut self,
        event: &Event,
        runtime: Option<&Runtime>,
    ) -> Option<KeyAction> {
        if let Some(help) = &mut self.keyboard_shortcuts {
            let result =
                help.handle_event(event, usize::from(self.size.0), usize::from(self.size.1));
            match result {
                ShortcutEvent::None => {}
                ShortcutEvent::Close => self.keyboard_shortcuts = None,
                ShortcutEvent::Activate(target) => {
                    self.keyboard_shortcuts = None;
                    return Some(match target {
                        ShortcutTarget::Editor(action) => action,
                        ShortcutTarget::Surface(id) => {
                            if let Some(dialog) = &mut self.current_dialog {
                                dialog.activate_surface_action(&id)
                            } else {
                                self.panel_manager
                                    .surface_actions()
                                    .iter()
                                    .find(|action| action.id == id && action.enabled)
                                    .and_then(crate::ui::UiAction::event)
                                    .and_then(|event| self.handle_panel_event(&event, runtime))
                            }
                            .unwrap_or(KeyAction::Single(Action::Refresh))
                        }
                        ShortcutTarget::Workspace(id) => self
                            .workspace_shortcut_action(id)
                            .unwrap_or(KeyAction::Single(Action::Refresh)),
                    });
                }
            }
            return Some(KeyAction::Single(Action::Refresh));
        }

        if let Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            ..
        }) = event
        {
            let clicked = self
                .shortcut_help_regions
                .iter()
                .rev()
                .find(|region| {
                    region
                        .rect
                        .contains(usize::from(*column), usize::from(*row))
                })
                .cloned();
            if let Some(region) = clicked {
                self.open_keyboard_shortcuts(runtime, Some(region));
                return Some(KeyAction::Single(Action::Refresh));
            }
        }

        let local_help = self.current_dialog.is_some()
            || self.workspace_manager.is_active()
            || self.panel_manager.has_focused_panel();
        let f1 = matches!(event, Event::Key(key) if key.code == KeyCode::F(1) && key.modifiers.is_empty());
        let workspace_help = self.current_dialog.is_none()
            && self.workspace_manager.is_active()
            && !self.workspace_manager.is_filtering()
            && matches!(event, Event::Key(key) if key.code == KeyCode::Char('?') && !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT));
        let editor_mapping = match self.mode {
            Mode::Normal => &self.config.keys.normal,
            Mode::Insert => &self.config.keys.insert,
            Mode::Visual => &self.config.keys.visual,
            Mode::VisualLine => &self.config.keys.visual_line,
            Mode::VisualBlock => &self.config.keys.visual_block,
            Mode::Command | Mode::Search => &self.config.keys.command,
        }
        .get("F1");
        let editor_help = editor_mapping
            .is_none_or(|mapping| matches!(mapping, KeyAction::Single(Action::KeyboardShortcuts)));
        if workspace_help || (f1 && !self.has_term() && (local_help || editor_help)) {
            self.open_keyboard_shortcuts(runtime, None);
            return Some(KeyAction::Single(Action::Refresh));
        }
        None
    }

    fn workspace_shortcut_action(&mut self, id: String) -> Option<KeyAction> {
        let event = self.workspace_manager.handle_action(
            id,
            usize::from(self.size.1),
            usize::from(self.size.0),
        )?;
        let id = event.workspace_id.clone();
        let notify = event.notify_plugin;
        let payload = serde_json::to_value(event).ok()?;
        let mut actions = Vec::new();
        if notify {
            actions.push(Action::NotifyPlugins(
                format!("workspace:event:{id}"),
                payload,
            ));
        }
        actions.push(Action::Refresh);
        Some(KeyAction::Multiple(actions))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct DraftDialog(Arc<Mutex<String>>);
    impl Component for DraftDialog {
        fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
            let theme = Theme::default();
            crate::ui::ActionBar::new(&self.surface_actions()).render(
                buffer,
                0,
                2,
                buffer.width,
                &theme,
                &theme.style,
            );
            Ok(())
        }
        fn surface_actions(&self) -> Vec<crate::ui::UiAction> {
            vec![crate::ui::UiAction::new("edit", "a", "Edit draft")]
        }
        fn handle_event(&mut self, event: &Event) -> Option<KeyAction> {
            if let Event::Key(KeyEvent {
                code: KeyCode::Char(c),
                ..
            }) = event
            {
                self.0.lock().unwrap().push(*c);
            }
            None
        }
    }
    fn editor() -> Editor {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            lsp,
            80,
            24,
            config,
            Theme::default(),
            vec![Buffer::new(None, "hello".into())],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        editor
    }
    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn shortcut_overlay_preserves_underlying_dialog_and_draft() {
        let mut editor = editor();
        let draft = Arc::new(Mutex::new("unsent draft".to_owned()));
        editor.current_dialog = Some(Box::new(DraftDialog(draft.clone())));
        editor.handle_event(&key(KeyCode::F(1))).unwrap();
        assert!(editor.keyboard_shortcuts.is_some());
        for c in "find".chars() {
            editor.handle_event(&key(KeyCode::Char(c))).unwrap();
        }
        editor.handle_event(&key(KeyCode::Esc)).unwrap();
        assert!(editor.keyboard_shortcuts.is_some());
        editor.handle_event(&key(KeyCode::Esc)).unwrap();
        assert!(editor.keyboard_shortcuts.is_none());
        assert!(editor.current_dialog.is_some());
        assert_eq!(*draft.lock().unwrap(), "unsent draft");
    }

    #[test]
    fn shortcut_help_click_uses_painted_target_and_keeps_dialog() {
        let mut editor = editor();
        editor.current_dialog = Some(Box::new(DraftDialog(Arc::new(Mutex::new(String::new())))));
        let mut buffer = RenderBuffer::new(80, 24, &Style::default());
        editor.render(&mut buffer).unwrap();
        let hit = editor.shortcut_help_regions.last().unwrap().rect;
        editor
            .handle_event(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: hit.x as u16,
                row: hit.y as u16,
                modifiers: KeyModifiers::NONE,
            }))
            .unwrap();
        assert!(editor.keyboard_shortcuts.is_some());
        assert!(editor.current_dialog.is_some());
        editor.render(&mut buffer).unwrap();
        assert!(buffer
            .cells
            .iter()
            .map(|cell| cell.c)
            .collect::<String>()
            .contains("Keyboard shortcuts"));
        assert_eq!(editor.render_cursor_position(), None);
    }

    #[test]
    fn shortcut_help_respects_editor_f1_override() {
        let mut editor = editor();
        editor
            .config
            .keys
            .normal
            .insert("F1".into(), KeyAction::Single(Action::Save));
        assert_eq!(
            editor.handle_event(&key(KeyCode::F(1))).unwrap(),
            Some(KeyAction::Single(Action::Save))
        );
        assert!(editor.keyboard_shortcuts.is_none());
    }
}
