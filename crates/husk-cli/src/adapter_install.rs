use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::Context;
use husk::{
    CrateExtensionSource, LOCK_FILE, LockedCrateExtension, LockedExtension, PackageLimits,
    PackageLock, PackageManifest, ResolvedPackage, installed_extension_path,
    vendored_extension_path,
};
use husk_extension::{BundleLimits, ExtensionBundle};
use semver::Version;
use sha2::{Digest, Sha256};
use toml_edit::{Document, Item, Table, value};

pub(crate) struct AdapterIdentity {
    pub(crate) name: String,
    pub(crate) version: Version,
    pub(crate) module: String,
    pub(crate) world: String,
}

pub(crate) struct AdapterSource {
    pub(crate) declaration: CrateExtensionSource,
    pub(crate) checksum: Option<String>,
}

#[derive(Debug)]
pub(crate) struct InstallOutput {
    pub(crate) bundle: PathBuf,
    pub(crate) digest: String,
}

pub(crate) fn install(
    manifest_path: &Path,
    identity: &AdapterIdentity,
    component: &[u8],
    report: &[u8],
    source: &AdapterSource,
    locked: bool,
) -> anyhow::Result<InstallOutput> {
    anyhow::ensure!(
        report.len() <= 16 * 1024 * 1024,
        "adapter selection report exceeds the 16 MiB install limit"
    );
    let package = ResolvedPackage::open(manifest_path, PackageLimits::default())
        .with_context(|| format!("open Husk package `{}`", manifest_path.display()))?;
    if locked {
        package
            .enforce_lock()
            .context("verify existing Husk.lock before installation")?;
    }
    anyhow::ensure!(
        !package.manifest.extensions.contains_key(&identity.name),
        "package already declares extension `{}`",
        identity.name
    );

    let digest = hex_digest(component);
    let relative_bundle = installed_extension_path(&digest);
    let vendored_bundle = vendored_extension_path(&digest);
    let updated_manifest =
        updated_manifest_source(&package.manifest_path, &identity.name, &source.declaration)?;
    PackageManifest::parse(&updated_manifest).context("validate updated Husk.toml")?;

    let mut updated_lock = package.lock.clone();
    let replaced = updated_lock.extensions.insert(
        identity.name.clone(),
        LockedExtension {
            module: identity.module.clone(),
            version: identity.version.clone(),
            source: relative_bundle.clone(),
            sha256: digest.clone(),
            crate_source: Some(LockedCrateExtension {
                package: source.declaration.package.clone(),
                version: identity.version.clone(),
                requirement: source.declaration.version.clone(),
                features: source.declaration.features.clone(),
                default_features: source.declaration.default_features,
                include: source.declaration.include.clone(),
                checksum: source.checksum.clone(),
            }),
            artifact: Some(vendored_bundle.clone()),
        },
    );
    anyhow::ensure!(
        replaced.is_none(),
        "package lock already contains extension `{}`",
        identity.name
    );
    let updated_lock = updated_lock
        .to_toml()
        .context("serialize updated Husk.lock")?;
    if locked {
        let existing = read_lock(&package.root.join(LOCK_FILE))?;
        anyhow::ensure!(
            existing == package_lock_from_source(&updated_lock)?,
            "`--locked` prevented Husk.lock from changing"
        );
    }

    let original_manifest = fs::read(&package.manifest_path)
        .with_context(|| format!("read `{}`", package.manifest_path.display()))?;
    let lock_path = package.root.join(LOCK_FILE);
    let original_lock = if lock_path.exists() {
        let metadata = fs::symlink_metadata(&lock_path)
            .with_context(|| format!("inspect `{}`", lock_path.display()))?;
        anyhow::ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "lock path `{}` is not a regular file",
            lock_path.display()
        );
        Some(fs::read(&lock_path).with_context(|| format!("read `{}`", lock_path.display()))?)
    } else {
        None
    };
    let extensions = package.root.join(".husk").join("extensions");
    let vendor = package.root.join("vendor");
    let vendor_husk = vendor.join("husk");
    let created_husk = ensure_directory(&package.root.join(".husk"))?;
    let created_extensions = match ensure_directory(&extensions) {
        Ok(created) => created,
        Err(error) => {
            remove_if_empty(&package.root.join(".husk"), created_husk);
            return Err(error);
        }
    };
    let created_vendor = match ensure_directory(&vendor) {
        Ok(created) => created,
        Err(error) => {
            remove_if_empty(&extensions, created_extensions);
            remove_if_empty(&package.root.join(".husk"), created_husk);
            return Err(error);
        }
    };
    let created_vendor_husk = match ensure_directory(&vendor_husk) {
        Ok(created) => created,
        Err(error) => {
            remove_if_empty(&vendor, created_vendor);
            remove_if_empty(&extensions, created_extensions);
            remove_if_empty(&package.root.join(".husk"), created_husk);
            return Err(error);
        }
    };
    let vendor_bundle = package.root.join(&vendored_bundle);
    let created_vendor_bundle = match publish_bundle(
        &vendor_husk,
        &vendor_bundle,
        identity,
        component,
        report,
        &digest,
    ) {
        Ok(created) => created,
        Err(error) => {
            remove_if_empty(&vendor_husk, created_vendor_husk);
            remove_if_empty(&vendor, created_vendor);
            remove_if_empty(&extensions, created_extensions);
            remove_if_empty(&package.root.join(".husk"), created_husk);
            return Err(error);
        }
    };
    let bundle = package.root.join(&relative_bundle);
    let created_bundle =
        match publish_bundle(&extensions, &bundle, identity, component, report, &digest) {
            Ok(created) => created,
            Err(error) => {
                if created_vendor_bundle && let Err(cleanup) = fs::remove_dir_all(&vendor_bundle) {
                    return Err(anyhow::anyhow!(
                        "{error:#}; rollback could not remove `{}`: {cleanup}",
                        vendor_bundle.display()
                    ));
                }
                remove_if_empty(&vendor_husk, created_vendor_husk);
                remove_if_empty(&vendor, created_vendor);
                remove_if_empty(&extensions, created_extensions);
                remove_if_empty(&package.root.join(".husk"), created_husk);
                return Err(error);
            }
        };

    let publication = publish_package_files(
        &package.manifest_path,
        updated_manifest.as_bytes(),
        &lock_path,
        updated_lock.as_bytes(),
        &original_manifest,
        original_lock.as_deref(),
    )
    .and_then(|()| {
        let installed = ResolvedPackage::open(&package.manifest_path, PackageLimits::default())
            .context("validate installed Husk package")?;
        installed
            .enforce_lock()
            .context("validate installed Husk.lock")
    });
    if let Err(error) = publication {
        let rollback = replace_file(&package.manifest_path, &original_manifest)
            .and_then(|()| restore_optional_file(&lock_path, original_lock.as_deref()));
        if created_bundle && let Err(cleanup) = fs::remove_dir_all(&bundle) {
            return Err(anyhow::anyhow!(
                "{error:#}; rollback could not remove `{}`: {cleanup}",
                bundle.display()
            ));
        }
        if created_vendor_bundle && let Err(cleanup) = fs::remove_dir_all(&vendor_bundle) {
            return Err(anyhow::anyhow!(
                "{error:#}; rollback could not remove `{}`: {cleanup}",
                vendor_bundle.display()
            ));
        }
        remove_if_empty(&vendor_husk, created_vendor_husk);
        remove_if_empty(&vendor, created_vendor);
        remove_if_empty(&extensions, created_extensions);
        remove_if_empty(&package.root.join(".husk"), created_husk);
        if let Err(rollback) = rollback {
            return Err(anyhow::anyhow!(
                "{error:#}; package rollback failed: {rollback:#}"
            ));
        }
        return Err(error);
    }

    Ok(InstallOutput {
        bundle: relative_bundle,
        digest,
    })
}

