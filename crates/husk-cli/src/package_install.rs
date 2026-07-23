use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;
use husk::{
    ExtensionSource, LOCK_FILE, PackageLimits, PackageLock, PackageManifest, ResolvedPackage,
};
use husk_extension::{BundleLimits, ExtensionBundle};

pub(crate) struct InstallOptions {
    pub(crate) locked: bool,
    pub(crate) offline: bool,
}

#[derive(Debug)]
pub(crate) struct InstallOutput {
    pub(crate) extension_count: usize,
}

pub(crate) fn install(
    manifest_path: &Path,
    options: &InstallOptions,
) -> anyhow::Result<InstallOutput> {
    let root = manifest_path
        .parent()
        .context("Husk.toml has no package directory")?;
    let manifest_source = fs::read_to_string(manifest_path)
        .with_context(|| format!("read `{}`", manifest_path.display()))?;
    let manifest = PackageManifest::parse(&manifest_source).context("parse Husk.toml")?;
    let lock_path = root.join(LOCK_FILE);
    let lock_source = fs::read_to_string(&lock_path).with_context(|| {
        let mode = if options.locked { "locked " } else { "" };
        format!("read {mode}package file `{}`", lock_path.display())
    })?;
    let lock = PackageLock::parse(&lock_source).context("parse Husk.lock")?;
    lock.validate_manifest(&manifest)
        .context("verify Husk.toml against Husk.lock")?;

    // Version 1 installs only content-addressed vendored artifacts. The flag is
    // accepted now so the CLI contract does not change when registry fetching
    // is added; this path never accesses the network in either mode.
    let _offline = options.offline;
    let husk_directory = root.join(".husk");
    ensure_directory(&husk_directory)?;
    let staging = tempfile::Builder::new()
        .prefix(".extensions-install-")
        .tempdir_in(&husk_directory)
        .context("create extension installation staging directory")?;
    let staged_extensions = staging.path().join("extensions");
    fs::create_dir(&staged_extensions).context("create staged extension directory")?;

    let mut extension_count = 0;
    for (name, declaration) in &manifest.extensions {
        if !matches!(declaration, ExtensionSource::Crate(_)) {
            continue;
        }
        let locked = lock
            .extensions
            .get(name)
            .context("validated lock omitted a crate extension")?;
        let artifact = locked
            .artifact
            .as_ref()
            .context("validated crate extension omitted its artifact")?;
        let artifact = resolve_package_path(root, artifact)?;
        let source = ExtensionBundle::open(&artifact, BundleLimits::default())
            .with_context(|| format!("validate vendored extension `{}`", artifact.display()))?;
        anyhow::ensure!(
            source.digest().to_string() == locked.sha256,
            "vendored extension `{name}` has digest {}, expected {}",
            source.digest(),
            locked.sha256
        );
        anyhow::ensure!(
            source.manifest().name == *name,
            "vendored extension `{name}` declares package `{}`",
            source.manifest().name
        );
        if let Some(expected) = &locked.report_sha256 {
            verify_adapter_report(&artifact, name, expected)?;
        }
        let destination = staged_extensions.join(format!("{}.huskext", locked.sha256));
        copy_adapter_bundle(&artifact, &destination)?;
        let installed = ExtensionBundle::open(&destination, BundleLimits::default())
            .with_context(|| format!("verify staged extension `{}`", destination.display()))?;
        anyhow::ensure!(
            installed.digest().to_string() == locked.sha256,
            "staged extension `{name}` changed during installation"
        );
        if let Some(expected) = &locked.report_sha256 {
            verify_adapter_report(&destination, name, expected)?;
        }
        extension_count += 1;
    }

    publish_extensions(root, manifest_path, &staged_extensions)?;
    Ok(InstallOutput { extension_count })
}

fn verify_adapter_report(bundle: &Path, name: &str, expected: &str) -> anyhow::Result<()> {
    const MAX_ADAPTER_REPORT_BYTES: u64 = 16 * 1024 * 1024;

    let report_path = bundle.join("husk-adapter.json");
    let metadata = fs::symlink_metadata(&report_path)
        .with_context(|| format!("inspect extension `{name}` adapter selection report"))?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "extension `{name}` adapter selection report is not a regular file"
    );
    anyhow::ensure!(
        metadata.len() <= MAX_ADAPTER_REPORT_BYTES,
        "extension `{name}` adapter selection report exceeds the 16 MiB install limit"
    );
    let report = fs::read(&report_path)
        .with_context(|| format!("read extension `{name}` adapter selection report"))?;
    anyhow::ensure!(
        report.len() as u64 <= MAX_ADAPTER_REPORT_BYTES,
        "extension `{name}` adapter selection report exceeds the 16 MiB install limit"
    );
    let actual = crate::adapter_install::hex_digest(&report);
    anyhow::ensure!(
        actual == expected,
        "extension `{name}` adapter selection report has digest {actual}, expected {expected}"
    );
    Ok(())
}

