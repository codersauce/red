//! Standalone Red release discovery and verified in-place replacement.
//!
//! The updater intentionally understands only Red's four published release targets and
//! installations marked by the official installers. Package-manager and source builds keep
//! ownership of their own executable.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::Context as _;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use tempfile::{Builder, NamedTempFile, TempPath};
use tokio::io::AsyncReadExt as _;

const REPOSITORY: &str = "codersauce/red";
const RELEASE_API: &str = "https://api.github.com/repos/codersauce/red/releases/latest";
const RELEASE_DOWNLOADS: &str = "https://github.com/codersauce/red/releases/download";
const RECEIPT_NAME: &str = ".red-install.toml";
const LOCK_NAME: &str = ".red-update.lock";
const MAX_METADATA_BYTES: u64 = 256 * 1024;
const CHECKSUM_ASSET: &str = "SHA256SUMS.txt";
const SIGNATURE_ASSET: &str = "SHA256SUMS.txt.minisig";
const MAX_CHECKSUM_BYTES: u64 = 256 * 1024;
const MAX_SIGNATURE_BYTES: u64 = 4 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 128 * 1024 * 1024;
const MAX_COMMAND_OUTPUT_BYTES: u64 = 256 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Minisign keys trusted to sign `SHA256SUMS.txt`. Keep the retiring key listed for at least
/// one release when rotating so existing installations can verify the handover.
const RELEASE_PUBLIC_KEYS: &[&str] = &["RWRNnsasbTdAjgpMwO4feHrX57Rr4uPupGSKZxVZRDTi3IuRON8kF3hE"];

#[derive(Clone, Copy)]
struct ReleaseSource<'a> {
    api: &'a str,
    downloads: &'a str,
    public_keys: &'a [&'a str],
}

const OFFICIAL_RELEASES: ReleaseSource<'static> = ReleaseSource {
    api: RELEASE_API,
    downloads: RELEASE_DOWNLOADS,
    public_keys: RELEASE_PUBLIC_KEYS,
};

#[derive(Debug)]
pub struct ManagedSessionGuard {
    _lock: File,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallReceipt {
    schema_version: u32,
    source: String,
    target: String,
}

#[derive(Clone, Debug, Deserialize)]
struct ReleaseAsset {
    name: String,
    size: u64,
}

#[derive(Clone, Debug, Deserialize)]
struct LatestRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<ReleaseAsset>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Candidate {
    version: Version,
    tag: String,
    archive: String,
    archive_size: u64,
    checksum_size: u64,
    signature_size: u64,
}

struct ManagedInstall {
    executable: PathBuf,
    identity: same_file::Handle,
    _lock: File,
}

struct CommandOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    status: std::process::ExitStatus,
}

/// Prefix of `red update --auto` output when installation was postponed.
pub const DEFERRED_PREFIX: &str = "Update deferred:";
/// Prefix of update output announcing a newer release that wasn't installed.
pub const AVAILABLE_PREFIX: &str = "Update available:";
/// Prefix of update output after a newer release replaced the executable.
pub const INSTALLED_PREFIX: &str = "Updated Red";

