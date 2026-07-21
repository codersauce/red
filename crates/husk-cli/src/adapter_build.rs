use std::{
    env,
    ffi::OsString,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::Context;
#[cfg(target_os = "macos")]
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
#[cfg(target_os = "macos")]
use std::os::unix::process::CommandExt as _;

const TARGET: &str = "wasm32-unknown-unknown";

pub(crate) struct BuildOptions {
    pub(crate) allow_network: bool,
    pub(crate) timeout: Duration,
    pub(crate) max_output_bytes: u64,
}

pub(crate) struct BuildOutput {
    pub(crate) core_module: Vec<u8>,
    pub(crate) lockfile: Vec<u8>,
    pub(crate) report: Vec<u8>,
}

pub(crate) fn build(adapter: &Path, options: &BuildOptions) -> anyhow::Result<BuildOutput> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (adapter, options);
        anyhow::bail!("the initial adapter build sandbox is available only on macOS");
    }

    #[cfg(target_os = "macos")]
    build_macos(adapter, options)
}

#[cfg(target_os = "macos")]
fn build_macos(adapter: &Path, options: &BuildOptions) -> anyhow::Result<BuildOutput> {
    let adapter = adapter
        .canonicalize()
        .with_context(|| format!("resolve adapter directory `{}`", adapter.display()))?;
    anyhow::ensure!(
        adapter.is_dir(),
        "adapter path `{}` is not a directory",
        adapter.display()
    );
    let build = tempfile::tempdir().context("create isolated adapter build directory")?;
    let build_root = build
        .path()
        .canonicalize()
        .context("resolve isolated adapter build directory")?;
    copy_adapter_source(&adapter, &build_root)?;
    fs::create_dir(build_root.join("home")).context("create isolated build home")?;
    fs::create_dir(build_root.join("tmp")).context("create isolated build temp directory")?;

    let toolchain = toolchain()?;
    let cargo_home = cargo_home()?;
    let manifest = build_root.join("Cargo.toml");
    if !build_root.join("Cargo.lock").exists() {
        let mut resolve = Command::new(&toolchain.cargo);
        resolve
            .args(["generate-lockfile", "--manifest-path"])
            .arg(&manifest);
        if !options.allow_network {
            resolve.arg("--offline");
        }
        configure_environment(
            &mut resolve,
            &build_root,
            &cargo_home,
            &toolchain,
            options.allow_network,
        );
        run_bounded(
            &mut resolve,
            &build_root,
            options,
            "resolve adapter dependencies",
        )?;
    }

    let mut fetch = Command::new(&toolchain.cargo);
    fetch
        .args(["fetch", "--locked", "--target", TARGET, "--manifest-path"])
        .arg(&manifest);
    if !options.allow_network {
        fetch.arg("--offline");
    }
    configure_environment(
        &mut fetch,
        &build_root,
        &cargo_home,
        &toolchain,
        options.allow_network,
    );
    run_bounded(
        &mut fetch,
        &build_root,
        options,
        "fetch adapter dependencies",
    )?;

    let profile = sandbox_profile(&build_root, &cargo_home, &toolchain.sysroot)?;
    let mut compile = Command::new("/usr/bin/sandbox-exec");
    compile
        .arg("-p")
        .arg(profile)
        .arg(&toolchain.cargo)
        .args([
            "build",
            "--release",
            "--locked",
            "--offline",
            "--target",
            TARGET,
            "--manifest-path",
        ])
        .arg(&manifest);
    configure_environment(&mut compile, &build_root, &cargo_home, &toolchain, false);
    run_bounded(
        &mut compile,
        &build_root,
        options,
        "compile adapter in sandbox",
    )?;

    let report_bytes =
        fs::read(build_root.join("husk-adapter.json")).context("read generated adapter report")?;
    let report: serde_json::Value =
        serde_json::from_slice(&report_bytes).context("parse generated adapter report")?;
    let crate_name = report
        .get("crate")
        .and_then(serde_json::Value::as_str)
        .context("generated adapter report omitted its crate name")?;
    let artifact_name = format!("husk_adapter_{}.wasm", crate_name.replace('-', "_"));
    let artifact = build_root
        .join("target")
        .join(TARGET)
        .join("release")
        .join(artifact_name);
    let core_module = read_bounded(&artifact, 64 * 1024 * 1024)
        .with_context(|| format!("read built adapter `{}`", artifact.display()))?;
    let lockfile = fs::read(build_root.join("Cargo.lock")).context("read generated Cargo.lock")?;
    Ok(BuildOutput {
        core_module,
        lockfile,
        report: report_bytes,
    })
}

