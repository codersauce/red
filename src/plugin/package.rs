//! External plugin package manifests, installation records, and lifecycle operations.
//!
//! Packages live in isolated directories below Red's configuration directory. An
//! installation record may point at a local development checkout, a GitHub repository,
//! or an immutable curated-catalog bundle. Updates are staged and validated before an
//! atomic directory swap so a failed install never replaces a working plugin.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    io::{Cursor, Read as _},
    path::{Component, Path, PathBuf},
    process::Stdio,
};

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{io::AsyncWriteExt as _, process::Command};

use crate::{config::LanguageConfig, language::GrammarTrustStore};

use super::{
    catalog::{validate_catalog_url, CatalogArtifact, CatalogPackage, PluginCatalog},
    Runtime, RED_HOST_API_VERSION,
};

/// File name of the Red-specific package manifest.
pub const PLUGIN_MANIFEST_FILE: &str = "red-plugin.toml";
/// File name of Red's installation record inside an installed package directory.
pub const INSTALL_RECORD_FILE: &str = ".red-install.json";
/// Current external plugin manifest schema.
pub const PLUGIN_MANIFEST_SCHEMA: u32 = 1;
const MAX_PACKAGE_ARCHIVE_BYTES: usize = 128 * 1024 * 1024;
const MAX_PACKAGE_UNPACKED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PACKAGE_ARCHIVE_FILES: usize = 1024;
const MAX_CATALOG_GRAMMAR_BYTES: u64 = 64 * 1024 * 1024;

/// A parsed and validated stable plugin identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PluginId(String);

impl PluginId {
    /// Parses an identifier accepted in manifests and lifecycle commands.
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        anyhow::ensure!(
            !value.is_empty()
                && value.len() <= 64
                && value.bytes().all(|byte| byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'_')),
            "plugin id must contain only lowercase ASCII letters, digits, `-`, or `_`"
        );
        Ok(Self(value))
    }

    /// Returns the identifier as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// External plugin package manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginPackageManifest {
    #[serde(default = "default_manifest_schema")]
    pub schema_version: u32,
    pub plugin: PluginManifestSection,
    #[serde(default)]
    pub activation: PluginActivation,
    #[serde(default)]
    pub keymaps: BTreeMap<String, BTreeMap<String, String>>,
    pub companion: Option<PluginCompanionManifest>,
    /// Reusable language definitions contributed without requiring a Husk entrypoint.
    #[serde(default)]
    pub languages: BTreeMap<String, LanguageConfig>,
    #[serde(default)]
    pub migration: PluginMigration,
}

/// Declarative imports from fields written by a formerly bundled plugin.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginMigration {
    /// Old top-level session field mapped to the plugin's private storage key.
    #[serde(default)]
    pub legacy_session_fields: BTreeMap<String, String>,
}

/// Identity and host compatibility for one plugin package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifestSection {
    pub id: PluginId,
    pub name: String,
    pub version: Version,
    pub red_api: VersionReq,
    pub husk_manifest: Option<PathBuf>,
    pub entry: Option<PathBuf>,
    pub description: Option<String>,
    pub repository: Option<String>,
    pub license: Option<String>,
}

/// Lazy activation declarations.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginActivation {
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub commands: Vec<String>,
}

/// Native companion declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginCompanionManifest {
    /// Package-relative development or bundled executable.
    pub command: Option<PathBuf>,
    /// Platform-specific development or bundled executable overrides.
    #[serde(default)]
    pub commands: BTreeMap<String, PathBuf>,
    /// Release artifacts keyed by Rust target triple.
    #[serde(default)]
    pub artifacts: BTreeMap<String, PluginCompanionArtifact>,
}

/// One downloadable native companion executable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginCompanionArtifact {
    pub url: String,
    pub sha256: String,
}

/// Origin retained so an installed package can be updated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PluginInstallSource {
    Path {
        path: PathBuf,
    },
    GitHub {
        repository: String,
        requested_version: Option<String>,
    },
    Catalog {
        catalog_url: String,
        package: PluginId,
        version: Version,
        resolved_commit: String,
        artifact_url: String,
        artifact_sha256: String,
    },
}

/// Crash-safe record associated with an installed plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginInstallRecord {
    pub schema_version: u32,
    pub id: PluginId,
    pub version: Version,
    pub enabled: bool,
    pub source: PluginInstallSource,
    pub package_root: PathBuf,
    pub installed_at_ms: u64,
}

/// Installed plugin information shown by CLI and editor management surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstalledPlugin {
    pub id: PluginId,
    pub name: String,
    pub version: Version,
    pub enabled: bool,
    pub compatible: bool,
    pub has_companion: bool,
    pub has_husk: bool,
    /// Whether the package contributes one or more language definitions.
    pub has_languages: bool,
    pub has_native_grammars: bool,
    pub languages: Vec<String>,
    pub source: PluginInstallSource,
    pub package_root: PathBuf,
}

/// External package lifecycle authority for one Red configuration root.
#[derive(Debug, Clone)]
pub struct PluginPackageManager {
    config_dir: PathBuf,
}

impl PluginPackageManifest {
    /// Loads, parses, and validates a package manifest.
    pub fn load(package_root: &Path) -> Result<Self> {
        let path = package_root.join(PLUGIN_MANIFEST_FILE);
        let source = fs::read_to_string(&path)
            .with_context(|| format!("failed to read plugin manifest {}", path.display()))?;
        let manifest: Self = toml::from_str(&source)
            .with_context(|| format!("failed to parse plugin manifest {}", path.display()))?;
        manifest.validate(package_root)?;
        Ok(manifest)
    }

