//! Hunk staging through the real diff parser, workspace UI, and local index.

use super::*;
use crate::learn::{staging as fixture, LEARN_GIT_WORKSPACE};
use crate::plugin::workspace::WorkspaceAction;
use crate::plugin::{
    PanelSegment, WorkspaceConfig, WorkspaceDocument, WorkspaceDocumentLine, WorkspaceModel,
    WorkspaceRow,
};
use crate::ui::UiAction;

const FILE: &str = "score.rs";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Unstaged,
    Staged,
}

impl Section {
    fn id(self) -> &'static str {
        match self {
            Self::Unstaged => "unstaged",
            Self::Staged => "staged",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Unstaged => "Unstaged",
            Self::Staged => "Staged",
        }
    }
    fn from_event(event: &Value) -> Option<Self> {
        match event["row"]["id"].as_str()? {
            "unstaged" => Some(Self::Unstaged),
            "staged" => Some(Self::Staged),
            _ => None,
        }
    }
}

struct DiffSide {
    section: Section,
    patch: String,
    document: WorkspaceDocument,
}

pub(super) struct LearnStageState {
    sides: [DiffSide; 2],
    selected: Section,
    index_correct: bool,
    inspected: bool,
}

impl LearnStageState {
    pub async fn prepare(
        workspace: &PracticeWorkspace,
        runtime: &mut Runtime,
    ) -> anyhow::Result<Self> {
        workspace.init_git(&[(FILE, fixture::BASE)]).await?;
        workspace.write_fixture(FILE, fixture::WORKTREE)?;
        let mut state = Self {
            sides: [
                Self::side(workspace, runtime, Section::Unstaged).await?,
                Self::side(workspace, runtime, Section::Staged).await?,
            ],
            selected: Section::Unstaged,
            index_correct: false,
            inspected: false,
        };
        state.refresh(workspace, runtime).await?;
        let hunks = state.sides[0]
            .document
            .lines
            .iter()
            .filter_map(|line| line.hunk_id.as_deref())
            .collect::<HashSet<_>>();
        anyhow::ensure!(
            hunks.len() == 2,
            "practice staging fixture must have two hunks"
        );
        Ok(state)
    }

    async fn side(
        workspace: &PracticeWorkspace,
        runtime: &mut Runtime,
        section: Section,
    ) -> anyhow::Result<DiffSide> {
        let patch = if section == Section::Staged {
            workspace.staged_diff(FILE).await?
        } else {
            workspace.unstaged_diff(FILE).await?
        };
        let document = runtime.git_detail_document(&patch, FILE, section.id())?;
        Ok(DiffSide {
            section,
            patch,
            document,
        })
    }