#[cfg(target_os = "macos")]
struct Toolchain {
    cargo: PathBuf,
    rustc: PathBuf,
    sysroot: PathBuf,
}

#[cfg(target_os = "macos")]
fn toolchain() -> anyhow::Result<Toolchain> {
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| OsString::from("rustc"));
    let output = Command::new(&rustc)
        .args(["--print", "sysroot"])
        .output()
        .context("locate active Rust toolchain")?;
    anyhow::ensure!(
        output.status.success(),
        "rustc could not report its sysroot"
    );
    let sysroot = PathBuf::from(
        String::from_utf8(output.stdout)
            .context("rustc returned a non-UTF-8 sysroot")?
            .trim(),
    );
    let cargo = sysroot.join("bin/cargo");
    let rustc = sysroot.join("bin/rustc");
    anyhow::ensure!(
        cargo.is_file(),
        "Cargo is missing from `{}`",
        cargo.display()
    );
    anyhow::ensure!(
        rustc.is_file(),
        "rustc is missing from `{}`",
        rustc.display()
    );
    anyhow::ensure!(
        sysroot.join("lib/rustlib").join(TARGET).is_dir(),
        "Rust target `{TARGET}` is not installed; run `rustup target add {TARGET}`"
    );
    Ok(Toolchain {
        cargo,
        rustc,
        sysroot,
    })
}

#[cfg(target_os = "macos")]
fn cargo_home() -> anyhow::Result<PathBuf> {
    if let Some(path) = env::var_os("CARGO_HOME") {
        return PathBuf::from(path)
            .canonicalize()
            .context("resolve CARGO_HOME");
    }
    let home = env::var_os("HOME").context("HOME is not set")?;
    PathBuf::from(home)
        .join(".cargo")
        .canonicalize()
        .context("resolve default Cargo home")
}