/// A condition that blocks replacement now but should clear on its own, such as another
/// update or a detached session holding the installation.
#[derive(Debug)]
struct Deferred(&'static str);

impl std::fmt::Display for Deferred {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for Deferred {}

/// Reports whether the designated latest stable Red release is newer.
pub async fn check() -> anyhow::Result<String> {
    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let client = http_client()?;
    match latest_candidate(&client, &current, OFFICIAL_RELEASES).await? {
        Some(candidate) => Ok(format!(
            "{AVAILABLE_PREFIX} Red {} (installed {}). Run `red update`.",
            candidate.version, current
        )),
        None => Ok(format!("Red {current} is up to date.")),
    }
}

/// Downloads, verifies, validates, and installs the designated latest stable release.
pub async fn install() -> anyhow::Result<String> {
    let running = Version::parse(env!("CARGO_PKG_VERSION"))?;
    install_from(
        canonical_executable()?,
        running,
        OFFICIAL_RELEASES,
        &crate::config::Config::path("run"),
    )
    .await
}

/// Installs unattended when the standalone updater owns this executable.
///
/// Package-manager and source builds only check, and transient installation locks defer the
/// update to the next scheduled attempt instead of failing.
pub async fn automatic() -> anyhow::Result<String> {
    let executable = canonical_executable()?;
    if validate_receipt(&executable).is_err() {
        return check().await;
    }
    match install().await {
        Err(error) if error.downcast_ref::<Deferred>().is_some() => {
            Ok(format!("{DEFERRED_PREFIX} {error}"))
        }
        result => result,
    }
}

async fn install_from(
    executable: PathBuf,
    running: Version,
    source: ReleaseSource<'_>,
    session_directory: &Path,
) -> anyhow::Result<String> {
    let managed = ManagedInstall::acquire(executable)?;
    let installed = executable_version(&managed.executable).await?;
    anyhow::ensure!(
        installed == running,
        "Red changed on disk after this process started; relaunch and retry"
    );
    ensure_no_detached_owner(session_directory)?;

    let client = http_client()?;
    let Some(candidate) = latest_candidate(&client, &installed, source).await? else {
        return Ok(format!("Red {installed} is already up to date."));
    };

    let checksums_url = download_url(source, &candidate.tag, CHECKSUM_ASSET);
    let checksums = download_exact_bytes(&client, &checksums_url, candidate.checksum_size).await?;
    let signature_url = download_url(source, &candidate.tag, SIGNATURE_ASSET);
    let signature = download_exact_bytes(&client, &signature_url, candidate.signature_size).await?;
    verify_checksums(&checksums, &signature, &candidate.tag, source.public_keys)?;
    let expected_digest = checksum_for(&checksums, &candidate.archive)?;
    let archive_url = download_url(source, &candidate.tag, &candidate.archive);
    let mut archive = download_archive(
        &client,
        &archive_url,
        candidate.archive_size,
        &expected_digest,
    )
    .await?;
    let staged = stage_binary(&mut archive, &managed.executable)?;
    validate_candidate(&staged, &candidate.version).await?;

    managed.revalidate(&running).await?;
    ensure_no_detached_owner(session_directory)?;
    replace_binary(&managed.executable, &staged)?;

    Ok(format!(
        "{INSTALLED_PREFIX} {installed} to {} at {}. New sessions will use it.",
        candidate.version,
        managed.executable.display()
    ))
}

/// Holds a shared installation lock for a managed detached owner's lifetime.
pub async fn managed_session_guard() -> anyhow::Result<Option<ManagedSessionGuard>> {
    let executable = canonical_executable()?;
    if !receipt_path(&executable).exists() {
        return Ok(None);
    }
    validate_receipt(&executable)?;
    let lock_path = lock_path(&executable);
    ensure_regular_file(&lock_path, "update lock")?;
    let lock = File::open(&lock_path)
        .with_context(|| format!("could not open update lock {}", lock_path.display()))?;
    lock.try_lock_shared().map_err(|error| match error {
        fs::TryLockError::WouldBlock => {
            anyhow::anyhow!("Red is being updated; retry detached startup when it finishes")
        }
        fs::TryLockError::Error(error) => {
            anyhow::Error::new(error).context("could not lock the managed Red installation")
        }
    })?;
    let installed = executable_version(&executable).await?;
    let running = Version::parse(env!("CARGO_PKG_VERSION"))?;
    anyhow::ensure!(
        installed == running,
        "Red changed on disk after this process started; relaunch before detaching"
    );
    Ok(Some(ManagedSessionGuard { _lock: lock }))
}

impl ManagedInstall {
    fn acquire(executable: PathBuf) -> anyhow::Result<Self> {
        validate_receipt(&executable)?;
        let identity = same_file::Handle::from_path(&executable)
            .with_context(|| format!("could not identify {}", executable.display()))?;
        let lock_path = lock_path(&executable);
        ensure_regular_file(&lock_path, "update lock")?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("could not open update lock {}", lock_path.display()))?;
        lock.try_lock().map_err(|error| match error {
            fs::TryLockError::WouldBlock => {
                anyhow::Error::new(Deferred("another Red update or detached session is active"))
            }
            fs::TryLockError::Error(error) => {
                anyhow::Error::new(error).context("could not lock the managed Red installation")
            }
        })?;
        let parent = executable
            .parent()
            .context("Red executable has no installation directory")?;
        Builder::new()
            .prefix(".red.update-write-test.")
            .tempfile_in(parent)
            .with_context(|| format!("Red installation is not writable: {}", parent.display()))?;
        Ok(Self {
            executable,
            identity,
            _lock: lock,
        })
    }

    async fn revalidate(&self, running: &Version) -> anyhow::Result<()> {
        let current = same_file::Handle::from_path(&self.executable)
            .context("the managed Red executable disappeared during the update")?;
        anyhow::ensure!(
            current == self.identity,
            "the managed Red executable changed during the update; retry"
        );
        let installed = executable_version(&self.executable).await?;
        anyhow::ensure!(
            installed == *running,
            "the managed Red executable changed during the update; retry"
        );
        Ok(())
    }
}

fn canonical_executable() -> anyhow::Result<PathBuf> {
    std::env::current_exe()?
        .canonicalize()
        .context("could not resolve the running Red executable")
}

fn receipt_path(executable: &Path) -> PathBuf {
    executable.with_file_name(RECEIPT_NAME)
}

fn lock_path(executable: &Path) -> PathBuf {
    executable.with_file_name(LOCK_NAME)
}

fn validate_receipt(executable: &Path) -> anyhow::Result<()> {
    let path = receipt_path(executable);
    if !path.exists() {
        return Err(unmanaged_install_error(executable));
    }
    ensure_regular_file(&path, "installation receipt")?;
    let receipt: InstallReceipt = toml::from_str(
        &fs::read_to_string(&path).with_context(|| format!("could not read {}", path.display()))?,
    )
    .context("the standalone installation receipt is invalid")?;
    anyhow::ensure!(
        receipt.schema_version == 1,
        "unsupported installation receipt"
    );
    anyhow::ensure!(
        receipt.source == "standalone",
        "this Red installation is not owned by the standalone updater"
    );
    let target = release_target()?;
    anyhow::ensure!(
        receipt.target == target,
        "installation target {} does not match this build ({target})",
        receipt.target
    );
    Ok(())
}

fn ensure_regular_file(path: &Path, label: &str) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("missing {label}: {}", path.display()))?;
    anyhow::ensure!(
        metadata.file_type().is_file() && !metadata.file_type().is_symlink(),
        "{label} must be a regular file: {}",
        path.display()
    );
    Ok(())
}

