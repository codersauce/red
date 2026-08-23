//! Publication boundaries for local edits and synthetic key replay.
//!
//! Text and undo state always change synchronously. Only derived notifications
//! and repainting are delayed. Unknown/external actions are barriers, so a new
//! action defaults to observing an up-to-date document.

use super::*;

const REPLAY_STEPS_PER_SLICE: usize = 512;

pub(super) struct EditBatch {
    depth: usize,
    suspended: usize,
    changed: Vec<BufferId>,
    started: Option<Instant>,
    steps: usize,
    budget: Duration,
    cancelled: bool,
    terminal_input: bool,
    pub(super) pending_input: VecDeque<Event>,
    #[cfg(test)]
    pub(super) published_changes: usize,
}

impl Default for EditBatch {
    fn default() -> Self {
        Self {
            depth: 0,
            suspended: 0,
            changed: Vec::new(),
            started: None,
            steps: 0,
            budget: Duration::from_millis(16),
            cancelled: false,
            terminal_input: false,
            pending_input: VecDeque::new(),
            #[cfg(test)]
            published_changes: 0,
        }
    }
}

impl EditBatch {
    pub(super) fn is_active(&self) -> bool {
        self.depth > 0 && self.suspended == 0
    }

    pub(super) fn record_change(&mut self, id: BufferId) {
        if !self.changed.contains(&id) {
            self.changed.push(id);
        }
    }
}

impl Editor {
    pub(super) fn render_is_deferred(&self) -> bool {
        self.defer_motion_render || self.edit_batch.is_active() || self.block_replay_depth > 0
    }

    /// Only source-local operations may run past an unpublished edit. In
    /// particular, save, LSP, plugin, dialog and document-switch actions are
    /// deliberately absent. This list is conservative for new action variants.
    pub(super) fn action_is_batch_local(action: &Action) -> bool {
        Self::action_is_navigation(action)
            || matches!(
                action,
                Action::EnterMode(
                    Mode::Normal
                        | Mode::Insert
                        | Mode::Visual
                        | Mode::VisualLine
                        | Mode::VisualBlock
                ) | Action::RepeatLastChange
                    | Action::SelectNextOccurrence
                    | Action::SelectPreviousOccurrence
                    | Action::SkipMultiSelection
                    | Action::RemoveActiveMultiSelection
                    | Action::ChangeMultiSelection
                    | Action::InsertAtMultiSelectionStart
                    | Action::AppendAtMultiSelectionEnd
                    | Action::DeleteMultiSelection
                    | Action::DeleteMultiSelectionBlackHole
                    | Action::ClearMultiSelection
                    | Action::PlayMacro(_)
                    | Action::InsertBlock
                    | Action::MoveToLineEnd
                    | Action::MoveToLineStart
                    | Action::MoveToFirstLineChar
                    | Action::MoveToLastLineChar
                    | Action::MoveToBottom
                    | Action::MoveToTop
                    | Action::MoveTo(_, _)
                    | Action::SetCursor(_, _)
                    | Action::GoToLine(_)
                    | Action::MoveToFilePercent(_)
                    | Action::SetMark(_)
                    | Action::Undo
                    | Action::Redo
                    | Action::InsertCharAtCursorPos(_)
                    | Action::InsertString(_)
                    | Action::InsertPastedText(_)
                    | Action::InsertNewLine
                    | Action::InsertLineAt(_, _)
                    | Action::InsertLineBelowCursor
                    | Action::InsertLineAtCursor
                    | Action::InsertTab
                    | Action::InsertText { .. }
                    | Action::DeletePreviousChar
                    | Action::DeleteCharAtCursorPos
                    | Action::DeleteCharAt(_, _)
                    | Action::DeleteCurrentLine
                    | Action::DeleteCurrentLines(_)
                    | Action::DeleteLineAt(_)
                    | Action::DeleteRange(_, _, _, _)
                    | Action::DeleteTextRange(_)
                    | Action::ChangeTextRange(_)
                    | Action::DeleteLinewiseRange(_)
                    | Action::ChangeLinewiseRange(_)
                    | Action::ChangeCurrentLine
                    | Action::ChangeCurrentLines(_)
                    | Action::DeleteWord
                    | Action::DeleteToLineEnd(_)
                    | Action::ChangeToLineEnd(_)
                    | Action::DeletePreviousChars(_)
                    | Action::ChangeCharsAtCursor(_)
                    | Action::Delete
                    | Action::ChangeSelection
                    | Action::Paste
                    | Action::PasteBefore
                    | Action::ReplaceCharsAtCursor { .. }
                    | Action::ReplaceLineAt(_, _)
                    | Action::ReplaceSelection(_)
                    | Action::Substitute(_)
                    | Action::ConfirmSubstitute(_)
                    | Action::JoinLines(_)
                    | Action::JoinLinesKeepSpaces(_)
                    | Action::JoinLinesInRange { .. }
                    | Action::ToggleCharCase(_)
                    | Action::TransformTextRange { .. }
                    | Action::TransformSelection(_)
                    | Action::IndentLine
                    | Action::UnindentLine
                    | Action::IndentSelection(_)
                    | Action::UnindentSelection(_)
                    | Action::ToggleCommentLines(_)
                    | Action::ToggleCommentRange(_)
                    | Action::ToggleCommentSelection
                    | Action::StartCommentOperator(_)
                    | Action::StartFormatOperator(_)
                    | Action::FormatTextRange(_)
                    | Action::FormatSelection
                    | Action::StartLowercaseOperator(_)
                    | Action::StartUppercaseOperator(_)
                    | Action::StartToggleCaseOperator(_)
                    | Action::SwapNextParameter
                    | Action::SwapPreviousParameter
                    | Action::SwapNextFunction
                    | Action::SwapPreviousFunction
            )
    }