#[cfg(target_os = "macos")]
fn copy_adapter_source(source: &Path, destination: &Path) -> anyhow::Result<()> {
    for relative in [
        "Cargo.toml",
        "husk-adapter.json",
        "README.md",
        "src/lib.rs",
        "wit/world.wit",
    ] {
        let source = source.join(relative);
        let metadata = fs::symlink_metadata(&source)
            .with_context(|| format!("inspect generated adapter file `{}`", source.display()))?;
        anyhow::ensure!(
            metadata.file_type().is_file(),
            "generated adapter file `{}` is not a regular file",
            source.display()
        );
        let destination = destination.join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create build directory `{}`", parent.display()))?;
        }
        fs::copy(&source, &destination).with_context(|| {
            format!(
                "copy generated adapter file `{}` to `{}`",
                source.display(),
                destination.display()
            )
        })?;
    }
    let lockfile = source.join("Cargo.lock");
    if lockfile.exists() {
        let metadata = fs::symlink_metadata(&lockfile)
            .with_context(|| format!("inspect adapter lockfile `{}`", lockfile.display()))?;
        anyhow::ensure!(
            metadata.file_type().is_file(),
            "adapter lockfile `{}` is not a regular file",
            lockfile.display()
        );
        fs::copy(&lockfile, destination.join("Cargo.lock"))
            .with_context(|| format!("copy adapter lockfile `{}`", lockfile.display()))?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn configure_environment(
    command: &mut Command,
    build: &Path,
    cargo_home: &Path,
    toolchain: &Toolchain,
    allow_network: bool,
) {
    let path = format!("{}:/usr/bin:/bin", toolchain.sysroot.join("bin").display());
    command
        .current_dir(build)
        .env_clear()
        .env("CARGO_HOME", cargo_home)
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_NET_OFFLINE", (!allow_network).to_string())
        .env("HOME", build.join("home"))
        .env("PATH", path)
        .env("RUST_BACKTRACE", "0")
        .env("RUSTC", &toolchain.rustc)
        .env("SOURCE_DATE_EPOCH", "1")
        .env("TMPDIR", build.join("tmp"));
}

#[cfg(target_os = "macos")]
fn sandbox_profile(build: &Path, cargo_home: &Path, sysroot: &Path) -> anyhow::Result<String> {
    fn literal(path: &Path) -> anyhow::Result<String> {
        serde_json::to_string(&path.to_string_lossy()).context("quote sandbox path")
    }

    let readable = [
        literal(build)?,
        literal(&cargo_home.join("registry"))?,
        literal(&cargo_home.join("git"))?,
        literal(sysroot)?,
        "\"/usr\"".to_string(),
        "\"/bin\"".to_string(),
        "\"/System\"".to_string(),
        "\"/Library/Apple\"".to_string(),
        "\"/Library/Developer/CommandLineTools\"".to_string(),
        "\"/dev\"".to_string(),
        "\"/private/etc/ssl\"".to_string(),
    ]
    .into_iter()
    .map(|path| format!("(subpath {path})"))
    .collect::<Vec<_>>()
    .join(" ");
    Ok(format!(
        "(version 1)\n\
         (deny default)\n\
         (import \"system.sb\")\n\
         (allow process*)\n\
         (allow signal (target same-sandbox))\n\
         (deny network*)\n\
         (allow file-read-metadata)\n\
         (allow file-read* {readable})\n\
         (allow file-write* (subpath {}))\n",
        literal(build)?
    ))
}

#[cfg(target_os = "macos")]
fn run_bounded(
    command: &mut Command,
    directory: &Path,
    options: &BuildOptions,
    description: &str,
) -> anyhow::Result<()> {
    let stdout_path = directory.join("command.stdout");
    let stderr_path = directory.join("command.stderr");
    let stdout = File::create(&stdout_path).context("create bounded command stdout")?;
    let stderr = File::create(&stderr_path).context("create bounded command stderr")?;
    command
        .process_group(0)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    let mut child = command
        .spawn()
        .with_context(|| format!("{description}: launch process"))?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("{description}: poll process"))?
        {
            break status;
        }
        let output_bytes = file_len(&stdout_path)? + file_len(&stderr_path)?;
        if output_bytes > options.max_output_bytes {
            terminate(&mut child);
            anyhow::bail!(
                "{description} exceeded the {} byte output limit",
                options.max_output_bytes
            );
        }
        if started.elapsed() > options.timeout {
            terminate(&mut child);
            anyhow::bail!(
                "{description} exceeded the {} second timeout",
                options.timeout.as_secs()
            );
        }
        thread::sleep(Duration::from_millis(25));
    };
    let stdout = read_bounded(&stdout_path, options.max_output_bytes)?;
    let stderr = read_bounded(&stderr_path, options.max_output_bytes)?;
    anyhow::ensure!(
        stdout.len() as u64 + stderr.len() as u64 <= options.max_output_bytes,
        "{description} exceeded the {} byte combined output limit",
        options.max_output_bytes
    );
    if !status.success() {
        anyhow::bail!(
            "{description} failed with {}:\n{}{}",
            display_status(status),
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn terminate(child: &mut std::process::Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        killpg(Pid::from_raw(pid), Signal::SIGKILL).ok();
    }
    child.kill().ok();
    child.wait().ok();
}

#[cfg(target_os = "macos")]
fn file_len(path: &Path) -> anyhow::Result<u64> {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .with_context(|| format!("inspect bounded output `{}`", path.display()))
}

#[cfg(target_os = "macos")]
fn read_bounded(path: &Path, limit: u64) -> anyhow::Result<Vec<u8>> {
    let file = File::open(path).with_context(|| format!("open `{}`", path.display()))?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read `{}`", path.display()))?;
    anyhow::ensure!(
        bytes.len() as u64 <= limit,
        "`{}` exceeds the {limit} byte limit",
        path.display()
    );
    Ok(bytes)
}

#[cfg(target_os = "macos")]
fn display_status(status: ExitStatus) -> String {
    status.code().map_or_else(
        || "a signal".to_string(),
        |code| format!("exit code {code}"),
    )
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn sandbox_profile_denies_network_and_limits_writes_to_build_root() {
        let profile = sandbox_profile(
            Path::new("/private/tmp/husk-build"),
            Path::new("/Users/example/.cargo"),
            Path::new("/Users/example/.rustup/toolchains/stable"),
        )
        .unwrap();

        assert!(profile.contains("(deny network*)"));
        assert!(profile.contains("(allow file-write* (subpath \"/private/tmp/husk-build\"))"));
        assert!(profile.contains("(subpath \"/Users/example/.cargo/registry\")"));
        assert!(!profile.contains("(subpath \"/Users/example/.cargo\")"));
        assert!(!profile.contains("credentials"));
    }
}
