//! Best-effort, workspace-scoped recovery of provisional language-server findings.
//!
//! Cached diagnostics are never authoritative. Their document text, language-server
//! configuration, and workspace manifests must still match before they are displayed,
//! and the first fresh report replaces both restored producer channels.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs::{self, File},
    io::{self, Read as _, Write as _},
    path::{Component, Path, PathBuf},
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ropey::Rope;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::{
    buffer::Buffer,
    config::{LanguageServerConfig, LspConfig},
    lsp::{file_path, Diagnostic, LspManager},
};

use super::diagnostics::DiagnosticReports;

const CACHE_SCHEMA_VERSION: u32 = 1;
const MAX_CACHE_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CACHED_DOCUMENT_BYTES: u64 = 16 * 1024 * 1024;
const CACHE_WRITE_DEBOUNCE: Duration = Duration::from_secs(2);
const MAX_CACHE_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const WORKSPACE_MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain",
    "rust-toolchain.toml",
    "Husk.toml",
    "package.json",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
    "pyproject.toml",
    "poetry.lock",
    "uv.lock",
    "requirements.txt",
    "go.mod",
    "go.sum",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedDocument {
    uri: String,
    server_name: String,
    content_sha256: [u8; 32],
    server_config_sha256: [u8; 32],
    #[serde(default)]
    push: Vec<Diagnostic>,
    #[serde(default)]
    pull: Vec<Diagnostic>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedWorkspace {
    version: u32,
    workspace_root: PathBuf,
    workspace_sha256: [u8; 32],
    captured_at_ms: u64,
    documents: Vec<CachedDocument>,
}

#[derive(Clone)]
struct OpenDocument {
    file: String,
    uri: String,
    contents: Rope,
}

#[derive(Clone)]
struct ReportSnapshot {
    uri: String,
    push: Vec<Diagnostic>,
    pull: Vec<Diagnostic>,
}

pub(super) struct RestoredDiagnosticReports {
    pub(super) uri: String,
    pub(super) push: Vec<Diagnostic>,
    pub(super) pull: Vec<Diagnostic>,
}

/// Debounces owner-private cache writes and tracks workspaces already hydrated.
pub(super) struct DiagnosticCache {
    directory: PathBuf,
    loaded_workspaces: HashSet<PathBuf>,
    dirty_since: Option<Instant>,
    writer: Option<JoinHandle<anyhow::Result<()>>>,
}

impl DiagnosticCache {
    pub(super) fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            loaded_workspaces: HashSet::new(),
            dirty_since: None,
            writer: None,
        }
    }

    pub(super) fn mark_dirty(&mut self) {
        self.dirty_since.get_or_insert_with(Instant::now);
    }

    /// Opens each workspace at most once, including when a file is opened later.
    pub(super) fn load(
        &mut self,
        config: &LspConfig,
        buffers: &[Buffer],
    ) -> Vec<RestoredDiagnosticReports> {
        if !config.enabled {
            return Vec::new();
        }

        let routing = LspManager::new(config.clone());
        let open_documents = open_documents(buffers);
        let open_by_uri = open_documents
            .iter()
            .map(|document| (document.uri.as_str(), &document.contents))
            .collect::<HashMap<_, _>>();
        let mut restored = Vec::new();

        for document in &open_documents {
            let Some(route) = routing.resolve_document(&document.file) else {
                continue;
            };
            let workspace_root = canonical_workspace_root(&route.workspace_root);
            if !self.loaded_workspaces.insert(workspace_root.clone()) {
                continue;
            }

            let cache_path = workspace_cache_path(&self.directory, &workspace_root);
            let workspace = match read_workspace(&cache_path) {
                Ok(Some(workspace)) => workspace,
                Ok(None) => continue,
                Err(error) => {
                    crate::log!(
                        "ignoring unreadable diagnostic cache {}: {error}",
                        cache_path.display()
                    );
                    continue;
                }
            };
            if workspace.version != CACHE_SCHEMA_VERSION
                || workspace.workspace_root != workspace_root
                || cache_expired(workspace.captured_at_ms)
            {
                continue;
            }
            let Ok(fingerprint) = workspace_fingerprint(&workspace_root, config) else {
                continue;
            };
            if workspace.workspace_sha256 != fingerprint {
                continue;
            }

            for cached in workspace.documents {
                if cached.push.is_empty() && cached.pull.is_empty() {
                    continue;
                }
                let Ok(path) = file_path(&cached.uri) else {
                    continue;
                };
                let Some(route) = routing.resolve_document(&path) else {
                    continue;
                };
                if route.uri != cached.uri
                    || route.server_name != cached.server_name
                    || canonical_workspace_root(&route.workspace_root) != workspace_root
                {
                    continue;
                }
                let Some(server) = config.servers.get(&route.server_name) else {
                    continue;
                };
                if server_fingerprint(server).ok() != Some(cached.server_config_sha256) {
                    continue;
                }
                let current_hash = open_by_uri
                    .get(cached.uri.as_str())
                    .map(|contents| hash_rope(contents))
                    .or_else(|| hash_file(Path::new(&path)).ok());
                if current_hash != Some(cached.content_sha256) {
                    continue;
                }
                restored.push(RestoredDiagnosticReports {
                    uri: cached.uri,
                    push: cached.push,
                    pull: cached.pull,
                });
            }
        }

        restored
    }

    /// Captures cheap rope snapshots and moves hashing and disk writes off-thread.
    pub(super) fn flush(
        &mut self,
        config: &LspConfig,
        buffers: &[Buffer],
        reports: &DiagnosticReports,
        force: bool,
    ) -> anyhow::Result<()> {
        self.finish_writer(force)?;
        if self.writer.is_some() {
            return Ok(());
        }
        let Some(dirty_since) = self.dirty_since else {
            return Ok(());
        };
        if !force && dirty_since.elapsed() < CACHE_WRITE_DEBOUNCE {
            return Ok(());
        }

        let directory = self.directory.clone();
        let config = config.clone();
        let documents = open_documents(buffers);
        let reports = reports
            .entries()
            .map(|(uri, push, pull)| ReportSnapshot {
                uri: uri.to_string(),
                push: push.to_vec(),
                pull: pull.to_vec(),
            })
            .collect::<Vec<_>>();
        self.dirty_since = None;
        match std::thread::Builder::new()
            .name("red-diagnostic-cache".to_string())
            .spawn(move || persist_workspaces(&directory, &config, &documents, &reports))
        {
            Ok(writer) => self.writer = Some(writer),
            Err(error) => {
                self.mark_dirty();
                return Err(error.into());
            }
        }

        if force {
            self.finish_writer(true)?;
        }
        Ok(())
    }

    fn finish_writer(&mut self, force: bool) -> anyhow::Result<()> {
        if !self
            .writer
            .as_ref()
            .is_some_and(|writer| force || writer.is_finished())
        {
            return Ok(());
        }
        let writer = self.writer.take().expect("finished cache writer exists");
        match writer.join() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                self.mark_dirty();
                Err(error)
            }
            Err(_) => {
                self.mark_dirty();
                anyhow::bail!("diagnostic cache writer panicked")
            }
        }
    }
}