fn updated_manifest_source(
    manifest_path: &Path,
    name: &str,
    source: &CrateExtensionSource,
) -> anyhow::Result<String> {
    let manifest_source = fs::read_to_string(manifest_path)
        .with_context(|| format!("read `{}`", manifest_path.display()))?;
    let mut document = manifest_source
        .parse::<Document>()
        .context("parse Husk.toml for editing")?;
    if !document.contains_key("extensions") {
        document.insert("extensions", Item::Table(Table::new()));
    }
    let extensions = document["extensions"]
        .as_table_mut()
        .context("Husk.toml `extensions` value is not a table")?;
    anyhow::ensure!(
        !extensions.contains_key(name),
        "package already declares extension `{name}`"
    );
    let mut extension = Table::new();
    extension.insert("crate", value(&source.package));
    extension.insert("version", value(&source.version));
    if !source.features.is_empty() {
        let features = source
            .features
            .iter()
            .cloned()
            .map(toml_edit::Value::from)
            .collect::<toml_edit::Array>();
        extension.insert("features", value(features));
    }
    if !source.default_features {
        extension.insert("default_features", value(false));
    }
    if !source.include.is_empty() {
        let include = source
            .include
            .iter()
            .cloned()
            .map(toml_edit::Value::from)
            .collect::<toml_edit::Array>();
        extension.insert("include", value(include));
    }
    extensions.insert(name, Item::Table(extension));
    Ok(document.to_string())
}

