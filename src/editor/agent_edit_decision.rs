//! Asking the user before an agent edit lands in a buffer with unsaved changes.
//!
//! A successful agent write used to save the whole buffer, so the user's work in
//! progress reached disk without their say. Now a write to a buffer the user has
//! changed waits while Red asks: apply and keep the buffer unsaved (the default),
//! apply and save everything, or don't apply. The answer is remembered for that
//! conversation and file until the file is next saved, so a multi-edit turn asks once.
//!
//! The dialog may appear while the user is typing, so it waits for a pause in input,
//! ignores keys for a moment after it opens, and only "keep unsaved" is on Enter.

use super::*;
use crate::ui::{Confirmation, ConfirmationChoice};

/// Input pause before the question may interrupt the user.
const TYPING_PAUSE: Duration = Duration::from_millis(700);

/// What the user decided about an agent edit to a buffer with unsaved changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentUnsavedChoice {
    /// Apply the edit and leave the buffer unsaved.
    KeepUnsaved,
    /// Apply the edit and save the buffer, including the user's changes.
    SaveAll,
    /// Leave the buffer alone; the agent is told the user declined.
    Decline,
}

/// An agent write held until the user answers.
pub(super) struct PendingAgentEditDecision {
    tool: PendingEditorTool,
    path: PathBuf,
    title: String,
    shown: bool,
}

#[derive(Default)]
pub(super) struct AgentEditDecisions {
    /// Choice per conversation and file, valid while the buffer's save generation
    /// is unchanged.
    choices: HashMap<(String, PathBuf), (AgentUnsavedChoice, u64)>,
    pending: Option<PendingAgentEditDecision>,
}

impl AgentEditDecisions {
    fn remembered(
        &self,
        session_id: &str,
        path: &Path,
        generation: u64,
    ) -> Option<AgentUnsavedChoice> {
        self.choices
            .get(&(session_id.to_string(), path.to_path_buf()))
            .filter(|(_, saved_at)| *saved_at == generation)
            .map(|(choice, _)| *choice)
    }

    pub(super) fn is_waiting(&self) -> bool {
        self.pending.is_some()
    }
}

impl Editor {
    /// Records that the user typed, so an unprompted question can wait for a pause.
    pub(super) fn note_user_input(&mut self) {
        self.last_user_input_at = Some(Instant::now());
    }

    /// How an agent write to the current buffer treats the user's unsaved changes.
    /// A clean buffer is saved as before; a dirty one keeps them unsaved unless the
    /// user chose otherwise for this conversation and file.
    pub(super) fn agent_unsaved_choice(&self, session_id: &str, path: &Path) -> AgentUnsavedChoice {
        let buffer = self.current_buffer();
        if !buffer.is_dirty() {
            return AgentUnsavedChoice::SaveAll;
        }
        self.agent_edit_decisions
            .remembered(session_id, path, buffer.save_generation())
            .unwrap_or(AgentUnsavedChoice::KeepUnsaved)
    }

    /// Returns the target of an agent write that must wait for the user: its buffer
    /// is open with unsaved changes, the write would apply cleanly and keep them, and
    /// there is no remembered answer. Writes that will fail anyway are not held.
    pub(super) fn agent_write_needing_decision(
        &self,
        request: &EditorToolRequest,
    ) -> Option<PathBuf> {
        let (path, expected_revision) = match &request.call {
            EditorToolCall::WriteFile {
                path,
                expected_revision,
                ..
            }
            | EditorToolCall::ApplyEdits {
                path,
                expected_revision,
                ..
            } => (path, *expected_revision),
            _ => return None,
        };
        let root = self.agent_manager.root()?;
        let path = resolve_agent_tool_path_with_policy(
            root,
            path,
            self.config.agent.allow_sensitive_paths,
        )
        .ok()?;
        let path = normalized_file_path(&path.to_string_lossy()).ok()?;
        let buffer = self.buffer_manager.get(self.file_buffer_index(&path)?)?;
        if !buffer.is_dirty() || !agent_revision_matches(buffer, expected_revision) {
            return None;
        }
        let proposed = match &request.call {
            EditorToolCall::WriteFile { content, .. } => content.clone(),
            EditorToolCall::ApplyEdits { edits, .. } => {
                crate::agent_tools::apply_text_edits(&buffer.contents(), edits).ok()?
            }
            _ => return None,
        };
        if proposed == buffer.contents() || !buffer.preserves_unsaved_edits(&proposed) {
            return None;
        }
        self.agent_edit_decisions
            .remembered(&request.session_id, &path, buffer.save_generation())
            .is_none()
            .then_some(path)
    }

