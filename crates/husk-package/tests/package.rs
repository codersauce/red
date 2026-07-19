use std::fs;

use husk_package::{LOCK_FILE, PackageError, PackageLimits, ResolvedPackage, discover_manifest};
use tempfile::TempDir;

fn package(manifest_extensions: &str) -> TempDir {
    let directory = TempDir::new().unwrap();
    fs::create_dir_all(directory.path().join("src")).unwrap();
    fs::write(
        directory.path().join("Husk.toml"),
        format!(
            r#"
                schema_version = 1

                [package]
                name = "example"
                version = "0.1.0"
                entry = "src/main.hk"

                {manifest_extensions}
            "#
        ),
    )
    .unwrap();
    directory
}

#[test]
fn resolves_flat_and_nested_modules_in_stable_order() {
    let directory = package("");
    fs::write(
        directory.path().join("src/main.hk"),
        "mod util;\nfn main() -> i32 { util::answer() }",
    )
    .unwrap();
    fs::write(
        directory.path().join("src/util.hk"),
        "mod nested;\npub fn answer() -> i32 { nested::value() }",
    )
    .unwrap();
    fs::create_dir(directory.path().join("src/util")).unwrap();
    fs::create_dir(directory.path().join("src/util/nested")).unwrap();
    fs::write(
        directory.path().join("src/util/nested/mod.hk"),
        "pub fn value() -> i32 { 42 }",
    )
    .unwrap();

    let resolved =
        ResolvedPackage::open(directory.path().join("Husk.toml"), PackageLimits::default())
            .unwrap();
    assert_eq!(
        resolved
            .modules
            .iter()
            .map(|module| module.module_path.join("::"))
            .collect::<Vec<_>>(),
        vec!["", "util", "util::nested"]
    );
    assert_eq!(
        discover_manifest(directory.path().join("src/util/nested/mod.hk")).unwrap(),
        directory.path().join("Husk.toml").canonicalize().unwrap()
    );
}

#[test]
fn rejects_missing_and_ambiguous_module_files() {
    let directory = package("");
    fs::write(directory.path().join("src/main.hk"), "mod missing;").unwrap();
    let error = ResolvedPackage::open(directory.path().join("Husk.toml"), PackageLimits::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("expected exactly one"), "{error}");

    let directory = package("");
    fs::write(directory.path().join("src/main.hk"), "mod util;").unwrap();
    fs::write(directory.path().join("src/util.hk"), "pub fn value() {}").unwrap();
    fs::create_dir(directory.path().join("src/util")).unwrap();
    fs::write(
        directory.path().join("src/util/mod.hk"),
        "pub fn value() {}",
    )
    .unwrap();
    let error = ResolvedPackage::open(directory.path().join("Husk.toml"), PackageLimits::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("ambiguous"), "{error}");
}

#[test]
fn creates_and_enforces_a_deterministic_local_extension_lock() {
    let directory = package(
        r#"
            [extensions.regex]
            path = "vendor/regex.huskext"
        "#,
    );
    fs::write(directory.path().join("src/main.hk"), "fn main() {}").unwrap();
    let bundle = directory.path().join("vendor/regex.huskext");
    fs::create_dir_all(&bundle).unwrap();
    fs::write(
        bundle.join("extension.toml"),
        r#"
            schema_version = 1
            name = "regex"
            version = "1.2.3"
            module = "regex"
            artifact = "component.wasm"
            world = "example:regex/husk-extension@1.2.3"
            minimum_husk = "0.1.0"
        "#,
    )
    .unwrap();
    fs::write(bundle.join("component.wasm"), b"component-v1").unwrap();

    let resolved =
        ResolvedPackage::open(directory.path().join("Husk.toml"), PackageLimits::default())
            .unwrap();
    resolved.write_lock().unwrap();
    let first = fs::read_to_string(directory.path().join(LOCK_FILE)).unwrap();
    resolved.enforce_lock().unwrap();
    resolved.write_lock().unwrap();
    assert_eq!(
        first,
        fs::read_to_string(directory.path().join(LOCK_FILE)).unwrap()
    );

    fs::write(bundle.join("component.wasm"), b"component-v2").unwrap();
    let changed =
        ResolvedPackage::open(directory.path().join("Husk.toml"), PackageLimits::default())
            .unwrap();
    assert!(matches!(
        changed.enforce_lock(),
        Err(PackageError::LockChanged { .. })
    ));
}

#[test]
fn enforces_source_and_module_limits() {
    let directory = package("");
    fs::write(directory.path().join("src/main.hk"), "fn main() {}").unwrap();
    let error = ResolvedPackage::open(
        directory.path().join("Husk.toml"),
        PackageLimits {
            max_source_bytes: 2,
            ..PackageLimits::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("maximum is 2"), "{error}");
}

#[cfg(unix)]
#[test]
fn canonical_duplicate_or_cycle_through_symlink_fails_closed() {
    use std::os::unix::fs::symlink;

    let directory = package("");
    fs::write(directory.path().join("src/main.hk"), "fn main() {}").unwrap();
    let manifest = directory.path().join("Husk.toml");
    let real_manifest = directory.path().join("manifest.toml");
    fs::rename(&manifest, &real_manifest).unwrap();
    symlink("manifest.toml", &manifest).unwrap();
    let error = ResolvedPackage::open(&manifest, PackageLimits::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("must not be a symlink"), "{error}");

    let directory = package("");
    fs::write(directory.path().join("src/main.hk"), "mod looped;").unwrap();
    symlink("main.hk", directory.path().join("src/looped.hk")).unwrap();
    let error = ResolvedPackage::open(directory.path().join("Husk.toml"), PackageLimits::default())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("must not be a symlink") || error.contains("cycle"),
        "{error}"
    );
}

#[test]
fn lock_reads_use_the_package_specific_limit() {
    let directory = package("");
    fs::write(directory.path().join("src/main.hk"), "fn main() {}").unwrap();
    let resolved = ResolvedPackage::open(
        directory.path().join("Husk.toml"),
        PackageLimits {
            max_lock_bytes: 4,
            ..PackageLimits::default()
        },
    )
    .unwrap();
    fs::write(directory.path().join(LOCK_FILE), "more than four bytes").unwrap();

    let error = resolved.enforce_lock().unwrap_err().to_string();
    assert!(error.contains("maximum is 4"), "{error}");
}