    async fn refresh(
        &mut self,
        workspace: &PracticeWorkspace,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            workspace.permits_file(&workspace.path(FILE)),
            "practice file is unavailable"
        );
        anyhow::ensure!(
            std::fs::read_to_string(workspace.path(FILE))? == fixture::WORKTREE,
            "practice file changed on disk; restart the lesson"
        );
        self.sides = [
            Self::side(workspace, runtime, Section::Unstaged).await?,
            Self::side(workspace, runtime, Section::Staged).await?,
        ];
        self.index_correct = workspace.git(&["show", ":score.rs"]).await? == fixture::INDEX;
        if !self.index_correct {
            self.inspected = false;
        }
        Ok(())
    }

    fn side_for(&self, section: Section) -> &DiffSide {
        &self.sides[usize::from(section == Section::Staged)]
    }

    fn selected_line(&self, event: &Value, section: Section) -> Option<&WorkspaceDocumentLine> {
        if self.selected != section || event["focus"] != "detail" {
            return None;
        }
        let line: WorkspaceDocumentLine =
            serde_json::from_value(event["detail_line"].clone()).ok()?;
        self.side_for(section)
            .document
            .lines
            .iter()
            .find(|known| **known == line)
    }

    async fn apply_hunk(
        &mut self,
        workspace: &PracticeWorkspace,
        runtime: &mut Runtime,
        event: &Value,
        section: Section,
    ) -> anyhow::Result<()> {
        let hunk = self
            .selected_line(event, section)
            .and_then(|line| line.hunk_id.as_deref())
            .ok_or_else(|| anyhow::anyhow!("focus a current diff hunk first"))?;
        let side = self.side_for(section);
        let fresh = if section == Section::Staged {
            workspace.staged_diff(FILE).await?
        } else {
            workspace.unstaged_diff(FILE).await?
        };
        anyhow::ensure!(
            fresh == side.patch,
            "practice diff changed; refresh before staging"
        );
        let patch = runtime.git_dashboard_hunk(&fresh, FILE, hunk)?;
        anyhow::ensure!(
            !patch.is_empty(),
            "selected practice hunk is no longer available"
        );
        // The bundled dashboard terminates parser-selected patches with a newline.
        workspace
            .apply_index_patch(&format!("{patch}\n"), section == Section::Staged)
            .await?;
        self.inspected = false;
        self.refresh(workspace, runtime).await
    }

    fn observe(&mut self, event: &Value, section: Section) {
        if self.index_correct
            && section == Section::Staged
            && self
                .selected_line(event, section)
                .is_some_and(|line| matches!(line.kind.as_str(), "added" | "removed"))
        {
            self.inspected = true;
        }
        self.selected = section;
    }

    pub fn model(&self) -> WorkspaceModel {
        WorkspaceModel {
            header: vec![segment(
                "Local practice  ·  stage one focused change  ·  no remote",
            )],
            rows: self
                .sides
                .iter()
                .map(|side| WorkspaceRow {
                    id: side.section.id().into(),
                    selectable: true,
                    depth: 0,
                    path: Some(FILE.into()),
                    segments: vec![segment(&format!("{}  {FILE}", side.section.label()))],
                    right_segments: vec![segment(&format!(
                        "+{} −{}",
                        side.document.added, side.document.removed
                    ))],
                    data: json!({"path":FILE,"section":side.section.id()}),
                })
                .collect(),
            detail_document: Some(self.side_for(self.selected).document.clone()),
            detail_title: format!("{} · {FILE}", self.selected.label()),
            actions: vec![
                hunk_action("S", "stage hunk", Section::Unstaged),
                hunk_action("U", "unstage hunk", Section::Staged),
                ordinary_action("r", "refresh"),
                ordinary_action("q", "back to code"),
            ],
            status: if self.index_correct {
                "Only the score fix is staged"
            } else {
                "Stage the score fix; leave the title change unstaged"
            }
            .into(),
            ..WorkspaceModel::default()
        }
    }

    fn update_step(&self, step: &mut PracticeStep) {
        if matches!(
            *step,
            PracticeStep::StageChoose | PracticeStep::StageInspect | PracticeStep::StageReturn
        ) {
            *step = if !self.index_correct {
                PracticeStep::StageChoose
            } else if self.inspected {
                PracticeStep::StageReturn
            } else {
                PracticeStep::StageInspect
            };
        }
    }
}

fn segment(text: &str) -> PanelSegment {
    PanelSegment {
        text: text.into(),
        style: None,
        semantic: None,
    }
}
fn ordinary_action(key: &str, label: &str) -> WorkspaceAction {
    WorkspaceAction {
        hint: UiAction::new(key, key, label),
        focus: String::new(),
        sections: Vec::new(),
        selection: String::new(),
        change_only: false,
        hunk_only: false,
    }
}
fn hunk_action(key: &str, label: &str, section: Section) -> WorkspaceAction {
    WorkspaceAction {
        focus: "detail".into(),
        sections: vec![section.id().into()],
        hunk_only: true,
        ..ordinary_action(key, label)
    }
}