    /// Holds an agent write until the user answers.
    pub(super) fn hold_agent_write_for_decision(&mut self, tool: PendingEditorTool, path: PathBuf) {
        let name = self
            .agent_manager
            .root()
            .and_then(|root| path.strip_prefix(root).ok())
            .unwrap_or(&path)
            .display()
            .to_string();
        self.agent_edit_decisions.pending = Some(PendingAgentEditDecision {
            tool,
            title: format!("Unsaved changes in {name}"),
            path,
            shown: false,
        });
    }

    /// Opens the question once the user pauses, and lets go of a held write whose
    /// tool call already timed out or was cancelled.
    pub(super) fn service_agent_edit_decision(
        &mut self,
        render_buffer: &mut RenderBuffer,
    ) -> anyhow::Result<()> {
        let ours_open = self.agent_decision_dialog_open();
        let Some(pending) = self.agent_edit_decisions.pending.as_mut() else {
            return Ok(());
        };
        if pending.tool.response.is_closed() {
            self.abandon_agent_edit_decision(None, "the tool call ended");
            return self.render(render_buffer);
        }
        // Another dialog replaced ours; ask again when it closes.
        pending.shown &= ours_open;
        let paused = self
            .last_user_input_at
            .is_none_or(|at| at.elapsed() >= TYPING_PAUSE);
        if pending.shown || self.current_dialog.is_some() || !paused {
            return Ok(());
        }
        pending.shown = true;
        let title = pending.title.clone();
        let name = title.trim_start_matches("Unsaved changes in ").to_string();
        let message = format!(
            "The agent wants to edit {name}, which has changes you haven't saved. Your changes stay in the buffer either way.\n\nRed remembers your answer for this file until you save it."
        );
        let choice = |choice| Action::AgentUnsavedEditChoice(choice);
        let dialog = Confirmation::new_choices(
            self,
            title,
            message,
            vec![
                ConfirmationChoice::new(
                    "Apply, keep unsaved",
                    Some('u'),
                    choice(AgentUnsavedChoice::KeepUnsaved),
                ),
                ConfirmationChoice::new(
                    "Apply and save",
                    None,
                    choice(AgentUnsavedChoice::SaveAll),
                ),
                ConfirmationChoice::new(
                    "Don't apply",
                    Some('n'),
                    choice(AgentUnsavedChoice::Decline),
                ),
            ],
            /*selected*/ 0,
            /*cancel*/ 2,
        );
        self.current_dialog = Some(Box::new(dialog));
        self.render(render_buffer)
    }

    fn agent_decision_dialog_open(&self) -> bool {
        let Some(pending) = self.agent_edit_decisions.pending.as_ref() else {
            return false;
        };
        self.current_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.shortcut_context() == pending.title)
    }

    /// Applies the user's answer to the held write and runs it.
    pub(super) async fn resolve_agent_edit_decision(
        &mut self,
        choice: AgentUnsavedChoice,
        render_buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let Some(pending) = self.agent_edit_decisions.pending.take() else {
            return Ok(());
        };
        if let Some(buffer) = self
            .file_buffer_index(&pending.path)
            .and_then(|index| self.buffer_manager.get(index))
        {
            let generation = buffer.save_generation();
            self.agent_edit_decisions.choices.insert(
                (
                    pending.tool.request.session_id.clone(),
                    pending.path.clone(),
                ),
                (choice, generation),
            );
        }
        self.set_quiet_message(Some(
            match choice {
                AgentUnsavedChoice::KeepUnsaved => "Agent edit applied; buffer left unsaved",
                AgentUnsavedChoice::SaveAll => "Agent edit applied and saved",
                AgentUnsavedChoice::Decline => "Agent edit not applied",
            }
            .to_string(),
        ));
        self.run_staged_agent_tool(pending.tool, render_buffer, runtime)
            .await;
        Ok(())
    }

    /// Drops a held write, answering its tool call. With a session, only that
    /// session's write is dropped.
    pub(super) fn abandon_agent_edit_decision(&mut self, session_id: Option<&str>, reason: &str) {
        if session_id.is_some_and(|session_id| {
            self.agent_edit_decisions
                .pending
                .as_ref()
                .is_some_and(|pending| pending.tool.request.session_id != session_id)
        }) {
            return;
        }
        let ours_open = self.agent_decision_dialog_open();
        let Some(pending) = self.agent_edit_decisions.pending.take() else {
            return;
        };
        if ours_open {
            self.current_dialog = None;
        }
        let _ = pending.tool.response.send(Err(format!(
            "edit not applied: {reason} before the user decided about unsaved changes"
        )));
    }

    /// Forgets remembered answers for a conversation that ended.
    pub(super) fn forget_agent_edit_choices(&mut self, session_id: &str) {
        self.agent_edit_decisions
            .choices
            .retain(|(session, _), _| session != session_id);
    }
}
