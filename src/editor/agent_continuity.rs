//! Keeps the Agent pane's Codex thread across normal Red launches.
//!
//! Each workspace has one saved conversation under
//! `<config>/agent/conversations/<sha256 of the workspace>.json`. Only the Red
//! process that holds the workspace's owner claim (`<sha256>.owner`, which
//! records its pid) writes or removes that record, so two editors open in one
//! workspace never drive the same Codex thread: the second editor starts its
//! own thread, and that thread is not saved for resume. A claim whose process
//! has exited is taken over by the next editor that needs it.

use std::{
    collections::HashSet,
    fs,
    io::{self, Read as _, Write as _},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::agent_conversation::AgentConversationSnapshot;

const RECORD_VERSION: u32 = 1;
const MAX_RECORD_BYTES: u64 = 16 * 1024 * 1024;

/// A conversation saved by an earlier editor in this workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedConversation {
    pub saved_at_ms: u64,
    pub conversation: AgentConversationSnapshot,
}

#[derive(Debug, Serialize, Deserialize)]
struct Record {
    version: u32,
    workspace: String,
    saved_at_ms: u64,
    conversation: AgentConversationSnapshot,
}

/// Who may resume and save a workspace's conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    /// This editor owns the workspace's conversation.
    Owned,
    /// Another running editor owns it.
    HeldBy(u32),
}

pub struct ConversationStore {
    directory: PathBuf,
    pid: u32,
    /// Workspace keys this editor has claimed.
    claimed: HashSet<String>,
}

impl ConversationStore {
    pub fn new(directory: PathBuf) -> Self {
        Self::for_process(directory, std::process::id())
    }

    fn for_process(directory: PathBuf, pid: u32) -> Self {
        Self {
            directory,
            pid,
            claimed: HashSet::new(),
        }
    }

    /// Claims a workspace's conversation for this editor unless another live
    /// editor holds it.
    ///
    /// Every read or replacement of the owner file happens under an OS lock on
    /// `<key>.lock`, and the pid is published by an atomic rename, so two
    /// editors racing for one workspace never both see it as free.
    pub fn claim(&mut self, workspace: &Path) -> io::Result<Claim> {
        let key = workspace_key(workspace);
        if self.claimed.contains(&key) {
            return Ok(Claim::Owned);
        }
        ensure_private_directory(&self.directory)?;
        let _lock = self.lock(&key)?;
        let path = self.directory.join(format!("{key}.owner"));
        match read_owner(&path)? {
            Some(pid) if pid == self.pid => {}
            Some(pid) if process_is_alive(pid) => return Ok(Claim::HeldBy(pid)),
            // Free, or left behind by an editor that exited without releasing it.
            _ => {
                let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
                temporary.write_all(self.pid.to_string().as_bytes())?;
                temporary.as_file().sync_all()?;
                temporary.persist(&path).map_err(|error| error.error)?;
            }
        }
        self.claimed.insert(key);
        Ok(Claim::Owned)
    }

    pub fn owns(&self, workspace: &Path) -> bool {
        self.claimed.contains(&workspace_key(workspace))
    }

    /// Releases every claim so the next editor in these workspaces can resume.
    pub fn release_all(&mut self) {
        for key in std::mem::take(&mut self.claimed) {
            let Ok(_lock) = self.lock(&key) else {
                continue;
            };
            let path = self.directory.join(format!("{key}.owner"));
            if read_owner(&path).ok().flatten() == Some(self.pid) {
                let _ = fs::remove_file(path);
            }
        }
    }

