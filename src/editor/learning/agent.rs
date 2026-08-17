//! Recorded Agent practice using the production text pane and editor tools.

use super::*;
use crate::learn::{AGENT_EXAMPLE_FIXED, AI_FIXED_CONTENTS, LEARN_AGENT_PANEL};
use crate::plugin::{
    PanelConfig, PanelSide, TextPanelBlock, TextPanelBlockFormat, TextPanelBlockKind,
    TextPanelComposerConfig, TextPanelHeaderAction,
};

const PRACTICE_SESSION: &str = "learn-practice:agent";

#[derive(Default)]
pub(super) struct LearnAgentState {
    blocks: Vec<TextPanelBlock>,
    saved: bool,
}

fn block(id: &str, kind: TextPanelBlockKind, text: impl Into<String>) -> TextPanelBlock {
    TextPanelBlock {
        id: id.into(),
        kind,
        format: TextPanelBlockFormat::Markdown,
        text: text.into(),
    }
}

impl Editor {
    pub(in crate::editor) fn learn_agent_pane_open(&self) -> bool {
        self.learn_session
            .as_ref()
            .is_some_and(|session| session.agent.is_some())
            && self.panel_manager.is_visible(LEARN_AGENT_PANEL)
    }

    pub(in crate::editor) fn learn_agent_files_saved(&self) -> bool {
        self.learn_session.as_ref().is_some_and(|session| {
            session.agent.as_ref().is_some_and(|agent| agent.saved)
                && session.workspace.as_ref().is_some_and(|workspace| {
                    [
                        ("score.rs", AI_FIXED_CONTENTS),
                        ("example.rs", AGENT_EXAMPLE_FIXED),
                    ]
                    .into_iter()
                    .all(|(name, expected)| {
                        let path = workspace.path(name);
                        workspace.permits_file(&path)
                            && std::fs::read_to_string(path).is_ok_and(|actual| actual == expected)
                    })
                })
        })
    }

    fn refresh_learn_agent_panel(&mut self) {
        let Some(agent) = self
            .learn_session
            .as_ref()
            .and_then(|session| session.agent.as_ref())
        else {
            return;
        };
        self.panel_manager.update_text_panel(
            LEARN_AGENT_PANEL,
            agent.blocks.clone(),
            usize::from(self.size.1.saturating_sub(2)),
            usize::from(self.size.0),
        );
    }

    fn open_learn_agent_panel(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        if self.panel_manager.panel_layout(LEARN_AGENT_PANEL).is_none() {
            self.panel_manager.create_text_panel(
                LEARN_AGENT_PANEL.into(),
                PanelConfig {
                    side: PanelSide::Top,
                    width: usize::from(self.size.1).saturating_sub(
                        CoachLayout::for_panel(usize::from(self.size.1)).bottom + 2,
                    ),
                    title: Some("Agent · recorded practice · no network".into()),
                    composer: Some(TextPanelComposerConfig {
                        placeholder: "Save the fix and update the usage example…".into(),
                        rows: 3,
                    }),
                    header_actions: vec![TextPanelHeaderAction {
                        id: "close".into(),
                        label: "Back to code".into(),
                        compact_label: Some("Back".into()),
                    }],
                    ..PanelConfig::default()
                },
            );
            if let Some(agent) = self
                .learn_session
                .as_mut()
                .and_then(|session| session.agent.as_mut())
            {
                agent.blocks = vec![block("context", TextPanelBlockKind::Text,
                    "**Recorded Agent practice**\n\nThe inline selection from `score.rs` is included. Ask to save the fix and update `example.rs`. This runs a fixed local demonstration through Red's real file tools. No model is contacted.")];
            }
            self.refresh_learn_agent_panel();
        }
        self.panel_manager
            .set_panel_visible(LEARN_AGENT_PANEL, true);
        self.panel_manager
            .focus_text_panel_composer(LEARN_AGENT_PANEL);
        self.apply_panel_layout();
        self.force_full_redraw = true;
        self.render(buffer)
    }

    fn close_learn_agent_panel(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.panel_manager
            .set_panel_visible(LEARN_AGENT_PANEL, false);
        self.panel_manager.focus_editor();
        self.apply_panel_layout();
        self.force_full_redraw = true;
        self.render(buffer)
    }

    async fn run_learn_agent_turn(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let root = self
            .learn_session
            .as_ref()
            .and_then(|session| session.workspace.as_ref())
            .ok_or_else(|| anyhow::anyhow!("recorded Agent workspace is missing"))?
            .root()
            .canonicalize()?;
        // No part of the prompt is interpreted as a path or executable command.
        // These are the same revision-checked, securely persisted editor tools
        // used by a live Agent, restricted to the two owned fixture paths.
        for (name, contents) in [
            ("score.rs", AI_FIXED_CONTENTS),
            ("example.rs", AGENT_EXAMPLE_FIXED),
        ] {
            let read = self
                .dispatch_agent_editor_tool_in_workspace(
                    EditorToolRequest {
                        session_id: PRACTICE_SESSION.into(),
                        call: EditorToolCall::ReadFile { path: name.into() },
                    },
                    root.clone(),
                    buffer,
                    runtime,
                )
                .await?;
            if name == "score.rs" {
                anyhow::ensure!(
                    read["content"].as_str() == Some(AI_FIXED_CONTENTS),
                    "the inline fix changed; restart the lesson to try again"
                );
            }
            let revision = read["revision"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("practice file has no revision"))?;
            let result = self
                .dispatch_agent_editor_tool_in_workspace(
                    EditorToolRequest {
                        session_id: PRACTICE_SESSION.into(),
                        call: EditorToolCall::WriteFile {
                            path: name.into(),
                            expected_revision: revision,
                            content: contents.into(),
                        },
                    },
                    root.clone(),
                    buffer,
                    runtime,
                )
                .await?;
            anyhow::ensure!(
                result["saved"].as_bool() == Some(true),
                "could not save {name}: {result}"
            );
        }
        self.dispatch_agent_editor_tool_in_workspace(
            EditorToolRequest {
                session_id: PRACTICE_SESSION.into(),
                call: EditorToolCall::ReadFile {
                    path: "score.rs".into(),
                },
            },
            root,
            buffer,
            runtime,
        )
        .await?;
        if let Some(agent) = self
            .learn_session
            .as_mut()
            .and_then(|session| session.agent.as_mut())
        {
            agent.saved = true;
        }
        anyhow::ensure!(
            self.learn_agent_files_saved(),
            "saved practice files did not match the expected result"
        );
        Ok(())
    }

