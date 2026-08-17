//! Temporary search state and prompt histories for protected lessons.

use super::*;

pub(super) struct LearnInputState {
    term: String,
    direction: SearchDirection,
    matches: Option<SearchMatchCache>,
    highlights_suppressed: bool,
    commands: Vec<String>,
    searches: Vec<String>,
}

impl LearnInputState {
    pub fn install(editor: &mut Editor) -> Self {
        Self {
            term: std::mem::take(&mut editor.search_term),
            direction: std::mem::replace(&mut editor.search_direction, SearchDirection::Forward),
            matches: editor.search_match_cache.take(),
            highlights_suppressed: std::mem::replace(
                &mut editor.search_highlights_suppressed,
                false,
            ),
            commands: Vec::new(),
            searches: Vec::new(),
        }
    }

    pub fn restore(self, editor: &mut Editor) {
        editor.active_search = None;
        editor.substitute_confirmation = None;
        editor.command_history_navigation = None;
        editor.command_completion = None;
        editor.search_term = self.term;
        editor.search_direction = self.direction;
        editor.search_match_cache = self.matches;
        editor.search_highlights_suppressed = self.highlights_suppressed;
    }
}

fn record(history: &mut Vec<String>, text: &str) {
    if text.is_empty() || history.last().is_some_and(|last| last == text) {
        return;
    }
    history.push(text.to_string());
    if history.len() > 100 {
        history.remove(0);
    }
}

impl Editor {
    pub(in crate::editor) fn record_learn_command(&mut self, command: &str) -> bool {
        let Some(session) = self.learn_session.as_mut() else {
            return false;
        };
        record(&mut session.input.commands, command.trim());
        true
    }

    pub(in crate::editor) fn record_learn_search(&mut self, pattern: &str) -> bool {
        let Some(session) = self.learn_session.as_mut() else {
            return false;
        };
        record(&mut session.input.searches, pattern);
        true
    }

    pub(in crate::editor) fn navigate_learn_command_history(
        &mut self,
        direction: PromptHistoryDirection,
    ) -> bool {
        let Some(session) = self.learn_session.as_ref() else {
            return false;
        };
        PromptHistoryNavigation::navigate(
            &session.input.commands,
            &mut self.command_history_navigation,
            &mut self.command,
            direction,
        );
        true
    }

    pub(in crate::editor) fn navigate_learn_search_history(
        &mut self,
        direction: PromptHistoryDirection,
    ) -> bool {
        let Some(lesson) = self.learn_session.as_ref() else {
            return false;
        };
        let Some(search) = self.active_search.as_mut() else {
            return true;
        };
        if PromptHistoryNavigation::navigate(
            &lesson.input.searches,
            &mut search.history_navigation,
            &mut search.draft,
            direction,
        ) {
            search.preview = None;
            self.set_legacy_message(None);
            self.update_search_preview();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn learn_search_and_command_history_remain_local() {
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
        editor.record_search_history("original search");
        editor.record_command_history("original command");
        let searches = editor.preferences.search_history().to_vec();
        let commands = editor.preferences.command_history().to_vec();
        editor.search_term = "original search".into();
        editor.search_direction = SearchDirection::Backward;
        editor.search_highlights_suppressed = true;
        editor
            .start_learn_lesson(Lesson::FindAndReplace, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert!(editor.search_term.is_empty());
        editor.record_search_history("old");
        editor.record_command_history("%s/old/new/g");
        assert_eq!(editor.preferences.search_history(), searches);
        assert_eq!(editor.preferences.command_history(), commands);
        editor.begin_search(SearchDirection::Forward);
        editor.navigate_search_history(PromptHistoryDirection::Previous);
        assert_eq!(editor.active_search_text(), Some("old"));
        editor.cancel_active_search();
        editor.command.clear();
        editor.navigate_command_history(PromptHistoryDirection::Previous);
        assert_eq!(editor.command, "%s/old/new/g");
        editor.substitute_confirmation = Some(SubstituteConfirmation {
            substitutions: Vec::new(),
            current: 0,
            accepted: Vec::new(),
        });
        editor
            .finish_learn_lesson(&mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(editor.search_term, "original search");
        assert_eq!(editor.search_direction, SearchDirection::Backward);
        assert!(editor.search_highlights_suppressed);
        assert!(editor.substitute_confirmation.is_none());
        assert_eq!(editor.preferences.search_history(), searches);
        assert_eq!(editor.preferences.command_history(), commands);
    }
}
