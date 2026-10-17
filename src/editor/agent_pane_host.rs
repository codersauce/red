//! Editor glue for the core-owned Agent pane lifecycle in [`super::agent_pane`].
//!
//! Every agent notification the editor sends to plugins passes through
//! [`Editor::notify_agent`], which first feeds it to the pane state machine. Pane
//! effects are queued and run by [`Editor::drain_agent_pane_effects`], which turns
//! them into the existing agent host requests and publishes the pane snapshot.

use super::agent_pane::{AgentPaneEffect, AgentSubmitResult};
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
        self.agent_manager
            .update_pane(|lifecycle| lifecycle.observe(event, payload));
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

    /// Sends the pane a fresh snapshot if anything changed, then runs queued pane
    /// effects through the existing agent requests.
    pub(super) async fn drain_agent_pane_effects(
        &mut self,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let effects = self.agent_manager.take_pane_effects();
        // Publish first so the pane renders a sent message as part of a running turn.
        if let Some(snapshot) = self.agent_manager.take_changed_pane_snapshot() {
            self.plugin_registry
                .notify(runtime, "agent:pane_state", serde_json::to_value(snapshot)?)
                .await?;
        }
        for effect in effects {
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
                    wire_text,
                    ..
                } => {
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
        Ok(())
    }
}
