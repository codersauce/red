//! User language definitions, native-grammar trust, and immutable grammar staging.
//!
//! Native grammars execute arbitrary process code when loaded. Approval is therefore
//! bound to both the canonical source path and its SHA-256 digest. A trusted grammar is
//! copied into an immutable digest-addressed cache before the dynamic loader sees it.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{
    config::{Config, ConfigDiagnosticSeverity, LoadedConfig},
    highlighter::LanguageRegistry,
    plugin::package::{PluginPackageManager, PluginPackageManifest},
};

const TRUST_STORE_FILENAME: &str = "trusted-grammars.json";
const MAX_NATIVE_GRAMMAR_BYTES: u64 = 64 * 1024 * 1024;

/// Returns the Rust target triple understood by portable language packages.
#[must_use]
pub fn host_target() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
        "aarch64-pc-windows-msvc"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else {
        "unsupported"
    }
}

/// Merges one enabled package's languages without replacing explicit user definitions.
pub fn merge_package_languages(
    config: &mut Config,
    manifest: &PluginPackageManifest,
    package_root: &Path,
) {
    for (id, definition) in &manifest.languages {
        if config.languages.contains_key(id) {
            continue;
        }
        let mut definition = definition.clone();
        let grammar_path = manifest.grammar_path(package_root, id, &definition);
        if let Some(grammar) = definition.grammar.as_mut() {
            grammar.path = grammar_path;
            grammar.targets.clear();
            // A package cannot approve executable code on the user's behalf.
            grammar.trusted = false;
            for path in &mut grammar.highlights {
                *path = package_root.join(&*path);
            }
            if let Some(path) = grammar.injections.as_mut() {
                *path = package_root.join(&*path);
            }
        }
        config.languages.insert(id.clone(), definition);
    }
}

