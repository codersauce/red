//! Read-only review of an owned repository through the real Git workspace UI.

use super::*;
use crate::learn::{
    AGENT_EXAMPLE, AGENT_EXAMPLE_FIXED, AI_CONTENTS, AI_FIXED_CONTENTS, LEARN_GIT_WORKSPACE,
};
use crate::plugin::{
    PanelSegment, WorkspaceConfig, WorkspaceDocument, WorkspaceModel, WorkspaceRow,
};

pub(super) struct LearnGitState {
    documents: Vec<WorkspaceDocument>,
    selected: String,
    inspected: HashSet<String>,
    pub(super) staging: Option<staging::LearnStageState>,
}

impl LearnGitState {
    pub async fn prepare(
        workspace: &PracticeWorkspace,
        runtime: &mut Runtime,
        lesson: Lesson,
    ) -> anyhow::Result<Self> {
        if lesson == Lesson::StageTheRightHunk {
            return Ok(Self {
                documents: Vec::new(),
                selected: String::new(),
                inspected: HashSet::new(),
                staging: Some(staging::LearnStageState::prepare(workspace, runtime).await?),
            });
        }
        workspace
            .init_git(&[("score.rs", AI_CONTENTS), ("example.rs", AGENT_EXAMPLE)])
            .await?;
        workspace.write_fixture("score.rs", AI_FIXED_CONTENTS)?;
        workspace.write_fixture("example.rs", AGENT_EXAMPLE_FIXED)?;
        let mut documents = Vec::new();
        for name in ["score.rs", "example.rs"] {
            let patch = workspace.unstaged_diff(name).await?;
            let document = runtime.git_detail_document(&patch, name, "unstaged")?;
            anyhow::ensure!(
                document.added > 0 && document.removed > 0,
                "missing practice diff for {name}"
            );
            documents.push(document);
        }
        Ok(Self {
            documents,
            selected: "score.rs".into(),
            inspected: HashSet::new(),
            staging: None,
        })
    }

    fn model(&self) -> WorkspaceModel {
        if let Some(staging) = &self.staging {
            return staging.model();
        }
        WorkspaceModel {
            header: vec![segment(
                "Read-only practice  ·  2 unstaged files  ·  no remote",
            )],
            rows: self
                .documents
                .iter()
                .map(|document| WorkspaceRow {
                    id: document.path.clone(),
                    selectable: true,
                    depth: 0,
                    path: Some(document.path.clone()),
                    segments: vec![segment(&format!("M  {}", document.path))],
                    right_segments: vec![segment(&format!(
                        "+{} −{}",
                        document.added, document.removed
                    ))],
                    data: serde_json::json!({"path": document.path, "section": "unstaged"}),
                })
                .collect(),
            detail_document: self
                .documents
                .iter()
                .find(|document| document.path == self.selected)
                .cloned(),
            detail_title: self.selected.clone(),
            footer: vec![segment("Tab Files / diff  ·  j/k Move  ·  q Back to code")],
            status: "Read-only practice · staging and commit come next".into(),
            ..WorkspaceModel::default()
        }
    }

    fn observe(&mut self, event: &serde_json::Value) {
        let Some(path) = event["row"]["path"].as_str() else {
            return;
        };
        let Some(document) = self.documents.iter().find(|document| document.path == path) else {
            return;
        };
        // The event's line must belong to the document that was actually shown,
        // not a stale preview from the previously selected row.
        if self.selected == path && event["focus"] == "detail" {
            let line = &event["detail_line"];
            if document.lines.iter().any(|known| {
                line["id"] == known.id
                    && line["kind"] == known.kind
                    && matches!(known.kind.as_str(), "added" | "removed")
            }) {
                self.inspected.insert(path.into());
            }
        }
        self.selected = path.into();
    }
}

fn segment(text: &str) -> PanelSegment {
    PanelSegment {
        text: text.into(),
        style: None,
        semantic: None,
    }
}

impl Editor {
    pub(in crate::editor) fn learn_git_workspace_open(&self) -> bool {
        self.learn_session
            .as_ref()
            .is_some_and(|session| session.git.is_some())
            && self.workspace_manager.is_active()
    }

