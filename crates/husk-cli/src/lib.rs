use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    ffi::OsString,
    fmt::Write as _,
    fs,
    io::{self, BufRead, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, ExitCode},
    time::Duration,
};

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use husk::{
    CallContext, CompiledModule, Engine, MainArguments, MainResult, NativeError, NativeModule,
    OwnedValue, PackageLimits, ReplOutcome, ResolvedPackage, TestExpectation, Version,
    WasmCompileOptions, WasmComponent,
};
use husk_extension::{BundleLimits, ExtensionBundle, pack_directory};
use serde::{Deserialize, Serialize};

const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_RUSTDOC_JSON_BYTES: u64 = 256 * 1024 * 1024;
const MAX_COMPONENT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Parser)]
#[command(name = "husk", version, about = "Compile and run Husk scripts")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start an interactive Husk session.
    Repl {
        /// Pure portable extension bundle to expose in the session.
        #[arg(long = "extension", value_name = "BUNDLE")]
        extensions: Vec<PathBuf>,
    },
    /// Parse and type-check a Husk script.
    Check {
        /// Portable extension bundle to expose while checking.
        #[arg(long = "extension", value_name = "BUNDLE")]
        extensions: Vec<PathBuf>,
        /// Require an existing, up-to-date Husk.lock for package input.
        #[arg(long)]
        locked: bool,
        path: PathBuf,
    },
    /// Run a Husk script's main function.
    Run {
        /// Pure portable extension bundle to load. Version 1 grants no imports.
        #[arg(long = "extension", value_name = "BUNDLE")]
        extensions: Vec<PathBuf>,
        /// Require an existing, up-to-date Husk.lock for package input.
        #[arg(long)]
        locked: bool,
        path: PathBuf,
        /// Arguments passed to `main`, after `--`.
        #[arg(last = true)]
        arguments: Vec<String>,
    },
    /// Compile and run functions marked with `#[test]`.
    Test {
        /// Portable extension bundle to expose while testing.
        #[arg(long = "extension", value_name = "BUNDLE")]
        extensions: Vec<PathBuf>,
        /// Require an existing, up-to-date Husk.lock for package input.
        #[arg(long)]
        locked: bool,
        /// Run tests marked with `#[ignore]`.
        #[arg(long)]
        include_ignored: bool,
        /// Print matching test names without executing them.
        #[arg(long)]
        list: bool,
        path: PathBuf,
        /// Optional substring used to select tests.
        filter: Option<String>,
    },
    /// Inspect or assemble portable extension bundles.
    Extension {
        #[command(subcommand)]
        command: ExtensionCommand,
    },
    /// Analyze a Rust crate before attempting adapter generation.
    Crate {
        #[command(subcommand)]
        command: CrateCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ExtensionCommand {
    /// Validate, compile, and print a bundle's derived module signature.
    Inspect { bundle: PathBuf },
    /// Turn a WIT-aware core Wasm module into a verified component.
    Componentize {
        #[arg(long)]
        core_module: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Assemble a new directory bundle without invoking Cargo.
    Pack {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        component: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum CrateCommand {
    /// Resolve a crate and report whether it can proceed to API analysis.
    Inspect {
        #[command(flatten)]
        request: CrateRequest,
        /// Emit the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Generate a reviewable WIT proposal for selected compatible items.
    Interface {
        #[command(flatten)]
        request: CrateRequest,
        /// Exact public API path to include. Repeat for each selected item.
        #[arg(long = "include", required = true)]
        includes: Vec<String>,
    },
    /// Generate a deterministic Rust adapter crate without building it.
    Adapter {
        #[command(flatten)]
        request: CrateRequest,
        /// Exact public API path to include. Repeat for each selected item.
        #[arg(long = "include", required = true)]
        includes: Vec<String>,
        /// New directory that will receive the generated adapter crate.
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Debug, Args)]
struct CrateRequest {
    crate_name: String,
    /// Cargo version requirement. Defaults to `*`.
    #[arg(long)]
    version: Option<String>,
    /// Cargo features to enable.
    #[arg(long, value_delimiter = ',')]
    features: Vec<String>,
    /// Disable the crate's default feature set.
    #[arg(long)]
    no_default_features: bool,
    /// Inspect a local crate directory instead of crates.io.
    #[arg(long)]
    path: Option<PathBuf>,
    /// Require all Cargo metadata to be available locally.
    #[arg(long)]
    offline: bool,
}

#[derive(Default)]
struct CliState;

/// Run the Husk command line using the current process arguments.
pub fn run() -> ExitCode {
    run_from(std::env::args_os())
}

/// Run the Husk command line with an explicit argument vector.
///
/// The first value is the display name used by clap, allowing Red to forward
/// `red husk ...` without spawning a second process.
pub fn run_from(arguments: impl IntoIterator<Item = OsString>) -> ExitCode {
    let cli = match Cli::try_parse_from(arguments) {
        Ok(cli) => cli,
        Err(error) => {
            let status = error.exit_code();
            let _ = error.print();
            return ExitCode::from(u8::try_from(status).unwrap_or(1));
        }
    };

    match execute(cli) {
        Ok(status) => status,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

fn execute(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        Command::Repl { extensions } => {
            let engine = cli_engine(&extensions, None, false)?;
            let interactive = io::stdin().is_terminal();
            let stdin = io::stdin();
            let stdout = io::stdout();
            run_repl(&engine, stdin.lock(), stdout.lock(), interactive)
        }
        Command::Check {
            extensions,
            locked,
            path,
        } => {
            let input = resolve_input(&path, locked)?;
            let engine = cli_engine(&extensions, input.package(), false)?;
            compile_input(&engine, &input)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Run {
            extensions,
            locked,
            path,
            arguments,
        } => {
            let input = resolve_input(&path, locked)?;
            let engine = cli_engine(&extensions, input.package(), false)?;
            let compiled = compile_input(&engine, &input)?;
            run_compiled(&engine, compiled, arguments)
        }
        Command::Test {
            extensions,
            locked,
            include_ignored,
            list,
            path,
            filter,
        } => {
            let input = resolve_input(&path, locked)?;
            let engine = cli_engine(&extensions, input.package(), true)?;
            let compiled = compile_input(&engine, &input)?;
            test_compiled(&engine, compiled, filter.as_deref(), include_ignored, list)
        }
        Command::Extension { command } => execute_extension(command),
        Command::Crate { command } => execute_crate(command),
    }
}

enum Input {
    Script(PathBuf),
    Package(Box<ResolvedPackage>),
}

impl Input {
    fn package(&self) -> Option<&ResolvedPackage> {
        match self {
            Self::Package(package) => Some(package.as_ref()),
            Self::Script(_) => None,
        }
    }
}

fn resolve_input(path: &Path, locked: bool) -> anyhow::Result<Input> {
    let package_manifest = if path.is_dir() {
        Some(path.join(husk::MANIFEST_FILE))
    } else if path.file_name().and_then(|name| name.to_str()) == Some(husk::MANIFEST_FILE) {
        Some(path.to_path_buf())
    } else {
        None
    };
    let Some(manifest) = package_manifest else {
        if locked {
            anyhow::bail!("`--locked` requires a package directory or Husk.toml input");
        }
        return Ok(Input::Script(path.to_path_buf()));
    };

    let package = ResolvedPackage::open(
        &manifest,
        PackageLimits {
            max_source_bytes: MAX_SOURCE_BYTES as u64,
            ..PackageLimits::default()
        },
    )
    .with_context(|| format!("resolve Husk package `{}`", manifest.display()))?;
    if locked {
        package
            .enforce_lock()
            .context("verify locked package inputs")?;
    } else {
        package.write_lock().context("write Husk.lock")?;
    }
    Ok(Input::Package(Box::new(package)))
}

fn cli_engine(
    extension_paths: &[PathBuf],
    package: Option<&ResolvedPackage>,
    test_mode: bool,
) -> anyhow::Result<Engine<CliState>> {
    let std_module = NativeModule::builder("std")
        .typed_function(
            "print",
            |_context: &mut CallContext<'_, CliState>, value: String| -> Result<(), NativeError> {
                print!("{value}");
                Ok(())
            },
        )
        .typed_function(
            "println",
            |_context: &mut CallContext<'_, CliState>, value: String| -> Result<(), NativeError> {
                println!("{value}");
                Ok(())
            },
        )
        .build()?;
    let mut builder = Engine::builder().register_module(std_module)?;
    if test_mode {
        builder = builder.cfg_flag("test");
    }
    for path in extension_paths {
        let bundle = ExtensionBundle::open(path, BundleLimits::default())
            .with_context(|| format!("validate extension bundle `{}`", path.display()))?;
        let component = WasmComponent::from_bundle(&bundle, WasmCompileOptions::default())
            .with_context(|| format!("compile extension bundle `{}`", path.display()))?;
        builder = builder.register_wasm_component(component)?;
    }
    if let Some(package) = package {
        for extension in &package.extensions {
            let component =
                WasmComponent::from_bundle(&extension.bundle, WasmCompileOptions::default())
                    .with_context(|| {
                        format!(
                            "compile package extension `{}` from `{}`",
                            extension.manifest_name,
                            extension.source.display()
                        )
                    })?;
            builder = builder.register_wasm_component(component)?;
        }
    }
    builder.build()
}

fn run_repl(
    engine: &Engine<CliState>,
    mut input: impl BufRead,
    mut output: impl Write,
    interactive: bool,
) -> anyhow::Result<ExitCode> {
    let mut session = engine.repl(CliState)?;
    let mut pending = String::new();
    let mut line = String::new();
    let mut had_error = false;

    if interactive {
        writeln!(
            output,
            "Husk {} — type :help for commands",
            env!("CARGO_PKG_VERSION")
        )?;
    }

    loop {
        if interactive {
            if pending.is_empty() {
                write!(output, "husk> ")?;
            } else {
                write!(output, "....> ")?;
            }
            output.flush()?;
        }

        line.clear();
        if input.read_line(&mut line)? == 0 {
            if !pending.trim().is_empty() {
                writeln!(output, "error: incomplete input at end of file")?;
                had_error = true;
            }
            break;
        }

        let command = line.trim();
        if command.starts_with(':') {
            match command {
                ":quit" | ":q" => break,
                ":help" | ":h" => {
                    writeln!(
                        output,
                        ":help   show this help\n:reset  clear definitions and values\n:quit   exit the REPL"
                    )?;
                }
                ":reset" => {
                    session = engine.repl(CliState)?;
                    pending.clear();
                    if interactive {
                        writeln!(output, "session reset")?;
                    }
                }
                command => {
                    writeln!(output, "error: unknown REPL command `{command}`")?;
                    had_error = true;
                }
            }
            continue;
        }

        pending.push_str(&line);
        match session.submit(&pending) {
            Ok(ReplOutcome::Incomplete) => {}
            Ok(ReplOutcome::Empty | ReplOutcome::Defined) => pending.clear(),
            Ok(ReplOutcome::Value(OwnedValue::Unit)) => pending.clear(),
            Ok(ReplOutcome::Value(value)) => {
                writeln!(output, "{}", format_repl_value(&value))?;
                pending.clear();
            }
            Err(error) => {
                writeln!(output, "error: {error:#}")?;
                pending.clear();
                had_error = true;
            }
        }
    }

    Ok(if had_error && !interactive {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn execute_extension(command: ExtensionCommand) -> anyhow::Result<ExitCode> {
    match command {
        ExtensionCommand::Inspect { bundle } => {
            let bundle = ExtensionBundle::open(&bundle, BundleLimits::default())
                .with_context(|| format!("validate extension bundle `{}`", bundle.display()))?;
            let requested = bundle
                .manifest()
                .capabilities
                .requested
                .iter()
                .cloned()
                .collect();
            let component = WasmComponent::from_bundle(
                &bundle,
                WasmCompileOptions {
                    granted_capabilities: requested,
                    ..WasmCompileOptions::default()
                },
            )?;
            let descriptor = component.descriptor();
            println!(
                "package: {} {}",
                bundle.manifest().name,
                bundle.manifest().version
            );
            println!("module: {} {}", descriptor.name, descriptor.version);
            println!("world: {}", bundle.manifest().world);
            println!("sha256: {}", bundle.digest());
            println!(
                "imports: {}",
                if component.raw_imports().is_empty() {
                    "none".to_string()
                } else {
                    component.raw_imports().join(", ")
                }
            );
            for function in &descriptor.functions {
                println!("export: {}::{}", descriptor.name, function.name);
            }
            for interface in &descriptor.interfaces {
                for function in &interface.functions {
                    println!(
                        "export: {}::{}::{}",
                        descriptor.name, interface.name, function.name
                    );
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        ExtensionCommand::Componentize {
            core_module,
            output,
        } => {
            let component = componentize_core_module(&core_module)?;
            let verified = WasmComponent::compile_bytes(
                "adapter",
                Version::new(0, 0, 0),
                &component,
                WasmCompileOptions::default(),
            )
            .context("verify component exports and capability imports")?;
            anyhow::ensure!(
                verified.raw_imports().is_empty(),
                "component unexpectedly imports: {}",
                verified.raw_imports().join(", ")
            );
            write_new_file(&output, &component)?;
            println!(
                "componentized {} as {} with no capability imports",
                core_module.display(),
                output.display()
            );
            Ok(ExitCode::SUCCESS)
        }
        ExtensionCommand::Pack {
            manifest,
            component,
            output,
        } => {
            let bundle = pack_directory(&manifest, &component, &output, BundleLimits::default())
                .with_context(|| format!("pack extension bundle `{}`", output.display()))?;
            println!(
                "packed {} {} as {} ({})",
                bundle.manifest().name,
                bundle.manifest().version,
                bundle.root().display(),
                bundle.digest()
            );
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn componentize_core_module(path: &Path) -> anyhow::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect core module `{}`", path.display()))?;
    anyhow::ensure!(
        metadata.is_file(),
        "core module `{}` is not a regular file",
        path.display()
    );
    anyhow::ensure!(
        metadata.len() <= MAX_COMPONENT_BYTES,
        "core module `{}` exceeds the {} byte limit",
        path.display(),
        MAX_COMPONENT_BYTES
    );
    let module =
        fs::read(path).with_context(|| format!("read core module `{}`", path.display()))?;
    wit_component::ComponentEncoder::default()
        .module(&module)
        .context("read embedded component metadata from core module")?
        .validate(true)
        .encode()
        .context("encode WebAssembly component")
}

fn write_new_file(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    anyhow::ensure!(!path.exists(), "output `{}` already exists", path.display());
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    anyhow::ensure!(
        parent.is_dir(),
        "output parent `{}` is not a directory",
        parent.display()
    );
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create temporary output in `{}`", parent.display()))?;
    temporary
        .write_all(contents)
        .with_context(|| format!("write temporary output for `{}`", path.display()))?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)
        .with_context(|| format!("publish output `{}`", path.display()))?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    resolve: Option<CargoResolve>,
}

#[derive(Debug, Deserialize)]
struct CargoResolve {
    root: Option<String>,
    nodes: Vec<CargoNode>,
}

#[derive(Debug, Deserialize)]
struct CargoNode {
    id: String,
    features: Vec<String>,
    deps: Vec<CargoNodeDependency>,
}

#[derive(Debug, Deserialize)]
struct CargoNodeDependency {
    name: String,
    pkg: String,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    id: String,
    name: String,
    version: String,
    source: Option<String>,
    rust_version: Option<String>,
    license: Option<String>,
    repository: Option<String>,
    links: Option<String>,
    targets: Vec<CargoTarget>,
    features: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct CargoTarget {
    name: String,
    kind: Vec<String>,
    crate_types: Vec<String>,
}

#[derive(Debug, Serialize)]
struct CrateInspection {
    name: String,
    version: String,
    source: String,
    rust_version: Option<String>,
    license: Option<String>,
    repository: Option<String>,
    enabled_features: Vec<String>,
    available_features: Vec<String>,
    targets: Vec<CargoTarget>,
    has_library: bool,
    has_build_script: bool,
    native_links: Option<String>,
    readiness: &'static str,
    blockers: Vec<String>,
    warnings: Vec<String>,
    next_step: &'static str,
    public_api: PublicApiInspection,
}

#[derive(Debug, Serialize)]
struct PublicApiInspection {
    status: &'static str,
    source: Option<String>,
    format_version: Option<u64>,
    unavailable_reason: Option<String>,
    resources: Vec<String>,
    compatible_items: usize,
    incompatible_items: usize,
    items: Vec<ApiItemInspection>,
}

#[derive(Debug, Serialize)]
struct ApiItemInspection {
    path: String,
    kind: &'static str,
    signature: String,
    compatibility: &'static str,
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wit: Option<WitCallableInspection>,
}

#[derive(Debug, Serialize)]
struct WitCallableInspection {
    owner_resource: Option<String>,
    declaration: String,
    resources: Vec<String>,
    resource_types: Vec<AdapterResourceInspection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    adapter: Option<AdapterCallableInspection>,
}

#[derive(Debug, Serialize)]
struct AdapterResourceInspection {
    wit_name: String,
    rust_path: String,
}

#[derive(Debug, Serialize)]
struct AdapterCallableInspection {
    implementation: String,
}

fn execute_crate(command: CrateCommand) -> anyhow::Result<ExitCode> {
    match command {
        CrateCommand::Inspect { request, json } => {
            let report = inspect_crate(request.into())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_crate_inspection(&report);
            }
            Ok(ExitCode::SUCCESS)
        }
        CrateCommand::Interface { request, includes } => {
            let report = inspect_crate(request.into())?;
            println!("{}", generate_wit_interface(&report, &includes)?);
            Ok(ExitCode::SUCCESS)
        }
        CrateCommand::Adapter {
            request,
            includes,
            output,
        } => {
            let report = inspect_crate(request.into())?;
            write_adapter_package(&report, &includes, &output)?;
            println!("generated adapter source at {}", output.display());
            Ok(ExitCode::SUCCESS)
        }
    }
}

struct CrateInspectOptions {
    crate_name: String,
    version: Option<String>,
    features: Vec<String>,
    no_default_features: bool,
    path: Option<PathBuf>,
    offline: bool,
}

impl From<CrateRequest> for CrateInspectOptions {
    fn from(request: CrateRequest) -> Self {
        Self {
            crate_name: request.crate_name,
            version: request.version,
            features: request.features,
            no_default_features: request.no_default_features,
            path: request.path,
            offline: request.offline,
        }
    }
}

fn inspect_crate(options: CrateInspectOptions) -> anyhow::Result<CrateInspection> {
    validate_crate_name(&options.crate_name)?;
    let version = options.version.as_deref().unwrap_or("*");
    semver::VersionReq::parse(version)
        .with_context(|| format!("invalid Cargo version requirement `{version}`"))?;

    let temporary = tempfile::tempdir().context("create isolated crate inspection directory")?;
    let dependency_source = if let Some(path) = &options.path {
        let path = path
            .canonicalize()
            .with_context(|| format!("resolve local crate path `{}`", path.display()))?;
        anyhow::ensure!(
            path.join("Cargo.toml").is_file(),
            "local crate path `{}` has no Cargo.toml",
            path.display()
        );
        format!("path = {}", toml_string(&path.to_string_lossy()))
    } else {
        format!("version = {}", toml_string(version))
    };
    let features = options
        .features
        .iter()
        .map(|feature| toml_string(feature))
        .collect::<Vec<_>>()
        .join(", ");
    let manifest = format!(
        "[package]\nname = \"husk-crate-inspect\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
         [dependencies.inspected]\npackage = {}\n{}\ndefault-features = {}\nfeatures = [{}]\n",
        toml_string(&options.crate_name),
        dependency_source,
        !options.no_default_features,
        features,
    );
    let manifest_path = temporary.path().join("Cargo.toml");
    fs::write(&manifest_path, manifest).context("write isolated inspection manifest")?;
    fs::create_dir(temporary.path().join("src"))
        .context("create isolated inspection source directory")?;
    fs::write(temporary.path().join("src/lib.rs"), "")
        .context("write isolated inspection source")?;

    let mut cargo =
        ProcessCommand::new(std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")));
    cargo
        .arg("metadata")
        .arg("--format-version")
        .arg("1")
        .arg("--manifest-path")
        .arg(&manifest_path);
    if options.offline {
        cargo.arg("--offline");
    }
    let output = cargo
        .output()
        .context("run Cargo metadata for crate inspection")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "Cargo could not resolve `{}`:\n{stderr}",
            options.crate_name
        );
    }
    let metadata: CargoMetadata =
        serde_json::from_slice(&output.stdout).context("parse Cargo metadata output")?;
    let resolved_package_id = metadata
        .resolve
        .as_ref()
        .and_then(|resolve| {
            let root = resolve.root.as_ref()?;
            resolve.nodes.iter().find(|node| &node.id == root)
        })
        .and_then(|root| {
            root.deps
                .iter()
                .find(|dependency| dependency.name == "inspected")
        })
        .map(|dependency| dependency.pkg.as_str())
        .ok_or_else(|| anyhow::anyhow!("Cargo metadata omitted the inspected dependency"))?;
    let package = metadata
        .packages
        .iter()
        .find(|package| package.id == resolved_package_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Cargo resolved the inspection project without package `{}`",
                options.crate_name
            )
        })?;

    let enabled_features = metadata
        .resolve
        .as_ref()
        .and_then(|resolve| resolve.nodes.iter().find(|node| node.id == package.id))
        .map(|node| node.features.clone())
        .unwrap_or_default();
    let mut available_features = package.features.keys().cloned().collect::<Vec<_>>();
    available_features.sort();
    let has_library = package.targets.iter().any(|target| {
        target
            .crate_types
            .iter()
            .any(|kind| matches!(kind.as_str(), "lib" | "rlib" | "cdylib" | "staticlib"))
    });
    let proc_macro_only = package
        .targets
        .iter()
        .any(|target| target.crate_types.iter().any(|kind| kind == "proc-macro"))
        && !has_library;
    let has_build_script = package
        .targets
        .iter()
        .any(|target| target.kind.iter().any(|kind| kind == "custom-build"));

    let mut blockers = Vec::new();
    if !has_library {
        blockers.push("no Rust library target is available".to_string());
    }
    if proc_macro_only {
        blockers.push("procedural macro crates cannot become runtime adapters".to_string());
    }
    let mut warnings = Vec::new();
    if has_build_script {
        warnings.push(
            "the crate has a build script; a future adapter build must run it in the build sandbox"
                .to_string(),
        );
    }
    if let Some(links) = &package.links {
        warnings.push(format!(
            "the crate links native library `{links}`; portable wasm compatibility is unproven"
        ));
    }
    let public_api = inspect_public_api(package, options.offline, options.path.is_some());
    if let Some(reason) = &public_api.unavailable_reason {
        warnings.push(format!("public API analysis is unavailable: {reason}"));
    }
    let api_analyzed = public_api.status == "available";

    Ok(CrateInspection {
        name: package.name.clone(),
        version: package.version.clone(),
        source: package
            .source
            .clone()
            .unwrap_or_else(|| "local path".to_string()),
        rust_version: package.rust_version.clone(),
        license: package.license.clone(),
        repository: package.repository.clone(),
        enabled_features,
        available_features,
        targets: package.targets.clone(),
        has_library,
        has_build_script,
        native_links: package.links.clone(),
        readiness: if !blockers.is_empty() {
            "unsupported"
        } else if api_analyzed {
            "ready-for-adapter-design"
        } else {
            "ready-for-api-analysis"
        },
        blockers,
        warnings,
        next_step: if api_analyzed {
            "select the compatible API surface and generate an adapter"
        } else {
            "extract and classify the crate's public Rust API"
        },
        public_api,
    })
}

fn inspect_public_api(
    package: &CargoPackage,
    offline: bool,
    local_path: bool,
) -> PublicApiInspection {
    if offline {
        return unavailable_public_api("offline mode does not contact docs.rs");
    }
    if local_path {
        return unavailable_public_api(
            "local crates require sandboxed Rustdoc generation, which is not implemented yet",
        );
    }
    if !package
        .source
        .as_deref()
        .is_some_and(|source| source.contains("crates.io-index"))
    {
        return unavailable_public_api("only crates.io releases currently have a safe API source");
    }

    match download_rustdoc_json(&package.name, &package.version)
        .and_then(|document| analyze_rustdoc_json(&package.name, &package.version, document))
    {
        Ok(report) => report,
        Err(error) => unavailable_public_api(&error.to_string()),
    }
}

fn unavailable_public_api(reason: &str) -> PublicApiInspection {
    PublicApiInspection {
        status: "unavailable",
        source: None,
        format_version: None,
        unavailable_reason: Some(reason.to_string()),
        resources: Vec::new(),
        compatible_items: 0,
        incompatible_items: 0,
        items: Vec::new(),
    }
}

fn download_rustdoc_json(crate_name: &str, version: &str) -> anyhow::Result<serde_json::Value> {
    validate_crate_name(crate_name)?;
    semver::Version::parse(version)
        .with_context(|| format!("Cargo returned invalid crate version `{version}`"))?;
    let crate_name = crate_name.to_string();
    let version = version.to_string();
    std::thread::Builder::new()
        .name("husk-rustdoc-download".to_string())
        .spawn(move || download_rustdoc_json_blocking(&crate_name, &version))
        .context("start docs.rs download thread")?
        .join()
        .map_err(|_| anyhow::anyhow!("docs.rs download thread panicked"))?
}

fn download_rustdoc_json_blocking(
    crate_name: &str,
    version: &str,
) -> anyhow::Result<serde_json::Value> {
    let url = format!("https://docs.rs/crate/{crate_name}/{version}/json.gz");
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(45))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .context("create docs.rs client")?;
    let response = client
        .get(&url)
        .send()
        .with_context(|| format!("download Rustdoc JSON from `{url}`"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!("docs.rs has no Rustdoc JSON for this exact release");
    }
    let response = response
        .error_for_status()
        .with_context(|| format!("docs.rs rejected Rustdoc JSON request `{url}`"))?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RUSTDOC_JSON_BYTES)
    {
        anyhow::bail!("compressed Rustdoc JSON exceeds the safety limit");
    }

    let decoder = flate2::read::GzDecoder::new(response);
    let mut bytes = Vec::new();
    decoder
        .take(MAX_RUSTDOC_JSON_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("decompress Rustdoc JSON")?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_RUSTDOC_JSON_BYTES,
        "decompressed Rustdoc JSON exceeds the safety limit"
    );
    serde_json::from_slice(&bytes).context("parse docs.rs Rustdoc JSON")
}

#[derive(Debug)]
struct ExportedRustItem {
    id: String,
    path: String,
    kind: String,
}

struct ApiResourceMappings {
    names: BTreeMap<String, String>,
    paths: BTreeMap<String, String>,
}

fn analyze_rustdoc_json(
    crate_name: &str,
    crate_version: &str,
    document: serde_json::Value,
) -> anyhow::Result<PublicApiInspection> {
    let format_version = document
        .get("format_version")
        .and_then(serde_json::Value::as_u64)
        .context("Rustdoc JSON omitted format_version")?;
    let root = rustdoc_id(
        document
            .get("root")
            .context("Rustdoc JSON omitted its root module")?,
    )
    .context("Rustdoc JSON root has an invalid ID")?;
    let root_namespace = rustdoc_index_item(&document, &root)
        .and_then(|item| item.get("name"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or(crate_name);
    let mut exports = BTreeMap::new();
    let mut visited_modules = HashSet::new();
    collect_module_exports(
        &document,
        &root,
        root_namespace,
        &mut visited_modules,
        &mut exports,
    );
    let resource_mappings = build_wit_resource_names(&document, root_namespace, &exports);

    let mut resources = BTreeSet::new();
    let mut items = Vec::new();
    for exported in exports.values() {
        let definition = rustdoc_index_item(&document, &exported.id);
        if matches!(exported.kind.as_str(), "struct" | "enum" | "union")
            && !definition.is_some_and(|item| rustdoc_item_has_generics(item, &exported.kind))
        {
            resources.insert(exported.path.clone());
        }
        match exported.kind.as_str() {
            "function" => {
                if let Some(item) = rustdoc_index_item(&document, &exported.id) {
                    items.push(classify_callable(
                        &document,
                        item,
                        &exported.path,
                        "function",
                        None,
                        None,
                        &resource_mappings,
                    ));
                }
            }
            "struct" | "enum" | "union" => {
                collect_inherent_methods(&document, exported, &resource_mappings, &mut items);
            }
            _ => {}
        }
    }
    items.sort_by(|left, right| left.path.cmp(&right.path));
    let compatible_items = items
        .iter()
        .filter(|item| item.compatibility == "compatible")
        .count();
    let incompatible_items = items.len() - compatible_items;

    Ok(PublicApiInspection {
        status: "available",
        source: Some(format!(
            "https://docs.rs/crate/{crate_name}/{crate_version}/json"
        )),
        format_version: Some(format_version),
        unavailable_reason: None,
        resources: resources.into_iter().collect(),
        compatible_items,
        incompatible_items,
        items,
    })
}

fn build_wit_resource_names(
    document: &serde_json::Value,
    root_namespace: &str,
    exports: &BTreeMap<(String, String), ExportedRustItem>,
) -> ApiResourceMappings {
    let mut names = BTreeMap::new();
    let mut paths = BTreeMap::new();
    for exported in exports.values() {
        if !matches!(exported.kind.as_str(), "struct" | "enum" | "union") {
            continue;
        }
        let Some(item) = rustdoc_index_item(document, &exported.id) else {
            continue;
        };
        if rustdoc_item_has_generics(item, &exported.kind) {
            continue;
        }
        let public_path = exported
            .path
            .strip_prefix(root_namespace)
            .and_then(|path| path.strip_prefix("::"))
            .unwrap_or(&exported.path);
        let candidate = wit_identifier(&public_path.replace("::", "-"));
        let should_replace = names.get(&exported.id).is_none_or(|current: &String| {
            candidate.len() < current.len()
                || (candidate.len() == current.len() && candidate < *current)
        });
        if should_replace {
            names.insert(exported.id.clone(), candidate);
            paths.insert(exported.id.clone(), exported.path.clone());
        }
    }
    ApiResourceMappings { names, paths }
}

fn collect_module_exports(
    document: &serde_json::Value,
    module_id: &str,
    namespace: &str,
    visited_modules: &mut HashSet<(String, String)>,
    exports: &mut BTreeMap<(String, String), ExportedRustItem>,
) {
    if !visited_modules.insert((module_id.to_string(), namespace.to_string())) {
        return;
    }
    let Some(module) =
        rustdoc_index_item(document, module_id).and_then(|item| item.pointer("/inner/module"))
    else {
        return;
    };
    let Some(item_ids) = module.get("items").and_then(serde_json::Value::as_array) else {
        return;
    };
    for item_id in item_ids {
        let Some(item_id) = rustdoc_id(item_id) else {
            continue;
        };
        let Some(item) = rustdoc_index_item(document, &item_id) else {
            continue;
        };
        if item.get("visibility").and_then(serde_json::Value::as_str) != Some("public") {
            continue;
        }
        let Some(inner) = item.get("inner").and_then(serde_json::Value::as_object) else {
            continue;
        };
        if let Some(import) = inner.get("use") {
            let Some(target_id) = import.get("id").and_then(rustdoc_id) else {
                continue;
            };
            if import
                .get("is_glob")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                collect_module_exports(document, &target_id, namespace, visited_modules, exports);
            } else if let Some(name) = import.get("name").and_then(serde_json::Value::as_str) {
                collect_exported_target(
                    document,
                    &target_id,
                    &format!("{namespace}::{name}"),
                    visited_modules,
                    exports,
                );
            }
            continue;
        }
        let Some(name) = item.get("name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        collect_exported_target(
            document,
            &item_id,
            &format!("{namespace}::{name}"),
            visited_modules,
            exports,
        );
    }
}

fn collect_exported_target(
    document: &serde_json::Value,
    item_id: &str,
    path: &str,
    visited_modules: &mut HashSet<(String, String)>,
    exports: &mut BTreeMap<(String, String), ExportedRustItem>,
) {
    let Some(item) = rustdoc_index_item(document, item_id) else {
        return;
    };
    let Some(kind) = item
        .get("inner")
        .and_then(serde_json::Value::as_object)
        .and_then(|inner| inner.keys().next())
    else {
        return;
    };
    if kind == "module" {
        collect_module_exports(document, item_id, path, visited_modules, exports);
    } else {
        exports.insert(
            (path.to_string(), item_id.to_string()),
            ExportedRustItem {
                id: item_id.to_string(),
                path: path.to_string(),
                kind: kind.clone(),
            },
        );
    }
}

fn collect_inherent_methods(
    document: &serde_json::Value,
    exported: &ExportedRustItem,
    resource_mappings: &ApiResourceMappings,
    items: &mut Vec<ApiItemInspection>,
) {
    let Some(definition) = rustdoc_index_item(document, &exported.id) else {
        return;
    };
    let owner_has_generics = rustdoc_item_has_generics(definition, &exported.kind);
    let Some(impl_ids) = definition
        .pointer(&format!("/inner/{}/impls", exported.kind))
        .and_then(serde_json::Value::as_array)
    else {
        return;
    };
    for impl_id in impl_ids {
        let Some(impl_id) = rustdoc_id(impl_id) else {
            continue;
        };
        let Some(implementation) =
            rustdoc_index_item(document, &impl_id).and_then(|item| item.pointer("/inner/impl"))
        else {
            continue;
        };
        if !implementation
            .get("trait")
            .is_some_and(serde_json::Value::is_null)
        {
            continue;
        }
        let Some(method_ids) = implementation
            .get("items")
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        for method_id in method_ids {
            let Some(method_id) = rustdoc_id(method_id) else {
                continue;
            };
            let Some(method) = rustdoc_index_item(document, &method_id) else {
                continue;
            };
            if method.get("visibility").and_then(serde_json::Value::as_str) != Some("public") {
                continue;
            }
            let Some(name) = method.get("name").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let mut classified = classify_callable(
                document,
                method,
                &format!("{}::{name}", exported.path),
                "associated-function",
                Some(&exported.id),
                Some(&exported.path),
                resource_mappings,
            );
            if owner_has_generics {
                classified.compatibility = "incompatible";
                classified.reason =
                    Some("the owning resource type has generic or lifetime parameters".to_string());
                classified.wit = None;
            }
            items.push(classified);
        }
    }
}

fn classify_callable(
    document: &serde_json::Value,
    item: &serde_json::Value,
    path: &str,
    default_kind: &'static str,
    owner_id: Option<&str>,
    owner_path: Option<&str>,
    resource_mappings: &ApiResourceMappings,
) -> ApiItemInspection {
    let Some(function) = item.pointer("/inner/function") else {
        return incompatible_api_item(path, default_kind, String::new(), "not a Rust function");
    };
    let inputs = function
        .pointer("/sig/inputs")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let output = function.pointer("/sig/output");
    let has_receiver = inputs.first().is_some_and(|input| {
        input
            .as_array()
            .and_then(|parts| parts.first())
            .and_then(serde_json::Value::as_str)
            == Some("self")
    });
    let kind = if has_receiver {
        "method"
    } else if owner_id
        .is_some_and(|id| output.is_some_and(|output| is_constructor_output(output, id)))
    {
        "constructor"
    } else {
        default_kind
    };
    let signature = render_signature(path, inputs, output);

    if function
        .pointer("/header/is_unsafe")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return incompatible_api_item(path, kind, signature, "unsafe functions are not exposed");
    }
    if function
        .pointer("/header/is_async")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return incompatible_api_item(path, kind, signature, "async functions are not supported");
    }
    if function
        .pointer("/generics/params")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|params| !params.is_empty())
    {
        return incompatible_api_item(
            path,
            kind,
            signature,
            "generic or lifetime parameters are not supported",
        );
    }
    for input in inputs {
        let Some(parts) = input.as_array() else {
            return incompatible_api_item(path, kind, signature, "invalid Rustdoc input");
        };
        let name = parts
            .first()
            .and_then(serde_json::Value::as_str)
            .unwrap_or("argument");
        let Some(ty) = parts.get(1) else {
            return incompatible_api_item(path, kind, signature, "invalid Rustdoc input");
        };
        if let Err(reason) = rust_type_compatibility(document, ty, name == "self") {
            return incompatible_api_item(
                path,
                kind,
                signature,
                &format!("parameter `{name}`: {reason}"),
            );
        }
    }
    if let Some(output) = output
        && !output.is_null()
        && let Err(reason) = rust_type_compatibility(document, output, false)
    {
        return incompatible_api_item(path, kind, signature, &format!("return type: {reason}"));
    }

    match render_wit_callable(
        function,
        path,
        kind,
        owner_id,
        owner_path,
        resource_mappings,
    ) {
        Ok(wit) => ApiItemInspection {
            path: path.to_string(),
            kind,
            signature,
            compatibility: "compatible",
            reason: None,
            wit: Some(wit),
        },
        Err(reason) => incompatible_api_item(path, kind, signature, &reason),
    }
}

fn incompatible_api_item(
    path: &str,
    kind: &'static str,
    signature: String,
    reason: &str,
) -> ApiItemInspection {
    ApiItemInspection {
        path: path.to_string(),
        kind,
        signature,
        compatibility: "incompatible",
        reason: Some(reason.to_string()),
        wit: None,
    }
}

fn rust_type_compatibility(
    document: &serde_json::Value,
    ty: &serde_json::Value,
    receiver: bool,
) -> Result<(), String> {
    if let Some(primitive) = ty.get("primitive").and_then(serde_json::Value::as_str) {
        return if matches!(primitive, "bool" | "u8" | "i32" | "i64" | "f64" | "str") {
            Ok(())
        } else {
            Err(format!("primitive `{primitive}` has no Husk/WIT mapping"))
        };
    }
    if let Some(reference) = ty.get("borrowed_ref") {
        if reference
            .get("is_mutable")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
            && !receiver
        {
            return Err("mutable borrowed values cannot cross the component boundary".to_string());
        }
        return reference
            .get("type")
            .ok_or_else(|| "borrowed type is missing".to_string())
            .and_then(|ty| rust_type_compatibility(document, ty, receiver));
    }
    if let Some(path) = ty.get("resolved_path") {
        let name = path
            .get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        if path
            .pointer("/args/angle_bracketed/args")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|arguments| {
                arguments
                    .iter()
                    .any(|argument| argument.get("lifetime").is_some())
            })
        {
            return Err(format!("type `{name}` carries a lifetime"));
        }
        if matches!(name.rsplit("::").next(), Some("String")) {
            return Ok(());
        }
        if matches!(name.rsplit("::").next(), Some("Vec" | "Option" | "Result")) {
            return rustdoc_type_arguments(path)
                .try_for_each(|argument| rust_type_compatibility(document, argument, false));
        }
        if let Some(item) = path
            .get("id")
            .and_then(rustdoc_id)
            .and_then(|id| rustdoc_index_item(document, &id))
            && item.get("crate_id").and_then(serde_json::Value::as_u64) == Some(0)
        {
            let kind = item
                .get("inner")
                .and_then(serde_json::Value::as_object)
                .and_then(|inner| inner.keys().next())
                .map(String::as_str)
                .unwrap_or("");
            return if rustdoc_item_has_generics(item, kind) {
                Err(format!("type `{name}` has generic or lifetime parameters"))
            } else {
                Ok(())
            };
        }
        return Err(format!("external type `{name}` has no known mapping"));
    }
    if let Some(generic) = ty.get("generic").and_then(serde_json::Value::as_str) {
        return if receiver && generic == "Self" {
            Ok(())
        } else {
            Err(format!("generic type `{generic}` is not concrete"))
        };
    }
    if let Some(tuple) = ty.get("tuple").and_then(serde_json::Value::as_array) {
        return tuple
            .iter()
            .try_for_each(|element| rust_type_compatibility(document, element, false));
    }
    if let Some(slice) = ty.get("slice") {
        return rust_type_compatibility(document, slice, false);
    }
    Err("this Rust type shape is not supported".to_string())
}

fn render_wit_callable(
    function: &serde_json::Value,
    path: &str,
    kind: &'static str,
    owner_id: Option<&str>,
    owner_path: Option<&str>,
    resource_mappings: &ApiResourceMappings,
) -> Result<WitCallableInspection, String> {
    let owner_resource = owner_id
        .and_then(|id| resource_mappings.names.get(id))
        .cloned()
        .or_else(|| owner_path.map(wit_path_leaf));
    let mut resources = BTreeSet::new();
    if let Some(owner) = &owner_resource {
        resources.insert(owner.clone());
    }
    let inputs = function
        .pointer("/sig/inputs")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let parameters = inputs
        .iter()
        .filter_map(serde_json::Value::as_array)
        .filter(|parts| parts.first().and_then(serde_json::Value::as_str) != Some("self"))
        .map(|parts| {
            let name = parts
                .first()
                .and_then(serde_json::Value::as_str)
                .unwrap_or("argument");
            let ty = parts
                .get(1)
                .ok_or_else(|| "Rustdoc input omitted its type".to_string())?;
            Ok(format!(
                "{}: {}",
                wit_identifier(name),
                rust_type_to_wit(ty, false, &resource_mappings.names, &mut resources)?
            ))
        })
        .collect::<Result<Vec<_>, String>>()?
        .join(", ");
    let output = function
        .pointer("/sig/output")
        .filter(|output| !output.is_null())
        .map(|output| rust_type_to_wit(output, false, &resource_mappings.names, &mut resources))
        .transpose()?;
    let result = output
        .as_deref()
        .map(|output| format!(" -> {output}"))
        .unwrap_or_default();
    let name = wit_identifier(path.rsplit("::").next().unwrap_or("call"));
    let declaration = match kind {
        "constructor" => {
            let owner = owner_resource
                .as_deref()
                .ok_or_else(|| "constructor has no owning resource".to_string())?;
            let Some(output) = output.as_deref() else {
                return Err("constructor has no resource return type".to_string());
            };
            if output != owner && !output.starts_with(&format!("result<{owner},")) {
                return Err(format!(
                    "constructor return `{output}` is not `{owner}` or `result<{owner}, ...>`"
                ));
            }
            if output == owner {
                format!("constructor({parameters});")
            } else {
                format!("constructor({parameters}) -> {output};")
            }
        }
        "method" => format!("{name}: func({parameters}){result};"),
        "associated-function" => format!("{name}: static func({parameters}){result};"),
        "function" => format!("{name}: func({parameters}){result};"),
        _ => return Err(format!("unsupported callable kind `{kind}`")),
    };

    let resources = resources.into_iter().collect::<Vec<_>>();
    let resource_types = resources
        .iter()
        .filter_map(|wit_name| {
            let id = resource_mappings
                .names
                .iter()
                .find_map(|(id, name)| (name == wit_name).then_some(id))?;
            Some(AdapterResourceInspection {
                wit_name: wit_name.clone(),
                rust_path: resource_mappings.paths.get(id)?.clone(),
            })
        })
        .collect();
    let adapter = render_adapter_callable(function, path, kind, owner_id, resource_mappings)
        .ok()
        .map(|implementation| AdapterCallableInspection { implementation });

    Ok(WitCallableInspection {
        owner_resource,
        declaration,
        resources,
        resource_types,
        adapter,
    })
}

fn render_adapter_callable(
    function: &serde_json::Value,
    path: &str,
    kind: &'static str,
    owner_id: Option<&str>,
    resource_mappings: &ApiResourceMappings,
) -> Result<String, String> {
    let inputs = function
        .pointer("/sig/inputs")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut parameters = Vec::new();
    let mut arguments = Vec::new();
    for input in inputs {
        let parts = input
            .as_array()
            .ok_or_else(|| "Rustdoc input is invalid".to_string())?;
        let name = parts
            .first()
            .and_then(serde_json::Value::as_str)
            .unwrap_or("argument");
        if name == "self" {
            continue;
        }
        let ty = parts
            .get(1)
            .ok_or_else(|| "Rustdoc input omitted its type".to_string())?;
        let rust_name = rust_identifier(&wit_identifier(name));
        parameters.push(format!(
            "{rust_name}: {}",
            adapter_trait_type(ty, &resource_mappings.names)?
        ));
        arguments.push(adapter_argument_expression(ty, &rust_name)?);
    }
    let parameters = parameters.join(", ");
    let arguments = arguments.join(", ");
    let rust_path = adapter_rust_path(path);
    let call = match kind {
        "method" => format!(
            "self.0.{}({arguments})",
            path.rsplit("::").next().unwrap_or("call")
        ),
        "constructor" | "associated-function" | "function" => {
            format!("{rust_path}({arguments})")
        }
        _ => return Err(format!("unsupported adapter callable kind `{kind}`")),
    };
    let method_name = if kind == "constructor" {
        "new".to_string()
    } else {
        rust_identifier(&wit_identifier(path.rsplit("::").next().unwrap_or("call")))
    };
    let receiver = if kind == "method" { "&self, " } else { "" };
    let output = function
        .pointer("/sig/output")
        .filter(|output| !output.is_null());

    if kind == "constructor" {
        let owner_id = owner_id.ok_or_else(|| "constructor owner is missing".to_string())?;
        if output.is_some_and(|output| resolved_path_has_id(output, owner_id)) {
            return Ok(format!(
                "fn {method_name}({parameters}) -> Self {{\n    Self({call})\n}}"
            ));
        }
        let output = output.ok_or_else(|| "constructor output is missing".to_string())?;
        let path = output
            .get("resolved_path")
            .ok_or_else(|| "fallible constructor output is not Result".to_string())?;
        let mut arguments = rustdoc_type_arguments(path);
        let ok = arguments
            .next()
            .ok_or_else(|| "Result is missing its success type".to_string())?;
        let error = arguments
            .next()
            .ok_or_else(|| "Result is missing its error type".to_string())?;
        if !resolved_path_has_id(ok, owner_id) {
            return Err("constructor Result does not return its owner".to_string());
        }
        let error_id = error
            .get("resolved_path")
            .and_then(|path| path.get("id"))
            .and_then(rustdoc_id)
            .ok_or_else(|| "constructor error is not a local resource".to_string())?;
        let error_name = resource_mappings
            .names
            .get(&error_id)
            .ok_or_else(|| "constructor error has no WIT resource".to_string())?;
        let error_type = rust_type_name(error_name);
        let adapter_error_type = format!("Adapter{error_type}");
        return Ok(format!(
            "fn {method_name}({parameters}) -> Result<Self, {error_type}> {{\n    \
             {call}.map(Self).map_err(|error| {error_type}::new({adapter_error_type}(error)))\n}}"
        ));
    }

    let (return_type, body) = if let Some(output) = output {
        (
            format!(
                " -> {}",
                adapter_trait_type(output, &resource_mappings.names)?
            ),
            adapter_return_expression(
                output,
                &call,
                &resource_mappings.names,
                &resource_mappings.paths,
            )?,
        )
    } else {
        (String::new(), call)
    };
    Ok(format!(
        "fn {method_name}({receiver}{parameters}){return_type} {{\n    {body}\n}}"
    ))
}

fn adapter_trait_type(
    ty: &serde_json::Value,
    resource_names: &BTreeMap<String, String>,
) -> Result<String, String> {
    if let Some(primitive) = ty.get("primitive").and_then(serde_json::Value::as_str) {
        return match primitive {
            "bool" | "u8" | "i32" | "i64" | "f64" => Ok(primitive.to_string()),
            "str" => Ok("String".to_string()),
            _ => Err(format!("primitive `{primitive}` has no adapter mapping")),
        };
    }
    if let Some(reference) = ty.get("borrowed_ref") {
        return reference
            .get("type")
            .ok_or_else(|| "borrowed type is missing".to_string())
            .and_then(|ty| adapter_trait_type(ty, resource_names));
    }
    if let Some(path) = ty.get("resolved_path") {
        let leaf = path
            .get("path")
            .and_then(serde_json::Value::as_str)
            .and_then(|path| path.rsplit("::").next())
            .unwrap_or("unknown");
        if leaf == "String" {
            return Ok("String".to_string());
        }
        if let Some(id) = path.get("id").and_then(rustdoc_id)
            && let Some(resource) = resource_names.get(&id)
        {
            return Ok(rust_type_name(resource));
        }
    }
    Err("adapter source lowering does not support this type yet".to_string())
}

fn adapter_argument_expression(ty: &serde_json::Value, name: &str) -> Result<String, String> {
    if let Some(reference) = ty.get("borrowed_ref") {
        let inner = reference
            .get("type")
            .ok_or_else(|| "borrowed type is missing".to_string())?;
        if inner.get("primitive").and_then(serde_json::Value::as_str) == Some("str")
            || inner.get("slice").is_some()
        {
            return Ok(format!("&{name}"));
        }
        return Err("borrowed resource parameters are not lowered yet".to_string());
    }
    if ty.get("primitive").is_some() {
        return Ok(name.to_string());
    }
    if ty
        .pointer("/resolved_path/path")
        .and_then(serde_json::Value::as_str)
        .and_then(|path| path.rsplit("::").next())
        == Some("String")
    {
        return Ok(name.to_string());
    }
    if ty.get("resolved_path").is_some() {
        return Err("owned resource parameters are not lowered yet".to_string());
    }
    Err("adapter source lowering does not support this parameter yet".to_string())
}

fn adapter_return_expression(
    ty: &serde_json::Value,
    call: &str,
    resource_names: &BTreeMap<String, String>,
    resource_paths: &BTreeMap<String, String>,
) -> Result<String, String> {
    if let Some(reference) = ty.get("borrowed_ref") {
        let inner = reference
            .get("type")
            .ok_or_else(|| "borrowed type is missing".to_string())?;
        if inner.get("primitive").and_then(serde_json::Value::as_str) == Some("str") {
            return Ok(format!("{call}.to_string()"));
        }
        return Err("borrowed return values are not lowered yet".to_string());
    }
    if ty.get("primitive").is_some() {
        return Ok(call.to_string());
    }
    if let Some(path) = ty.get("resolved_path") {
        let leaf = path
            .get("path")
            .and_then(serde_json::Value::as_str)
            .and_then(|path| path.rsplit("::").next());
        if leaf == Some("String") {
            return Ok(call.to_string());
        }
        if let Some(id) = path.get("id").and_then(rustdoc_id)
            && let (Some(resource), Some(_rust_path)) =
                (resource_names.get(&id), resource_paths.get(&id))
        {
            return Ok(format!(
                "{}::new(Adapter{}({call}))",
                rust_type_name(resource),
                rust_type_name(resource)
            ));
        }
    }
    Err("adapter source lowering does not support this return type yet".to_string())
}

fn adapter_rust_path(path: &str) -> String {
    path.split_once("::")
        .map(|(_, suffix)| format!("inspected::{suffix}"))
        .unwrap_or_else(|| format!("inspected::{path}"))
}

fn rust_identifier(wit_name: &str) -> String {
    let identifier = wit_name.trim_start_matches('%').replace('-', "_");
    if matches!(
        identifier.as_str(),
        "as" | "async"
            | "await"
            | "break"
            | "const"
            | "continue"
            | "crate"
            | "dyn"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
            | "yield"
    ) {
        format!("r#{identifier}")
    } else {
        identifier
    }
}

fn rust_type_name(wit_name: &str) -> String {
    wit_name
        .trim_start_matches('%')
        .split('-')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut characters = part.chars();
            characters
                .next()
                .map(|first| first.to_ascii_uppercase().to_string() + characters.as_str())
                .unwrap_or_default()
        })
        .collect()
}

fn rust_type_to_wit(
    ty: &serde_json::Value,
    borrowed: bool,
    resource_names: &BTreeMap<String, String>,
    resources: &mut BTreeSet<String>,
) -> Result<String, String> {
    if let Some(primitive) = ty.get("primitive").and_then(serde_json::Value::as_str) {
        return match primitive {
            "bool" | "u8" => Ok(primitive.to_string()),
            "i32" => Ok("s32".to_string()),
            "i64" => Ok("s64".to_string()),
            "f64" => Ok("f64".to_string()),
            "str" => Ok("string".to_string()),
            _ => Err(format!("primitive `{primitive}` has no WIT mapping")),
        };
    }
    if let Some(reference) = ty.get("borrowed_ref") {
        return reference
            .get("type")
            .ok_or_else(|| "borrowed type is missing".to_string())
            .and_then(|ty| rust_type_to_wit(ty, true, resource_names, resources));
    }
    if let Some(path) = ty.get("resolved_path") {
        let rust_name = path
            .get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let leaf = rust_name.rsplit("::").next().unwrap_or(rust_name);
        if leaf == "String" {
            return Ok("string".to_string());
        }
        let arguments = rustdoc_type_arguments(path)
            .map(|argument| rust_type_to_wit(argument, false, resource_names, resources))
            .collect::<Result<Vec<_>, String>>()?;
        match leaf {
            "Vec" => {
                let [element] = arguments.as_slice() else {
                    return Err("Vec must have one type argument".to_string());
                };
                return Ok(format!("list<{element}>"));
            }
            "Option" => {
                let [element] = arguments.as_slice() else {
                    return Err("Option must have one type argument".to_string());
                };
                return Ok(format!("option<{element}>"));
            }
            "Result" => {
                let [ok, error] = arguments.as_slice() else {
                    return Err("Result must have two type arguments".to_string());
                };
                return Ok(format!("result<{ok}, {error}>"));
            }
            _ => {}
        }
        if let Some(resource) = path
            .get("id")
            .and_then(rustdoc_id)
            .and_then(|id| resource_names.get(&id))
        {
            let resource = resource.clone();
            resources.insert(resource.clone());
            return if borrowed {
                Ok(format!("borrow<{resource}>"))
            } else {
                Ok(resource)
            };
        }
        return Err(format!("external type `{rust_name}` has no WIT mapping"));
    }
    if let Some(tuple) = ty.get("tuple").and_then(serde_json::Value::as_array) {
        let elements = tuple
            .iter()
            .map(|element| rust_type_to_wit(element, false, resource_names, resources))
            .collect::<Result<Vec<_>, String>>()?;
        return Ok(format!("tuple<{}>", elements.join(", ")));
    }
    if let Some(slice) = ty.get("slice") {
        return Ok(format!(
            "list<{}>",
            rust_type_to_wit(slice, false, resource_names, resources)?
        ));
    }
    Err("this Rust type shape has no WIT mapping".to_string())
}

fn is_constructor_output(output: &serde_json::Value, owner_id: &str) -> bool {
    if resolved_path_has_id(output, owner_id) {
        return true;
    }
    let Some(path) = output.get("resolved_path") else {
        return false;
    };
    if path
        .get("path")
        .and_then(serde_json::Value::as_str)
        .and_then(|path| path.rsplit("::").next())
        != Some("Result")
    {
        return false;
    }
    rustdoc_type_arguments(path)
        .next()
        .is_some_and(|ok| resolved_path_has_id(ok, owner_id))
}

fn resolved_path_has_id(ty: &serde_json::Value, expected_id: &str) -> bool {
    ty.get("resolved_path")
        .and_then(|path| path.get("id"))
        .and_then(rustdoc_id)
        .is_some_and(|id| id == expected_id)
}

fn rustdoc_type_arguments(path: &serde_json::Value) -> impl Iterator<Item = &serde_json::Value> {
    path.pointer("/args/angle_bracketed/args")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|argument| argument.get("type"))
}

fn rustdoc_item_has_generics(item: &serde_json::Value, kind: &str) -> bool {
    item.pointer(&format!("/inner/{kind}/generics/params"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|parameters| !parameters.is_empty())
}

fn wit_path_leaf(path: &str) -> String {
    wit_identifier(path.rsplit("::").next().unwrap_or(path))
}

fn wit_identifier(name: &str) -> String {
    let mut identifier = String::with_capacity(name.len());
    let mut previous_was_separator = true;
    let mut previous_was_lowercase = false;
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            if character.is_ascii_uppercase() && previous_was_lowercase {
                identifier.push('-');
            }
            identifier.push(character.to_ascii_lowercase());
            previous_was_separator = false;
            previous_was_lowercase = character.is_ascii_lowercase();
        } else if !previous_was_separator && !identifier.is_empty() {
            identifier.push('-');
            previous_was_separator = true;
            previous_was_lowercase = false;
        }
    }
    while identifier.ends_with('-') {
        identifier.pop();
    }
    if identifier.is_empty() {
        identifier.push_str("item");
    }
    if identifier.starts_with(|character: char| character.is_ascii_digit()) {
        identifier.insert_str(0, "item-");
    }
    if is_wit_keyword(&identifier) {
        identifier.insert(0, '%');
    }
    identifier
}

fn is_wit_keyword(identifier: &str) -> bool {
    matches!(
        identifier,
        "as" | "async"
            | "bool"
            | "borrow"
            | "char"
            | "constructor"
            | "enum"
            | "export"
            | "f32"
            | "f64"
            | "flags"
            | "from"
            | "func"
            | "future"
            | "import"
            | "include"
            | "interface"
            | "list"
            | "map"
            | "option"
            | "own"
            | "record"
            | "resource"
            | "result"
            | "s16"
            | "s32"
            | "s64"
            | "s8"
            | "static"
            | "stream"
            | "string"
            | "tuple"
            | "type"
            | "u16"
            | "u32"
            | "u64"
            | "u8"
            | "use"
            | "variant"
            | "with"
            | "world"
    )
}

fn render_signature(
    path: &str,
    inputs: &[serde_json::Value],
    output: Option<&serde_json::Value>,
) -> String {
    let inputs = inputs
        .iter()
        .filter_map(serde_json::Value::as_array)
        .map(|parts| {
            let name = parts
                .first()
                .and_then(serde_json::Value::as_str)
                .unwrap_or("_");
            let ty = parts.get(1).map(render_rust_type).unwrap_or_default();
            format!("{name}: {ty}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    let output = output
        .filter(|output| !output.is_null())
        .map(|output| format!(" -> {}", render_rust_type(output)))
        .unwrap_or_default();
    format!("{path}({inputs}){output}")
}

fn render_rust_type(ty: &serde_json::Value) -> String {
    if let Some(primitive) = ty.get("primitive").and_then(serde_json::Value::as_str) {
        return primitive.to_string();
    }
    if let Some(reference) = ty.get("borrowed_ref") {
        let mutable = if reference
            .get("is_mutable")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            "mut "
        } else {
            ""
        };
        return format!(
            "&{mutable}{}",
            reference
                .get("type")
                .map(render_rust_type)
                .unwrap_or_else(|| "?".to_string())
        );
    }
    if let Some(path) = ty.get("resolved_path") {
        let name = path
            .get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        let arguments = rustdoc_type_arguments(path)
            .map(render_rust_type)
            .collect::<Vec<_>>();
        return if arguments.is_empty() {
            name.to_string()
        } else {
            format!("{name}<{}>", arguments.join(", "))
        };
    }
    if let Some(generic) = ty.get("generic").and_then(serde_json::Value::as_str) {
        return generic.to_string();
    }
    if let Some(tuple) = ty.get("tuple").and_then(serde_json::Value::as_array) {
        return format!(
            "({})",
            tuple
                .iter()
                .map(render_rust_type)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some(slice) = ty.get("slice") {
        return format!("[{}]", render_rust_type(slice));
    }
    "?".to_string()
}

fn rustdoc_index_item<'a>(
    document: &'a serde_json::Value,
    id: &str,
) -> Option<&'a serde_json::Value> {
    document.get("index")?.get(id)
}

fn rustdoc_id(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_u64().map(|id| id.to_string()))
}

fn validate_crate_name(name: &str) -> anyhow::Result<()> {
    let valid = !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    anyhow::ensure!(valid, "invalid Cargo crate name `{name}`");
    Ok(())
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).expect("serializing a string cannot fail")
}

fn print_crate_inspection(report: &CrateInspection) {
    println!("crate: {} {}", report.name, report.version);
    println!("source: {}", report.source);
    println!(
        "rust-version: {}",
        report.rust_version.as_deref().unwrap_or("unspecified")
    );
    println!("readiness: {}", report.readiness);
    println!(
        "library-target: {}",
        if report.has_library { "yes" } else { "no" }
    );
    println!(
        "build-script: {}",
        if report.has_build_script { "yes" } else { "no" }
    );
    println!(
        "enabled-features: {}",
        if report.enabled_features.is_empty() {
            "none".to_string()
        } else {
            report.enabled_features.join(", ")
        }
    );
    for blocker in &report.blockers {
        println!("blocker: {blocker}");
    }
    for warning in &report.warnings {
        println!("warning: {warning}");
    }
    println!("public-api: {}", report.public_api.status);
    if let Some(source) = &report.public_api.source {
        println!("public-api-source: {source}");
        println!(
            "compatible-api-items: {}",
            report.public_api.compatible_items
        );
        println!(
            "incompatible-api-items: {}",
            report.public_api.incompatible_items
        );
        for item in &report.public_api.items {
            if item.compatibility == "compatible" {
                println!("compatible: {} [{}]", item.signature, item.kind);
            }
        }
    }
    println!("next: {}", report.next_step);
}

fn generate_wit_interface(report: &CrateInspection, includes: &[String]) -> anyhow::Result<String> {
    render_wit_interface(&report.name, &report.version, &report.public_api, includes)
}

fn render_wit_interface(
    crate_name: &str,
    crate_version: &str,
    public_api: &PublicApiInspection,
    includes: &[String],
) -> anyhow::Result<String> {
    let selected = select_public_api_items(public_api, includes)?;

    let mut resources = BTreeSet::new();
    let mut resource_members: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut functions = BTreeMap::new();
    for item in selected {
        let mapping = item
            .wit
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("public API item `{}` has no WIT mapping", item.path))?;
        resources.extend(mapping.resources.iter().cloned());
        if let Some(owner) = &mapping.owner_resource {
            let member_name = wit_declaration_name(&mapping.declaration);
            let existing = resource_members
                .entry(owner.clone())
                .or_default()
                .insert(member_name.to_string(), mapping.declaration.clone());
            anyhow::ensure!(
                existing.is_none(),
                "selected APIs produce duplicate WIT member `{owner}.{member_name}`"
            );
        } else {
            let function_name = wit_declaration_name(&mapping.declaration);
            let existing = functions.insert(function_name.to_string(), mapping.declaration.clone());
            anyhow::ensure!(
                existing.is_none(),
                "selected APIs produce duplicate WIT function `{function_name}`"
            );
        }
    }

    let package = wit_identifier(crate_name);
    let interface = package.clone();
    let world = wit_identifier(&format!("{crate_name}-adapter"));
    let mut wit = String::new();
    writeln!(wit, "package husk:{package}@{crate_version};\n")?;
    writeln!(wit, "interface {interface} {{")?;
    for resource in &resources {
        if let Some(members) = resource_members.get(resource) {
            writeln!(wit, "  resource {resource} {{")?;
            for declaration in members.values() {
                writeln!(wit, "    {declaration}")?;
            }
            writeln!(wit, "  }}")?;
        } else {
            writeln!(wit, "  resource {resource};")?;
        }
    }
    if !resources.is_empty() && !functions.is_empty() {
        writeln!(wit)?;
    }
    for declaration in functions.values() {
        writeln!(wit, "  {declaration}")?;
    }
    writeln!(wit, "}}\n")?;
    writeln!(wit, "world {world} {{")?;
    writeln!(wit, "  export {interface};")?;
    writeln!(wit, "}}")?;
    wit_parser::Resolve::default()
        .push_str("husk-generated.wit", &wit)
        .context("generated WIT proposal is invalid")?;
    Ok(wit)
}

