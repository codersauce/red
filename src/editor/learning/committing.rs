//! Local commit practice using Red's managed scratch-message behavior.

use super::*;
use crate::learn::{staging as fixture, LEARN_GIT_WORKSPACE};
use crate::plugin::workspace::WorkspaceAction;
use crate::plugin::{
    PanelSegment, WorkspaceConfig, WorkspaceDocument, WorkspaceDocumentLine, WorkspaceModel,
    WorkspaceRow,
};
use crate::ui::UiAction;

const FILE: &str = "score.rs";
const CONTEXT_MARKER: &str = "# --- Red commit context (not part of the commit message) ---";

pub(super) struct LearnCommitState {
    parent: String,
    commit_id: Option<String>,
    subject: String,
    patch: String,
    document: WorkspaceDocument,
    pub scratch_buffer: Option<BufferId>,
    draft: Option<String>,
}

impl LearnCommitState {
    pub async fn prepare(
        workspace: &PracticeWorkspace,
        runtime: &mut Runtime,
    ) -> anyhow::Result<Self> {
        workspace.init_git(&[(FILE, fixture::BASE)]).await?;
        workspace.write_fixture(FILE, fixture::INDEX)?;
        workspace.git(&["add", "--", FILE]).await?;
        workspace.write_fixture(FILE, fixture::WORKTREE)?;
        let patch = workspace.staged_diff(FILE).await?;
        Ok(Self {
            parent: workspace.git(&["rev-parse", "HEAD"]).await?.trim().into(),
            commit_id: None,
            subject: String::new(),
            document: runtime.git_detail_document(&patch, FILE, "staged")?,
            patch,
            scratch_buffer: None,
            draft: None,
        })
    }

