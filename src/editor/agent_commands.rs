//! Editor-side agent commands: every place the editor asks the backend to act.

use super::*;

impl Editor {
    pub(super) async fn dispatch_agent_prompt(
        &mut self,
        runtime: &mut Runtime,
        session_id: String,
        text: String,
        context: Option<(String, String)>,
    ) -> anyhow::Result<bool> {
        if self.agent_manager.is_task_finished() {
            let message = self
                .finish_agent_bridge(
                    runtime,
                    "Codex app-server stopped before the prompt was sent",
                )
                .await?;
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
        if !self.agent_manager.has_bridge() {
            self.abort_agent_bridge();
            self.notify_agent(
                runtime,
                "agent:session_lost",
                json!({
                    "session_id": session_id,
                    "prompt": text,
                    "message": "No Codex app-server session is running"
                }),
            )
            .await?;
            return Ok(false);
        }
        if self.agent_manager.is_session_busy(&session_id) {
            // A cancelled turn keeps running until Codex confirms the interrupt;
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
        let Some(bridge) = self.agent_manager.bridge() else {
            return Ok(false);
        };
        let command = context.map_or_else(
            || CodexCommand::Prompt {
                session_id: session_id.clone(),
                turn,
                text: text.clone(),
            },
            |(uri, context)| CodexCommand::PromptWithContext {
                session_id: session_id.clone(),
                turn,
                text: text.clone(),
                uri,
                context,
            },
        );
        if bridge.send(command).await.is_err() {
            let message = self
                .finish_agent_bridge(runtime, "Codex app-server stopped while sending the prompt")
                .await?;
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
        Ok(false)
    }

    pub(super) fn ensure_agent_bridge(&mut self, cwd: &Path) -> anyhow::Result<()> {
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
        if self.agent_manager.has_bridge() {
            return Ok(());
        }
        let configured = self.config.agent.command.as_deref().unwrap_or("codex");
        let command = crate::utils::find_executable(configured).ok_or_else(|| {
            anyhow::anyhow!(
                "Codex CLI was not found; install Codex, run `codex login`, and try again"
            )
        })?;
        let policy = AgentRuntimePolicy::new(
            self.config.agent.allow_sensitive_paths,
            self.config.agent.enabled_mcp_servers.clone(),
            self.config.agent.enabled_codex_features.clone(),
        );
        let mut spec = CodexProcessSpec::new(command, cwd.clone())
            .args(self.config.agent.args.clone())
            .policy(policy);
        spec.environment.extend(
            self.config
                .agent
                .env
                .clone()
                .into_iter()
                .map(|(key, value)| (key.into(), value.into())),
        );
        let capacity =
            NonZeroUsize::new(AGENT_BRIDGE_CAPACITY).expect("agent bridge capacity is non-zero");
        let (tool_sender, tool_requests) = editor_tool_channel(AGENT_BRIDGE_CAPACITY);
        let host = EditorToolHost::new(tool_sender);
        let (bridge, task) = start_codex(spec, ToolHostAdapter(host), capacity)?;
        self.agent_manager.set_root(Some(cwd));
        self.agent_manager.set_bridge(bridge);
        self.agent_manager.set_task(task);
        self.agent_manager.set_tool_requests(tool_requests);
        Ok(())
    }

    pub(super) async fn agent_new_session(
        &mut self,
        runtime: &mut Runtime,
        cwd: PathBuf,
    ) -> anyhow::Result<()> {
        if self.agent_manager.is_task_finished() {
            let model_only_bridge = self.agent_manager.is_model_only_bridge();
            let message = self
                .finish_agent_bridge(runtime, "Codex app-server stopped unexpectedly")
                .await?;
            if !model_only_bridge {
                self.notify_agent(runtime, "agent:session_lost", json!({ "message": message }))
                    .await?;
            }
        }
        self.agent_manager.mark_conversation_requested();
        if let Err(error) = self.ensure_agent_bridge(&cwd) {
            self.notify_agent(
                runtime,
                "agent:error",
                json!({ "message": error.to_string() }),
            )
            .await?;
            return Ok(());
        }
        let Some(bridge) = self.agent_manager.bridge() else {
            return Ok(());
        };
        let command = self.agent_manager.next_model().cloned().map_or_else(
            || CodexCommand::NewSession { cwd: cwd.clone() },
            |selection| CodexCommand::NewSessionWithModel {
                cwd: cwd.clone(),
                selection,
            },
        );
        if bridge.send(command).await.is_err() {
            let message = self
                .finish_agent_bridge(
                    runtime,
                    "Codex app-server stopped while starting the session",
                )
                .await?;
            self.notify_agent(runtime, "agent:session_lost", json!({ "message": message }))
                .await?;
        }
        Ok(())
    }

    pub(super) async fn agent_resume_session(
        &mut self,
        runtime: &mut Runtime,
        cwd: PathBuf,
        session_id: String,
    ) -> anyhow::Result<()> {
        self.agent_manager.mark_conversation_requested();
        if self
            .agent_manager
            .conversation_tool_contract_version(&session_id)
            .is_some_and(|version| {
                version != crate::agent_conversation::AGENT_TOOL_CONTRACT_VERSION
            })
        {
            self.notify_agent(runtime, "agent:session_restore_failed",
                        json!({
                            "session_id": session_id,
                            "message": "Agent tools changed after this conversation was saved; the transcript remains available and the next message will start a compatible session"
                        }),
                    )
                    .await?;
            return Ok(());
        }
        if self.agent_manager.is_task_finished() {
            let _ = self
                .finish_agent_bridge(runtime, "Codex app-server stopped before restoration")
                .await?;
        }
        if let Err(error) = self.ensure_agent_bridge(&cwd) {
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
        let Some(bridge) = self.agent_manager.bridge() else {
            return Ok(());
        };
        if bridge
            .send(CodexCommand::ResumeSession {
                cwd,
                session_id: session_id.clone(),
            })
            .await
            .is_err()
        {
            let message = self
                .finish_agent_bridge(
                    runtime,
                    "Codex app-server stopped while restoring the session",
                )
                .await?;
            self.notify_agent(
                runtime,
                "agent:session_restore_failed",
                json!({ "session_id": session_id, "message": message }),
            )
            .await?;
        }
        Ok(())
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
        self.agent_manager.mark_conversation_requested();
        if self.agent_manager.is_task_finished() {
            let _ = self
                .finish_agent_bridge(
                    runtime,
                    "Codex app-server stopped before switching sessions",
                )
                .await?;
        }
        if let Err(error) = self.ensure_agent_bridge(&cwd) {
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
        let Some(bridge) = self.agent_manager.bridge() else {
            return Ok(());
        };
        let tool_contract_version = if resume {
            self.agent_manager
                .conversation_tool_contract_version(&session_id)
        } else {
            None
        };
        let command =
            agent_sessions::session_switch_command(cwd, session_id.clone(), tool_contract_version);
        if bridge.send(command).await.is_err() {
            let message = self
                .finish_agent_bridge(runtime, "Codex app-server stopped while switching sessions")
                .await?;
            self.notify_agent(
                runtime,
                "agent:session_restore_failed",
                json!({ "session_id": session_id, "message": message }),
            )
            .await?;
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
        let Some(bridge) = self.agent_manager.bridge() else {
            return Ok(());
        };
        if bridge
            .send(CodexCommand::Cancel { session_id })
            .await
            .is_err()
        {
            self.notify_agent(
                runtime,
                "agent:error",
                json!({ "message": "Codex app-server stopped" }),
            )
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
        let Some(bridge) = self.agent_manager.bridge() else {
            return Ok(());
        };
        if bridge
            .send(CodexCommand::CloseSession { session_id })
            .await
            .is_err()
        {
            self.notify_agent(
                runtime,
                "agent:error",
                json!({ "message": "Codex app-server stopped" }),
            )
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
        let result = match prompt {
            Err(error) => Err(error),
            Ok(prompt) => {
                if self.agent_manager.is_task_finished() {
                    let _ = self
                        .finish_agent_bridge(runtime, "Codex app-server stopped unexpectedly")
                        .await?;
                }
                self.ensure_agent_bridge(&get_workspace_path())
                    .and_then(|()| {
                        let bridge = self
                            .agent_manager
                            .bridge()
                            .ok_or_else(|| anyhow::anyhow!("Codex app-server did not start"))?;
                        bridge.try_send(CodexCommand::GenerateCommitMessage {
                            request_id: request_id.get(),
                            cwd,
                            prompt,
                        })
                    })
            }
        };
        if let Err(error) = result {
            self.plugin_registry
                .resolve_request(
                    runtime,
                    request_id,
                    json!({ "message": "", "error": error.to_string() }),
                )
                .await?;
        } else {
            self.agent_manager
                .mark_commit_message_pending(request_id.get());
        }
        Ok(())
    }

    pub(super) async fn agent_permission_response(
        &mut self,
        runtime: &mut Runtime,
        request_id: String,
        option_id: Option<String>,
    ) -> anyhow::Result<()> {
        let Some(bridge) = self.agent_manager.bridge() else {
            return Ok(());
        };
        if bridge
            .send(CodexCommand::PermissionResponse {
                request_id,
                option_id,
            })
            .await
            .is_err()
        {
            self.notify_agent(
                runtime,
                "agent:error",
                json!({ "message": "Codex app-server stopped" }),
            )
            .await?;
        }
        Ok(())
    }

    /// Sends one inline-assist turn: a fresh thread, or a follow-up on `existing_session`.
    pub(super) async fn agent_send_inline_assist(
        &mut self,
        request_id: &str,
        cwd: PathBuf,
        existing_session: Option<String>,
        prompt: &str,
        context: &str,
    ) -> anyhow::Result<()> {
        let command = existing_session.map_or_else(
            || CodexCommand::InlineAssist {
                request_id: request_id.to_string(),
                cwd,
                prompt: prompt.to_string(),
                context: context.to_string(),
            },
            |session_id| CodexCommand::InlineAssistFollowup {
                request_id: request_id.to_string(),
                session_id,
                prompt: prompt.to_string(),
                context: context.to_string(),
            },
        );
        if let Some(bridge) = self.agent_manager.bridge() {
            bridge.send(command).await
        } else {
            Err(anyhow::anyhow!("Codex bridge is unavailable"))
        }
    }
}