    /// Ensures paths remain within the package and the declared host API is supported.
    pub fn validate(&self, package_root: &Path) -> Result<()> {
        anyhow::ensure!(
            self.schema_version == PLUGIN_MANIFEST_SCHEMA,
            "unsupported plugin manifest schema {}",
            self.schema_version
        );
        anyhow::ensure!(!self.plugin.name.trim().is_empty(), "plugin name is empty");
        anyhow::ensure!(
            self.plugin.husk_manifest.is_some()
                || self.plugin.entry.is_some()
                || self.companion.is_some()
                || !self.languages.is_empty(),
            "plugin package must declare a Husk entrypoint, native companion, or language"
        );
        let host_api = Version::parse(RED_HOST_API_VERSION)?;
        anyhow::ensure!(
            self.plugin.red_api.matches(&host_api),
            "plugin requires Red host API `{}`, but this release provides `{host_api}`",
            self.plugin.red_api
        );

        for relative in self
            .plugin
            .husk_manifest
            .iter()
            .chain(self.plugin.entry.iter())
            .chain(
                self.companion
                    .iter()
                    .filter_map(|companion| companion.command.as_ref()),
            )
            .chain(
                self.companion
                    .iter()
                    .flat_map(|companion| companion.commands.values()),
            )
        {
            validate_relative_path(relative)?;
            let target = package_root.join(relative);
            anyhow::ensure!(
                target.is_file(),
                "plugin package file does not exist: {}",
                target.display()
            );
        }
        for (target, artifact) in self
            .companion
            .iter()
            .flat_map(|companion| &companion.artifacts)
        {
            anyhow::ensure!(!target.trim().is_empty(), "companion target is empty");
            anyhow::ensure!(
                artifact.url.starts_with("https://github.com/")
                    || artifact.url.starts_with("https://api.github.com/"),
                "companion artifacts must use a GitHub HTTPS URL"
            );
            anyhow::ensure!(
                artifact.sha256.len() == 64
                    && artifact.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "companion artifact for `{target}` has an invalid SHA-256 digest"
            );
        }
        for (field, storage_key) in &self.migration.legacy_session_fields {
            anyhow::ensure!(
                !field.trim().is_empty() && !storage_key.trim().is_empty(),
                "legacy session migration keys cannot be empty"
            );
        }
        for (id, language) in &self.languages {
            anyhow::ensure!(
                !id.trim().is_empty()
                    && id.bytes().all(|byte| byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'-' | b'_')),
                "invalid package language identifier `{id}`"
            );
            let Some(grammar) = &language.grammar else {
                continue;
            };
            for relative in grammar
                .path
                .iter()
                .chain(grammar.highlights.iter())
                .chain(grammar.injections.iter())
            {
                validate_package_file(package_root, relative)?;
            }
            for (target, artifact) in &grammar.targets {
                anyhow::ensure!(
                    !target.trim().is_empty(),
                    "language grammar target is empty"
                );
                anyhow::ensure!(
                    artifact.path.is_some() != artifact.url.is_some(),
                    "language `{id}` grammar target `{target}` must have exactly one path or URL"
                );
                if let Some(path) = &artifact.path {
                    validate_package_file(package_root, path)?;
                }
                if let Some(url) = &artifact.url {
                    anyhow::ensure!(
                        url.starts_with("https://github.com/")
                            || url.starts_with("https://api.github.com/"),
                        "language grammar artifacts must use a GitHub HTTPS URL"
                    );
                    let digest = artifact.sha256.as_deref().unwrap_or_default();
                    anyhow::ensure!(
                        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
                        "language `{id}` grammar target `{target}` has an invalid SHA-256 digest"
                    );
                }
            }
        }
        validate_keymaps(&self.keymaps)?;
        Ok(())
    }

    /// Returns the absolute Husk source or package manifest used by the registry.
    #[must_use]
    pub fn husk_entry(&self, package_root: &Path) -> Option<PathBuf> {
        self.plugin
            .husk_manifest
            .as_ref()
            .or(self.plugin.entry.as_ref())
            .map(|relative| package_root.join(relative))
    }

    /// Resolves the native companion executable for the running host.
    #[must_use]
    pub fn companion_command(&self, package_root: &Path) -> Option<PathBuf> {
        self.companion.as_ref().and_then(|companion| {
            companion
                .commands
                .get(host_target())
                .or(companion.command.as_ref())
                .map(|relative| package_root.join(relative))
                .or_else(|| {
                    let downloaded = package_root
                        .join(".red")
                        .join("bin")
                        .join(companion_binary_name());
                    downloaded.is_file().then_some(downloaded)
                })
        })
    }

    /// Resolves one language grammar for the running host without loading native code.
    #[must_use]
    pub fn grammar_path(
        &self,
        package_root: &Path,
        id: &str,
        language: &LanguageConfig,
    ) -> Option<PathBuf> {
        let grammar = language.grammar.as_ref()?;
        if let Some(path) = &grammar.path {
            return Some(package_root.join(path));
        }
        let artifact = grammar.targets.get(host_target())?;
        artifact
            .path
            .as_ref()
            .map(|path| package_root.join(path))
            .or_else(|| {
                artifact.url.as_ref().map(|_| {
                    package_root
                        .join(".red")
                        .join("grammars")
                        .join(host_target())
                        .join(grammar_filename(id))
                })
            })
    }
}

impl PluginPackageManager {
    /// Creates a manager rooted at Red's platform configuration directory.
    #[must_use]
    pub fn new(config_dir: impl Into<PathBuf>) -> Self {
        Self {
            config_dir: config_dir.into(),
        }
    }

    /// Directory containing isolated installed package records.
    #[must_use]
    pub fn packages_dir(&self) -> PathBuf {
        self.config_dir.join("plugins")
    }

    /// Durable, namespaced storage owned by one plugin.
    #[must_use]
    pub fn data_dir(&self, id: &PluginId) -> PathBuf {
        self.config_dir.join("plugin-data").join(id.as_str())
    }

    /// Installs a local development checkout without copying its source.
    pub async fn install_path(&self, source: &Path) -> Result<InstalledPlugin> {
        self.install_path_with_trust(source, false).await
    }

    /// Installs a local package, optionally approving its current native grammar bytes.
    pub async fn install_path_with_trust(
        &self,
        source: &Path,
        trust_native_grammars: bool,
    ) -> Result<InstalledPlugin> {
        let source = source
            .canonicalize()
            .with_context(|| format!("failed to resolve plugin path {}", source.display()))?;
        let manifest = PluginPackageManifest::load(&source)?;
        validate_husk_package(&manifest, &source).await?;
        self.install_language_artifacts(&manifest, &source, trust_native_grammars)
            .await?;
        let install_source = PluginInstallSource::Path {
            path: source.clone(),
        };
        self.install_record(&manifest, source, install_source).await
    }

    /// Installs a package directly from a GitHub repository.
    pub async fn install_github(
        &self,
        repository: &str,
        requested_version: Option<&str>,
    ) -> Result<InstalledPlugin> {
        self.install_github_with_trust(repository, requested_version, false)
            .await
    }