    fn begin_edit_batch(&mut self) {
        if self.edit_batch.depth == 0 {
            self.edit_batch.started = Some(Instant::now());
            self.edit_batch.steps = 0;
            self.edit_batch.terminal_input = self.terminal_output_enabled;
        }
        self.edit_batch.depth += 1;
    }

    /// Keep failed deliveries queued; the next boundary can retry them.
    pub(super) async fn flush_edit_batch_changes(
        &mut self,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        while let Some(id) = self.edit_batch.changed.first().copied() {
            if let Some(index) = self
                .buffer_manager
                .iter()
                .position(|buffer| buffer.id() == id)
            {
                let revision = self.buffer_manager[index].revision();
                if !self.lsp_coordinator.is_revision_notified(id, revision) {
                    self.notify_buffer_change(index, runtime).await?;
                }
            }
            self.edit_batch.changed.remove(0);
        }
        Ok(())
    }

    pub(super) async fn flush_edit_batch_events(
        &mut self,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        self.edit_batch.suspended += 1;
        let result = self.flush_deferred_plugin_event(runtime).await;
        self.edit_batch.suspended -= 1;
        result
    }

    async fn publish_edit_batch(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        self.edit_batch.suspended += 1;
        let notification_result = self.flush_edit_batch_changes(runtime).await;
        let event_result = self.flush_deferred_plugin_event(runtime).await;
        let render_result = if self.defer_motion_render {
            Ok(())
        } else {
            self.flush_deferred_motion_render(buffer)
        };
        self.edit_batch.suspended -= 1;
        notification_result.and(event_result).and(render_result)
    }

    async fn finish_edit_batch(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        self.edit_batch.depth -= 1;
        if self.edit_batch.depth == 0 && self.block_replay_depth == 0 {
            if std::mem::take(&mut self.edit_batch.cancelled) {
                // Keep completed edits undoable, but do not replicate an unfinished
                // visual-block insert when the user interrupts it.
                self.pending_select_action = None;
                self.pending_semantic_change = None;
                self.waiting_key_action = None;
                self.waiting_command = None;
                self.pending_macro_action = None;
                self.pending_mark_action = None;
                self.pending_replace = false;
                self.repeater = None;
                self.clear_keymap_hints();
                self.edit_batch.depth = 1;
                let cleanup = self
                    .execute_with_tracking(&Action::EnterMode(Mode::Normal), buffer, runtime, false)
                    .await;
                self.edit_batch.depth = 0;
                cleanup?;
                self.set_legacy_message(Some("replay interrupted".into()));
                self.request_motion_render(MotionRender::Full);
            }
            self.publish_edit_batch(buffer, runtime).await?;
        }
        Ok(())
    }