fn publish_extensions(
    root: &Path,
    manifest_path: &Path,
    staged_extensions: &Path,
) -> anyhow::Result<()> {
    let husk_directory = root.join(".husk");
    let destination = husk_directory.join("extensions");
    reject_symlink_if_present(&destination)?;
    let backup = tempfile::Builder::new()
        .prefix(".extensions-backup-")
        .tempdir_in(&husk_directory)
        .context("create extension installation backup directory")?;
    let previous = backup.path().join("extensions");
    let had_previous = destination.exists();
    if had_previous {
        fs::rename(&destination, &previous).context("stage previous extension installation")?;
    }
    if let Err(error) = fs::rename(staged_extensions, &destination) {
        if had_previous && let Err(restore) = fs::rename(&previous, &destination) {
            return Err(anyhow::anyhow!(
                "could not publish installed extensions: {error}; rollback could not restore `{}`: {restore}",
                destination.display()
            ));
        }
        return Err(error).context("publish installed extensions");
    }

    let validation = ResolvedPackage::open(manifest_path, PackageLimits::default())
        .context("open installed Husk package")
        .and_then(|package| package.enforce_lock().context("verify installed Husk.lock"));
    if let Err(error) = validation {
        if let Err(cleanup) = fs::remove_dir_all(&destination) {
            return Err(anyhow::anyhow!(
                "{error:#}; rollback could not remove `{}`: {cleanup}",
                destination.display()
            ));
        }
        if had_previous && let Err(restore) = fs::rename(&previous, &destination) {
            return Err(anyhow::anyhow!(
                "{error:#}; rollback could not restore `{}`: {restore}",
                destination.display()
            ));
        }
        return Err(error);
    }
    Ok(())
}

fn copy_adapter_bundle(source: &Path, destination: &Path) -> anyhow::Result<()> {
    fs::create_dir(destination)
        .with_context(|| format!("create staged bundle `{}`", destination.display()))?;
    for name in ["extension.toml", "component.wasm", "husk-adapter.json"] {
        let source_file = source.join(name);
        if name == "husk-adapter.json" && !source_file.exists() {
            continue;
        }
        let metadata = fs::symlink_metadata(&source_file)
            .with_context(|| format!("inspect `{}`", source_file.display()))?;
        anyhow::ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "bundle member `{}` is not a regular file",
            source_file.display()
        );
        fs::copy(&source_file, destination.join(name))
            .with_context(|| format!("copy `{}`", source_file.display()))?;
    }
    Ok(())
}

fn resolve_package_path(root: &Path, relative: &Path) -> anyhow::Result<PathBuf> {
    let unresolved = root.join(relative);
    let metadata = fs::symlink_metadata(&unresolved)
        .with_context(|| format!("inspect `{}`", unresolved.display()))?;
    anyhow::ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "extension artifact `{}` is not a regular directory",
        unresolved.display()
    );
    let resolved = unresolved
        .canonicalize()
        .with_context(|| format!("resolve `{}`", unresolved.display()))?;
    let root = root
        .canonicalize()
        .with_context(|| format!("resolve package root `{}`", root.display()))?;
    anyhow::ensure!(
        resolved.starts_with(&root),
        "extension artifact `{}` escapes the package root",
        relative.display()
    );
    Ok(resolved)
}

fn ensure_directory(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        let metadata =
            fs::symlink_metadata(path).with_context(|| format!("inspect `{}`", path.display()))?;
        anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "package path `{}` is not a regular directory",
            path.display()
        );
    } else {
        fs::create_dir(path).with_context(|| format!("create `{}`", path.display()))?;
    }
    Ok(())
}

fn reject_symlink_if_present(path: &Path) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "installation path `{}` is not a regular directory",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("inspect `{}`", path.display())),
    }
    Ok(())
}
