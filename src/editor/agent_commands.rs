//! Editor-side agent commands: every place the editor asks the backend to act.

use super::*;
use crate::agent_backend::{
    backend::AgentError,
    ids::{Continuity, ContractStamp, QueryId, SavedSession, SessionId},
    types::{EditorContext, Prompt},
};

impl Editor {
    pub(super) async fn dispatch_agent_prompt(
        &mut self,
        runtime: &mut Runtime,
        session_id: String,
        text: String,
        context: Option<(String, String)>,
    ) -> anyhow::Result<bool> {
        if !self.agent_manager.is_running() {
            self.reset_agent_state("Agent session closed.");
            let message = self.agent_not_running_message();
            self.notify_agent(
                runtime,
                "agent:session_lost",
                json!({
                    "session_id": session_id,
                    "prompt": text,
                    "message": message
                }),
            )
            .await?;
            return Ok(false);
        }
        if self.agent_manager.is_session_busy(&session_id) {
            // A cancelled turn keeps running until the agent confirms the stop;
            // holding the prompt avoids racing that turn's terminal event.
            self.agent_manager.queue_prompt(
                session_id,
                agent_manager::QueuedAgentPrompt { text, context },
            );
            return Ok(false);
        }
        let turn = TurnId::mint();
        let turn_id = turn.to_string();
        self.begin_inline_agent_outcome(&session_id, &turn_id, &text)?;
        self.agent_manager
            .set_turn_id(session_id.clone(), turn_id.clone());
        self.agent_manager
            .record_user_message(&session_id, &turn_id, &text);
        self.notify_agent(
            runtime,
            "agent:turn_started",
            json!({ "session_id": session_id, "turn_id": turn_id }),
        )
        .await?;
        self.agent_manager.mark_session_active(session_id.clone());
        self.agent_manager.record_turn_start(session_id.clone());
        let prompt = Prompt {
            text: text.clone(),
            context: context.map(|(uri, text)| EditorContext { uri, text }),
            recap: None,
        };
        match self.agent_manager.agents_mut().prompt(
            &SessionId::from_raw(session_id.clone()),
            turn,
            prompt,
        ) {
            Ok(()) => {}
            // The loss that follows carries the prompt back to the plugin.
            Err(AgentError::Gone(_)) => {
                self.agent_manager
                    .set_loss_context(agent_manager::AgentLossContext::Prompt {
                        session_id,
                        prompt: text,
                    });
            }
            Err(AgentError::Unavailable(message)) => {
                self.finish_inline_agent_outcome(
                    &session_id,
                    crate::inline_history::InlineAgentState::Failed,
                    Some(&message),
                );
                self.agent_manager.mark_session_inactive(&session_id);
                self.agent_manager.discard_turn(&session_id);
                self.notify_agent(
                    runtime,
                    "agent:session_lost",
                    json!({
                        "session_id": session_id,
                        "prompt": text,
                        "message": message
                    }),
                )
                .await?;
            }
        }
        Ok(false)
    }

    /// Makes sure the agent runs in `cwd`, starting it when it is not running.
    pub(super) fn ensure_agent(&mut self, cwd: &Path) -> anyhow::Result<()> {
        if self.config.disable_ai {
            anyhow::bail!("agent support is disabled by `disable_ai = true`");
        }
        let cwd = cwd.absolutize()?.into_owned();
        if let Some(root) = self.agent_manager.root() {
            anyhow::ensure!(
                root == cwd,
                "Codex session root `{}` does not match the active agent workspace `{}`",
                cwd.display(),
                root.display()
            );
        }
        if self.agent_manager.is_running() {
            return Ok(());
        }
        self.agent_manager.agents_mut().ensure(&cwd)?;
        self.agent_manager.set_root(Some(cwd));
        Ok(())
    }

    /// What the user hears when a command needs an agent that is not running.
    pub(super) fn agent_not_running_message(&self) -> String {
        format!(
            "No {} app-server session is running",
            self.agent_manager.agents().kind().name()
        )
    }