fn select_public_api_items<'a>(
    public_api: &'a PublicApiInspection,
    includes: &[String],
) -> anyhow::Result<Vec<&'a ApiItemInspection>> {
    anyhow::ensure!(
        public_api.status == "available",
        "public API analysis is unavailable: {}",
        public_api
            .unavailable_reason
            .as_deref()
            .unwrap_or("unknown reason")
    );

    let requested = includes.iter().collect::<BTreeSet<_>>();
    anyhow::ensure!(
        requested.len() == includes.len(),
        "the interface selection contains duplicate API paths"
    );
    let items_by_path = public_api
        .items
        .iter()
        .map(|item| (item.path.as_str(), item))
        .collect::<BTreeMap<_, _>>();
    let mut selected = Vec::with_capacity(requested.len());
    for path in requested {
        let item = items_by_path
            .get(path.as_str())
            .ok_or_else(|| anyhow::anyhow!("public API item `{path}` was not found"))?;
        anyhow::ensure!(
            item.compatibility == "compatible",
            "public API item `{path}` is incompatible: {}",
            item.reason.as_deref().unwrap_or("unknown reason")
        );
        selected.push(*item);
    }
    Ok(selected)
}

struct GeneratedAdapterPackage {
    manifest: String,
    wit: String,
    source: String,
    report: String,
    readme: String,
}

