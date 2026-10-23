//! Editor integration for the standalone `red update` worker.

use std::{
    fs::{self, OpenOptions},
    io::{Read as _, Seek as _, Write as _},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use sha2::{Digest as _, Sha256};
use tokio::sync::oneshot;

use super::Editor;
use crate::{
    config::UpdateMode,
    log,
    notification::{
        AttentionPolicy, MessageContent, Notice, NotificationId, NotificationSource,
        NotificationTime, ProgressOutcome, ProgressPriority, Severity,
    },
    update,
};

const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const STARTUP_DELAY: Duration = Duration::from_secs(1);
const UPDATE_NOTICE_KEY: &str = "red-update-available";

#[derive(Default)]
pub(super) struct UpdateState {
    executable: Option<PathBuf>,
    check_stamp: Option<PathBuf>,
    check_after: Option<Instant>,
    pending: Option<PendingUpdate>,
}

struct PendingUpdate {
    kind: WorkerKind,
    notification: Option<NotificationId>,
    result: oneshot::Receiver<WorkerResult>,
}

#[derive(Clone, Copy)]
enum WorkerKind {
    /// Scheduled `red update --check`.
    Check,
    /// Scheduled `red update --auto`.
    Auto,
    /// User-requested `red update`.
    Install,
}

struct WorkerResult {
    success: bool,
    message: String,
}

impl Editor {
    /// Enables production update scheduling after editor construction and startup setup.
    pub fn enable_update_checks(&mut self, cache: PathBuf) {
        self.update.executable = std::env::current_exe()
            .and_then(|path| path.canonicalize())
            .ok();
        if self.config.updates.mode == UpdateMode::Off {
            return;
        }
        self.update.check_stamp = self.update.executable.as_ref().map(|executable| {
            let digest = Sha256::digest(executable.as_os_str().as_encoded_bytes());
            cache.join(format!("{digest:x}.stamp"))
        });
        self.update.check_after = Some(Instant::now() + STARTUP_DELAY);
    }

    pub(super) fn start_red_update(&mut self) {
        if self.update.pending.is_some() {
            self.set_legacy_message(Some("A Red update is already running".to_string()));
            return;
        }
        let Ok(executable) = self.update_executable() else {
            self.set_notification_message(
                Severity::Error,
                Some("Could not locate the running Red executable".to_string()),
            );
            return;
        };
        let notification = match self.notifications.begin_progress(
            NotificationSource::Editor,
            "red-update",
            MessageContent::new("Updating Red"),
            ProgressPriority::UserInitiated,
            NotificationTime::now(),
        ) {
            Ok(notification) => notification,
            Err(error) => {
                self.set_notification_message(
                    Severity::Error,
                    Some(format!("Could not start Red update: {error}")),
                );
                return;
            }
        };
        self.update.pending = Some(spawn_worker(
            executable,
            WorkerKind::Install,
            Some(notification),
            None,
        ));
    }

    pub(super) fn service_red_update(&mut self) -> bool {
        self.start_background_check_if_due();
        let Some(pending) = self.update.pending.as_mut() else {
            return false;
        };
        let result = match pending.result.try_recv() {
            Ok(result) => result,
            Err(oneshot::error::TryRecvError::Empty) => return false,
            Err(oneshot::error::TryRecvError::Closed) => WorkerResult {
                success: false,
                message: "Red update worker stopped unexpectedly".to_string(),
            },
        };
        let pending = self.update.pending.take().expect("pending update exists");
        match pending.kind {
            WorkerKind::Install => self.finish_manual_update(pending.notification, result),
            WorkerKind::Check | WorkerKind::Auto => {
                self.finish_background_update(pending.kind, result)
            }
        }
        true
    }

    fn update_executable(&mut self) -> anyhow::Result<PathBuf> {
        if let Some(executable) = &self.update.executable {
            return Ok(executable.clone());
        }
        let executable = std::env::current_exe()?.canonicalize()?;
        self.update.executable = Some(executable.clone());
        Ok(executable)
    }

    fn start_background_check_if_due(&mut self) {
        if self.update.pending.is_some()
            || !self
                .update
                .check_after
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return;
        }
        self.update.check_after = Some(Instant::now() + CHECK_INTERVAL);
        let Some(stamp) = self.update.check_stamp.clone() else {
            return;
        };
        let Ok(executable) = self.update_executable() else {
            return;
        };
        let kind = match self.config.updates.mode {
            UpdateMode::Auto => WorkerKind::Auto,
            UpdateMode::Notify | UpdateMode::Off => WorkerKind::Check,
        };
        self.update.pending = Some(spawn_worker(executable, kind, None, Some(stamp)));
    }

    fn finish_manual_update(&mut self, notification: Option<NotificationId>, result: WorkerResult) {
        let Some(notification) = notification else {
            return;
        };
        let outcome = if result.success {
            ProgressOutcome::Succeeded
        } else {
            ProgressOutcome::Failed
        };
        let _ = self.notifications.finish_progress(
            notification,
            outcome,
            MessageContent::new(&result.message).with_details(&result.message),
            NotificationTime::now(),
        );
        self.last_error = (!result.success).then_some(result.message);
    }

    fn finish_background_update(&mut self, kind: WorkerKind, result: WorkerResult) {
        let notice = match background_outcome(kind, &result) {
            BackgroundOutcome::Quiet => {
                if !result.message.is_empty() {
                    log!("Red update: {}", result.message);
                }
                return;
            }
            BackgroundOutcome::Available => {
                self.set_quiet_message(Some(format!("{}. Run :update to install.", result.message)));
                return;
            }
            BackgroundOutcome::Installed => {
                Notice::new(NotificationSource::Editor, Severity::Info, &result.message)
                    .with_key(UPDATE_NOTICE_KEY)
                    .with_attention(AttentionPolicy::RequiresAcknowledgment)
                    .with_details(format!(
                        "This session keeps running Red {} until you restart it. Set [updates] mode = \"notify\" to install releases yourself.",
                        env!("CARGO_PKG_VERSION")
                    ))
            }
            BackgroundOutcome::Failed => {
                log!("Red automatic update failed (will retry): {}", result.message);
                return;
            }
        };
        if let Err(error) = self.publish_notification(notice) {
            log!("could not publish Red update notice: {error}");
        }
    }
}