fn open_documents(buffers: &[Buffer]) -> Vec<OpenDocument> {
    buffers
        .iter()
        .filter_map(|buffer| {
            Some(OpenDocument {
                file: buffer.file.clone()?,
                uri: buffer.uri().ok().flatten()?,
                contents: buffer.contents_snapshot(),
            })
        })
        .collect()
}

fn persist_workspaces(
    directory: &Path,
    config: &LspConfig,
    open_documents: &[OpenDocument],
    reports: &[ReportSnapshot],
) -> anyhow::Result<()> {
    if !config.enabled {
        return Ok(());
    }
    let routing = LspManager::new(config.clone());
    let open_by_uri = open_documents
        .iter()
        .map(|document| (document.uri.as_str(), &document.contents))
        .collect::<HashMap<_, _>>();
    let mut workspaces = HashMap::<PathBuf, CachedWorkspace>::new();

    for document in open_documents {
        let Some(route) = routing.resolve_document(&document.file) else {
            continue;
        };
        let workspace_root = canonical_workspace_root(&route.workspace_root);
        if workspaces.contains_key(&workspace_root) {
            continue;
        }
        let workspace_sha256 = workspace_fingerprint(&workspace_root, config)?;
        workspaces.insert(
            workspace_root.clone(),
            CachedWorkspace {
                version: CACHE_SCHEMA_VERSION,
                workspace_root,
                workspace_sha256,
                captured_at_ms: current_time_ms(),
                documents: Vec::new(),
            },
        );
    }

    for report in reports {
        if report.push.is_empty() && report.pull.is_empty() {
            continue;
        }
        let Ok(path) = file_path(&report.uri) else {
            continue;
        };
        let Some(route) = routing.resolve_document(&path) else {
            continue;
        };
        if route.uri != report.uri {
            continue;
        }
        let workspace_root = canonical_workspace_root(&route.workspace_root);
        let Some(workspace) = workspaces.get_mut(&workspace_root) else {
            continue;
        };
        let Some(server) = config.servers.get(&route.server_name) else {
            continue;
        };
        let content_sha256 = match open_by_uri.get(report.uri.as_str()) {
            Some(contents) => hash_rope(contents),
            None => match hash_file(Path::new(&path)) {
                Ok(hash) => hash,
                Err(_) => continue,
            },
        };
        workspace.documents.push(CachedDocument {
            uri: report.uri.clone(),
            server_name: route.server_name,
            content_sha256,
            server_config_sha256: server_fingerprint(server)?,
            push: report.push.clone(),
            pull: report.pull.clone(),
        });
    }

    for workspace in workspaces.values_mut() {
        workspace
            .documents
            .sort_unstable_by(|left, right| left.uri.cmp(&right.uri));
        write_workspace(directory, workspace)?;
    }
    Ok(())
}

