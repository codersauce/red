use std::{fs, path::Path};

use husk::{Engine, NativeModule, OwnedValue, PackageLimits, ResolvedPackage};
use tempfile::TempDir;

fn write_package(main: &str, modules: &[(&str, &str)]) -> TempDir {
    let directory = TempDir::new().unwrap();
    let source = directory.path().join("src");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        directory.path().join("Husk.toml"),
        r#"
            schema_version = 1

            [package]
            name = "package-test"
            version = "0.1.0"
            entry = "src/main.hk"
        "#,
    )
    .unwrap();
    fs::write(source.join("main.hk"), main).unwrap();
    for (path, contents) in modules {
        let path = source.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    directory
}

fn resolve(directory: &Path) -> ResolvedPackage {
    ResolvedPackage::open(directory.join("Husk.toml"), PackageLimits::default()).unwrap()
}

#[test]
fn package_compiles_once_and_dispatches_across_relative_and_absolute_modules() {
    let directory = write_package(
        r#"
            mod util;
            use crate::util::answer;

            fn main() -> i32 { answer() }
            fn direct() -> i32 { util::answer() }
            fn deep() -> i32 { crate::util::nested::value() }
        "#,
        &[
            (
                "util.hk",
                r#"
                    mod nested;

                    fn private_helper() -> i32 { 40 }
                    pub fn answer() -> i32 { private_helper() + 2 }
                    pub fn relative() -> i32 { nested::value() }
                "#,
            ),
            ("util/nested.hk", "pub fn value() -> i32 { 7 }"),
        ],
    );
    let package = resolve(directory.path());
    let engine = Engine::<()>::builder().build().unwrap();
    let compiled = engine.compile_package(&package).unwrap();

    assert_eq!(compiled.program().source_map().sources().len(), 3);
    assert_eq!(compiled.program().module_semantic_results().len(), 3);
    assert_eq!(compiled.program().source_modules().len(), 1);

    let mut instance = engine.instantiate(compiled, ()).unwrap();
    assert_eq!(instance.call("main", &[]).unwrap(), OwnedValue::I64(42));
    assert_eq!(instance.call("direct", &[]).unwrap(), OwnedValue::I64(42));
    assert_eq!(instance.call("deep", &[]).unwrap(), OwnedValue::I64(7));
    assert_eq!(
        instance.call("util::relative", &[]).unwrap(),
        OwnedValue::I64(7)
    );
}

#[test]
fn grouped_source_module_imports_resolve_and_dispatch_each_function() {
    let directory = write_package(
        "mod util;\nuse crate::util::{answer, offset};\nfn main() -> i32 { answer() + offset() }",
        &[(
            "util.hk",
            "pub fn answer() -> i32 { 40 }\npub fn offset() -> i32 { 2 }",
        )],
    );
    let package = resolve(directory.path());
    let engine = Engine::<()>::builder().build().unwrap();
    let compiled = engine.compile_package(&package).unwrap();
    let mut instance = engine.instantiate(compiled, ()).unwrap();

    assert_eq!(instance.call("main", &[]).unwrap(), OwnedValue::I64(42));
}

#[test]
fn private_functions_do_not_cross_source_module_boundaries() {
    let directory = write_package(
        "mod util;\nfn main() -> i32 { util::secret() }",
        &[(
            "util.hk",
            "fn secret() -> i32 { 42 }\npub fn visible() -> i32 { secret() }",
        )],
    );
    let package = resolve(directory.path());
    let engine = Engine::<()>::builder().build().unwrap();
    let error = engine.compile_package(&package).unwrap_err().to_string();

    assert!(error.contains("HUSK-T0001"), "{error}");
    assert!(error.contains("unknown"), "{error}");
    assert!(error.contains("main.hk:2:"), "{error}");
}

#[test]
fn runtime_diagnostics_point_to_the_failing_source_module() {
    let directory = write_package(
        "mod util;\nfn main() -> i32 { util::explode() }",
        &[("util.hk", "pub fn explode() -> i32 {\n    1 / 0\n}")],
    );
    let package = resolve(directory.path());
    let engine = Engine::<()>::builder().typecheck(false).build().unwrap();
    let compiled = engine.compile_package(&package).unwrap();
    let mut instance = engine.instantiate(compiled, ()).unwrap();
    let error = instance.call("main", &[]).unwrap_err().to_string();

    assert!(error.contains("integer division by zero"), "{error}");
    assert!(error.contains("util.hk:2:"), "{error}");
}

#[test]
fn source_and_registered_module_roots_cannot_conflict() {
    let directory = write_package(
        "mod util;\nfn main() -> i32 { util::answer() }",
        &[("util.hk", "pub fn answer() -> i32 { 42 }")],
    );
    let package = resolve(directory.path());
    let util = NativeModule::<()>::builder("util").build().unwrap();
    let engine = Engine::builder()
        .register_module(util)
        .unwrap()
        .build()
        .unwrap();
    let error = engine.compile_package(&package).unwrap_err().to_string();

    assert!(
        error.contains("conflicts with a registered external module"),
        "{error}"
    );
}

#[test]
fn public_struct_signatures_and_fields_cross_a_module_boundary() {
    let directory = write_package(
        "mod util;\nfn main() -> i32 {\n    let point = util::point();\n    point.x + point.y\n}",
        &[(
            "util.hk",
            r#"
                pub struct Point {
                    x: i32,
                    y: i32,
                }

                pub fn point() -> Point {
                    Point { x: 19, y: 23 }
                }
            "#,
        )],
    );
    let package = resolve(directory.path());
    let engine = Engine::<()>::builder().build().unwrap();
    let compiled = engine.compile_package(&package).unwrap();
    let mut instance = engine.instantiate(compiled, ()).unwrap();

    assert_eq!(instance.call("main", &[]).unwrap(), OwnedValue::I64(42));
}
