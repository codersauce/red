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

#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::Context;
#[cfg(unix)]
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;

const TARGET: &str = "wasm32-unknown-unknown";

pub(crate) struct BuildOptions {
    pub(crate) allow_network: bool,
    pub(crate) timeout: Duration,
    pub(crate) max_output_bytes: u64,
    pub(crate) max_memory_bytes: u64,
    pub(crate) max_processes: u64,
}

pub(crate) struct BuildOutput {
    pub(crate) core_module: Vec<u8>,
    pub(crate) lockfile: Vec<u8>,
    pub(crate) report: Vec<u8>,
}

pub(crate) fn build(adapter: &Path, options: &BuildOptions) -> anyhow::Result<BuildOutput> {
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = (adapter, options);
        anyhow::bail!(
            "no adapter build sandbox is available for this operating system; \
             Husk refuses to compile the crate"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        build_sandboxed(adapter, options)
    }

    #[cfg(windows)]
    {
        build_windows(adapter, options)
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn build_sandboxed(adapter: &Path, options: &BuildOptions) -> anyhow::Result<BuildOutput> {
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

    let mut compile = sandboxed_compile_command(&build_root, &cargo_home, &toolchain, &manifest)?;
    configure_environment(&mut compile, &build_root, &cargo_home, &toolchain, false);
    run_bounded(
        &mut compile,
        &build_root,
        options,
        "compile adapter in sandbox",
    )?;

    collect_build_output(&build_root)
}

#[cfg(windows)]
fn build_windows(adapter: &Path, options: &BuildOptions) -> anyhow::Result<BuildOutput> {
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
        run_bounded_windows(
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
    run_bounded_windows(
        &mut fetch,
        &build_root,
        options,
        "fetch adapter dependencies",
    )?;
    run_windows_sandbox(&build_root, &cargo_home, &toolchain, options)?;
    collect_build_output(&build_root)
}

#[cfg(windows)]
fn run_windows_sandbox(
    build: &Path,
    cargo_home: &Path,
    toolchain: &Toolchain,
    options: &BuildOptions,
) -> anyhow::Result<()> {
    const MINIMUM_SANDBOX_MEMORY_BYTES: u64 = 2 * 1024 * 1024 * 1024;
    anyhow::ensure!(
        options.max_memory_bytes >= MINIMUM_SANDBOX_MEMORY_BYTES,
        "Windows Sandbox requires a memory limit of at least {MINIMUM_SANDBOX_MEMORY_BYTES} bytes"
    );
    let windows = env::var_os("WINDIR").context("WINDIR is not set")?;
    let launcher = PathBuf::from(windows)
        .join("System32")
        .join("WindowsSandbox.exe");
    anyhow::ensure!(
        launcher.is_file(),
        "Windows Sandbox is unavailable; enable the Windows Sandbox optional feature"
    );

    let script_path = build.join("run-build.ps1");
    fs::write(
        &script_path,
        windows_build_script(options.max_memory_bytes, options.max_processes),
    )
    .context("write Windows Sandbox build script")?;
    let memory_mib = options.max_memory_bytes.div_ceil(1024 * 1024);
    let registry = cargo_home.join("registry");
    anyhow::ensure!(
        registry.is_dir(),
        "Cargo registry cache `{}` is unavailable",
        registry.display()
    );
    let git = cargo_home.join("git");
    let git_mapping = if git.is_dir() {
        format!(
            "             <MappedFolder><HostFolder>{}</HostFolder><SandboxFolder>C:\\husk-cargo\\git</SandboxFolder><ReadOnly>true</ReadOnly></MappedFolder>\n",
            xml_escape(&git)
        )
    } else {
        String::new()
    };
    let configuration = format!(
        "<Configuration>\n\
           <MappedFolders>\n\
             <MappedFolder><HostFolder>{}</HostFolder><SandboxFolder>C:\\husk-build</SandboxFolder><ReadOnly>false</ReadOnly></MappedFolder>\n\
             <MappedFolder><HostFolder>{}</HostFolder><SandboxFolder>C:\\husk-cargo\\registry</SandboxFolder><ReadOnly>true</ReadOnly></MappedFolder>\n\
{git_mapping}\
             <MappedFolder><HostFolder>{}</HostFolder><SandboxFolder>C:\\husk-rust</SandboxFolder><ReadOnly>true</ReadOnly></MappedFolder>\n\
           </MappedFolders>\n\
           <Networking>Disable</Networking>\n\
           <VGpu>Disable</VGpu>\n\
           <ClipboardRedirection>Disable</ClipboardRedirection>\n\
           <MemoryInMB>{memory_mib}</MemoryInMB>\n\
           <LogonCommand><Command>powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File C:\\husk-build\\run-build.ps1</Command></LogonCommand>\n\
         </Configuration>\n",
        xml_escape(build),
        xml_escape(&registry),
        xml_escape(&toolchain.sysroot),
    );
    let configuration_path = build.join("husk-build.wsb");
    fs::write(&configuration_path, configuration).context("write Windows Sandbox configuration")?;
    let status_path = build.join("sandbox.status");
    let stdout_path = build.join("compile.stdout");
    let stderr_path = build.join("compile.stderr");
    let mut child = Command::new(launcher)
        .arg(&configuration_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("launch Windows Sandbox")?;
    let started = Instant::now();
    loop {
        if status_path.is_file() {
            break;
        }
        if let Some(status) = child.try_wait().context("poll Windows Sandbox")? {
            anyhow::bail!(
                "Windows Sandbox exited with {} before reporting the build result",
                display_status(status)
            );
        }
        let output_bytes = optional_file_len(&stdout_path)? + optional_file_len(&stderr_path)?;
        if output_bytes > options.max_output_bytes {
            child.kill().ok();
            child.wait().ok();
            anyhow::bail!(
                "compile adapter in Windows Sandbox exceeded the {} byte output limit",
                options.max_output_bytes
            );
        }
        if started.elapsed() > options.timeout {
            child.kill().ok();
            child.wait().ok();
            anyhow::bail!(
                "compile adapter in Windows Sandbox exceeded the {} second timeout",
                options.timeout.as_secs()
            );
        }
        thread::sleep(Duration::from_millis(100));
    }
    child.kill().ok();
    child.wait().ok();
    let status = fs::read_to_string(&status_path).context("read Windows Sandbox build status")?;
    let stdout = read_bounded(&stdout_path, options.max_output_bytes)?;
    let stderr = read_bounded(&stderr_path, options.max_output_bytes)?;
    anyhow::ensure!(
        stdout.len() as u64 + stderr.len() as u64 <= options.max_output_bytes,
        "compile adapter in Windows Sandbox exceeded the {} byte combined output limit",
        options.max_output_bytes
    );
    anyhow::ensure!(
        status.trim() == "0",
        "compile adapter in Windows Sandbox failed ({status}):\n{}{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    Ok(())
}

#[cfg(any(windows, test))]
fn windows_build_script(max_memory_bytes: u64, max_processes: u64) -> String {
    format!(
        "$ErrorActionPreference = 'Stop'\n\
         trap {{ Set-Content -NoNewline 'C:\\husk-build\\sandbox.status' 'launcher:error'; shutdown.exe /s /t 0 /f; exit 1 }}\n\
         $env:CARGO_HOME = 'C:\\husk-cargo'\n\
         $env:CARGO_INCREMENTAL = '0'\n\
         $env:CARGO_NET_OFFLINE = 'true'\n\
         $env:HOME = 'C:\\husk-build\\home'\n\
         $env:PATH = 'C:\\husk-rust\\bin;C:\\Windows\\System32'\n\
         $env:RUST_BACKTRACE = '0'\n\
         $env:RUSTC = 'C:\\husk-rust\\bin\\rustc.exe'\n\
         $env:SOURCE_DATE_EPOCH = '1'\n\
         $env:TMPDIR = 'C:\\husk-build\\tmp'\n\
         $arguments = @('build', '--release', '--locked', '--offline', '--target', '{TARGET}', '--manifest-path', 'C:\\husk-build\\Cargo.toml')\n\
         $process = Start-Process -FilePath 'C:\\husk-rust\\bin\\cargo.exe' -ArgumentList $arguments -WorkingDirectory 'C:\\husk-build' -PassThru -NoNewWindow -RedirectStandardOutput 'C:\\husk-build\\compile.stdout' -RedirectStandardError 'C:\\husk-build\\compile.stderr'\n\
         while (-not $process.HasExited) {{\n\
           $all = @(Get-CimInstance Win32_Process)\n\
           $ids = [System.Collections.Generic.HashSet[int]]::new()\n\
           [void]$ids.Add($process.Id)\n\
           do {{\n\
             $changed = $false\n\
             foreach ($candidate in $all) {{\n\
               if ($ids.Contains([int]$candidate.ParentProcessId) -and $ids.Add([int]$candidate.ProcessId)) {{ $changed = $true }}\n\
             }}\n\
           }} while ($changed)\n\
           $owned = @($all | Where-Object {{ $ids.Contains([int]$_.ProcessId) }})\n\
           $memory = ($owned | Measure-Object -Property WorkingSetSize -Sum).Sum\n\
           if ($owned.Count -gt {max_processes}) {{ taskkill.exe /PID $process.Id /T /F | Out-Null; Set-Content -NoNewline 'C:\\husk-build\\sandbox.status' 'limit:processes'; shutdown.exe /s /t 0 /f; exit }}\n\
           if ($memory -gt {max_memory_bytes}) {{ taskkill.exe /PID $process.Id /T /F | Out-Null; Set-Content -NoNewline 'C:\\husk-build\\sandbox.status' 'limit:memory'; shutdown.exe /s /t 0 /f; exit }}\n\
           Start-Sleep -Milliseconds 50\n\
           $process.Refresh()\n\
         }}\n\
         Set-Content -NoNewline 'C:\\husk-build\\sandbox.status' $process.ExitCode\n\
         shutdown.exe /s /t 0 /f\n"
    )
}

#[cfg(any(windows, test))]
fn xml_escape(path: &Path) -> String {
    let path = path.to_string_lossy();
    let path = path
        .strip_prefix(r"\\?\UNC\")
        .map(|path| format!(r"\\{path}"))
        .or_else(|| path.strip_prefix(r"\\?\").map(str::to_string))
        .unwrap_or_else(|| path.into_owned());
    path.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn collect_build_output(build_root: &Path) -> anyhow::Result<BuildOutput> {
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

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
struct Toolchain {
    cargo: PathBuf,
    rustc: PathBuf,
    sysroot: PathBuf,
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
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
    let executable_suffix = if cfg!(windows) { ".exe" } else { "" };
    let cargo = sysroot.join(format!("bin/cargo{executable_suffix}"));
    let rustc = sysroot.join(format!("bin/rustc{executable_suffix}"));
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

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn cargo_home() -> anyhow::Result<PathBuf> {
    if let Some(path) = env::var_os("CARGO_HOME") {
        return PathBuf::from(path)
            .canonicalize()
            .context("resolve CARGO_HOME");
    }
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .context("neither HOME nor USERPROFILE is set")?;
    PathBuf::from(home)
        .join(".cargo")
        .canonicalize()
        .context("resolve default Cargo home")
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
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

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn configure_environment(
    command: &mut Command,
    build: &Path,
    cargo_home: &Path,
    toolchain: &Toolchain,
    allow_network: bool,
) {
    let mut paths = vec![toolchain.sysroot.join("bin")];
    #[cfg(unix)]
    paths.extend([PathBuf::from("/usr/bin"), PathBuf::from("/bin")]);
    #[cfg(windows)]
    if let Some(windows) = env::var_os("WINDIR") {
        paths.push(PathBuf::from(windows).join("System32"));
    }
    let path = env::join_paths(paths).expect("trusted toolchain paths are valid");
    command
        .current_dir(build)
        .env_clear()
        .env("CARGO_BUILD_JOBS", "1")
        .env("CARGO_HOME", cargo_home)
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_NET_OFFLINE", (!allow_network).to_string())
        .env("HOME", build.join("home"))
        .env("PATH", path)
        .env("RUST_BACKTRACE", "0")
        .env("RUSTC", &toolchain.rustc)
        .env("SOURCE_DATE_EPOCH", "1")
        .env("TMPDIR", build.join("tmp"));

    #[cfg(target_os = "macos")]
    configure_macos_toolchain(command);
}

#[cfg(target_os = "macos")]
fn configure_macos_toolchain(command: &mut Command) {
    let sdk = env::var_os("SDKROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk"));
    if sdk.is_dir() {
        command.env("SDKROOT", sdk);
    }

    // The `/usr/bin/cc` shim invokes xcrun, whose global cache is intentionally
    // outside the adapter sandbox. Invoke the installed compiler directly.
    let clang = Path::new("/Library/Developer/CommandLineTools/usr/bin/clang");
    if clang.is_file() {
        command
            .env("CC", clang)
            .env("CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER", clang)
            .env("CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER", clang);
    }
}

#[cfg(target_os = "macos")]
fn sandboxed_compile_command(
    build: &Path,
    cargo_home: &Path,
    toolchain: &Toolchain,
    manifest: &Path,
) -> anyhow::Result<Command> {
    let profile = sandbox_profile(build, cargo_home, &toolchain.sysroot)?;
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command
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
        .arg(manifest);
    Ok(command)
}

#[cfg(target_os = "linux")]
fn sandboxed_compile_command(
    build: &Path,
    cargo_home: &Path,
    toolchain: &Toolchain,
    manifest: &Path,
) -> anyhow::Result<Command> {
    let bubblewrap = ["/usr/bin/bwrap", "/bin/bwrap"]
        .into_iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .context(
            "the Linux adapter build sandbox requires `bwrap` (Bubblewrap) at \
             /usr/bin/bwrap or /bin/bwrap",
        )?;
    let mut command = Command::new(bubblewrap);
    command.args([
        "--die-with-parent",
        "--new-session",
        "--unshare-all",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
    ]);
    for path in ["/usr", "/bin", "/lib", "/lib64", "/etc/ssl"] {
        let path = Path::new(path);
        if path.exists() {
            command.arg("--ro-bind").arg(path).arg(path);
        }
    }
    for path in [
        toolchain.sysroot.clone(),
        cargo_home.join("registry"),
        cargo_home.join("git"),
    ] {
        if path.exists() {
            command.arg("--ro-bind").arg(&path).arg(&path);
        }
    }
    command
        .arg("--bind")
        .arg(build)
        .arg(build)
        .arg("--chdir")
        .arg(build)
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
        .arg(manifest);
    Ok(command)
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
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
        let usage = match process_tree_resource_usage(child.id()) {
            Ok(usage) => usage,
            Err(error) => {
                terminate(&mut child);
                return Err(error)
                    .with_context(|| format!("{description}: inspect build process tree"));
            }
        };
        if usage.resident_bytes > options.max_memory_bytes {
            terminate(&mut child);
            anyhow::bail!(
                "{description} exceeded the {} byte aggregate resident memory limit",
                options.max_memory_bytes
            );
        }
        if usage.processes > options.max_processes {
            terminate(&mut child);
            anyhow::bail!(
                "{description} exceeded the {} process limit",
                options.max_processes
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

#[cfg(windows)]
fn run_bounded_windows(
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
            child.kill().ok();
            child.wait().ok();
            anyhow::bail!(
                "{description} exceeded the {} byte output limit",
                options.max_output_bytes
            );
        }
        if started.elapsed() > options.timeout {
            child.kill().ok();
            child.wait().ok();
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessSnapshot {
    pid: u32,
    parent_pid: u32,
    process_group: u32,
    resident_bytes: u64,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ProcessTreeResourceUsage {
    processes: u64,
    resident_bytes: u64,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn process_tree_resource_usage(root_pid: u32) -> anyhow::Result<ProcessTreeResourceUsage> {
    let snapshots = process_snapshots()?;
    Ok(measure_process_tree(root_pid, &snapshots))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn measure_process_tree(root_pid: u32, snapshots: &[ProcessSnapshot]) -> ProcessTreeResourceUsage {
    let mut children: HashMap<u32, Vec<&ProcessSnapshot>> = HashMap::new();
    let mut pending = VecDeque::new();
    for snapshot in snapshots {
        children
            .entry(snapshot.parent_pid)
            .or_default()
            .push(snapshot);
        if snapshot.pid == root_pid || snapshot.process_group == root_pid {
            pending.push_back(snapshot);
        }
    }

    let mut visited = HashSet::new();
    let mut usage = ProcessTreeResourceUsage::default();
    while let Some(snapshot) = pending.pop_front() {
        if !visited.insert(snapshot.pid) {
            continue;
        }
        usage.processes = usage.processes.saturating_add(1);
        usage.resident_bytes = usage.resident_bytes.saturating_add(snapshot.resident_bytes);
        if let Some(descendants) = children.get(&snapshot.pid) {
            pending.extend(descendants.iter().copied());
        }
    }
    usage
}

#[cfg(target_os = "linux")]
fn process_snapshots() -> anyhow::Result<Vec<ProcessSnapshot>> {
    let mut snapshots = Vec::new();
    for entry in fs::read_dir("/proc").context("inspect Linux process table")? {
        let entry = entry.context("read Linux process table entry")?;
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let stat = match fs::read_to_string(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(_) => continue,
        };
        let Some((parent_pid, process_group)) = linux_process_identity(&stat) else {
            continue;
        };
        let status = match fs::read_to_string(entry.path().join("status")) {
            Ok(status) => status,
            Err(_) => continue,
        };
        let resident_bytes = status
            .lines()
            .find_map(|line| line.strip_prefix("VmRSS:"))
            .and_then(|value| value.split_whitespace().next())
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0)
            .saturating_mul(1024);
        snapshots.push(ProcessSnapshot {
            pid,
            parent_pid,
            process_group,
            resident_bytes,
        });
    }
    Ok(snapshots)
}

#[cfg(any(target_os = "linux", test))]
fn linux_process_identity(stat: &str) -> Option<(u32, u32)> {
    // Linux permits spaces and parentheses in `comm`, so split after the last
    // closing parenthesis before reading state, parent PID, and process group.
    let (_, fields) = stat.rsplit_once(')')?;
    let mut fields = fields.split_whitespace();
    fields.next()?;
    let parent_pid = fields.next()?.parse().ok()?;
    let process_group = fields.next()?.parse().ok()?;
    Some((parent_pid, process_group))
}

#[cfg(target_os = "macos")]
fn process_snapshots() -> anyhow::Result<Vec<ProcessSnapshot>> {
    const PROC_UID_ONLY: u32 = 4;
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { nix::libc::getuid() };
    // SAFETY: no pointer is dereferenced when the buffer size is zero.
    let bytes = unsafe { nix::libc::proc_listpids(PROC_UID_ONLY, uid, std::ptr::null_mut(), 0) };
    anyhow::ensure!(bytes >= 0, "inspect macOS process table");
    let pid_size = std::mem::size_of::<i32>();
    let capacity = usize::try_from(bytes)
        .unwrap_or(0)
        .div_ceil(pid_size)
        .saturating_add(32);
    let mut pids = vec![0i32; capacity];
    let buffer_bytes = i32::try_from(pids.len().saturating_mul(pid_size)).unwrap_or(i32::MAX);
    // SAFETY: `pids` is writable for exactly `buffer_bytes`.
    let bytes = unsafe {
        nix::libc::proc_listpids(PROC_UID_ONLY, uid, pids.as_mut_ptr().cast(), buffer_bytes)
    };
    anyhow::ensure!(bytes >= 0, "read macOS process table");
    pids.truncate(usize::try_from(bytes).unwrap_or(0) / pid_size);

    let mut snapshots = Vec::with_capacity(pids.len());
    for pid in pids.into_iter().filter(|pid| *pid > 0) {
        let mut task = std::mem::MaybeUninit::<nix::libc::proc_taskallinfo>::uninit();
        let task_size = i32::try_from(std::mem::size_of_val(&task)).unwrap_or(i32::MAX);
        // SAFETY: `task` points to writable storage for one proc_taskallinfo.
        let read = unsafe {
            nix::libc::proc_pidinfo(
                pid,
                nix::libc::PROC_PIDTASKALLINFO,
                0,
                task.as_mut_ptr().cast(),
                task_size,
            )
        };
        if read == task_size {
            // SAFETY: proc_pidinfo initialized the complete structure.
            let task = unsafe { task.assume_init() };
            snapshots.push(ProcessSnapshot {
                pid: task.pbsd.pbi_pid,
                parent_pid: task.pbsd.pbi_ppid,
                process_group: task.pbsd.pbi_pgid,
                resident_bytes: task.ptinfo.pti_resident_size,
            });
        }
    }
    Ok(snapshots)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn terminate(child: &mut std::process::Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        killpg(Pid::from_raw(pid), Signal::SIGKILL).ok();
    }
    child.kill().ok();
    child.wait().ok();
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn file_len(path: &Path) -> anyhow::Result<u64> {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .with_context(|| format!("inspect bounded output `{}`", path.display()))
}

#[cfg(windows)]
fn optional_file_len(path: &Path) -> anyhow::Result<u64> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => {
            Err(error).with_context(|| format!("inspect bounded output `{}`", path.display()))
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
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

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn display_status(status: ExitStatus) -> String {
    status.code().map_or_else(
        || "a signal".to_string(),
        |code| format!("exit code {code}"),
    )
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;

    fn snapshot(
        pid: u32,
        parent_pid: u32,
        process_group: u32,
        resident_bytes: u64,
    ) -> ProcessSnapshot {
        ProcessSnapshot {
            pid,
            parent_pid,
            process_group,
            resident_bytes,
        }
    }

    fn bounded_test_options(max_processes: u64) -> BuildOptions {
        BuildOptions {
            allow_network: false,
            timeout: Duration::from_secs(5),
            max_output_bytes: 1024,
            max_memory_bytes: 64 * 1024 * 1024,
            max_processes,
        }
    }

    struct BackgroundProcess(std::process::Child);

    impl BackgroundProcess {
        fn spawn() -> Self {
            let child = Command::new("/bin/sleep")
                .arg("5")
                .process_group(0)
                .spawn()
                .expect("start isolated background process");
            Self(child)
        }
    }

    impl Drop for BackgroundProcess {
        fn drop(&mut self) {
            terminate(&mut self.0);
        }
    }

    #[test]
    fn process_tree_excludes_unrelated_user_processes_and_resident_memory() {
        let snapshots = [
            snapshot(91, 1, 91, u64::MAX),
            snapshot(43, 42, 42, 30),
            snapshot(42, 1, 42, 20),
            snapshot(92, 91, 91, u64::MAX),
        ];

        assert_eq!(
            measure_process_tree(42, &snapshots),
            ProcessTreeResourceUsage {
                processes: 2,
                resident_bytes: 50,
            }
        );
    }

    #[test]
    fn process_tree_includes_descendants_that_create_a_new_process_group() {
        let snapshots = [
            snapshot(44, 43, 43, 40),
            snapshot(43, 42, 43, 30),
            snapshot(42, 1, 42, 20),
            snapshot(99, 1, 99, 1_000_000),
        ];

        assert_eq!(
            measure_process_tree(42, &snapshots),
            ProcessTreeResourceUsage {
                processes: 3,
                resident_bytes: 90,
            }
        );
    }

    #[test]
    fn process_tree_keeps_tracking_reparented_members_of_the_build_group() {
        let snapshots = [snapshot(43, 1, 42, 30), snapshot(42, 1, 42, 20)];

        assert_eq!(
            measure_process_tree(42, &snapshots),
            ProcessTreeResourceUsage {
                processes: 2,
                resident_bytes: 50,
            }
        );
    }

    #[test]
    fn bounded_build_ignores_unrelated_processes_owned_by_the_same_user() {
        let _first_unrelated_process = BackgroundProcess::spawn();
        let _second_unrelated_process = BackgroundProcess::spawn();
        let directory = tempfile::tempdir().unwrap();
        let mut command = Command::new("/bin/sleep");
        command.arg("0.1");

        run_bounded(
            &mut command,
            directory.path(),
            &bounded_test_options(1),
            "run process-tree isolation fixture",
        )
        .unwrap();
    }

    #[test]
    fn bounded_build_rejects_extra_processes_in_its_own_tree() {
        let directory = tempfile::tempdir().unwrap();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "/bin/sleep 5 & wait"]);

        let error = run_bounded(
            &mut command,
            directory.path(),
            &bounded_test_options(1),
            "run process limit fixture",
        )
        .unwrap_err();

        assert!(error.to_string().contains("process limit"), "{error:#}");
    }

    #[test]
    fn bounded_build_rejects_memory_used_by_its_own_process_tree() {
        let directory = tempfile::tempdir().unwrap();
        let mut command = Command::new("/bin/sleep");
        command.arg("1");
        let options = BuildOptions {
            max_memory_bytes: 1,
            ..bounded_test_options(1)
        };

        let error = run_bounded(
            &mut command,
            directory.path(),
            &options,
            "run memory limit fixture",
        )
        .unwrap_err();

        assert!(error.to_string().contains("memory limit"), "{error:#}");
    }

    #[test]
    fn linux_process_identity_handles_spaces_and_parentheses_in_command_names() {
        assert_eq!(
            linux_process_identity("42 (cargo (build script)) S 12 34 56"),
            Some((12, 34))
        );
    }

    #[test]
    fn linux_process_identity_rejects_malformed_process_table_entries() {
        for stat in ["", "42 cargo S 12 34", "42 (cargo) S", "42 (cargo) S x 34"] {
            assert!(linux_process_identity(stat).is_none(), "{stat}");
        }
    }

    #[cfg(target_os = "macos")]
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

    #[test]
    fn windows_sandbox_script_is_offline_and_enforces_declared_limits() {
        let script = windows_build_script(2_147_483_648, 64);

        assert!(script.contains("$env:CARGO_NET_OFFLINE = 'true'"));
        assert!(script.contains("'--locked', '--offline'"));
        assert!(script.contains("$owned.Count -gt 64"));
        assert!(script.contains("$memory -gt 2147483648"));
        assert!(script.contains("taskkill.exe /PID $process.Id /T /F"));
        assert!(!script.contains("credentials"));
        assert_eq!(
            xml_escape(Path::new(r"\\?\C:\build&cache")),
            r"C:\build&amp;cache"
        );
    }
}
