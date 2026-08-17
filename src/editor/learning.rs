//! Lifecycle for Learn Red's protected practice-buffer lessons.

use super::*;
use crate::learn::{practice_action_allowed, Lesson, PracticeStep};
use crate::ui::{draw_learn_coach, CoachLayout, LearnHub};

pub(super) struct LearnSession {
    lesson: Lesson,
    pub(super) step: PracticeStep,
    practice_buffer_id: BufferId,
    original_buffer_id: BufferId,
    original_windows: WindowManager,
    original_zoom: Option<FocusTarget>,
    original_panel_focus: Option<String>,
    original_repeat: Option<SemanticChange>,
    original_registers: HashMap<char, Content>,
}

impl Editor {
    pub(super) fn open_learn_hub(&mut self, runtime: &mut Runtime) {
        if self.inline_assist.is_some()
            || self.workspace_manager.is_active()
            || self
                .current_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.composer_handle().is_some())
        {
            self.set_legacy_message(Some(
                "finish the current proposal, composer, or workspace before opening Learn Red"
                    .into(),
            ));
            return;
        }
        self.release_current_dialog_callbacks(runtime);
        self.current_dialog = Some(Box::new(LearnHub::new(
            self,
            Lesson::AVAILABLE.map(|lesson| self.preferences.learn_lesson_completed(lesson.id())),
        )));
    }

    pub(super) fn next_learn_lesson(&self) -> Lesson {
        Lesson::AVAILABLE
            .into_iter()
            .find(|lesson| !self.preferences.learn_lesson_completed(lesson.id()))
            .unwrap_or_default()
    }

    pub(super) async fn continue_learn_lesson(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        if let Some(session) = &self.learn_session {
            if session.step != PracticeStep::Complete {
                self.set_legacy_message(Some(
                    "finish this checkpoint first, or use :tutorial quit to leave".into(),
                ));
                return self.render(buffer);
            }
            if let Some(next) = session.lesson.next() {
                return self.start_learn_lesson(next, buffer, runtime).await;
            }
        }
        self.finish_learn_lesson(buffer, runtime)?;
        self.open_learn_hub(runtime);
        self.render(buffer)
    }

    pub(super) async fn restart_learn_lesson(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let lesson = self
            .learn_session
            .as_ref()
            .map_or_else(|| self.next_learn_lesson(), |session| session.lesson);
        self.start_learn_lesson(lesson, buffer, runtime).await
    }

    // Lesson startup owns a saved window layout across an await. Keep that
    // future out of the editor dispatcher's nested motion/replay stack frames.
    #[inline(never)]
    pub(super) fn start_learn_lesson<'a>(
        &'a mut self,
        lesson: Lesson,
        buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(self.start_learn_lesson_impl(lesson, buffer, runtime))
    }

    async fn start_learn_lesson_impl(
        &mut self,
        lesson: Lesson,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        if self.learn_session.is_some() {
            self.finish_learn_lesson(buffer, runtime)?;
        }
        if self.size.0 < 32 || self.size.1 < 12 {
            self.set_legacy_message(Some(
                "make the terminal at least 32 columns by 12 rows to start a lesson".into(),
            ));
            return self.render(buffer);
        }
        if self.inline_assist.is_some()
            || self.workspace_manager.is_active()
            || self.agent_manager.has_active_sessions()
        {
            self.set_legacy_message(Some(
                "finish the current agent turn, inline assist, or workspace before starting a lesson".into(),
            ));
            return self.render(buffer);
        }
        self.release_current_dialog_callbacks(runtime);
        self.current_dialog = None;
        self.sync_to_window();
        // The temporary lesson must never replace the user's recovery layout.
        self.persist_session_snapshot(true);
        let original_buffer_id = self.current_buffer().id();
        let original_panel_focus = self.panel_manager.focused_panel_id().map(str::to_string);
        self.panel_manager.focus_editor();
        let original_zoom = self.zoomed_pane.take();
        let original_repeat = self.last_semantic_change.take();
        let original_registers = self.registers.clone();
        self.pending_semantic_change = None;
        let practice = Buffer::new(None, lesson.contents().to_string());
        let practice_buffer_id = practice.id();
        let practice_index = self.buffer_manager.len();
        self.buffer_manager.push_buffer(practice);
        let original_windows = std::mem::replace(
            &mut self.window_manager,
            WindowManager::new(
                practice_index,
                (usize::from(self.size.0), usize::from(self.size.1)),
            ),
        );
        self.learn_session = Some(LearnSession {
            lesson,
            step: lesson.first_step(),
            practice_buffer_id,
            original_buffer_id,
            original_windows,
            original_zoom,
            original_panel_focus,
            original_repeat,
            original_registers,
        });
        self.mode = Mode::Normal;
        self.splash_dismissed = true;
        self.force_full_redraw = true;
        self.set_current_buffer(buffer, practice_index).await
    }

    pub(super) fn finish_learn_lesson(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let Some(session) = self.learn_session.take() else {
            return Ok(());
        };
        self.release_current_dialog_callbacks(runtime);
        self.current_dialog = None;
        self.completion_snapshot = None;
        if let Some(index) = self
            .buffer_manager
            .iter()
            .position(|candidate| candidate.id() == session.practice_buffer_id)
        {
            self.buffer_manager.remove_buffer(index);
        }
        self.lsp_coordinator
            .forget_buffer(session.practice_buffer_id);
        self.window_manager = session.original_windows;
        if let Some(index) = self
            .buffer_manager
            .iter()
            .position(|candidate| candidate.id() == session.original_buffer_id)
        {
            self.buffer_manager.set_active_index(index);
        }
        self.zoomed_pane = session.original_zoom;
        self.last_semantic_change = session.original_repeat;
        self.pending_semantic_change = None;
        self.registers = session.original_registers;
        self.local_marks.remove(&session.practice_buffer_id);
        self.special_marks
            .retain(|(id, _), _| *id != session.practice_buffer_id);
        self.last_visual_selections
            .remove(&session.practice_buffer_id);
        self.sync_with_window();
        self.mode = Mode::Normal;
        self.waiting_key_action = None;
        self.command.clear();
        if let Some(panel) = session.original_panel_focus {
            self.panel_manager.restore_panel_focus(&panel);
        }
        self.highlight_cache.clear();
        self.layout_cache.borrow_mut().clear();
        self.force_full_redraw = true;
        self.render(buffer)?;
        self.persist_session_snapshot(true);
        Ok(())
    }

    /// Returns true when a practice action was handled or safely refused.
    pub(super) fn intercept_learn_action(
        &mut self,
        action: &Action,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<bool> {
        let Some(session) = self.learn_session.as_ref() else {
            return Ok(false);
        };
        if matches!(action, Action::Quit(_)) {
            self.finish_learn_lesson(buffer, runtime)?;
            return Ok(true);
        }
        if self.current_buffer().id() != session.practice_buffer_id {
            self.finish_learn_lesson(buffer, runtime)?;
            self.set_legacy_message(Some(
                "lesson paused because the active buffer changed".into(),
            ));
            self.render(buffer)?;
            return Ok(true);
        }
        if !practice_action_allowed(action) {
            self.set_legacy_message(Some(
                "this practice step only edits tutorial text; use :tutorial quit to return".into(),
            ));
            self.render(buffer)?;
            return Ok(true);
        }
        Ok(false)
    }

    pub(super) fn observe_learn_action(
        &mut self,
        action: &Action,
        buffer: &mut RenderBuffer,
    ) -> anyhow::Result<()> {
        if matches!(
            action,
            Action::StartLearnLesson
                | Action::StartLearnLessonAt(_)
                | Action::RestartLearnLesson
                | Action::FinishLearnLesson
        ) {
            return Ok(());
        }
        let Some(session) = self.learn_session.as_ref() else {
            return Ok(());
        };
        if self.current_buffer().id() != session.practice_buffer_id {
            return Ok(());
        }
        let contents = self.current_buffer().contents();
        let position = self.cursor_text_position();
        let cursor = (position.character, position.line);
        let mode = self.mode;
        let session = self
            .learn_session
            .as_mut()
            .expect("the active lesson was checked above");
        if session
            .step
            .observe(session.lesson, action, mode, &contents, cursor)
        {
            if session.step == PracticeStep::Complete {
                if let Err(error) = self.preferences.complete_learn_lesson(session.lesson.id()) {
                    log!("could not persist Learn Red progress: {error}");
                }
            }
            self.render(buffer)?;
        }
        Ok(())
    }

    pub(super) fn resize_learn_layout(&mut self, size: (usize, usize)) -> bool {
        if self.learn_session.is_none() {
            return false;
        }
        let layout = CoachLayout::new(size.0, size.1);
        self.sync_to_window();
        self.window_manager
            .set_presentation(WindowPresentation::All);
        self.panel_manager
            .set_presentation(plugin::panel::PanelPresentation::Hidden);
        self.window_manager.resize_with_origin(
            Point::new(0, layout.top),
            (
                size.0.saturating_sub(layout.right),
                size.1.saturating_sub(layout.top + layout.bottom),
            ),
        );
        self.sync_with_window();
        true
    }

    pub(super) fn render_learn_coach(&self, buffer: &mut RenderBuffer) {
        let Some(session) = &self.learn_session else {
            return;
        };
        let shortcut = session.step.suggested_action().and_then(|action| {
            command_palette::shortcuts_for_action(&self.config.keys, &action)
                .into_iter()
                .next()
        });
        draw_learn_coach(
            buffer,
            &self.theme,
            session.lesson,
            session.step,
            shortcut.as_deref(),
        );
    }
}