fn canonical_workspace_root(root: &Path) -> PathBuf {
    fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}

fn workspace_cache_path(directory: &Path, workspace_root: &Path) -> PathBuf {
    let digest = Sha256::digest(workspace_root.to_string_lossy().as_bytes());
    directory.join(format!("{digest:x}.json"))
}

fn cache_expired(captured_at_ms: u64) -> bool {
    current_time_ms().saturating_sub(captured_at_ms)
        > u64::try_from(MAX_CACHE_AGE.as_millis()).unwrap_or(u64::MAX)
}

fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

fn hash_rope(contents: &Rope) -> [u8; 32] {
    let mut digest = Sha256::new();
    for chunk in contents.chunks() {
        digest.update(chunk.as_bytes());
    }
    digest.finalize().into()
}

fn hash_file(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_CACHED_DOCUMENT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "diagnostic cache document is not a bounded regular file",
        ));
    }
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(digest.finalize().into())
}

fn server_fingerprint(server: &LanguageServerConfig) -> anyhow::Result<[u8; 32]> {
    let mut value = serde_json::to_value(server)?;
    canonicalize_json(&mut value);
    Ok(Sha256::digest(serde_json::to_vec(&value)?).into())
}

fn canonicalize_json(value: &mut Value) {
    match value {
        Value::Object(object) => {
            let mut entries = std::mem::take(object).into_iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            for (key, mut value) in entries {
                canonicalize_json(&mut value);
                object.insert(key, value);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(canonicalize_json),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn workspace_fingerprint(root: &Path, config: &LspConfig) -> io::Result<[u8; 32]> {
    let mut markers = WORKSPACE_MANIFESTS
        .iter()
        .copied()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    for marker in config
        .servers
        .values()
        .flat_map(|server| server.root_markers.iter())
    {
        let mut components = Path::new(marker).components();
        if matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none() {
            markers.insert(marker.clone());
        }
    }

    let mut digest = Sha256::new();
    for marker in markers {
        digest.update(marker.as_bytes());
        digest.update([0]);
        match fs::metadata(root.join(&marker)) {
            Ok(metadata) if metadata.is_file() => {
                digest.update([1]);
                digest.update(hash_file(&root.join(&marker))?);
            }
            Ok(metadata) if metadata.is_dir() => digest.update([2]),
            Ok(_) => digest.update([3]),
            Err(error) if error.kind() == io::ErrorKind::NotFound => digest.update([0]),
            Err(error) => return Err(error),
        }
    }
    Ok(digest.finalize().into())
}

fn read_workspace(path: &Path) -> anyhow::Result<Option<CachedWorkspace>> {
    let mut file = match open_cache_file(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file() && metadata.len() <= MAX_CACHE_FILE_BYTES,
        "diagnostic cache must be a bounded regular file"
    );
    let mut contents = Vec::new();
    (&mut file)
        .take(MAX_CACHE_FILE_BYTES + 1)
        .read_to_end(&mut contents)?;
    anyhow::ensure!(
        contents.len() as u64 <= MAX_CACHE_FILE_BYTES,
        "diagnostic cache exceeds its read limit"
    );
    Ok(Some(serde_json::from_slice(&contents)?))
}

#[cfg(unix)]
fn open_cache_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
}

#[cfg(not(unix))]
fn open_cache_file(path: &Path) -> io::Result<File> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "diagnostic cache must not be a symlink",
        ));
    }
    File::open(path)
}

fn write_workspace(directory: &Path, workspace: &CachedWorkspace) -> anyhow::Result<()> {
    fs::create_dir_all(directory)?;
    let metadata = fs::symlink_metadata(directory)?;
    anyhow::ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "diagnostic cache directory must not be a symlink"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }

    let contents = serde_json::to_vec(workspace)?;
    anyhow::ensure!(
        contents.len() as u64 <= MAX_CACHE_FILE_BYTES,
        "diagnostic cache exceeds its write limit"
    );
    let destination = workspace_cache_path(directory, &workspace.workspace_root);
    if let Ok(metadata) = fs::symlink_metadata(&destination) {
        anyhow::ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "diagnostic cache destination must be a regular file"
        );
    }
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
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
    Ok(())
}
