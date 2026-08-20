//! Prompt history and submission policy around the reusable modal text area.
//!
//! Plugin composers and agent dialogs own independent [`TextArea`] values. Their
//! documents never enter the editor's file-buffer list; this wrapper adds bounded
//! prompt history plus surface-specific submit and cancellation shortcuts.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::{
    buffer::Buffer,
    editing::{TextArea, TextAreaOutcome},
    editor::Mode,
    text_layout::{LayoutOptions, TextLayout},
    unicode_utils::grapheme_len,
};

/// Largest prompt accepted by the direct Codex app-server integration.
pub(crate) const PROMPT_MAX_BYTES: usize = 128 * 1024;

/// Outcome of applying one terminal input event to an ephemeral prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptInput {
    /// The draft, cursor, history selection, editor mode, or pending motion changed.
    Changed,
    /// The current complete prompt should be validated and submitted.
    Submit,
    /// The containing surface should be cancelled or blurred.
    Cancel,
    /// The event is not a prompt-editing action.
    Unhandled,
}

/// Submission policy belongs to the host surface, not the shared Vim engine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum PromptKeyPolicy {
    #[default]
    Vim,
    EnterSends,
    EnterSendsWithShellHistory,
}

impl PromptKeyPolicy {
    const fn enter_sends(self) -> bool {
        matches!(self, Self::EnterSends | Self::EnterSendsWithShellHistory)
    }

    const fn shell_history(self) -> bool {
        matches!(self, Self::EnterSendsWithShellHistory)
    }
}

const PROMPT_HISTORY_LIMIT: usize = 50;

#[derive(Debug)]
struct PromptHistoryDraft {
    text: String,
    cursor: usize,
}

#[derive(Debug, Default)]
struct PromptHistorySearch {
    query: String,
    current: Option<usize>,
}

/// Fileless modal text area with thread-local prompt history and submission policy.
#[derive(Debug)]
pub(crate) struct PromptBuffer {
    area: TextArea,
    key_policy: PromptKeyPolicy,
    history: Vec<String>,
    history_position: Option<usize>,
    history_draft: Option<PromptHistoryDraft>,
    history_search: Option<PromptHistorySearch>,
}

impl PromptBuffer {
    /// Creates an unnamed prompt in insert mode with no previous history.
    pub(crate) fn new(text: impl AsRef<str>) -> Self {
        Self::with_history(text, Vec::new())
    }

    /// Creates an unnamed prompt while preserving only bounded history entries.
    pub(crate) fn with_history(text: impl AsRef<str>, history: Vec<String>) -> Self {
        Self {
            area: TextArea::with_max_bytes(text, PROMPT_MAX_BYTES),
            key_policy: PromptKeyPolicy::default(),
            history: normalized_prompt_history(history),
            history_position: None,
            history_draft: None,
            history_search: None,
        }
    }

    /// Selects the surface-local Enter behavior without changing the text area.
    pub(crate) fn with_key_policy(mut self, key_policy: PromptKeyPolicy) -> Self {
        self.key_policy = key_policy;
        self
    }

    /// Returns the underlying unnamed editor buffer.
    #[must_use]
    pub(crate) fn buffer(&self) -> &Buffer {
        self.area.buffer()
    }

    /// Returns the exact UTF-8 draft without adding a synthetic newline.
    #[must_use]
    pub(crate) fn text(&self) -> String {
        self.area.text()
    }

    /// Returns the draft or the non-destructive reverse-search preview.
    #[must_use]
    pub(crate) fn display_text(&self) -> String {
        self.history_search
            .as_ref()
            .and_then(|search| search.current)
            .and_then(|index| self.history.get(index))
            .cloned()
            .unwrap_or_else(|| self.text())
    }

    /// Projects the current draft without modifying its logical representation.
    pub(crate) fn layout(&self, options: LayoutOptions) -> TextLayout {
        TextLayout::new(&self.display_text(), options)
    }

    /// Returns the cursor as an absolute extended-grapheme index.
    #[must_use]
    pub(crate) const fn cursor(&self) -> usize {
        self.area.cursor()
    }

    /// Returns the cursor for the draft or reverse-search preview.
    #[must_use]
    pub(crate) fn display_cursor(&self) -> usize {
        if self.history_search.is_some() {
            grapheme_len(&self.display_text())
        } else {
            self.cursor()
        }
    }

    /// Returns the prompt's independent editor mode.
    #[must_use]
    pub(crate) const fn mode(&self) -> Mode {
        self.area.mode()
    }

    /// Whether a local Vim count or command still owns the next key.
    pub(crate) fn has_pending_input(&self) -> bool {
        self.area.state().has_pending_input()
    }

    /// Changes the local prompt mode without mutating the global editor.
    pub(crate) fn set_mode(&mut self, mode: Mode) {
        if mode != Mode::Insert {
            self.cancel_history_search();
        }
        self.area.set_mode(mode);
    }

    /// Returns thread-local prompt history, newest submission first.
    #[must_use]
    pub(crate) fn history(&self) -> &[String] {
        &self.history
    }

    /// Replaces surface-provided history without changing the current draft.
    pub(crate) fn set_history(&mut self, history: Vec<String>) {
        self.history = normalized_prompt_history(history);
        self.history_position = None;
        self.history_draft = None;
        self.cancel_history_search();
    }

    #[must_use]
    pub(crate) const fn history_search_active(&self) -> bool {
        self.history_search.is_some()
    }

    #[must_use]
    pub(crate) fn history_search_query(&self) -> Option<&str> {
        self.history_search
            .as_ref()
            .map(|search| search.query.as_str())
    }

    #[must_use]
    pub(crate) fn history_search_match_position(&self) -> Option<(usize, usize)> {
        let search = self.history_search.as_ref()?;
        let current = search.current?;
        let matches = self.history_matches(&search.query);
        let position = matches.iter().position(|index| *index == current)?;
        Some((position + 1, matches.len()))
    }

    /// Moves the cursor to a bounded absolute grapheme position.
    pub(crate) fn set_cursor(&mut self, cursor: usize) {
        self.area.set_cursor(cursor);
    }

    /// Replaces the complete draft through the shared transaction boundary.
    pub(crate) fn set_text(&mut self, text: &str) -> bool {
        self.area.set_text(text)
    }

    /// Loads a different draft as its own undoable edit, leaving prompt-history
    /// browsing and any previous insert session before editing the replacement.
    pub(crate) fn replace_draft(&mut self, text: &str) -> bool {
        let text = normalize_prompt_newlines(text);
        if text.len() > PROMPT_MAX_BYTES {
            return false;
        }
        self.area.set_mode(Mode::Normal);
        let changed = self.area.set_text(&text);
        self.history_position = None;
        self.history_draft = None;
        self.history_search = None;
        self.area.set_mode(Mode::Insert);
        changed
    }

    /// Inserts normalized text as one undoable prompt transaction.
    pub(crate) fn insert(&mut self, text: &str) -> bool {
        self.cancel_history_search();
        let changed = self.area.insert(text);
        self.detach_history_after_edit(changed);
        changed
    }

    /// Removes the previous complete extended grapheme.
    pub(crate) fn backspace(&mut self) -> bool {
        self.cancel_history_search();
        let changed = self.area.backspace();
        self.detach_history_after_edit(changed);
        changed
    }

