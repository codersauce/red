//! Exercises the bundled Git scheduler with gated processes and injected timers.
//! The fake executable avoids changing PATH and never scans a real repository.

use super::tests::{drain_requests, load_git_runtime_source, pump_process_events};
use super::*;
use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const GOOD_STATUS: &[u8] = b"# branch.oid abcd\0# branch.head main\0? pending.txt\0";

struct GitHarness {
    runtime: Runtime,
    root: tempfile::TempDir,
    command: std::path::PathBuf,
    dashboard: Option<WorkspaceModel>,
}

impl GitHarness {
    async fn new() -> Self {
        drain_requests();
        let root = tempfile::tempdir().unwrap();
        prepare(root.path());
        let command = root.path().join("fake-git");
        fs::write(
            &command,
            r#"#!/bin/sh
case "$1" in
    rev-parse)
        if [ "$2" = --show-toplevel ]; then pwd; else printf '.git\n'; fi
        ;;
    status)
        printf 'start\n' >> starts
        while [ ! -f release ]; do sleep 0.01; done
        cat response
        printf 'complete\n' >> completions
        exit "$(cat exit-code)"
        ;;
    diff)
        if [ "$2" = --numstat ]; then printf '%s\n' "$*" >> stats; fi
        ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).unwrap();
        let source = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/plugins/git.hk"))
            .unwrap()
            .replace(
                "command: \"git\"",
                &format!("command: {}", serde_json::to_string(&command).unwrap()),
            )
            + r#"
#[red::on("test:age")]
fn age_status(event: Json) {
    let now: i64 = red::execute("MonotonicTime");
    red::state_patch(GitPluginState { status_started_ms: now - event.ms });
}
#[red::on("test:root")]
fn change_root(event: GitStringValueEvent) { git_cwd_loaded(event); }
"#;
        let runtime =
            load_git_runtime_source(root.path(), &source, command.to_str().unwrap()).await;
        Self {
            runtime,
            root,
            command,
            dashboard: None,
        }
    }

    fn state(&self) -> serde_json::Value {
        let inner = self.runtime.inner.lock().unwrap();
        value_to_json(inner.host.policy().typed_states.get("git").unwrap())
    }

    fn count(&self, file: &str) -> usize {
        fs::read_to_string(self.root.path().join(file))
            .unwrap_or_default()
            .lines()
            .count()
    }

    async fn notify(&mut self, event: &str, payload: serde_json::Value) {
        self.runtime.notify(event, payload).await.unwrap();
    }

    async fn timer(&mut self, id: &str) {
        self.notify("timeout:callback", json!({"timer_id": id}))
            .await;
    }

    async fn refresh(&mut self) {
        self.runtime.execute_command("GitRefresh").await.unwrap();
    }

    async fn wait(&mut self, predicate: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            pump_process_events(&mut self.runtime).await.unwrap();
            while let Some(request) = self.runtime.try_recv_request() {
                if let PluginRequest::UpdateWorkspace { id, model } = request {
                    assert_eq!(id, "git-dashboard");
                    self.dashboard = Some(model);
                }
            }
            if predicate(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "Git scheduler did not settle: {}",
                self.state()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    async fn release(&mut self) {
        fs::write(self.root.path().join("release"), "").unwrap();
        self.wait(|h| h.state()["process"] == "").await;
    }
}

impl Drop for GitHarness {
    fn drop(&mut self) {
        // Release a blocked fake even when an assertion unwinds the runtime.
        let _ = fs::write(self.root.path().join("release"), "");
    }
}

fn prepare(root: &Path) {
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join("response"), GOOD_STATUS).unwrap();
    fs::write(root.join("exit-code"), "0").unwrap();
}

#[tokio::test]
async fn git_refresh_slow_scan_survives_poll_ticks_and_finishes() {
    let mut h = GitHarness::new().await;
    let stale_poll = h.state()["poll_timer"].as_str().unwrap().to_owned();
    h.refresh().await;
    h.wait(|h| h.count("starts") == 1).await;
    let process = h.state()["process"].clone();
    h.notify("test:age", json!({"ms": 20000})).await;
    for _ in 0..4 {
        h.timer(&stale_poll).await;
    }
    assert_eq!(h.state()["process"], process);
    assert_eq!(h.state()["poll_timer"], "");
    assert_eq!(h.state()["refresh_pending"], false);
    assert_eq!(h.count("starts"), 1);
    h.release().await;
    assert_eq!(h.count("completions"), 1);
    assert_eq!(h.state()["state"]["head"], "main");
    assert!(h.state()["poll_delay_ms"].as_i64().unwrap() >= 180000);
    assert_ne!(h.state()["poll_timer"], "");
    h.runtime.deactivate_all().await.unwrap();
}

