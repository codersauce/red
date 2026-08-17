//! Fork a specific annotation into a new, draft-safe discussion.

use super::inline_comments::InlineCommentOrigin;
use super::*;
use crate::inline_history::{
    comment_context::{bounded, MAX_DISCUSSION_BYTES, MAX_SOURCE_BYTES},
    InlineCommentContext, InlineSourceState,
};

#[cfg(test)]
mod tests;

impl Editor {
    pub(super) fn selected_comment_context(
        &self,
        id: uuid::Uuid,
    ) -> anyhow::Result<(Box<InlineCommentContext>, Option<TextRange>)> {
        let comment = self
            .inline_comments
            .iter()
            .find(|comment| comment.id == id)
            .ok_or_else(|| anyhow::anyhow!("inline comment is no longer available"))?;
        let (request, comment_index) = match &comment.origin {
            InlineCommentOrigin::Assist {
                request_id,
                comment_index,
                ..
            }
            | InlineCommentOrigin::HistoryPreview {
                request_id,
                comment_index,
            } => (request_id, *comment_index),
            _ => anyhow::bail!("this item is not a retained inline comment"),
        };
        let conversation = self
            .inline_history
            .conversations
            .iter()
            .find(|conversation| {
                conversation
                    .turns
                    .iter()
                    .any(|turn| &turn.request_id == request)
            })
            .ok_or_else(|| anyhow::anyhow!("inline discussion is no longer available"))?;
        anyhow::ensure!(
            same_file_path(Path::new(&conversation.cwd), &get_workspace_path()),
            "open the comment's workspace before starting a follow-up"
        );
        let turn_index = conversation
            .turns
            .iter()
            .position(|turn| &turn.request_id == request)
            .expect("conversation contains selected request");
        let turn = &conversation.turns[turn_index];
        let location = turn
            .comment_locations
            .get(comment_index)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("comment source location is no longer available"))?;
        let source = turn
            .comment_source_ids
            .get(comment_index)
            .and_then(Option::as_ref)
            .and_then(|id| self.inline_history.sources.get(id))
            .map_or("", String::as_str);
        let resolved = self.resolve_history_comment(turn, comment_index);
        let range = resolved
            .filter(|(index, _, state)| {
                *index == self.buffer_manager.active_index()
                    && *state != InlineSourceState::Detached
            })
            .map(|(_, range, _)| range);
        let outdated = comment.stale
            || comment.detached
            || resolved.is_none_or(|(_, _, state)| state != InlineSourceState::Unchanged);
        // Stop at the selected turn, so newer sibling requests cannot change what
        // “this comment” means. Keep the immediate parent even for long answers.
        let selected = bounded(
            &format!("You: {}\nAssistant: {}", turn.prompt, turn.answer_text()),
            MAX_DISCUSSION_BYTES * 3 / 4,
        );
        let mut remaining = MAX_DISCUSSION_BYTES.saturating_sub(selected.len());
        let mut earlier = Vec::new();
        for previous in conversation.turns[..turn_index]
            .iter()
            .rev()
            .filter(|turn| turn.state != InlineTurnState::Pending)
            .take(3)
        {
            let text = bounded(
                &format!(
                    "Earlier user: {}\nEarlier assistant: {}\n\n",
                    previous.prompt,
                    previous.answer_text()
                ),
                remaining,
            );
            remaining = remaining.saturating_sub(text.len());
            earlier.push(text);
        }
        earlier.reverse();
        let mut discussion = earlier.concat();
        discussion.push_str(&selected);
        if let Some(parent) = &turn.parent_comment {
            let previous = format!("\nEarlier selected comment: {}", parent.message);
            discussion.push_str(&bounded(
                &previous,
                MAX_DISCUSSION_BYTES.saturating_sub(discussion.len()),
            ));
        }
        let context = Box::new(InlineCommentContext {
            cwd: conversation.cwd.clone(),
            request_id: request.clone(),
            comment_index,
            location,
            message: comment.message.clone(),
            source: bounded(source, MAX_SOURCE_BYTES),
            source_truncated: source.len() > MAX_SOURCE_BYTES,
            discussion,
            outdated,
        });
        context.validate()?;
        Ok((context, range))
    }

    pub(crate) fn inline_comment_context_label(&self) -> Option<String> {
        self.inline_assist
            .as_ref()?
            .parent_comment
            .as_ref()
            .map(|context| {
                format!(
                    "About{}: {}",
                    if context.outdated {
                        " outdated comment"
                    } else {
                        " comment"
                    },
                    crate::ui::first_prompt_line(&context.message)
                )
            })
    }

    pub(super) async fn ask_inline_comment(
        &mut self,
        id: uuid::Uuid,
        in_agent: bool,
        frame: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let (context, range) = match self.selected_comment_context(id) {
            Ok(selected) => selected,
            Err(error) => {
                self.set_legacy_message(Some(error.to_string()));
                return self.render(frame);
            }
        };
        if in_agent {
            let request_id = uuid::Uuid::new_v4().to_string();
            let prompt = format!("Follow up on inline comment: {}\nRead the current project files before making claims or edits.\n\n{}\n\n{}\n\nMy question: ",
                crate::ui::first_prompt_line(&context.message),
                super::inline_agent_outcomes::handoff_marker(&request_id), context.agent_context());
            return self
                .queue_inline_handoff(Some(request_id), Some(context), prompt, frame, runtime)
                .await;
        }
        let Some(range) = range else {
            self.current_dialog = Some(Box::new(HoverInfo::new(self,
                "This comment's source is detached. Start in Agent to locate and verify the current code; no editable range has been guessed.".into(),
                HoverInfoFormat::Plaintext, Vec::new())
                .with_label("Comment source detached")
                .with_shortcut('A', "ask Agent", Action::AskInlineComment { id, in_agent: true })));
            return self.render(frame);
        };
        // Use the ordinary inline context gate before creating even a draft.
        if let Err(error) = self.inline_assist_context(range) {
            self.set_legacy_message(Some(error.to_string()));
            return self.render(frame);
        }
        let Some(window_id) = self.window_manager.active_stable_window_id() else {
            return Ok(());
        };
        let scope = format!(
            "comment · lines {}–{}{}",
            range.start.line + 1,
            range.end.line + usize::from(range.end.character > 0),
            if context.outdated {
                " · source changed"
            } else {
                ""
            }
        );
        self.close_inline_history(true, frame, runtime).await?;
        self.park_inline_assist();
        self.inline_assist = Some(InlineAssistSession {
            parent_comment: Some(context),
            allow_expansion: false,
            buffer_id: self.current_buffer().id(),
            window_id,
            expected_revision: self.current_buffer().revision(),
            range,
            expected_text: self.current_buffer().text_in_range(range),
            scope: scope.clone(),
            request_id: None,
            session_id: None,
            transaction_id: None,
            annotation_group_id: uuid::Uuid::new_v4().to_string(),
            has_result: false,
            result_request_id: None,
        });
        self.current_dialog = Some(Box::new(self.inline_assist_popup(
            scope,
            InlineAssistPopupState::Prompt {
                initial: String::new(),
                refining: false,
            },
        )));
        self.render(frame)
    }

    pub(super) async fn queue_inline_handoff(
        &mut self,
        request_id: Option<String>,
        comment_followup: Option<Box<InlineCommentContext>>,
        prompt: String,
        frame: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        self.plugin_registry
            .ensure_command_registered(runtime, "AgentOpen")
            .await;
        if runtime.command_plugin("AgentOpen").is_none() {
            self.set_legacy_message(Some(
                "Agent is unavailable; the inline discussion remains in history".into(),
            ));
            return self.render(frame);
        }
        self.close_inline_history(true, frame, runtime).await?;
        self.park_inline_assist();
        self.current_dialog = None;
        if !self.is_normal() {
            self.execute_with_tracking(&Action::EnterMode(Mode::Normal), frame, runtime, false)
                .await?;
        }
        self.plugin_registry.execute(runtime, "AgentOpen").await?;
        runtime.send_request(PluginRequest::Action(Action::StageInlineAssistHandoff {
            request_id,
            comment_followup,
            prompt,
            expected_draft: None,
        }));
        Ok(())
    }
}