    /// Removes the complete extended grapheme under the cursor.
    pub(crate) fn delete(&mut self) -> bool {
        self.cancel_history_search();
        let changed = self.area.delete();
        self.detach_history_after_edit(changed);
        changed
    }

    /// Removes whitespace and the word immediately preceding the cursor.
    pub(crate) fn delete_previous_word(&mut self) -> bool {
        self.cancel_history_search();
        let changed = self.area.delete_previous_word();
        self.detach_history_after_edit(changed);
        changed
    }

    /// Selects the previous submitted prompt while preserving the current draft.
    pub(crate) fn history_previous(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        let index = match self.history_position {
            Some(index) => (index + 1).min(self.history.len() - 1),
            None => {
                self.history_draft = Some(PromptHistoryDraft {
                    text: self.text(),
                    cursor: self.cursor(),
                });
                0
            }
        };
        let entry = self.history[index].clone();
        self.history_position = Some(index);
        self.set_text(&entry)
    }

    /// Moves toward newer history or restores the exact unsent draft.
    pub(crate) fn history_next(&mut self) -> bool {
        let Some(index) = self.history_position else {
            return false;
        };
        let (next, cursor) = if index == 0 {
            self.history_position = None;
            let draft = self.history_draft.take().unwrap_or(PromptHistoryDraft {
                text: String::new(),
                cursor: 0,
            });
            (draft.text, Some(draft.cursor))
        } else {
            self.history_position = Some(index - 1);
            (self.history[index - 1].clone(), None)
        };
        let changed = self.set_text(&next);
        if let Some(cursor) = cursor {
            self.set_cursor(cursor);
        }
        changed
    }

    /// Undoes one real, branch-preserving prompt edit.
    pub(crate) fn undo(&mut self) -> bool {
        self.area.undo()
    }

    /// Redoes one real prompt transaction on the selected undo branch.
    pub(crate) fn redo(&mut self) -> bool {
        self.area.redo()
    }

    /// Clears the draft without associating it with a file or losing history.
    pub(crate) fn clear(&mut self) {
        self.area.clear();
        self.history_position = None;
        self.history_draft = None;
        self.history_search = None;
    }

    /// Takes a nonblank draft and adds it to bounded, deduplicated history.
    pub(crate) fn take_submission(&mut self) -> Option<String> {
        let text = self.text();
        if text.trim().is_empty() || text.len() > PROMPT_MAX_BYTES {
            return None;
        }
        self.history.retain(|entry| entry != &text);
        self.history.insert(0, text.clone());
        self.history.truncate(PROMPT_HISTORY_LIMIT);
        self.clear();
        self.set_mode(Mode::Insert);
        Some(text)
    }

    /// Exercises the legacy grapheme-wrap policy in prompt compatibility tests.
    #[cfg(test)]
    pub(crate) fn handle_event(&mut self, event: &Event, wrap_width: usize) -> PromptInput {
        self.handle_event_with_layout_options(event, LayoutOptions::grapheme(wrap_width.max(1)))
    }

