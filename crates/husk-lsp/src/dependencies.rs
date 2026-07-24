//! Read-only indexing for locked Husk extension adapters.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use husk_extension::{BundleLimits, ExtensionBundle};
use husk_package::{LOCK_FILE, PackageLock, PackageManifest, discover_manifest};
use husk_runtime::{WasmCompileOptions, WasmComponent, module_declaration_source};
use husk_types::ModuleDescriptor;
use sha2::{Digest, Sha256};

pub(crate) struct DependencyIndex {
    pub(crate) modules: Vec<ModuleDescriptor>,
    pub(crate) stubs: Vec<(PathBuf, String)>,
    pub(crate) diagnostics: Vec<String>,
}

pub(crate) fn index_dependencies(root: &Path) -> DependencyIndex {
    let mut index = DependencyIndex {
        modules: Vec::new(),
        stubs: Vec::new(),
        diagnostics: Vec::new(),
    };
    if let Err(error) = index_dependencies_inner(root, &mut index) {
        index
            .diagnostics
            .push(format!("could not index Husk extensions: {error:#}"));
    }
    index
}

fn index_dependencies_inner(root: &Path, index: &mut DependencyIndex) -> anyhow::Result<()> {
    let Ok(manifest_path) = discover_manifest(root) else {
        return Ok(());
    };
    let package_root = manifest_path
        .parent()
        .context("Husk.toml has no parent directory")?;
    let manifest_source = fs::read_to_string(&manifest_path)
        .with_context(|| format!("read `{}`", manifest_path.display()))?;
    let manifest = PackageManifest::parse(&manifest_source)?;
    if manifest.extensions.is_empty() {
        return Ok(());
    }
    let lock_path = package_root.join(LOCK_FILE);
    let lock_source = fs::read_to_string(&lock_path).with_context(|| {
        format!(
            "read `{}`; run `red husk install --locked --offline`",
            lock_path.display()
        )
    })?;
    let lock = PackageLock::parse(&lock_source)?;
    lock.validate_manifest(&manifest)?;

    for (manifest_name, locked) in &lock.extensions {
        let installed = package_root.join(&locked.source);
        let artifact = locked
            .artifact
            .as_ref()
            .map(|artifact| package_root.join(artifact));
        let bundle_path = if installed.is_dir() {
            installed
        } else if artifact.as_ref().is_some_and(|artifact| artifact.is_dir()) {
            artifact.expect("artifact existence was checked")
        } else {
            index.diagnostics.push(format!(
                "extension `{manifest_name}` is not installed or vendored; run `red husk install --locked --offline`"
            ));
            continue;
        };
        let bundle = match ExtensionBundle::open(&bundle_path, BundleLimits::default()) {
            Ok(bundle) => bundle,
            Err(error) => {
                index.diagnostics.push(format!(
                    "extension `{manifest_name}` at `{}` is invalid: {error}",
                    bundle_path.display()
                ));
                continue;
            }
        };
        let digest = bundle.digest().to_string();
        if digest != locked.sha256 {
            index.diagnostics.push(format!(
                "extension `{manifest_name}` digest is {digest}, expected {}",
                locked.sha256
            ));
            continue;
        }
        let report_path = bundle.root().join("husk-adapter.json");
        let report = match fs::read(&report_path) {
            Ok(report)
                if locked
                    .report_sha256
                    .as_ref()
                    .is_none_or(|expected| hex_digest(&report) == *expected) =>
            {
                Some(report)
            }
            Ok(_) => {
                index.diagnostics.push(format!(
                    "extension `{manifest_name}` adapter report digest does not match Husk.lock"
                ));
                continue;
            }
            Err(error) if locked.report_sha256.is_some() => {
                index.diagnostics.push(format!(
                    "extension `{manifest_name}` adapter report is unavailable: {error}"
                ));
                continue;
            }
            Err(_) => None,
        };
        let component = match WasmComponent::from_bundle(&bundle, WasmCompileOptions::default()) {
            Ok(component) => component,
            Err(error) => {
                index.diagnostics.push(format!(
                    "extension `{manifest_name}` cannot be inspected: {error:#}"
                ));
                continue;
            }
        };
        let mut descriptor = component.descriptor().clone();
        if let Some(report) = report
            && let Err(error) = enrich_documentation(&mut descriptor, &report)
        {
            index.diagnostics.push(format!(
                "extension `{manifest_name}` adapter documentation is invalid: {error}"
            ));
        }
        let declaration = module_declaration_source(&descriptor)?;
        let stub_path = dependency_stub_path(package_root, &digest, descriptor.name.as_str());
        write_stub(package_root, &stub_path, &declaration)?;
        index.stubs.push((stub_path, declaration));
        index.modules.push(descriptor);
    }
    index
        .modules
        .sort_by(|left, right| left.name.cmp(&right.name));
    index.stubs.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(())
}

