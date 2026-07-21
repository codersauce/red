use std::{collections::BTreeSet, fs};

use husk_extension::{
    BundleLimits, Capability, CapabilityError, ExtensionBundle, pack_directory,
    validate_capabilities,
};
use tempfile::TempDir;

fn valid_bundle() -> TempDir {
    let directory = TempDir::new().unwrap();
    fs::write(
        directory.path().join("extension.toml"),
        r#"
            schema_version = 1
            name = "regex"
            version = "0.1.0"
            module = "regex"
            artifact = "component.wasm"
            world = "example:regex/husk-extension@0.1.0"
            minimum_husk = "0.1.0"

            [capabilities]
            requested = []
        "#,
    )
    .unwrap();
    fs::write(directory.path().join("component.wasm"), b"component").unwrap();
    directory
}

#[test]
fn validates_and_hashes_a_bounded_directory_bundle() {
    let directory = valid_bundle();
    let bundle = ExtensionBundle::open(directory.path(), BundleLimits::default()).unwrap();

    assert_eq!(bundle.module().as_str(), "regex");
    assert_eq!(bundle.manifest().version.to_string(), "0.1.0");
    assert_eq!(bundle.component(), b"component");
    assert_eq!(bundle.digest().to_string().len(), 64);
    assert!(bundle.component_path().starts_with(bundle.root()));
}

#[test]
fn rejects_unknown_manifest_fields_and_unsupported_schemas() {
    let directory = valid_bundle();
    fs::write(
        directory.path().join("extension.toml"),
        r#"
            schema_version = 2
            name = "regex"
            version = "0.1.0"
            module = "regex"
            artifact = "component.wasm"
            world = "world"
            minimum_husk = "0.1.0"
            surprise = true
        "#,
    )
    .unwrap();
    let error = ExtensionBundle::open(directory.path(), BundleLimits::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("unknown field `surprise`"), "{error}");

    let directory = valid_bundle();
    let manifest = fs::read_to_string(directory.path().join("extension.toml"))
        .unwrap()
        .replace("schema_version = 1", "schema_version = 2");
    fs::write(directory.path().join("extension.toml"), manifest).unwrap();
    let error = ExtensionBundle::open(directory.path(), BundleLimits::default())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("unsupported extension schema version 2"),
        "{error}"
    );
}

#[test]
fn rejects_oversized_components_and_path_escape() {
    let directory = valid_bundle();
    let error = ExtensionBundle::open(
        directory.path(),
        BundleLimits {
            max_component_bytes: 2,
            ..BundleLimits::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("maximum is 2"), "{error}");

    let parent = TempDir::new().unwrap();
    let bundle = parent.path().join("escape.huskext");
    fs::create_dir(&bundle).unwrap();
    fs::write(parent.path().join("outside.wasm"), b"outside").unwrap();
    fs::write(
        bundle.join("extension.toml"),
        r#"
            schema_version = 1
            name = "escape"
            version = "0.1.0"
            module = "escape"
            artifact = "../outside.wasm"
            world = "example:escape/world@0.1.0"
            minimum_husk = "0.1.0"
        "#,
    )
    .unwrap();
    let error = ExtensionBundle::open(&bundle, BundleLimits::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid extension artifact path"), "{error}");
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_control_files_and_artifacts() {
    use std::os::unix::fs::symlink;

    let parent = TempDir::new().unwrap();
    let real = valid_bundle();
    let linked_root = parent.path().join("linked.huskext");
    symlink(real.path(), &linked_root).unwrap();
    let error = ExtensionBundle::open(&linked_root, BundleLimits::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("must not be a symlink"), "{error}");

    let directory = valid_bundle();
    let component = directory.path().join("component.wasm");
    fs::rename(&component, directory.path().join("real.wasm")).unwrap();
    symlink("real.wasm", &component).unwrap();
    let error = ExtensionBundle::open(directory.path(), BundleLimits::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("must not be a symlink"), "{error}");
}

#[test]
fn capability_comparison_fails_closed_in_both_directions() {
    let capability = |name| Capability::new(name).unwrap();
    let actual = BTreeSet::from([capability("filesystem.read")]);
    let requested = BTreeSet::new();
    let granted = BTreeSet::new();
    assert!(matches!(
        validate_capabilities(&actual, &requested, &granted),
        Err(CapabilityError::UndeclaredImports(_))
    ));

    let requested = actual.clone();
    assert!(matches!(
        validate_capabilities(&actual, &requested, &granted),
        Err(CapabilityError::NotGranted(_))
    ));

    let granted = requested.clone();
    validate_capabilities(&actual, &requested, &granted).unwrap();
}

#[test]
fn packs_to_a_new_validated_directory_and_refuses_overwrite() {
    let source = valid_bundle();
    let output_parent = TempDir::new().unwrap();
    let output = output_parent.path().join("regex.huskext");
    let bundle = pack_directory(
        source.path().join("extension.toml"),
        source.path().join("component.wasm"),
        &output,
        BundleLimits::default(),
    )
    .unwrap();
    assert_eq!(bundle.component(), b"component");
    assert!(output.join("extension.toml").is_file());
    assert!(output.join("component.wasm").is_file());

    let error = pack_directory(
        source.path().join("extension.toml"),
        source.path().join("component.wasm"),
        &output,
        BundleLimits::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("File exists"), "{error}");
    assert!(output.join("component.wasm").is_file());
}