    /// Installs a GitHub package and optionally approves verified native grammars.
    pub async fn install_github_with_trust(
        &self,
        repository: &str,
        requested_version: Option<&str>,
        trust_native_grammars: bool,
    ) -> Result<InstalledPlugin> {
        validate_github_repository(repository)?;
        let staging = self.staging_dir("github");
        if let Some(parent) = staging.parent() {
            fs::create_dir_all(parent)?;
        }
        remove_if_exists(&staging)?;

        let mut command = Command::new("git");
        command
            .args(["clone", "--depth", "1"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(version) = requested_version {
            command.args(["--branch", version]);
        }
        command
            .arg(format!("https://github.com/{repository}.git"))
            .arg(&staging);
        let output = command
            .output()
            .await
            .context("failed to launch git for plugin install")?;
        anyhow::ensure!(
            output.status.success(),
            "git clone failed: {}",
            bounded_stderr(&output.stderr)
        );

        let manifest = PluginPackageManifest::load(&staging)?;
        validate_husk_package(&manifest, &staging).await?;
        let id = manifest.plugin.id.clone();
        let destination = self.packages_dir().join(id.as_str());
        let record = PluginInstallRecord {
            schema_version: 1,
            id: id.clone(),
            version: manifest.plugin.version.clone(),
            enabled: true,
            source: PluginInstallSource::GitHub {
                repository: repository.to_string(),
                requested_version: requested_version.map(str::to_string),
            },
            package_root: destination.clone(),
            installed_at_ms: now_ms(),
        };
        write_record(&staging, &record)?;
        self.install_companion_artifact(&manifest, &staging).await?;
        self.install_language_artifacts(&manifest, &staging, false)
            .await?;
        self.activate_remote_package(&manifest, &staging, &destination, trust_native_grammars)?;
        self.installed(&id)?
            .ok_or_else(|| anyhow::anyhow!("installed plugin `{id}` could not be read"))
    }

    /// Installs the current compatible release of one curated catalog package.
    pub async fn install_catalog(
        &self,
        catalog_url: &str,
        id: &PluginId,
        trust_native_grammars: bool,
    ) -> Result<InstalledPlugin> {
        let catalog = PluginCatalog::fetch(catalog_url).await?;
        let package = catalog.installable(id, host_target())?;
        self.install_catalog_package(catalog_url, package, trust_native_grammars)
            .await
    }

    /// Installs one already-resolved catalog entry after revalidating its release bundle.
    pub async fn install_catalog_package(
        &self,
        catalog_url: &str,
        package: &CatalogPackage,
        trust_native_grammars: bool,
    ) -> Result<InstalledPlugin> {
        validate_catalog_url(catalog_url)?;
        package.validate()?;
        let host_api = Version::parse(RED_HOST_API_VERSION)?;
        anyhow::ensure!(
            package.red_api.matches(&host_api),
            "language pack `{}` requires Red API `{}`, but this release provides `{host_api}`",
            package.id,
            package.red_api
        );
        let artifact = package.artifact(host_target()).ok_or_else(|| {
            anyhow::anyhow!(
                "language pack `{}` has no release for `{}`",
                package.id,
                host_target()
            )
        })?;
        let bytes = download_catalog_artifact(artifact).await?;
        self.install_catalog_archive(
            catalog_url,
            package,
            artifact,
            &bytes,
            trust_native_grammars,
        )
        .await
    }

    async fn install_catalog_archive(
        &self,
        catalog_url: &str,
        package: &CatalogPackage,
        artifact: &CatalogArtifact,
        bytes: &[u8],
        trust_native_grammars: bool,
    ) -> Result<InstalledPlugin> {
        verify_catalog_artifact_bytes(artifact, bytes)?;
        let staging = self.staging_dir("catalog");
        if let Some(parent) = staging.parent() {
            fs::create_dir_all(parent)?;
        }
        remove_if_exists(&staging)?;
        fs::create_dir_all(&staging)?;
        if let Err(error) = extract_catalog_archive(bytes, &staging) {
            let _ = remove_if_exists(&staging);
            return Err(error);
        }

        let result = async {
            let manifest = PluginPackageManifest::load(&staging)?;
            validate_catalog_manifest(package, artifact, &manifest, &staging)?;
            validate_husk_package(&manifest, &staging).await?;
            let id = manifest.plugin.id.clone();
            let destination = self.packages_dir().join(id.as_str());
            let record = PluginInstallRecord {
                schema_version: 1,
                id: id.clone(),
                version: manifest.plugin.version.clone(),
                enabled: true,
                source: PluginInstallSource::Catalog {
                    catalog_url: catalog_url.to_string(),
                    package: package.id.clone(),
                    version: package.version.clone(),
                    resolved_commit: package.resolved_commit.clone(),
                    artifact_url: artifact.url.clone(),
                    artifact_sha256: artifact.sha256.clone(),
                },
                package_root: destination.clone(),
                installed_at_ms: now_ms(),
            };
            write_record(&staging, &record)?;
            self.install_language_artifacts(&manifest, &staging, false)
                .await?;
            self.activate_remote_package(&manifest, &staging, &destination, trust_native_grammars)?;
            self.installed(&id)?
                .ok_or_else(|| anyhow::anyhow!("installed plugin `{id}` could not be read"))
        }
        .await;
        if result.is_err() {
            let _ = remove_if_exists(&staging);
        }
        result
    }

    fn activate_remote_package(
        &self,
        manifest: &PluginPackageManifest,
        staging: &Path,
        destination: &Path,
        trust_native_grammars: bool,
    ) -> Result<()> {
        atomic_replace_directory_validated(staging, destination, |activated| {
            if trust_native_grammars {
                self.approve_package_grammars(manifest, activated)?;
            }
            Ok(())
        })
    }

    /// Lists installed packages in stable identifier order.
    pub fn list(&self) -> Result<Vec<InstalledPlugin>> {
        let mut plugins = Vec::new();
        let directory = self.packages_dir();
        if !directory.is_dir() {
            return Ok(plugins);
        }
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("failed to list {}", directory.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_dir()
                || entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".red-stage-")
            {
                continue;
            }
            let record_path = entry.path().join(INSTALL_RECORD_FILE);
            if !record_path.is_file() {
                continue;
            }
            let record = read_record(&record_path)?;
            plugins.push(installed_from_record(record)?);
        }
        plugins.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(plugins)
    }

    /// Returns enabled Husk entrypoints for editor startup.
    ///
    /// This only reads manifests and installation records. It never launches a
    /// companion, accesses Git, or performs a network request.
    pub fn enabled_husk_plugins(&self) -> Result<Vec<(PluginId, PathBuf)>> {
        self.list()?
            .into_iter()
            .filter(|plugin| plugin.enabled && plugin.compatible)
            .filter_map(|plugin| {
                let manifest = match PluginPackageManifest::load(&plugin.package_root) {
                    Ok(manifest) => manifest,
                    Err(error) => return Some(Err(error)),
                };
                manifest
                    .husk_entry(&plugin.package_root)
                    .map(|entry| Ok((plugin.id, entry)))
            })
            .collect()
    }

    /// Resolves manifest-declared legacy session fields into private plugin storage.
    ///
    /// Unknown fields remain in the session snapshot as well, so a missing or
    /// disabled package never destroys migration data.
    pub fn legacy_session_imports(
        &self,
        legacy: &BTreeMap<String, serde_json::Value>,
    ) -> Result<Vec<(PluginId, String, serde_json::Value)>> {
        let mut imports = Vec::new();
        for plugin in self
            .list()?
            .into_iter()
            .filter(|plugin| plugin.enabled && plugin.compatible)
        {
            let manifest = PluginPackageManifest::load(&plugin.package_root)?;
            for (field, storage_key) in &manifest.migration.legacy_session_fields {
                if let Some(value) = legacy.get(field) {
                    imports.push((plugin.id.clone(), storage_key.clone(), value.clone()));
                }
            }
        }
        Ok(imports)
    }

    /// Returns one installed package.
    pub fn installed(&self, id: &PluginId) -> Result<Option<InstalledPlugin>> {
        let record_path = self
            .packages_dir()
            .join(id.as_str())
            .join(INSTALL_RECORD_FILE);
        if !record_path.is_file() {
            return Ok(None);
        }
        installed_from_record(read_record(&record_path)?).map(Some)
    }

    /// Enables or disables an installed package without deleting its state.
    pub fn set_enabled(&self, id: &PluginId, enabled: bool) -> Result<()> {
        let install_dir = self.packages_dir().join(id.as_str());
        let path = install_dir.join(INSTALL_RECORD_FILE);
        let mut record = read_record(&path)?;
        record.enabled = enabled;
        write_record_atomic(&install_dir, &record)
    }

    /// Explicitly approves every current native grammar shipped by one installed package.
    pub fn trust_package_grammars(&self, id: &PluginId) -> Result<()> {
        let installed = self
            .installed(id)?
            .ok_or_else(|| anyhow::anyhow!("plugin `{id}` is not installed"))?;
        let manifest = PluginPackageManifest::load(&installed.package_root)?;
        self.approve_package_grammars(&manifest, &installed.package_root)
    }

    /// Updates an installed package from its retained source.
    pub async fn update(&self, id: &PluginId) -> Result<InstalledPlugin> {
        self.update_with_trust(id, false).await
    }

    /// Updates a package, optionally approving the new bytes of its native grammars.
    pub async fn update_with_trust(
        &self,
        id: &PluginId,
        trust_native_grammars: bool,
    ) -> Result<InstalledPlugin> {
        let installed = self
            .installed(id)?
            .ok_or_else(|| anyhow::anyhow!("plugin `{id}` is not installed"))?;
        match installed.source {
            PluginInstallSource::Path { path } => {
                self.install_path_with_trust(&path, trust_native_grammars)
                    .await
            }
            PluginInstallSource::GitHub {
                repository,
                requested_version,
            } => {
                self.install_github_with_trust(
                    &repository,
                    requested_version.as_deref(),
                    trust_native_grammars,
                )
                .await
            }
            PluginInstallSource::Catalog {
                catalog_url,
                package,
                ..
            } => {
                self.install_catalog(&catalog_url, &package, trust_native_grammars)
                    .await
            }
        }
    }

    /// Updates every enabled installed package.
    pub async fn update_all(&self) -> Vec<(PluginId, Result<InstalledPlugin>)> {
        let plugins = match self.list() {
            Ok(plugins) => plugins,
            Err(error) => {
                return vec![(
                    PluginId::parse("registry").expect("static plugin id is valid"),
                    Err(error),
                )]
            }
        };
        let mut results = Vec::with_capacity(plugins.len());
        for plugin in plugins.into_iter().filter(|plugin| plugin.enabled) {
            let id = plugin.id;
            let result = self.update(&id).await;
            results.push((id, result));
        }
        results
    }

    /// Removes an installation. Namespaced plugin data survives unless `purge` is true.
    pub fn remove(&self, id: &PluginId, purge: bool) -> Result<()> {
        let install_dir = self.packages_dir().join(id.as_str());
        anyhow::ensure!(install_dir.is_dir(), "plugin `{id}` is not installed");
        fs::remove_dir_all(&install_dir)
            .with_context(|| format!("failed to remove {}", install_dir.display()))?;
        if purge {
            remove_if_exists(&self.config_dir.join("plugin-data").join(id.as_str()))?;
        }
        Ok(())
    }

    async fn install_record(
        &self,
        manifest: &PluginPackageManifest,
        package_root: PathBuf,
        source: PluginInstallSource,
    ) -> Result<InstalledPlugin> {
        let id = manifest.plugin.id.clone();
        let staging = self.staging_dir(id.as_str());
        remove_if_exists(&staging)?;
        fs::create_dir_all(&staging)?;
        let record = PluginInstallRecord {
            schema_version: 1,
            id: id.clone(),
            version: manifest.plugin.version.clone(),
            enabled: true,
            source,
            package_root,
            installed_at_ms: now_ms(),
        };
        write_record(&staging, &record)?;
        atomic_replace_directory(&staging, &self.packages_dir().join(id.as_str()))?;
        self.installed(&id)?
            .ok_or_else(|| anyhow::anyhow!("installed plugin `{id}` could not be read"))
    }

    async fn install_companion_artifact(
        &self,
        manifest: &PluginPackageManifest,
        package_root: &Path,
    ) -> Result<()> {
        let Some(companion) = &manifest.companion else {
            return Ok(());
        };
        if companion.command.is_some() || companion.commands.contains_key(host_target()) {
            return Ok(());
        }
        let target = host_target();
        let Some(artifact) = companion.artifacts.get(target) else {
            anyhow::bail!("plugin has no native companion for `{target}`");
        };
        let response = reqwest::get(&artifact.url)
            .await
            .with_context(|| format!("failed to download companion from {}", artifact.url))?
            .error_for_status()
            .with_context(|| format!("companion download failed for {}", artifact.url))?;
        let bytes = response.bytes().await?;
        let actual = format!("{:x}", Sha256::digest(&bytes));
        anyhow::ensure!(
            actual.eq_ignore_ascii_case(&artifact.sha256),
            "companion checksum mismatch: expected {}, got {actual}",
            artifact.sha256
        );
        let bin_dir = package_root.join(".red").join("bin");
        tokio::fs::create_dir_all(&bin_dir).await?;
        let path = bin_dir.join(companion_binary_name());
        let mut file = tokio::fs::File::create(&path).await?;
        file.write_all(&bytes).await?;
        file.sync_all().await?;
        set_executable(&path)?;
        Ok(())
    }

    async fn install_language_artifacts(
        &self,
        manifest: &PluginPackageManifest,
        package_root: &Path,
        trust_native_grammars: bool,
    ) -> Result<()> {
        for (id, language) in &manifest.languages {
            let Some(grammar) = &language.grammar else {
                continue;
            };
            let Some(artifact) = grammar.targets.get(host_target()) else {
                continue;
            };
            let Some(url) = &artifact.url else {
                continue;
            };
            let directory = grammar_download_directory(package_root)?;
            let bytes = reqwest::get(url)
                .await
                .with_context(|| format!("failed to download grammar for `{id}` from {url}"))?
                .error_for_status()
                .with_context(|| format!("grammar download failed for `{id}` from {url}"))?
                .bytes()
                .await?;
            let actual = format!("{:x}", Sha256::digest(&bytes));
            let expected = artifact.sha256.as_deref().unwrap_or_default();
            anyhow::ensure!(
                actual.eq_ignore_ascii_case(expected),
                "grammar checksum mismatch for `{id}`: expected {expected}, got {actual}"
            );
            let filename = grammar_filename(id);
            let path = directory.join(&filename);
            publish_downloaded_grammar(&path, &bytes).await?;
        }
        if trust_native_grammars {
            self.approve_package_grammars(manifest, package_root)?;
        }
        Ok(())
    }

    fn approve_package_grammars(
        &self,
        manifest: &PluginPackageManifest,
        package_root: &Path,
    ) -> Result<()> {
        let trust = GrammarTrustStore::new(&self.config_dir);
        let paths = manifest
            .languages
            .iter()
            .filter_map(|(id, language)| manifest.grammar_path(package_root, id, language))
            .collect::<Vec<_>>();
        trust.trust_paths(&paths)
    }

    fn staging_dir(&self, label: &str) -> PathBuf {
        self.packages_dir().join(format!(
            ".red-stage-{label}-{}-{}",
            std::process::id(),
            now_ms()
        ))
    }
}

async fn validate_husk_package(
    manifest: &PluginPackageManifest,
    package_root: &Path,
) -> Result<()> {
    let Some(entry) = manifest.husk_entry(package_root) else {
        return Ok(());
    };
    Runtime::new()
        .load_plugin_package(manifest.plugin.id.as_str(), &entry)
        .await
        .with_context(|| format!("failed to compile Husk package {}", entry.display()))
}

fn default_manifest_schema() -> u32 {
    PLUGIN_MANIFEST_SCHEMA
}

fn validate_keymaps(keymaps: &BTreeMap<String, BTreeMap<String, String>>) -> Result<()> {
    const SUPPORTED_MODES: [&str; 6] = [
        "normal",
        "insert",
        "command",
        "visual",
        "visual_line",
        "visual_block",
    ];

    for (mode, bindings) in keymaps {
        anyhow::ensure!(
            SUPPORTED_MODES.contains(&mode.as_str()),
            "plugin keymap uses unsupported mode `{mode}`"
        );

        let mut sequences: Vec<(&str, Vec<&str>)> = Vec::with_capacity(bindings.len());
        for (sequence, command) in bindings {
            anyhow::ensure!(
                !command.trim().is_empty(),
                "plugin keymap command for `{sequence}` in mode `{mode}` is empty"
            );
            let keys = sequence.split_whitespace().collect::<Vec<_>>();
            anyhow::ensure!(
                !keys.is_empty(),
                "plugin keymap sequence in mode `{mode}` is empty"
            );

            for (existing_sequence, existing_keys) in &sequences {
                anyhow::ensure!(
                    keys != *existing_keys,
                    "plugin keymap sequences `{existing_sequence}` and `{sequence}` normalize to the same keys in mode `{mode}`"
                );
                anyhow::ensure!(
                    !keys.starts_with(existing_keys) && !existing_keys.starts_with(&keys),
                    "plugin keymap prefix conflict in mode `{mode}`: `{existing_sequence}` and `{sequence}` cannot both be declared"
                );
            }
            sequences.push((sequence, keys));
        }
    }
    Ok(())
}

fn validate_relative_path(path: &Path) -> Result<()> {
    anyhow::ensure!(!path.as_os_str().is_empty(), "plugin path is empty");
    anyhow::ensure!(!path.is_absolute(), "plugin path must be package-relative");
    anyhow::ensure!(
        path.components()
            .all(|component| matches!(component, Component::Normal(_))),
        "plugin path contains an unsafe component: {}",
        path.display()
    );
    Ok(())
}

fn validate_package_file(package_root: &Path, relative: &Path) -> Result<()> {
    validate_relative_path(relative)?;
    let path = package_root.join(relative);
    anyhow::ensure!(
        path.is_file(),
        "plugin package file does not exist: {}",
        path.display()
    );
    let canonical_root = package_root.canonicalize()?;
    let canonical_path = path.canonicalize()?;
    anyhow::ensure!(
        canonical_path.starts_with(canonical_root),
        "plugin package file escapes its package: {}",
        path.display()
    );
    Ok(())
}

fn grammar_filename(id: &str) -> String {
    let extension = if cfg!(target_os = "windows") {
        "dll"
    } else if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };
    format!("{id}.{extension}")
}

