//! Translate Helix register notation at the terminal boundary.
use super::*;
use crossterm::event::{MediaKeyCode, ModifierKeyCode};
use felix_core::{MacroCode, MacroKey, NamedKey};

// An exhaustive mapping in both directions keeps the terminal types out of Felix.
macro_rules! terminal_keys {
    ($($terminal:ident $(($inner:path))? => $code:ident),+ $(,)?) => {
        fn macro_key(key: KeyEvent) -> MacroKey {
            let code = match key.code {
                KeyCode::Char(ch) => MacroCode::Char(ch),
                KeyCode::F(n) => MacroCode::Function(n),
                KeyCode::BackTab => MacroCode::Named(NamedKey::Tab),
                $(KeyCode::$terminal $(($inner))? => MacroCode::Named(NamedKey::$code)),+
            };
            MacroKey {
                code,
                shift: key.modifiers.contains(KeyModifiers::SHIFT) || key.code == KeyCode::BackTab,
                control: key.modifiers.contains(KeyModifiers::CONTROL),
                alt: key.modifiers.contains(KeyModifiers::ALT),
                meta: key.modifiers.contains(KeyModifiers::SUPER),
            }
        }

        pub(super) fn terminal_key(key: MacroKey) -> KeyEvent {
            let mut modifiers = KeyModifiers::NONE;
            modifiers.set(KeyModifiers::SHIFT, key.shift);
            modifiers.set(KeyModifiers::CONTROL, key.control);
            modifiers.set(KeyModifiers::ALT, key.alt);
            modifiers.set(KeyModifiers::SUPER, key.meta);
            let code = match key.code {
                MacroCode::Char(ch) => KeyCode::Char(ch),
                MacroCode::Function(n) => KeyCode::F(n),
                MacroCode::Named(NamedKey::Tab) if key.shift => KeyCode::BackTab,
                $(MacroCode::Named(NamedKey::$code) => KeyCode::$terminal $(($inner))?),+
            };
            KeyEvent::new(code, modifiers)
        }
    };
}
terminal_keys! {
    Enter => Enter,
    Tab => Tab,
    Esc => Esc,
    Backspace => Backspace,
    Delete => Delete,
    Insert => Insert,
    Left => Left,
    Right => Right,
    Up => Up,
    Down => Down,
    Home => Home,
    End => End,
    PageUp => PageUp,
    PageDown => PageDown,
    Null => Null,
    CapsLock => CapsLock,
    ScrollLock => ScrollLock,
    NumLock => NumLock,
    PrintScreen => PrintScreen,
    Pause => Pause,
    Menu => Menu,
    KeypadBegin => KeypadBegin,
    Media(MediaKeyCode::Play) => MediaPlay,
    Media(MediaKeyCode::Pause) => MediaPause,
    Media(MediaKeyCode::PlayPause) => MediaPlayPause,
    Media(MediaKeyCode::Reverse) => MediaReverse,
    Media(MediaKeyCode::Stop) => MediaStop,
    Media(MediaKeyCode::FastForward) => MediaFastForward,
    Media(MediaKeyCode::Rewind) => MediaRewind,
    Media(MediaKeyCode::TrackNext) => MediaTrackNext,
    Media(MediaKeyCode::TrackPrevious) => MediaTrackPrevious,
    Media(MediaKeyCode::Record) => MediaRecord,
    Media(MediaKeyCode::LowerVolume) => MediaLowerVolume,
    Media(MediaKeyCode::RaiseVolume) => MediaRaiseVolume,
    Media(MediaKeyCode::MuteVolume) => MediaMuteVolume,
    Modifier(ModifierKeyCode::LeftShift) => LeftShift,
    Modifier(ModifierKeyCode::LeftControl) => LeftControl,
    Modifier(ModifierKeyCode::LeftAlt) => LeftAlt,
    Modifier(ModifierKeyCode::LeftSuper) => LeftSuper,
    Modifier(ModifierKeyCode::LeftHyper) => LeftHyper,
    Modifier(ModifierKeyCode::LeftMeta) => LeftMeta,
    Modifier(ModifierKeyCode::RightShift) => RightShift,
    Modifier(ModifierKeyCode::RightControl) => RightControl,
    Modifier(ModifierKeyCode::RightAlt) => RightAlt,
    Modifier(ModifierKeyCode::RightSuper) => RightSuper,
    Modifier(ModifierKeyCode::RightHyper) => RightHyper,
    Modifier(ModifierKeyCode::RightMeta) => RightMeta,
    Modifier(ModifierKeyCode::IsoLevel3Shift) => IsoLevel3Shift,
    Modifier(ModifierKeyCode::IsoLevel5Shift) => IsoLevel5Shift,
}