    pub(in crate::editor) fn learn_workspace_height(&self, height: usize) -> usize {
        if self.learn_git_workspace_open() {
            height.saturating_sub(CoachLayout::for_panel(height).bottom + 1)
        } else {
            height
        }
    }

    pub(in crate::editor) fn render_learn_git_workspace(&self, buffer: &mut RenderBuffer) -> bool {
        if !self.learn_git_workspace_open() {
            return false;
        }
        let height = self.learn_workspace_height(buffer.height);
        let mut workspace = RenderBuffer::new(buffer.width, height, &Style::default());
        self.workspace_manager
            .render(&mut workspace, &self.theme, self.picker_icons());
        buffer.cells[..workspace.cells.len()].clone_from_slice(&workspace.cells);
        buffer.shortcut_help_regions = workspace.shortcut_help_regions;
        true
    }

    pub(super) fn refresh_learn_git_workspace(&mut self) {
        if let Some(git) = self
            .learn_session
            .as_ref()
            .and_then(|session| session.git.as_ref())
        {
            let model = git.model();
            self.workspace_manager.update_with_registry(
                LEARN_GIT_WORKSPACE,
                model,
                &self.theme,
                &self.language_registry(),
            );
        }
    }

    #[inline(never)]
    pub(in crate::editor) fn intercept_learn_git_action<'a>(
        &'a mut self,
        action: &'a Action,
        buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> BoxFuture<'a, anyhow::Result<bool>> {
        Box::pin(async move {
            if self
                .learn_session
                .as_ref()
                .and_then(|session| session.git.as_ref())
                .is_some_and(|git| git.staging.is_some())
            {
                self.intercept_learn_staging_action(action, buffer, runtime)
                    .await
            } else {
                self.intercept_learn_review_action(action, buffer)
            }
        })
    }

    fn intercept_learn_review_action(
        &mut self,
        action: &Action,
        buffer: &mut RenderBuffer,
    ) -> anyhow::Result<bool> {
        if self
            .learn_session
            .as_ref()
            .is_none_or(|session| session.git.is_none())
        {
            return Ok(false);
        }
        match action {
            Action::PluginCommand(name) if name == "GitDashboard" => {
                if let Some(git) = self
                    .learn_session
                    .as_mut()
                    .and_then(|session| session.git.as_mut())
                {
                    // Opening a workspace selects its first row. Its preview
                    // must agree even when the previous review ended elsewhere.
                    git.selected = "score.rs".into();
                }
                self.workspace_manager.open(
                    LEARN_GIT_WORKSPACE.into(),
                    WorkspaceConfig {
                        title: "Git · practice review".into(),
                        notify_detail_navigation: true,
                        ..WorkspaceConfig::default()
                    },
                );
                self.refresh_learn_git_workspace();
                if let Some(session) = self.learn_session.as_mut() {
                    if session.step == PracticeStep::GitOpen {
                        session.step = PracticeStep::GitScore;
                    }
                }
            }
            Action::NotifyPlugins(method, event)
                if method == &format!("workspace:event:{LEARN_GIT_WORKSPACE}") =>
            {
                let Some(session) = self.learn_session.as_mut() else {
                    return Ok(false);
                };
                let git = session.git.as_mut().expect("Git lesson was checked above");
                let previous = git.selected.clone();
                git.observe(event);
                if session.step == PracticeStep::GitScore && git.inspected.contains("score.rs") {
                    session.step = PracticeStep::GitExample;
                }
                if session.step == PracticeStep::GitExample && git.inspected.contains("example.rs")
                {
                    session.step = PracticeStep::GitReturn;
                }
                if matches!(event["action"].as_str(), Some("q" | "escape")) {
                    self.workspace_manager.close(LEARN_GIT_WORKSPACE);
                    if session.step == PracticeStep::GitReturn {
                        session.step = PracticeStep::Complete;
                        if let Err(error) =
                            self.preferences.complete_learn_lesson(session.lesson.id())
                        {
                            log!("could not persist Learn Red progress: {error}");
                        }
                    }
                } else if git.selected != previous {
                    self.refresh_learn_git_workspace();
                }
            }
            _ => return Ok(false),
        }
        self.apply_panel_layout();
        self.force_full_redraw = true;
        self.render(buffer)?;
        Ok(true)
    }
}