fn grammar_download_directory(package_root: &Path) -> Result<PathBuf> {
    let mut directory = package_root.to_path_buf();
    for component in [".red", "grammars", host_target()] {
        directory.push(component);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) => {
                anyhow::ensure!(
                    !metadata.file_type().is_symlink(),
                    "grammar download directory contains a symbolic link: {}",
                    directory.display()
                );
                anyhow::ensure!(
                    metadata.is_dir(),
                    "grammar download destination is not a directory: {}",
                    directory.display()
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&directory).with_context(|| {
                    format!(
                        "failed to create grammar download directory {}",
                        directory.display()
                    )
                })?;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to inspect grammar download directory {}",
                        directory.display()
                    )
                });
            }
        }
    }
    Ok(directory)
}

async fn publish_downloaded_grammar(path: &Path, bytes: &[u8]) -> Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("downloaded grammar path has no parent"))?;
    let temporary = tempfile::Builder::new()
        .prefix(".red-grammar-")
        .tempfile_in(directory)
        .with_context(|| {
            format!(
                "failed to create downloaded grammar replacement in {}",
                directory.display()
            )
        })?;
    let mut file = tokio::fs::File::from_std(temporary.reopen()?);
    file.write_all(bytes).await?;
    file.sync_all().await?;
    drop(file);
    temporary
        .persist(path)
        .map(|_| ())
        .map_err(|error| error.error)
        .with_context(|| format!("failed to publish downloaded grammar {}", path.display()))
}

