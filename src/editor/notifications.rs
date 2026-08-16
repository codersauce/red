//! Editor-owned notification adapters and message-browser coordination.

use super::*;
use crate::{
    notification::{MessageAction, Notification, NotificationCounts, NotificationState},
    ui::{MessageRow, MessagesPanel, MessagesView},
};

#[derive(Clone, Copy, Default)]
enum MessageFilter {
    #[default]
    All,
    Active,
    Problems,
}

impl MessageFilter {
    fn next(self) -> Self {
        match self {
            Self::All => Self::Active,
            Self::Active => Self::Problems,
            Self::Problems => Self::All,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Active => "active",
            Self::Problems => "warnings/errors",
        }
    }
    fn includes(self, record: &Notification, now: Instant) -> bool {
        match self {
            Self::All => true,
            Self::Active => record.is_active(now),
            Self::Problems => matches!(record.severity, Severity::Warning | Severity::Error),
        }
    }
}

pub(super) struct MessageBrowser {
    return_dialog: Option<Box<dyn Component>>,
    selected: Option<NotificationId>,
    query: String,
    searching: bool,
    filter: MessageFilter,
    scroll: usize,
    feedback: Option<String>,
}

/// Last observed notification state. The default matches a new, empty center so
/// its first background poll does not invalidate otherwise reusable surfaces.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct NotificationPresentation {
    revision: u64,
    counts: NotificationCounts,
    frame: u64,
}

fn record_state(record: &Notification, now: Instant) -> &'static str {
    match record.state {
        NotificationState::Running { .. } => "running",
        NotificationState::Finished(crate::notification::ProgressOutcome::Succeeded) => "succeeded",
        NotificationState::Finished(crate::notification::ProgressOutcome::Failed) => "failed",
        NotificationState::Finished(crate::notification::ProgressOutcome::Cancelled) => "cancelled",
        NotificationState::Notice if record.is_active(now) => "active",
        NotificationState::Notice => "history",
    }
}

fn record_details(record: &Notification, now: Instant) -> String {
    let timestamp: chrono::DateTime<chrono::Local> = record.updated_at.into();
    let body = record
        .content
        .details
        .as_deref()
        .unwrap_or(&record.content.summary);
    format!(
        "{} · {} · {}\n{} · {} occurrence{}\n\n{}{}",
        record.source.label(),
        record.severity.label(),
        record_state(record, now),
        timestamp.format("%Y-%m-%d %H:%M:%S"),
        record.occurrences,
        if record.occurrences == 1 { "" } else { "s" },
        crate::notification::detail_text(body),
        if record.content.truncated {
            "\n\n[Message truncated by Red]"
        } else {
            ""
        }
    )
}

impl Editor {
    /// Transitional operation-local result plus durable notification publication.
    /// Clearing the old slot never clears notification history or active work.
    pub(super) fn set_legacy_message(&mut self, message: Option<String>) {
        self.set_notification_message(Severity::Info, message);
    }

    pub(super) fn set_notification_message(&mut self, severity: Severity, message: Option<String>) {
        if let Some(message) = message.as_ref().filter(|message| !message.is_empty()) {
            let notice =
                Notice::new(NotificationSource::Editor, severity, message).with_details(message);
            if let Err(error) = self.publish_notification(notice) {
                self.notification_fallback = Some(crate::notification::single_line(
                    truncate_chars(message, 4_096),
                ));
                log!("could not retain notification: {error}");
            } else {
                self.notification_fallback = None;
            }
        }
        self.last_error = message;
    }

    pub(super) fn notification_presentation_changed(&mut self) -> bool {
        self.notification_presentation_changed_at(Instant::now())
    }