fn spawn_worker(
    executable: PathBuf,
    kind: WorkerKind,
    notification: Option<NotificationId>,
    check_stamp: Option<PathBuf>,
) -> PendingUpdate {
    let (sender, result) = oneshot::channel();
    tokio::spawn(async move {
        if let Some(stamp) = check_stamp {
            match claim_daily_check(&stamp) {
                Ok(true) => {}
                Ok(false) => {
                    let _ = sender.send(WorkerResult {
                        success: true,
                        message: String::new(),
                    });
                    return;
                }
                Err(error) => {
                    let _ = sender.send(WorkerResult {
                        success: false,
                        message: format!("Could not claim Red update check: {error}"),
                    });
                    return;
                }
            }
        }
        let mut command = tokio::process::Command::new(executable);
        command
            .arg("update")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match kind {
            WorkerKind::Check => {
                command.arg("--check");
            }
            WorkerKind::Auto => {
                command.arg("--auto");
            }
            WorkerKind::Install => {}
        }
        let result = match command.output().await {
            Ok(output) => WorkerResult {
                success: output.status.success(),
                message: worker_message(&output.stdout, &output.stderr),
            },
            Err(error) => WorkerResult {
                success: false,
                message: format!("Could not start Red update: {error}"),
            },
        };
        let _ = sender.send(result);
    });
    PendingUpdate {
        kind,
        notification,
        result,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum BackgroundOutcome {
    /// Nothing the user needs to see: up to date, deferred, or a failed check.
    Quiet,
    Available,
    Installed,
    /// An unattended installation failed and left the current version in place.
    Failed,
}

fn background_outcome(kind: WorkerKind, result: &WorkerResult) -> BackgroundOutcome {
    if !result.success {
        return match kind {
            WorkerKind::Auto => BackgroundOutcome::Failed,
            WorkerKind::Check | WorkerKind::Install => BackgroundOutcome::Quiet,
        };
    }
    if result.message.starts_with(update::AVAILABLE_PREFIX) {
        BackgroundOutcome::Available
    } else if result.message.starts_with(update::INSTALLED_PREFIX) {
        BackgroundOutcome::Installed
    } else {
        BackgroundOutcome::Quiet
    }
}

fn worker_message(stdout: &[u8], stderr: &[u8]) -> String {
    let output = if stdout.is_empty() { stderr } else { stdout };
    let message = String::from_utf8_lossy(output).trim().to_string();
    if message.is_empty() {
        "Red update produced no output".to_string()
    } else {
        message
    }
}

fn claim_daily_check(path: &Path) -> anyhow::Result<bool> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut stamp = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    if let Err(error) = stamp.try_lock() {
        return match error {
            fs::TryLockError::WouldBlock => Ok(false),
            fs::TryLockError::Error(error) => Err(error.into()),
        };
    }
    let mut contents = String::new();
    stamp.read_to_string(&mut contents)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    if contents.trim().parse::<u64>().is_ok_and(|next| next > now) {
        return Ok(false);
    }
    stamp.set_len(0)?;
    stamp.rewind()?;
    writeln!(stamp, "{}", now + CHECK_INTERVAL.as_secs())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_check_is_claimed_once() {
        let directory = tempfile::TempDir::new().unwrap();
        let stamp = directory.path().join("updates/check");
        assert!(claim_daily_check(&stamp).unwrap());
        assert!(!claim_daily_check(&stamp).unwrap());
    }

    fn worker(success: bool, message: &str) -> WorkerResult {
        WorkerResult {
            success,
            message: message.to_string(),
        }
    }

    #[test]
    fn background_outcomes_follow_worker_output() {
        let available = worker(true, "Update available: Red 9.9.9 (installed 0.9.1).");
        let installed = worker(true, "Updated Red 0.9.1 to 9.9.9 at /bin/red.");
        let deferred = worker(true, "Update deferred: a detached Red session is active");
        let current = worker(true, "Red 0.9.1 is up to date.");
        let failed = worker(false, "release signature is malformed");

        for kind in [WorkerKind::Check, WorkerKind::Auto] {
            assert_eq!(
                background_outcome(kind, &available),
                BackgroundOutcome::Available
            );
            assert_eq!(
                background_outcome(kind, &deferred),
                BackgroundOutcome::Quiet
            );
            assert_eq!(background_outcome(kind, &current), BackgroundOutcome::Quiet);
        }
        assert_eq!(
            background_outcome(WorkerKind::Auto, &installed),
            BackgroundOutcome::Installed
        );
        assert_eq!(
            background_outcome(WorkerKind::Auto, &failed),
            BackgroundOutcome::Failed
        );
        assert_eq!(
            background_outcome(WorkerKind::Check, &failed),
            BackgroundOutcome::Quiet
        );
    }

    #[test]
    fn worker_output_prefers_stdout_and_has_a_fallback() {
        assert_eq!(
            worker_message(b"update available\n", b"noise"),
            "update available"
        );
        assert_eq!(worker_message(b"", b"failure\n"), "failure");
        assert_eq!(worker_message(b"", b""), "Red update produced no output");
    }
}