fn dependency_stub_path(root: &Path, digest: &str, module: &str) -> PathBuf {
    root.join(".husk")
        .join("lsp")
        .join(digest)
        .join(format!("{module}.hk"))
}

fn write_stub(root: &Path, path: &Path, source: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        path.starts_with(root),
        "dependency stub `{}` escapes package root `{}`",
        path.display(),
        root.display()
    );
    let parent = path
        .parent()
        .context("dependency stub path has no parent directory")?;
    let mut current = root.to_path_buf();
    for component in parent
        .strip_prefix(root)
        .context("dependency stub parent escapes package root")?
        .components()
    {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                anyhow::bail!(
                    "dependency stub directory `{}` is a symlink",
                    current.display()
                );
            }
            Ok(metadata) => anyhow::ensure!(
                metadata.is_dir(),
                "dependency stub parent `{}` is not a directory",
                current.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current).with_context(|| {
                    format!("create dependency stub directory `{}`", current.display())
                })?;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("inspect dependency stub directory `{}`", current.display())
                });
            }
        }
    }
    if path.is_symlink() {
        anyhow::bail!("dependency stub `{}` is a symlink", path.display());
    }
    if path.is_file() && fs::read_to_string(path).is_ok_and(|existing| existing == source) {
        return Ok(());
    }
    fs::write(path, source).with_context(|| format!("write dependency stub `{}`", path.display()))
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn enrich_documentation(descriptor: &mut ModuleDescriptor, report: &[u8]) -> anyhow::Result<()> {
    let report: serde_json::Value =
        serde_json::from_slice(report).context("parse adapter report")?;
    let items = report
        .get("items")
        .or_else(|| report.pointer("/public_api/items"))
        .and_then(serde_json::Value::as_array)
        .context("adapter report omitted its selected items")?;
    let mut documentation = BTreeMap::<String, Option<String>>::new();
    for item in items {
        let Some(docs) = item
            .get("documentation")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|docs| !docs.is_empty())
        else {
            continue;
        };
        let Some(wit) = item.get("wit") else {
            continue;
        };
        let Some(declaration) = wit.get("declaration").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let member = declaration
            .split_once(':')
            .map_or_else(|| declaration.trim(), |(name, _)| name.trim());
        let owner = wit
            .get("owner_resource")
            .and_then(serde_json::Value::as_str);
        let raw_name = if member == "constructor" {
            let Some(owner) = owner else {
                continue;
            };
            format!("{owner}-new")
        } else if let Some(owner) = owner {
            format!("{owner}-{member}")
        } else {
            member.to_string()
        };
        let Ok(name) = husk_wasm::normalize_wit_name(&raw_name) else {
            continue;
        };
        documentation
            .entry(name)
            .and_modify(|existing| *existing = None)
            .or_insert_with(|| Some(docs.to_string()));
    }
    for function in descriptor.functions.iter_mut().chain(
        descriptor
            .interfaces
            .iter_mut()
            .flat_map(|interface| &mut interface.functions),
    ) {
        if let Some(Some(docs)) = documentation.get(&function.name) {
            function.documentation = Some(docs.clone());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use husk_package::{
        LockedCrateExtension, LockedExtension, PackageLock, installed_extension_path,
        vendored_extension_path,
    };
    use husk_types::{FunctionDescriptor, TypeDescriptor, Version};
    use serde_json::json;

    use super::*;

    const RESOURCE_COMPONENT: &str = include_str!("../../husk-wasm/tests/fixtures/resource.wat");

    #[test]
    fn stub_paths_are_digest_scoped_and_workspace_local() {
        let root = Path::new("/workspace");
        assert_eq!(
            dependency_stub_path(root, "abc123", "serde_json"),
            root.join(".husk/lsp/abc123/serde_json.hk")
        );
    }

    #[test]
    fn indexes_a_locked_crate_adapter_into_a_typed_navigation_stub() {
        let package = tempfile::tempdir().expect("create package fixture");
        let root = package.path();
        fs::create_dir(root.join("src")).expect("create source directory");
        fs::write(root.join("src/main.hk"), "fn main() {}\n").expect("write package source");
        fs::write(
            root.join("Husk.toml"),
            "schema_version = 1\n\
             [package]\n\
             name = \"fixture\"\n\
             version = \"0.1.0\"\n\
             entry = \"src/main.hk\"\n\n\
             [extensions.resource-fixture]\n\
             crate = \"resource-fixture\"\n\
             version = \"=1.0.0\"\n",
        )
        .expect("write package manifest");

        let digest = hex_digest(RESOURCE_COMPONENT.as_bytes());
        let source = installed_extension_path(&digest);
        let bundle = root.join(&source);
        fs::create_dir_all(&bundle).expect("create installed extension");
        fs::write(
            bundle.join("extension.toml"),
            "schema_version = 1\n\
             name = \"resource-fixture\"\n\
             version = \"1.0.0\"\n\
             module = \"resource_fixture\"\n\
             artifact = \"component.wasm\"\n\
             world = \"fixture:resource/factory\"\n\
             minimum_husk = \"0.1.0\"\n",
        )
        .expect("write extension manifest");
        fs::write(bundle.join("component.wasm"), RESOURCE_COMPONENT)
            .expect("write component fixture");

        let mut lock = PackageLock::empty("fixture".to_string(), Version::new(0, 1, 0));
        lock.extensions = BTreeMap::from([(
            "resource-fixture".to_string(),
            LockedExtension {
                module: "resource_fixture".to_string(),
                version: Version::new(1, 0, 0),
                source,
                sha256: digest.clone(),
                crate_source: Some(LockedCrateExtension {
                    package: "resource-fixture".to_string(),
                    version: Version::new(1, 0, 0),
                    requirement: "=1.0.0".to_string(),
                    features: Vec::new(),
                    default_features: true,
                    include: Vec::new(),
                    specializations: Vec::new(),
                    checksum: Some("fixture-checksum".to_string()),
                }),
                artifact: Some(vendored_extension_path(&digest)),
                report_sha256: None,
            },
        )]);
        fs::write(
            root.join("Husk.lock"),
            lock.to_toml().expect("serialize package lock"),
        )
        .expect("write package lock");

        let index = index_dependencies(root);

        assert!(index.diagnostics.is_empty(), "{:?}", index.diagnostics);
        assert_eq!(index.modules.len(), 1);
        assert_eq!(index.modules[0].name.as_str(), "resource_fixture");
        assert_eq!(index.stubs.len(), 1);
        assert!(
            index.stubs[0].0.starts_with(
                root.canonicalize()
                    .expect("canonical package")
                    .join(".husk/lsp")
            )
        );
        assert!(index.stubs[0].1.contains("mod global resource_fixture"));
        assert!(index.stubs[0].1.contains("fn new_item"));
    }

    #[test]
    fn adapter_report_documentation_is_preserved_in_generated_stubs() {
        let mut descriptor = ModuleDescriptor::new(
            "sample",
            Version::new(1, 0, 0),
            vec![
                FunctionDescriptor::new("parse_value", Vec::new(), TypeDescriptor::String)
                    .expect("create function descriptor"),
            ],
            Vec::new(),
        )
        .expect("create module descriptor");
        let report = serde_json::to_vec(&json!({
            "items": [{
                "documentation": "Parse one value.\n\nReturns owned data.",
                "wit": {
                    "owner_resource": null,
                    "declaration": "parse-value: func() -> string;"
                }
            }]
        }))
        .expect("serialize adapter report");

        enrich_documentation(&mut descriptor, &report).expect("enrich descriptor");
        let declaration = module_declaration_source(&descriptor).expect("render declaration");

        assert!(declaration.contains("/// Parse one value."));
        assert!(declaration.contains("/// Returns owned data."));
        assert!(declaration.contains("fn parse_value"));
    }

    #[cfg(unix)]
    #[test]
    fn generated_stub_rejects_a_symlinked_cache_directory() {
        use std::os::unix::fs::symlink;

        let package = tempfile::tempdir().expect("create package");
        let outside = tempfile::tempdir().expect("create outside directory");
        symlink(outside.path(), package.path().join(".husk")).expect("create cache symlink");
        let stub = package.path().join(".husk/lsp/digest/module.hk");

        let error = write_stub(package.path(), &stub, "fn hidden() {}")
            .expect_err("symlink must be rejected");

        assert!(error.to_string().contains("symlink"), "{error:#}");
        assert!(!outside.path().join("lsp/digest/module.hk").exists());
    }
}