    async fn verify_ready(&self, workspace: &PracticeWorkspace) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.commit_id.is_none(),
            "the practice commit already exists"
        );
        anyhow::ensure!(
            workspace.git(&["rev-parse", "HEAD"]).await?.trim() == self.parent,
            "practice HEAD changed; restart the lesson"
        );
        anyhow::ensure!(
            workspace
                .git(&["diff", "--cached", "--name-only", "--no-ext-diff"])
                .await?
                == "score.rs\n",
            "the practice index includes unexpected files; restart the lesson"
        );
        anyhow::ensure!(
            workspace.git(&["show", ":score.rs"]).await? == fixture::INDEX,
            "the practice index changed; restart the lesson"
        );
        anyhow::ensure!(
            workspace.permits_file(&workspace.path(FILE))
                && std::fs::read_to_string(workspace.path(FILE))? == fixture::WORKTREE,
            "the practice file changed; restart the lesson"
        );
        Ok(())
    }

    async fn refresh_commit(
        &mut self,
        workspace: &PracticeWorkspace,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let id = self
            .commit_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("no practice commit yet"))?;
        anyhow::ensure!(
            workspace.git(&["rev-parse", "HEAD"]).await?.trim() == id,
            "practice HEAD changed; restart the lesson"
        );
        anyhow::ensure!(
            workspace.git(&["rev-parse", "HEAD^"]).await?.trim() == self.parent,
            "unexpected practice commit parent"
        );
        anyhow::ensure!(
            workspace.git(&["show", "HEAD:score.rs"]).await? == fixture::INDEX,
            "practice commit contains unexpected changes"
        );
        anyhow::ensure!(
            workspace
                .git(&["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"])
                .await?
                == "score.rs\n",
            "practice commit includes unexpected files"
        );
        anyhow::ensure!(
            workspace
                .git(&["diff", "--cached", "--name-only", "--no-ext-diff"])
                .await?
                .is_empty(),
            "practice index has unexpected changes"
        );
        anyhow::ensure!(
            std::fs::read_to_string(workspace.path(FILE))? == fixture::WORKTREE,
            "practice working file changed"
        );
        self.subject = workspace
            .git(&["log", "-1", "--format=%s"])
            .await?
            .trim()
            .into();
        self.patch = workspace
            .git(&[
                "show",
                "--format=",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "HEAD",
                "--",
                FILE,
            ])
            .await?;
        self.document = runtime.git_detail_document(&self.patch, FILE, "committed")?;
        Ok(())
    }

    async fn create(
        &mut self,
        workspace: &PracticeWorkspace,
        runtime: &mut Runtime,
        text: &str,
    ) -> anyhow::Result<()> {
        let message = commit_message(text);
        if self.commit_id.is_none() {
            self.verify_ready(workspace).await?;
            self.commit_id = Some(workspace.commit_index(&message, &self.parent).await?);
        }
        self.refresh_commit(workspace, runtime).await
    }

    fn scratch_text(&self) -> String {
        if let Some(draft) = &self.draft {
            return draft.clone();
        }
        let patch = self
            .patch
            .lines()
            .map(|line| format!("# {line}\n"))
            .collect::<String>();
        format!("\n\n{CONTEXT_MARKER}\n# Write the commit message above. :w or :wq submits; :q cancels.\n#\n# Local tutorial repository; no remote or user hooks.\n# Staged: score fix. Unstaged: title change.\n#\n# Staged diff:\n{patch}")
    }

    fn inspected(&self, event: &Value) -> bool {
        if self.commit_id.is_none() || event["focus"] != "detail" || event["row"]["id"] != "commit"
        {
            return false;
        }
        let Ok(line) =
            serde_json::from_value::<WorkspaceDocumentLine>(event["detail_line"].clone())
        else {
            return false;
        };
        matches!(line.kind.as_str(), "added" | "removed") && self.document.lines.contains(&line)
    }

    pub fn model(&self) -> WorkspaceModel {
        let committed = self.commit_id.is_some();
        let header = self.commit_id.as_ref().map_or_else(
            || "Local practice · one staged fix · no remote".into(),
            |id| format!("Commit {} · {}", &id[..id.len().min(8)], self.subject),
        );
        WorkspaceModel {
            header: vec![segment(&header)],
            rows: vec![WorkspaceRow {
                id: if committed { "commit" } else { "staged" }.into(),
                selectable: true,
                depth: 0,
                path: Some(FILE.into()),
                segments: vec![segment(&format!(
                    "{}  {FILE}",
                    if committed { "Committed" } else { "Staged" }
                ))],
                right_segments: vec![segment(&format!(
                    "+{} −{}",
                    self.document.added, self.document.removed
                ))],
                data: json!({"path":FILE,"section":if committed {"committed"} else {"staged"}}),
            }],
            detail_document: Some(self.document.clone()),
            detail_title: FILE.into(),
            actions: if committed {
                vec![action("q", "back to code")]
            } else {
                vec![action("c", "commit"), action("q", "back to code")]
            },
            status: "The title change remains unstaged · nothing will be pushed".into(),
            ..WorkspaceModel::default()
        }
    }
}

fn commit_message(text: &str) -> String {
    text.split('\n')
        .take_while(|line| *line != CONTEXT_MARKER)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .into()
}
fn segment(text: &str) -> PanelSegment {
    PanelSegment {
        text: text.into(),
        style: None,
        semantic: None,
    }
}
fn action(key: &str, label: &str) -> WorkspaceAction {
    WorkspaceAction {
        hint: UiAction::new(key, key, label),
        focus: String::new(),
        sections: Vec::new(),
        selection: String::new(),
        change_only: false,
        hunk_only: false,
    }
}

impl Editor {
    pub(super) fn learn_commit_scratch_active(&self) -> bool {
        self.learn_session
            .as_ref()
            .and_then(|session| session.git.as_ref())
            .and_then(|git| git.committing.as_ref())
            .is_some_and(|state| state.scratch_buffer == Some(self.current_buffer().id()))
    }

    fn open_learn_commit_workspace(&mut self) {
        self.workspace_manager.open(
            LEARN_GIT_WORKSPACE.into(),
            WorkspaceConfig {
                title: "Git · practice commit".into(),
                notify_detail_navigation: true,
                ..WorkspaceConfig::default()
            },
        );
        self.refresh_learn_git_workspace();
    }