    /// Holds the workspace's claim lock until the returned file is dropped.
    fn lock(&self, key: &str) -> io::Result<fs::File> {
        let path = self.directory.join(format!("{key}.lock"));
        let file = match create_private_file(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                fs::OpenOptions::new().write(true).open(&path)?
            }
            Err(error) => return Err(error),
        };
        file.lock()?;
        Ok(file)
    }

    /// The conversation saved for a workspace, if it is readable and was saved
    /// for exactly this workspace.
    pub fn load(&self, workspace: &Path) -> Option<SavedConversation> {
        let key = workspace_key(workspace);
        let record: Record = read_bounded_json(&self.directory.join(format!("{key}.json")))?;
        (record.version == RECORD_VERSION && record.workspace == key_source(workspace)).then_some(
            SavedConversation {
                saved_at_ms: record.saved_at_ms,
                conversation: record.conversation,
            },
        )
    }

    /// Saves a workspace's conversation. Does nothing unless this editor owns it.
    pub fn save(
        &mut self,
        workspace: &Path,
        conversation: &AgentConversationSnapshot,
    ) -> anyhow::Result<bool> {
        if !self.owns(workspace) {
            return Ok(false);
        }
        let record = Record {
            version: RECORD_VERSION,
            workspace: key_source(workspace),
            saved_at_ms: now_ms(),
            conversation: conversation.clone(),
        };
        let contents = serde_json::to_vec(&record)?;
        anyhow::ensure!(
            contents.len() as u64 <= MAX_RECORD_BYTES,
            "agent conversation is too large to save"
        );
        ensure_private_directory(&self.directory)?;
        let destination = self
            .directory
            .join(format!("{}.json", workspace_key(workspace)));
        if let Ok(metadata) = fs::symlink_metadata(&destination) {
            anyhow::ensure!(
                metadata.is_file(),
                "agent conversation destination must be a regular file"
            );
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            temporary
                .as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        temporary.write_all(&contents)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(&destination)
            .map_err(|error| error.error)?;
        Ok(true)
    }

    /// Removes a workspace's saved conversation. Does nothing unless this editor
    /// owns it.
    pub fn remove(&mut self, workspace: &Path) -> io::Result<bool> {
        if !self.owns(workspace) {
            return Ok(false);
        }
        let path = self
            .directory
            .join(format!("{}.json", workspace_key(workspace)));
        match fs::remove_file(path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
}

impl Drop for ConversationStore {
    fn drop(&mut self) {
        self.release_all();
    }
}

/// "3 minutes ago"-style age of a saved conversation.
pub fn describe_age(saved_at_ms: u64) -> String {
    let seconds = now_ms().saturating_sub(saved_at_ms) / 1000;
    let plural = |count: u64, unit: &str| {
        if count == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{count} {unit}s ago")
        }
    };
    match seconds {
        0..=59 => "a moment ago".to_string(),
        60..=3_599 => plural(seconds / 60, "minute"),
        3_600..=86_399 => plural(seconds / 3_600, "hour"),
        _ => plural(seconds / 86_400, "day"),
    }
}

fn key_source(workspace: &Path) -> String {
    fs::canonicalize(workspace)
        .unwrap_or_else(|_| workspace.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn workspace_key(workspace: &Path) -> String {
    format!("{:x}", Sha256::digest(key_source(workspace).as_bytes()))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

fn ensure_private_directory(directory: &Path) -> io::Result<()> {
    fs::create_dir_all(directory)?;
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir() {
        return Err(io::Error::other(
            "agent conversation directory must be a real directory",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn create_private_file(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

/// The pid recorded in an owner file; unreadable or garbled contents count as no owner.
fn read_owner(path: &Path) -> io::Result<Option<u32>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text.trim().parse().ok()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn read_bounded_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_RECORD_BYTES {
        return None;
    }
    let mut contents = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut contents)
        .ok()?;
    serde_json::from_slice(&contents).ok()
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), /*signal*/ None).is_ok()
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED, STILL_ACTIVE},
        System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };
    // SAFETY: OpenProcess takes no pointers; a null handle reports failure.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        // A process we may not query still exists; any other error means it is gone.
        // SAFETY: reads this thread's last-error value.
        return unsafe { GetLastError() } == ERROR_ACCESS_DENIED;
    }
    let mut code = 0u32;
    // SAFETY: `handle` is a live process handle and `code` outlives the call.
    let queried = unsafe { GetExitCodeProcess(handle, &mut code) } != 0;
    // SAFETY: `handle` came from OpenProcess and is closed exactly once.
    unsafe { CloseHandle(handle) };
    queried && code == STILL_ACTIVE as u32
}

#[cfg(not(any(unix, windows)))]
fn process_is_alive(_pid: u32) -> bool {
    // Without a liveness check, treat a recorded owner as live; a stale claim
    // then only costs a fresh thread, never two editors on one thread.
    true
}

/// Shortest pause between saves while nothing is streaming.
const IDLE_SAVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
/// Shortest pause between saves while a reply streams in.
const STREAMING_SAVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

impl super::Editor {
    /// Saves the Agent pane's conversation per workspace and adopts the one an
    /// earlier editor saved here, so the next message continues that Codex thread.
    pub fn enable_agent_continuity(&mut self, directory: PathBuf) {
        let workspace = std::env::current_dir().unwrap_or_else(|_| super::get_workspace_path());
        self.enable_agent_continuity_in(ConversationStore::new(directory), &workspace);
    }

    fn enable_agent_continuity_in(&mut self, mut store: ConversationStore, workspace: &Path) {
        // A --resume or detached restore already brought its own conversation.
        if self.agent_manager.conversation().is_none() {
            self.adopt_saved_agent_conversation(&mut store, workspace);
        }
        self.agent_continuity = Some(store);
    }

    fn adopt_saved_agent_conversation(&mut self, store: &mut ConversationStore, workspace: &Path) {
        let Some(saved) = store.load(workspace) else {
            return;
        };
        let compatible = saved.conversation.tool_contract_version
            == crate::agent_conversation::AGENT_TOOL_CONTRACT_VERSION
            && !saved.conversation.thread_id.is_empty()
            && !saved.conversation.items.is_empty();
        if !compatible {
            // The archived transcript still recaps it in a new thread.
            return;
        }
        match store.claim(workspace) {
            Ok(Claim::Owned) => {
                self.agent_continuity_adopted_age = Some(describe_age(saved.saved_at_ms));
                self.agent_manager.restore_conversation(saved.conversation);
                self.agent_continuity_saved = Some((
                    Some(self.agent_manager.conversation_revision()),
                    std::time::Instant::now(),
                ));
                self.agent_continuity_workspace = Some(workspace.to_path_buf());
            }
            Ok(Claim::HeldBy(pid)) => {
                crate::log!(
                    "agent conversation for {} is open in Red pid {pid}; starting a separate one",
                    workspace.display()
                );
                self.set_quiet_message(Some(format!(
                    "Agent conversation is open in another Red (pid {pid}); this window starts its own"
                )));
            }
            Err(error) => crate::log!("could not claim the saved agent conversation: {error}"),
        }
    }

    /// The age of a conversation adopted at launch, reported once.
    pub(super) fn take_adopted_agent_conversation_age(&mut self) -> Option<String> {
        self.agent_continuity_adopted_age.take()
    }

    /// Writes the conversation when it changed, at most once per interval unless
    /// `force` is set. A forgotten conversation removes the saved one.
    pub(super) fn persist_agent_conversation(&mut self, force: bool) {
        let Some(store) = self.agent_continuity.as_mut() else {
            return;
        };
        let revision = self.agent_manager.conversation_revision();
        if let Some((saved, at)) = self.agent_continuity_saved {
            let interval = if self.agent_manager.has_active_sessions() {
                STREAMING_SAVE_INTERVAL
            } else {
                IDLE_SAVE_INTERVAL
            };
            if saved == Some(revision) || (!force && at.elapsed() < interval) {
                return;
            }
        }
        let result = match self.agent_manager.conversation() {
            Some(conversation) if !conversation.items.is_empty() => {
                let workspace = PathBuf::from(&conversation.cwd);
                if let Some(previous) = self
                    .agent_continuity_workspace
                    .take()
                    .filter(|previous| previous != &workspace)
                {
                    if let Err(error) = store.remove(&previous) {
                        crate::log!(
                            "could not remove the agent conversation saved for {}: {error}",
                            previous.display()
                        );
                    }
                }
                self.agent_continuity_workspace = Some(workspace.clone());
                match store.claim(&workspace) {
                    Ok(Claim::Owned) => store.save(&workspace, conversation).map(|_| ()),
                    // Another editor owns this workspace's conversation.
                    Ok(Claim::HeldBy(_)) => Ok(()),
                    Err(error) => Err(error.into()),
                }
            }
            // No conversation, or a new thread with nothing in it yet (as right
            // after :AgentNew): nothing worth resuming, so the old record goes.
            _ => match self.agent_continuity_workspace.clone() {
                // Keep the workspace until its record is really gone, so a
                // failed removal is retried instead of resuming a forgotten thread.
                Some(workspace) => store
                    .remove(&workspace)
                    .map(|_| self.agent_continuity_workspace = None)
                    .map_err(Into::into),
                None => Ok(()),
            },
        };
        let saved = match result {
            Ok(()) => Some(revision),
            Err(error) => {
                crate::log!("could not save the agent conversation: {error:#}");
                // Still stale: retry after the interval and on the forced exit save.
                self.agent_continuity_saved.and_then(|(saved, _)| saved)
            }
        };
        self.agent_continuity_saved = Some((saved, std::time::Instant::now()));
    }

    /// Final save on exit; releasing the claim lets the next launch resume.
    pub(super) fn finish_agent_continuity(&mut self) {
        self.persist_agent_conversation(/*force*/ true);
        if let Some(store) = self.agent_continuity.as_mut() {
            store.release_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation(thread: &str, workspace: &Path) -> AgentConversationSnapshot {
        let mut conversation = AgentConversationSnapshot::new(thread, workspace.to_string_lossy());
        conversation.append_user("t1", "Remember HERON");
        conversation
    }

    #[test]
    fn an_owner_saves_and_a_later_editor_loads_the_same_workspace_only() {
        let config = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let directory = config.path().join("agent/conversations");

        let mut first = ConversationStore::for_process(directory.clone(), std::process::id());
        assert_eq!(first.claim(workspace.path()).unwrap(), Claim::Owned);
        let saved = conversation("thread-1", workspace.path());
        assert!(first.save(workspace.path(), &saved).unwrap());
        first.release_all();

        let second = ConversationStore::for_process(directory, std::process::id());
        let loaded = second.load(workspace.path()).unwrap();
        assert_eq!(loaded.conversation, saved);
        assert!(
            second.load(other.path()).is_none(),
            "workspaces stay separate"
        );
    }

    #[test]
    fn a_live_owner_keeps_other_editors_from_saving_or_removing() {
        let config = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let directory = config.path().to_path_buf();
        let mut owner = ConversationStore::for_process(directory.clone(), std::process::id());
        owner.claim(workspace.path()).unwrap();
        owner
            .save(workspace.path(), &conversation("mine", workspace.path()))
            .unwrap();

        // A different pid in the same workspace while this test process is alive.
        let mut other = ConversationStore::for_process(directory, u32::MAX - 1);
        assert_eq!(
            other.claim(workspace.path()).unwrap(),
            Claim::HeldBy(std::process::id())
        );
        assert!(!other
            .save(workspace.path(), &conversation("theirs", workspace.path()))
            .unwrap());
        assert!(!other.remove(workspace.path()).unwrap());
        assert_eq!(
            other.load(workspace.path()).unwrap().conversation.thread_id,
            "mine"
        );
    }

    #[test]
    fn a_claim_left_by_an_exited_editor_is_taken_over() {
        let config = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let key = workspace_key(workspace.path());
        fs::create_dir_all(config.path()).unwrap();
        // pid_max is far below this on every supported platform.
        fs::write(config.path().join(format!("{key}.owner")), "2147483000").unwrap();

        let mut store = ConversationStore::for_process(config.path().to_path_buf(), 4242);
        assert_eq!(store.claim(workspace.path()).unwrap(), Claim::Owned);
        assert_eq!(
            fs::read_to_string(config.path().join(format!("{key}.owner"))).unwrap(),
            "4242"
        );
        store.release_all();
        assert!(!config.path().join(format!("{key}.owner")).exists());
    }

    #[cfg(unix)]
    #[test]
    fn two_live_editors_racing_for_a_workspace_never_both_own_it() {
        // Two pids that are both alive: this test process and its parent.
        let pids = [std::process::id(), nix::unistd::getppid().as_raw() as u32];
        for _ in 0..50 {
            let config = tempfile::tempdir().unwrap();
            let workspace = tempfile::tempdir().unwrap();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let claims = pids.map(|pid| {
                let directory = config.path().to_path_buf();
                let workspace = workspace.path().to_path_buf();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut store = ConversationStore::for_process(directory, pid);
                    barrier.wait();
                    let claim = store.claim(&workspace).unwrap();
                    // Keep the claim: dropping the store would release it.
                    std::mem::forget(store);
                    claim
                })
            });
            let claims = claims.map(|handle| handle.join().unwrap());
            let owned = claims
                .iter()
                .filter(|claim| **claim == Claim::Owned)
                .count();
            assert_eq!(owned, 1, "{claims:?}");
        }
    }

    #[test]
    fn removing_a_forgotten_conversation_means_nothing_resumes() {
        let config = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut store = ConversationStore::new(config.path().to_path_buf());
        store.claim(workspace.path()).unwrap();
        store
            .save(workspace.path(), &conversation("old", workspace.path()))
            .unwrap();
        assert!(store.remove(workspace.path()).unwrap());
        assert!(store.load(workspace.path()).is_none());
    }

    fn editor() -> crate::editor::Editor {
        let config = crate::config::Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = crate::editor::Editor::with_size(
            lsp,
            80,
            24,
            config,
            crate::theme::Theme::default(),
            vec![crate::buffer::Buffer::new(None, String::new())],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        editor
    }

    #[test]
    fn an_editor_saves_its_conversation_and_the_next_launch_adopts_it() {
        let config = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let directory = config.path().to_path_buf();

        let mut first = editor();
        first.enable_agent_continuity_in(
            ConversationStore::new(directory.clone()),
            workspace.path(),
        );
        assert!(
            first.agent_manager.conversation().is_none(),
            "nothing saved yet"
        );
        first
            .agent_manager
            .begin_conversation("thread-1", workspace.path());
        first.agent_manager.mark_session_active("thread-1");
        first.agent_manager.set_turn_id("thread-1", "t1");
        first
            .agent_manager
            .record_user_message("thread-1", "t1", "Remember HERON");
        first.finish_agent_continuity();

        let mut second = editor();
        second.enable_agent_continuity_in(
            ConversationStore::new(directory.clone()),
            workspace.path(),
        );
        let adopted = second.agent_manager.conversation().unwrap();
        assert_eq!(adopted.thread_id, "thread-1");
        assert_eq!(adopted.items[0].text, "Remember HERON");
        assert_eq!(
            second.take_adopted_agent_conversation_age().as_deref(),
            Some("a moment ago")
        );

        // A third editor in the same workspace while the second is open.
        let mut third = editor();
        third.enable_agent_continuity_in(
            ConversationStore::for_process(directory, u32::MAX - 1),
            workspace.path(),
        );
        assert!(
            third.agent_manager.conversation().is_none(),
            "a thread is never driven by two editors"
        );
    }

    #[test]
    fn forgetting_the_conversation_stops_it_from_resuming() {
        let config = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let directory = config.path().to_path_buf();
        let mut first = editor();
        first.enable_agent_continuity_in(
            ConversationStore::new(directory.clone()),
            workspace.path(),
        );
        first
            .agent_manager
            .begin_conversation("old", workspace.path());
        first.agent_manager.mark_session_active("old");
        first.agent_manager.set_turn_id("old", "t1");
        first
            .agent_manager
            .record_user_message("old", "t1", "hello");
        first.persist_agent_conversation(/*force*/ true);
        assert!(ConversationStore::new(directory.clone())
            .load(workspace.path())
            .is_some());

        first.agent_manager.forget_conversation("old");
        first.finish_agent_continuity();
        assert!(ConversationStore::new(directory)
            .load(workspace.path())
            .is_none());
    }

    #[test]
    fn a_failed_save_is_retried_and_a_failed_removal_keeps_the_record_to_remove() {
        let config = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let directory = config.path().join("conversations");
        // A file where the directory should be makes every write fail.
        fs::write(&directory, "not a directory").unwrap();
        let mut editor = editor();
        editor.enable_agent_continuity_in(
            ConversationStore::new(directory.clone()),
            workspace.path(),
        );
        editor
            .agent_manager
            .begin_conversation("thread", workspace.path());
        editor.agent_manager.mark_session_active("thread");
        editor.agent_manager.set_turn_id("thread", "t1");
        editor
            .agent_manager
            .record_user_message("thread", "t1", "Remember HERON");
        editor.persist_agent_conversation(/*force*/ false);
        let current = editor.agent_manager.conversation_revision();
        assert_ne!(
            editor.agent_continuity_saved.map(|(saved, _)| saved),
            Some(Some(current)),
            "a failed write is not recorded as saved"
        );

        fs::remove_file(&directory).unwrap();
        editor.persist_agent_conversation(/*force*/ true);
        assert!(ConversationStore::new(directory.clone())
            .load(workspace.path())
            .is_some());

        let record = directory.join(format!("{}.json", workspace_key(workspace.path())));
        fs::remove_file(&record).unwrap();
        fs::create_dir(&record).unwrap();
        editor.agent_manager.forget_conversation("thread");
        editor.persist_agent_conversation(/*force*/ true);
        assert_eq!(
            editor.agent_continuity_workspace.as_deref(),
            Some(workspace.path()),
            "the record is still there, so it is still ours to remove"
        );

        fs::remove_dir(&record).unwrap();
        editor.persist_agent_conversation(/*force*/ true);
        assert!(editor.agent_continuity_workspace.is_none());
    }

    #[test]
    fn a_new_empty_thread_replaces_the_forgotten_record_before_its_first_message() {
        let config = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let directory = config.path().to_path_buf();
        let mut editor = editor();
        editor.enable_agent_continuity_in(
            ConversationStore::new(directory.clone()),
            workspace.path(),
        );
        editor
            .agent_manager
            .begin_conversation("old", workspace.path());
        editor.agent_manager.mark_session_active("old");
        editor.agent_manager.set_turn_id("old", "t1");
        editor
            .agent_manager
            .record_user_message("old", "t1", "Remember HERON");
        editor.persist_agent_conversation(/*force*/ true);

        // :AgentNew forgets the thread and the new one starts before the next save.
        editor.agent_manager.forget_conversation("old");
        editor
            .agent_manager
            .begin_conversation("fresh", workspace.path());
        editor.finish_agent_continuity();
        assert!(ConversationStore::new(directory)
            .load(workspace.path())
            .is_none());
    }

    #[test]
    fn a_conversation_from_an_older_tool_contract_is_not_resumed() {
        let config = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut store = ConversationStore::new(config.path().to_path_buf());
        store.claim(workspace.path()).unwrap();
        let mut old = conversation("old-tools", workspace.path());
        old.tool_contract_version = 0;
        store.save(workspace.path(), &old).unwrap();
        store.release_all();

        let mut editor = editor();
        editor.enable_agent_continuity_in(
            ConversationStore::new(config.path().to_path_buf()),
            workspace.path(),
        );
        assert!(editor.agent_manager.conversation().is_none());
    }

    #[test]
    fn ages_read_naturally() {
        let now = now_ms();
        assert_eq!(describe_age(now), "a moment ago");
        assert_eq!(describe_age(now - 60_000), "1 minute ago");
        assert_eq!(describe_age(now - 2 * 3_600_000), "2 hours ago");
        assert_eq!(describe_age(now - 3 * 86_400_000), "3 days ago");
    }
}
