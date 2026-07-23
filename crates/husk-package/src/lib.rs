//! Deterministic, filesystem-only Husk package resolution.

use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    fs::{self, File},
    io::{self, Read},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use husk_ast::{File as AstFile, ItemKind, SetFilePath};
use husk_extension::{BundleLimits, ExtensionBundle};
use semver::Version;
use serde::{Deserialize, Serialize};

pub const MANIFEST_FILE: &str = "Husk.toml";
pub const LOCK_FILE: &str = "Husk.lock";
pub const INSTALL_DIRECTORY: &str = ".husk/extensions";
pub const VENDOR_DIRECTORY: &str = "vendor/husk";
pub const SUPPORTED_MANIFEST_SCHEMA: u32 = 1;
pub const SUPPORTED_LOCK_SCHEMA: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackageLimits {
    pub max_manifest_bytes: u64,
    pub max_lock_bytes: u64,
    pub max_source_bytes: u64,
    pub max_modules: usize,
    pub bundle: BundleLimits,
}

impl Default for PackageLimits {
    fn default() -> Self {
        Self {
            max_manifest_bytes: 256 * 1024,
            max_lock_bytes: 1024 * 1024,
            max_source_bytes: 1024 * 1024,
            max_modules: 256,
            bundle: BundleLimits::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageManifest {
    #[serde(default = "default_manifest_schema")]
    pub schema_version: u32,
    pub package: PackageSection,
    #[serde(default)]
    pub extensions: BTreeMap<String, ExtensionSource>,
}

const fn default_manifest_schema() -> u32 {
    SUPPORTED_MANIFEST_SCHEMA
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSection {
    pub name: String,
    pub version: Version,
    pub entry: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ExtensionSource {
    Path(PathExtensionSource),
    Crate(CrateExtensionSource),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathExtensionSource {
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrateExtensionSource {
    #[serde(rename = "crate")]
    pub package: String,
    pub version: String,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default = "default_true")]
    pub default_features: bool,
    #[serde(default)]
    pub include: Vec<String>,
    /// Canonical, ahead-of-time Rust generic instantiations for this adapter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub specializations: Vec<String>,
}

const fn default_true() -> bool {
    true
}

impl PackageManifest {
    pub fn parse(source: &str) -> Result<Self, PackageError> {
        let manifest: Self =
            toml::from_str(source).map_err(|error| PackageError::InvalidManifest {
                message: error.to_string(),
            })?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), PackageError> {
        if self.schema_version != SUPPORTED_MANIFEST_SCHEMA {
            return Err(PackageError::UnsupportedManifestSchema(self.schema_version));
        }
        validate_package_name(&self.package.name)?;
        validate_local_path(&self.package.entry, "package entry")?;
        for (name, extension) in &self.extensions {
            validate_package_name(name)?;
            match extension {
                ExtensionSource::Path(extension) => {
                    validate_local_path(&extension.path, "extension")?;
                }
                ExtensionSource::Crate(extension) => {
                    validate_package_name(&extension.package)?;
                    semver::VersionReq::parse(&extension.version).map_err(|error| {
                        PackageError::InvalidManifest {
                            message: format!(
                                "invalid crate version requirement `{}` for extension `{name}`: {error}",
                                extension.version
                            ),
                        }
                    })?;
                    if extension.include.iter().any(|path| path.is_empty()) {
                        return Err(PackageError::InvalidManifest {
                            message: format!(
                                "crate extension `{name}` contains an empty include path"
                            ),
                        });
                    }
                    if extension
                        .specializations
                        .iter()
                        .any(|specialization| specialization.is_empty())
                    {
                        return Err(PackageError::InvalidManifest {
                            message: format!(
                                "crate extension `{name}` contains an empty generic specialization"
                            ),
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct SourceModule {
    pub module_path: Vec<String>,
    pub canonical_path: PathBuf,
    pub display_path: PathBuf,
    pub source: Arc<str>,
    pub syntax: AstFile,
}

#[derive(Debug, Clone)]
pub struct ResolvedExtension {
    pub manifest_name: String,
    pub source: PathBuf,
    pub bundle: ExtensionBundle,
    pub crate_source: Option<LockedCrateExtension>,
    pub artifact: Option<PathBuf>,
    pub report_sha256: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedPackage {
    pub root: PathBuf,
    pub source_root: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest: PackageManifest,
    pub modules: Vec<SourceModule>,
    pub extensions: Vec<ResolvedExtension>,
    pub lock: PackageLock,
    limits: PackageLimits,
}

impl ResolvedPackage {
    pub fn open(
        manifest_path: impl AsRef<Path>,
        limits: PackageLimits,
    ) -> Result<Self, PackageError> {
        let unresolved_manifest = manifest_path.as_ref();
        reject_symlink(unresolved_manifest)?;
        let manifest_path =
            unresolved_manifest
                .canonicalize()
                .map_err(|source| PackageError::Io {
                    path: unresolved_manifest.to_path_buf(),
                    source,
                })?;
        if manifest_path.file_name().and_then(|name| name.to_str()) != Some(MANIFEST_FILE) {
            return Err(PackageError::InvalidManifest {
                message: format!("package manifest must be named `{MANIFEST_FILE}`"),
            });
        }
        let root = manifest_path
            .parent()
            .expect("a manifest path has a parent")
            .to_path_buf();
        let manifest_bytes = read_bounded(&manifest_path, limits.max_manifest_bytes)?;
        let manifest_source = std::str::from_utf8(&manifest_bytes).map_err(|error| {
            PackageError::InvalidManifest {
                message: format!("manifest is not UTF-8: {error}"),
            }
        })?;
        let manifest = PackageManifest::parse(manifest_source)?;

        let entry_unresolved = root.join(&manifest.package.entry);
        reject_symlink(&entry_unresolved)?;
        let entry = entry_unresolved
            .canonicalize()
            .map_err(|source| PackageError::Io {
                path: entry_unresolved.clone(),
                source,
            })?;
        if !entry.starts_with(&root) {
            return Err(PackageError::PathEscapesRoot(entry));
        }
        if !entry.is_file() {
            return Err(PackageError::NotFile(entry));
        }
        let source_root = entry
            .parent()
            .expect("entry source has a parent")
            .canonicalize()
            .map_err(|source| PackageError::Io {
                path: entry.clone(),
                source,
            })?;
        let mut resolver = ModuleResolver::new(source_root.clone(), limits);
        resolver.resolve(Vec::new(), entry)?;
        let modules = resolver.finish();

        let locked = if manifest
            .extensions
            .values()
            .any(|source| matches!(source, ExtensionSource::Crate(_)))
        {
            Some(read_package_lock(
                &root.join(LOCK_FILE),
                limits.max_lock_bytes,
            )?)
        } else {
            None
        };
        if let Some(locked) = &locked {
            locked.validate_manifest(&manifest)?;
        }

        let mut extensions = Vec::new();
        for (name, declaration) in &manifest.extensions {
            let (source, crate_source, artifact, report_sha256) = match declaration {
                ExtensionSource::Path(extension) => (extension.path.clone(), None, None, None),
                ExtensionSource::Crate(_) => {
                    let locked_extension = locked
                        .as_ref()
                        .and_then(|lock| lock.extensions.get(name))
                        .ok_or_else(|| PackageError::InvalidLock {
                            message: format!(
                                "crate extension `{name}` is missing from Husk.lock; run `husk add` or `husk install`"
                            ),
                        })?;
                    (
                        locked_extension.source.clone(),
                        locked_extension.crate_source.clone(),
                        locked_extension.artifact.clone(),
                        locked_extension.report_sha256.clone(),
                    )
                }
            };
            let unresolved = root.join(&source);
            reject_symlink(&unresolved)?;
            let canonical = unresolved
                .canonicalize()
                .map_err(|error| PackageError::Io {
                    path: unresolved.clone(),
                    source: error,
                })?;
            if !canonical.starts_with(&root) {
                return Err(PackageError::PathEscapesRoot(canonical));
            }
            let bundle = ExtensionBundle::open(&canonical, limits.bundle).map_err(|source| {
                PackageError::Extension {
                    name: name.clone(),
                    source,
                }
            })?;
            if bundle.manifest().name != *name {
                return Err(PackageError::InvalidManifest {
                    message: format!(
                        "extension key `{name}` does not match bundle package `{}`",
                        bundle.manifest().name
                    ),
                });
            }
            extensions.push(ResolvedExtension {
                manifest_name: name.clone(),
                source,
                bundle,
                crate_source,
                artifact,
                report_sha256,
            });
        }
        extensions.sort_unstable_by(|left, right| left.manifest_name.cmp(&right.manifest_name));
        let lock = PackageLock::from_resolved(&manifest, &extensions);

        Ok(Self {
            root,
            source_root,
            manifest_path,
            manifest,
            modules,
            extensions,
            lock,
            limits,
        })
    }

    pub fn enforce_lock(&self) -> Result<(), PackageError> {
        let path = self.root.join(LOCK_FILE);
        reject_symlink(&path)?;
        let bytes = read_bounded(&path, self.limits.max_lock_bytes)?;
        let source = std::str::from_utf8(&bytes).map_err(|error| PackageError::InvalidLock {
            message: format!("lock file is not UTF-8: {error}"),
        })?;
        let actual = PackageLock::parse(source)?;
        if actual != self.lock {
            return Err(PackageError::LockChanged {
                path,
                expected: Box::new(self.lock.clone()),
                actual: Box::new(actual),
            });
        }
        Ok(())
    }

    pub fn write_lock(&self) -> Result<(), PackageError> {
        let path = self.root.join(LOCK_FILE);
        if path.exists() {
            reject_symlink(&path)?;
        }
        let source = self.lock.to_toml()?;
        fs::write(&path, source).map_err(|source| PackageError::Io { path, source })
    }
}

/// Find the nearest `Husk.toml`, starting at a file's parent or at a
/// directory itself.
pub fn discover_manifest(start: impl AsRef<Path>) -> Result<PathBuf, PackageError> {
    let start = start.as_ref();
    let mut current = if start.is_file() {
        start.parent().unwrap_or(start)
    } else {
        start
    }
    .canonicalize()
    .map_err(|source| PackageError::Io {
        path: start.to_path_buf(),
        source,
    })?;
    loop {
        let candidate = current.join(MANIFEST_FILE);
        if candidate.is_file() {
            reject_symlink(&candidate)?;
            return candidate.canonicalize().map_err(|source| PackageError::Io {
                path: candidate,
                source,
            });
        }
        if !current.pop() {
            return Err(PackageError::ManifestNotFound(start.to_path_buf()));
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageLock {
    pub schema_version: u32,
    pub package: LockedPackage,
    #[serde(default)]
    pub extensions: BTreeMap<String, LockedExtension>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedPackage {
    pub name: String,
    pub version: Version,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedExtension {
    pub module: String,
    pub version: Version,
    pub source: PathBuf,
    pub sha256: String,
    #[serde(default, rename = "crate", skip_serializing_if = "Option::is_none")]
    pub crate_source: Option<LockedCrateExtension>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<PathBuf>,
    /// SHA-256 of the exact adapter selection and specialization report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedCrateExtension {
    pub package: String,
    pub version: Version,
    pub requirement: String,
    #[serde(default)]
    pub features: Vec<String>,
    pub default_features: bool,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub specializations: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
}

impl PackageLock {
    fn from_resolved(manifest: &PackageManifest, extensions: &[ResolvedExtension]) -> Self {
        Self {
            schema_version: SUPPORTED_LOCK_SCHEMA,
            package: LockedPackage {
                name: manifest.package.name.clone(),
                version: manifest.package.version.clone(),
            },
            extensions: extensions
                .iter()
                .map(|extension| {
                    (
                        extension.manifest_name.clone(),
                        LockedExtension {
                            module: extension.bundle.module().as_str().to_string(),
                            version: extension.bundle.manifest().version.clone(),
                            source: extension.source.clone(),
                            sha256: extension.bundle.digest().to_string(),
                            crate_source: extension.crate_source.clone(),
                            artifact: extension.artifact.clone(),
                            report_sha256: extension.report_sha256.clone(),
                        },
                    )
                })
                .collect(),
        }
    }

    pub fn parse(source: &str) -> Result<Self, PackageError> {
        let lock: Self = toml::from_str(source).map_err(|error| PackageError::InvalidLock {
            message: error.to_string(),
        })?;
        if lock.schema_version != SUPPORTED_LOCK_SCHEMA {
            return Err(PackageError::UnsupportedLockSchema(lock.schema_version));
        }
        Ok(lock)
    }

    pub fn empty(name: String, version: Version) -> Self {
        Self {
            schema_version: SUPPORTED_LOCK_SCHEMA,
            package: LockedPackage { name, version },
            extensions: BTreeMap::new(),
        }
    }

    pub fn validate_manifest(&self, manifest: &PackageManifest) -> Result<(), PackageError> {
        if self.package.name != manifest.package.name
            || self.package.version != manifest.package.version
            || self.extensions.len() != manifest.extensions.len()
        {
            return Err(PackageError::InvalidLock {
                message: "Husk.toml and Husk.lock describe different package inputs".to_string(),
            });
        }
        for (name, declaration) in &manifest.extensions {
            let locked = self
                .extensions
                .get(name)
                .ok_or_else(|| PackageError::InvalidLock {
                    message: format!("extension `{name}` is missing from Husk.lock"),
                })?;
            validate_local_path(&locked.source, "locked extension source")?;
            if let Some(artifact) = &locked.artifact {
                validate_local_path(artifact, "locked extension artifact")?;
            }
            match declaration {
                ExtensionSource::Path(extension) => {
                    if locked.source != extension.path || locked.crate_source.is_some() {
                        return Err(PackageError::InvalidLock {
                            message: format!(
                                "path extension `{name}` differs between Husk.toml and Husk.lock"
                            ),
                        });
                    }
                }
                ExtensionSource::Crate(extension) => {
                    let crate_source =
                        locked
                            .crate_source
                            .as_ref()
                            .ok_or_else(|| PackageError::InvalidLock {
                                message: format!(
                                    "crate extension `{name}` has no crate provenance in Husk.lock"
                                ),
                            })?;
                    let expected_source = installed_extension_path(&locked.sha256);
                    if crate_source.package != extension.package
                        || crate_source.requirement != extension.version
                        || crate_source.features != extension.features
                        || crate_source.default_features != extension.default_features
                        || crate_source.include != extension.include
                        || crate_source.specializations != extension.specializations
                        || locked.source != expected_source
                        || locked.artifact.is_none()
                    {
                        return Err(PackageError::InvalidLock {
                            message: format!(
                                "crate extension `{name}` differs between Husk.toml and Husk.lock"
                            ),
                        });
                    }
                    let requirement =
                        semver::VersionReq::parse(&extension.version).map_err(|error| {
                            PackageError::InvalidManifest {
                                message: format!(
                                    "invalid crate version requirement `{}`: {error}",
                                    extension.version
                                ),
                            }
                        })?;
                    if !requirement.matches(&crate_source.version) {
                        return Err(PackageError::InvalidLock {
                            message: format!(
                                "locked crate version {} does not satisfy `{}` for extension `{name}`",
                                crate_source.version, extension.version
                            ),
                        });
                    }
                }
            }
        }
        Ok(())
    }

    pub fn to_toml(&self) -> Result<String, PackageError> {
        let mut source =
            toml::to_string_pretty(self).map_err(|error| PackageError::InvalidLock {
                message: error.to_string(),
            })?;
        if !source.ends_with('\n') {
            source.push('\n');
        }
        Ok(source)
    }
}

pub fn installed_extension_path(digest: &str) -> PathBuf {
    PathBuf::from(INSTALL_DIRECTORY).join(format!("{digest}.huskext"))
}

pub fn vendored_extension_path(digest: &str) -> PathBuf {
    PathBuf::from(VENDOR_DIRECTORY).join(format!("{digest}.huskext"))
}

fn read_package_lock(path: &Path, maximum: u64) -> Result<PackageLock, PackageError> {
    reject_symlink(path)?;
    let bytes = read_bounded(path, maximum)?;
    let source = std::str::from_utf8(&bytes).map_err(|error| PackageError::InvalidLock {
        message: format!("lock file is not UTF-8: {error}"),
    })?;
    PackageLock::parse(source)
}

struct ModuleResolver {
    source_root: PathBuf,
    limits: PackageLimits,
    modules: BTreeMap<Vec<String>, SourceModule>,
    canonical_owners: HashMap<PathBuf, Vec<String>>,
    stack: Vec<(Vec<String>, PathBuf)>,
}

impl ModuleResolver {
    fn new(source_root: PathBuf, limits: PackageLimits) -> Self {
        Self {
            source_root,
            limits,
            modules: BTreeMap::new(),
            canonical_owners: HashMap::new(),
            stack: Vec::new(),
        }
    }

    fn resolve(
        &mut self,
        module_path: Vec<String>,
        source_path: PathBuf,
    ) -> Result<(), PackageError> {
        if self.modules.len() >= self.limits.max_modules {
            return Err(PackageError::TooManyModules {
                maximum: self.limits.max_modules,
            });
        }
        let canonical = source_path
            .canonicalize()
            .map_err(|source| PackageError::Io {
                path: source_path.clone(),
                source,
            })?;
        if !canonical.starts_with(&self.source_root) {
            return Err(PackageError::PathEscapesRoot(canonical));
        }
        if let Some(index) = self
            .stack
            .iter()
            .position(|(_, active)| active == &canonical)
        {
            let mut cycle = self.stack[index..]
                .iter()
                .map(|(path, _)| display_module(path))
                .collect::<Vec<_>>();
            cycle.push(display_module(&module_path));
            return Err(PackageError::ModuleCycle(cycle));
        }
        if let Some(first) = self.canonical_owners.get(&canonical) {
            return Err(PackageError::DuplicateModuleFile {
                path: canonical,
                first: display_module(first),
                second: display_module(&module_path),
            });
        }
        reject_symlink(&source_path)?;
        let bytes = read_bounded(&canonical, self.limits.max_source_bytes)?;
        let source = String::from_utf8(bytes).map_err(|error| PackageError::InvalidSource {
            path: canonical.clone(),
            message: error.to_string(),
        })?;
        let parsed = husk_parser::parse_str(&source);
        if !parsed.errors.is_empty() {
            return Err(PackageError::Parse {
                path: canonical,
                errors: parsed
                    .errors
                    .into_iter()
                    .map(|error| {
                        format!(
                            "{} at {}..{}",
                            error.message, error.span.range.start, error.span.range.end
                        )
                    })
                    .collect(),
            });
        }
        let mut syntax = parsed.file.expect("parser returns a file");
        let display_path = canonical
            .strip_prefix(&self.source_root)
            .unwrap_or(&canonical)
            .to_path_buf();
        let ast_path = Arc::<str>::from(display_path.to_string_lossy().as_ref());
        for item in &mut syntax.items {
            item.set_file_path(Arc::clone(&ast_path));
        }
        let child_names = syntax
            .items
            .iter()
            .filter_map(|item| match &item.kind {
                ItemKind::Mod { name } => Some(name.name.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();

        self.canonical_owners
            .insert(canonical.clone(), module_path.clone());
        self.stack.push((module_path.clone(), canonical.clone()));
        self.modules.insert(
            module_path.clone(),
            SourceModule {
                module_path: module_path.clone(),
                canonical_path: canonical.clone(),
                display_path,
                source: Arc::from(source),
                syntax,
            },
        );

        let child_base = module_child_base(&canonical, module_path.is_empty());
        for child_name in child_names {
            let flat = child_base.join(format!("{child_name}.hk"));
            let nested = child_base.join(&child_name).join("mod.hk");
            let flat_exists = flat.is_file();
            let nested_exists = nested.is_file();
            let selected = match (flat_exists, nested_exists) {
                (true, false) => flat,
                (false, true) => nested,
                (true, true) => {
                    return Err(PackageError::AmbiguousModule {
                        module: child_name,
                        flat,
                        nested,
                    });
                }
                (false, false) => {
                    return Err(PackageError::MissingModule {
                        module: child_name,
                        flat,
                        nested,
                    });
                }
            };
            let mut child_path = module_path.clone();
            child_path.push(child_name);
            self.resolve(child_path, selected)?;
        }
        self.stack.pop();
        Ok(())
    }

    fn finish(self) -> Vec<SourceModule> {
        self.modules.into_values().collect()
    }
}

fn module_child_base(path: &Path, root: bool) -> PathBuf {
    let parent = path.parent().expect("source file has a parent");
    if root || path.file_name().and_then(|name| name.to_str()) == Some("mod.hk") {
        parent.to_path_buf()
    } else {
        parent.join(path.file_stem().expect("source file has a stem"))
    }
}

fn display_module(path: &[String]) -> String {
    if path.is_empty() {
        "crate".to_string()
    } else {
        format!("crate::{}", path.join("::"))
    }
}

fn validate_package_name(name: &str) -> Result<(), PackageError> {
    let valid = !name.is_empty()
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || character == '_'
                || character == '-'
                || character == '.'
        });
    if valid {
        Ok(())
    } else {
        Err(PackageError::InvalidManifest {
            message: format!("invalid package or extension name `{name}`"),
        })
    }
}

fn validate_local_path(path: &Path, kind: &str) -> Result<(), PackageError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || !path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(PackageError::InvalidManifest {
            message: format!(
                "{kind} path must be a normalized relative path: `{}`",
                path.display()
            ),
        });
    }
    Ok(())
}

fn reject_symlink(path: &Path) -> Result<(), PackageError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| PackageError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        Err(PackageError::Symlink(path.to_path_buf()))
    } else {
        Ok(())
    }
}

fn read_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, PackageError> {
    let metadata = fs::metadata(path).map_err(|source| PackageError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.len() > maximum {
        return Err(PackageError::TooLarge {
            path: path.to_path_buf(),
            actual: metadata.len(),
            maximum,
        });
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|source| PackageError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| PackageError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum {
        return Err(PackageError::TooLarge {
            path: path.to_path_buf(),
            actual: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            maximum,
        });
    }
    Ok(bytes)
}

#[derive(Debug)]
pub enum PackageError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    ManifestNotFound(PathBuf),
    InvalidManifest {
        message: String,
    },
    InvalidLock {
        message: String,
    },
    UnsupportedManifestSchema(u32),
    UnsupportedLockSchema(u32),
    InvalidSource {
        path: PathBuf,
        message: String,
    },
    Parse {
        path: PathBuf,
        errors: Vec<String>,
    },
    NotFile(PathBuf),
    Symlink(PathBuf),
    PathEscapesRoot(PathBuf),
    TooLarge {
        path: PathBuf,
        actual: u64,
        maximum: u64,
    },
    TooManyModules {
        maximum: usize,
    },
    MissingModule {
        module: String,
        flat: PathBuf,
        nested: PathBuf,
    },
    AmbiguousModule {
        module: String,
        flat: PathBuf,
        nested: PathBuf,
    },
    DuplicateModuleFile {
        path: PathBuf,
        first: String,
        second: String,
    },
    ModuleCycle(Vec<String>),
    Extension {
        name: String,
        source: husk_extension::BundleError,
    },
    LockChanged {
        path: PathBuf,
        expected: Box<PackageLock>,
        actual: Box<PackageLock>,
    },
}

impl fmt::Display for PackageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "`{}`: {source}", path.display()),
            Self::ManifestNotFound(start) => write!(
                formatter,
                "no `{MANIFEST_FILE}` found at or above `{}`",
                start.display()
            ),
            Self::InvalidManifest { message } => {
                write!(formatter, "invalid Husk manifest: {message}")
            }
            Self::InvalidLock { message } => write!(formatter, "invalid Husk lock file: {message}"),
            Self::UnsupportedManifestSchema(version) => {
                write!(formatter, "unsupported Husk manifest schema {version}")
            }
            Self::UnsupportedLockSchema(version) => {
                write!(formatter, "unsupported Husk lock schema {version}")
            }
            Self::InvalidSource { path, message } => {
                write!(
                    formatter,
                    "invalid Husk source `{}`: {message}",
                    path.display()
                )
            }
            Self::Parse { path, errors } => write!(
                formatter,
                "failed to parse Husk module `{}`: {}",
                path.display(),
                errors.join("; ")
            ),
            Self::NotFile(path) => write!(formatter, "expected a file at `{}`", path.display()),
            Self::Symlink(path) => write!(
                formatter,
                "package control path must not be a symlink: `{}`",
                path.display()
            ),
            Self::PathEscapesRoot(path) => write!(
                formatter,
                "package path escapes its root: `{}`",
                path.display()
            ),
            Self::TooLarge {
                path,
                actual,
                maximum,
            } => write!(
                formatter,
                "`{}` is {actual} bytes; maximum is {maximum}",
                path.display()
            ),
            Self::TooManyModules { maximum } => {
                write!(
                    formatter,
                    "package exceeds its maximum of {maximum} source modules"
                )
            }
            Self::MissingModule {
                module,
                flat,
                nested,
            } => write!(
                formatter,
                "module `{module}` was not found; expected exactly one of `{}` or `{}`",
                flat.display(),
                nested.display()
            ),
            Self::AmbiguousModule {
                module,
                flat,
                nested,
            } => write!(
                formatter,
                "module `{module}` is ambiguous because both `{}` and `{}` exist",
                flat.display(),
                nested.display()
            ),
            Self::DuplicateModuleFile {
                path,
                first,
                second,
            } => write!(
                formatter,
                "module file `{}` resolves as both `{first}` and `{second}`",
                path.display()
            ),
            Self::ModuleCycle(cycle) => write!(
                formatter,
                "source module cycle detected: {}",
                cycle.join(" -> ")
            ),
            Self::Extension { name, source } => {
                write!(formatter, "invalid extension `{name}`: {source}")
            }
            Self::LockChanged { path, .. } => write!(
                formatter,
                "locked package inputs differ from `{}`; regenerate the lock file",
                path.display()
            ),
        }
    }
}

impl std::error::Error for PackageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Extension { source, .. } => Some(source),
            _ => None,
        }
    }
}
