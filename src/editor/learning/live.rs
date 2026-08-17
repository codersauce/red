//! Explicit live inline practice, isolated from the user's Agent session.

use super::*;
use crate::editor::agent_manager::AgentManager;

pub(super) struct LearnLiveAiState {
    original_agent: AgentManager,
    pub ready: bool,
    pub received: bool,
}

impl LearnLiveAiState {
    #[inline(never)]
    pub fn install(editor: &mut Editor, root: &Path) -> Box<Self> {
        let original_agent = std::mem::take(&mut editor.agent_manager);
        editor.agent_manager.set_root(Some(root.to_path_buf()));
        Box::new(Self {
            original_agent,
            ready: false,
            received: false,
        })
    }

    pub fn restore(self, editor: &mut Editor) {
        editor.abort_agent_bridge();
        editor.agent_manager = self.original_agent;
    }
}

impl Editor {
    pub(in crate::editor) fn is_learn_live_practice(&self) -> bool {
        self.learn_session
            .as_ref()
            .is_some_and(|session| session.live.is_some())
    }

    fn learn_live_ready(&self) -> bool {
        self.learn_session.as_ref().is_some_and(|session| {
            !session.original_language.original_ai_disabled()
                && session.live.as_ref().is_some_and(|live| live.ready)
        })
    }

    pub(in crate::editor) fn inline_assist_workspace(&self) -> PathBuf {
        self.learn_session
            .as_ref()
            .filter(|session| session.live.is_some())
            .and_then(|session| session.workspace.as_ref())
            .map_or_else(get_workspace_path, |workspace| {
                workspace.root().to_path_buf()
            })
    }

    pub(in crate::editor) fn ensure_inline_assist_bridge(
        &mut self,
        cwd: &Path,
    ) -> anyhow::Result<()> {
        if !self.is_learn_live_practice() {
            return self.ensure_agent_bridge(cwd);
        }
        anyhow::ensure!(
            self.learn_live_ready(),
            "run :tutorial ai-check before submitting live practice"
        );
        let permitted = self
            .learn_session
            .as_ref()
            .and_then(|session| session.workspace.as_ref())
            .is_some_and(|workspace| {
                cwd == workspace.root()
                    && self
                        .current_buffer()
                        .file
                        .as_deref()
                        .is_some_and(|file| workspace.permits_file(Path::new(file)))
            });
        anyhow::ensure!(
            permitted,
            "live practice context is outside the owned workspace"
        );
        // All other AI features stay disabled for the lesson. Only the explicit
        // inline submit may start this separate, read-only Codex bridge.
        let disabled = std::mem::replace(&mut self.config.disable_ai, false);
        let result = self.ensure_agent_bridge(cwd);
        self.config.disable_ai = disabled;
        result
    }

    pub(in crate::editor) fn observe_learn_live_result(
        &mut self,
        buffer: &mut RenderBuffer,
    ) -> anyhow::Result<()> {
        let received = self
            .inline_assist
            .as_ref()
            .is_some_and(|assist| assist.has_result && assist.result_request_id.is_some());
        let Some(live) = self
            .learn_session
            .as_mut()
            .and_then(|session| session.live.as_mut())
        else {
            return Ok(());
        };
        if received {
            live.received = true;
            self.observe_learn_action(&Action::Refresh, buffer)?;
        }
        Ok(())
    }

    #[inline(never)]
    pub(super) fn intercept_learn_live_action<'a>(
        &'a mut self,
        action: &'a Action,
        buffer: &'a mut RenderBuffer,
    ) -> BoxFuture<'a, anyhow::Result<bool>> {
        Box::pin(async move {
            if !self.is_learn_live_practice() {
                return Ok(false);
            }
            if matches!(action, Action::InlineAssist | Action::SubmitInlineAssist(_))
                && !self.learn_live_ready()
            {
                self.set_quiet_message(Some(
                    "run :tutorial ai-check before opening live inline assist".into(),
                ));
                self.render(buffer)?;
                return Ok(true);
            }
            if !matches!(action, Action::CheckLearnLiveAi) {
                return Ok(false);
            }
            let config = Config {
                disable_ai: self
                    .learn_session
                    .as_ref()
                    .unwrap()
                    .original_language
                    .original_ai_disabled(),
                agent: self.config.agent.clone(),
                ..Config::default()
            };
            let report =
                tokio::task::spawn_blocking(move || crate::agent_check::run(&config)).await?;
            self.learn_session
                .as_mut()
                .unwrap()
                .live
                .as_mut()
                .unwrap()
                .ready = report.production_ready;
            self.current_dialog = Some(Box::new(HoverInfo::new(
                self,
                format!("OFFLINE READINESS CHECK\n\n{}\n\nNo prompt has been sent. Authentication is checked by the first real request.\n\nSubmitting in this optional lesson sends your typed prompt and the owned practice code to your configured Codex service. Normal account usage may apply. Red supplies no original-project buffers or Agent history.\n\nClose this report to continue, or use :tutorial quit. The recorded AI track remains available offline.", report.format()),
                crate::ui::HoverInfoFormat::Plaintext,
                Vec::new(),
            ).with_label("Live AI readiness")));
            self.observe_learn_action(action, buffer)?;
            self.render(buffer)?;
            Ok(true)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn learn_live_ai_requires_readiness_and_restores_original_agent() {
        let config = Config {
            disable_ai: true,
            ..Config::default()
        };
        let client = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            client,
            100,
            30,
            config,
            Theme::default(),
            vec![Buffer::new(None, "private original".into())],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        let original_root = PathBuf::from("/original-agent-root");
        editor.agent_manager.set_root(Some(original_root.clone()));
        let mut buffer = RenderBuffer::new(100, 30, &Style::default());
        let mut runtime = Runtime::new();
        editor
            .start_learn_lesson(Lesson::TryLiveAi, &mut buffer, &mut runtime)
            .await
            .unwrap();
        let root = editor.inline_assist_workspace();
        assert_eq!(editor.agent_manager.root(), Some(root.as_path()));
        assert!(editor.ensure_inline_assist_bridge(&root).is_err());
        editor
            .execute(&Action::CheckLearnLiveAi, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert!(!editor.learn_live_ready());
        assert!(!editor.agent_manager.has_bridge());
        assert!(editor.config.disable_ai);
        let context = editor
            .inline_assist_context(TextRange::new(
                TextPosition::new(0, 0),
                TextPosition::new(1, 0),
            ))
            .unwrap();
        assert!(context.contains(crate::learn::AI_LINE.trim()));
        assert!(!context.contains("private original"));
        editor
            .finish_learn_lesson(&mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(editor.agent_manager.root(), Some(original_root.as_path()));
        assert_eq!(editor.current_buffer().contents(), "private original");
        assert!(editor.config.disable_ai);
        assert!(!root.exists());
    }
}
