//! Curated package catalog models and bounded retrieval.
//!
//! A catalog is discovery metadata, not native-code approval. Entries resolve a stable
//! package id to one immutable, checksummed release bundle for each supported host target.

use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, Result};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

use super::{
    package::PluginId,
    registry::{host_api_requirement_is_supported, SUPPORTED_HOST_API_VERSIONS},
};

pub const PLUGIN_CATALOG_SCHEMA: u32 = 1;
pub const DEFAULT_PLUGIN_CATALOG_URL: &str =
    "https://github.com/codersauce/red-language-packs/releases/download/catalog-v1/v1.json";
const MAX_CATALOG_BYTES: usize = 1024 * 1024;

/// Returns the official catalog URL, allowing an explicit development override.
#[must_use]
pub fn catalog_url() -> String {
    std::env::var("RED_PLUGIN_CATALOG_URL")
        .unwrap_or_else(|_| DEFAULT_PLUGIN_CATALOG_URL.to_string())
}

/// One versioned snapshot of curated package releases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginCatalog {
    pub schema_version: u32,
    #[serde(default)]
    pub packages: Vec<CatalogPackage>,
}

/// Catalog review tier displayed to users without implying native-code trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogTier {
    Official,
    Curated,
}

impl CatalogTier {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Official => "Official",
            Self::Curated => "Curated",
        }
    }
}

/// One independently versioned language package in the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogPackage {
    pub id: PluginId,
    pub name: String,
    pub version: Version,
    pub red_api: VersionReq,
    pub description: String,
    pub repository: String,
    pub source_path: PathBuf,
    pub resolved_commit: String,
    pub license: String,
    pub tier: CatalogTier,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub requirements: Vec<CatalogRequirement>,
    #[serde(default)]
    pub artifacts: BTreeMap<String, CatalogArtifact>,
}

/// External command needed for optional or complete language intelligence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogRequirement {
    pub command: String,
    pub purpose: String,
    #[serde(default)]
    pub optional: bool,
}

/// Immutable package bundle for one Red host target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogArtifact {
    pub url: String,
    pub sha256: String,
    pub size: u64,
    /// Native grammar digests keyed by language id.
    #[serde(default)]
    pub grammars: BTreeMap<String, String>,
}

impl PluginCatalog {
    /// Parses and validates one complete catalog snapshot.
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        anyhow::ensure!(
            bytes.len() <= MAX_CATALOG_BYTES,
            "plugin catalog exceeds the {} byte safety limit",
            MAX_CATALOG_BYTES
        );
        let catalog: Self =
            serde_json::from_slice(bytes).context("failed to parse plugin catalog")?;
        catalog.validate()?;
        Ok(catalog)
    }

    /// Fetches one bounded HTTPS catalog snapshot.
    pub async fn fetch(url: &str) -> Result<Self> {
        validate_catalog_url(url)?;
        let mut response = reqwest::get(url)
            .await
            .with_context(|| format!("failed to download plugin catalog from {url}"))?
            .error_for_status()
            .with_context(|| format!("plugin catalog download failed for {url}"))?;
        if let Some(length) = response.content_length() {
            anyhow::ensure!(
                length <= MAX_CATALOG_BYTES as u64,
                "plugin catalog exceeds the {MAX_CATALOG_BYTES} byte safety limit"
            );
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            anyhow::ensure!(
                bytes.len().saturating_add(chunk.len()) <= MAX_CATALOG_BYTES,
                "plugin catalog exceeds the {MAX_CATALOG_BYTES} byte safety limit"
            );
            bytes.extend_from_slice(&chunk);
        }
        Self::from_slice(&bytes)
    }

    /// Fetches the official catalog or the explicit development override.
    pub async fn fetch_default() -> Result<Self> {
        Self::fetch(&catalog_url()).await
    }

    /// Returns a package only when it supports this Red release and host target.
    pub fn installable(&self, id: &PluginId, target: &str) -> Result<&CatalogPackage> {
        let package = self
            .packages
            .iter()
            .find(|package| &package.id == id)
            .ok_or_else(|| anyhow::anyhow!("language pack `{id}` is not in the catalog"))?;
        anyhow::ensure!(
            package.supports_current_red_release()?,
            "language pack `{id}` requires Red API `{}`, but this release supports {}",
            package.red_api,
            SUPPORTED_HOST_API_VERSIONS.join(", ")
        );
        anyhow::ensure!(
            package.artifacts.contains_key(target),
            "language pack `{id}` has no release for `{target}`"
        );
        Ok(package)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.schema_version == PLUGIN_CATALOG_SCHEMA,
            "unsupported plugin catalog schema {}",
            self.schema_version
        );
        let mut ids = std::collections::BTreeSet::new();
        for package in &self.packages {
            anyhow::ensure!(
                ids.insert(package.id.clone()),
                "duplicate catalog package `{}`",
                package.id
            );
            package.validate()?;
        }
        Ok(())
    }
}