#[tokio::test]
async fn git_refresh_coalesces_explicit_and_debounced_requests() {
    let mut h = GitHarness::new().await;
    h.refresh().await;
    h.wait(|h| h.count("starts") == 1).await;
    let process = h.state()["process"].clone();
    for _ in 0..20 {
        h.refresh().await;
        h.notify("file:saved", json!({})).await;
    }
    let timer = h.state()["refresh_timer"].as_str().unwrap().to_owned();
    h.timer(&timer).await;
    assert_eq!(h.state()["process"], process);
    assert_eq!(h.state()["refresh_pending"], true);
    assert_eq!(h.count("starts"), 1);
    h.release().await;
    assert_eq!(h.count("starts"), 2);
    assert_eq!(h.count("completions"), 2);
    assert_eq!(h.state()["refresh_pending"], false);
    h.timer(&timer).await;
    assert_eq!(h.count("starts"), 2);
    h.runtime.deactivate_all().await.unwrap();
}

#[tokio::test]
async fn git_refresh_idle_polls_back_off_and_explicit_refresh_is_immediate() {
    let mut h = GitHarness::new().await;
    h.refresh().await;
    h.release().await;
    let mut delay = h.state()["poll_delay_ms"].as_i64().unwrap();
    assert!(delay >= 5000);
    for _ in 0..5 {
        let minimum_delay = (delay * 2).min(60000);
        let timer = h.state()["poll_timer"].as_str().unwrap().to_owned();
        h.timer(&timer).await;
        h.wait(|h| h.state()["process"] == "").await;
        delay = h.state()["poll_delay_ms"].as_i64().unwrap();
        assert!(delay >= minimum_delay);
    }
    let starts = h.count("starts");
    h.refresh().await;
    h.wait(|h| h.state()["process"] == "").await;
    assert_eq!(h.count("starts"), starts + 1);
    assert!(h.state()["poll_delay_ms"].as_i64().unwrap() >= 5000);
    h.runtime.deactivate_all().await.unwrap();
}

#[tokio::test]
async fn git_refresh_only_scans_statistics_for_sections_with_tracked_changes() {
    let mut h = GitHarness::new().await;
    h.runtime.execute_command("GitDashboard").await.unwrap();
    h.release().await;
    assert_eq!(
        h.count("stats"),
        0,
        "untracked files have no diff statistics"
    );
    for (code, cached, count) in [("M.", true, 1), (".M", false, 2)] {
        fs::write(
            h.root.path().join("response"),
            format!(
                "# branch.head main\0\
                1 {code} N... 100644 100644 100644 abc def tracked.txt\0"
            ),
        )
        .unwrap();
        h.refresh().await;
        h.wait(|h| h.count("stats") == count && h.state()["stats_process"] == "")
            .await;
        let commands = fs::read_to_string(h.root.path().join("stats")).unwrap();
        assert_eq!(
            commands.lines().last().unwrap().contains("--cached"),
            cached
        );
    }
    fs::write(h.root.path().join("response"), "# branch.head main\0").unwrap();
    h.refresh().await;
    h.wait(|h| h.state()["process"] == "").await;
    assert_eq!(h.state()["stats_process"], "");
    assert_eq!(
        h.count("stats"),
        2,
        "a clean status needs no statistics scan"
    );
    h.runtime.deactivate_all().await.unwrap();
}

#[tokio::test]
async fn git_refresh_failed_output_preserves_the_last_successful_status() {
    let mut h = GitHarness::new().await;
    h.runtime.execute_command("GitDashboard").await.unwrap();
    h.release().await;
    let good = h.state()["state"].clone();
    let output = h.state()["status_output"].clone();
    let previous_delay = h.state()["poll_delay_ms"].as_i64().unwrap();
    fs::write(h.root.path().join("response"), "# branch.head wrong\0").unwrap();
    fs::write(h.root.path().join("exit-code"), "1").unwrap();
    h.refresh().await;
    h.wait(|h| h.state()["process"] == "").await;
    assert_eq!(h.state()["state"], good);
    assert_eq!(h.state()["status_output"], output);
    assert!(h.state()["poll_delay_ms"].as_i64().unwrap() >= (previous_delay * 2).min(60000));
    let dashboard = h.dashboard.as_ref().unwrap();
    assert!(dashboard.status.starts_with("Git status stale:"));
    assert!(dashboard
        .rows
        .iter()
        .any(|row| row.path.as_deref() == Some("pending.txt")));
    fs::write(h.root.path().join("exit-code"), "0").unwrap();
    h.refresh().await;
    let process = h.state()["process"].as_str().unwrap().to_owned();
    h.notify(
        &format!("process:{process}"),
        json!({
            "type": "error", "plugin_name": "git", "process_id": process,
            "message": "raw process output exceeds the limit",
        }),
    )
    .await;
    h.wait(|h| h.state()["process"] == "").await;
    assert_eq!(h.state()["state"], good);
    assert!(h
        .dashboard
        .as_ref()
        .unwrap()
        .status
        .contains("raw process output"));
    fs::write(h.root.path().join("response"), GOOD_STATUS).unwrap();
    // Recovery via a background poll must remove the stale banner even if the
    // successful output is identical to the cached result. A failed launch
    // during recovery must not erase the error that makes that repaint needed.
    let executable = fs::read(&h.command).unwrap();
    fs::remove_file(&h.command).unwrap();
    let poll = h.state()["poll_timer"].as_str().unwrap().to_owned();
    assert!(h
        .runtime
        .notify("timeout:callback", json!({"timer_id": poll}))
        .await
        .is_err());
    assert_ne!(h.state()["status_error"], "");
    fs::write(&h.command, executable).unwrap();
    fs::set_permissions(&h.command, fs::Permissions::from_mode(0o755)).unwrap();
    let poll = h.state()["poll_timer"].as_str().unwrap().to_owned();
    h.timer(&poll).await;
    h.wait(|h| h.state()["process"] == "").await;
    assert_eq!(h.state()["state"], good);
    assert_eq!(h.state()["status_error"], "");
    assert!(!h.dashboard.as_ref().unwrap().status.contains("stale"));
    h.runtime.deactivate_all().await.unwrap();
}

