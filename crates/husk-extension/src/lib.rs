//! Validation for portable `.huskext` directory bundles.
//!
//! This crate intentionally has no Wasmtime dependency. It establishes the
//! bounded filesystem and manifest trust boundary before component
//! compilation begins.

use std::{
    collections::BTreeSet,
    fmt,
    fs::{self, File},
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

use husk_types::ModuleName;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MANIFEST_FILE: &str = "extension.toml";
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// Limits applied before allocating or compiling untrusted bundle contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundleLimits {
    pub max_manifest_bytes: u64,
    pub max_component_bytes: u64,
}

impl Default for BundleLimits {
    fn default() -> Self {
        Self {
            max_manifest_bytes: 256 * 1024,
            max_component_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Capability name requested by an extension package.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Capability(String);

impl Capability {
    pub fn new(value: impl Into<String>) -> Result<Self, BundleError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.split('.').all(|segment| {
                !segment.is_empty()
                    && segment.chars().all(|character| {
                        character.is_ascii_lowercase()
                            || character.is_ascii_digit()
                            || character == '_'
                            || character == '-'
                    })
            });
        if !valid {
            return Err(BundleError::InvalidCapability(value));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Enforce `actual imports ⊆ requested capabilities ⊆ granted capabilities`.
pub fn validate_capabilities(
    actual: &BTreeSet<Capability>,
    requested: &BTreeSet<Capability>,
    granted: &BTreeSet<Capability>,
) -> Result<(), CapabilityError> {
    let undeclared = actual.difference(requested).cloned().collect::<Vec<_>>();
    if !undeclared.is_empty() {
        return Err(CapabilityError::UndeclaredImports(undeclared));
    }
    let denied = requested.difference(granted).cloned().collect::<Vec<_>>();
    if !denied.is_empty() {
        return Err(CapabilityError::NotGranted(denied));
    }
    Ok(())
}

/// Declared extension capabilities. Empty means a pure component.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CapabilityManifest {
    pub requested: Vec<Capability>,
}

/// Versioned, strictly parsed `.huskext` manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionManifest {
    pub schema_version: u32,
    pub name: String,
    pub version: Version,
    pub module: String,
    pub artifact: PathBuf,
    pub world: String,
    pub minimum_husk: Version,
    #[serde(default)]
    pub capabilities: CapabilityManifest,
}

impl ExtensionManifest {
    pub fn parse(text: &str) -> Result<Self, BundleError> {
        let manifest: Self =
            toml::from_str(text).map_err(|error| BundleError::InvalidManifest {
                message: error.to_string(),
            })?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<ModuleName, BundleError> {
        if self.schema_version != SUPPORTED_SCHEMA_VERSION {
            return Err(BundleError::UnsupportedSchema(self.schema_version));
        }
        validate_package_name(&self.name)?;
        let module =
            ModuleName::new(self.module.clone()).map_err(|error| BundleError::InvalidManifest {
                message: error.to_string(),
            })?;
        if self.artifact.as_os_str().is_empty()
            || self.artifact.is_absolute()
            || !self
                .artifact
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
        {
            return Err(BundleError::InvalidArtifactPath(self.artifact.clone()));
        }
        if self.world.trim().is_empty() {
            return Err(BundleError::InvalidManifest {
                message: "extension world must not be empty".to_string(),
            });
        }
        let mut capabilities = self.capabilities.requested.clone();
        capabilities.sort();
        capabilities.dedup();
        if capabilities.len() != self.capabilities.requested.len() {
            return Err(BundleError::InvalidManifest {
                message: "requested capabilities must not contain duplicates".to_string(),
            });
        }
        for capability in &self.capabilities.requested {
            Capability::new(capability.as_str())?;
        }
        Ok(module)
    }
}

/// Fully bounded and canonicalized bundle ready for component inspection.
#[derive(Debug, Clone)]
pub struct ExtensionBundle {
    root: PathBuf,
    manifest: ExtensionManifest,
    module: ModuleName,
    component_path: PathBuf,
    component: Vec<u8>,
    digest: ComponentDigest,
}

impl ExtensionBundle {
    pub fn open(path: impl AsRef<Path>, limits: BundleLimits) -> Result<Self, BundleError> {
        let unresolved_root = path.as_ref();
        reject_symlink(unresolved_root)?;
        let root = unresolved_root
            .canonicalize()
            .map_err(|source| BundleError::Io {
                path: unresolved_root.to_path_buf(),
                source,
            })?;
        if !root.is_dir() {
            return Err(BundleError::NotDirectory(root));
        }

        let manifest_path = root.join(MANIFEST_FILE);
        reject_symlink(&manifest_path)?;
        let manifest_bytes = read_bounded(&manifest_path, limits.max_manifest_bytes)?;
        let manifest_text =
            std::str::from_utf8(&manifest_bytes).map_err(|error| BundleError::InvalidManifest {
                message: format!("manifest is not UTF-8: {error}"),
            })?;
        let manifest = ExtensionManifest::parse(manifest_text)?;
        let module = manifest.validate()?;

        let unresolved_component = root.join(&manifest.artifact);
        reject_symlink(&unresolved_component)?;
        let component_path =
            unresolved_component
                .canonicalize()
                .map_err(|source| BundleError::Io {
                    path: unresolved_component.clone(),
                    source,
                })?;
        if !component_path.starts_with(&root) {
            return Err(BundleError::ArtifactEscapesBundle(component_path));
        }
        if !component_path.is_file() {
            return Err(BundleError::ArtifactNotFile(component_path));
        }
        let component = read_bounded(&component_path, limits.max_component_bytes)?;
        let digest = ComponentDigest(Sha256::digest(&component).into());

        Ok(Self {
            root,
            manifest,
            module,
            component_path,
            component,
            digest,
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn manifest(&self) -> &ExtensionManifest {
        &self.manifest
    }

    #[must_use]
    pub fn module(&self) -> &ModuleName {
        &self.module
    }

    #[must_use]
    pub fn component_path(&self) -> &Path {
        &self.component_path
    }

    #[must_use]
    pub fn component(&self) -> &[u8] {
        &self.component
    }

    #[must_use]
    pub fn digest(&self) -> ComponentDigest {
        self.digest
    }
}

/// Assemble a directory bundle from an existing manifest and component.
///
/// The destination must not exist. If validation of the assembled bundle
/// fails, the newly-created destination is removed.
pub fn pack_directory(
    manifest_path: impl AsRef<Path>,
    component_path: impl AsRef<Path>,
    output: impl AsRef<Path>,
    limits: BundleLimits,
) -> Result<ExtensionBundle, BundleError> {
    let manifest_path = manifest_path.as_ref();
    let component_path = component_path.as_ref();
    let output = output.as_ref();
    reject_symlink(manifest_path)?;
    reject_symlink(component_path)?;
    let manifest_bytes = read_bounded(manifest_path, limits.max_manifest_bytes)?;
    let manifest_text =
        std::str::from_utf8(&manifest_bytes).map_err(|error| BundleError::InvalidManifest {
            message: format!("manifest is not UTF-8: {error}"),
        })?;
    let manifest = ExtensionManifest::parse(manifest_text)?;
    let component = read_bounded(component_path, limits.max_component_bytes)?;

    fs::create_dir(output).map_err(|source| BundleError::Io {
        path: output.to_path_buf(),
        source,
    })?;
    let assembled = (|| {
        fs::write(output.join(MANIFEST_FILE), &manifest_bytes).map_err(|source| {
            BundleError::Io {
                path: output.join(MANIFEST_FILE),
                source,
            }
        })?;
        let destination = output.join(&manifest.artifact);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|source| BundleError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        fs::write(&destination, component).map_err(|source| BundleError::Io {
            path: destination,
            source,
        })?;
        ExtensionBundle::open(output, limits)
    })();
    if assembled.is_err() {
        let _ = fs::remove_dir_all(output);
    }
    assembled
}

fn validate_package_name(name: &str) -> Result<(), BundleError> {
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
        Err(BundleError::InvalidManifest {
            message: format!("invalid extension package name `{name}`"),
        })
    }
}

fn reject_symlink(path: &Path) -> Result<(), BundleError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| BundleError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        Err(BundleError::Symlink(path.to_path_buf()))
    } else {
        Ok(())
    }
}

fn read_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, BundleError> {
    let metadata = fs::metadata(path).map_err(|source| BundleError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.len() > maximum {
        return Err(BundleError::TooLarge {
            path: path.to_path_buf(),
            actual: metadata.len(),
            maximum,
        });
    }

    let file = File::open(path).map_err(|source| BundleError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut bytes = Vec::new();
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| BundleError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum {
        return Err(BundleError::TooLarge {
            path: path.to_path_buf(),
            actual: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            maximum,
        });
    }
    Ok(bytes)
}

/// SHA-256 identity of the exact component bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ComponentDigest([u8; 32]);

impl ComponentDigest {
    #[must_use]
    pub fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Display for ComponentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum BundleError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    NotDirectory(PathBuf),
    Symlink(PathBuf),
    TooLarge {
        path: PathBuf,
        actual: u64,
        maximum: u64,
    },
    InvalidManifest {
        message: String,
    },
    UnsupportedSchema(u32),
    InvalidCapability(String),
    InvalidArtifactPath(PathBuf),
    ArtifactEscapesBundle(PathBuf),
    ArtifactNotFile(PathBuf),
}

impl fmt::Display for BundleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "`{}`: {source}", path.display()),
            Self::NotDirectory(path) => {
                write!(
                    formatter,
                    "extension bundle is not a directory: `{}`",
                    path.display()
                )
            }
            Self::Symlink(path) => write!(
                formatter,
                "extension bundle control file must not be a symlink: `{}`",
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
            Self::InvalidManifest { message } => {
                write!(formatter, "invalid extension manifest: {message}")
            }
            Self::UnsupportedSchema(version) => {
                write!(formatter, "unsupported extension schema version {version}")
            }
            Self::InvalidCapability(capability) => {
                write!(formatter, "invalid extension capability `{capability}`")
            }
            Self::InvalidArtifactPath(path) => {
                write!(
                    formatter,
                    "invalid extension artifact path `{}`",
                    path.display()
                )
            }
            Self::ArtifactEscapesBundle(path) => write!(
                formatter,
                "extension artifact escapes its bundle: `{}`",
                path.display()
            ),
            Self::ArtifactNotFile(path) => {
                write!(
                    formatter,
                    "extension artifact is not a file: `{}`",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for BundleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::NotDirectory(_)
            | Self::Symlink(_)
            | Self::TooLarge { .. }
            | Self::InvalidManifest { .. }
            | Self::UnsupportedSchema(_)
            | Self::InvalidCapability(_)
            | Self::InvalidArtifactPath(_)
            | Self::ArtifactEscapesBundle(_)
            | Self::ArtifactNotFile(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityError {
    UndeclaredImports(Vec<Capability>),
    NotGranted(Vec<Capability>),
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (prefix, capabilities) = match self {
            Self::UndeclaredImports(capabilities) => (
                "component imports capabilities missing from its manifest",
                capabilities,
            ),
            Self::NotGranted(capabilities) => (
                "extension capabilities were not granted by the host",
                capabilities,
            ),
        };
        write!(
            formatter,
            "{prefix}: {}",
            capabilities
                .iter()
                .map(Capability::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl std::error::Error for CapabilityError {}
