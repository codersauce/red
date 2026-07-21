use std::{
    fs,
    process::{Command, Output},
};

use tempfile::TempDir;

fn run_script(source: &str, arguments: &[&str]) -> Output {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("script.hk");
    fs::write(&path, source).unwrap();
    Command::new(env!("CARGO_BIN_EXE_husk"))
        .arg("run")
        .arg(path)
        .arg("--")
        .args(arguments)
        .output()
        .unwrap()
}

#[test]
fn check_accepts_a_valid_script() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("valid.hk");
    fs::write(&path, "fn main() {}").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_husk"))
        .arg("check")
        .arg(path)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
}

#[test]
fn help_is_a_successful_command() {
    let output = Command::new(env!("CARGO_BIN_EXE_husk"))
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("Compile and run Husk scripts")
    );
}

#[test]
fn new_creates_a_clone_ready_package() {
    let directory = TempDir::new().unwrap();
    let project = directory.path().join("hello");
    let created = Command::new(env!("CARGO_BIN_EXE_husk"))
        .arg("new")
        .arg("hello")
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(created.status.success(), "{created:?}");
    assert!(project.join("Husk.toml").is_file());
    assert!(project.join("Husk.lock").is_file());
    assert_eq!(
        fs::read_to_string(project.join(".gitignore")).unwrap(),
        "/.husk/\n"
    );

    let installed = Command::new(env!("CARGO_BIN_EXE_husk"))
        .args(["install", "--locked", "--offline", "--package"])
        .arg(&project)
        .output()
        .unwrap();
    assert!(installed.status.success(), "{installed:?}");
    assert!(project.join(".husk/extensions").is_dir());

    let run = Command::new(env!("CARGO_BIN_EXE_husk"))
        .args(["run", "--locked"])
        .arg(&project)
        .output()
        .unwrap();
    assert!(run.status.success(), "{run:?}");
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "Hello from Husk!\n");
}