    /// Applies local prompt policy before delegating modal editing to the shared engine.
    pub(crate) fn handle_event_with_layout_options(
        &mut self,
        event: &Event,
        layout: LayoutOptions,
    ) -> PromptInput {
        let Event::Key(key) = event else {
            return self.apply_area_event(event, layout);
        };
        if key.kind == KeyEventKind::Release {
            return PromptInput::Changed;
        }
        if self.history_search.is_some() {
            return self.handle_history_search_event(event, layout);
        }
        if self.key_policy.shell_history() {
            if let Some(outcome) = self.handle_shell_history_key(*key, layout) {
                return outcome;
            }
        }
        if self.key_policy.enter_sends() {
            if let Some(outcome) = self.handle_composer_key(*key, layout) {
                return outcome;
            }
        }
        let modifiers = key.modifiers;
        match key.code {
            KeyCode::Enter | KeyCode::Char('\n')
                if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                PromptInput::Submit
            }
            KeyCode::Char('c' | 'C') if modifiers.contains(KeyModifiers::CONTROL) => {
                PromptInput::Cancel
            }
            KeyCode::Esc
                if self.mode() == Mode::Normal && !self.area.state().has_pending_input() =>
            {
                PromptInput::Cancel
            }
            KeyCode::Enter if self.mode() == Mode::Normal => PromptInput::Submit,
            KeyCode::Char('j' | 'J') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert("\n");
                PromptInput::Changed
            }
            KeyCode::Char('p' | 'P') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.history_previous();
                PromptInput::Changed
            }
            KeyCode::Char('n' | 'N') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.history_next();
                PromptInput::Changed
            }
            KeyCode::Char('w' | 'W')
                if modifiers.contains(KeyModifiers::CONTROL) && self.mode() != Mode::Search =>
            {
                self.delete_previous_word();
                PromptInput::Changed
            }
            KeyCode::Char('r' | 'R')
                if modifiers.contains(KeyModifiers::CONTROL)
                    && self.key_policy.shell_history()
                    && self.mode() == Mode::Insert =>
            {
                self.begin_history_search();
                PromptInput::Changed
            }
            KeyCode::Char('r' | 'R') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.redo();
                PromptInput::Changed
            }
            KeyCode::Char('z' | 'Z') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.undo();
                PromptInput::Changed
            }
            KeyCode::Char('a' | 'A') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.set_cursor(0);
                PromptInput::Changed
            }
            KeyCode::Char('e' | 'E') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.set_cursor(grapheme_len(&self.text()));
                PromptInput::Changed
            }
            _ => self.apply_area_event(event, layout),
        }
    }

    fn handle_composer_key(&mut self, key: KeyEvent, layout: LayoutOptions) -> Option<PromptInput> {
        let enter = matches!(key.code, KeyCode::Enter | KeyCode::Char('\r'));
        let newline = matches!(key.code, KeyCode::Char('\n'))
            || (matches!(key.code, KeyCode::Char('j' | 'J'))
                && key.modifiers.contains(KeyModifiers::CONTROL))
            || (enter
                && key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT));
        if !enter && !newline {
            return None;
        }
        if !key
            .modifiers
            .difference(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT)
            .is_empty()
        {
            return Some(PromptInput::Changed);
        }

        // A search owns Enter, and unfinished Vim commands must not send a draft.
        if self.mode() == Mode::Search || self.area.state().has_pending_input() {
            let outcome = self.apply_area_event(&Event::Key(key), layout);
            return Some(match outcome {
                PromptInput::Unhandled => PromptInput::Changed,
                outcome => outcome,
            });
        }
        if newline {
            if self.mode() == Mode::Insert {
                // Preserve insert-session undo grouping and dot-repeat recording.
                self.apply_area_event(
                    &Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                    layout,
                );
            } else {
                self.insert("\n");
            }
            return Some(PromptInput::Changed);
        }
        Some(if key.kind == KeyEventKind::Repeat {
            PromptInput::Changed
        } else if matches!(self.mode(), Mode::Insert | Mode::Normal) {
            PromptInput::Submit
        } else {
            // A visual selection remains local until the user leaves visual mode.
            PromptInput::Changed
        })
    }

    fn handle_shell_history_key(
        &mut self,
        key: KeyEvent,
        options: LayoutOptions,
    ) -> Option<PromptInput> {
        if self.mode() != Mode::Insert || !key.modifiers.is_empty() {
            return None;
        }
        match key.code {
            KeyCode::Up => {
                let layout = TextLayout::new(&self.text(), options);
                let first_row = layout
                    .position(self.cursor())
                    .is_none_or(|position| position.row == 0);
                if self.history_position.is_some() || first_row {
                    self.history_previous();
                    Some(PromptInput::Changed)
                } else {
                    None
                }
            }
            KeyCode::Down if self.history_position.is_some() => {
                self.history_next();
                Some(PromptInput::Changed)
            }
            _ => None,
        }
    }

    fn begin_history_search(&mut self) {
        let current = (!self.history.is_empty()).then_some(0);
        self.history_search = Some(PromptHistorySearch {
            query: String::new(),
            current,
        });
    }

    fn handle_history_search_event(&mut self, event: &Event, layout: LayoutOptions) -> PromptInput {
        match event {
            Event::Paste(text) => {
                if let Some(search) = self.history_search.as_mut() {
                    search.query.push_str(&normalize_prompt_newlines(text));
                }
                self.select_first_history_match();
            }
            Event::Key(key) => match key.code {
                KeyCode::Esc | KeyCode::Char('g' | 'G')
                    if key.code == KeyCode::Esc
                        || key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    self.cancel_history_search();
                }
                KeyCode::Enter | KeyCode::Char('\r') => {
                    self.accept_history_search();
                }
                KeyCode::Backspace => {
                    if let Some(search) = self.history_search.as_mut() {
                        search.query.pop();
                    }
                    self.select_first_history_match();
                }
                KeyCode::Char('r' | 'R') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.select_older_history_match();
                }
                KeyCode::Up if key.modifiers.is_empty() => {
                    self.select_older_history_match();
                }
                KeyCode::Down if key.modifiers.is_empty() => {
                    self.select_newer_history_match();
                }
                KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End
                    if key.modifiers.is_empty() =>
                {
                    if self.accept_history_search() {
                        return self.apply_area_event(event, layout);
                    }
                }
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    if let Some(search) = self.history_search.as_mut() {
                        search.query.push(character);
                    }
                    self.select_first_history_match();
                }
                _ => {}
            },
            _ => {}
        }
        PromptInput::Changed
    }

    fn history_matches(&self, query: &str) -> Vec<usize> {
        self.history
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| entry.contains(query).then_some(index))
            .collect()
    }

    fn select_first_history_match(&mut self) {
        let Some(query) = self
            .history_search
            .as_ref()
            .map(|search| search.query.clone())
        else {
            return;
        };
        let current = self.history_matches(&query).first().copied();
        if let Some(search) = self.history_search.as_mut() {
            search.current = current;
        }
    }

    fn select_older_history_match(&mut self) {
        let Some((query, current)) = self
            .history_search
            .as_ref()
            .map(|search| (search.query.clone(), search.current))
        else {
            return;
        };
        let matches = self.history_matches(&query);
        let next = current
            .and_then(|current| matches.iter().position(|index| *index == current))
            .and_then(|position| matches.get(position + 1))
            .copied()
            .or_else(|| {
                current
                    .is_none()
                    .then(|| matches.first().copied())
                    .flatten()
            })
            .or(current);
        if let Some(search) = self.history_search.as_mut() {
            search.current = next;
        }
    }

    fn select_newer_history_match(&mut self) {
        let Some((query, current)) = self
            .history_search
            .as_ref()
            .map(|search| (search.query.clone(), search.current))
        else {
            return;
        };
        let matches = self.history_matches(&query);
        let next = current
            .and_then(|current| matches.iter().position(|index| *index == current))
            .and_then(|position| position.checked_sub(1))
            .and_then(|position| matches.get(position))
            .copied()
            .or(current);
        if let Some(search) = self.history_search.as_mut() {
            search.current = next;
        }
    }

    pub(crate) fn accept_history_search(&mut self) -> bool {
        let Some(current) = self
            .history_search
            .as_ref()
            .and_then(|search| search.current)
        else {
            return false;
        };
        let Some(entry) = self.history.get(current).cloned() else {
            return false;
        };
        self.history_search = None;
        self.replace_draft(&entry)
    }

    pub(crate) fn cancel_history_search(&mut self) -> bool {
        self.history_search.take().is_some()
    }

    fn apply_area_event(&mut self, event: &Event, layout: LayoutOptions) -> PromptInput {
        let previous_revision = self.buffer().revision();
        let result = self.area.handle_event_with_layout_options(event, layout);
        self.detach_history_after_edit(self.buffer().revision() != previous_revision);
        match result {
            TextAreaOutcome::Changed => PromptInput::Changed,
            TextAreaOutcome::Unhandled => PromptInput::Unhandled,
        }
    }

    fn detach_history_after_edit(&mut self, changed: bool) {
        if changed {
            self.history_position = None;
            self.history_draft = None;
        }
    }
}

fn normalized_prompt_history(history: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for entry in history {
        let entry = normalize_prompt_newlines(&entry);
        if entry.trim().is_empty() || entry.len() > PROMPT_MAX_BYTES || normalized.contains(&entry)
        {
            continue;
        }
        normalized.push(entry);
        if normalized.len() == PROMPT_HISTORY_LIMIT {
            break;
        }
    }
    normalized
}

