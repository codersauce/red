//! Transient, actionable completion notices backed by retained request IDs.

use super::*;
use crate::unicode_utils::{
    truncate_display_width, truncate_display_width_with_marker, TruncationSide,
};

#[cfg(test)]
mod tests;

const NOTICE_DURATION: Duration = Duration::from_secs(12);

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompletionNotice {
    request_id: String,
    /// Start the lifetime only once the message row can actually show it.
    expires_at: Option<Instant>,
}

#[derive(Debug, Default)]
pub(super) struct InlineCompletionState {
    latest: Option<String>,
    notice: Option<CompletionNotice>,
    hit: Option<(std::ops::Range<usize>, String)>,
}

impl InlineCompletionState {
    pub(super) fn clear_hit(&mut self) {
        self.hit = None;
    }
}

impl Editor {
    pub(super) fn inline_request_is_foreground(&self, request: &str) -> bool {
        self.inline_assist
            .as_ref()
            .is_some_and(|assist| assist.request_id.as_deref() == Some(request))
            && self
                .current_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.inline_assist_state().is_some())
    }

    pub(super) fn notify_inline_completion(&mut self, request: &str) {
        if self.inline_request_is_foreground(request)
            || !self.inline_history.turn(request).is_some_and(|turn| {
                !matches!(
                    turn.state,
                    InlineTurnState::Pending | InlineTurnState::Cancelled
                )
            })
        {
            return;
        }
        self.inline_completion.latest = Some(request.to_owned());
        self.inline_completion.notice = Some(CompletionNotice {
            request_id: request.to_owned(),
            expires_at: None,
        });
        self.inline_completion.clear_hit();
    }

    pub(super) fn poll_inline_completion_notice(&mut self, now: Instant) -> bool {
        if self
            .inline_completion
            .notice
            .as_ref()
            .and_then(|notice| notice.expires_at)
            .is_some_and(|until| now >= until)
        {
            self.inline_completion.notice = None;
            return self.inline_completion.hit.take().is_some();
        }
        false
    }

    fn inline_completion_surface_available(&self) -> bool {
        !self.has_term()
            && !self.workspace_manager.is_active()
            && self.current_dialog.as_ref().is_none_or(|dialog| {
                dialog.is_inline_history() || dialog.inline_assist_state().is_some()
            })
    }

    /// Paint the location as a bounded link; errors and command input own the row first.
    pub(super) fn draw_inline_completion_notice(
        &mut self,
        buffer: &mut RenderBuffer,
        width: usize,
        y: usize,
    ) {
        if !self.inline_completion_surface_available() || width == 0 {
            return;
        }
        let Some(notice) = &self.inline_completion.notice else {
            return;
        };
        if notice
            .expires_at
            .is_some_and(|until| Instant::now() >= until)
        {
            return;
        }
        let request = &notice.request_id;
        let Some((conversation, turn)) =
            self.inline_history
                .conversations
                .iter()
                .find_map(|conversation| {
                    conversation
                        .turns
                        .iter()
                        .find(|turn| &turn.request_id == request)
                        .map(|turn| (conversation, turn))
                })
        else {
            return;
        };
        let status = match turn.state {
            InlineTurnState::Ready
                if turn
                    .result
                    .as_ref()
                    .is_some_and(|result| result.changes_text(&turn.before)) =>
            {
                "Inline edit ready"
            }
            InlineTurnState::Ready => "Inline answer retained",
            InlineTurnState::Completed
                if turn
                    .result
                    .as_ref()
                    .is_some_and(|result| result.needs_agent.is_some()) =>
            {
                "Inline needs Agent"
            }
            InlineTurnState::Completed => "Inline finished",
            InlineTurnState::Failed | InlineTurnState::Rejected => "Inline failed",
            InlineTurnState::Pending | InlineTurnState::Cancelled => return,
        };
        let path = Path::new(&turn.location.file);
        let file = if turn.location.file.is_empty() {
            "[No Name]".into()
        } else {
            path.strip_prefix(&conversation.cwd)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned()
        };
        let start = turn.location.range.start.line + 1;
        let end = (turn.location.range.end.line
            + usize::from(turn.location.range.end.character > 0))
        .max(start);
        let location = if start == end {
            format!("{file}:{start}")
        } else {
            format!("{file}:{start}–{end}")
        };
        let location = location
            .chars()
            .map(|ch| if ch.is_control() { ' ' } else { ch })
            .collect::<String>();
        let hint = if width >= 48 { " · Space N" } else { "" };
        let prefix = if width >= 48 {
            format!("{status} · ")
        } else {
            String::new()
        };
        let available = width.saturating_sub(display_width(&prefix) + display_width(hint));
        let location = truncate_display_width_with_marker(
            &location,
            available.saturating_sub(2),
            "…",
            TruncationSide::Left,
        );
        let link = truncate_display_width(&format!("[{location}]"), available);
        let x = display_width(&prefix);
        let link_width = display_width(&link);
        let style = &self.theme.style;
        let mut link_style = self
            .theme
            .get_style("markup.underline.link.markdown")
            .unwrap_or_else(|| style.clone());
        link_style.bg = style.bg;
        link_style.bold = true;
        buffer.set_text(0, y, &prefix, style);
        buffer.set_text(x, y, &link, &link_style);
        buffer.set_text(x + link_width, y, hint, style);
        if link_width > 0 {
            self.inline_completion.hit = Some((x..x + link_width, request.clone()));
            if let Some(notice) = &mut self.inline_completion.notice {
                notice
                    .expires_at
                    .get_or_insert_with(|| Instant::now() + NOTICE_DURATION);
            }
        }
    }

    pub(super) fn inline_completion_click(&self, event: &Event) -> Option<KeyAction> {
        if !self.inline_completion_surface_available()
            || self.last_error.is_some()
            || self.session_manager.warning().is_some()
            || self.config_diagnostics_banner().is_some()
        {
            return None;
        }
        let Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            ..
        }) = event
        else {
            return None;
        };
        let (columns, request) = self.inline_completion.hit.as_ref()?;
        (usize::from(*row) == usize::from(self.size.1).saturating_sub(1)
            && columns.contains(&usize::from(*column))
            && self
                .inline_completion
                .notice
                .as_ref()
                .is_some_and(|notice| {
                    &notice.request_id == request
                        && notice
                            .expires_at
                            .is_some_and(|until| Instant::now() < until)
                }))
        .then(|| KeyAction::Single(Action::OpenInlineCompletion(request.clone())))
    }

    pub(super) async fn open_latest_inline_completion(
        &mut self,
        frame: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        if let Some(request) = self.inline_completion.latest.clone() {
            self.open_inline_completion(&request, frame, runtime).await
        } else {
            self.last_error =
                Some("no background inline completion yet · Space H opens history".into());
            self.render(frame)
        }
    }

    pub(super) async fn open_inline_completion(
        &mut self,
        request: &str,
        frame: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let target = self
            .inline_history
            .conversations
            .iter()
            .find_map(|conversation| {
                conversation
                    .turns
                    .iter()
                    .any(|turn| turn.request_id == request)
                    .then(|| {
                        (
                            conversation.id.clone(),
                            conversation
                                .turns
                                .last()
                                .is_some_and(|turn| turn.request_id == request),
                        )
                    })
            });
        let Some((group, latest)) = target else {
            self.last_error =
                Some("inline result is no longer available · Space H opens history".into());
            return self.render(frame);
        };
        if self
            .inline_completion
            .notice
            .as_ref()
            .is_some_and(|notice| notice.request_id == request)
        {
            self.inline_completion.notice = None;
            self.inline_completion.clear_hit();
        }
        self.waiting_key_action = None;
        self.waiting_command = None;
        self.clear_keymap_hints();
        self.repeater = None;
        self.park_inline_assist();
        if !self.is_normal() {
            self.execute_with_tracking(&Action::EnterMode(Mode::Normal), frame, runtime, false)
                .await?;
        }
        self.panel_manager.focus_editor();
        if latest {
            let origin = self.current_jump_entry();
            self.open_inline_job(&group, frame, runtime).await?;
            self.save_to_history(origin);
            Ok(())
        } else {
            self.open_inline_history_request(&group, request, frame, runtime)
                .await
        }
    }
}