fn validate_github_repository(repository: &str) -> Result<()> {
    let parts = repository.split('/').collect::<Vec<_>>();
    anyhow::ensure!(
        parts.len() == 2
            && parts.iter().all(|part| {
                !part.is_empty()
                    && part.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                    })
            }),
        "GitHub plugin source must be `owner/repository`"
    );
    Ok(())
}

fn installed_from_record(record: PluginInstallRecord) -> Result<InstalledPlugin> {
    anyhow::ensure!(
        record.schema_version == 1,
        "unsupported install record schema"
    );
    let manifest = PluginPackageManifest::load(&record.package_root)?;
    anyhow::ensure!(
        manifest.plugin.id == record.id,
        "install record id does not match plugin manifest"
    );
    let host_api = Version::parse(RED_HOST_API_VERSION)?;
    let has_husk = manifest.husk_entry(&record.package_root).is_some();
    let languages = manifest.languages.keys().cloned().collect::<Vec<_>>();
    let has_native_grammars = manifest.languages.iter().any(|(id, language)| {
        language
            .grammar
            .as_ref()
            .is_some_and(|grammar| grammar.builtin.is_none())
            && manifest
                .grammar_path(&record.package_root, id, language)
                .is_some()
    });
    Ok(InstalledPlugin {
        id: record.id,
        name: manifest.plugin.name,
        version: manifest.plugin.version,
        enabled: record.enabled,
        compatible: manifest.plugin.red_api.matches(&host_api),
        has_companion: manifest.companion.is_some(),
        has_husk,
        has_languages: !manifest.languages.is_empty(),
        has_native_grammars,
        languages,
        source: record.source,
        package_root: record.package_root,
    })
}

async fn download_catalog_artifact(artifact: &CatalogArtifact) -> Result<Vec<u8>> {
    anyhow::ensure!(
        artifact.size <= MAX_PACKAGE_ARCHIVE_BYTES as u64,
        "catalog package archive exceeds the {MAX_PACKAGE_ARCHIVE_BYTES} byte safety limit"
    );
    let mut response = reqwest::get(&artifact.url)
        .await
        .with_context(|| format!("failed to download catalog package from {}", artifact.url))?
        .error_for_status()
        .with_context(|| format!("catalog package download failed for {}", artifact.url))?;
    if let Some(length) = response.content_length() {
        anyhow::ensure!(
            length <= MAX_PACKAGE_ARCHIVE_BYTES as u64,
            "catalog package archive exceeds the {MAX_PACKAGE_ARCHIVE_BYTES} byte safety limit"
        );
    }
    let mut bytes = Vec::with_capacity(usize::try_from(artifact.size).unwrap_or_default());
    while let Some(chunk) = response.chunk().await? {
        anyhow::ensure!(
            bytes.len().saturating_add(chunk.len()) <= MAX_PACKAGE_ARCHIVE_BYTES,
            "catalog package archive exceeds the {MAX_PACKAGE_ARCHIVE_BYTES} byte safety limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    verify_catalog_artifact_bytes(artifact, &bytes)?;
    Ok(bytes)
}

fn verify_catalog_artifact_bytes(artifact: &CatalogArtifact, bytes: &[u8]) -> Result<()> {
    anyhow::ensure!(
        bytes.len() as u64 == artifact.size,
        "catalog package size mismatch: expected {}, got {}",
        artifact.size,
        bytes.len()
    );
    let actual = format!("{:x}", Sha256::digest(bytes));
    anyhow::ensure!(
        actual.eq_ignore_ascii_case(&artifact.sha256),
        "catalog package checksum mismatch: expected {}, got {actual}",
        artifact.sha256
    );
    Ok(())
}

fn extract_catalog_archive(bytes: &[u8], destination: &Path) -> Result<()> {
    let decoder = GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    let mut paths = BTreeSet::new();
    let mut total_size = 0_u64;
    let mut count = 0_usize;
    for entry in archive
        .entries()
        .context("failed to read catalog package archive")?
    {
        let mut entry = entry.context("failed to read catalog package entry")?;
        count = count.saturating_add(1);
        anyhow::ensure!(
            count <= MAX_PACKAGE_ARCHIVE_FILES,
            "catalog package archive exceeds the {MAX_PACKAGE_ARCHIVE_FILES} file safety limit"
        );
        let relative = safe_archive_path(&entry.path()?)?;
        anyhow::ensure!(
            relative != Path::new(INSTALL_RECORD_FILE),
            "catalog package archive contains Red's reserved install record"
        );
        anyhow::ensure!(
            paths.insert(relative.clone()),
            "catalog package archive repeats {}",
            relative.display()
        );
        let target = destination.join(&relative);
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        anyhow::ensure!(
            kind.is_file(),
            "catalog package archive contains unsupported entry {}",
            relative.display()
        );
        let size = entry.header().size()?;
        total_size = total_size.saturating_add(size);
        anyhow::ensure!(
            total_size <= MAX_PACKAGE_UNPACKED_BYTES,
            "catalog package archive exceeds the {MAX_PACKAGE_UNPACKED_BYTES} byte unpacked safety limit"
        );
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&target)
            .with_context(|| {
                format!("failed to create catalog package file {}", target.display())
            })?;
        let copied = std::io::copy(
            &mut entry.by_ref().take(size.saturating_add(1)),
            &mut output,
        )?;
        anyhow::ensure!(
            copied == size,
            "catalog package entry {} has an invalid size",
            relative.display()
        );
        output.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = entry.header().mode()? & 0o777;
            fs::set_permissions(&target, fs::Permissions::from_mode(mode))?;
        }
    }
    anyhow::ensure!(
        destination.join(PLUGIN_MANIFEST_FILE).is_file(),
        "catalog package archive has no root {PLUGIN_MANIFEST_FILE}"
    );
    Ok(())
}

fn safe_archive_path(path: &Path) -> Result<PathBuf> {
    let mut safe = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => safe.push(value),
            _ => anyhow::bail!(
                "catalog package archive contains unsafe path {}",
                path.display()
            ),
        }
    }
    anyhow::ensure!(
        !safe.as_os_str().is_empty(),
        "catalog package archive contains an empty path"
    );
    Ok(safe)
}