impl Editor {
    pub(super) async fn intercept_learn_staging_action(
        &mut self,
        action: &Action,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<bool> {
        match action {
            Action::PluginCommand(name) if name == "GitDashboard" => {
                let session = self
                    .learn_session
                    .as_mut()
                    .expect("staging lesson was checked");
                let state = session
                    .git
                    .as_mut()
                    .and_then(|git| git.staging.as_mut())
                    .expect("staging state was checked");
                state
                    .refresh(
                        session.workspace.as_ref().expect("Git workspace is owned"),
                        runtime,
                    )
                    .await?;
                state.selected = Section::Unstaged;
                if session.step == PracticeStep::StageOpen {
                    session.step = PracticeStep::StageChoose;
                }
                state.update_step(&mut session.step);
                self.workspace_manager.open(
                    LEARN_GIT_WORKSPACE.into(),
                    WorkspaceConfig {
                        title: "Git · practice staging".into(),
                        notify_detail_navigation: true,
                        ..WorkspaceConfig::default()
                    },
                );
                self.refresh_learn_git_workspace();
            }
            Action::NotifyPlugins(method, event)
                if method == &format!("workspace:event:{LEARN_GIT_WORKSPACE}") =>
            {
                let session = self
                    .learn_session
                    .as_mut()
                    .expect("staging lesson was checked");
                let state = session
                    .git
                    .as_mut()
                    .and_then(|git| git.staging.as_mut())
                    .expect("staging state was checked");
                let workspace = session.workspace.as_ref().expect("Git workspace is owned");
                let event_action = event["action"].as_str().unwrap_or_default();
                let section = Section::from_event(event);
                let result = match (event_action, section) {
                    ("S", Some(Section::Unstaged)) => {
                        state
                            .apply_hunk(workspace, runtime, event, Section::Unstaged)
                            .await
                    }
                    ("U", Some(Section::Staged)) => {
                        state
                            .apply_hunk(workspace, runtime, event, Section::Staged)
                            .await
                    }
                    ("r" | "q" | "escape", _) => state.refresh(workspace, runtime).await,
                    _ => Ok(()),
                };
                if let Some(section) = section {
                    state.observe(event, section);
                }
                state.update_step(&mut session.step);
                if matches!(event_action, "q" | "escape") {
                    self.workspace_manager.close(LEARN_GIT_WORKSPACE);
                    if result.is_ok()
                        && session.step == PracticeStep::StageReturn
                        && state.index_correct
                    {
                        session.step = PracticeStep::Complete;
                        if let Err(error) =
                            self.preferences.complete_learn_lesson(session.lesson.id())
                        {
                            log!("could not persist Learn Red progress: {error}");
                        }
                    }
                } else {
                    self.refresh_learn_git_workspace();
                }
                if let Err(error) = result {
                    self.set_notification_message(
                        Severity::Error,
                        Some(format!("practice Git: {error:#}")),
                    );
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

#[cfg(test)]
mod tests {
    use super::*;

    fn event(state: &LearnStageState, section: Section, text: &str, action: &str) -> Value {
        let line = state
            .side_for(section)
            .document
            .lines
            .iter()
            .find(|line| line.kind == "added" && line.text.contains(text))
            .unwrap();
        json!({"action":action,"row":{"id":section.id(),"path":FILE},"focus":"detail","detail_line":line})
    }

    #[tokio::test]
    async fn learn_staging_checks_the_real_index_and_can_recover_the_wrong_hunk() {
        let workspace = PracticeWorkspace::new().unwrap();
        let mut runtime = Runtime::new();
        let mut state = LearnStageState::prepare(&workspace, &mut runtime)
            .await
            .unwrap();
        let head = workspace.git(&["rev-parse", "HEAD"]).await.unwrap();
        let mut step = PracticeStep::StageChoose;
        let wrong = event(&state, Section::Unstaged, "Scoreboard", "S");
        state
            .apply_hunk(&workspace, &mut runtime, &wrong, Section::Unstaged)
            .await
            .unwrap();
        state.update_step(&mut step);
        assert_eq!(step, PracticeStep::StageChoose);
        assert!(!state.index_correct);
        assert!(workspace
            .git(&["show", ":score.rs"])
            .await
            .unwrap()
            .contains("Scoreboard"));
        assert!(state
            .apply_hunk(&workspace, &mut runtime, &wrong, Section::Unstaged)
            .await
            .is_err());

        state.selected = Section::Staged;
        let unstage = event(&state, Section::Staged, "Scoreboard", "U");
        state
            .apply_hunk(&workspace, &mut runtime, &unstage, Section::Staged)
            .await
            .unwrap();
        assert_eq!(
            workspace.git(&["show", ":score.rs"]).await.unwrap(),
            fixture::BASE
        );
        state.selected = Section::Unstaged;
        let correct = event(&state, Section::Unstaged, "score + points", "S");
        let mut forged = correct.clone();
        forged["detail_line"]["text"] = "forged".into();
        assert!(state
            .apply_hunk(&workspace, &mut runtime, &forged, Section::Unstaged)
            .await
            .is_err());
        state
            .apply_hunk(&workspace, &mut runtime, &correct, Section::Unstaged)
            .await
            .unwrap();
        state.update_step(&mut step);
        assert_eq!(step, PracticeStep::StageInspect);
        assert_eq!(
            workspace.git(&["show", ":score.rs"]).await.unwrap(),
            fixture::INDEX
        );
        assert!(workspace
            .unstaged_diff(FILE)
            .await
            .unwrap()
            .contains("Scoreboard"));
        let inspect = event(&state, Section::Staged, "score + points", "move_down");
        state.observe(&inspect, Section::Staged);
        assert!(!state.inspected); // It was not the visible document yet.
        state.observe(&inspect, Section::Staged);
        state.update_step(&mut step);
        assert_eq!(step, PracticeStep::StageReturn);
        assert_eq!(workspace.git(&["rev-parse", "HEAD"]).await.unwrap(), head);
        assert_eq!(
            std::fs::read_to_string(workspace.path(FILE)).unwrap(),
            fixture::WORKTREE
        );
        assert!(workspace.git(&["remote"]).await.unwrap().is_empty());
    }
}
