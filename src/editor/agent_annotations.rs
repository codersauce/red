//! Agent-owned source annotations backed by the shared inline-comment UI.

#[cfg(test)]
mod tests;

use std::collections::HashSet;

use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    inline_comments::{InlineCommentOrigin, MAX_BUFFER_COMMENTS},
    Editor, RenderBuffer,
};
use crate::{
    agent_conversation::{AgentAnnotationRecord, MAX_AGENT_ANNOTATIONS},
    agent_tools::{EditorAnnotationInput, MAX_AGENT_ANNOTATIONS_PER_CALL},
    inline_assist::MAX_COMMENT_BYTES,
    plugin::Runtime,
    undo::TextPosition,
};

impl Editor {
    pub(super) async fn open_linked_agent_annotation(
        &mut self,
        id: Uuid,
        frame: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let target_buffer = self
            .inline_comments
            .iter()
            .find(|comment| {
                comment.id == id
                    && matches!(comment.origin, InlineCommentOrigin::AgentAnnotation { .. })
                    && self.inline_comment_visible(comment)
            })
            .map(|comment| comment.anchor.buffer_id);
        let Some(target_buffer) = target_buffer else {
            self.set_quiet_message(Some("Annotation is no longer available".into()));
            return self.render(frame);
        };
        let Some(index) = self
            .buffer_manager
            .iter()
            .position(|buffer| buffer.id() == target_buffer)
        else {
            self.set_quiet_message(Some("Annotation is no longer available".into()));
            return self.render(frame);
        };
        self.panel_manager.focus_editor();
        if index != self.buffer_manager.active_index() {
            self.set_current_buffer(frame, index).await?;
        }
        self.open_inline_comment_by_id(id, frame, runtime).await
    }

    pub(super) fn agent_annotation_state(&self) -> Value {
        let buffer = self.current_buffer();
        let visible = self
            .inline_comments
            .iter()
            .filter(|comment| {
                comment.anchor.buffer_id == buffer.id() && self.inline_comment_visible(comment)
            })
            .collect::<Vec<_>>();
        let current = self.current_inline_comment_id().and_then(|id| {
            visible
                .iter()
                .find(|comment| comment.id == id)
                .map(|comment| {
                    let (start_line, end_line) = comment.lines(buffer);
                    let (kind, session_id, turn_id) = match &comment.origin {
                        InlineCommentOrigin::Sample => ("sample", None, None),
                        InlineCommentOrigin::Activity { .. } => ("activity", None, None),
                        InlineCommentOrigin::ChangeSummary { .. } => {
                            ("inline_change_summary", None, None)
                        }
                        InlineCommentOrigin::AgentOutcome { .. } => {
                            ("agent_change_summary", None, None)
                        }
                        InlineCommentOrigin::AgentAnnotation {
                            session_id,
                            turn_id,
                        } => ("agent", Some(session_id.as_str()), Some(turn_id.as_str())),
                        InlineCommentOrigin::HistoryPreview { .. } => {
                            ("history_preview", None, None)
                        }
                        InlineCommentOrigin::Assist { session_id, .. } => {
                            ("inline_assist", Some(session_id.as_str()), None)
                        }
                    };
                    json!({
                        "id": comment.id.to_string(),
                        "start_line": start_line,
                        "end_line": end_line,
                        "message": comment.message,
                        "kind": kind,
                        "session_id": session_id,
                        "turn_id": turn_id,
                        "stale": comment.stale,
                    })
                })
        });
        json!({
            "visible_count": visible.len(),
            "current": current,
        })
    }