fn render_adapter_package(
    report: &CrateInspection,
    includes: &[String],
) -> anyhow::Result<GeneratedAdapterPackage> {
    anyhow::ensure!(
        report.source.contains("crates.io-index"),
        "adapter source generation currently requires a crates.io release"
    );
    let selected = select_public_api_items(&report.public_api, includes)?;
    let wit = generate_wit_interface(report, includes)?;

    let mut resources = BTreeMap::new();
    let mut resource_methods: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut free_functions = Vec::new();
    for item in &selected {
        let mapping = item
            .wit
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("public API item `{}` has no WIT mapping", item.path))?;
        let adapter = mapping.adapter.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "public API item `{}` has WIT mapping but Rust adapter lowering is not implemented",
                item.path
            )
        })?;
        for resource in &mapping.resource_types {
            if let Some(existing) =
                resources.insert(resource.wit_name.clone(), resource.rust_path.clone())
            {
                anyhow::ensure!(
                    existing == resource.rust_path,
                    "WIT resource `{}` maps to conflicting Rust types",
                    resource.wit_name
                );
            }
        }
        if let Some(owner) = &mapping.owner_resource {
            resource_methods
                .entry(owner.clone())
                .or_default()
                .push(adapter.implementation.clone());
        } else {
            free_functions.push(adapter.implementation.clone());
        }
    }
    for methods in resource_methods.values_mut() {
        methods.sort();
    }
    free_functions.sort();

    let package_module = rust_identifier(&wit_identifier(&report.name));
    let interface_module = package_module.clone();
    let world = wit_identifier(&format!("{}-adapter", report.name));
    let mut source = String::new();
    writeln!(
        source,
        "wit_bindgen::generate!({{\n    world: {},\n    path: \"wit\",\n}});\n",
        toml_string(&world)
    )?;
    writeln!(
        source,
        "use exports::husk::{package_module}::{interface_module}::*;\n"
    )?;
    writeln!(source, "struct Component;\n")?;
    for (wit_name, rust_path) in &resources {
        writeln!(
            source,
            "#[allow(dead_code)]\nstruct Adapter{}({});",
            rust_type_name(wit_name),
            adapter_rust_path(rust_path)
        )?;
    }
    if !resources.is_empty() {
        writeln!(source)?;
    }
    writeln!(source, "impl Guest for Component {{")?;
    for wit_name in resources.keys() {
        let type_name = rust_type_name(wit_name);
        writeln!(source, "    type {type_name} = Adapter{type_name};")?;
    }
    for implementation in &free_functions {
        writeln!(source)?;
        write_indented(&mut source, implementation, 4)?;
    }
    writeln!(source, "}}\n")?;
    for wit_name in resources.keys() {
        let type_name = rust_type_name(wit_name);
        if let Some(methods) = resource_methods.get(wit_name) {
            writeln!(source, "impl Guest{type_name} for Adapter{type_name} {{")?;
            for (index, implementation) in methods.iter().enumerate() {
                if index > 0 {
                    writeln!(source)?;
                }
                write_indented(&mut source, implementation, 4)?;
            }
            writeln!(source, "}}\n")?;
        } else {
            writeln!(
                source,
                "impl Guest{type_name} for Adapter{type_name} {{}}\n"
            )?;
        }
    }
    writeln!(source, "export!(Component);")?;

    let mut enabled_features = report
        .enabled_features
        .iter()
        .filter(|feature| {
            feature.as_str() != "default" && report.available_features.contains(feature)
        })
        .cloned()
        .collect::<Vec<_>>();
    enabled_features.sort();
    let features = enabled_features
        .iter()
        .map(|feature| toml_string(feature))
        .collect::<Vec<_>>()
        .join(", ");
    let crate_package = format!("husk-adapter-{}", report.name.replace('_', "-"));
    let manifest = format!(
        "[package]\nname = {}\nversion = \"0.0.0\"\nedition = \"2024\"\npublish = false\n\n\
         [lib]\ncrate-type = [\"cdylib\"]\n\n\
         [dependencies]\nwit-bindgen = \"=0.59.0\"\n\n\
         [dependencies.inspected]\npackage = {}\nversion = {}\ndefault-features = false\nfeatures = [{}]\n",
        toml_string(&crate_package),
        toml_string(&report.name),
        toml_string(&format!("={}", report.version)),
        features,
    );
    let selection_report = serde_json::to_string_pretty(&serde_json::json!({
        "crate": report.name,
        "version": report.version,
        "features": report.enabled_features,
        "items": selected,
        "build_status": "not-built",
    }))?;
    let readme = format!(
        "# Generated Husk adapter for `{}`\n\n\
         This source was generated from the exact public API selection in \
         `husk-adapter.json`.\n\n\
         It has **not** been built or executed. Build it only through Husk's \
         future adapter sandbox; ordinary Husk commands must not invoke Cargo.\n",
        report.name
    );
    Ok(GeneratedAdapterPackage {
        manifest,
        wit,
        source,
        report: selection_report + "\n",
        readme,
    })
}

