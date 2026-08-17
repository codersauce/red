//! Local-only Git for disposable tutorial repositories.

use std::{path::Path, process::Stdio, time::Duration};

use anyhow::{Context, Result};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::PracticeWorkspace;

impl PracticeWorkspace {
    /// Start a repository without copying the user's templates or running hooks.
    pub async fn init_git(&self, files: &[(&str, &str)]) -> Result<()> {
        let template = tempfile::Builder::new()
            .prefix("red-learn-template-")
            .tempdir()?;
        for (name, contents) in files {
            self.write_fixture(name, contents)?;
        }
        let template_arg = format!("--template={}", template.path().display());
        self.git(&[
            "init",
            "--quiet",
            "--initial-branch=practice",
            &template_arg,
        ])
        .await?;
        let mut add = vec!["add", "--"];
        add.extend(files.iter().map(|(name, _)| *name));
        self.git(&add).await?;
        self.git(&["commit", "--quiet", "-m", "tutorial baseline"])
            .await?;
        Ok(())
    }

    /// Execute only editor-owned arguments, with every repository/config path
    /// pinned to this workspace. Never pass a prompt or arbitrary user command.
    pub async fn git(&self, args: &[&str]) -> Result<String> {
        self.git_with_input(args, None).await
    }

    async fn git_with_input(&self, args: &[&str], input: Option<&str>) -> Result<String> {
        let git_dir = self.path(".git");
        match std::fs::symlink_metadata(&git_dir) {
            Ok(metadata) => anyhow::ensure!(
                metadata.file_type().is_dir(),
                "invalid practice Git directory"
            ),
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && args.first() == Some(&"init") => {}
            Err(error) => return Err(error).context("practice Git directory is unavailable"),
        }
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("GIT_")
            {
                command.env_remove(key);
            }
        }
        let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
        command
            .current_dir(self.root())
            .env("GIT_DIR", &git_dir)
            .env("GIT_WORK_TREE", self.root())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", null)
            .env("GIT_CONFIG_SYSTEM", null)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_PAGER", "cat")
            .env("LC_ALL", "C")
            .arg("-c")
            .arg(format!(
                "core.hooksPath={}",
                git_dir.join("disabled-hooks").display()
            ))
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "tag.gpgsign=false",
                "-c",
                "user.name=Red Tutorial",
                "-c",
                "user.email=tutorial@localhost",
                "-c",
                "core.autocrlf=false",
                "-c",
                "protocol.allow=never",
            ])
            .args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(15), async {
            let mut child = command.spawn()?;
            if let Some(input) = input {
                let mut stdin = child
                    .stdin
                    .take()
                    .context("practice Git stdin is unavailable")?;
                stdin.write_all(input.as_bytes()).await?;
                stdin.shutdown().await?;
            }
            child.wait_with_output().await.map_err(anyhow::Error::from)
        })
        .await
        .context("practice Git command timed out")?
        .context("could not run Git; install Git to use this lesson")?;
        anyhow::ensure!(
            output.status.success(),
            "practice Git failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        String::from_utf8(output.stdout).context("practice Git output was not UTF-8")
    }

    pub async fn unstaged_diff(&self, name: &str) -> Result<String> {
        self.file_diff(name, false).await
    }

    pub async fn staged_diff(&self, name: &str) -> Result<String> {
        self.file_diff(name, true).await
    }

    async fn file_diff(&self, name: &str, staged: bool) -> Result<String> {
        anyhow::ensure!(
            self.permits_file(&self.path(name)) && Path::new(name).components().count() == 1,
            "invalid practice diff path"
        );
        let mut args = vec!["diff", "--no-ext-diff", "--no-textconv", "--no-color"];
        if staged {
            args.push("--cached");
        }
        args.extend(["--", name]);
        self.git(&args).await
    }

    /// Apply a parser-selected patch only to this repository's index.
    pub async fn apply_index_patch(&self, patch: &str, reverse: bool) -> Result<()> {
        anyhow::ensure!(
            !patch.is_empty() && patch.len() <= 65_536,
            "invalid practice patch size"
        );
        let mut args = vec!["apply", "--cached", "--unidiff-zero"];
        if reverse {
            args.push("--reverse");
        }
        args.push("-");
        self.git_with_input(&args, Some(patch)).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn learn_git_is_local_and_ignores_repository_hooks_and_signing() {
        let workspace = PracticeWorkspace::new().unwrap();
        workspace
            .init_git(&[("score.rs", "before\n")])
            .await
            .unwrap();
        assert!(workspace.git(&["remote"]).await.unwrap().is_empty());
        assert_eq!(
            workspace
                .git(&["rev-list", "--count", "HEAD"])
                .await
                .unwrap()
                .trim(),
            "1"
        );
        workspace.write_fixture("score.rs", "after\n").unwrap();
        assert!(workspace
            .unstaged_diff("score.rs")
            .await
            .unwrap()
            .contains("+after"));
        assert!(workspace.unstaged_diff("../outside").await.is_err());
        let hooks = workspace.path("hooks");
        std::fs::create_dir(&hooks).unwrap();
        let hook = hooks.join("pre-commit");
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        workspace
            .git(&["config", "core.hooksPath", hooks.to_str().unwrap()])
            .await
            .unwrap();
        workspace
            .git(&["config", "commit.gpgsign", "true"])
            .await
            .unwrap();
        workspace.git(&["add", "--", "score.rs"]).await.unwrap();
        workspace
            .git(&["commit", "--quiet", "-m", "local practice"])
            .await
            .unwrap();
        assert_eq!(
            workspace
                .git(&["rev-list", "--count", "HEAD"])
                .await
                .unwrap()
                .trim(),
            "2"
        );
        assert!(workspace.git(&["remote"]).await.unwrap().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn learn_git_rejects_a_redirected_git_directory() {
        let workspace = PracticeWorkspace::new().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), workspace.path(".git")).unwrap();
        assert!(workspace.git(&["status"]).await.is_err());
        assert!(workspace
            .init_git(&[("score.rs", "practice")])
            .await
            .is_err());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