impl CatalogPackage {
    /// Whether this package targets any Red API version supported by this release.
    pub fn supports_current_red_release(&self) -> Result<bool> {
        host_api_requirement_is_supported(&self.red_api)
    }

    #[must_use]
    pub fn artifact(&self, target: &str) -> Option<&CatalogArtifact> {
        self.artifacts.get(target)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.name.trim().is_empty(),
            "catalog package name is empty"
        );
        anyhow::ensure!(
            !self.description.trim().is_empty(),
            "catalog package `{}` has an empty description",
            self.id
        );
        anyhow::ensure!(
            !self.license.trim().is_empty(),
            "catalog package `{}` has an empty license",
            self.id
        );
        validate_repository(&self.repository)?;
        validate_relative_path(&self.source_path)?;
        anyhow::ensure!(
            self.resolved_commit.len() == 40
                && self
                    .resolved_commit
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit()),
            "catalog package `{}` has an invalid resolved commit",
            self.id
        );
        anyhow::ensure!(
            !self.languages.is_empty(),
            "catalog package `{}` does not declare a language",
            self.id
        );
        let mut languages = std::collections::BTreeSet::new();
        for language in &self.languages {
            anyhow::ensure!(
                valid_identifier(language),
                "catalog package `{}` has invalid language id `{language}`",
                self.id
            );
            anyhow::ensure!(
                languages.insert(language),
                "catalog package `{}` repeats language `{language}`",
                self.id
            );
        }
        for requirement in &self.requirements {
            anyhow::ensure!(
                valid_command(&requirement.command),
                "catalog package `{}` has invalid required command `{}`",
                self.id,
                requirement.command
            );
            anyhow::ensure!(
                !requirement.purpose.trim().is_empty(),
                "catalog package `{}` has an empty requirement purpose",
                self.id
            );
        }
        anyhow::ensure!(
            !self.artifacts.is_empty(),
            "catalog package `{}` has no release artifacts",
            self.id
        );
        for (target, artifact) in &self.artifacts {
            anyhow::ensure!(
                !target.trim().is_empty(),
                "catalog artifact target is empty"
            );
            validate_release_url(&artifact.url)?;
            validate_digest(&artifact.sha256, "catalog artifact")?;
            anyhow::ensure!(
                artifact.size > 0,
                "catalog package `{}` has an empty artifact for `{target}`",
                self.id
            );
            for (language, digest) in &artifact.grammars {
                anyhow::ensure!(
                    languages.contains(language),
                    "catalog artifact for `{}` names unknown grammar `{language}`",
                    self.id
                );
                validate_digest(digest, "catalog grammar")?;
            }
        }
        Ok(())
    }
}

pub(crate) fn validate_catalog_url(url: &str) -> Result<()> {
    anyhow::ensure!(
        url.starts_with("https://raw.githubusercontent.com/")
            || url.starts_with("https://github.com/"),
        "plugin catalog must use a GitHub HTTPS URL"
    );
    Ok(())
}