fn publish_bundle(
    parent: &Path,
    destination: &Path,
    identity: &AdapterIdentity,
    component: &[u8],
    report: &[u8],
    expected_digest: &str,
) -> anyhow::Result<bool> {
    if destination.exists() {
        let existing = ExtensionBundle::open(destination, BundleLimits::default())
            .with_context(|| format!("validate installed bundle `{}`", destination.display()))?;
        anyhow::ensure!(
            existing.digest().to_string() == expected_digest
                && existing.manifest().name == identity.name
                && existing.manifest().version == identity.version
                && existing.module().as_str() == identity.module
                && existing.manifest().world == identity.world
                && fs::read(destination.join("husk-adapter.json"))
                    .is_ok_and(|existing| existing == report),
            "digest-addressed bundle `{}` does not match the adapter being installed",
            destination.display()
        );
        return Ok(false);
    }

    let staging = tempfile::Builder::new()
        .prefix(".husk-install-")
        .tempdir_in(parent)
        .with_context(|| format!("create adapter staging directory in `{}`", parent.display()))?;
    let manifest = extension_manifest(identity);
    fs::write(staging.path().join("extension.toml"), manifest)
        .context("write staged extension manifest")?;
    fs::write(staging.path().join("component.wasm"), component)
        .context("write staged extension component")?;
    fs::write(staging.path().join("husk-adapter.json"), report)
        .context("write staged adapter selection report")?;
    let staged = ExtensionBundle::open(staging.path(), BundleLimits::default())
        .context("validate staged extension bundle")?;
    anyhow::ensure!(
        staged.digest().to_string() == expected_digest,
        "staged extension digest changed before publication"
    );
    fs::rename(staging.path(), destination)
        .with_context(|| format!("publish adapter bundle `{}`", destination.display()))?;
    Ok(true)
}

fn extension_manifest(identity: &AdapterIdentity) -> String {
    format!(
        "schema_version = 1\n\
         name = {}\n\
         version = {}\n\
         module = {}\n\
         artifact = \"component.wasm\"\n\
         world = {}\n\
         minimum_husk = {}\n\n\
         [capabilities]\n\
         requested = []\n",
        toml_string(&identity.name),
        toml_string(&identity.version.to_string()),
        toml_string(&identity.module),
        toml_string(&identity.world),
        toml_string(env!("CARGO_PKG_VERSION")),
    )
}

fn publish_package_files(
    manifest_path: &Path,
    manifest: &[u8],
    lock_path: &Path,
    lock: &[u8],
    original_manifest: &[u8],
    original_lock: Option<&[u8]>,
) -> anyhow::Result<()> {
    replace_file(manifest_path, manifest)?;
    let publication = replace_file(lock_path, lock);
    if let Err(error) = publication {
        replace_file(manifest_path, original_manifest).ok();
        restore_optional_file(lock_path, original_lock).ok();
        return Err(error);
    }
    Ok(())
}