/// Normalizes terminal and clipboard line endings before prompt editing.
#[must_use]
pub(crate) fn normalize_prompt_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Keeps multiline paste from submitting or executing a single-line prompt.
#[must_use]
pub(crate) fn first_prompt_line(text: &str) -> String {
    normalize_prompt_newlines(text)
        .split('\n')
        .next()
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    use super::{
        first_prompt_line, normalize_prompt_newlines, Mode, PromptBuffer, PromptInput,
        PromptKeyPolicy, PROMPT_MAX_BYTES,
    };
    use crate::text_layout::LayoutOptions;

    #[test]
    fn single_line_paste_shares_crlf_normalization_without_accepting_a_newline() {
        assert_eq!(
            normalize_prompt_newlines("first\r\nsecond\rthird"),
            "first\nsecond\nthird"
        );
        assert_eq!(first_prompt_line("first\r\nsecond"), "first");
        assert_eq!(first_prompt_line("\r\nsecond"), "");
        assert_eq!(first_prompt_line("👨‍👩‍👧\nignored"), "👨‍👩‍👧");
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    fn composer(text: &str) -> PromptBuffer {
        PromptBuffer::new(text).with_key_policy(PromptKeyPolicy::EnterSends)
    }

    #[test]
    fn word_backspace_detaches_history_under_both_prompt_policies() {
        for policy in [PromptKeyPolicy::Vim, PromptKeyPolicy::EnterSends] {
            for modifiers in [KeyModifiers::ALT, KeyModifiers::CONTROL] {
                let mut prompt =
                    PromptBuffer::with_history("unsent draft", vec!["recalled entry".to_string()])
                        .with_key_policy(policy);
                assert!(prompt.history_previous());
                prompt.handle_event(&key(KeyCode::Backspace, modifiers), 40);
                assert_eq!(prompt.text(), "recalled ");
                assert!(!prompt.history_next());
                assert!(prompt.undo());
                assert_eq!(prompt.text(), "recalled entry");
            }
        }
    }

    #[test]
    fn word_backspace_does_not_delete_the_draft_during_embedded_search() {
        for shortcut in [
            key(KeyCode::Backspace, KeyModifiers::ALT),
            key(KeyCode::Backspace, KeyModifiers::CONTROL),
            key(KeyCode::Char('w'), KeyModifiers::CONTROL),
        ] {
            let mut prompt = composer("keep this draft");
            prompt.set_mode(Mode::Normal);
            for character in "/first second".chars() {
                prompt.handle_event(&key(KeyCode::Char(character), KeyModifiers::NONE), 40);
            }
            prompt.handle_event(&shortcut, 40);
            assert_eq!(prompt.text(), "keep this draft");
            assert_eq!(prompt.mode(), Mode::Search);
        }
    }

    #[test]
    fn composer_enter_policy_keeps_submit_and_newline_distinct() {
        let cases = [
            (KeyCode::Enter, KeyModifiers::NONE, PromptInput::Submit),
            (KeyCode::Char('\r'), KeyModifiers::NONE, PromptInput::Submit),
            (KeyCode::Enter, KeyModifiers::CONTROL, PromptInput::Submit),
            (KeyCode::Enter, KeyModifiers::ALT, PromptInput::Changed),
            (KeyCode::Enter, KeyModifiers::SHIFT, PromptInput::Changed),
            (
                KeyCode::Enter,
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                PromptInput::Changed,
            ),
            (
                KeyCode::Enter,
                KeyModifiers::CONTROL | KeyModifiers::ALT,
                PromptInput::Changed,
            ),
            (
                KeyCode::Char('j'),
                KeyModifiers::CONTROL,
                PromptInput::Changed,
            ),
            (
                KeyCode::Char('\n'),
                KeyModifiers::NONE,
                PromptInput::Changed,
            ),
            (
                KeyCode::Char('\n'),
                KeyModifiers::CONTROL,
                PromptInput::Changed,
            ),
        ];
        for mode in [Mode::Insert, Mode::Normal] {
            for (code, modifiers, expected) in cases {
                let mut prompt = composer("hello");
                prompt.set_mode(mode);
                assert_eq!(
                    prompt.handle_event(&key(code, modifiers), 40),
                    expected,
                    "{mode:?} {code:?} {modifiers:?}"
                );
                assert_eq!(
                    prompt.text(),
                    if expected == PromptInput::Submit {
                        "hello"
                    } else {
                        "hello\n"
                    }
                );
                assert_eq!(prompt.mode(), mode);
            }
        }
        let mut ordinary = PromptBuffer::new("hello");
        assert_eq!(
            ordinary.handle_event(&key(KeyCode::Enter, KeyModifiers::NONE), 40),
            PromptInput::Changed
        );
        assert_eq!(ordinary.text(), "hello\n");
    }

    #[test]
    fn word_wrapped_composer_keeps_enter_and_newline_at_the_visual_cursor() {
        let layout = LayoutOptions::word(7);
        let mut prompt = composer("one two three");
        prompt.set_cursor(0);
        assert_eq!(
            prompt
                .handle_event_with_layout_options(&key(KeyCode::Down, KeyModifiers::NONE), layout),
            PromptInput::Changed
        );
        assert_eq!(prompt.cursor(), 8);
        assert_eq!(
            prompt.handle_event_with_layout_options(
                &key(KeyCode::Enter, KeyModifiers::SHIFT),
                layout
            ),
            PromptInput::Changed
        );
        assert_eq!(prompt.text(), "one two \nthree");
        assert!(prompt.undo());
        assert_eq!(prompt.text(), "one two three");
        assert!(prompt.redo());
        assert_eq!(
            prompt
                .handle_event_with_layout_options(&key(KeyCode::Enter, KeyModifiers::NONE), layout),
            PromptInput::Submit
        );
        assert_eq!(prompt.text(), "one two \nthree");
    }

    #[test]
    fn composer_enter_does_not_escape_vim_substates_or_repeat_submission() {
        let mut prompt = composer("one two");
        prompt.set_mode(Mode::Normal);
        prompt.handle_event(&key(KeyCode::Char('/'), KeyModifiers::NONE), 40);
        assert_eq!(prompt.mode(), Mode::Search);
        prompt.handle_event(&key(KeyCode::Char('t'), KeyModifiers::NONE), 40);
        assert_eq!(
            prompt.handle_event(&key(KeyCode::Enter, KeyModifiers::NONE), 40),
            PromptInput::Changed
        );
        assert_eq!(prompt.mode(), Mode::Normal);
        prompt.handle_event(&key(KeyCode::Char('d'), KeyModifiers::NONE), 40);
        assert_eq!(
            prompt.handle_event(&key(KeyCode::Enter, KeyModifiers::NONE), 40),
            PromptInput::Changed
        );
        assert_eq!(prompt.text(), "one two");
        prompt.set_mode(Mode::Visual);
        assert_eq!(
            prompt.handle_event(&key(KeyCode::Enter, KeyModifiers::NONE), 40),
            PromptInput::Changed
        );
        prompt.set_mode(Mode::Insert);
        for kind in [KeyEventKind::Repeat, KeyEventKind::Release] {
            assert_eq!(
                prompt.handle_event(
                    &Event::Key(KeyEvent::new_with_kind(
                        KeyCode::Enter,
                        KeyModifiers::NONE,
                        kind
                    )),
                    40
                ),
                PromptInput::Changed
            );
        }
        assert_eq!(prompt.text(), "one two");
    }

    #[test]
    fn composer_newline_is_undoable_and_paste_never_submits() {
        let mut prompt = composer("first");
        prompt.handle_event(&key(KeyCode::Enter, KeyModifiers::ALT), 40);
        assert_eq!(prompt.text(), "first\n");
        assert!(prompt.undo());
        assert_eq!(prompt.text(), "first");
        assert!(prompt.redo());
        assert_eq!(prompt.text(), "first\n");
        assert_eq!(
            prompt.handle_event(&Event::Paste("second\r\nthird".into()), 40),
            PromptInput::Changed
        );
        assert_eq!(prompt.text(), "first\nsecond\nthird");
    }

    #[test]
    fn empty_prompt_is_a_real_unnamed_buffer_without_a_synthetic_newline() {
        let prompt = PromptBuffer::new("");

        assert!(prompt.buffer().file.is_none());
        assert_eq!(prompt.buffer().contents(), "");
        assert_eq!(prompt.text(), "");
        assert_eq!(prompt.cursor(), 0);
        assert_eq!(prompt.mode(), Mode::Insert);
        assert_eq!(prompt.buffer().undo_history.node_count(), 0);
    }

    #[test]
    fn prompt_normalizes_initial_text_history_and_pasted_newlines() {
        let mut prompt = PromptBuffer::with_history(
            "first\r\nsecond\rthird",
            vec!["history\r\nentry".to_string()],
        );

        assert_eq!(prompt.text(), "first\nsecond\nthird");
        assert_eq!(prompt.history(), ["history\nentry"]);
        assert_eq!(
            prompt.handle_event(&Event::Paste("\r\nfourth".to_string()), 40),
            PromptInput::Changed
        );
        assert_eq!(prompt.text(), "first\nsecond\nthird\nfourth");
    }

    #[test]
    fn edits_commit_to_the_real_buffer_undo_and_redo_history() {
        let mut prompt = PromptBuffer::new("hello");

        assert!(prompt.insert(" world"));
        assert_eq!(prompt.text(), "hello world");
        assert_eq!(prompt.buffer().undo_history.node_count(), 1);
        assert!(prompt.undo());
        assert_eq!(prompt.text(), "hello");
        assert_eq!(prompt.cursor(), 5);
        assert!(prompt.redo());
        assert_eq!(prompt.text(), "hello world");
        assert_eq!(prompt.cursor(), 11);
    }

    #[test]
    fn insertion_keeps_cursor_after_merged_graphemes() {
        let cases = [
            (
                "combining mark",
                "aX",
                1,
                "\u{301}",
                "a\u{301}X",
                1,
                "a\u{301}ZX",
            ),
            (
                "stacked combining marks",
                "a\u{301}X",
                1,
                "\u{327}",
                "a\u{301}\u{327}X",
                1,
                "a\u{301}\u{327}ZX",
            ),
            ("emoji modifier", "👍X", 1, "🏽", "👍🏽X", 1, "👍🏽ZX"),
            (
                "joined emoji",
                "👩X",
                1,
                "\u{200d}💻",
                "👩\u{200d}💻X",
                1,
                "👩\u{200d}💻ZX",
            ),
            (
                "multiline combining mark",
                "one\naX\ntail",
                5,
                "\u{301}",
                "one\na\u{301}X\ntail",
                5,
                "one\na\u{301}ZX\ntail",
            ),
            (
                "multiple inserted graphemes",
                "aX",
                1,
                "\u{301}b",
                "a\u{301}bX",
                2,
                "a\u{301}bZX",
            ),
        ];

        for (name, initial, cursor, inserted, merged, merged_cursor, subsequent) in cases {
            let mut prompt = PromptBuffer::new(initial);
            prompt.set_cursor(cursor);

            assert!(prompt.insert(inserted), "{name}: insert grapheme extension");
            assert_eq!(prompt.text(), merged, "{name}: preserve merged grapheme");
            assert_eq!(
                prompt.cursor(),
                merged_cursor,
                "{name}: position cursor after resulting prefix"
            );

            assert!(prompt.insert("Z"), "{name}: insert following character");
            assert_eq!(prompt.text(), subsequent, "{name}: preserve following text");
            assert_eq!(prompt.cursor(), merged_cursor + 1, "{name}: advance cursor");

            assert!(prompt.undo(), "{name}: undo following character");
            assert_eq!(prompt.text(), merged, "{name}: restore merged grapheme");
            assert_eq!(
                prompt.cursor(),
                merged_cursor,
                "{name}: restore merged cursor"
            );
            assert!(prompt.undo(), "{name}: undo grapheme extension");
            assert_eq!(prompt.text(), initial, "{name}: restore original text");
            assert_eq!(prompt.cursor(), cursor, "{name}: restore original cursor");

            assert!(prompt.redo(), "{name}: redo grapheme extension");
            assert_eq!(prompt.text(), merged, "{name}: redo merged grapheme");
            assert_eq!(prompt.cursor(), merged_cursor, "{name}: redo merged cursor");
            assert!(prompt.redo(), "{name}: redo following character");
            assert_eq!(prompt.text(), subsequent, "{name}: redo following text");
            assert_eq!(prompt.cursor(), merged_cursor + 1, "{name}: redo cursor");
        }
    }

    #[test]
    fn deletion_and_undo_preserve_complete_unicode_graphemes() {
        let mut prompt = PromptBuffer::new("e\u{301}👨‍👩‍👧漢");

        assert!(prompt.backspace());
        assert_eq!(prompt.text(), "e\u{301}👨‍👩‍👧");
        assert!(prompt.backspace());
        assert_eq!(prompt.text(), "e\u{301}");
        assert!(prompt.undo());
        assert_eq!(prompt.text(), "e\u{301}👨‍👩‍👧");
        assert!(prompt.undo());
        assert_eq!(prompt.text(), "e\u{301}👨‍👩‍👧漢");
        prompt.set_cursor(0);
        assert!(prompt.delete());
        assert_eq!(prompt.text(), "👨‍👩‍👧漢");
    }

    #[test]
    fn modified_enter_and_modified_linefeed_submit_in_insert_mode() {
        for event in [
            key(KeyCode::Enter, KeyModifiers::CONTROL),
            key(KeyCode::Char('\n'), KeyModifiers::CONTROL),
            key(KeyCode::Enter, KeyModifiers::ALT),
            key(KeyCode::Char('\n'), KeyModifiers::ALT),
        ] {
            let mut prompt = PromptBuffer::new("send this");

            assert_eq!(prompt.handle_event(&event, 40), PromptInput::Submit);
            assert_eq!(prompt.text(), "send this");
            assert_eq!(prompt.mode(), Mode::Insert);
        }
    }

    #[test]
    fn ordinary_enter_and_control_j_insert_real_newlines() {
        let mut prompt = PromptBuffer::new("first");

        assert_eq!(
            prompt.handle_event(&key(KeyCode::Enter, KeyModifiers::NONE), 40),
            PromptInput::Changed
        );
        assert!(prompt.insert("second"));
        assert_eq!(
            prompt.handle_event(&key(KeyCode::Char('j'), KeyModifiers::CONTROL), 40),
            PromptInput::Changed
        );

        assert_eq!(prompt.text(), "first\nsecond\n");
        assert_eq!(prompt.buffer().undo_history.node_count(), 3);
    }

    #[test]
    fn escape_enters_normal_mode_and_enter_submits_without_mutating_the_draft() {
        let mut prompt = PromptBuffer::new("draft");

        assert_eq!(
            prompt.handle_event(&key(KeyCode::Esc, KeyModifiers::NONE), 40),
            PromptInput::Changed
        );
        assert_eq!(prompt.mode(), Mode::Normal);
        assert_eq!(
            prompt.handle_event(&key(KeyCode::Enter, KeyModifiers::NONE), 40),
            PromptInput::Submit
        );
        assert_eq!(prompt.text(), "draft");
    }

    #[test]
    fn normal_mode_line_boundary_escape_selects_a_current_line_grapheme() {
        let cases = [
            ("single line", "abc", 3, 2),
            ("first line end", "abc\ndef", 3, 2),
            ("second line start", "abc\ndef", 4, 4),
            ("second line interior", "abc\ndef", 6, 5),
            ("empty middle line", "abc\n\ndef", 4, 4),
            ("empty draft", "", 0, 0),
            ("Unicode graphemes", "e\u{301}👨‍👩‍👧", 2, 1),
        ];

        for (name, text, insertion_cursor, normal_cursor) in cases {
            let mut prompt = PromptBuffer::new(text);
            prompt.set_cursor(insertion_cursor);

            assert_eq!(
                prompt.handle_event(&key(KeyCode::Esc, KeyModifiers::NONE), 40),
                PromptInput::Changed,
                "{name}: enter normal mode"
            );
            assert_eq!(prompt.mode(), Mode::Normal, "{name}: set normal mode");
            assert_eq!(
                prompt.cursor(),
                normal_cursor,
                "{name}: remain on the current line"
            );
            assert_eq!(prompt.text(), text, "{name}: preserve draft");
        }
    }

    #[test]
    fn normal_mode_line_boundary_horizontal_motions_stay_on_current_line() {
        let cases = [
            (
                "h at second line start",
                "one\ntwo",
                4,
                'h',
                4,
                Some("one\nwo"),
            ),
            (
                "l at first line end",
                "one\ntwo",
                2,
                'l',
                2,
                Some("on\ntwo"),
            ),
            (
                "h within second line",
                "one\ntwo",
                5,
                'h',
                4,
                Some("one\nwo"),
            ),
            (
                "l within second line",
                "one\ntwo",
                4,
                'l',
                5,
                Some("one\nto"),
            ),
            ("h on empty line", "one\n\ntwo", 4, 'h', 4, None),
            ("l on empty line", "one\n\ntwo", 4, 'l', 4, None),
            (
                "l after Unicode grapheme",
                "e\u{301}👨‍👩‍👧\nlast",
                1,
                'l',
                1,
                Some("e\u{301}\nlast"),
            ),
        ];

        for (name, text, cursor, motion, expected_cursor, expected_delete) in cases {
            let mut prompt = PromptBuffer::new(text);
            prompt.set_cursor(cursor);
            prompt.set_mode(Mode::Normal);

            assert_eq!(
                prompt.handle_event(&key(KeyCode::Char(motion), KeyModifiers::NONE), 40),
                PromptInput::Changed,
                "{name}: apply horizontal motion"
            );
            assert_eq!(
                prompt.cursor(),
                expected_cursor,
                "{name}: stay on current line"
            );
            assert_eq!(prompt.text(), text, "{name}: preserve line breaks");

            if let Some(expected) = expected_delete {
                assert_eq!(
                    prompt.handle_event(&key(KeyCode::Char('x'), KeyModifiers::NONE), 40),
                    PromptInput::Changed,
                    "{name}: delete selected grapheme"
                );
                assert_eq!(prompt.text(), expected, "{name}: preserve adjacent line");
            }
        }
    }

    #[test]
    fn normal_mode_line_boundary_arrow_motions_stay_on_current_line() {
        let cases = [
            (
                "Left at second line start",
                "one\ntwo",
                4,
                KeyCode::Left,
                4,
                Some("one\nwo"),
            ),
            (
                "Right at first line end",
                "one\ntwo",
                2,
                KeyCode::Right,
                2,
                Some("on\ntwo"),
            ),
            (
                "Left within second line",
                "one\ntwo",
                5,
                KeyCode::Left,
                4,
                Some("one\nwo"),
            ),
            (
                "Right within second line",
                "one\ntwo",
                4,
                KeyCode::Right,
                5,
                Some("one\nto"),
            ),
            (
                "Left on empty line",
                "one\n\ntwo",
                4,
                KeyCode::Left,
                4,
                None,
            ),
            (
                "Right on empty line",
                "one\n\ntwo",
                4,
                KeyCode::Right,
                4,
                None,
            ),
            (
                "Right after Unicode grapheme",
                "e\u{301}👨‍👩‍👧\nlast",
                1,
                KeyCode::Right,
                1,
                Some("e\u{301}\nlast"),
            ),
        ];

        for (name, text, cursor, arrow, expected_cursor, expected_delete) in cases {
            let mut prompt = PromptBuffer::new(text);
            prompt.set_cursor(cursor);
            prompt.set_mode(Mode::Normal);

            assert_eq!(
                prompt.handle_event(&key(arrow, KeyModifiers::NONE), 40),
                PromptInput::Changed,
                "{name}: move with arrow key"
            );
            assert_eq!(prompt.mode(), Mode::Normal, "{name}: remain in normal mode");
            assert_eq!(
                prompt.cursor(),
                expected_cursor,
                "{name}: stay on current line"
            );
            assert_eq!(prompt.text(), text, "{name}: preserve line breaks");

            if let Some(expected) = expected_delete {
                assert_eq!(
                    prompt.handle_event(&key(KeyCode::Char('x'), KeyModifiers::NONE), 40),
                    PromptInput::Changed,
                    "{name}: delete selected grapheme"
                );
                assert_eq!(prompt.text(), expected, "{name}: preserve adjacent line");
            }
        }
    }

    #[test]
    fn insert_mode_arrow_motions_cross_prompt_line_boundaries() {
        let cases = [
            (
                "Left from second line",
                "one\ntwo",
                4,
                KeyCode::Left,
                3,
                "oneZ\ntwo",
            ),
            (
                "Right across newline",
                "one\ntwo",
                3,
                KeyCode::Right,
                4,
                "one\nZtwo",
            ),
            (
                "Right across empty line",
                "one\n\ntwo",
                4,
                KeyCode::Right,
                5,
                "one\n\nZtwo",
            ),
            (
                "Left after Unicode graphemes",
                "e\u{301}👨‍👩‍👧\nlast",
                3,
                KeyCode::Left,
                2,
                "e\u{301}👨‍👩‍👧Z\nlast",
            ),
        ];

        for (name, text, cursor, arrow, expected_cursor, expected_text) in cases {
            let mut prompt = PromptBuffer::new(text);
            prompt.set_cursor(cursor);

            assert_eq!(
                prompt.handle_event(&key(arrow, KeyModifiers::NONE), 40),
                PromptInput::Changed,
                "{name}: move with arrow key"
            );
            assert_eq!(prompt.mode(), Mode::Insert, "{name}: remain in insert mode");
            assert_eq!(
                prompt.cursor(),
                expected_cursor,
                "{name}: cross line boundary"
            );

            assert_eq!(
                prompt.handle_event(&key(KeyCode::Char('Z'), KeyModifiers::NONE), 40),
                PromptInput::Changed,
                "{name}: insert at destination"
            );
            assert_eq!(
                prompt.text(),
                expected_text,
                "{name}: preserve cursor semantics"
            );
        }
    }

    #[test]
    fn normal_mode_line_boundary_delete_preserves_empty_line_separators() {
        let cases = [
            ("empty first line", "\ntwo", 0),
            ("empty middle line", "one\n\ntwo", 4),
            ("empty final line", "one\n", 4),
            ("only a newline", "\n", 0),
            ("empty draft", "", 0),
        ];

        for (name, text, cursor) in cases {
            let mut prompt = PromptBuffer::new(text);
            prompt.set_cursor(cursor);
            prompt.set_mode(Mode::Normal);
            let original_history = prompt.buffer().undo_history.node_count();

            assert_eq!(
                prompt.handle_event(&key(KeyCode::Char('$'), KeyModifiers::NONE), 40),
                PromptInput::Changed,
                "{name}: select empty line"
            );
            assert_eq!(prompt.cursor(), cursor, "{name}: remain on empty line");

            assert_eq!(
                prompt.handle_event(&key(KeyCode::Char('x'), KeyModifiers::NONE), 40),
                PromptInput::Changed,
                "{name}: ignore deletion on empty line"
            );
            assert_eq!(prompt.text(), text, "{name}: preserve newline separator");
            assert_eq!(prompt.cursor(), cursor, "{name}: preserve cursor");
            assert_eq!(
                prompt.buffer().undo_history.node_count(),
                original_history,
                "{name}: avoid creating an undo transaction"
            );
        }
    }

    #[test]
    fn normal_mode_line_boundary_append_stays_on_empty_line() {
        let mut prompt = PromptBuffer::new("one\n\ntwo");
        prompt.set_cursor(4);
        prompt.set_mode(Mode::Normal);

        assert_eq!(
            prompt.handle_event(&key(KeyCode::Char('a'), KeyModifiers::NONE), 40),
            PromptInput::Changed
        );
        assert_eq!(prompt.mode(), Mode::Insert);
        assert_eq!(prompt.cursor(), 4);

        assert_eq!(
            prompt.handle_event(&key(KeyCode::Char('Z'), KeyModifiers::NONE), 40),
            PromptInput::Changed
        );
        assert_eq!(prompt.text(), "one\nZ\ntwo");
        assert_eq!(prompt.cursor(), 5);
    }

    #[test]
    fn normal_mode_supports_vim_word_deletion_and_real_undo() {
        let mut prompt = PromptBuffer::new("first second");
        prompt.set_cursor(0);
        prompt.set_mode(Mode::Normal);

        assert_eq!(
            prompt.handle_event(&key(KeyCode::Char('d'), KeyModifiers::NONE), 40),
            PromptInput::Changed
        );
        assert_eq!(
            prompt.handle_event(&key(KeyCode::Char('w'), KeyModifiers::NONE), 40),
            PromptInput::Changed
        );
        assert_eq!(prompt.text(), "second");
        assert_eq!(
            prompt.handle_event(&key(KeyCode::Char('u'), KeyModifiers::NONE), 40),
            PromptInput::Changed
        );
        assert_eq!(prompt.text(), "first second");
        assert_eq!(
            prompt.handle_event(&key(KeyCode::Char('r'), KeyModifiers::CONTROL), 40),
            PromptInput::Changed
        );
        assert_eq!(prompt.text(), "second");
    }

    #[test]
    fn normal_mode_line_end_selects_the_current_lines_final_grapheme() {
        let cases = [
            ("single line", "abc", 0, 2, Some("ab"), "abZc"),
            ("first line", "abc\ndef", 0, 2, Some("ab\ndef"), "abZc\ndef"),
            (
                "middle line",
                "one\ntwo\nthree",
                5,
                6,
                Some("one\ntw\nthree"),
                "one\ntwZo\nthree",
            ),
            ("last line", "one\ntwo", 5, 6, Some("one\ntw"), "one\ntwZo"),
            (
                "Unicode graphemes",
                "e\u{301}👨‍👩‍👧\nlast",
                0,
                1,
                Some("e\u{301}\nlast"),
                "e\u{301}Z👨‍👩‍👧\nlast",
            ),
            ("empty line", "first\n\nlast", 6, 6, None, "first\nZ\nlast"),
            ("empty draft", "", 0, 0, None, "Z"),
        ];

        for (name, text, cursor, line_end, expected_delete, expected_insert) in cases {
            let at_line_end = || {
                let mut prompt = PromptBuffer::new(text);
                prompt.set_cursor(cursor);
                prompt.set_mode(Mode::Normal);

                assert_eq!(
                    prompt.handle_event(&key(KeyCode::Char('$'), KeyModifiers::NONE), 40),
                    PromptInput::Changed,
                    "{name}: move to current line end"
                );
                assert_eq!(prompt.mode(), Mode::Normal, "{name}: remain in normal mode");
                assert_eq!(prompt.cursor(), line_end, "{name}: select final grapheme");
                prompt
            };

            if let Some(expected) = expected_delete {
                let mut prompt = at_line_end();
                assert_eq!(
                    prompt.handle_event(&key(KeyCode::Char('x'), KeyModifiers::NONE), 40),
                    PromptInput::Changed,
                    "{name}: delete final grapheme"
                );
                assert_eq!(prompt.text(), expected, "{name}: preserve other lines");
            }

            let mut prompt = at_line_end();
            assert_eq!(
                prompt.handle_event(&key(KeyCode::Char('i'), KeyModifiers::NONE), 40),
                PromptInput::Changed,
                "{name}: enter insert mode"
            );
            assert_eq!(prompt.mode(), Mode::Insert, "{name}: enter insert mode");
            assert_eq!(
                prompt.handle_event(&key(KeyCode::Char('Z'), KeyModifiers::NONE), 40),
                PromptInput::Changed,
                "{name}: insert before final grapheme"
            );
            assert_eq!(
                prompt.text(),
                expected_insert,
                "{name}: insert on current line"
            );
        }
    }

    #[test]
    fn normal_mode_append_stays_on_the_current_prompt_line() {
        let cases = [
            ("first line", "first\nsecond", 2, 5, "first!\nsecond"),
            ("middle line", "one\ntwo\nthree", 5, 7, "one\ntwo!\nthree"),
            ("last line", "first\nlast", 8, 10, "first\nlast!"),
            ("empty line", "first\n\nlast", 6, 6, "first\n!\nlast"),
            (
                "Unicode graphemes",
                "e\u{301}👨‍👩‍👧\nlast",
                0,
                2,
                "e\u{301}👨‍👩‍👧!\nlast",
            ),
        ];

        for (name, text, cursor, line_end, expected) in cases {
            let mut prompt = PromptBuffer::new(text);
            prompt.set_cursor(cursor);
            prompt.set_mode(Mode::Normal);

            assert_eq!(
                prompt.handle_event(&key(KeyCode::Char('A'), KeyModifiers::NONE), 40),
                PromptInput::Changed,
                "{name}: enter append mode"
            );
            assert_eq!(prompt.mode(), Mode::Insert, "{name}: enter insert mode");
            assert_eq!(prompt.cursor(), line_end, "{name}: stay on current line");

            assert_eq!(
                prompt.handle_event(&key(KeyCode::Char('!'), KeyModifiers::NONE), 40),
                PromptInput::Changed,
                "{name}: append to current line"
            );
            assert_eq!(prompt.text(), expected, "{name}: preserve remaining lines");
            assert_eq!(prompt.cursor(), line_end + 1, "{name}: advance cursor");
        }
    }

    #[test]
    fn normal_mode_open_line_enters_insert_without_sending() {
        let mut prompt = PromptBuffer::new("first\nlast");
        prompt.set_cursor(2);
        prompt.set_mode(Mode::Normal);

        assert_eq!(
            prompt.handle_event(&key(KeyCode::Char('o'), KeyModifiers::NONE), 40),
            PromptInput::Changed
        );
        assert_eq!(prompt.text(), "first\n\nlast");
        assert_eq!(prompt.mode(), Mode::Insert);
    }

    #[test]
    fn history_navigation_restores_the_exact_unsent_draft() {
        let mut prompt = PromptBuffer::with_history(
            "unsent draft",
            vec!["newer\r\nentry".to_string(), "older".to_string()],
        );

        assert!(prompt.history_previous());
        assert_eq!(prompt.text(), "newer\nentry");
        assert!(prompt.history_previous());
        assert_eq!(prompt.text(), "older");
        assert!(prompt.history_next());
        assert_eq!(prompt.text(), "newer\nentry");
        assert!(prompt.history_next());
        assert_eq!(prompt.text(), "unsent draft");
    }

    #[test]
    fn shell_history_arrows_preserve_multiline_motion_and_restore_the_draft_cursor() {
        let mut prompt = PromptBuffer::with_history(
            "first line\nsecond line",
            vec!["newest".to_string(), "older".to_string()],
        )
        .with_key_policy(PromptKeyPolicy::EnterSendsWithShellHistory);

        assert_eq!(
            prompt.handle_event(&key(KeyCode::Up, KeyModifiers::NONE), 40),
            PromptInput::Changed
        );
        assert_eq!(prompt.text(), "first line\nsecond line");
        let draft_cursor = prompt.cursor();

        prompt.handle_event(&key(KeyCode::Up, KeyModifiers::NONE), 40);
        assert_eq!(prompt.text(), "newest");
        prompt.handle_event(&key(KeyCode::Up, KeyModifiers::NONE), 40);
        assert_eq!(prompt.text(), "older");
        prompt.handle_event(&key(KeyCode::Down, KeyModifiers::NONE), 40);
        assert_eq!(prompt.text(), "newest");
        prompt.handle_event(&key(KeyCode::Down, KeyModifiers::NONE), 40);
        assert_eq!(prompt.text(), "first line\nsecond line");
        assert_eq!(prompt.cursor(), draft_cursor);
    }

    #[test]
    fn reverse_history_search_previews_cycles_accepts_and_cancels_without_submitting() {
        let mut prompt = PromptBuffer::with_history(
            "keep this draft",
            vec![
                "deploy production".to_string(),
                "show status".to_string(),
                "deploy staging".to_string(),
            ],
        )
        .with_key_policy(PromptKeyPolicy::EnterSendsWithShellHistory);
        prompt.set_cursor(4);

        prompt.handle_event(&key(KeyCode::Char('r'), KeyModifiers::CONTROL), 40);
        for character in ['d', 'e', 'p'] {
            prompt.handle_event(&key(KeyCode::Char(character), KeyModifiers::NONE), 40);
        }
        assert!(prompt.history_search_active());
        assert_eq!(prompt.text(), "keep this draft");
        assert_eq!(prompt.display_text(), "deploy production");
        assert_eq!(prompt.history_search_match_position(), Some((1, 2)));

        prompt.handle_event(&key(KeyCode::Char('r'), KeyModifiers::CONTROL), 40);
        assert_eq!(prompt.display_text(), "deploy staging");
        assert_eq!(prompt.history_search_match_position(), Some((2, 2)));
        assert_eq!(
            prompt.handle_event(&key(KeyCode::Enter, KeyModifiers::NONE), 40),
            PromptInput::Changed
        );
        assert_eq!(prompt.text(), "deploy staging");
        assert!(!prompt.history_search_active());
        assert_eq!(
            prompt.handle_event(&key(KeyCode::Enter, KeyModifiers::NONE), 40),
            PromptInput::Submit
        );

        prompt.replace_draft("another draft");
        prompt.set_cursor(3);
        prompt.handle_event(&key(KeyCode::Char('r'), KeyModifiers::CONTROL), 40);
        prompt.handle_event(&key(KeyCode::Char('x'), KeyModifiers::NONE), 40);
        assert_eq!(prompt.history_search_match_position(), None);
        prompt.handle_event(&key(KeyCode::Esc, KeyModifiers::NONE), 40);
        assert_eq!(prompt.text(), "another draft");
        assert_eq!(prompt.cursor(), 3);
        assert!(!prompt.history_search_active());
    }

    #[test]
    fn shell_history_keeps_control_r_as_normal_mode_redo() {
        let mut prompt =
            PromptBuffer::new("first").with_key_policy(PromptKeyPolicy::EnterSendsWithShellHistory);
        assert!(prompt.insert(" second"));
        assert!(prompt.undo());
        prompt.set_mode(Mode::Normal);

        assert_eq!(
            prompt.handle_event(&key(KeyCode::Char('r'), KeyModifiers::CONTROL), 40),
            PromptInput::Changed
        );
        assert_eq!(prompt.text(), "first second");
        assert!(!prompt.history_search_active());
    }

    #[test]
    fn history_navigation_detaches_after_successful_prompt_deletions() {
        type PromptEdit = fn(&mut PromptBuffer) -> bool;

        let scenarios: [(&str, PromptEdit, &str); 4] = [
            ("backspace", PromptBuffer::backspace, "recalled entr"),
            (
                "delete",
                |prompt| {
                    prompt.set_cursor(0);
                    prompt.delete()
                },
                "ecalled entry",
            ),
            (
                "delete previous word",
                PromptBuffer::delete_previous_word,
                "recalled ",
            ),
            (
                "Vim dw",
                |prompt| {
                    prompt.set_cursor(0);
                    prompt.set_mode(Mode::Normal);
                    assert_eq!(
                        prompt.handle_event(&key(KeyCode::Char('d'), KeyModifiers::NONE), 40),
                        PromptInput::Changed
                    );
                    prompt.handle_event(&key(KeyCode::Char('w'), KeyModifiers::NONE), 40)
                        == PromptInput::Changed
                },
                "entry",
            ),
        ];

        for (name, edit, expected) in scenarios {
            let mut prompt =
                PromptBuffer::with_history("unsent draft", vec!["recalled entry".to_string()]);

            assert!(prompt.history_previous(), "{name}: recall prompt");
            assert!(edit(&mut prompt), "{name}: apply deletion");
            assert_eq!(prompt.text(), expected, "{name}: preserve edited text");
            assert!(
                !prompt.history_next(),
                "{name}: deletion must detach history navigation"
            );
            assert_eq!(prompt.text(), expected, "{name}: keep edited draft");

            assert!(prompt.history_previous(), "{name}: restart navigation");
            assert_eq!(prompt.text(), "recalled entry");
            assert!(prompt.history_next(), "{name}: restore edited draft");
            assert_eq!(prompt.text(), expected);
        }
    }

    #[test]
    fn unsuccessful_prompt_deletion_preserves_history_navigation() {
        let mut prompt =
            PromptBuffer::with_history("unsent draft", vec!["recalled entry".to_string()]);

        assert!(prompt.history_previous());
        prompt.set_cursor(0);
        assert!(!prompt.backspace());
        assert!(prompt.history_next());
        assert_eq!(prompt.text(), "unsent draft");
    }

    #[test]
    fn submission_preserves_exact_text_and_deduplicates_bounded_history() {
        let mut prompt = PromptBuffer::with_history(
            "  first\nsecond  ",
            vec!["  first\nsecond  ".to_string(), "older".to_string()],
        );

        assert_eq!(
            prompt.take_submission().as_deref(),
            Some("  first\nsecond  ")
        );
        assert_eq!(prompt.text(), "");
        assert_eq!(prompt.cursor(), 0);
        assert_eq!(prompt.mode(), Mode::Insert);
        assert_eq!(prompt.history(), ["  first\nsecond  ", "older"]);
        assert!(prompt.buffer().file.is_none());
    }

    #[test]
    fn blank_submission_never_discards_the_existing_draft() {
        let mut prompt = PromptBuffer::new(" \n\t");

        assert!(prompt.take_submission().is_none());
        assert_eq!(prompt.text(), " \n\t");
    }

    #[test]
    fn oversized_edits_never_replace_the_existing_buffer_or_history() {
        let mut prompt = PromptBuffer::new("safe draft");
        let oversized = "x".repeat(PROMPT_MAX_BYTES);

        assert!(!prompt.insert(&oversized));
        assert_eq!(prompt.text(), "safe draft");
        assert_eq!(prompt.buffer().undo_history.node_count(), 0);
        assert!(!prompt.set_text(&"x".repeat(PROMPT_MAX_BYTES + 1)));
        assert_eq!(prompt.text(), "safe draft");
    }

    #[test]
    fn control_s_is_not_a_hidden_submission_shortcut() {
        let mut prompt = PromptBuffer::new("keep editing");

        assert_eq!(
            prompt.handle_event(&key(KeyCode::Char('s'), KeyModifiers::CONTROL), 40),
            PromptInput::Unhandled
        );
        assert_eq!(prompt.text(), "keep editing");
    }
}