    pub(super) fn add_agent_annotations(
        &mut self,
        session_id: &str,
        turn_id: &str,
        path: &str,
        expected_revision: u64,
        annotations: Vec<EditorAnnotationInput>,
    ) -> anyhow::Result<Value> {
        anyhow::ensure!(
            !annotations.is_empty() && annotations.len() <= MAX_AGENT_ANNOTATIONS_PER_CALL,
            "Agent annotation call must contain 1–{MAX_AGENT_ANNOTATIONS_PER_CALL} comments"
        );
        anyhow::ensure!(
            self.current_buffer().revision() == expected_revision,
            "stale editor revision: expected {expected_revision}, current {}",
            self.current_buffer().revision()
        );
        let retained = self
            .inline_comments
            .iter()
            .filter(|comment| matches!(comment.origin, InlineCommentOrigin::AgentAnnotation { .. }))
            .count();
        anyhow::ensure!(
            retained.saturating_add(annotations.len()) <= MAX_AGENT_ANNOTATIONS,
            "Agent annotation limit reached; dismiss existing annotations first"
        );
        let buffer_count = self
            .inline_comments
            .iter()
            .filter(|comment| comment.anchor.buffer_id == self.current_buffer().id())
            .filter(|comment| {
                !matches!(
                    comment.origin,
                    InlineCommentOrigin::Activity { .. }
                        | InlineCommentOrigin::ChangeSummary { .. }
                        | InlineCommentOrigin::AgentOutcome { .. }
                        | InlineCommentOrigin::HistoryPreview { .. }
                )
            })
            .count();
        anyhow::ensure!(
            buffer_count.saturating_add(annotations.len()) <= MAX_BUFFER_COMMENTS,
            "inline comment limit reached; dismiss existing comments first"
        );
        let last_line = self
            .current_buffer()
            .navigable_line_count()
            .saturating_sub(1);
        let mut normalized = Vec::with_capacity(annotations.len());
        for annotation in annotations {
            let message = annotation
                .message
                .replace("\r\n", "\n")
                .replace('\t', "    ")
                .trim()
                .to_string();
            anyhow::ensure!(
                annotation.start_line <= annotation.last_line()
                    && annotation.last_line() <= last_line,
                "Agent annotation range is outside the file"
            );
            anyhow::ensure!(!message.is_empty(), "Agent annotation cannot be empty");
            anyhow::ensure!(
                message.len() <= MAX_COMMENT_BYTES,
                "Agent annotation exceeds {MAX_COMMENT_BYTES} bytes"
            );
            anyhow::ensure!(
                !message.chars().any(|ch| {
                    (ch.is_control() && ch != '\n')
                        || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                }),
                "Agent annotation contains control characters"
            );
            normalized.push((annotation.start_line, annotation.last_line(), message));
        }

        let mut added = Vec::with_capacity(normalized.len());
        for (start_line, end_line, message) in normalized {
            let comment = self.make_inline_comment(
                start_line,
                end_line,
                message,
                InlineCommentOrigin::AgentAnnotation {
                    session_id: session_id.to_string(),
                    turn_id: turn_id.to_string(),
                },
            );
            added.push(json!({
                "id": comment.id.to_string(),
                "href": crate::plugin::annotation_link_destination(comment.id),
                "start_line": start_line,
                "end_line": end_line,
                "message": comment.message,
            }));
            self.inline_comments.push(comment);
        }
        if let Some(first) = added.first() {
            let id = Uuid::parse_str(first["id"].as_str().unwrap_or_default())?;
            self.set_active_inline_comment(Some(id));
            let line = first["start_line"].as_u64().unwrap_or_default() as usize;
            self.move_to_text_position(TextPosition::new(line, 0));
            self.refresh_cursor_goal();
        }
        self.layout_cache.borrow_mut().clear();
        Ok(json!({
            "ok": true,
            "path": path,
            "revision": self.current_buffer().revision(),
            "dirty": self.current_buffer().is_dirty(),
            "annotations": added,
        }))
    }