fn write_indented(output: &mut String, value: &str, spaces: usize) -> std::fmt::Result {
    let indentation = " ".repeat(spaces);
    for line in value.lines() {
        writeln!(output, "{indentation}{line}")?;
    }
    Ok(())
}

fn write_adapter_package(
    report: &CrateInspection,
    includes: &[String],
    output: &Path,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !output.exists(),
        "adapter output `{}` already exists",
        output.display()
    );
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    anyhow::ensure!(
        parent.is_dir(),
        "adapter output parent `{}` is not a directory",
        parent.display()
    );
    let package = render_adapter_package(report, includes)?;
    let temporary = tempfile::Builder::new()
        .prefix(".husk-adapter-")
        .tempdir_in(parent)
        .with_context(|| {
            format!(
                "create temporary adapter directory in `{}`",
                parent.display()
            )
        })?;
    fs::create_dir(temporary.path().join("src")).context("create adapter source directory")?;
    fs::create_dir(temporary.path().join("wit")).context("create adapter WIT directory")?;
    fs::write(temporary.path().join("Cargo.toml"), package.manifest)
        .context("write adapter Cargo.toml")?;
    fs::write(temporary.path().join("src/lib.rs"), package.source)
        .context("write adapter Rust source")?;
    fs::write(temporary.path().join("wit/world.wit"), package.wit).context("write adapter WIT")?;
    fs::write(temporary.path().join("husk-adapter.json"), package.report)
        .context("write adapter selection report")?;
    fs::write(temporary.path().join("README.md"), package.readme)
        .context("write adapter README")?;
    fs::rename(temporary.path(), output)
        .with_context(|| format!("publish generated adapter at `{}`", output.display()))?;
    Ok(())
}

