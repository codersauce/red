//! Lifecycle for Learn Red's protected practice-buffer lessons.

use super::*;
use crate::learn::{
    practice_action_allowed, Lesson, PracticeStep, PracticeView, PracticeWorkspace,
};
use crate::ui::{draw_learn_coach, draw_learn_panel_coach, CoachLayout, LearnHub};

mod agent;
mod git;

pub(super) struct LearnSession {
    lesson: Lesson,
    pub(super) step: PracticeStep,
    practice_buffer_id: BufferId,
    workspace: Option<PracticeWorkspace>,
    original_buffer_id: BufferId,
    original_windows: WindowManager,
    original_zoom: Option<FocusTarget>,
    original_panel_focus: Option<String>,
    original_repeat: Option<SemanticChange>,
    original_registers: HashMap<char, Content>,
    original_inline_history: Option<InlineHistory>,
    original_active_inline_comment: Option<uuid::Uuid>,
    original_panels: Option<plugin::panel::PanelManager>,
    agent: Option<agent::LearnAgentState>,
    git: Option<git::LearnGitState>,
    original_workspaces: Option<plugin::WorkspaceManager>,
}

impl LearnSession {
    fn owns_buffer(&self, buffer: &Buffer) -> bool {
        buffer.id() == self.practice_buffer_id
            || self.workspace.as_ref().is_some_and(|workspace| {
                buffer
                    .file
                    .as_deref()
                    .is_some_and(|file| workspace.permits_file(Path::new(file)))
            })
    }
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
        self.next_learn_lesson_for_track(0).unwrap_or_default()
    }

    pub(super) fn next_learn_lesson_for_track(&self, track: usize) -> Option<Lesson> {
        Lesson::for_track(track)
            .find(|lesson| !self.preferences.learn_lesson_completed(lesson.id()))
            .or_else(|| Lesson::for_track(track).next())
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
        // Create owned storage before changing any editor state, so a failed
        // setup leaves the user's current workspace untouched.
        let workspace = if matches!(
            lesson,
            Lesson::SaveAPracticeFile | Lesson::ContinueInAgent | Lesson::ReviewWhatChanged
        ) {
            Some(PracticeWorkspace::new()?)
        } else {
            None
        };
        if lesson == Lesson::ContinueInAgent {
            let workspace = workspace.as_ref().expect("file lesson owns a workspace");
            workspace.write_fixture("score.rs", lesson.contents())?;
            workspace.write_fixture("example.rs", crate::learn::AGENT_EXAMPLE)?;
        }
        let git = if lesson == Lesson::ReviewWhatChanged {
            let workspace = workspace.as_ref().expect("Git lesson owns a workspace");
            match git::LearnGitState::prepare(workspace, runtime).await {
                Ok(state) => Some(state),
                Err(error) => {
                    self.set_notification_message(
                        Severity::Error,
                        Some(format!("could not start Git practice: {error:#}")),
                    );
                    return self.render(buffer);
                }
            }
        } else {
            None
        };
        if self.inline_assist.is_some()
            || self.workspace_manager.is_active()
            || self.agent_manager.has_active_sessions()
            || self.inline_history_browser.is_some()
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
        let original_panels =
            (lesson == Lesson::ContinueInAgent).then(|| std::mem::take(&mut self.panel_manager));
        let original_workspaces = git
            .as_ref()
            .map(|_| std::mem::take(&mut self.workspace_manager));
        self.panel_manager.focus_editor();
        let original_zoom = self.zoomed_pane.take();
        let original_repeat = self.last_semantic_change.take();
        let original_registers = self.registers.clone();
        let original_inline_history = lesson
            .is_ai_practice()
            .then(|| std::mem::take(&mut self.inline_history));
        let original_active_inline_comment = self.active_inline_comment;
        self.pending_semantic_change = None;
        let file = workspace.as_ref().map(|workspace| {
            workspace
                .path(
                    if matches!(lesson, Lesson::ContinueInAgent | Lesson::ReviewWhatChanged) {
                        "score.rs"
                    } else {
                        "practice.txt"
                    },
                )
                .to_string_lossy()
                .into_owned()
        });
        let practice = Buffer::new(file, lesson.contents().to_string());
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
            workspace,
            original_buffer_id,
            original_windows,
            original_zoom,
            original_panel_focus,
            original_repeat,
            original_registers,
            original_inline_history,
            original_active_inline_comment,
            original_panels,
            agent: (lesson == Lesson::ContinueInAgent).then(agent::LearnAgentState::default),
            git,
            original_workspaces,
        });
        self.mode = Mode::Normal;
        if lesson == Lesson::FindACommand {
            self.wrap = true;
        }
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
        if session.lesson.is_ai_practice() {
            // Recorded sessions never belong to a live agent bridge.
            if let Some(assist) = self.inline_assist.as_mut() {
                assist.session_id = None;
            }
            self.close_inline_assist_session();
        }
        self.release_current_dialog_callbacks(runtime);
        self.current_dialog = None;
        self.completion_snapshot = None;
        let owned = self
            .buffer_manager
            .iter()
            .enumerate()
            .filter(|(_, candidate)| session.owns_buffer(candidate))
            .map(|(index, candidate)| (index, candidate.id()))
            .collect::<Vec<_>>();
        for &(index, id) in owned.iter().rev() {
            self.buffer_manager.remove_buffer(index);
            self.lsp_coordinator.forget_buffer(id);
            self.local_marks.remove(&id);
            self.special_marks
                .retain(|(buffer_id, _), _| *buffer_id != id);
            self.last_visual_selections.remove(&id);
            self.forget_jumps_for_buffer(id);
        }
        if let Some(workspaces) = session.original_workspaces {
            self.workspace_manager = workspaces;
        }
        let restored_panels = session.original_panels.is_some();
        if let Some(panels) = session.original_panels {
            self.panel_manager = panels;
        }
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
        if let Some(history) = session.original_inline_history {
            self.inline_history = history;
        }
        self.inline_comments
            .retain(|comment| !owned.iter().any(|(_, id)| *id == comment.anchor.buffer_id));
        self.active_inline_comment = session.original_active_inline_comment;
        self.sync_with_window();
        self.mode = Mode::Normal;
        self.selection = None;
        self.selection_start = None;
        self.pending_visual_text_object_scope = None;
        self.pending_operator = None;
        self.pending_character_motion = None;
        self.waiting_key_action = None;
        self.command.clear();
        if !restored_panels {
            if let Some(panel) = session.original_panel_focus {
                self.panel_manager.restore_panel_focus(&panel);
            }
        }
        self.highlight_cache.clear();
        self.layout_cache.borrow_mut().clear();
        // Finish deleting owned files before showing the restored workspace.
        drop(session.workspace);
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
        if !session.owns_buffer(self.current_buffer()) {
            self.finish_learn_lesson(buffer, runtime)?;
            self.set_legacy_message(Some(
                "lesson paused because the active buffer changed".into(),
            ));
            self.render(buffer)?;
            return Ok(true);
        }
        if !practice_action_allowed(session.lesson, action) {
            self.set_legacy_message(Some(
                "this practice step only edits tutorial text; use :tutorial quit to return".into(),
            ));
            self.render(buffer)?;
            return Ok(true);
        }
        if matches!(action, Action::Save) && session.workspace.is_some() {
            let permitted = self.current_buffer().file.as_deref().is_some_and(|file| {
                session
                    .workspace
                    .as_ref()
                    .is_some_and(|workspace| workspace.permits_file(Path::new(file)))
            });
            if !permitted {
                self.set_legacy_message(Some(
                    "practice save refused: file is outside the lesson workspace".into(),
                ));
            } else {
                // Use the editor's real buffer save and transaction semantics,
                // without running project formatters or user plugin hooks.
                let resume_insert = self.commit_active_transaction_before_save();
                let result = self.current_buffer_mut().save();
                self.resume_insert_transaction_after_save(resume_insert);
                match result {
                    Ok(message) => self.set_notification_message(Severity::Success, Some(message)),
                    Err(error) => {
                        self.set_notification_message(Severity::Error, Some(error.to_string()))
                    }
                }
                self.observe_learn_action(action, buffer)?;
            }
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
        if !session.owns_buffer(self.current_buffer()) {
            return Ok(());
        }
        let contents = self.current_buffer().contents();
        let position = self.cursor_text_position();
        let cursor = (position.character, position.line);
        let mode = self.mode;
        let view = PracticeView {
            command_palette_open: self
                .current_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.shortcut_context() == "Commands"),
            wrapping: self.wrap,
            shortcuts_open: self.keyboard_shortcuts.is_some(),
            file_matches_buffer: session.workspace.as_ref().is_some_and(|workspace| {
                self.current_buffer().file.as_deref().is_some_and(|file| {
                    workspace.permits_file(Path::new(file))
                        && !self.current_buffer().is_dirty()
                        && std::fs::read_to_string(file).is_ok_and(|saved| saved == contents)
                })
            }),
            dirty: self.current_buffer().is_dirty(),
            inline_target_selected: self.inline_assist.as_ref().is_some_and(|assist| {
                assist.expected_text == crate::learn::AI_LINE && assist.scope.contains("selection")
            }),
            inline_explanation_received: self.inline_assist.as_ref().is_some_and(|assist| {
                assist.has_result
                    && assist.transaction_id.is_none()
                    && self.inline_comment_group_count(&assist.annotation_group_id) > 0
            }) && contents == crate::learn::AI_CONTENTS,
            inline_comment_open: self.inline_assist.is_none()
                && self.current_dialog.is_some()
                && self
                    .inline_comments
                    .iter()
                    .any(|comment| comment.anchor.buffer_id == session.practice_buffer_id)
                && contents == crate::learn::AI_CONTENTS,
            inline_edit_applied: self
                .inline_assist
                .as_ref()
                .is_some_and(|assist| assist.has_result && assist.transaction_id.is_some()),
            inline_closed: self.inline_assist.is_none(),
            fixed_text: contents == crate::learn::AI_FIXED_CONTENTS,
            bonus_text: contents == crate::learn::AI_BONUS_CONTENTS,
            original_text: contents == session.lesson.contents(),
            agent_pane_open: self.learn_agent_pane_open(),
            agent_files_saved: self.learn_agent_files_saved(),
            agent_example_visible: contents == crate::learn::AGENT_EXAMPLE_FIXED,
        };
        let session = self
            .learn_session
            .as_mut()
            .expect("the active lesson was checked above");
        if session
            .step
            .observe(session.lesson, action, mode, &contents, cursor)
            || session.step.observe_view(action, view)
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

    pub(super) fn is_learn_inline_practice(&self) -> bool {
        self.learn_session
            .as_ref()
            .is_some_and(|session| session.lesson.is_ai_practice())
    }

    /// Exercise the production result path without starting a model or sending
    /// the user's prompt anywhere. History is isolated for the whole lesson.
    #[inline(never)]
    pub(super) fn submit_learn_inline<'a>(
        &'a mut self,
        prompt: &'a str,
        buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let Some(assist) = self.inline_assist.as_ref() else {
                return Ok(());
            };
            let range = assist.range;
            let scope = assist.scope.clone();
            let lesson = self.learn_session.as_ref().map(|session| session.lesson);
            let refining_choice = lesson == Some(Lesson::ChooseWhatToKeep)
                && assist.expected_text == crate::learn::AI_BONUS_LINE;
            if (assist.expected_text != crate::learn::AI_LINE && !refining_choice)
                || !scope.contains("selection")
                || prompt.trim().is_empty()
            {
                self.current_dialog = Some(Box::new(self.inline_assist_popup(scope,
                    InlineAssistPopupState::Failed("Recorded practice needs the first function line selected and a nonempty question. Esc, then select it with V and try again.".into()))));
                return self.render(buffer);
            }
            let request = format!("learn-practice:{}", uuid::Uuid::new_v4());
            self.begin_inline_history_turn(&request, prompt, range)?;
            if let Some(assist) = self.inline_assist.as_mut() {
                assist.request_id = Some(request.clone());
            }
            let (replacement, message) = match lesson {
                Some(Lesson::MakeAFocusedChange | Lesson::ContinueInAgent) => (Some(crate::learn::AI_FIXED_LINE),
                    "Recorded practice response: changed subtraction to addition. This edit is in the buffer and has not been saved."),
                Some(Lesson::ChooseWhatToKeep) if refining_choice => (Some(crate::learn::AI_FIXED_LINE),
                    "Recorded practice response: removed the extra bonus point. The refined edit is still unsaved."),
                Some(Lesson::ChooseWhatToKeep) => (Some(crate::learn::AI_BONUS_LINE),
                    "Recorded practice response: this intentionally imperfect suggestion adds a bonus point. Inspect the extra + 1 before deciding what to keep."),
                _ => (None,
                    "Recorded practice response: this function subtracts points from score. Its name suggests adding points instead. This explanation did not change the source."),
            };
            let result = InlineAssistResult {
                replacement: replacement.map(str::to_string),
                comments: vec![crate::inline_assist::InlineCommentInput {
                    start_line: 1,
                    end_line: None,
                    message: message.into(),
                }],
            };
            self.apply_inline_result(&request, "learn-practice:offline", &result, buffer, runtime)
                .await?;
            self.observe_learn_action(&Action::SubmitInlineAssist(prompt.into()), buffer)
        })
    }

    pub(super) fn resize_learn_layout(&mut self, size: (usize, usize)) -> bool {
        if self.learn_session.is_none() {
            return false;
        }
        if self.learn_git_workspace_open() {
            self.sync_to_window();
            self.window_manager
                .set_presentation(WindowPresentation::Hidden);
            self.panel_manager
                .set_presentation(plugin::panel::PanelPresentation::Hidden);
            return true;
        }
        if self.learn_agent_pane_open() {
            let layout = CoachLayout::for_panel(size.1);
            self.sync_to_window();
            self.window_manager
                .set_presentation(WindowPresentation::Hidden);
            self.panel_manager
                .set_presentation(plugin::panel::PanelPresentation::Docked);
            self.panel_manager.update_panel_layout(
                crate::learn::LEARN_AGENT_PANEL,
                plugin::PanelSide::Top,
                size.1.saturating_sub(layout.bottom + 2),
            );
            return true;
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
        let draw = if self.learn_agent_pane_open() || self.learn_git_workspace_open() {
            draw_learn_panel_coach
        } else {
            draw_learn_coach
        };
        draw(
            buffer,
            &self.theme,
            session.lesson,
            session.step,
            shortcut.as_deref(),
        );
    }
}