fn restore_optional_file(path: &Path, contents: Option<&[u8]>) -> anyhow::Result<()> {
    if let Some(contents) = contents {
        replace_file(path, contents)
    } else if path.exists() {
        fs::remove_file(path).with_context(|| format!("remove `{}`", path.display()))
    } else {
        Ok(())
    }
}

fn replace_file(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create replacement for `{}`", path.display()))?;
    temporary
        .write_all(contents)
        .with_context(|| format!("write replacement for `{}`", path.display()))?;
    if let Ok(metadata) = fs::metadata(path) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())
            .with_context(|| format!("preserve permissions for `{}`", path.display()))?;
    }
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("flush replacement for `{}`", path.display()))?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("replace `{}`", path.display()))?;
    Ok(())
}

fn ensure_directory(path: &Path) -> anyhow::Result<bool> {
    if path.exists() {
        let metadata =
            fs::symlink_metadata(path).with_context(|| format!("inspect `{}`", path.display()))?;
        anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "package path `{}` is not a regular directory",
            path.display()
        );
        return Ok(false);
    }
    fs::create_dir(path).with_context(|| format!("create `{}`", path.display()))?;
    Ok(true)
}

fn remove_if_empty(path: &Path, created: bool) {
    if created {
        fs::remove_dir(path).ok();
    }
}

fn read_lock(path: &Path) -> anyhow::Result<PackageLock> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("read locked file `{}`", path.display()))?;
    package_lock_from_source(&source)
}

fn package_lock_from_source(source: &str) -> anyhow::Result<PackageLock> {
    PackageLock::parse(source).context("parse Husk.lock")
}