fn wit_declaration_name(declaration: &str) -> &str {
    if declaration.starts_with("constructor(") {
        "constructor"
    } else {
        declaration.split(':').next().unwrap_or(declaration)
    }
}

fn compile_input(engine: &Engine<CliState>, input: &Input) -> anyhow::Result<CompiledModule> {
    match input {
        Input::Script(path) => compile_path(engine, path),
        Input::Package(package) => engine.compile_package(package),
    }
}

fn compile_path(engine: &Engine<CliState>, path: &Path) -> anyhow::Result<CompiledModule> {
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to read `{}`", path.display()))?;
    if bytes.len() > MAX_SOURCE_BYTES {
        anyhow::bail!(
            "Husk source is {} bytes; the CLI limit is {} bytes",
            bytes.len(),
            MAX_SOURCE_BYTES
        );
    }
    let source = String::from_utf8(bytes)
        .with_context(|| format!("Husk source `{}` must be UTF-8", path.display()))?;
    let source = strip_shebang_preserving_locations(source);
    let name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("script");
    engine.compile_source(name, path.to_string_lossy(), &source)
}

fn strip_shebang_preserving_locations(mut source: String) -> String {
    if source.starts_with("#!") {
        let end = source.find('\n').unwrap_or(source.len());
        source.replace_range(..end, &" ".repeat(end));
    }
    source
}