fn unmanaged_install_error(executable: &Path) -> anyhow::Error {
    if executable
        .components()
        .any(|part| part.as_os_str() == "Cellar")
    {
        anyhow::anyhow!(
            "this Red installation is managed by Homebrew; run `brew upgrade codersauce/tap/red`"
        )
    } else {
        let directory = executable.parent().unwrap_or(Path::new("."));
        anyhow::anyhow!(
            "this Red installation is not managed by the standalone updater; rerun the official installer with RED_INSTALL_DIR={} once",
            directory.display()
        )
    }
}

fn release_target() -> anyhow::Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", "x86_64") if cfg!(target_env = "gnu") => {
            Ok("x86_64-unknown-linux-gnu")
        }
        ("windows", "x86_64") if cfg!(target_env = "msvc") => {
            Ok("x86_64-pc-windows-msvc")
        }
        (os, arch) => anyhow::bail!(
            "automatic update is not published for {os} {arch}; use the installation method for this platform"
        ),
    }
}

fn archive_name() -> anyhow::Result<String> {
    let extension = if cfg!(windows) { "zip" } else { "tar.gz" };
    Ok(format!("red-{}.{}", release_target()?, extension))
}

fn http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(format!("red/{} ({REPOSITORY})", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()
        .context("could not initialize the update client")
}

fn download_url(source: ReleaseSource<'_>, tag: &str, asset: &str) -> String {
    format!("{}/{tag}/{asset}", source.downloads)
}

async fn latest_candidate(
    client: &reqwest::Client,
    current: &Version,
    source: ReleaseSource<'_>,
) -> anyhow::Result<Option<Candidate>> {
    let bytes = download_bytes(client, source.api, MAX_METADATA_BYTES).await?;
    let release: LatestRelease =
        serde_json::from_slice(&bytes).context("latest Red release metadata was invalid")?;
    select_candidate(&release, current)
}

fn select_candidate(
    release: &LatestRelease,
    current: &Version,
) -> anyhow::Result<Option<Candidate>> {
    anyhow::ensure!(
        !release.draft && !release.prerelease,
        "GitHub's latest Red release is not stable"
    );
    let version_text = release
        .tag_name
        .strip_prefix('v')
        .context("latest Red release tag does not start with v")?;
    let version = Version::parse(version_text).context("latest Red release tag is not SemVer")?;
    anyhow::ensure!(version.pre.is_empty(), "latest Red release is a prerelease");
    if version <= *current {
        return Ok(None);
    }
    let archive = archive_name()?;
    let archive_asset = exact_asset(&release.assets, &archive)?;
    let checksum_asset = exact_asset(&release.assets, CHECKSUM_ASSET)?;
    let signature_asset = exact_asset(&release.assets, SIGNATURE_ASSET)?;
    anyhow::ensure!(
        archive_asset.size > 0 && archive_asset.size <= MAX_ARCHIVE_BYTES,
        "release archive size is outside Red's update limit"
    );
    anyhow::ensure!(
        checksum_asset.size > 0 && checksum_asset.size <= MAX_CHECKSUM_BYTES,
        "release checksum size is outside Red's update limit"
    );
    anyhow::ensure!(
        signature_asset.size > 0 && signature_asset.size <= MAX_SIGNATURE_BYTES,
        "release signature size is outside Red's update limit"
    );
    Ok(Some(Candidate {
        version,
        tag: release.tag_name.clone(),
        archive,
        archive_size: archive_asset.size,
        checksum_size: checksum_asset.size,
        signature_size: signature_asset.size,
    }))
}

fn exact_asset<'a>(assets: &'a [ReleaseAsset], name: &str) -> anyhow::Result<&'a ReleaseAsset> {
    let mut matching = assets.iter().filter(|asset| asset.name == name);
    let asset = matching
        .next()
        .with_context(|| format!("latest Red release is missing {name}"))?;
    anyhow::ensure!(
        matching.next().is_none(),
        "latest Red release contains duplicate {name} assets"
    );
    Ok(asset)
}

