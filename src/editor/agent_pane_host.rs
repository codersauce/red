//! Editor glue for the core-owned Agent pane lifecycle in [`super::agent_pane`].
//!
//! Every agent notification the editor sends to plugins passes through
//! [`Editor::notify_agent`], which first feeds it to the pane state machine. Pane
//! effects are queued and run by [`Editor::drain_agent_pane_effects`], which turns
//! them into the existing agent host requests and publishes the pane snapshot.

use super::agent_pane::{AgentPaneEffect, AgentSubmitResult, AgentTurnOutcome};
use super::*;

impl Editor {
    /// Sends an agent notification to plugins after updating the pane lifecycle.
    pub(super) async fn notify_agent(
        &mut self,
        runtime: &mut Runtime,
        event: &str,
        payload: Value,
    ) -> anyhow::Result<()> {
        self.observe_agent_event(event, &payload);
        self.plugin_registry.notify(runtime, event, payload).await
    }

    fn observe_agent_event(&mut self, event: &str, payload: &Value) {
        let text = |key: &str| payload.get(key).and_then(Value::as_str).unwrap_or_default();
        let session_id = text("session_id").to_string();
        let turn_id = payload
            .get("turn_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        let pane = &mut self.agent_manager;
        match event {
            "agent:session_created" => {
                if pane.pane().phase() == super::agent_pane::AgentPanePhase::Starting {
                    pane.update_pane(|lifecycle| lifecycle.session_created(&session_id));
                }
            }
            "agent:turn_started" => {
                if let Some(turn_id) = turn_id {
                    pane.update_pane(|lifecycle| lifecycle.turn_started(&session_id, &turn_id));
                }
            }
            "agent:completed" => {
                let outcome = match text("stop_reason") {
                    "end_turn" | "completed" | "" => AgentTurnOutcome::Completed,
                    _ => AgentTurnOutcome::Stopped,
                };
                pane.update_pane(|lifecycle| {
                    lifecycle.turn_ended(&session_id, turn_id.as_deref(), outcome)
                });
            }
            "agent:error" if session_id.is_empty() => {
                pane.update_pane(|lifecycle| lifecycle.start_failed());
            }
            "agent:error" => {
                pane.update_pane(|lifecycle| {
                    lifecycle.turn_ended(&session_id, turn_id.as_deref(), AgentTurnOutcome::Stopped)
                });
            }
            "agent:cancelled" => pane.update_pane(|lifecycle| lifecycle.stopping(&session_id)),
            "agent:cancel_failed" => {
                pane.update_pane(|lifecycle| lifecycle.stop_failed(&session_id));
            }
            "agent:session_lost" => {
                // A prompt in the payload never reached Codex and is safe to retry.
                let unsent = !text("prompt").is_empty();
                pane.update_pane(|lifecycle| lifecycle.session_lost(unsent));
            }
            "agent:conversation_restore_pending" => {
                let thread_id = text("thread_id").to_string();
                let cwd = text("cwd").to_string();
                pane.update_pane(|lifecycle| lifecycle.restore_pending(&thread_id, &cwd));
            }
            "agent:session_restored" => {
                let thread_id = text("thread_id").to_string();
                pane.update_pane(|lifecycle| lifecycle.restored(&thread_id));
            }
            "agent:session_restore_failed" => {
                pane.update_pane(|lifecycle| lifecycle.restore_failed(&session_id));
            }
            "agent:transcript_restored" => {
                let has_transcript = !text("transcript").trim().is_empty();
                pane.update_pane(|lifecycle| {
                    lifecycle.archived_transcript_restored(has_transcript)
                });
            }
            _ => {}
        }
    }

    /// Handles an Agent pane intent from the bundled plugin.
    pub(super) fn handle_agent_pane_request(&mut self, request: PluginRequest) {
        match request {
            PluginRequest::AgentSubmit { text, transcript } => {
                let (result, _) = self
                    .agent_manager
                    .update_pane(|lifecycle| lifecycle.submit(text, &transcript));
                if result == AgentSubmitResult::QueueFull {
                    self.set_notification_message(
                        Severity::Warning,
                        Some("Agent follow-up queue is full".to_string()),
                    );
                }
            }
            PluginRequest::AgentStop => {
                self.agent_manager.update_pane(|lifecycle| lifecycle.stop());
            }
            PluginRequest::AgentSendQueued { transcript } => {
                self.agent_manager
                    .update_pane(|lifecycle| lifecycle.send_queued(&transcript));
            }
            PluginRequest::AgentDiscardQueued => {
                self.agent_manager
                    .update_pane(|lifecycle| lifecycle.discard_queued());
            }
            PluginRequest::AgentReset { start } => {
                self.agent_manager.update_pane(|lifecycle| {
                    let mut effects = lifecycle.reset();
                    if start {
                        effects.extend(lifecycle.start());
                    }
                    effects
                });
            }
            _ => unreachable!("not an Agent pane request"),
        }
    }

    /// Runs queued pane effects through the existing agent requests, then sends
    /// the pane a fresh snapshot if anything changed.
    pub(super) async fn drain_agent_pane_effects(
        &mut self,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        for effect in self.agent_manager.take_pane_effects() {
            match effect {
                AgentPaneEffect::StartSession => {
                    let cwd = std::env::current_dir().unwrap_or_else(|_| get_workspace_path());
                    runtime.send_request(PluginRequest::AgentNewSession { cwd });
                }
                AgentPaneEffect::ResumeSession { session_id, cwd } => {
                    runtime.send_request(PluginRequest::AgentResumeSession {
                        cwd: PathBuf::from(cwd),
                        session_id,
                    });
                }
                AgentPaneEffect::Dispatch {
                    session_id,
                    prompt,
                    wire_text,
                } => {
                    // The pane commits the message before the turn starts streaming.
                    self.plugin_registry
                        .notify(
                            runtime,
                            "agent:prompt_sent",
                            json!({ "session_id": session_id, "prompt": prompt }),
                        )
                        .await?;
                    runtime.send_request(PluginRequest::AgentPrompt {
                        session_id,
                        text: wire_text,
                    });
                }
                AgentPaneEffect::Interrupt { session_id } => {
                    runtime.send_request(PluginRequest::AgentCancel { session_id });
                }
                AgentPaneEffect::CloseSession { session_id } => {
                    runtime.send_request(PluginRequest::AgentCloseSession { session_id });
                }
                AgentPaneEffect::ForgetSession { session_id } => {
                    runtime.send_request(PluginRequest::AgentCloseSession {
                        session_id: session_id.clone(),
                    });
                    runtime.send_request(PluginRequest::AgentForgetSession { session_id });
                }
            }
        }
        if let Some(snapshot) = self.agent_manager.take_changed_pane_snapshot() {
            self.plugin_registry
                .notify(runtime, "agent:pane_state", serde_json::to_value(snapshot)?)
                .await?;
        }
        Ok(())
    }
}
