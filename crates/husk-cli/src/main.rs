use std::{
    ffi::OsString,
    io::{self, BufRead, IsTerminal, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::Context;
use clap::{Parser, Subcommand};
use husk::{
    CallContext, CompiledModule, Engine, MainArguments, MainResult, NativeError, NativeModule,
    OwnedValue, PackageLimits, ReplOutcome, ResolvedPackage, TestExpectation, WasmCompileOptions,
    WasmComponent,
};
use husk_extension::{BundleLimits, ExtensionBundle, pack_directory};

const MAX_SOURCE_BYTES: usize = 1024 * 1024;

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
}

#[derive(Debug, Subcommand)]
enum ExtensionCommand {
    /// Validate, compile, and print a bundle's derived module signature.
    Inspect { bundle: PathBuf },
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

#[derive(Default)]
struct CliState;

fn main() -> ExitCode {
    run(std::env::args_os())
}

fn run(arguments: impl IntoIterator<Item = OsString>) -> ExitCode {
    let cli = match Cli::try_parse_from(arguments) {
        Ok(cli) => cli,
        Err(error) => {
            let _ = error.print();
            return ExitCode::from(2);
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
    }
}

enum Input {
    Script(PathBuf),
    Package(ResolvedPackage),
}

impl Input {
    fn package(&self) -> Option<&ResolvedPackage> {
        match self {
            Self::Package(package) => Some(package),
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
    Ok(Input::Package(package))
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
}