async fn response(
    client: &reqwest::Client,
    url: &str,
    expected_size: u64,
) -> anyhow::Result<reqwest::Response> {
    anyhow::ensure!(expected_size > 0, "update response has no expected size");
    let response = client.get(url).send().await?.error_for_status()?;
    if let Some(length) = response.content_length() {
        anyhow::ensure!(
            length == expected_size,
            "update response size changed (expected {expected_size}, received {length})"
        );
    }
    Ok(response)
}

async fn download_bytes(
    client: &reqwest::Client,
    url: &str,
    max_size: u64,
) -> anyhow::Result<Vec<u8>> {
    let mut response = client.get(url).send().await?.error_for_status()?;
    if let Some(length) = response.content_length() {
        anyhow::ensure!(
            length <= max_size,
            "update response exceeded its size limit"
        );
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        anyhow::ensure!(
            bytes.len().saturating_add(chunk.len()) <= max_size as usize,
            "update response exceeded its size limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn download_exact_bytes(
    client: &reqwest::Client,
    url: &str,
    expected_size: u64,
) -> anyhow::Result<Vec<u8>> {
    let bytes = download_bytes(client, url, expected_size).await?;
    anyhow::ensure!(
        bytes.len() as u64 == expected_size,
        "update response was truncated"
    );
    Ok(bytes)
}

async fn download_archive(
    client: &reqwest::Client,
    url: &str,
    expected_size: u64,
    expected_digest: &str,
) -> anyhow::Result<NamedTempFile> {
    anyhow::ensure!(
        expected_size <= MAX_ARCHIVE_BYTES,
        "release archive exceeded Red's update limit"
    );
    let mut response = response(client, url, expected_size).await?;
    let mut archive = NamedTempFile::new().context("could not create update download")?;
    let mut digest = Sha256::new();
    let mut received = 0_u64;
    while let Some(chunk) = response.chunk().await? {
        received = received.saturating_add(chunk.len() as u64);
        anyhow::ensure!(
            received <= MAX_ARCHIVE_BYTES,
            "release archive was too large"
        );
        digest.update(&chunk);
        archive.write_all(&chunk)?;
    }
    anyhow::ensure!(
        received == expected_size,
        "release archive was truncated (expected {expected_size} bytes, received {received})"
    );
    let actual_digest = format!("{:x}", digest.finalize());
    anyhow::ensure!(
        actual_digest == expected_digest,
        "checksum mismatch for the Red release archive"
    );
    archive.flush()?;
    archive.as_file().sync_all()?;
    archive.seek(SeekFrom::Start(0))?;
    Ok(archive)
}

/// Trusts `SHA256SUMS.txt` only when a release key signed it for this exact tag.
///
/// The trusted comment binds the signature to the tag, so a validly signed checksum list from
/// an older release cannot be replayed under a newer release name.
fn verify_checksums(
    checksums: &[u8],
    signature: &[u8],
    tag: &str,
    public_keys: &[&str],
) -> anyhow::Result<()> {
    let signature = std::str::from_utf8(signature).context("release signature was not UTF-8")?;
    let signature = minisign_verify::Signature::decode(signature)
        .map_err(|error| anyhow::anyhow!("release signature is malformed: {error}"))?;
    let verified = public_keys.iter().any(|key| {
        minisign_verify::PublicKey::from_base64(key)
            .is_ok_and(|key| key.verify(checksums, &signature, false).is_ok())
    });
    anyhow::ensure!(
        verified,
        "{CHECKSUM_ASSET} is not signed by a trusted Red release key"
    );
    let expected = signature_comment(tag);
    anyhow::ensure!(
        signature.trusted_comment() == expected,
        "release signature belongs to a different release (expected \"{expected}\")"
    );
    Ok(())
}

fn signature_comment(tag: &str) -> String {
    format!("red {tag} {CHECKSUM_ASSET}")
}

fn checksum_for(contents: &[u8], archive: &str) -> anyhow::Result<String> {
    let contents = std::str::from_utf8(contents).context("SHA256SUMS.txt was not UTF-8")?;
    let mut digest = None;
    for line in contents.lines() {
        let Some((hash, filename)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let filename = filename.trim_start().trim_start_matches('*');
        if filename != archive {
            continue;
        }
        anyhow::ensure!(
            digest.is_none(),
            "SHA256SUMS.txt contains duplicate {archive} entries"
        );
        anyhow::ensure!(
            hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "SHA256SUMS.txt contains an invalid digest for {archive}"
        );
        digest = Some(hash.to_ascii_lowercase());
    }
    digest.with_context(|| format!("SHA256SUMS.txt does not contain {archive}"))
}

fn stage_binary(archive: &mut NamedTempFile, installed: &Path) -> anyhow::Result<TempPath> {
    let parent = installed
        .parent()
        .context("Red executable has no installation directory")?;
    let mut staged = Builder::new()
        .prefix(".red.update.")
        .suffix(std::env::consts::EXE_SUFFIX)
        .tempfile_in(parent)
        .context("could not stage the Red update beside the installation")?;
    extract_binary(archive, staged.as_file_mut())?;
    staged.as_file_mut().flush()?;
    staged.as_file().sync_all()?;
    staged
        .as_file()
        .set_permissions(fs::metadata(installed)?.permissions())?;
    Ok(staged.into_temp_path())
}

#[cfg(unix)]
fn extract_binary(archive: &mut NamedTempFile, output: &mut File) -> anyhow::Result<()> {
    let decoder = flate2::read::GzDecoder::new(archive.as_file_mut());
    let mut archive = tar::Archive::new(decoder);
    let mut found = false;
    for entry in archive.entries()? {
        let entry = entry?;
        let path = entry.path()?;
        if path.as_ref() != Path::new("red") && path.as_ref() != Path::new("./red") {
            continue;
        }
        anyhow::ensure!(!found, "release archive contains duplicate red binaries");
        anyhow::ensure!(
            entry.header().entry_type().is_file(),
            "release archive red entry is not a regular file"
        );
        anyhow::ensure!(
            entry.size() <= MAX_BINARY_BYTES,
            "release archive red binary is too large"
        );
        let copied = std::io::copy(&mut entry.take(MAX_BINARY_BYTES + 1), output)?;
        anyhow::ensure!(
            copied <= MAX_BINARY_BYTES,
            "release archive red binary is too large"
        );
        found = true;
    }
    anyhow::ensure!(found, "release archive did not contain red");
    Ok(())
}

#[cfg(windows)]
fn extract_binary(archive: &mut NamedTempFile, output: &mut File) -> anyhow::Result<()> {
    let mut archive = zip::ZipArchive::new(archive.as_file_mut())?;
    let mut found = false;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        if entry.name()? != "red.exe" {
            continue;
        }
        anyhow::ensure!(
            !found,
            "release archive contains duplicate red.exe binaries"
        );
        anyhow::ensure!(
            entry.is_file(),
            "release archive red.exe entry is not a regular file"
        );
        anyhow::ensure!(
            entry.size() <= MAX_BINARY_BYTES,
            "release archive red.exe is too large"
        );
        let copied = std::io::copy(&mut entry.take(MAX_BINARY_BYTES + 1), output)?;
        anyhow::ensure!(
            copied <= MAX_BINARY_BYTES,
            "release archive red.exe is too large"
        );
        found = true;
    }
    anyhow::ensure!(found, "release archive did not contain red.exe");
    Ok(())
}

async fn validate_candidate(path: &Path, expected: &Version) -> anyhow::Result<()> {
    let version = executable_version(path).await?;
    anyhow::ensure!(
        version == *expected,
        "downloaded Red reported version {version}, expected {expected}"
    );
    let output = run_bounded(path, &["--self-check"]).await?;
    anyhow::ensure!(
        output.status.success(),
        "downloaded Red failed its self-check: {}",
        command_failure(&output)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    anyhow::ensure!(
        stdout.lines().last() == Some("red self-check ok"),
        "downloaded Red returned unexpected self-check output"
    );
    Ok(())
}

async fn executable_version(path: &Path) -> anyhow::Result<Version> {
    let output = run_bounded(path, &["--version"]).await?;
    anyhow::ensure!(
        output.status.success(),
        "could not read installed Red version: {}",
        command_failure(&output)
    );
    let stdout = String::from_utf8(output.stdout).context("Red version output was not UTF-8")?;
    let version = stdout
        .split_whitespace()
        .nth(1)
        .context("Red version output was invalid")?;
    Version::parse(version).context("Red version output was not SemVer")
}

async fn run_bounded(path: &Path, arguments: &[&str]) -> anyhow::Result<CommandOutput> {
    let mut child = tokio::process::Command::new(path)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("could not run {}", path.display()))?;
    let stdout = child
        .stdout
        .take()
        .context("child stdout was unavailable")?;
    let stderr = child
        .stderr
        .take()
        .context("child stderr was unavailable")?;
    let execution = async {
        let (stdout, stderr, status) =
            tokio::try_join!(read_bounded(stdout), read_bounded(stderr), async {
                child.wait().await.map_err(anyhow::Error::from)
            })?;
        Ok::<_, anyhow::Error>(CommandOutput {
            stdout,
            stderr,
            status,
        })
    };
    match tokio::time::timeout(COMMAND_TIMEOUT, execution).await {
        Ok(result) => result,
        Err(_) => {
            let _ = child.kill().await;
            anyhow::bail!("{} timed out", path.display())
        }
    }
}

async fn read_bounded(reader: impl tokio::io::AsyncRead + Unpin) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_COMMAND_OUTPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .await?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_COMMAND_OUTPUT_BYTES,
        "Red validation output exceeded its size limit"
    );
    Ok(bytes)
}