fn hex_digest(component: &[u8]) -> String {
    let digest = Sha256::digest(component);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).expect("serializing a string cannot fail")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("src")).unwrap();
        fs::write(
            directory.path().join("Husk.toml"),
            "# keep this comment\n\
             schema_version = 1\n\n\
             [package]\n\
             name = \"example\"\n\
             version = \"0.1.0\"\n\
             entry = \"src/main.hk\"\n",
        )
        .unwrap();
        fs::write(directory.path().join("src/main.hk"), "fn main() {}\n").unwrap();
        directory
    }

    fn identity() -> AdapterIdentity {
        AdapterIdentity {
            name: "sample-crate".to_string(),
            version: Version::new(1, 2, 3),
            module: "sample_crate".to_string(),
            world: "husk:sample-crate/sample-crate-adapter@1.2.3".to_string(),
        }
    }

    fn source() -> AdapterSource {
        AdapterSource {
            declaration: CrateExtensionSource {
                package: "sample-crate".to_string(),
                version: "^1.2".to_string(),
                features: vec!["fast".to_string()],
                default_features: false,
                include: vec!["sample_crate::run".to_string()],
            },
            checksum: Some("crate-checksum".to_string()),
        }
    }

    #[test]
    fn installs_digest_addressed_bundle_and_updates_package_files() {
        let directory = package();
        let component = b"verified component";

        let installed = install(
            &directory.path().join("Husk.toml"),
            &identity(),
            component,
            b"{\"selection\":\"automatic\"}\n",
            &source(),
            false,
        )
        .unwrap();

        assert!(directory.path().join(&installed.bundle).is_dir());
        assert_eq!(installed.digest, hex_digest(component));
        let manifest = fs::read_to_string(directory.path().join("Husk.toml")).unwrap();
        assert!(manifest.starts_with("# keep this comment\n"), "{manifest}");
        assert!(manifest.contains("[extensions.sample-crate]"), "{manifest}");
        assert!(manifest.contains("crate = \"sample-crate\""), "{manifest}");
        assert!(manifest.contains("version = \"^1.2\""), "{manifest}");
        assert!(!manifest.contains(&installed.digest), "{manifest}");
        assert!(
            directory
                .path()
                .join("vendor/husk")
                .join(format!("{}.huskext", installed.digest))
                .is_dir()
        );
        let lock = fs::read_to_string(directory.path().join("Husk.lock")).unwrap();
        assert!(lock.contains("[extensions.sample-crate]"), "{lock}");
        assert!(lock.contains(&installed.digest), "{lock}");
        let package =
            ResolvedPackage::open(directory.path().join("Husk.toml"), PackageLimits::default())
                .unwrap();
        package.enforce_lock().unwrap();
    }

    #[test]
    fn locked_add_rejects_changes_without_creating_install_state() {
        let directory = package();
        let package =
            ResolvedPackage::open(directory.path().join("Husk.toml"), PackageLimits::default())
                .unwrap();
        package.write_lock().unwrap();
        let original_manifest = fs::read(directory.path().join("Husk.toml")).unwrap();
        let original_lock = fs::read(directory.path().join("Husk.lock")).unwrap();

        let error = install(
            &directory.path().join("Husk.toml"),
            &identity(),
            b"verified component",
            b"{}\n",
            &source(),
            true,
        )
        .unwrap_err();

        assert!(error.to_string().contains("--locked"), "{error:#}");
        assert_eq!(
            fs::read(directory.path().join("Husk.toml")).unwrap(),
            original_manifest
        );
        assert_eq!(
            fs::read(directory.path().join("Husk.lock")).unwrap(),
            original_lock
        );
        assert!(!directory.path().join(".husk").exists());
    }

    #[test]
    fn vendored_bundle_reinstalls_after_the_project_state_is_removed() {
        let directory = package();
        let installed = install(
            &directory.path().join("Husk.toml"),
            &identity(),
            b"verified component",
            b"{}\n",
            &source(),
            false,
        )
        .unwrap();
        fs::write(
            directory.path().join(".husk/extensions/stale.huskext"),
            "stale",
        )
        .unwrap();
        fs::remove_dir_all(directory.path().join(&installed.bundle)).unwrap();

        let restored = crate::package_install::install(
            &directory.path().join("Husk.toml"),
            &crate::package_install::InstallOptions {
                locked: true,
                offline: true,
            },
        )
        .unwrap();

        assert_eq!(restored.extension_count, 1);
        assert!(directory.path().join(&installed.bundle).is_dir());
        assert!(
            !directory
                .path()
                .join(".husk/extensions/stale.huskext")
                .exists()
        );
        let package =
            ResolvedPackage::open(directory.path().join("Husk.toml"), PackageLimits::default())
                .unwrap();
        package.enforce_lock().unwrap();
    }

    #[test]
    fn install_rejects_a_tampered_vendor_without_replacing_working_state() {
        let directory = package();
        let installed = install(
            &directory.path().join("Husk.toml"),
            &identity(),
            b"verified component",
            b"{}\n",
            &source(),
            false,
        )
        .unwrap();
        let original_component = fs::read(
            directory
                .path()
                .join(&installed.bundle)
                .join("component.wasm"),
        )
        .unwrap();
        let vendor = directory
            .path()
            .join("vendor/husk")
            .join(format!("{}.huskext/component.wasm", installed.digest));
        fs::write(vendor, b"tampered").unwrap();

        let error = crate::package_install::install(
            &directory.path().join("Husk.toml"),
            &crate::package_install::InstallOptions {
                locked: true,
                offline: true,
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("digest"), "{error:#}");
        assert_eq!(
            fs::read(
                directory
                    .path()
                    .join(&installed.bundle)
                    .join("component.wasm")
            )
            .unwrap(),
            original_component
        );
    }

    #[test]
    fn package_file_publication_rolls_manifest_back_when_lock_fails() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = directory.path().join("Husk.toml");
        let invalid_lock = directory.path().join("Husk.lock");
        fs::write(&manifest, b"original manifest").unwrap();
        fs::create_dir(&invalid_lock).unwrap();

        let error = publish_package_files(
            &manifest,
            b"updated manifest",
            &invalid_lock,
            b"updated lock",
            b"original manifest",
            None,
        )
        .unwrap_err();

        assert!(error.to_string().contains("replace"), "{error:#}");
        assert_eq!(fs::read(&manifest).unwrap(), b"original manifest");
        assert!(invalid_lock.is_dir());
    }
}