    pub(super) async fn dismiss_agent_requested_annotations(
        &mut self,
        root: &std::path::Path,
        annotation_ids: Vec<String>,
        frame: &mut RenderBuffer,
    ) -> anyhow::Result<Value> {
        anyhow::ensure!(
            !annotation_ids.is_empty() && annotation_ids.len() <= MAX_AGENT_ANNOTATIONS_PER_CALL,
            "dismiss call must contain 1–{MAX_AGENT_ANNOTATIONS_PER_CALL} annotation IDs"
        );
        let mut unique = HashSet::new();
        let ids = annotation_ids
            .into_iter()
            .map(|id| {
                let parsed = Uuid::parse_str(&id)
                    .map_err(|_| anyhow::anyhow!("invalid annotation ID: {id}"))?;
                anyhow::ensure!(unique.insert(parsed), "duplicate annotation ID: {id}");
                Ok(parsed)
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let targets = ids
            .iter()
            .map(|id| {
                let comment = self
                    .inline_comments
                    .iter()
                    .find(|comment| comment.id == *id && self.inline_comment_visible(comment))
                    .ok_or_else(|| anyhow::anyhow!("annotation is not visible: {id}"))?;
                anyhow::ensure!(
                    !matches!(comment.origin, InlineCommentOrigin::Activity { .. }),
                    "running activity annotations cannot be dismissed"
                );
                let path = self
                    .buffer_manager
                    .iter()
                    .find(|buffer| buffer.id() == comment.anchor.buffer_id)
                    .and_then(|buffer| buffer.file.as_deref())
                    .ok_or_else(|| {
                        anyhow::anyhow!("annotation buffer has no workspace file: {id}")
                    })?;
                let path = super::resolve_agent_tool_path(root, path)?;
                Ok((*id, path))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        for (_, path) in &targets {
            anyhow::ensure!(
                self.open_agent_buffer(path, /*create*/ false, frame)
                    .await?
                    .is_some(),
                "annotation file no longer exists: {}",
                path.display()
            );
        }
        for (id, path) in &targets {
            self.open_agent_buffer(path, /*create*/ false, frame)
                .await?;
            anyhow::ensure!(
                self.select_inline_comment_by_id(*id),
                "annotation is no longer visible: {id}"
            );
            self.dismiss_inline_comment();
        }
        self.render(frame)?;
        Ok(json!({
            "ok": true,
            "dismissed": ids.iter().map(Uuid::to_string).collect::<Vec<_>>(),
            "annotations": self.agent_annotation_state(),
        }))
    }

    pub(super) fn sync_agent_annotation_records(&mut self) {
        let annotations = self
            .inline_comments
            .iter()
            .filter_map(|comment| {
                let InlineCommentOrigin::AgentAnnotation {
                    session_id,
                    turn_id,
                } = &comment.origin
                else {
                    return None;
                };
                let buffer = self
                    .buffer_manager
                    .iter()
                    .find(|buffer| buffer.id() == comment.anchor.buffer_id)?;
                let path = buffer.file.clone()?;
                let (start_line, end_line) = comment.lines(buffer);
                Some(AgentAnnotationRecord {
                    id: comment.id.to_string(),
                    session_id: session_id.clone(),
                    turn_id: turn_id.clone(),
                    path,
                    start_line,
                    end_line,
                    message: comment.message.clone(),
                    expected_fingerprint: comment.expected_fingerprint(),
                })
            })
            .take(MAX_AGENT_ANNOTATIONS)
            .collect();
        self.agent_manager.replace_annotation_records(annotations);
    }

    pub(super) fn restore_agent_annotations(&mut self) {
        let Some(conversation) = self.agent_manager.conversation_snapshot() else {
            return;
        };
        self.inline_comments.retain(|comment| {
            !matches!(comment.origin, InlineCommentOrigin::AgentAnnotation { .. })
        });
        for record in conversation
            .annotations
            .into_iter()
            .take(MAX_AGENT_ANNOTATIONS)
        {
            let Ok(id) = Uuid::parse_str(&record.id) else {
                continue;
            };
            if record.message.is_empty()
                || record.message.len() > MAX_COMMENT_BYTES
                || record.message.chars().any(|ch| {
                    (ch.is_control() && ch != '\n')
                        || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                })
            {
                continue;
            }
            let Ok(resolved_path) = super::resolve_agent_tool_path(
                std::path::Path::new(&conversation.cwd),
                &record.path,
            ) else {
                continue;
            };
            let Some(buffer) = self.buffer_manager.iter().find(|buffer| {
                buffer.file.as_deref().is_some_and(|buffer_path| {
                    super::same_file_path(std::path::Path::new(buffer_path), &resolved_path)
                })
            }) else {
                continue;
            };
            if record.start_line > record.end_line
                || record.end_line >= buffer.navigable_line_count()
            {
                continue;
            }
            let mut comment = Self::make_inline_comment_in_buffer(
                buffer,
                record.start_line,
                record.end_line,
                record.message,
                InlineCommentOrigin::AgentAnnotation {
                    session_id: if record.session_id.is_empty() {
                        conversation.thread_id.clone()
                    } else {
                        record.session_id
                    },
                    turn_id: record.turn_id,
                },
            );
            comment.id = id;
            comment.set_expected_fingerprint(record.expected_fingerprint);
            comment.refresh_staleness(buffer);
            self.inline_comments.push(comment);
        }
        self.layout_cache.borrow_mut().clear();
    }
}