fn validate_catalog_manifest(
    package: &CatalogPackage,
    artifact: &CatalogArtifact,
    manifest: &PluginPackageManifest,
    package_root: &Path,
) -> Result<()> {
    anyhow::ensure!(
        manifest.plugin.id == package.id,
        "catalog package id does not match its manifest"
    );
    anyhow::ensure!(
        manifest.plugin.name == package.name,
        "catalog package name does not match its manifest"
    );
    anyhow::ensure!(
        manifest.plugin.version == package.version,
        "catalog package version does not match its manifest"
    );
    anyhow::ensure!(
        manifest.plugin.red_api == package.red_api,
        "catalog package Red API requirement does not match its manifest"
    );
    anyhow::ensure!(
        manifest.plugin.husk_manifest.is_none()
            && manifest.plugin.entry.is_none()
            && manifest.companion.is_none()
            && manifest.activation.events.is_empty()
            && manifest.activation.commands.is_empty()
            && manifest.keymaps.is_empty(),
        "catalog language packs cannot contain Husk code, native companions, activation events, or keymaps"
    );
    let manifest_languages = manifest.languages.keys().cloned().collect::<BTreeSet<_>>();
    let catalog_languages = package.languages.iter().cloned().collect::<BTreeSet<_>>();
    anyhow::ensure!(
        manifest_languages == catalog_languages,
        "catalog language list does not match its manifest"
    );

    let mut actual_grammars = BTreeMap::new();
    for (language_id, language) in &manifest.languages {
        let Some(grammar) = &language.grammar else {
            continue;
        };
        anyhow::ensure!(
            grammar.targets.values().all(|target| target.url.is_none()),
            "catalog package grammars must be bundled instead of downloaded indirectly"
        );
        let Some(path) = manifest.grammar_path(package_root, language_id, language) else {
            continue;
        };
        let metadata = fs::metadata(&path)
            .with_context(|| format!("failed to inspect catalog grammar {}", path.display()))?;
        anyhow::ensure!(
            metadata.len() <= MAX_CATALOG_GRAMMAR_BYTES,
            "catalog grammar {} exceeds the safety limit",
            path.display()
        );
        let bytes = fs::read(&path)
            .with_context(|| format!("failed to read catalog grammar {}", path.display()))?;
        actual_grammars.insert(language_id.clone(), format!("{:x}", Sha256::digest(bytes)));
    }
    let grammar_digests_match = actual_grammars.len() == artifact.grammars.len()
        && actual_grammars.iter().all(|(language, actual)| {
            artifact
                .grammars
                .get(language)
                .is_some_and(|expected| actual.eq_ignore_ascii_case(expected))
        });
    anyhow::ensure!(
        grammar_digests_match,
        "catalog grammar digests do not match the package manifest"
    );
    Ok(())
}

fn read_record(path: &Path) -> Result<PluginInstallRecord> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("failed to read install record {}", path.display()))?;
    serde_json::from_str(&source)
        .with_context(|| format!("failed to parse install record {}", path.display()))
}

fn write_record(directory: &Path, record: &PluginInstallRecord) -> Result<()> {
    fs::create_dir_all(directory)?;
    let bytes = serde_json::to_vec_pretty(record)?;
    fs::write(directory.join(INSTALL_RECORD_FILE), bytes)?;
    Ok(())
}

fn write_record_atomic(directory: &Path, record: &PluginInstallRecord) -> Result<()> {
    let path = directory.join(INSTALL_RECORD_FILE);
    let temporary = directory.join(format!("{INSTALL_RECORD_FILE}.tmp"));
    fs::write(&temporary, serde_json::to_vec_pretty(record)?)?;
    fs::rename(&temporary, &path)?;
    Ok(())
}

fn atomic_replace_directory(staging: &Path, destination: &Path) -> Result<()> {
    atomic_replace_directory_validated(staging, destination, |_| Ok(()))
}

fn atomic_replace_directory_validated(
    staging: &Path,
    destination: &Path,
    validate: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow::anyhow!("plugin destination has no parent"))?;
    fs::create_dir_all(parent)?;
    let backup = parent.join(format!(
        ".red-backup-{}-{}",
        destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("plugin"),
        now_ms()
    ));
    if destination.exists() {
        fs::rename(destination, &backup).with_context(|| {
            format!(
                "failed to stage previous plugin installation {}",
                destination.display()
            )
        })?;
    }
    if let Err(error) = fs::rename(staging, destination) {
        if backup.exists() {
            let _ = fs::rename(&backup, destination);
        }
        return Err(error).with_context(|| {
            format!(
                "failed to activate plugin installation {}",
                destination.display()
            )
        });
    }
    if let Err(error) = validate(destination) {
        if let Err(rollback_error) = remove_if_exists(destination).and_then(|()| {
            if backup.exists() {
                fs::rename(&backup, destination).with_context(|| {
                    format!(
                        "failed to restore previous plugin installation {}",
                        destination.display()
                    )
                })?;
            }
            Ok(())
        }) {
            return Err(error).context(format!(
                "failed to roll back plugin installation {}: {rollback_error:#}",
                destination.display()
            ));
        }
        return Err(error);
    }
    remove_if_exists(&backup)?;
    Ok(())
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))
        }
        Ok(_) => {
            fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", path.display())),
    }
}

fn bounded_stderr(stderr: &[u8]) -> String {
    const LIMIT: usize = 4 * 1024;
    String::from_utf8_lossy(&stderr[..stderr.len().min(LIMIT)])
        .trim()
        .to_string()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

fn host_target() -> &'static str {
    crate::language::host_target()
}