#[tokio::test]
async fn git_refresh_spawn_failure_keeps_a_retry_timer() {
    let mut h = GitHarness::new().await;
    h.refresh().await;
    h.release().await;
    let good = h.state()["state"].clone();
    let executable = fs::read(&h.command).unwrap();
    fs::remove_file(&h.command).unwrap();
    assert!(h.runtime.execute_command("GitRefresh").await.is_err());
    assert_eq!(h.state()["process"], "");
    assert_ne!(h.state()["poll_timer"], "");
    assert_eq!(h.state()["state"], good);
    fs::write(&h.command, executable).unwrap();
    fs::set_permissions(&h.command, fs::Permissions::from_mode(0o755)).unwrap();
    let poll = h.state()["poll_timer"].as_str().unwrap().to_owned();
    h.timer(&poll).await;
    h.wait(|h| h.state()["process"] == "").await;
    assert_eq!(h.count("completions"), 2);
    h.runtime.deactivate_all().await.unwrap();
}

#[tokio::test]
async fn git_refresh_repository_change_and_shutdown_discard_stale_events() {
    let mut h = GitHarness::new().await;
    let old_poll = h.state()["poll_timer"].as_str().unwrap().to_owned();
    h.refresh().await;
    h.wait(|h| h.count("starts") == 1).await;
    let old = h.state()["process"].as_str().unwrap().to_owned();
    let next = tempfile::tempdir().unwrap();
    prepare(next.path());
    fs::write(next.path().join("release"), "").unwrap();
    fs::write(next.path().join("response"), "# branch.head next\0").unwrap();
    h.notify("test:root", json!({"value": next.path()})).await;
    h.refresh().await;
    h.notify(
        &format!("process:{old}"),
        json!({
            "type": "stdout", "plugin_name": "git", "process_id": old,
            "line": "# branch.head stale\0",
        }),
    )
    .await;
    h.notify(
        &format!("process:{old}"),
        json!({
            "type": "exit", "plugin_name": "git", "process_id": old,
            "code": 1,
        }),
    )
    .await;
    h.wait(|h| h.state()["state"]["head"] == "next").await;
    let reported_root = h.state()["root"].as_str().unwrap().to_owned();
    assert_eq!(
        fs::canonicalize(reported_root).unwrap(),
        fs::canonicalize(next.path()).unwrap()
    );
    h.timer(&old_poll).await;
    let poll = h.state()["poll_timer"].as_str().unwrap().to_owned();
    h.runtime.deactivate_all().await.unwrap();
    h.timer(&poll).await;
    h.wait(|h| {
        h.runtime
            .inner
            .lock()
            .unwrap()
            .host
            .process_manager
            .active_process_count("git")
            == 0
    })
    .await;
}

#[tokio::test]
async fn git_refresh_shutdown_cancels_a_blocked_scan_and_pending_refresh() {
    let mut h = GitHarness::new().await;
    h.refresh().await;
    h.wait(|h| h.count("starts") == 1).await;
    h.refresh().await;
    h.notify("file:saved", json!({})).await;
    assert_eq!(h.state()["refresh_pending"], true);
    h.runtime.deactivate_all().await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        pump_process_events(&mut h.runtime).await.unwrap();
        let active = {
            let inner = h.runtime.inner.lock().unwrap();
            assert!(inner.host.pending_timeouts.is_empty());
            inner.host.process_manager.active_process_count("git")
        };
        if active == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "shutdown left a Git process alive"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(h.count("starts"), 1);
    assert_eq!(h.count("completions"), 0);
}