    /// What the user hears when a command cannot reach a running agent.
    fn agent_stopped_message(&self) -> String {
        format!(
            "{} app-server stopped",
            self.agent_manager.agents().kind().name()
        )
    }

    pub(super) async fn agent_new_session(
        &mut self,
        runtime: &mut Runtime,
        cwd: PathBuf,
    ) -> anyhow::Result<()> {
        if let Err(error) = self.ensure_agent(&cwd) {
            self.notify_agent(
                runtime,
                "agent:error",
                json!({ "message": error.to_string() }),
            )
            .await?;
            return Ok(());
        }
        let model = self.agent_manager.next_model().cloned();
        match self.agent_manager.agents_mut().new_session(&cwd, model) {
            Ok(()) => {}
            Err(AgentError::Gone(_)) => {
                self.agent_manager
                    .set_loss_context(agent_manager::AgentLossContext::NewSession);
            }
            Err(AgentError::Unavailable(message)) => {
                self.notify_agent(runtime, "agent:error", json!({ "message": message }))
                    .await?;
            }
        }
        Ok(())
    }

    pub(super) async fn agent_resume_session(
        &mut self,
        runtime: &mut Runtime,
        cwd: PathBuf,
        session_id: String,
    ) -> anyhow::Result<()> {
        // A conversation whose tool stamp is unknown has never been seen to
        // disagree with the current tools, so it rejoins.
        let contract = self
            .agent_manager
            .conversation_tool_contract_version(&session_id)
            .map(ContractStamp::new)
            .or_else(|| Some(self.agent_manager.agents().contract()));
        let saved = self.saved_session(&session_id, contract);
        if self.agent_manager.agents().continuity(&saved) != Continuity::Rejoin {
            self.notify_agent(runtime, "agent:session_restore_failed",
                        json!({
                            "session_id": session_id,
                            "message": "Agent tools changed after this conversation was saved; the transcript remains available and the next message will start a compatible session"
                        }),
                    )
                    .await?;
            return Ok(());
        }
        self.restore_agent_session(runtime, &cwd, &saved).await
    }

    pub(super) async fn agent_switch_session(
        &mut self,
        runtime: &mut Runtime,
        cwd: PathBuf,
        session_id: String,
        resume: bool,
    ) -> anyhow::Result<()> {
        let accepted = self
            .agent_manager
            .update_pane(|lifecycle| lifecycle.switch_pending(&session_id));
        if !accepted {
            self.notify_agent(
                    runtime,
                    "agent:session_restore_failed",
                    json!({
                        "session_id": session_id,
                        "message": "The Agent conversation is busy, has held follow-ups, or is already selected"
                    }),
                )
                .await?;
            return Ok(());
        }
        // Only a conversation known to run the current tools may be rejoined;
        // anything else is forked into a fresh session.
        let contract = if resume {
            self.agent_manager
                .conversation_tool_contract_version(&session_id)
                .map(ContractStamp::new)
        } else {
            None
        };
        let saved = self.saved_session(&session_id, contract);
        self.restore_agent_session(runtime, &cwd, &saved).await
    }

    fn saved_session(&self, session_id: &str, contract: Option<ContractStamp>) -> SavedSession {
        SavedSession {
            backend: self.agent_manager.agents().kind(),
            session: SessionId::from_raw(session_id.to_string()),
            contract,
        }
    }

    /// Starts the agent if needed and asks it to bring `saved` back. Failures
    /// reach the plugin as `agent:session_restore_failed`.
    async fn restore_agent_session(
        &mut self,
        runtime: &mut Runtime,
        cwd: &Path,
        saved: &SavedSession,
    ) -> anyhow::Result<()> {
        let session_id = saved.session.as_str().to_string();
        if let Err(error) = self.ensure_agent(cwd) {
            self.notify_agent(
                runtime,
                "agent:session_restore_failed",
                json!({
                    "session_id": session_id,
                    "message": error.to_string()
                }),
            )
            .await?;
            return Ok(());
        }
        match self.agent_manager.agents_mut().restore(cwd, saved) {
            Ok(()) => {}
            Err(AgentError::Gone(_)) => {
                self.agent_manager
                    .set_loss_context(agent_manager::AgentLossContext::Restore { session_id });
            }
            Err(AgentError::Unavailable(message)) => {
                self.notify_agent(
                    runtime,
                    "agent:session_restore_failed",
                    json!({ "session_id": session_id, "message": message }),
                )
                .await?;
            }
        }
        Ok(())
    }