fn command_failure(output: &CommandOutput) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        output.status.to_string()
    } else {
        stderr.to_string()
    }
}

#[cfg(unix)]
fn replace_binary(installed: &Path, staged: &Path) -> anyhow::Result<()> {
    fs::rename(staged, installed).context("could not replace the Red executable")
}

#[cfg(windows)]
fn replace_binary(installed: &Path, staged: &Path) -> anyhow::Result<()> {
    let recovery = installed.with_file_name(".red-update-recovery.exe");
    anyhow::ensure!(
        !recovery.exists(),
        "a previous recovery copy remains at {}; preserve it and rerun the installer",
        recovery.display()
    );
    fs::copy(installed, &recovery).context("could not create the Red recovery copy")?;
    OpenOptions::new().write(true).open(&recovery)?.sync_all()?;
    if let Err(error) = self_replace::self_replace(staged) {
        if !installed.exists() {
            fs::copy(&recovery, installed).with_context(|| {
                format!(
                    "replacement failed ({error}); automatic recovery also failed; restore {} manually",
                    recovery.display()
                )
            })?;
        }
        return Err(anyhow::Error::new(error).context(format!(
            "could not replace Red; recovery copy retained at {}",
            recovery.display()
        )));
    }
    fs::remove_file(&recovery)
        .context("Red was updated but its recovery copy could not be removed")?;
    Ok(())
}