#[test]
fn run_supports_unit_exit_codes_arguments_and_shebangs() {
    assert!(run_script("fn main() {}", &[]).status.success());

    let status = run_script("fn main() -> i32 { return 7; }", &[]).status;
    assert_eq!(status.code(), Some(7));

    let output = run_script(
        "#!/usr/bin/env husk\nfn main(args: [String]) -> i32 {\n\
         if args[0] == \"expected\" { return 0; }\n\
         return 9;\n\
         }",
        &["expected"],
    );
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn run_exposes_the_minimal_std_module() {
    let output = run_script(r#"fn main() { std::println("hello from Husk"); }"#, &[]);

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "hello from Husk\n"
    );
}

#[test]
fn compile_and_runtime_failures_return_one_with_source_diagnostics() {
    let compile = run_script("fn main( {", &[]);
    assert_eq!(compile.status.code(), Some(1));
    let stderr = String::from_utf8(compile.stderr).unwrap();
    assert!(stderr.contains("HUSK-P0001"), "{stderr}");
    assert!(stderr.contains("script.hk:1:"), "{stderr}");

    let runtime = run_script("fn main() { let value = 1 / 0; }", &[]);
    assert_eq!(runtime.status.code(), Some(1));
    let stderr = String::from_utf8(runtime.stderr).unwrap();
    assert!(stderr.contains("integer division by zero"), "{stderr}");
    assert!(stderr.contains("script.hk:1:"), "{stderr}");
}

#[test]
fn result_main_propagates_script_errors() {
    let success = run_script(r#"fn main() -> Result<(), String> { return Ok(()); }"#, &[]);
    assert!(success.status.success(), "{success:?}");

    let failure = run_script(
        r#"fn main() -> Result<(), String> { return Err("intentional"); }"#,
        &[],
    );
    assert_eq!(failure.status.code(), Some(1));
    assert!(
        String::from_utf8(failure.stderr)
            .unwrap()
            .contains("intentional")
    );
}

const MATH_COMPONENT: &str = r#"
    (component
        (core module $m
            (func (export "add") (param i32 i32) (result i32)
                local.get 0 local.get 1 i32.add))
        (core instance $i (instantiate $m))
        (func (export "add")
            (param "left" s32)
            (param "right" s32)
            (result s32)
            (canon lift (core func $i "add"))))
"#;

fn write_extension_source(directory: &TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let manifest = directory.path().join("math.toml");
    let component = directory.path().join("math.wasm");
    fs::write(
        &manifest,
        r#"
            schema_version = 1
            name = "math"
            version = "1.0.0"
            module = "math"
            artifact = "component.wasm"
            world = "example:math/husk-extension@1.0.0"
            minimum_husk = "0.1.0"

            [capabilities]
            requested = []
        "#,
    )
    .unwrap();
    fs::write(&component, MATH_COMPONENT).unwrap();
    (manifest, component)
}

#[test]
fn extension_pack_inspect_and_run_work_end_to_end() {
    let directory = TempDir::new().unwrap();
    let (manifest, component) = write_extension_source(&directory);
    let bundle = directory.path().join("math.huskext");
    let pack = Command::new(env!("CARGO_BIN_EXE_husk"))
        .args(["extension", "pack", "--manifest"])
        .arg(&manifest)
        .arg("--component")
        .arg(&component)
        .arg("--output")
        .arg(&bundle)
        .output()
        .unwrap();
    assert!(pack.status.success(), "{pack:?}");
    assert!(bundle.join("extension.toml").is_file());
    assert!(bundle.join("component.wasm").is_file());

    let inspect = Command::new(env!("CARGO_BIN_EXE_husk"))
        .args(["extension", "inspect"])
        .arg(&bundle)
        .output()
        .unwrap();
    assert!(inspect.status.success(), "{inspect:?}");
    let stdout = String::from_utf8(inspect.stdout).unwrap();
    assert!(stdout.contains("module: math 1.0.0"), "{stdout}");
    assert!(stdout.contains("export: math::add"), "{stdout}");
    assert!(stdout.contains("imports: none"), "{stdout}");

    let script = directory.path().join("script.hk");
    fs::write(&script, "fn main() -> i32 { math::add(20, 22) }").unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_husk"))
        .arg("run")
        .arg("--extension")
        .arg(&bundle)
        .arg(&script)
        .output()
        .unwrap();
    assert_eq!(run.status.code(), Some(42), "{run:?}");
}

fn write_package(directory: &TempDir) {
    fs::create_dir_all(directory.path().join("src/util")).unwrap();
    fs::write(
        directory.path().join("Husk.toml"),
        r#"
            schema_version = 1

            [package]
            name = "cli-package"
            version = "0.1.0"
            entry = "src/main.hk"
        "#,
    )
    .unwrap();
    fs::write(
        directory.path().join("src/main.hk"),
        "mod util;\nuse crate::util::answer;\nfn main() -> i32 { answer() }",
    )
    .unwrap();
    fs::write(
        directory.path().join("src/util.hk"),
        "mod nested;\npub fn answer() -> i32 { nested::value() }",
    )
    .unwrap();
    fs::write(
        directory.path().join("src/util/nested.hk"),
        "pub fn value() -> i32 { 42 }",
    )
    .unwrap();
}

#[test]
fn package_check_run_and_locked_mode_use_the_manifest_graph() {
    let directory = TempDir::new().unwrap();
    write_package(&directory);

    let check = Command::new(env!("CARGO_BIN_EXE_husk"))
        .arg("check")
        .arg(directory.path())
        .output()
        .unwrap();
    assert!(check.status.success(), "{check:?}");
    assert!(directory.path().join("Husk.lock").is_file());

    let locked = Command::new(env!("CARGO_BIN_EXE_husk"))
        .args(["check", "--locked"])
        .arg(directory.path().join("Husk.toml"))
        .output()
        .unwrap();
    assert!(locked.status.success(), "{locked:?}");

    let run = Command::new(env!("CARGO_BIN_EXE_husk"))
        .arg("run")
        .arg(directory.path())
        .output()
        .unwrap();
    assert_eq!(run.status.code(), Some(42), "{run:?}");
}

#[test]
fn locked_mode_rejects_missing_or_changed_package_lock() {
    let directory = TempDir::new().unwrap();
    write_package(&directory);

    let missing = Command::new(env!("CARGO_BIN_EXE_husk"))
        .args(["check", "--locked"])
        .arg(directory.path())
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(1), "{missing:?}");
    assert!(
        String::from_utf8(missing.stderr)
            .unwrap()
            .contains("Husk.lock")
    );

    let unlocked = Command::new(env!("CARGO_BIN_EXE_husk"))
        .arg("check")
        .arg(directory.path())
        .output()
        .unwrap();
    assert!(unlocked.status.success(), "{unlocked:?}");
    let manifest = directory.path().join("Husk.toml");
    let changed = fs::read_to_string(&manifest)
        .unwrap()
        .replace("version = \"0.1.0\"", "version = \"0.2.0\"");
    fs::write(manifest, changed).unwrap();

    let changed = Command::new(env!("CARGO_BIN_EXE_husk"))
        .args(["check", "--locked"])
        .arg(directory.path())
        .output()
        .unwrap();
    assert_eq!(changed.status.code(), Some(1), "{changed:?}");
    assert!(
        String::from_utf8(changed.stderr)
            .unwrap()
            .contains("regenerate the lock file")
    );
}

#[test]
fn add_locked_rejects_before_creating_install_state() {
    let directory = TempDir::new().unwrap();
    write_package(&directory);
    let initialize = Command::new(env!("CARGO_BIN_EXE_husk"))
        .arg("check")
        .arg(directory.path())
        .output()
        .unwrap();
    assert!(initialize.status.success(), "{initialize:?}");
    let original_manifest = fs::read(directory.path().join("Husk.toml")).unwrap();
    let original_lock = fs::read(directory.path().join("Husk.lock")).unwrap();

    let add = Command::new(env!("CARGO_BIN_EXE_husk"))
        .args(["add", "any-arbitrary-crate", "--locked", "--offline"])
        .arg("--package")
        .arg(directory.path())
        .output()
        .unwrap();

    assert_eq!(add.status.code(), Some(1), "{add:?}");
    let stderr = String::from_utf8_lossy(&add.stderr);
    assert!(stderr.contains("would change"), "{add:?}");
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
fn package_manifest_loads_declared_portable_extensions() {
    let directory = TempDir::new().unwrap();
    let (extension_manifest, component) = write_extension_source(&directory);
    let bundle = directory.path().join("vendor/math.huskext");
    fs::create_dir_all(bundle.parent().unwrap()).unwrap();
    let pack = Command::new(env!("CARGO_BIN_EXE_husk"))
        .args(["extension", "pack", "--manifest"])
        .arg(extension_manifest)
        .arg("--component")
        .arg(component)
        .arg("--output")
        .arg(&bundle)
        .output()
        .unwrap();
    assert!(pack.status.success(), "{pack:?}");

    fs::create_dir_all(directory.path().join("src")).unwrap();
    fs::write(
        directory.path().join("Husk.toml"),
        r#"
            schema_version = 1

            [package]
            name = "extension-package"
            version = "0.1.0"
            entry = "src/main.hk"

            [extensions.math]
            path = "vendor/math.huskext"
        "#,
    )
    .unwrap();
    fs::write(
        directory.path().join("src/main.hk"),
        "use math::add;\nfn main() -> i32 { add(20, 22) }",
    )
    .unwrap();

    let run = Command::new(env!("CARGO_BIN_EXE_husk"))
        .arg("run")
        .arg(directory.path())
        .output()
        .unwrap();
    assert_eq!(run.status.code(), Some(42), "{run:?}");
    let lock = fs::read_to_string(directory.path().join("Husk.lock")).unwrap();
    assert!(lock.contains("[extensions.math]"), "{lock}");
    assert!(lock.contains("sha256 = "), "{lock}");
}

#[test]
fn test_runs_attributes_filters_and_isolates_package_tests() {
    let directory = TempDir::new().unwrap();
    fs::create_dir_all(directory.path().join("src")).unwrap();
    fs::write(
        directory.path().join("Husk.toml"),
        r#"
            schema_version = 1

            [package]
            name = "tests"
            version = "0.1.0"
            entry = "src/main.hk"
        "#,
    )
    .unwrap();
    fs::write(
        directory.path().join("src/main.hk"),
        r#"
            mod nested;

            #[cfg(test)]
            fn helper() -> i32 { 42 }

            #[test]
            fn passes() {
                let answer = helper();
            }

            #[test]
            #[should_panic(expected = "division by zero")]
            fn expected_panic() {
                let failure = 1 / 0;
            }

            #[test]
            #[ignore]
            fn ignored_failure() {
                let failure = 1 / 0;
            }
        "#,
    )
    .unwrap();
    fs::write(
        directory.path().join("src/nested.hk"),
        "#[test]\npub fn nested_passes() {}",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_husk"))
        .arg("test")
        .arg(directory.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("test passes ... ok"), "{stdout}");
    assert!(stdout.contains("test expected_panic ... ok"), "{stdout}");
    assert!(
        stdout.contains("test ignored_failure ... ignored"),
        "{stdout}"
    );
    assert!(
        stdout.contains("test nested::nested_passes ... ok"),
        "{stdout}"
    );
    assert!(stdout.contains("3 passed; 0 failed; 1 ignored"), "{stdout}");

    let list = Command::new(env!("CARGO_BIN_EXE_husk"))
        .args(["test", "--list"])
        .arg(directory.path())
        .arg("nested")
        .output()
        .unwrap();
    assert!(list.status.success(), "{list:?}");
    assert_eq!(
        String::from_utf8(list.stdout).unwrap(),
        "nested::nested_passes\n"
    );
}

#[test]
fn test_returns_failure_when_a_test_fails() {
    let directory = TempDir::new().unwrap();
    let script = directory.path().join("failure.hk");
    fs::write(
        &script,
        "#[test]\nfn fails() {\n    let failure = 1 / 0;\n}",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_husk"))
        .arg("test")
        .arg(script)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("test fails ... FAILED"), "{stdout}");
    assert!(stdout.contains("integer division by zero"), "{stdout}");
    assert!(stdout.contains("0 passed; 1 failed; 0 ignored"), "{stdout}");
}