    pub(super) async fn agent_cancel(
        &mut self,
        runtime: &mut Runtime,
        session_id: String,
    ) -> anyhow::Result<()> {
        self.abandon_agent_edit_decision(Some(&session_id), "the turn was cancelled");
        self.agent_manager.begin_stopping(&session_id);
        // With no live turn the stop would have nothing to interrupt, and a
        // confirmation would land on whichever turn comes next.
        let Some(turn) = self.agent_manager.live_turn(&session_id) else {
            return Ok(());
        };
        if !self.agent_manager.is_running() {
            return Ok(());
        }
        if self
            .agent_manager
            .agents_mut()
            .stop(&SessionId::from_raw(session_id), turn)
            .is_err()
        {
            let message = self.agent_stopped_message();
            self.notify_agent(runtime, "agent:error", json!({ "message": message }))
                .await?;
        }
        Ok(())
    }

    pub(super) async fn agent_close_session(
        &mut self,
        runtime: &mut Runtime,
        session_id: String,
    ) -> anyhow::Result<()> {
        self.abandon_agent_edit_decision(Some(&session_id), "the conversation was closed");
        self.finish_inline_agent_outcome(
            &session_id,
            crate::inline_history::InlineAgentState::Cancelled,
            Some("Agent session closed."),
        );
        self.agent_manager.mark_session_inactive(&session_id);
        self.agent_manager.discard_session_queue(&session_id);
        self.agent_manager
            .update_pane(|lifecycle| lifecycle.session_closed(&session_id));
        if !self.agent_manager.is_running() {
            return Ok(());
        }
        if self
            .agent_manager
            .agents_mut()
            .close(&SessionId::from_raw(session_id))
            .is_err()
        {
            let message = self.agent_stopped_message();
            self.notify_agent(runtime, "agent:error", json!({ "message": message }))
                .await?;
        }
        Ok(())
    }

    pub(super) async fn agent_generate_commit_message(
        &mut self,
        runtime: &mut Runtime,
        request_id: RequestId,
        cwd: PathBuf,
        branch: String,
        staged_diff: String,
        recent_commits: String,
    ) -> anyhow::Result<()> {
        let prompt = commit_message_prompt(&branch, &staged_diff, &recent_commits);
        let result = prompt.and_then(|prompt| {
            self.ensure_agent(&get_workspace_path())?;
            self.agent_manager
                .agents_mut()
                .commit_messages()?
                .generate(QueryId(request_id.get()), &cwd, prompt)?;
            Ok(())
        });
        if let Err(error) = result {
            self.plugin_registry
                .resolve_request(
                    runtime,
                    request_id,
                    json!({ "message": "", "error": error.to_string() }),
                )
                .await?;
        }
        Ok(())
    }

    /// Sends one inline-assist turn: a fresh conversation, or a follow-up on
    /// `existing_session`.
    pub(super) fn agent_send_inline_assist(
        &mut self,
        request_id: &str,
        cwd: &Path,
        existing_session: Option<String>,
        prompt: &str,
        context: &str,
    ) -> anyhow::Result<()> {
        let inline = self.agent_manager.agents_mut().inline_assist()?;
        let (request, prompt, context) = (
            request_id.to_string(),
            prompt.to_string(),
            context.to_string(),
        );
        match existing_session {
            None => inline.start(request, cwd, prompt, context)?,
            Some(session) => {
                inline.followup(request, &SessionId::from_raw(session), prompt, context)?;
            }
        }
        Ok(())
    }
}