    async fn open_learn_commit_scratch(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        let session = self
            .learn_session
            .as_ref()
            .expect("commit lesson was checked");
        let state = session
            .git
            .as_ref()
            .and_then(|git| git.committing.as_ref())
            .expect("commit state was checked");
        anyhow::ensure!(
            state.scratch_buffer.is_none(),
            "finish the current commit message first"
        );
        state
            .verify_ready(session.workspace.as_ref().expect("Git workspace is owned"))
            .await?;
        let mut scratch = Buffer::new(Some("[Git Commit].gitcommit".into()), state.scratch_text());
        if let Some(language) = self.highlighter.language_id_for_name("gitcommit") {
            scratch.set_syntax_selection(SyntaxSelection::Language(language.into()));
        }
        let id = scratch.id();
        self.scratch_buffers.insert(
            id,
            ScratchBufferCommands {
                submit: Some("GitSubmitMessage".into()),
                cancel: Some("GitCancelMessage".into()),
            },
        );
        self.buffer_manager.push_buffer(scratch);
        let index = self.buffer_manager.len() - 1;
        let session = self
            .learn_session
            .as_mut()
            .expect("commit lesson was checked");
        session
            .git
            .as_mut()
            .and_then(|git| git.committing.as_mut())
            .expect("commit state was checked")
            .scratch_buffer = Some(id);
        session.step = PracticeStep::CommitWrite;
        self.current_dialog = None;
        self.workspace_manager.close(LEARN_GIT_WORKSPACE);
        self.mode = Mode::Normal;
        self.set_current_buffer(buffer, index).await
    }

    async fn close_learn_commit_scratch(
        &mut self,
        buffer: &mut RenderBuffer,
    ) -> anyhow::Result<()> {
        self.delete_current_buffer(buffer, true).await?;
        let session = self
            .learn_session
            .as_mut()
            .expect("commit lesson was checked");
        session
            .git
            .as_mut()
            .and_then(|git| git.committing.as_mut())
            .expect("commit state was checked")
            .scratch_buffer = None;
        let id = session.practice_buffer_id;
        let index = self
            .buffer_manager
            .iter()
            .position(|buffer| buffer.id() == id)
            .expect("practice source remains open");
        self.mode = Mode::Normal;
        self.set_current_buffer(buffer, index).await
    }