    fn retain_replay_input(&mut self, event: Event) -> bool {
        if matches!(event, Event::Key(KeyEvent { code: KeyCode::Char('c'), modifiers, kind: KeyEventKind::Press | KeyEventKind::Repeat, .. }) if modifiers.contains(KeyModifiers::CONTROL))
        {
            self.edit_batch.cancelled = true;
            true
        } else {
            self.edit_batch.pending_input.push_back(event);
            false
        }
    }

    pub(super) fn replay_is_cancelled(&self) -> bool {
        self.edit_batch.cancelled
    }

    // Keep the large background-service future out of recursive action frames.
    #[inline(never)]
    fn service_replay_background<'a>(
        &'a mut self,
        buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move { self.service_background(buffer, runtime).await })
    }

    /// Bound synthetic work without dropping ordinary queued terminal input.
    #[inline(never)]
    pub(super) fn replay_checkpoint<'a>(
        &'a mut self,
        buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> BoxFuture<'a, anyhow::Result<bool>> {
        Box::pin(async move {
            if self.edit_batch.cancelled {
                return Ok(true);
            }
            if !self.edit_batch.is_active() {
                return Ok(false);
            }
            self.edit_batch.steps += 1;
            if self.edit_batch.steps < REPLAY_STEPS_PER_SLICE
                && self
                    .edit_batch
                    .started
                    .is_none_or(|start| start.elapsed() < self.edit_batch.budget)
            {
                return Ok(false);
            }
            self.edit_batch.steps = 0;
            self.edit_batch.started = Some(Instant::now());
            perf::increment("edit:replay_slices", 1);
            if self.edit_batch.terminal_input {
                for _ in 0..64 {
                    if !event::poll(Duration::ZERO)? {
                        break;
                    }
                    if self.retain_replay_input(event::read()?) {
                        break;
                    }
                }
            }
            if self.block_replay_depth == 0 {
                self.publish_edit_batch(buffer, runtime).await?;
                // Do not let agent/plugin mutations interleave with the middle
                // of an unfinished insertion transaction.
                if self.is_normal()
                    && !self.transaction_active()
                    && !self.is_waiting_for_key_sequence()
                    && !self.edit_batch.cancelled
                {
                    self.edit_batch.suspended += 1;
                    let result = self.service_replay_background(buffer, runtime).await;
                    self.edit_batch.suspended -= 1;
                    result?;
                }
            }
            tokio::task::yield_now().await;
            Ok(self.edit_batch.cancelled)
        })
    }

    // Keep parsing temporaries out of recursive action futures and their 2 MiB stack.
    #[inline(never)]
    fn action_is_local_substitute_command(&self, action: &Action) -> bool {
        let Action::Command(command) = action else {
            return false;
        };
        matches!(self.parse_substitute_command(command), Ok(Some(_)))
    }

    #[async_recursion::async_recursion]
    pub(super) async fn execute_with_tracking(
        &mut self,
        action: &Action,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
        tracking: bool,
    ) -> anyhow::Result<bool> {
        let local =
            Self::action_is_batch_local(action) || self.action_is_local_substitute_command(action);
        let barrier = self.edit_batch.is_active() && !local;
        if barrier {
            self.publish_edit_batch(buffer, runtime).await?;
            self.edit_batch.suspended += 1;
        }
        // InsertBlock deliberately preserves the primary committed frame until
        // its enclosing mode transition. Its hidden row replay has its own guard.
        let owner = local
            && !Self::action_is_navigation(action)
            && !matches!(action, Action::InsertBlock)
            && self.edit_batch.suspended == 0
            && self.block_replay_depth == 0;
        if owner {
            self.begin_edit_batch();
        }
        let result = self
            .execute_action_inner(action, buffer, runtime, tracking)
            .await;
        if barrier {
            self.edit_batch.suspended -= 1;
        }
        if owner {
            let published = self.finish_edit_batch(buffer, runtime).await;
            return result.and_then(|quit| published.map(|()| quit));
        }
        result
    }

    #[async_recursion::async_recursion]
    pub(super) async fn handle_key_action(
        &mut self,
        event: &Event,
        action: &KeyAction,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<bool> {
        let owner = matches!(action, KeyAction::Multiple(_) | KeyAction::Repeating(_, _))
            && !Self::key_action_is_navigation(action)
            && self.edit_batch.suspended == 0;
        if owner {
            self.begin_edit_batch();
        }
        let result = self
            .handle_key_action_inner(event, action, buffer, runtime)
            .await;
        if owner {
            let published = self.finish_edit_batch(buffer, runtime).await;
            return result.and_then(|quit| published.map(|()| quit));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Harness {
        editor: Editor,
        frame: RenderBuffer,
        runtime: Runtime,
    }

    impl Harness {
        fn new(source: &str) -> Self {
            let mut config =
                Config::from_toml_with_overrides(crate::assets::DEFAULT_CONFIG, &[]).unwrap();
            config.lsp.enabled = false;
            let lsp = Box::new(crate::LspManager::new(config.lsp.clone()));
            let mut editor = Editor::with_size(
                lsp,
                80,
                24,
                config,
                Theme::default(),
                vec![Buffer::new(Some("fixture.rs".into()), source.into())],
            )
            .unwrap();
            editor.test_disable_terminal_output();
            editor.edit_batch.budget = Duration::MAX;
            editor.test_set_clipboard(Box::new(crate::clipboard::DisabledClipboardProvider));
            let mut frame = RenderBuffer::new(80, 24, &Style::default());
            editor.render(&mut frame).unwrap();
            Self {
                editor,
                frame,
                runtime: Runtime::new(),
            }
        }

        async fn event(&mut self, code: KeyCode, modifiers: KeyModifiers) {
            self.editor
                .process_editor_event(
                    Event::Key(KeyEvent::new(code, modifiers)),
                    &mut self.frame,
                    &mut self.runtime,
                    EventRenderMode::Immediate,
                )
                .await
                .unwrap();
        }

        async fn keys(&mut self, keys: &str) {
            for ch in keys.chars() {
                self.event(
                    if ch == '\u{1b}' {
                        KeyCode::Esc
                    } else {
                        KeyCode::Char(ch)
                    },
                    KeyModifiers::NONE,
                )
                .await;
            }
        }

        fn counts(&self) -> (u64, usize) {
            (
                self.editor.render_generation,
                self.editor.edit_batch.published_changes,
            )
        }

        fn assert_one_publication(&mut self, before: (u64, usize)) {
            assert_eq!(self.counts(), (before.0 + 1, before.1 + 1));
            assert_eq!(self.editor.edit_batch.depth, 0);
            assert_eq!(self.editor.edit_batch.suspended, 0);
            assert!(self.editor.edit_batch.changed.is_empty());
            let mut full = self.frame.clone();
            self.editor.render(&mut full).unwrap();
            assert_eq!(self.frame.cells, full.cells);
        }
    }

    #[tokio::test]
    async fn dot_macro_and_counted_delete_publish_once() {
        let mut h = Harness::new("one\ntwo\nthree\n");
        h.keys(&format!("i{}\u{1b}j", "x".repeat(64))).await;
        let before = h.counts();
        h.keys(".").await;
        h.assert_one_publication(before);
        assert!(h
            .editor
            .current_buffer()
            .get(1)
            .unwrap()
            .contains(&"x".repeat(64)));

        h.editor
            .execute(
                &Action::SetMacroRegister {
                    register: 'a',
                    keys: format!("i{}<Esc>", "y".repeat(64)),
                },
                &mut h.frame,
                &mut h.runtime,
            )
            .await
            .unwrap();
        let before = h.counts();
        h.editor
            .execute(&Action::PlayMacro('a'), &mut h.frame, &mut h.runtime)
            .await
            .unwrap();
        h.assert_one_publication(before);

        h.keys("64").await;
        let before = h.counts();
        h.keys("x").await;
        h.assert_one_publication(before);
    }

    #[tokio::test]
    async fn block_replay_only_publishes_the_completed_primary_frame() {
        let source = "value\n".repeat(16);
        let mut h = Harness::new(&source);
        h.event(KeyCode::Char('v'), KeyModifiers::CONTROL).await;
        h.keys("15jI").await;
        h.keys(&"x".repeat(32)).await;
        let before = h.counts();
        h.keys("\u{1b}").await;
        h.assert_one_publication(before);
        assert_eq!(
            h.editor.current_buffer().contents(),
            format!("{}value\n", "x".repeat(32)).repeat(16)
        );
        h.keys("u").await;
        assert_eq!(h.editor.current_buffer().contents(), source);
    }

    #[tokio::test]
    async fn bounded_block_replay_batches_unicode_rows_and_preserves_undo() {
        let source = "fn value_0() {}\r\nfn value_😀() {}\r\nfn value_2() {}\r\n";
        let inserted = "prefix_42";
        let mut h = Harness::new(source);
        h.event(KeyCode::Char('v'), KeyModifiers::CONTROL).await;
        h.keys("2jI").await;
        h.keys(inserted).await;

        let before = h.counts();
        let replay_start = h.editor.actions.len();
        h.keys("\u{1b}").await;
        h.assert_one_publication(before);

        let expected = source
            .split_inclusive('\n')
            .map(|line| format!("{inserted}{line}"))
            .collect::<String>();
        assert_eq!(h.editor.current_buffer().contents(), expected);
        assert_eq!(
            h.editor.actions[replay_start..]
                .iter()
                .filter(|action| matches!(action, Action::InsertString(text) if text == inserted))
                .count(),
            2,
            "each secondary row should use one bounded insertion"
        );
        assert_eq!(
            (h.editor.cx, h.editor.buffer_line()),
            (inserted.len() - 1, 0)
        );

        h.keys("u").await;
        assert_eq!(h.editor.current_buffer().contents(), source);
        h.event(KeyCode::Char('r'), KeyModifiers::CONTROL).await;
        assert_eq!(h.editor.current_buffer().contents(), expected);
    }

    #[tokio::test]
    async fn substitute_publishes_one_unicode_crlf_frame_and_preserves_undo() {
        let source = "fn value_😀() {}\r\nfn value_2() {}\r\n";
        let expected = source.replace("value", "replacement");
        let mut h = Harness::new(source);
        h.keys(":%s/value/replacement/g").await;
        let before = h.counts();

        h.event(KeyCode::Enter, KeyModifiers::NONE).await;

        h.assert_one_publication(before);
        assert_eq!(h.editor.current_buffer().contents(), expected);

        h.keys("u").await;
        assert_eq!(h.editor.current_buffer().contents(), source);
        h.event(KeyCode::Char('r'), KeyModifiers::CONTROL).await;
        assert_eq!(h.editor.current_buffer().contents(), expected);
    }

    #[test]
    fn first_line_replay_bounds_match_ordinary_wrapped_viewports() {
        let long_source = format!("{}\nnext\n", "z".repeat(2_000));
        for (source, cursor, scrolloff) in [
            ("ordinary text\nnext line\n", 5, 3),
            ("😀 tabs\there and wrapped text\nnext\n", 8, 3),
            ("    indented wrapped source text\nnext\n", 20, 6),
            (long_source.as_str(), 1_500, 3),
        ] {
            let mut ordinary = Harness::new(source);
            let mut replay = Harness::new(source);
            ordinary.editor.config.scrolloff = Some(scrolloff);
            replay.editor.config.scrolloff = Some(scrolloff);
            ordinary.editor.cx = cursor;
            replay.editor.cx = cursor;
            replay.editor.begin_edit_batch();

            assert_eq!(replay.editor.check_bounds(), ordinary.editor.check_bounds());
            assert_eq!(
                (
                    replay.editor.cx,
                    replay.editor.cy,
                    replay.editor.vtop,
                    replay.editor.skipcol,
                ),
                (
                    ordinary.editor.cx,
                    ordinary.editor.cy,
                    ordinary.editor.vtop,
                    ordinary.editor.skipcol,
                ),
                "replay bounds diverged for cursor {cursor}, scrolloff {scrolloff}: {source:?}"
            );
        }
    }

    #[tokio::test]
    async fn an_external_action_observes_the_latest_revision() {
        let mut h = Harness::new("value\n");
        h.editor.begin_edit_batch();
        h.editor
            .execute(
                &Action::InsertString("x".into()),
                &mut h.frame,
                &mut h.runtime,
            )
            .await
            .unwrap();
        assert_eq!(h.editor.edit_batch.changed.len(), 1);
        let before = h.editor.edit_batch.published_changes;
        h.editor
            .execute(
                &Action::NotifyPlugins("test:barrier".into(), Value::Null),
                &mut h.frame,
                &mut h.runtime,
            )
            .await
            .unwrap();
        assert_eq!(h.editor.edit_batch.published_changes, before + 1);
        assert!(h.editor.edit_batch.changed.is_empty());
        assert!(h.editor.lsp_coordinator.is_revision_notified(
            h.editor.current_buffer().id(),
            h.editor.current_buffer().revision()
        ));
        h.editor
            .finish_edit_batch(&mut h.frame, &mut h.runtime)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn queued_changes_use_stable_buffer_identity() {
        let mut h = Harness::new("first\n");
        h.editor
            .buffer_manager
            .push_buffer(Buffer::new(Some("second.rs".into()), "second\n".into()));
        h.editor.begin_edit_batch();
        for index in [0, 1] {
            h.editor.buffer_manager.set_active_index(index);
            h.editor.begin_transaction("fixture");
            h.editor
                .replace_range(TextRange::insertion(TextPosition::new(0, 0)), "x");
            h.editor.commit_transaction(h.editor.cursor_snapshot());
            h.editor.notify_change(&mut h.runtime).await.unwrap();
        }
        assert_eq!(h.editor.edit_batch.changed.len(), 2);
        let before = h.editor.edit_batch.published_changes;
        h.editor
            .finish_edit_batch(&mut h.frame, &mut h.runtime)
            .await
            .unwrap();
        assert_eq!(h.editor.edit_batch.published_changes, before + 2);
        assert_eq!(h.editor.buffer_manager.active_index(), 1);
        for source in h.editor.buffer_manager.iter() {
            assert!(h
                .editor
                .lsp_coordinator
                .is_revision_notified(source.id(), source.revision()));
        }
    }

    #[tokio::test]
    async fn mode_transitions_and_explicit_plugin_barriers_remain_observable() {
        let mut h = Harness::new("value\n");
        while ACTION_DISPATCHER.try_recv_request().is_some() {}
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("observer.hk");
        std::fs::write(&path, r#"
            pub fn activate() {
                red::on("mode:changed", mode_changed);
                red::on("buffer:changed", buffer_changed);
                red::on("test:barrier", barrier);
            }
            fn mode_changed(event: Json) { red::execute("Print", "mode:" + red::string(event.to, "")); }
            fn buffer_changed(event: Json) { red::execute("Print", "buffer"); }
            fn barrier(event: Json) { red::execute("Print", "barrier"); }
        "#).unwrap();
        h.editor
            .plugin_registry
            .add("observer", path.to_str().unwrap());
        h.editor
            .plugin_registry
            .initialize(&mut h.runtime)
            .await
            .unwrap();
        while ACTION_DISPATCHER.try_recv_request().is_some() {}
        h.editor
            .execute(
                &Action::SetMacroRegister {
                    register: 'a',
                    keys: "iabc<Esc>".into(),
                },
                &mut h.frame,
                &mut h.runtime,
            )
            .await
            .unwrap();
        h.editor
            .execute(&Action::PlayMacro('a'), &mut h.frame, &mut h.runtime)
            .await
            .unwrap();
        let mut messages = Vec::new();
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::Action(Action::Print(message)) = request {
                messages.push(message);
            }
        }
        assert_eq!(messages, ["mode:Insert", "buffer", "mode:Normal"]);
        h.editor.begin_edit_batch();
        h.editor
            .execute(
                &Action::InsertString("x".into()),
                &mut h.frame,
                &mut h.runtime,
            )
            .await
            .unwrap();
        h.editor
            .execute(
                &Action::NotifyPlugins("test:barrier".into(), Value::Null),
                &mut h.frame,
                &mut h.runtime,
            )
            .await
            .unwrap();
        h.editor
            .finish_edit_batch(&mut h.frame, &mut h.runtime)
            .await
            .unwrap();
        let mut messages = Vec::new();
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::Action(Action::Print(message)) = request {
                messages.push(message);
            }
        }
        assert_eq!(messages, ["buffer", "barrier"]);
    }

    #[tokio::test]
    async fn interruption_keeps_ordinary_input_and_commits_partial_insert() {
        let mut h = Harness::new("value\n");
        h.editor.begin_edit_batch();
        h.editor
            .execute(
                &Action::EnterMode(Mode::Insert),
                &mut h.frame,
                &mut h.runtime,
            )
            .await
            .unwrap();
        h.editor
            .execute(
                &Action::InsertString("partial".into()),
                &mut h.frame,
                &mut h.runtime,
            )
            .await
            .unwrap();
        h.editor.pending_macro_action = Some(PendingMacroAction::Play);
        h.editor.pending_replace = true;
        h.editor.repeater = Some(12);
        let ordinary = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert!(!h.editor.retain_replay_input(ordinary.clone()));
        assert!(h.editor.retain_replay_input(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ))));
        assert!(h
            .editor
            .replay_checkpoint(&mut h.frame, &mut h.runtime)
            .await
            .unwrap());
        h.editor
            .finish_edit_batch(&mut h.frame, &mut h.runtime)
            .await
            .unwrap();
        assert!(h.editor.is_normal());
        assert!(!h.editor.is_waiting_for_key_sequence());
        assert!(!h.editor.transaction_active());
        assert_eq!(
            h.editor.edit_batch.pending_input.pop_front(),
            Some(ordinary)
        );
        assert!(!h.editor.replay_is_cancelled());
        h.keys("u").await;
        assert_eq!(h.editor.current_buffer().contents(), "value\n");
    }

    #[tokio::test]
    async fn adjacent_insert_undo_records_are_compacted() {
        let mut h = Harness::new("value\n");
        h.keys("iλé").await;
        h.event(KeyCode::Enter, KeyModifiers::NONE).await;
        h.keys("hello\u{1b}").await;
        let inserted = h.editor.current_buffer().contents();
        let edits = &h
            .editor
            .current_buffer()
            .undo_history
            .latest_transaction()
            .unwrap()
            .edits;
        assert!(
            edits.len() < 8,
            "adjacent insertion should not store every typed character"
        );
        h.keys("u").await;
        assert_eq!(h.editor.current_buffer().contents(), "value\n");
        h.editor
            .execute(&Action::Redo, &mut h.frame, &mut h.runtime)
            .await
            .unwrap();
        assert_eq!(h.editor.current_buffer().contents(), inserted);
    }

    #[tokio::test]
    async fn failed_replay_restores_publication_state() {
        let mut h = Harness::new("value\n");
        h.editor
            .execute(
                &Action::SetMacroRegister {
                    register: 'a',
                    keys: "ix<Esc>:q!<CR>".into(),
                },
                &mut h.frame,
                &mut h.runtime,
            )
            .await
            .unwrap();
        assert!(h
            .editor
            .execute(&Action::PlayMacro('a'), &mut h.frame, &mut h.runtime)
            .await
            .is_err());
        assert_eq!(h.editor.edit_batch.depth, 0);
        assert_eq!(h.editor.edit_batch.suspended, 0);
        assert_eq!(h.editor.macro_replay_depth, 0);
        assert!(h.editor.edit_batch.changed.is_empty());
        h.keys("iY\u{1b}").await;
        assert!(h.editor.current_buffer().contents().starts_with("Yx"));
    }
}