fn run_compiled(
    engine: &Engine<CliState>,
    compiled: CompiledModule,
    arguments: Vec<String>,
) -> anyhow::Result<ExitCode> {
    let signature = compiled
        .program()
        .main_signature()
        .ok_or_else(|| anyhow::anyhow!("script does not define `main`"))?;
    let call_arguments = match signature.arguments {
        MainArguments::None => {
            if !arguments.is_empty() {
                anyhow::bail!("this script's `main` does not accept arguments");
            }
            Vec::new()
        }
        MainArguments::Strings => vec![OwnedValue::List(
            arguments.into_iter().map(OwnedValue::String).collect(),
        )],
    };

    let mut instance = engine.instantiate(compiled, CliState)?;
    let value = instance.call("main", &call_arguments)?;
    match signature.result {
        MainResult::Unit => match value {
            OwnedValue::Unit => Ok(ExitCode::SUCCESS),
            value => anyhow::bail!("`main` declared `()` but returned {value:?}"),
        },
        MainResult::ExitCode => {
            let status = match value {
                OwnedValue::I32(value) => value,
                OwnedValue::I64(value) => i32::try_from(value)
                    .map_err(|_| anyhow::anyhow!("`main` exit status is outside i32"))?,
                value => anyhow::bail!("`main` declared `i32` but returned {value:?}"),
            };
            let status = u8::try_from(status)
                .map_err(|_| anyhow::anyhow!("`main` exit status must be between 0 and 255"))?;
            Ok(ExitCode::from(status))
        }
        MainResult::Result => match value {
            OwnedValue::Variant { case, fields, .. } if case == "Ok" => {
                if fields.as_slice() != [OwnedValue::Unit] {
                    anyhow::bail!("`main` must return `Result<(), E>`");
                }
                Ok(ExitCode::SUCCESS)
            }
            OwnedValue::Variant { case, fields, .. } if case == "Err" => {
                let error = fields
                    .first()
                    .map(format_owned)
                    .unwrap_or_else(|| "script returned Err".to_string());
                anyhow::bail!("{error}")
            }
            value => anyhow::bail!("`main` declared `Result` but returned {value:?}"),
        },
    }
}