#[cfg(unix)]
fn ensure_no_detached_owner(directory: &Path) -> anyhow::Result<()> {
    if crate::headless::any_session_active(directory)? {
        return Err(
            Deferred("a detached Red session is active; save and stop it before updating").into(),
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_no_detached_owner(_directory: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::{collections::HashMap, os::unix::fs::PermissionsExt as _};
    #[cfg(unix)]
    use tokio::io::AsyncWriteExt as _;

    fn release(version: &str) -> LatestRelease {
        LatestRelease {
            tag_name: format!("v{version}"),
            draft: false,
            prerelease: false,
            assets: vec![
                ReleaseAsset {
                    name: archive_name().unwrap(),
                    size: 1024,
                },
                ReleaseAsset {
                    name: CHECKSUM_ASSET.to_string(),
                    size: 72,
                },
                ReleaseAsset {
                    name: SIGNATURE_ASSET.to_string(),
                    size: 300,
                },
            ],
        }
    }

    /// Produces minisign-compatible prehashed signatures without the minisign CLI.
    struct TestKey {
        signing: ed25519_dalek::SigningKey,
        id: [u8; 8],
    }

    impl TestKey {
        fn new(seed: u8) -> Self {
            Self {
                signing: ed25519_dalek::SigningKey::from_bytes(&[seed; 32]),
                id: [seed; 8],
            }
        }

        fn public_key(&self) -> String {
            use base64::Engine as _;

            let mut key = b"Ed".to_vec();
            key.extend_from_slice(&self.id);
            key.extend_from_slice(self.signing.verifying_key().as_bytes());
            base64::engine::general_purpose::STANDARD.encode(key)
        }

        fn sign(&self, data: &[u8], trusted_comment: &str) -> Vec<u8> {
            use base64::Engine as _;
            use ed25519_dalek::Signer as _;

            let signature = self.signing.sign(&blake2::Blake2b512::digest(data));
            let mut encoded = b"ED".to_vec();
            encoded.extend_from_slice(&self.id);
            encoded.extend_from_slice(&signature.to_bytes());
            let mut global = signature.to_bytes().to_vec();
            global.extend_from_slice(trusted_comment.as_bytes());
            let global = self.signing.sign(&global);
            let engine = base64::engine::general_purpose::STANDARD;
            format!(
                "untrusted comment: test signature\n{}\ntrusted comment: {trusted_comment}\n{}\n",
                engine.encode(encoded),
                engine.encode(global.to_bytes())
            )
            .into_bytes()
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Fault {
        None,
        Checksum,
        UntrustedKey,
        ReplayedTag,
    }

    #[cfg(unix)]
    fn write_managed_install(directory: &Path, version: &Version) -> PathBuf {
        let executable = directory.join("red");
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\ncase \"$1\" in\n  --version) echo 'red {version}' ;;\n  --self-check) echo 'red self-check ok' ;;\n  *) exit 1 ;;\nesac\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(directory.join(LOCK_NAME), "").unwrap();
        fs::write(
            directory.join(RECEIPT_NAME),
            format!(
                "schema_version = 1\nsource = \"standalone\"\ntarget = \"{}\"\n",
                release_target().unwrap()
            ),
        )
        .unwrap();
        executable
    }

    #[cfg(unix)]
    fn release_routes(fault: Fault, key: &TestKey) -> HashMap<String, Vec<u8>> {
        let candidate = b"#!/bin/sh\ncase \"$1\" in\n  --version) echo 'red 9.9.9' ;;\n  --self-check) echo 'red self-check ok' ;;\n  *) exit 1 ;;\nesac\n";
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(candidate.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "./red", &candidate[..])
            .unwrap();
        let archive = builder.into_inner().unwrap().finish().unwrap();
        let archive_name = archive_name().unwrap();
        let digest = if fault == Fault::Checksum {
            "0".repeat(64)
        } else {
            format!("{:x}", Sha256::digest(&archive))
        };
        let checksums = format!("{digest}  {archive_name}\n").into_bytes();
        let signer = if fault == Fault::UntrustedKey {
            TestKey::new(2)
        } else {
            TestKey::new(key.id[0])
        };
        let tag = if fault == Fault::ReplayedTag {
            "v9.9.8"
        } else {
            "v9.9.9"
        };
        let signature = signer.sign(&checksums, &signature_comment(tag));
        let metadata = serde_json::to_vec(&serde_json::json!({
            "tag_name": "v9.9.9",
            "draft": false,
            "prerelease": false,
            "assets": [
                { "name": &archive_name, "size": archive.len() },
                { "name": CHECKSUM_ASSET, "size": checksums.len() },
                { "name": SIGNATURE_ASSET, "size": signature.len() }
            ]
        }))
        .unwrap();
        HashMap::from([
            ("/latest".to_string(), metadata),
            ("/downloads/v9.9.9/SHA256SUMS.txt".to_string(), checksums),
            (
                "/downloads/v9.9.9/SHA256SUMS.txt.minisig".to_string(),
                signature,
            ),
            (format!("/downloads/v9.9.9/{archive_name}"), archive),
        ])
    }

    #[cfg(unix)]
    async fn serve_release(routes: HashMap<String, Vec<u8>>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = [0_u8; 4096];
                let Ok(length) = stream.read(&mut request).await else {
                    continue;
                };
                let request = String::from_utf8_lossy(&request[..length]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1));
                let (status, body) = path
                    .and_then(|path| routes.get(path))
                    .map_or(("404 Not Found", &b"not found"[..]), |body| {
                        ("200 OK", body.as_slice())
                    });
                let headers = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(headers.as_bytes()).await;
                let _ = stream.write_all(body).await;
            }
        });
        format!("http://{address}")
    }

    #[cfg(unix)]
    async fn install_fixture(
        fault: Fault,
    ) -> (tempfile::TempDir, PathBuf, Vec<u8>, anyhow::Result<String>) {
        let directory = tempfile::TempDir::new().unwrap();
        let running = Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
        let executable = write_managed_install(directory.path(), &running);
        let before = fs::read(&executable).unwrap();
        let key = TestKey::new(1);
        let public_key = key.public_key();
        let server = serve_release(release_routes(fault, &key)).await;
        let api = format!("{server}/latest");
        let downloads = format!("{server}/downloads");
        let result = install_from(
            executable.clone(),
            running,
            ReleaseSource {
                api: &api,
                downloads: &downloads,
                public_keys: &[public_key.as_str()],
            },
            &directory.path().join("run"),
        )
        .await;
        (directory, executable, before, result)
    }

    #[test]
    fn latest_stable_release_must_be_strictly_newer() {
        let current = Version::parse("0.8.1").unwrap();
        assert!(select_candidate(&release("0.8.1"), &current)
            .unwrap()
            .is_none());
        assert_eq!(
            select_candidate(&release("1.0.0"), &current)
                .unwrap()
                .unwrap()
                .version,
            Version::parse("1.0.0").unwrap()
        );

        let mut prerelease = release("0.9.0-beta.1");
        prerelease.prerelease = true;
        assert!(select_candidate(&prerelease, &current).is_err());
    }

    #[test]
    fn release_requires_one_archive_and_checksum_asset() {
        let current = Version::parse("0.8.1").unwrap();
        let mut missing = release("0.8.2");
        missing.assets.pop();
        assert!(select_candidate(&missing, &current).is_err());

        let mut duplicate = release("0.8.2");
        duplicate.assets.push(duplicate.assets[0].clone());
        assert!(select_candidate(&duplicate, &current).is_err());
    }

    #[test]
    fn checksum_entry_must_be_unique_and_well_formed() {
        let archive = "red-test.tar.gz";
        let hash = "a".repeat(64);
        assert_eq!(
            checksum_for(format!("{hash}  {archive}\n").as_bytes(), archive).unwrap(),
            hash
        );
        assert!(checksum_for(
            format!("{hash}  {archive}\n{hash} *{archive}\n").as_bytes(),
            archive
        )
        .is_err());
        assert!(checksum_for(format!("xyz  {archive}\n").as_bytes(), archive).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn managed_install_downloads_validates_and_replaces() {
        let (_directory, executable, before, result) = install_fixture(Fault::None).await;
        let message = result.unwrap();
        let after = fs::read(&executable).unwrap();
        assert_ne!(Sha256::digest(before), Sha256::digest(after));
        assert!(message.starts_with("Updated Red"));
        assert_eq!(
            executable_version(&executable).await.unwrap(),
            Version::parse("9.9.9").unwrap()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn checksum_failure_preserves_the_installed_binary() {
        let (_directory, executable, before, result) = install_fixture(Fault::Checksum).await;
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("checksum mismatch"));
        assert_eq!(fs::read(executable).unwrap(), before);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn untrusted_signature_preserves_the_installed_binary() {
        let (_directory, executable, before, result) = install_fixture(Fault::UntrustedKey).await;
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("not signed by a trusted Red release key"));
        assert_eq!(fs::read(executable).unwrap(), before);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn signature_for_another_tag_preserves_the_installed_binary() {
        let (_directory, executable, before, result) = install_fixture(Fault::ReplayedTag).await;
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("belongs to a different release"));
        assert_eq!(fs::read(executable).unwrap(), before);
    }

    #[test]
    fn official_release_keys_are_well_formed() {
        for key in RELEASE_PUBLIC_KEYS {
            minisign_verify::PublicKey::from_base64(key).unwrap();
        }
        // The release workflow verifies its signature with this file before uploading.
        let published = include_str!("../install/release-signing.pub");
        let published = published.lines().last().unwrap().trim();
        assert!(RELEASE_PUBLIC_KEYS.contains(&published));
    }

    #[test]
    fn checksums_verify_against_any_trusted_key() {
        let retiring = TestKey::new(3);
        let current = TestKey::new(4);
        let checksums = b"checksums";
        let signature = retiring.sign(checksums, &signature_comment("v1.0.0"));
        let keys = [current.public_key(), retiring.public_key()];
        let keys = [keys[0].as_str(), keys[1].as_str()];
        verify_checksums(checksums, &signature, "v1.0.0", &keys).unwrap();
        assert!(verify_checksums(b"tampered", &signature, "v1.0.0", &keys).is_err());
        assert!(verify_checksums(checksums, &signature, "v1.0.0", &keys[..1]).is_err());
    }

    #[test]
    fn transient_lock_conflicts_are_deferred() {
        let directory = tempfile::TempDir::new().unwrap();
        let executable = write_managed_install(directory.path(), &Version::parse("0.8.1").unwrap());
        let owner = File::open(directory.path().join(LOCK_NAME)).unwrap();
        owner.try_lock_shared().unwrap();
        let error = ManagedInstall::acquire(executable).err().unwrap();
        assert!(error.downcast_ref::<Deferred>().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn shared_session_lock_blocks_replacement() {
        let directory = tempfile::TempDir::new().unwrap();
        let executable = write_managed_install(directory.path(), &Version::parse("0.8.1").unwrap());
        let owner = File::open(directory.path().join(LOCK_NAME)).unwrap();
        owner.try_lock_shared().unwrap();
        assert!(ManagedInstall::acquire(executable.clone()).is_err());
        drop(owner);
        // flock belongs to the open file description, which a child forked by a
        // concurrently running test inherits until it execs. The released lock
        // can therefore stay held for a moment, so retry instead of expecting
        // the very next attempt to succeed.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match ManagedInstall::acquire(executable.clone()) {
                Ok(_) => break,
                Err(_) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("lock was not released: {error:#}"),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn tar_extraction_rejects_duplicate_binary_entries() {
        let mut archive = NamedTempFile::new().unwrap();
        {
            let encoder = flate2::write::GzEncoder::new(
                archive.as_file_mut(),
                flate2::Compression::default(),
            );
            let mut builder = tar::Builder::new(encoder);
            for path in ["red", "./red"] {
                let mut header = tar::Header::new_gnu();
                header.set_size(3);
                header.set_mode(0o755);
                header.set_cksum();
                builder.append_data(&mut header, path, &b"red"[..]).unwrap();
            }
            builder.into_inner().unwrap().finish().unwrap();
        }
        archive.seek(SeekFrom::Start(0)).unwrap();
        let mut output = NamedTempFile::new().unwrap();
        assert!(extract_binary(&mut archive, output.as_file_mut()).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn candidate_reports_the_selected_version_and_passes_self_check() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::TempDir::new().unwrap();
        let candidate = directory.path().join("red candidate");
        fs::write(
            &candidate,
            "#!/bin/sh\ncase \"$1\" in\n  --version) echo 'red 9.9.9' ;;\n  --self-check) echo 'red self-check ok' ;;\n  *) exit 1 ;;\nesac\n",
        )
        .unwrap();
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o755)).unwrap();

        validate_candidate(&candidate, &Version::parse("9.9.9").unwrap())
            .await
            .unwrap();
        assert!(
            validate_candidate(&candidate, &Version::parse("9.9.8").unwrap())
                .await
                .is_err()
        );
    }
}
