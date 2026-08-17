//! Resumable lesson bookmarks and non-destructive learning controls.

use super::*;
use crate::learn::TRACKS;

impl Editor {
    pub(crate) fn learn_last_track_index(&self) -> usize {
        self.preferences
            .learn_last_track()
            .and_then(|id| TRACKS.iter().position(|track| track.id == id))
            .unwrap_or(0)
    }

    pub(crate) fn learn_resume_lesson_for_track(&self, track: usize) -> Option<Lesson> {
        TRACKS
            .get(track)
            .and_then(|track| self.preferences.learn_resume_lesson(track.id))
            .and_then(Lesson::from_id)
            .filter(|lesson| lesson.track_index() == track && !lesson.is_optional())
    }

    pub(super) fn remember_learn_position(&mut self, lesson: Lesson) {
        let track = TRACKS[lesson.track_index()].id;
        // Entering live practice is always explicit. It must not replace the
        // recorded track's default resume target.
        let id = if lesson.is_optional() {
            self.preferences
                .learn_resume_lesson(track)
                .map(str::to_owned)
        } else {
            Some(lesson.id().to_owned())
        };
        if let Err(error) = self.preferences.remember_learn_lesson(track, id.as_deref()) {
            log!("could not persist Learn Red bookmark: {error}");
        }
    }

    pub(super) fn advance_learn_bookmark(&mut self, lesson: Lesson) {
        if lesson.is_optional() {
            return;
        }
        let next = lesson.next().map(Lesson::id);
        if let Err(error) = self
            .preferences
            .remember_learn_lesson(TRACKS[lesson.track_index()].id, next)
        {
            log!("could not persist Learn Red bookmark: {error}");
        }
    }

    pub(super) async fn skip_learn_lesson(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let Some(lesson) = self.learn_session.as_ref().map(|session| session.lesson) else {
            self.open_learn_hub(runtime);
            return self.render(buffer);
        };
        // Skipping deliberately does not award a completion badge.
        self.advance_learn_bookmark(lesson);
        if let Some(next) = lesson.next() {
            self.start_learn_lesson(next, buffer, runtime).await
        } else {
            self.finish_learn_lesson(buffer, runtime).await?;
            self.open_learn_hub(runtime);
            self.render(buffer)
        }
    }

    pub(super) fn show_learn_help(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let current = self
            .learn_session
            .as_ref()
            .map(|session| {
                let shortcut = session.step.suggested_action().and_then(|action| {
                    command_palette::shortcuts_for_action(&self.config.keys, &action)
                        .into_iter()
                        .next()
                });
                format!(
                    "{}\n{}\n\n",
                    session.lesson.title(),
                    session
                        .step
                        .instruction(session.lesson, shortcut.as_deref())
                )
            })
            .unwrap_or_default();
        self.release_current_dialog_callbacks(runtime);
        self.current_dialog = Some(Box::new(HoverInfo::new(
            self,
            format!("{current}LEARNING CONTROLS\n:tutorial next — continue after completing a lesson\n:tutorial skip — move on without marking it complete\n:tutorial restart — recreate this lesson's practice files\n:tutorial quit — restore your original workspace\n:tutorial resume — reopen your last lesson with fresh fixtures\n:tutorial — choose any track\n\nUse :tutorial <track> <number> to jump directly. Tracks: essentials, ai, ship, navigation, editing, custom.\n\nThe five recorded AI lessons work offline. :tutorial ai live opens the optional live exercise; submitting there sends a real request.\n\nCompleted lessons and per-track bookmarks survive restart. Practice edits, temporary files, and live requests do not. Your effective shortcut is shown beside each task."),
            crate::ui::HoverInfoFormat::Plaintext,
            Vec::new(),
        ).with_label("Learn Red help")));
        self.render(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn learn_skip_and_resume_preserve_real_completion() {
        let config = Config::default();
        let client = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            client,
            100,
            30,
            config,
            Theme::default(),
            vec![Buffer::new(None, "original".into())],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        let mut buffer = RenderBuffer::new(100, 30, &Style::default());
        let mut runtime = Runtime::new();
        editor
            .start_learn_lesson(Lesson::MoveWithIntent, &mut buffer, &mut runtime)
            .await
            .unwrap();
        editor
            .execute(&Action::SkipLearnLesson, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(
            editor.learn_session.as_ref().unwrap().lesson,
            Lesson::ChangeATextObject
        );
        assert!(!editor
            .preferences
            .learn_lesson_completed(Lesson::MoveWithIntent.id()));
        editor
            .execute(&Action::ExitLearnLesson, &mut buffer, &mut runtime)
            .await
            .unwrap();
        editor
            .execute(&Action::ResumeLearnLesson, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(
            editor.learn_session.as_ref().unwrap().lesson,
            Lesson::ChangeATextObject
        );
        assert_eq!(
            editor.learn_session.as_ref().unwrap().step,
            PracticeStep::ObjectFind
        );
        editor
            .execute(&Action::ShowLearnHelp, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(
            editor.current_dialog.as_ref().unwrap().shortcut_context(),
            "Learn Red help"
        );
        editor
            .execute(&Action::ExitLearnLesson, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(editor.current_buffer().contents(), "original");
    }
}