fn companion_binary_name() -> &'static str {
    if cfg!(windows) {
        "companion.exe"
    } else {
        "companion"
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog_package(artifact: CatalogArtifact) -> CatalogPackage {
        CatalogPackage {
            id: PluginId::parse("build-languages").unwrap(),
            name: "Language package".to_string(),
            version: Version::parse("1.0.0").unwrap(),
            red_api: VersionReq::parse(&format!("^{RED_HOST_API_VERSION}")).unwrap(),
            description: "Buildspec syntax support".to_string(),
            repository: "codersauce/red-language-packs".to_string(),
            source_path: PathBuf::from("packs/buildspec"),
            resolved_commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
            license: "MIT".to_string(),
            tier: super::super::catalog::CatalogTier::Official,
            languages: vec!["buildspec".to_string()],
            requirements: Vec::new(),
            artifacts: BTreeMap::from([(host_target().to_string(), artifact)]),
        }
    }

    fn catalog_archive() -> (Vec<u8>, CatalogArtifact) {
        use flate2::{write::GzEncoder, Compression};

        let package = tempfile::tempdir().unwrap();
        let grammars = package.path().join("grammars");
        fs::create_dir_all(&grammars).unwrap();
        let grammar = b"catalog grammar bytes";
        fs::write(grammars.join("buildspec.so"), grammar).unwrap();
        fs::write(
            package.path().join(PLUGIN_MANIFEST_FILE),
            format!(
                r#"
schema_version = 1

[plugin]
id = "build-languages"
name = "Language package"
version = "1.0.0"
red_api = "^{RED_HOST_API_VERSION}"

[languages.buildspec]
extensions = ["build"]

[languages.buildspec.grammar]
path = "grammars/buildspec.so"
symbol = "tree_sitter_buildspec"
"#
            ),
        )
        .unwrap();
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut builder = tar::Builder::new(encoder);
        builder
            .append_path_with_name(
                package.path().join(PLUGIN_MANIFEST_FILE),
                PLUGIN_MANIFEST_FILE,
            )
            .unwrap();
        builder
            .append_path_with_name(
                package.path().join("grammars/buildspec.so"),
                "grammars/buildspec.so",
            )
            .unwrap();
        let encoder = builder.into_inner().unwrap();
        let bytes = encoder.finish().unwrap();
        let artifact = CatalogArtifact {
            url: "https://github.com/codersauce/red-language-packs/releases/download/buildspec%2Fv1.0.0/buildspec.tar.gz".to_string(),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            size: bytes.len() as u64,
            grammars: BTreeMap::from([(
                "buildspec".to_string(),
                format!("{:x}", Sha256::digest(grammar)),
            )]),
        };
        (bytes, artifact)
    }

    fn write_package(root: &Path, id: &str, version: &str) {
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join(PLUGIN_MANIFEST_FILE),
            format!(
                r#"
schema_version = 1

[plugin]
id = "{id}"
name = "Test plugin"
version = "{version}"
red_api = "^{RED_HOST_API_VERSION}"
husk_manifest = "Husk.toml"

[activation]
commands = ["Test"]
"#
            ),
        )
        .unwrap();
        fs::write(
            root.join("Husk.toml"),
            r#"
schema_version = 1
[package]
name = "test-plugin"
version = "0.1.0"
entry = "src/main.hk"
"#,
        )
        .unwrap();
        fs::write(root.join("src/main.hk"), "pub fn activate() {}").unwrap();
    }

    fn write_language_package(root: &Path, id: &str) {
        fs::write(
            root.join(PLUGIN_MANIFEST_FILE),
            format!(
                r#"
schema_version = 1

[plugin]
id = "{id}"
name = "Language package"
version = "1.0.0"
red_api = "^{RED_HOST_API_VERSION}"

[languages.buildspec]
extensions = ["build"]
filenames = ["Buildfile"]

[languages.buildspec.grammar]
builtin = "rust"
"#
            ),
        )
        .unwrap();
    }

    fn write_oversized_native_language_package(root: &Path, id: &str, version: &str) {
        let grammar_dir = root.join("grammars");
        fs::create_dir_all(&grammar_dir).unwrap();
        fs::File::create(grammar_dir.join("buildspec.so"))
            .unwrap()
            .set_len(64 * 1024 * 1024 + 1)
            .unwrap();
        fs::write(
            root.join(PLUGIN_MANIFEST_FILE),
            format!(
                r#"
schema_version = 1

[plugin]
id = "{id}"
name = "Native language package"
version = "{version}"
red_api = "^{RED_HOST_API_VERSION}"

[languages.buildspec]
extensions = ["build"]

[languages.buildspec.grammar]
path = "grammars/buildspec.so"
symbol = "tree_sitter_buildspec"
"#
            ),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn downloaded_grammar_replaces_existing_destination() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(grammar_filename("buildspec"));

        publish_downloaded_grammar(&path, b"original grammar")
            .await
            .unwrap();
        publish_downloaded_grammar(&path, b"replacement grammar")
            .await
            .unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"replacement grammar");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn catalog_archive_installs_as_an_independent_language_package() {
        let config = tempfile::tempdir().unwrap();
        let manager = PluginPackageManager::new(config.path());
        let (bytes, artifact) = catalog_archive();
        let package = catalog_package(artifact.clone());

        let installed = manager
            .install_catalog_archive(
                "https://raw.githubusercontent.com/codersauce/red-language-packs/main/catalog/v1.json",
                &package,
                &artifact,
                &bytes,
                false,
            )
            .await
            .unwrap();

        assert_eq!(installed.id.as_str(), "build-languages");
        assert_eq!(installed.languages, ["buildspec"]);
        assert!(!installed.has_husk);
        assert!(matches!(
            installed.source,
            PluginInstallSource::Catalog {
                package,
                artifact_sha256,
                ..
            } if package.as_str() == "build-languages" && artifact_sha256 == artifact.sha256
        ));
        assert!(installed
            .package_root
            .join("grammars/buildspec.so")
            .is_file());
    }

    #[tokio::test]
    async fn catalog_package_install_revalidates_caller_supplied_metadata() {
        let config = tempfile::tempdir().unwrap();
        let manager = PluginPackageManager::new(config.path());
        let (_, artifact) = catalog_archive();
        let package = catalog_package(artifact);

        let error = manager
            .install_catalog_package("https://example.com/catalog.json", &package, false)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("GitHub HTTPS"));
        assert!(manager.list().unwrap().is_empty());
    }

    #[tokio::test]
    async fn catalog_archive_accepts_uppercase_grammar_digests() {
        let config = tempfile::tempdir().unwrap();
        let manager = PluginPackageManager::new(config.path());
        let (bytes, mut artifact) = catalog_archive();
        for digest in artifact.grammars.values_mut() {
            *digest = digest.to_ascii_uppercase();
        }
        let package = catalog_package(artifact.clone());

        let installed = manager
            .install_catalog_archive(
                "https://raw.githubusercontent.com/codersauce/red-language-packs/main/catalog/v1.json",
                &package,
                &artifact,
                &bytes,
                false,
            )
            .await
            .unwrap();

        assert_eq!(installed.id.as_str(), "build-languages");
        assert!(installed
            .package_root
            .join("grammars/buildspec.so")
            .is_file());
    }

    #[tokio::test]
    async fn catalog_archive_rejects_mismatched_grammar_without_replacing_installation() {
        let config = tempfile::tempdir().unwrap();
        let manager = PluginPackageManager::new(config.path());
        let (bytes, mut artifact) = catalog_archive();
        artifact.grammars.insert(
            "buildspec".to_string(),
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".to_string(),
        );
        let package = catalog_package(artifact.clone());

        let error = manager
            .install_catalog_archive(
                "https://raw.githubusercontent.com/codersauce/red-language-packs/main/catalog/v1.json",
                &package,
                &artifact,
                &bytes,
                true,
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("grammar digests"));
        assert!(manager.list().unwrap().is_empty());
    }

    #[test]
    fn catalog_archive_paths_reject_escape_and_absolute_components() {
        assert!(safe_archive_path(Path::new("../red-plugin.toml")).is_err());
        assert!(safe_archive_path(Path::new("/red-plugin.toml")).is_err());
        assert_eq!(
            safe_archive_path(Path::new("./queries/highlights.scm")).unwrap(),
            PathBuf::from("queries/highlights.scm")
        );
    }

    #[test]
    fn grammar_download_directory_creates_real_package_directories() {
        let package = tempfile::tempdir().unwrap();

        let directory = grammar_download_directory(package.path()).unwrap();

        assert_eq!(
            directory,
            package
                .path()
                .join(".red")
                .join("grammars")
                .join(host_target())
        );
        assert!(directory.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn grammar_download_directory_rejects_every_symlinked_component() {
        for depth in 0..3 {
            let package = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let components = [".red", "grammars", host_target()];
            let mut parent = package.path().to_path_buf();
            for component in components.iter().take(depth) {
                parent.push(component);
                fs::create_dir(&parent).unwrap();
            }
            let symlink = parent.join(components[depth]);
            std::os::unix::fs::symlink(outside.path(), &symlink).unwrap();

            let error = grammar_download_directory(package.path()).unwrap_err();

            assert!(error.to_string().contains("symbolic link"));
            assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
        }
    }

    #[tokio::test]
    async fn github_update_restores_previous_installation_when_grammar_approval_fails() {
        let config = tempfile::tempdir().unwrap();
        let previous = tempfile::tempdir().unwrap();
        write_language_package(previous.path(), "build-languages");
        let manager = PluginPackageManager::new(config.path());
        let original = manager.install_path(previous.path()).await.unwrap();
        let destination = manager.packages_dir().join(original.id.as_str());
        let staging = manager.staging_dir("github-test");
        fs::create_dir_all(&staging).unwrap();
        write_oversized_native_language_package(&staging, "build-languages", "2.0.0");
        let manifest = PluginPackageManifest::load(&staging).unwrap();

        let error = manager
            .activate_remote_package(&manifest, &staging, &destination, true)
            .unwrap_err();

        assert!(error.to_string().contains("safety limit"));
        let restored = manager.installed(&original.id).unwrap().unwrap();
        assert_eq!(restored.version, Version::parse("1.0.0").unwrap());
        assert_eq!(
            restored.package_root,
            previous.path().canonicalize().unwrap()
        );
        assert!(!staging.exists());
    }

    #[test]
    fn failed_github_grammar_approval_does_not_publish_new_installation() {
        let config = tempfile::tempdir().unwrap();
        let manager = PluginPackageManager::new(config.path());
        let destination = manager.packages_dir().join("build-languages");
        let staging = manager.staging_dir("github-test");
        fs::create_dir_all(&staging).unwrap();
        write_oversized_native_language_package(&staging, "build-languages", "1.0.0");
        let manifest = PluginPackageManifest::load(&staging).unwrap();

        let error = manager
            .activate_remote_package(&manifest, &staging, &destination, true)
            .unwrap_err();

        assert!(error.to_string().contains("safety limit"));
        assert!(!destination.exists());
        assert!(manager.list().unwrap().is_empty());
    }

    #[tokio::test]
    async fn language_only_package_installs_without_husk_or_native_companion() {
        let config = tempfile::tempdir().unwrap();
        let package = tempfile::tempdir().unwrap();
        write_language_package(package.path(), "build-languages");
        let manager = PluginPackageManager::new(config.path());

        let installed = manager.install_path(package.path()).await.unwrap();
        let manifest = PluginPackageManifest::load(&installed.package_root).unwrap();

        assert_eq!(installed.id.as_str(), "build-languages");
        assert!(installed.has_languages);
        assert!(!installed.has_companion);
        assert_eq!(manifest.languages["buildspec"].filenames, ["Buildfile"]);
        assert!(manifest.husk_entry(&installed.package_root).is_none());
    }

    #[test]
    fn language_only_manifest_declares_native_highlighting_and_lsp() {
        let manifest: PluginPackageManifest = toml::from_str(&format!(
            r#"
schema_version = 1

[plugin]
id = "acme-language"
name = "Acme language support"
version = "0.1.0"
red_api = "^{RED_HOST_API_VERSION}"

[languages.acme]
extensions = ["acme"]
filenames = ["Acmefile"]
comment = "// %s"

[languages.acme.grammar]
path = "grammars/acme.so"
symbol = "tree_sitter_acme"
highlights = ["queries/highlights.scm"]

[languages.acme.lsp]
command = "acme-language-server"
root_markers = ["Acmefile", ".git"]
"#
        ))
        .unwrap();
        let language = manifest.languages.get("acme").unwrap();
        let grammar = language.grammar.as_ref().unwrap();
        let server = language.lsp.as_ref().unwrap();

        assert_eq!(manifest.plugin.id.as_str(), "acme-language");
        assert_eq!(language.extensions, ["acme"]);
        assert_eq!(language.filenames, ["Acmefile"]);
        assert_eq!(language.comment.as_deref(), Some("// %s"));
        assert_eq!(grammar.path.as_deref(), Some(Path::new("grammars/acme.so")));
        assert_eq!(grammar.symbol.as_deref(), Some("tree_sitter_acme"));
        assert_eq!(
            grammar.highlights,
            [PathBuf::from("queries/highlights.scm")]
        );
        assert_eq!(server.command.as_deref(), Some("acme-language-server"));
        assert_eq!(server.root_markers, ["Acmefile", ".git"]);
    }

    #[test]
    fn language_package_rejects_unsafe_grammar_paths_and_unverified_urls() {
        let package = tempfile::tempdir().unwrap();
        write_language_package(package.path(), "build-languages");
        let manifest_path = package.path().join(PLUGIN_MANIFEST_FILE);
        let mut source = fs::read_to_string(&manifest_path).unwrap();
        source.push_str("path = \"../unsafe.so\"\n");
        fs::write(&manifest_path, source).unwrap();
        assert!(PluginPackageManifest::load(package.path())
            .unwrap_err()
            .to_string()
            .contains("unsafe component"));

        write_language_package(package.path(), "build-languages");
        let mut source = fs::read_to_string(&manifest_path).unwrap();
        source.push_str(
            "\n[languages.buildspec.grammar.targets.aarch64-apple-darwin]\nurl = \"https://example.com/grammar.so\"\nsha256 = \"0000000000000000000000000000000000000000000000000000000000000000\"\n",
        );
        fs::write(&manifest_path, source).unwrap();
        assert!(PluginPackageManifest::load(package.path())
            .unwrap_err()
            .to_string()
            .contains("GitHub HTTPS"));
    }

    #[test]
    fn rejects_unsafe_manifest_paths() {
        let directory = tempfile::tempdir().unwrap();
        write_package(directory.path(), "test", "1.0.0");
        let source = fs::read_to_string(directory.path().join(PLUGIN_MANIFEST_FILE))
            .unwrap()
            .replace("Husk.toml", "../Husk.toml");
        fs::write(directory.path().join(PLUGIN_MANIFEST_FILE), source).unwrap();

        assert!(PluginPackageManifest::load(directory.path())
            .unwrap_err()
            .to_string()
            .contains("unsafe component"));
    }

    #[test]
    fn rejects_keymap_prefix_conflicts() {
        let directory = tempfile::tempdir().unwrap();
        write_package(directory.path(), "test", "1.0.0");
        let manifest_path = directory.path().join(PLUGIN_MANIFEST_FILE);
        let mut source = fs::read_to_string(&manifest_path).unwrap();
        source.push_str(
            r#"

[keymaps.normal]
"Space R" = "Test"
"Space R g" = "Test"
"#,
        );
        fs::write(manifest_path, source).unwrap();

        let error = PluginPackageManifest::load(directory.path()).unwrap_err();
        assert!(error.to_string().contains("keymap prefix conflict"));
        assert!(error.to_string().contains("Space R"));
        assert!(error.to_string().contains("Space R g"));
    }

    #[test]
    fn accepts_keymap_siblings_under_a_shared_prefix() {
        let directory = tempfile::tempdir().unwrap();
        write_package(directory.path(), "test", "1.0.0");
        let manifest_path = directory.path().join(PLUGIN_MANIFEST_FILE);
        let mut source = fs::read_to_string(&manifest_path).unwrap();
        source.push_str(
            r#"

[keymaps.normal]
"Space R g" = "Test"
"Space R n" = "Test"
"#,
        );
        fs::write(manifest_path, source).unwrap();

        PluginPackageManifest::load(directory.path()).unwrap();
    }

    #[test]
    fn rejects_keymaps_for_unsupported_modes() {
        let directory = tempfile::tempdir().unwrap();
        write_package(directory.path(), "test", "1.0.0");
        let manifest_path = directory.path().join(PLUGIN_MANIFEST_FILE);
        let mut source = fs::read_to_string(&manifest_path).unwrap();
        source.push_str(
            r#"

[keymaps.select]
"Space R" = "Test"
"#,
        );
        fs::write(manifest_path, source).unwrap();

        let error = PluginPackageManifest::load(directory.path()).unwrap_err();
        assert!(error.to_string().contains("unsupported mode `select`"));
    }

    #[tokio::test]
    async fn local_install_preserves_source_and_data_across_remove() {
        let config = tempfile::tempdir().unwrap();
        let package = tempfile::tempdir().unwrap();
        write_package(package.path(), "test", "1.0.0");
        let manager = PluginPackageManager::new(config.path());
        let plugin = manager.install_path(package.path()).await.unwrap();
        assert_eq!(plugin.id.as_str(), "test");
        assert_eq!(plugin.package_root, package.path().canonicalize().unwrap());

        let data = config.path().join("plugin-data/test/session.json");
        fs::create_dir_all(data.parent().unwrap()).unwrap();
        fs::write(&data, "{}").unwrap();
        manager.remove(&plugin.id, false).unwrap();
        assert!(data.exists());
    }

    #[tokio::test]
    async fn disable_and_update_local_install_are_transactional() {
        let config = tempfile::tempdir().unwrap();
        let package = tempfile::tempdir().unwrap();
        write_package(package.path(), "test", "1.0.0");
        let manager = PluginPackageManager::new(config.path());
        let plugin = manager.install_path(package.path()).await.unwrap();
        manager.set_enabled(&plugin.id, false).unwrap();
        assert!(!manager.installed(&plugin.id).unwrap().unwrap().enabled);

        write_package(package.path(), "test", "1.1.0");
        let updated = manager.update(&plugin.id).await.unwrap();
        assert_eq!(updated.version, Version::parse("1.1.0").unwrap());
    }

    #[tokio::test]
    async fn installed_package_declares_legacy_session_import_without_host_product_knowledge() {
        let config = tempfile::tempdir().unwrap();
        let package = tempfile::tempdir().unwrap();
        write_package(package.path(), "extracted-feature", "1.0.0");
        let manifest_path = package.path().join(PLUGIN_MANIFEST_FILE);
        let mut source = fs::read_to_string(&manifest_path).unwrap();
        source.push_str(
            r#"

[migration.legacy_session_fields]
old_feature = "legacy_session"
"#,
        );
        fs::write(manifest_path, source).unwrap();

        let manager = PluginPackageManager::new(config.path());
        manager.install_path(package.path()).await.unwrap();
        let legacy = BTreeMap::from([(
            "old_feature".to_string(),
            serde_json::json!({ "version": 1 }),
        )]);
        let imports = manager.legacy_session_imports(&legacy).unwrap();

        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].0.as_str(), "extracted-feature");
        assert_eq!(imports[0].1, "legacy_session");
        assert_eq!(imports[0].2, serde_json::json!({ "version": 1 }));
        assert_eq!(
            legacy.get("old_feature"),
            Some(&serde_json::json!({ "version": 1 }))
        );
    }
}