impl Editor {
    fn felix_macro_surface(&self) -> bool {
        self.config.editing_model == crate::config::EditingModel::Helix
            && !self.panel_manager.has_focused_panel()
            && !self.workspace_manager.is_active()
            && !self
                .current_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.is_sensitive_input())
    }

    pub(in crate::editor) fn record_felix_macro_key(&mut self, event: &Event) {
        if self.felix.recorder.register().is_none() || !self.felix_macro_surface() {
            return;
        }
        if let Event::Key(key) = event {
            self.felix.recorder.record(|| macro_key(*key));
        }
    }

    pub(super) async fn play_felix_macro(
        &mut self,
        register: char,
        count: usize,
        frame: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let nested = self.felix.playback.active();
        let external;
        let values = if matches!(register, '+' | '*') {
            external = match self.read_felix_clipboard(register).await {
                Ok(value) => value,
                Err(error) => {
                    self.set_legacy_message(Some(error));
                    return Ok(());
                }
            };
            external.as_deref().map(std::borrow::Cow::Borrowed)
        } else if let Some(stored) = self.felix.registers.stored(register) {
            Some(std::borrow::Cow::Borrowed(stored))
        } else if self.felix.registers.exists(register) {
            let text = self.current_buffer().contents_snapshot();
            let document = felix_document_register_name(self.current_buffer().path());
            let Some(selections) = self.felix_active_selection() else {
                return Ok(());
            };
            Some(
                self.felix
                    .registers
                    .read(register, text.slice(..), selections, &document),
            )
        } else {
            None
        };
        if let Err(error) = self
            .felix
            .playback
            .start(register, values.as_deref(), count)
        {
            self.set_legacy_message(Some(error));
        } else if !nested {
            Box::pin(self.drain_felix_macro(frame, runtime)).await?;
        }
        Ok(())
    }

    pub(in crate::editor) async fn drain_felix_macro(
        &mut self,
        frame: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        self.begin_edit_batch();
        let result = async {
            loop {
                let key = match self.felix.playback.next_key(MACRO_MAX_REPLAY_EVENTS) {
                    Ok(Some(key)) => key,
                    Ok(None) => break,
                    Err(error) => {
                        self.set_legacy_message(Some(error));
                        break;
                    }
                };
                if !self.felix_macro_surface() {
                    self.set_legacy_message(Some("Macro stopped outside document input".into()));
                    break;
                }
                let event = Event::Key(terminal_key(key));
                let shell_was_running = self.felix.shell_job.is_some();
                let checkpoint = self.rpc_command_checkpoint();
                if let Some(action) = self.handle_event_with_runtime(&event, Some(runtime))? {
                    if self
                        .handle_resolved_key_action(&event, &action, frame, runtime)
                        .await?
                    {
                        self.set_legacy_message(Some("Macro stopped at quit".into()));
                        break;
                    }
                }
                if !shell_was_running {
                    if let Some(job) = &mut self.felix.shell_job {
                        job.tail = Some(std::mem::take(&mut self.felix.playback));
                        break;
                    }
                }
                if let Some(command) = self.rpc_command_started_since(checkpoint) {
                    let tail = std::mem::take(&mut self.felix.playback);
                    self.defer_rpc_action(command, rpc_commands::RpcContinuation::FelixMacro(tail));
                    break;
                }
                if self.replay_checkpoint(frame, runtime).await? {
                    // Finish document input through Felix so an interrupted
                    // insert retains its selections and undo checkpoint.
                    if let (true, Some(owner)) =
                        (self.felix.session.awaiting_input(), self.felix_owner())
                    {
                        self.execute_felix(Input::Key("<esc>"), owner, frame, runtime)
                            .await?;
                    }
                    if let (true, Some(owner)) = (
                        self.felix.session.mode != felix_core::Mode::Normal,
                        self.felix_owner(),
                    ) {
                        self.execute_felix(Input::Key("<esc>"), owner, frame, runtime)
                            .await?;
                    }
                    self.felix.session.cancel_pending();
                    break;
                }
            }
            Ok(())
        }
        .await;
        self.felix.playback = Default::default();
        let published = self.finish_edit_batch(frame, runtime).await;
        result.and(published)
    }
}