    pub(super) async fn intercept_learn_commit_action(
        &mut self,
        action: &Action,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<bool> {
        match action {
            Action::PluginCommand(name) if name == "GitDashboard" => {
                if self.learn_commit_scratch_active() {
                    self.set_quiet_message(Some(
                        "use :w to submit this commit message or :q to cancel it".into(),
                    ));
                } else {
                    self.open_learn_commit_workspace();
                }
            }
            Action::PluginCommand(name) if name == "LearnGitWriteMessage" => {
                if let Err(error) = self.open_learn_commit_scratch(buffer).await {
                    self.set_notification_message(
                        Severity::Error,
                        Some(format!("practice commit: {error:#}")),
                    );
                }
            }
            Action::PluginCommand(name)
                if matches!(name.as_str(), "GitSubmitMessage" | "GitCancelMessage") =>
            {
                if !self.learn_commit_scratch_active() {
                    return Ok(true);
                }
                let text = self.current_buffer().contents();
                if name == "GitSubmitMessage" && commit_message(&text).is_empty() {
                    self.set_quiet_message(Some(
                        "write a commit message above the context before submitting".into(),
                    ));
                    self.render(buffer)?;
                    return Ok(true);
                }
                let session = self
                    .learn_session
                    .as_mut()
                    .expect("commit lesson was checked");
                let state = session
                    .git
                    .as_mut()
                    .and_then(|git| git.committing.as_mut())
                    .expect("commit state was checked");
                state.draft = Some(text.clone());
                if name == "GitSubmitMessage" {
                    if let Err(error) = state
                        .create(
                            session.workspace.as_ref().expect("Git workspace is owned"),
                            runtime,
                            &text,
                        )
                        .await
                    {
                        self.set_notification_message(
                            Severity::Error,
                            Some(format!("practice commit: {error:#}")),
                        );
                        self.render(buffer)?;
                        return Ok(true);
                    }
                    state.draft = None;
                    session.step = PracticeStep::CommitInspect;
                } else {
                    session.step = PracticeStep::CommitOpen;
                }
                self.close_learn_commit_scratch(buffer).await?;
                self.open_learn_commit_workspace();
                if name == "GitSubmitMessage" {
                    self.set_notification_message(
                        Severity::Success,
                        Some("local practice commit created; nothing was pushed".into()),
                    );
                }
            }
            Action::NotifyPlugins(method, event)
                if method == &format!("workspace:event:{LEARN_GIT_WORKSPACE}") =>
            {
                let session = self
                    .learn_session
                    .as_mut()
                    .expect("commit lesson was checked");
                let state = session
                    .git
                    .as_mut()
                    .and_then(|git| git.committing.as_mut())
                    .expect("commit state was checked");
                if session.step == PracticeStep::CommitInspect && state.inspected(event) {
                    session.step = PracticeStep::CommitReturn;
                }
                match event["action"].as_str() {
                    Some("c") if state.commit_id.is_none() => {
                        self.current_dialog = Some(Box::new(
                            Picker::builder()
                                .title("Commit")
                                .items(vec!["Write message".into(), "Cancel".into()])
                                .status("Local practice · manual commit only")
                                .select_action(|item| {
                                    if item == "Write message" {
                                        Action::PluginCommand("LearnGitWriteMessage".into())
                                    } else {
                                        Action::CloseDialog
                                    }
                                })
                                .build(self),
                        ));
                    }
                    Some("q" | "escape") => {
                        if session.step == PracticeStep::CommitReturn {
                            match state
                                .refresh_commit(
                                    session.workspace.as_ref().expect("Git workspace is owned"),
                                    runtime,
                                )
                                .await
                            {
                                Ok(()) => {
                                    session.step = PracticeStep::Complete;
                                    if let Err(error) =
                                        self.preferences.complete_learn_lesson(session.lesson.id())
                                    {
                                        log!("could not persist Learn Red progress: {error}");
                                    }
                                }
                                Err(error) => self.set_notification_message(
                                    Severity::Error,
                                    Some(format!("practice commit: {error:#}")),
                                ),
                            }
                        }
                        self.workspace_manager.close(LEARN_GIT_WORKSPACE);
                    }
                    _ => {}
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

    #[tokio::test]
    async fn learn_commit_refuses_an_unexpected_staged_file() {
        let workspace = PracticeWorkspace::new().unwrap();
        let mut runtime = Runtime::new();
        let mut state = LearnCommitState::prepare(&workspace, &mut runtime)
            .await
            .unwrap();
        workspace.write_fixture("extra.rs", "unrelated\n").unwrap();
        workspace.git(&["add", "--", "extra.rs"]).await.unwrap();
        assert!(state
            .create(&workspace, &mut runtime, "fix(score): add points")
            .await
            .is_err());
        assert_eq!(
            workspace.git(&["rev-parse", "HEAD"]).await.unwrap().trim(),
            state.parent
        );
        assert!(state.commit_id.is_none());
    }

    #[test]
    fn learn_commit_message_strips_only_the_context_section() {
        assert_eq!(
            commit_message(&format!(
                "fix(score): add points\n\nWhy this is needed.\n\n{CONTEXT_MARKER}\n# diff"
            )),
            "fix(score): add points\n\nWhy this is needed."
        );
        assert_eq!(
            commit_message(&format!("mention {CONTEXT_MARKER} inline")),
            format!("mention {CONTEXT_MARKER} inline")
        );
    }

    #[tokio::test]
    async fn learn_commit_scratch_cancels_then_creates_and_inspects_one_local_commit() {
        let config = Config::default();
        let client = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            client,
            140,
            38,
            config,
            Theme::default(),
            vec![Buffer::new(None, "original".into())],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        let mut buffer = RenderBuffer::new(140, 38, &Style::default());
        let mut runtime = Runtime::new();
        editor
            .start_learn_lesson(Lesson::MakeALocalCommit, &mut buffer, &mut runtime)
            .await
            .unwrap();
        let root = editor
            .learn_session
            .as_ref()
            .unwrap()
            .workspace
            .as_ref()
            .unwrap()
            .root()
            .to_path_buf();
        let write = Action::PluginCommand("LearnGitWriteMessage".into());
        editor
            .execute(&write, &mut buffer, &mut runtime)
            .await
            .unwrap();
        let scratch = editor.current_buffer().id();
        assert!(editor.learn_commit_scratch_active());
        assert_eq!(
            editor.handle_command("w", &runtime),
            vec![Action::PluginCommand("GitSubmitMessage".into())]
        );
        assert_eq!(
            editor.handle_command("q", &runtime),
            vec![Action::PluginCommand("GitCancelMessage".into())]
        );
        editor
            .execute(&Action::Save, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert!(editor.learn_commit_scratch_active());
        assert!(editor
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("write a commit message")));
        editor
            .execute(
                &Action::InsertString("fix(score): add points".into()),
                &mut buffer,
                &mut runtime,
            )
            .await
            .unwrap();
        editor
            .execute(&Action::Quit(false), &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert!(!editor.scratch_buffers.contains_key(&scratch));
        assert!(!editor.learn_commit_scratch_active());
        assert_eq!(
            editor.learn_session.as_ref().unwrap().step,
            PracticeStep::CommitOpen
        );
        assert_eq!(
            editor
                .learn_session
                .as_ref()
                .unwrap()
                .workspace
                .as_ref()
                .unwrap()
                .git(&["rev-list", "--count", "HEAD"])
                .await
                .unwrap()
                .trim(),
            "1"
        );
        editor
            .execute(&write, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert!(editor
            .current_buffer()
            .contents()
            .starts_with("fix(score): add points"));
        editor
            .execute(&Action::Save, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(
            editor.learn_session.as_ref().unwrap().step,
            PracticeStep::CommitInspect
        );
        let session = editor.learn_session.as_ref().unwrap();
        let workspace = session.workspace.as_ref().unwrap();
        assert_eq!(
            workspace
                .git(&["rev-list", "--count", "HEAD"])
                .await
                .unwrap()
                .trim(),
            "2"
        );
        assert_eq!(
            workspace.git(&["show", "HEAD:score.rs"]).await.unwrap(),
            fixture::INDEX
        );
        assert_eq!(
            workspace
                .git(&["log", "-1", "--format=%s"])
                .await
                .unwrap()
                .trim(),
            "fix(score): add points"
        );
        assert!(workspace.git(&["remote"]).await.unwrap().is_empty());
        assert!(workspace
            .unstaged_diff(FILE)
            .await
            .unwrap()
            .contains("Scoreboard"));
        let state = session.git.as_ref().unwrap().committing.as_ref().unwrap();
        let line = state
            .document
            .lines
            .iter()
            .find(|line| line.kind == "added")
            .unwrap();
        let event =
            json!({"action":"down","row":{"id":"commit"},"focus":"detail","detail_line":line});
        let notify =
            |event| Action::NotifyPlugins(format!("workspace:event:{LEARN_GIT_WORKSPACE}"), event);
        editor
            .execute(&notify(event.clone()), &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(
            editor.learn_session.as_ref().unwrap().step,
            PracticeStep::CommitReturn
        );
        let mut close = event;
        close["action"] = "q".into();
        editor
            .execute(&notify(close), &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(
            editor.learn_session.as_ref().unwrap().step,
            PracticeStep::Complete
        );
        editor
            .finish_learn_lesson(&mut buffer, &mut runtime)
            .await
            .unwrap();
        assert!(!root.exists());
        assert_eq!(editor.current_buffer().contents(), "original");
        assert!(editor.scratch_buffers.is_empty());
    }
}