    fn notification_presentation_changed_at(&mut self, now: Instant) -> bool {
        let mut counts = self.notifications.counts(now);
        counts.active += usize::from(self.notification_fallback.is_some());
        let frame = if !self.has_term()
            && self
                .notifications
                .primary(now)
                .is_some_and(Notification::is_running)
        {
            now.saturating_duration_since(self.notification_animation_start)
                .as_millis() as u64
                / crate::ui::SPINNER_FRAME_INTERVAL_MS
        } else {
            0
        };
        let presentation = NotificationPresentation {
            revision: self.notifications.revision(),
            counts,
            frame,
        };
        if self.notification_presentation == presentation {
            return false;
        }
        self.notification_presentation = presentation;
        if self
            .current_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.is_message_history())
        {
            self.refresh_message_browser();
        }
        true
    }

    pub(super) fn sync_persistent_notifications(&mut self) {
        let config = self.config_diagnostics_banner();
        self.sync_persistent_notification("configuration", config, true);
        let recovery = self.session_manager.warning().map(str::to_string);
        self.sync_persistent_notification("session-snapshot", recovery, false);
    }

    fn sync_persistent_notification(&mut self, key: &str, message: Option<String>, config: bool) {
        let index = usize::from(!config);
        if self.persistent_notification_messages[index] == message {
            return;
        }
        let current = if config {
            self.config_notification
        } else {
            self.session_notification
        };
        let previous_message = message.clone();
        let next = match message {
            Some(message) => {
                if let Some(id) = current {
                    let _ = self.notifications.resolve(id);
                }
                self.publish_notification(
                    Notice::new(NotificationSource::Editor, Severity::Warning, message)
                        .with_key(key),
                )
                .ok()
            }
            None => {
                if let Some(id) = current {
                    let _ = self.notifications.resolve(id);
                }
                None
            }
        };
        if next.is_some() || previous_message.is_none() {
            self.persistent_notification_messages[index] = previous_message;
        }
        if config {
            self.config_notification = next;
        } else {
            self.session_notification = next;
        }
    }

    fn message_ids(&self, now: Instant) -> Vec<NotificationId> {
        let Some(browser) = &self.message_browser else {
            return Vec::new();
        };
        let query = browser.query.to_lowercase();
        let mut records = self
            .notifications
            .records()
            .filter(|record| {
                browser.filter.includes(record, now)
                    && (query.is_empty()
                        || record.source.label().to_lowercase().contains(&query)
                        || record.severity.label().contains(&query)
                        || record.content.summary.to_lowercase().contains(&query)
                        || record
                            .content
                            .details
                            .as_ref()
                            .is_some_and(|details| details.to_lowercase().contains(&query)))
            })
            .collect::<Vec<_>>();
        records.sort_by_key(|record| std::cmp::Reverse((record.is_active(now), record.id)));
        records.into_iter().map(|record| record.id).collect()
    }

    pub(super) fn open_messages(&mut self) {
        if self.message_browser.is_none() {
            self.message_browser = Some(MessageBrowser {
                return_dialog: self.current_dialog.take(),
                selected: None,
                query: String::new(),
                searching: false,
                filter: MessageFilter::All,
                scroll: 0,
                feedback: None,
            });
        }
        self.refresh_message_browser();
    }

    pub(super) fn close_messages(&mut self) {
        if let Some(browser) = self.message_browser.take() {
            self.current_dialog = browser.return_dialog;
        }
    }

    pub(super) fn refresh_message_browser(&mut self) {
        let now = Instant::now();
        let ids = self.message_ids(now);
        let Some(browser) = &mut self.message_browser else {
            return;
        };
        let selected = browser
            .selected
            .and_then(|id| ids.iter().position(|candidate| *candidate == id))
            .unwrap_or(0);
        browser.selected = ids.get(selected).copied();
        if let Some(id) = browser.selected {
            let _ = self.notifications.mark_read(id);
        }
        let rows = ids
            .iter()
            .filter_map(|id| self.notifications.get(*id))
            .map(|record| {
                let timestamp: chrono::DateTime<chrono::Local> = record.updated_at.into();
                MessageRow {
                    summary: format!("{} {}", record.severity.marker(), record.content.summary),
                    metadata: format!(
                        "{} · {} · {}{}",
                        record.source.label(),
                        timestamp.format("%H:%M:%S"),
                        record_state(record, now),
                        if record.occurrences > 1 {
                            format!(" · ×{}", record.occurrences)
                        } else {
                            String::new()
                        }
                    ),
                }
            })
            .collect();
        let mut detail = browser
            .selected
            .and_then(|id| self.notifications.get(id))
            .map(|record| record_details(record, now))
            .unwrap_or_else(|| {
                "Messages will appear here as you work. Press f to change the filter.".to_string()
            });
        let mut counts = self.notifications.counts(now);
        if let Some(fallback) = &self.notification_fallback {
            counts.active += 1;
            detail = format!("Latest message could not be retained:\n{fallback}\n\n{detail}");
        }
        let view = MessagesView {
            rows,
            selected,
            detail,
            scroll: browser.scroll,
            query: browser.query.clone(),
            searching: browser.searching,
            filter: browser.filter.label(),
            counts,
            feedback: browser.feedback.clone().or_else(|| {
                self.notification_fallback
                    .as_ref()
                    .map(|_| "History full; latest message was not retained".to_string())
            }),
        };
        self.current_dialog = Some(Box::new(MessagesPanel::new(
            view,
            usize::from(self.size.0),
            usize::from(self.size.1.saturating_sub(2)),
            &self.theme,
        )));
    }

    pub(super) fn handle_message_action(&mut self, action: &MessageAction) {
        if matches!(action, MessageAction::Close) {
            self.close_messages();
            return;
        }
        let now = Instant::now();
        let ids = self.message_ids(now);
        let Some(browser) = &mut self.message_browser else {
            return;
        };
        browser.feedback = None;
        let selected = browser
            .selected
            .and_then(|id| ids.iter().position(|candidate| *candidate == id))
            .unwrap_or(0);
        match action {
            MessageAction::Next | MessageAction::Previous => {
                let next = if matches!(action, MessageAction::Next) {
                    selected.saturating_add(1).min(ids.len().saturating_sub(1))
                } else {
                    selected.saturating_sub(1)
                };
                browser.selected = ids.get(next).copied();
                browser.scroll = 0;
            }
            MessageAction::Search => browser.searching = true,
            MessageAction::Query(text) => {
                let text = text.replace(['\r', '\n'], " ");
                if browser.query.len().saturating_add(text.len()) <= 4_096 {
                    browser.query.push_str(&text);
                }
                browser.scroll = 0;
            }
            MessageAction::Backspace => {
                browser.query.pop();
                browser.scroll = 0;
            }
            MessageAction::DeletePreviousWord => {
                crate::unicode_utils::delete_last_word(&mut browser.query);
                browser.scroll = 0;
            }
            MessageAction::EndSearch => browser.searching = false,
            MessageAction::ClearSearch => {
                browser.query.clear();
                browser.searching = false;
                browser.scroll = 0;
            }
            MessageAction::CycleFilter => {
                browser.filter = browser.filter.next();
                browser.scroll = 0;
            }
            MessageAction::Acknowledge => {
                self.notification_fallback = None;
                if let Some(id) = browser.selected {
                    let running = self
                        .notifications
                        .get(id)
                        .is_some_and(Notification::is_running);
                    let _ = self.notifications.acknowledge(id);
                    browser.feedback = Some(
                        if running {
                            "Marked read; operation is still running"
                        } else {
                            "Acknowledged"
                        }
                        .to_string(),
                    );
                }
            }
            MessageAction::ClearInactive => {
                let removed = self.notifications.clear_inactive(now);
                browser.feedback = Some(format!("Cleared {removed} inactive messages"));
            }
            MessageAction::Copy => {
                if let Some(record) = browser.selected.and_then(|id| self.notifications.get(id)) {
                    let text = record
                        .content
                        .details
                        .as_ref()
                        .unwrap_or(&record.content.summary)
                        .clone();
                    browser.feedback = Some("Copied message".to_string());
                    self.set_default_register(Content::charwise(text));
                }
            }
            MessageAction::ScrollDown => browser.scroll = browser.scroll.saturating_add(5),
            MessageAction::ScrollUp => browser.scroll = browser.scroll.saturating_sub(5),
            MessageAction::Close => unreachable!(),
        }
        self.refresh_message_browser();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_backspace_updates_the_editor_owned_message_query() {
        for modifiers in [KeyModifiers::ALT, KeyModifiers::CONTROL] {
            let mut editor = editor(100, 24);
            editor.open_messages();
            editor.handle_message_action(&MessageAction::Search);
            editor.handle_message_action(&MessageAction::Query("one 👨‍👩‍👧e\u{301}".into()));
            let event = Event::Key(KeyEvent::new(KeyCode::Backspace, modifiers));
            let Some(KeyAction::Single(Action::MessageHistory(action))) =
                editor.current_dialog.as_mut().unwrap().handle_event(&event)
            else {
                panic!("message search did not handle word backspace");
            };
            editor.handle_message_action(&action);
            let browser = editor.message_browser.as_ref().unwrap();
            assert_eq!(browser.query, "one ");
            assert!(browser.searching);
            assert_eq!(browser.scroll, 0);
        }
    }

    fn editor(width: usize, height: usize) -> Editor {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            lsp,
            width,
            height,
            config,
            Theme::default(),
            vec![Buffer::new(None, "hello".to_string())],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        editor
    }

    #[test]
    fn presentation_ignores_an_empty_center_but_detects_arrivals_and_expiry() {
        let mut editor = editor(90, 12);
        assert!(!editor.notification_presentation_changed());
        assert!(!editor.notification_presentation_changed());

        let id = editor
            .publish_notification(Notice::new(
                NotificationSource::Editor,
                Severity::Error,
                "save failed",
            ))
            .unwrap();
        assert!(editor.notification_presentation_changed());
        assert!(!editor.notification_presentation_changed());
        editor.notifications.acknowledge(id).unwrap();
        assert!(editor.notification_presentation_changed());
        assert!(!editor.notification_presentation_changed());

        let now = NotificationTime::now();
        editor
            .notifications
            .publish(
                Notice::new(NotificationSource::Editor, Severity::Info, "saved"),
                now,
            )
            .unwrap();
        assert!(editor.notification_presentation_changed_at(now.monotonic));
        let after_expiry = now.monotonic + Duration::from_secs(4);
        assert!(editor.notification_presentation_changed_at(after_expiry));
        assert!(!editor.notification_presentation_changed_at(after_expiry));
    }

    #[tokio::test]
    async fn bottom_line_keeps_multiple_messages_after_unrelated_actions() {
        let mut editor = editor(90, 12);
        let mut buffer = RenderBuffer::new(90, 12, &Style::default());
        let mut runtime = Runtime::new();
        for message in ["first message", "second message"] {
            editor
                .execute(
                    &Action::Print(message.to_string()),
                    &mut buffer,
                    &mut runtime,
                )
                .await
                .unwrap();
        }
        editor
            .execute(&Action::Refresh, &mut buffer, &mut runtime)
            .await
            .unwrap();
        let row = render_text_rows(&buffer).pop().unwrap();
        assert!(row.contains("second message"), "{row}");
        assert!(row.contains("2 active"), "{row}");
        assert!(editor.last_error.is_none());

        editor.mode = Mode::Command;
        editor.command = "write pending.txt".to_string();
        editor.draw_commandline(&mut buffer);
        let row = render_text_rows(&buffer).pop().unwrap();
        assert!(row.starts_with(":write pending.txt"));
        assert!(row.contains("2 active"));
    }

    #[test]
    fn bottom_line_prioritizes_errors_and_reports_concurrent_progress() {
        use crate::notification::{MessageContent, ProgressPriority};
        let mut editor = editor(100, 12);
        let now = NotificationTime::now();
        editor
            .notifications
            .begin_progress(
                NotificationSource::Editor,
                "index",
                MessageContent::new("indexing"),
                ProgressPriority::Background,
                now,
            )
            .unwrap();
        let push = editor
            .notifications
            .begin_progress(
                NotificationSource::Editor,
                "push",
                MessageContent::new("pushing commits"),
                ProgressPriority::UserInitiated,
                now,
            )
            .unwrap();
        editor
            .notifications
            .update_progress(push, MessageContent::new("pushing commits"), Some(40), now)
            .unwrap();
        let error = editor
            .publish_notification(Notice::new(
                NotificationSource::Editor,
                Severity::Error,
                "save failed",
            ))
            .unwrap();
        let mut buffer = RenderBuffer::new(100, 12, &Style::default());
        editor.draw_commandline(&mut buffer);
        let row = render_text_rows(&buffer).pop().unwrap();
        assert!(
            row.contains("save failed") && row.contains("3 active"),
            "{row}"
        );
        editor.notifications.acknowledge(error).unwrap();
        editor.draw_commandline(&mut buffer);
        let row = render_text_rows(&buffer).pop().unwrap();
        assert!(
            row.contains("pushing commits (40%)") && row.contains("2 active"),
            "{row}"
        );
    }

    #[test]
    fn ordinary_command_errors_are_retained_and_messages_is_discoverable() {
        let mut editor = editor(100, 24);
        let runtime = Runtime::new();
        assert!(editor
            .handle_command("DefinitelyNotACommand", &runtime)
            .is_empty());
        assert_eq!(
            editor.notifications.records().next().unwrap().severity,
            Severity::Error
        );
        assert_eq!(
            editor.handle_command("messages", &runtime),
            vec![Action::OpenMessages]
        );
        let entries = command_palette::entries(&editor.config.keys, &[]);
        assert!(entries
            .iter()
            .any(|entry| entry.action == Action::OpenMessages));
        let defaults: Config = toml::from_str(crate::assets::DEFAULT_CONFIG).unwrap();
        let Some(KeyAction::Nested(leader)) = defaults.keys.normal.get(" ") else {
            panic!("missing leader");
        };
        assert_eq!(
            leader.get("m"),
            Some(&KeyAction::Single(Action::OpenMessages))
        );
    }

    #[test]
    fn browser_searches_details_and_keeps_selection_when_messages_arrive() {
        let mut editor = editor(100, 24);
        let first = editor
            .publish_notification(
                Notice::new(NotificationSource::Editor, Severity::Error, "first")
                    .with_details("hidden diagnostic needle"),
            )
            .unwrap();
        editor.open_messages();
        assert_eq!(
            editor.message_browser.as_ref().unwrap().selected,
            Some(first)
        );
        editor
            .publish_notification(Notice::new(
                NotificationSource::Editor,
                Severity::Warning,
                "newer",
            ))
            .unwrap();
        editor.notification_presentation_changed();
        assert_eq!(
            editor.message_browser.as_ref().unwrap().selected,
            Some(first)
        );
        editor.handle_message_action(&MessageAction::Search);
        editor.handle_message_action(&MessageAction::Query("needle".to_string()));
        assert_eq!(editor.message_ids(Instant::now()), vec![first]);
        editor.handle_message_action(&MessageAction::EndSearch);
        editor.handle_message_action(&MessageAction::Acknowledge);
        assert!(!editor
            .notifications
            .get(first)
            .unwrap()
            .is_active(Instant::now()));
        editor.handle_message_action(&MessageAction::ClearInactive);
        assert!(editor.notifications.get(first).is_none());
    }

    struct RetainedDraft;
    impl Component for RetainedDraft {
        fn draw(&self, _buffer: &mut RenderBuffer) -> anyhow::Result<()> {
            Ok(())
        }
        fn is_sensitive_input(&self) -> bool {
            true
        }
        fn cursor_position(&self) -> Option<(usize, usize)> {
            Some((7, 3))
        }
    }

    #[test]
    fn browser_restores_the_exact_suspended_input_surface() {
        let mut editor = editor(100, 24);
        editor.current_dialog = Some(Box::new(RetainedDraft));
        editor.open_messages();
        assert!(editor.current_dialog.as_ref().unwrap().is_message_history());
        editor.close_messages();
        let restored = editor.current_dialog.as_ref().unwrap();
        assert!(restored.is_sensitive_input());
        assert_eq!(restored.cursor_position(), Some((7, 3)));
    }

    #[test]
    fn acknowledged_persistent_warning_is_not_republished_when_history_is_cleared() {
        let mut editor = editor(100, 24);
        editor.session_manager.set_warning(Some("snapshot failed"));
        editor.sync_persistent_notifications();
        let id = editor.session_notification.unwrap();
        editor.notifications.acknowledge(id).unwrap();
        editor.notifications.clear_inactive(Instant::now());
        editor.sync_persistent_notifications();
        assert_eq!(editor.notifications.records().len(), 0);
        editor.session_manager.set_warning(None);
        editor.sync_persistent_notifications();
        editor.session_manager.set_warning(Some("snapshot failed"));
        editor.sync_persistent_notifications();
        assert_ne!(editor.session_notification, Some(id));
        assert_eq!(editor.notifications.counts(Instant::now()).active, 1);
    }

    #[test]
    fn browser_renders_at_narrow_and_tiny_sizes_without_using_global_chrome() {
        for (width, height) in [(120, 30), (60, 16), (24, 8), (8, 4), (1, 1)] {
            let mut editor = editor(width, height);
            editor.set_legacy_message(Some("A long message with 界 and several words".to_string()));
            editor.open_messages();
            let mut buffer = RenderBuffer::new(width, height, &Style::default());
            editor
                .current_dialog
                .as_ref()
                .unwrap()
                .draw(&mut buffer)
                .unwrap();
            for row in render_text_rows(&buffer)
                .iter()
                .skip(height.saturating_sub(2))
            {
                assert!(row.trim().is_empty(), "{width}x{height}: {row}");
            }
        }
    }

    #[test]
    fn bottom_line_click_opens_history_over_a_workspace() {
        let mut editor = editor(100, 24);
        editor.set_legacy_message(Some("message".to_string()));
        editor
            .workspace_manager
            .open("git".to_string(), plugin::WorkspaceConfig::default());
        let action = editor
            .handle_event(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 80,
                row: 23,
                modifiers: KeyModifiers::NONE,
            }))
            .unwrap();
        assert_eq!(action, Some(KeyAction::Single(Action::OpenMessages)));
    }
}