    #[inline(never)]
    pub(in crate::editor) fn intercept_learn_agent_action<'a>(
        &'a mut self,
        action: &'a Action,
        buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> BoxFuture<'a, anyhow::Result<bool>> {
        Box::pin(async move {
            let Some(session) = self
                .learn_session
                .as_ref()
                .filter(|session| session.agent.is_some())
            else {
                return Ok(false);
            };
            let step = session.step;
            match action {
                Action::NextBuffer | Action::PreviousBuffer => {
                    let indices = self
                        .buffer_manager
                        .iter()
                        .enumerate()
                        .filter(|(_, candidate)| session.owns_buffer(candidate))
                        .map(|(index, _)| index)
                        .collect::<Vec<_>>();
                    if let Some(current) = indices
                        .iter()
                        .position(|index| *index == self.buffer_manager.active_index())
                    {
                        let next = if matches!(action, Action::PreviousBuffer) {
                            (current + indices.len() - 1) % indices.len()
                        } else {
                            (current + 1) % indices.len()
                        };
                        self.set_current_buffer(buffer, indices[next]).await?;
                    }
                }
                Action::EscalateInlineAssist => {
                    if step != PracticeStep::AgentEscalate
                        || self.current_buffer().contents() != AI_FIXED_CONTENTS
                    {
                        self.set_legacy_message(Some("make the practice inline fix first".into()));
                    } else {
                        self.close_inline_assist_session();
                        self.open_learn_agent_panel(buffer)?;
                    }
                }
                Action::PluginCommand(name)
                    if matches!(name.as_str(), "Agent" | "AgentOpen" | "AgentToggle") =>
                {
                    if matches!(
                        step,
                        PracticeStep::AgentPrompt
                            | PracticeStep::AgentInspect
                            | PracticeStep::Complete
                    ) {
                        self.open_learn_agent_panel(buffer)?;
                    } else {
                        self.set_legacy_message(Some(
                            "start with the inline fix, then choose A in its result controls"
                                .into(),
                        ));
                    }
                }
                Action::NotifyPlugins(method, params)
                    if method == &format!("panel:event:{LEARN_AGENT_PANEL}") =>
                {
                    match params.get("action").and_then(Value::as_str) {
                        Some("close") => self.close_learn_agent_panel(buffer)?,
                        Some("composer_focus") => {
                            self.panel_manager
                                .focus_text_panel_composer(LEARN_AGENT_PANEL);
                        }
                        Some("submit") => {
                            let text = params
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .trim();
                            if !text.is_empty() {
                                if let Some(agent) = self
                                    .learn_session
                                    .as_mut()
                                    .and_then(|session| session.agent.as_mut())
                                {
                                    let id = format!("prompt-{}", agent.blocks.len());
                                    agent
                                        .blocks
                                        .push(block(&id, TextPanelBlockKind::User, text));
                                }
                                self.refresh_learn_agent_panel();
                                let outcome = self.run_learn_agent_turn(buffer, runtime).await;
                                if let Some(agent) = self
                                    .learn_session
                                    .as_mut()
                                    .and_then(|session| session.agent.as_mut())
                                {
                                    let id = format!("result-{}", agent.blocks.len());
                                    let (kind, text) = match outcome {
                                        Ok(()) => (TextPanelBlockKind::Agent, "**Recorded response — files saved**\n\nSaved the addition fix in `score.rs` and updated `example.rs` to expect 42. These writes used Red's real editor tools. Choose **Back to code**, then `:bn` to inspect the example.".to_string()),
                                        Err(error) => (TextPanelBlockKind::Error, format!("Recorded practice failed: {error}. Restart the lesson to restore its files.")),
                                    };
                                    agent.blocks.push(block(&id, kind, text));
                                }
                                self.refresh_learn_agent_panel();
                                self.panel_manager.focus_panel(LEARN_AGENT_PANEL);
                            }
                        }
                        _ => {}
                    }
                }
                Action::Refresh
                    if self.learn_agent_pane_open() && !self.panel_manager.has_focused_panel() =>
                {
                    self.close_learn_agent_panel(buffer)?;
                }
                _ => return Ok(false),
            }
            self.observe_learn_action(action, buffer)?;
            self.render(buffer)?;
            Ok(true)
        })
    }
}