fn test_compiled(
    engine: &Engine<CliState>,
    compiled: CompiledModule,
    filter: Option<&str>,
    include_ignored: bool,
    list: bool,
) -> anyhow::Result<ExitCode> {
    let tests = compiled
        .program()
        .tests()
        .into_iter()
        .filter(|test| filter.is_none_or(|filter| test.qualified_name.contains(filter)))
        .collect::<Vec<_>>();
    if list {
        for test in &tests {
            println!("{}", test.qualified_name);
        }
        return Ok(ExitCode::SUCCESS);
    }

    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut ignored = 0usize;
    let mut failures = Vec::new();
    for test in tests {
        if test.ignored && !include_ignored {
            ignored += 1;
            println!("test {} ... ignored", test.qualified_name);
            continue;
        }

        let mut instance = engine.instantiate(compiled.clone(), CliState)?;
        let result = instance.call(&test.qualified_name, &[]);
        let outcome = match (&test.expectation, result) {
            (TestExpectation::Pass, Ok(OwnedValue::Unit)) => Ok(()),
            (TestExpectation::Pass, Ok(value)) => Err(format!(
                "test returned {value:?}; test functions must return ()"
            )),
            (TestExpectation::Pass, Err(error)) => Err(error.to_string()),
            (TestExpectation::Panic { .. }, Ok(_)) => {
                Err("test did not panic as expected".to_string())
            }
            (TestExpectation::Panic { expected: None }, Err(_)) => Ok(()),
            (
                TestExpectation::Panic {
                    expected: Some(expected),
                },
                Err(error),
            ) if error.to_string().contains(expected) => Ok(()),
            (
                TestExpectation::Panic {
                    expected: Some(expected),
                },
                Err(error),
            ) => Err(format!("panic did not contain `{expected}`: {error}")),
        };
        match outcome {
            Ok(()) => {
                passed += 1;
                println!("test {} ... ok", test.qualified_name);
            }
            Err(error) => {
                failed += 1;
                println!("test {} ... FAILED", test.qualified_name);
                failures.push((test.qualified_name, error));
            }
        }
    }

    if !failures.is_empty() {
        println!("\nfailures:");
        for (name, error) in failures {
            println!("\n---- {name} ----\n{error}");
        }
    }
    println!(
        "\ntest result: {}. {passed} passed; {failed} failed; {ignored} ignored",
        if failed == 0 { "ok" } else { "FAILED" }
    );
    Ok(if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn format_owned(value: &OwnedValue) -> String {
    match value {
        OwnedValue::String(value) => value.clone(),
        value => format!("{value:?}"),
    }
}

fn format_repl_value(value: &OwnedValue) -> String {
    match value {
        OwnedValue::Unit => "()".to_string(),
        OwnedValue::Null => "null".to_string(),
        OwnedValue::Bool(value) => value.to_string(),
        OwnedValue::I32(value) => value.to_string(),
        OwnedValue::I64(value) => value.to_string(),
        OwnedValue::F64(value) => value.to_string(),
        OwnedValue::String(value) => format!("{value:?}"),
        OwnedValue::Bytes(values) => format!("{values:?}"),
        OwnedValue::List(values) => format_repl_sequence("[", "]", values),
        OwnedValue::Tuple(values) => {
            let mut rendered = format_repl_sequence("(", ")", values);
            if values.len() == 1 {
                rendered.insert(rendered.len() - 1, ',');
            }
            rendered
        }
        OwnedValue::Range {
            start,
            end,
            inclusive,
        } => format!("{start}..{}{end}", if *inclusive { "=" } else { "" }),
        OwnedValue::Record(fields) => {
            let fields = fields
                .iter()
                .map(|(name, value)| format!("{name}: {}", format_repl_value(value)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{ {fields} }}")
        }
        OwnedValue::Struct { type_name, fields } => {
            let fields = fields
                .iter()
                .map(|(name, value)| format!("{name}: {}", format_repl_value(value)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{type_name} {{ {fields} }}")
        }
        OwnedValue::Variant {
            type_name,
            case,
            fields,
        } => {
            let name = if type_name.is_empty() {
                case.clone()
            } else {
                format!("{type_name}::{case}")
            };
            if fields.is_empty() {
                name
            } else {
                format!(
                    "{name}{}",
                    format_repl_sequence("(", ")", fields.as_slice())
                )
            }
        }
        OwnedValue::Resource { type_name, .. } => format!("<resource:{type_name}>"),
        OwnedValue::Json(value) => value.to_string(),
    }
}

fn format_repl_sequence(open: &str, close: &str, values: &[OwnedValue]) -> String {
    let values = values
        .iter()
        .map(format_repl_value)
        .collect::<Vec<_>>()
        .join(", ");
    format!("{open}{values}{close}")
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn repl_runs_multiline_definitions_and_preserves_locals() {
        let engine = cli_engine(&[], None, false).unwrap();
        let input = Cursor::new(
            b"fn double(value: i32) -> i32 {\nvalue * 2\n}\nlet mut answer = 20;\nanswer += 1\nanswer = double(answer)\nanswer\n:quit\n",
        );
        let mut output = Vec::new();

        let status = run_repl(&engine, input, &mut output, false).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert_eq!(status, ExitCode::SUCCESS);
        assert_eq!(output, "21\n42\n42\n");
    }

    #[test]
    fn repl_reset_discards_session_bindings() {
        let engine = cli_engine(&[], None, false).unwrap();
        let input = Cursor::new(b"let answer = 42;\n:reset\nanswer\n:quit\n");
        let mut output = Vec::new();

        let status = run_repl(&engine, input, &mut output, false).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert_eq!(status, ExitCode::FAILURE);
        assert!(output.contains("unknown identifier `answer`"), "{output}");
    }

    #[test]
    fn repl_value_formatter_uses_husk_syntax() {
        assert_eq!(
            format_repl_value(&OwnedValue::List(vec![
                OwnedValue::I64(1),
                OwnedValue::String("two".to_string()),
            ])),
            r#"[1, "two"]"#
        );
        assert_eq!(
            format_repl_value(&OwnedValue::Range {
                start: 1,
                end: 3,
                inclusive: true,
            }),
            "1..=3"
        );
    }

    #[test]
    fn crate_inspector_reports_local_library_without_building_it() {
        let crate_directory = tempfile::tempdir().unwrap();
        fs::create_dir(crate_directory.path().join("src")).unwrap();
        fs::write(
            crate_directory.path().join("Cargo.toml"),
            r#"
                [package]
                name = "inspect-me"
                version = "1.2.3"
                edition = "2024"
                rust-version = "1.85"

                [features]
                default = ["fast"]
                fast = []
            "#,
        )
        .unwrap();
        fs::write(
            crate_directory.path().join("src/lib.rs"),
            "pub fn answer() -> i32 { 42 }\n",
        )
        .unwrap();

        let report = inspect_crate(CrateInspectOptions {
            crate_name: "inspect-me".to_string(),
            version: None,
            features: Vec::new(),
            no_default_features: false,
            path: Some(crate_directory.path().to_path_buf()),
            offline: true,
        })
        .unwrap();

        assert_eq!(report.version, "1.2.3");
        assert!(report.has_library);
        assert_eq!(report.readiness, "ready-for-api-analysis");
        assert_eq!(report.enabled_features, ["default", "fast"]);
    }

    #[test]
    fn rustdoc_analysis_finds_generic_constructor_and_borrowed_method_shapes() {
        let function =
            |inputs: serde_json::Value, output: serde_json::Value, params: serde_json::Value| {
                serde_json::json!({
                    "function": {
                        "sig": {
                            "inputs": inputs,
                            "output": output,
                            "is_c_variadic": false
                        },
                        "generics": {
                            "params": params,
                            "where_predicates": []
                        },
                        "header": {
                            "is_const": false,
                            "is_unsafe": false,
                            "is_async": false,
                            "abi": "Rust"
                        }
                    }
                })
            };
        let document = serde_json::json!({
            "format_version": 60,
            "root": 0,
            "index": {
                "0": {
                    "id": 0,
                    "crate_id": 0,
                    "name": "any_crate",
                    "visibility": "public",
                    "inner": {"module": {"items": [1, 6]}}
                },
                "1": {
                    "id": 1,
                    "crate_id": 0,
                    "name": "Widget",
                    "visibility": "public",
                    "inner": {"struct": {"impls": [2]}}
                },
                "2": {
                    "id": 2,
                    "crate_id": 0,
                    "name": null,
                    "visibility": "default",
                    "inner": {"impl": {"trait": null, "items": [3, 4, 5]}}
                },
                "3": {
                    "id": 3,
                    "crate_id": 0,
                    "name": "open",
                    "visibility": "public",
                    "inner": function(
                        serde_json::json!([[
                            "name",
                            {"borrowed_ref": {
                                "lifetime": null,
                                "is_mutable": false,
                                "type": {"primitive": "str"}
                            }}
                        ]]),
                        serde_json::json!({"resolved_path": {
                            "path": "Result",
                            "id": 100,
                            "args": {"angle_bracketed": {
                                "args": [
                                    {"type": {"resolved_path": {
                                        "path": "Widget",
                                        "id": 1,
                                        "args": null
                                    }}},
                                    {"type": {"resolved_path": {
                                        "path": "OpenError",
                                        "id": 6,
                                        "args": null
                                    }}}
                                ],
                                "constraints": []
                            }}
                        }}),
                        serde_json::json!([])
                    )
                },
                "4": {
                    "id": 4,
                    "crate_id": 0,
                    "name": "matches",
                    "visibility": "public",
                    "inner": function(
                        serde_json::json!([
                            ["self", {"borrowed_ref": {
                                "lifetime": null,
                                "is_mutable": false,
                                "type": {"generic": "Self"}
                            }}],
                            ["input", {"borrowed_ref": {
                                "lifetime": null,
                                "is_mutable": false,
                                "type": {"primitive": "str"}
                            }}]
                        ]),
                        serde_json::json!({"primitive": "bool"}),
                        serde_json::json!([])
                    )
                },
                "5": {
                    "id": 5,
                    "crate_id": 0,
                    "name": "convert",
                    "visibility": "public",
                    "inner": function(
                        serde_json::json!([["value", {"generic": "T"}]]),
                        serde_json::Value::Null,
                        serde_json::json!([{"name": "T", "kind": {"type": {
                            "bounds": [],
                            "default": null,
                            "is_synthetic": false
                        }}}])
                    )
                },
                "6": {
                    "id": 6,
                    "crate_id": 0,
                    "name": "OpenError",
                    "visibility": "public",
                    "inner": {"enum": {"impls": []}}
                }
            }
        });

        let report = analyze_rustdoc_json("any-crate", "1.2.3", document).unwrap();

        assert_eq!(report.status, "available");
        assert_eq!(
            report.resources,
            ["any_crate::OpenError", "any_crate::Widget"]
        );
        assert_eq!(report.compatible_items, 2);
        assert_eq!(report.incompatible_items, 1);
        assert!(report.items.iter().any(|item| {
            item.path == "any_crate::Widget::open"
                && item.kind == "constructor"
                && item.compatibility == "compatible"
        }));
        assert!(report.items.iter().any(|item| {
            item.path == "any_crate::Widget::matches"
                && item.kind == "method"
                && item.compatibility == "compatible"
        }));

        let wit = render_wit_interface(
            "any-crate",
            "1.2.3",
            &report,
            &[
                "any_crate::Widget::matches".to_string(),
                "any_crate::Widget::open".to_string(),
            ],
        )
        .unwrap();
        assert_eq!(
            wit,
            "\
package husk:any-crate@1.2.3;

interface any-crate {
  resource open-error;
  resource widget {
    constructor(name: string) -> result<widget, open-error>;
    matches: func(input: string) -> bool;
  }
}

world any-crate-adapter {
  export any-crate;
}
"
        );
        wit_parser::Resolve::default()
            .push_str("proposal.wit", &wit)
            .unwrap();

        let error = render_wit_interface(
            "any-crate",
            "1.2.3",
            &report,
            &["any_crate::Widget::convert".to_string()],
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("generic or lifetime parameters are not supported"),
            "{error:#}"
        );

        let inspection = CrateInspection {
            name: "any-crate".to_string(),
            version: "1.2.3".to_string(),
            source: "registry+https://github.com/rust-lang/crates.io-index".to_string(),
            rust_version: None,
            license: None,
            repository: None,
            enabled_features: vec!["default".to_string()],
            available_features: vec!["default".to_string()],
            targets: Vec::new(),
            has_library: true,
            has_build_script: false,
            native_links: None,
            readiness: "ready-for-adapter-design",
            blockers: Vec::new(),
            warnings: Vec::new(),
            next_step: "generate adapter",
            public_api: report,
        };
        let package = render_adapter_package(
            &inspection,
            &[
                "any_crate::Widget::matches".to_string(),
                "any_crate::Widget::open".to_string(),
            ],
        )
        .unwrap();
        assert!(
            package
                .manifest
                .contains("version = \"=1.2.3\"\ndefault-features = false")
        );
        assert!(
            package
                .source
                .contains("struct AdapterWidget(inspected::Widget);")
        );
        assert!(
            package
                .source
                .contains("impl GuestWidget for AdapterWidget")
        );
        assert!(package.source.contains("self.0.matches(&input)"));
        assert!(package.source.contains(
            "inspected::Widget::open(&name).map(Self).map_err(|error| \
             OpenError::new(AdapterOpenError(error)))"
        ));
    }
}