/// Loads enabled package languages, quarantines invalid definitions, and expands LSP settings.
pub fn finalize_language_configuration(loaded: &mut LoadedConfig, config_dir: &Path) -> Result<()> {
    let explicit_servers = loaded.explicit_language_server_names();
    let explicit_comments = loaded.explicit_comment_language_names();
    let manager = PluginPackageManager::new(config_dir);
    for package in manager
        .list()?
        .into_iter()
        .filter(|package| package.enabled && package.compatible)
    {
        let manifest = PluginPackageManifest::load(&package.package_root)?;
        merge_package_languages(&mut loaded.config, &manifest, &package.package_root);
    }

    let mut accepted = std::collections::HashMap::new();
    let mut language_ids = loaded.config.languages.keys().cloned().collect::<Vec<_>>();
    language_ids.sort_unstable();
    for id in language_ids {
        let Some(definition) = loaded.config.languages.get(&id).cloned() else {
            continue;
        };
        let semantic_error = definition
            .comment
            .as_ref()
            .filter(|comment| comment.matches("%s").count() != 1)
            .map(|_| "comment must contain exactly one `%s` placeholder")
            .or_else(|| {
                definition
                    .indent_width
                    .filter(|width| *width == 0)
                    .map(|_| "indent_width must be positive")
            })
            .or_else(|| {
                definition.lsp.as_ref().and_then(|lsp| {
                    if lsp
                        .command
                        .as_ref()
                        .is_some_and(|command| command.trim().is_empty())
                    {
                        return Some("LSP command must not be empty");
                    }
                    let server = lsp.server.as_deref().unwrap_or(&id);
                    (lsp.command.is_none() && !loaded.config.lsp.servers.contains_key(server))
                        .then_some("LSP references an unknown server")
                })
            });
        if let Some(error) = semantic_error {
            loaded.add_runtime_diagnostic(
                "CFG401",
                ConfigDiagnosticSeverity::Error,
                &["languages".to_string(), id.clone()],
                error,
                "quarantined only the affected language",
            );
            continue;
        }

        accepted.insert(id.clone(), definition);
        if let Err(error) = LanguageRegistry::from_config(&accepted, config_dir) {
            accepted.remove(&id);
            loaded.add_runtime_diagnostic(
                "CFG401",
                ConfigDiagnosticSeverity::Error,
                &["languages".to_string(), id.clone()],
                format!("language definition could not be loaded: {error:#}"),
                "quarantined only the affected language",
            );
        }
    }
    loaded.config.languages = accepted;
    loaded
        .config
        .apply_language_definitions(&explicit_servers, &explicit_comments)
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrammarTrustData {
    #[serde(default)]
    grammars: BTreeMap<String, String>,
}

/// Durable, digest-bound approval authority for dynamically loaded grammars.
#[derive(Debug, Clone)]
pub struct GrammarTrustStore {
    config_dir: PathBuf,
}

impl GrammarTrustStore {
    /// Opens the approval store associated with one Red configuration directory.
    #[must_use]
    pub fn new(config_dir: impl Into<PathBuf>) -> Self {
        Self {
            config_dir: config_dir.into(),
        }
    }

    /// Explicitly approves the current bytes of one canonical native grammar.
    pub fn trust_path(&self, path: &Path) -> Result<String> {
        let (canonical, digest) = inspect_native_grammar(path)?;
        self.record_approval(&canonical, &digest)?;
        Ok(digest)
    }

    /// Approves a complete package grammar set in one durable trust-store update.
    pub(crate) fn trust_paths(&self, paths: &[PathBuf]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut trust = self.load()?;
        for path in paths {
            let (canonical, digest) = inspect_native_grammar(path)?;
            trust
                .grammars
                .insert(canonical.to_string_lossy().into_owned(), digest);
        }
        self.persist(&trust)
    }

    /// Revokes every digest approval associated with one canonical grammar path.
    pub fn revoke_path(&self, path: &Path) -> Result<()> {
        let canonical = path
            .canonicalize()
            .with_context(|| format!("failed to resolve native grammar {}", path.display()))?;
        let mut trust = self.load()?;
        trust.grammars.remove(canonical.to_string_lossy().as_ref());
        self.persist(&trust)
    }

    /// Returns an immutable approved copy, recording explicit configuration consent.
    pub(crate) fn approved_grammar_path(
        &self,
        path: &Path,
        explicitly_trusted: bool,
    ) -> Result<PathBuf> {
        let (canonical, digest) = inspect_native_grammar(path)?;
        let key = canonical.to_string_lossy().into_owned();
        let trust = self.load()?;
        match trust.grammars.get(&key) {
            Some(approved_digest) => anyhow::ensure!(
                approved_digest == &digest,
                "native grammar {} changed since its approval; run `red language trust {}` to approve its current contents",
                canonical.display(),
                canonical.display()
            ),
            None if explicitly_trusted => self.record_approval(&canonical, &digest)?,
            None => anyhow::bail!(
                "native grammar {} is not approved; run `red language trust {}` or set grammar.trusted = true explicitly",
                canonical.display(),
                canonical.display()
            ),
        }

        let extension = canonical
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("grammar");
        let directory = self.config_dir.join("grammar-cache");
        fs::create_dir_all(&directory)
            .with_context(|| format!("failed to create grammar cache {}", directory.display()))?;
        let staged = directory.join(format!("{digest}.{extension}"));
        if !staged.is_file() {
            let temporary = directory.join(format!(".{digest}.{}.tmp", std::process::id()));
            fs::copy(&canonical, &temporary).with_context(|| {
                format!("failed to stage native grammar {}", canonical.display())
            })?;
            let staged_bytes = fs::read(&temporary)?;
            anyhow::ensure!(
                format!("{:x}", Sha256::digest(&staged_bytes)) == digest,
                "native grammar changed while its approved bytes were staged"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&temporary, fs::Permissions::from_mode(0o500))?;
            }
            if let Err(error) = fs::rename(&temporary, &staged) {
                let _ = fs::remove_file(&temporary);
                if !staged.is_file() {
                    return Err(error).with_context(|| {
                        format!(
                            "failed to publish staged native grammar {}",
                            staged.display()
                        )
                    });
                }
            }
        }
        let (_, staged_digest) = inspect_native_grammar(&staged).with_context(|| {
            format!(
                "failed to verify cached native grammar {}",
                staged.display()
            )
        })?;
        anyhow::ensure!(
            staged_digest == digest,
            "cached native grammar {} does not match its approved digest",
            staged.display()
        );
        Ok(staged)
    }

    fn record_approval(&self, canonical: &Path, digest: &str) -> Result<()> {
        let mut trust = self.load()?;
        trust
            .grammars
            .insert(canonical.to_string_lossy().into_owned(), digest.to_string());
        self.persist(&trust)
    }

    fn load(&self) -> Result<GrammarTrustData> {
        let path = self.config_dir.join(TRUST_STORE_FILENAME);
        match fs::read(&path) {
            Ok(contents) => serde_json::from_slice(&contents)
                .with_context(|| format!("invalid native-grammar trust store {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(GrammarTrustData::default())
            }
            Err(error) => Err(error).with_context(|| {
                format!(
                    "failed to read native-grammar trust store {}",
                    path.display()
                )
            }),
        }
    }

    fn persist(&self, trust: &GrammarTrustData) -> Result<()> {
        fs::create_dir_all(&self.config_dir)?;
        let target = self.config_dir.join(TRUST_STORE_FILENAME);
        let mut temporary = tempfile::Builder::new()
            .prefix(&format!(".{TRUST_STORE_FILENAME}."))
            .tempfile_in(&self.config_dir)
            .with_context(|| {
                format!(
                    "failed to create native-grammar trust store replacement in {}",
                    self.config_dir.display()
                )
            })?;
        serde_json::to_writer_pretty(temporary.as_file_mut(), trust)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            temporary
                .as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        temporary.as_file().sync_all()?;
        temporary
            .persist(&target)
            .map(|_| ())
            .map_err(|error| error.error)
            .with_context(|| {
                format!(
                    "failed to update native-grammar trust store {}",
                    target.display()
                )
            })
    }
}

fn inspect_native_grammar(path: &Path) -> Result<(PathBuf, String)> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("failed to resolve native grammar {}", path.display()))?;
    let metadata = fs::metadata(&canonical)?;
    anyhow::ensure!(metadata.is_file(), "native grammar must be a regular file");
    anyhow::ensure!(
        metadata.len() <= MAX_NATIVE_GRAMMAR_BYTES,
        "native grammar exceeds the {}-byte safety limit",
        MAX_NATIVE_GRAMMAR_BYTES
    );
    let bytes = fs::read(&canonical)?;
    Ok((canonical, format!("{:x}", Sha256::digest(bytes))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_language_lsp_commands_are_quarantined_without_rejecting_valid_languages() {
        for command in ["", "   "] {
            let directory = tempfile::tempdir().unwrap();
            let source = format!(
                r#"
[languages.valid]
extensions = ["valid"]

[languages.invalid]
extensions = ["invalid"]

[languages.invalid.lsp]
command = "{command}"
"#
            );
            let mut loaded =
                Config::load_user_toml(&source, &directory.path().join("config.toml"), &[])
                    .unwrap();

            finalize_language_configuration(&mut loaded, directory.path()).unwrap();

            assert!(loaded.config.languages.contains_key("valid"));
            assert!(!loaded.config.languages.contains_key("invalid"));
            assert!(loaded.diagnostics.iter().any(|diagnostic| {
                diagnostic.path == "languages.invalid"
                    && diagnostic.message.contains("LSP command must not be empty")
            }));
        }
    }

    #[test]
    fn trust_is_bound_to_path_and_current_digest() {
        let directory = tempfile::tempdir().unwrap();
        let grammar = directory.path().join("example.so");
        fs::write(&grammar, b"original grammar").unwrap();
        let trust = GrammarTrustStore::new(directory.path().join("config"));

        assert!(trust.approved_grammar_path(&grammar, false).is_err());
        trust.trust_path(&grammar).unwrap();
        let staged = trust.approved_grammar_path(&grammar, false).unwrap();
        assert_eq!(fs::read(staged).unwrap(), b"original grammar");

        fs::write(&grammar, b"replaced grammar").unwrap();
        assert!(trust.approved_grammar_path(&grammar, false).is_err());
    }

    #[test]
    fn explicit_configuration_consent_persists_and_can_be_revoked() {
        let directory = tempfile::tempdir().unwrap();
        let grammar = directory.path().join("example.so");
        fs::write(&grammar, b"grammar").unwrap();
        let trust = GrammarTrustStore::new(directory.path().join("config"));

        trust.approved_grammar_path(&grammar, true).unwrap();
        trust.approved_grammar_path(&grammar, false).unwrap();
        trust.revoke_path(&grammar).unwrap();
        assert!(trust.approved_grammar_path(&grammar, false).is_err());
    }

    #[test]
    fn persistent_configuration_trust_does_not_approve_replaced_grammar() {
        let directory = tempfile::tempdir().unwrap();
        let grammar = directory.path().join("example.so");
        fs::write(&grammar, b"original grammar").unwrap();
        let trust = GrammarTrustStore::new(directory.path().join("config"));

        trust.approved_grammar_path(&grammar, true).unwrap();
        fs::write(&grammar, b"replaced grammar").unwrap();

        assert!(trust.approved_grammar_path(&grammar, true).is_err());
        assert!(trust.approved_grammar_path(&grammar, false).is_err());

        trust.trust_path(&grammar).unwrap();
        assert!(trust.approved_grammar_path(&grammar, true).is_ok());
    }

    #[test]
    fn cached_grammar_bytes_must_match_the_approved_digest() {
        let directory = tempfile::tempdir().unwrap();
        let grammar = directory.path().join("example.so");
        fs::write(&grammar, b"approved grammar").unwrap();
        let config_dir = directory.path().join("config");
        let trust = GrammarTrustStore::new(&config_dir);
        let digest = trust.trust_path(&grammar).unwrap();
        let cache_dir = config_dir.join("grammar-cache");
        fs::create_dir_all(&cache_dir).unwrap();
        let staged = cache_dir.join(format!("{digest}.so"));
        fs::write(&staged, b"unapproved grammar").unwrap();

        let error = trust.approved_grammar_path(&grammar, false).unwrap_err();

        assert!(error.to_string().contains("approved digest"));
        assert_eq!(fs::read(&staged).unwrap(), b"unapproved grammar");
    }

    #[test]
    fn trust_store_replaces_existing_approvals_and_revocations() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.so");
        let second = directory.path().join("second.so");
        fs::write(&first, b"first grammar").unwrap();
        fs::write(&second, b"second grammar").unwrap();
        let trust = GrammarTrustStore::new(directory.path().join("config"));

        trust.trust_path(&first).unwrap();
        trust.trust_path(&second).unwrap();
        assert!(trust.approved_grammar_path(&first, false).is_ok());
        assert!(trust.approved_grammar_path(&second, false).is_ok());

        trust.revoke_path(&first).unwrap();
        assert!(trust.approved_grammar_path(&first, false).is_err());
        assert!(trust.approved_grammar_path(&second, false).is_ok());

        fs::write(&second, b"renewed second grammar").unwrap();
        trust.trust_path(&second).unwrap();
        assert!(trust.approved_grammar_path(&second, false).is_ok());
    }

    #[test]
    fn package_grammar_approval_is_atomic_when_one_grammar_is_invalid() {
        let directory = tempfile::tempdir().unwrap();
        let valid = directory.path().join("valid.so");
        let oversized = directory.path().join("oversized.so");
        fs::write(&valid, b"valid grammar").unwrap();
        fs::File::create(&oversized)
            .unwrap()
            .set_len(MAX_NATIVE_GRAMMAR_BYTES + 1)
            .unwrap();
        let trust = GrammarTrustStore::new(directory.path().join("config"));

        let error = trust.trust_paths(&[valid.clone(), oversized]).unwrap_err();

        assert!(error.to_string().contains("safety limit"));
        assert!(trust.approved_grammar_path(&valid, false).is_err());
    }
}