fn validate_release_url(url: &str) -> Result<()> {
    anyhow::ensure!(
        url.starts_with("https://github.com/") || url.starts_with("https://api.github.com/"),
        "catalog artifacts must use a GitHub HTTPS URL"
    );
    Ok(())
}

fn validate_repository(repository: &str) -> Result<()> {
    let parts = repository.split('/').collect::<Vec<_>>();
    anyhow::ensure!(
        parts.len() == 2
            && parts.iter().all(|part| {
                !part.is_empty()
                    && part.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                    })
            }),
        "catalog repository must be `owner/repository`"
    );
    Ok(())
}

fn validate_relative_path(path: &std::path::Path) -> Result<()> {
    anyhow::ensure!(!path.as_os_str().is_empty(), "catalog source path is empty");
    anyhow::ensure!(!path.is_absolute(), "catalog source path must be relative");
    anyhow::ensure!(
        path.components()
            .all(|component| matches!(component, std::path::Component::Normal(_))),
        "catalog source path contains an unsafe component"
    );
    Ok(())
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

fn valid_command(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+'))
}

fn validate_digest(digest: &str, label: &str) -> Result<()> {
    anyhow::ensure!(
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{label} has an invalid SHA-256 digest"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_catalog() -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "packages": [{
                "id": "go-language",
                "name": "Go language support",
                "version": "0.1.0",
                "red_api": "^0.6.0",
                "description": "Go syntax and gopls integration",
                "repository": "codersauce/red-language-packs",
                "source_path": "packs/go",
                "resolved_commit": "0123456789abcdef0123456789abcdef01234567",
                "license": "MIT",
                "tier": "official",
                "languages": ["go"],
                "requirements": [{
                    "command": "gopls",
                    "purpose": "Language intelligence",
                    "optional": true
                }],
                "artifacts": {
                    "aarch64-apple-darwin": {
                        "url": "https://github.com/codersauce/red-language-packs/releases/download/go%2Fv0.1.0/go.tar.gz",
                        "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        "size": 123,
                        "grammars": {
                            "go": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                        }
                    }
                }
            }]
        })
    }

    #[test]
    fn parses_a_strict_catalog() {
        let bytes = serde_json::to_vec(&valid_catalog()).unwrap();
        let catalog = PluginCatalog::from_slice(&bytes).unwrap();

        assert_eq!(catalog.packages.len(), 1);
        assert_eq!(catalog.packages[0].id.as_str(), "go-language");
        assert_eq!(catalog.packages[0].tier.label(), "Official");
    }

    #[test]
    fn rejects_duplicates_and_unverified_artifacts() {
        let mut value = valid_catalog();
        let duplicate = value["packages"][0].clone();
        value["packages"].as_array_mut().unwrap().push(duplicate);
        assert!(
            PluginCatalog::from_slice(&serde_json::to_vec(&value).unwrap())
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );

        let mut value = valid_catalog();
        value["packages"][0]["artifacts"]["aarch64-apple-darwin"]["url"] =
            serde_json::json!("https://example.com/go.tar.gz");
        assert!(
            PluginCatalog::from_slice(&serde_json::to_vec(&value).unwrap())
                .unwrap_err()
                .to_string()
                .contains("GitHub HTTPS")
        );
    }

    #[test]
    fn filters_by_red_api_and_host_target() {
        let bytes = serde_json::to_vec(&valid_catalog()).unwrap();
        let catalog = PluginCatalog::from_slice(&bytes).unwrap();
        let id = PluginId::parse("go-language").unwrap();

        assert!(catalog.packages[0].supports_current_red_release().unwrap());
        assert!(catalog.installable(&id, "aarch64-apple-darwin").is_ok());
        assert!(catalog
            .installable(&id, "unsupported-target")
            .unwrap_err()
            .to_string()
            .contains("no release"));
    }
}
