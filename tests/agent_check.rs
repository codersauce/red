use std::{fs, process::Command};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

#[cfg(unix)]
fn fake_codex(directory: &std::path::Path, version: &str) -> std::path::PathBuf {
    let path = directory.join("codex");
    fs::write(&path, format!("#!/bin/sh\necho 'codex-cli {version}'\n")).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

#[cfg(unix)]
#[test]
fn direct_codex_check_reports_a_compatible_cli_as_ready() {
    let directory = tempfile::tempdir().unwrap();
    let codex = fake_codex(directory.path(), "0.144.5");
    let output = Command::new(env!("CARGO_BIN_EXE_red"))
        .args([
            "--agent-check",
            "--strict",
            "-c",
            &format!("agent.command={:?}", codex.display().to_string()),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("backend: Codex app-server"), "{stdout}");
    assert!(
        stdout.contains("reviewable-edit readiness: ready"),
        "{stdout}"
    );
    assert!(stdout.contains("codex-cli 0.144.5"), "{stdout}");
}

#[cfg(unix)]
#[test]
fn direct_codex_check_rejects_an_old_cli() {
    let directory = tempfile::tempdir().unwrap();
    let codex = fake_codex(directory.path(), "0.100.0");
    let output = Command::new(env!("CARGO_BIN_EXE_red"))
        .args([
            "--agent-check",
            "--strict",
            "-c",
            &format!("agent.command={:?}", codex.display().to_string()),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "{stdout}");
    assert!(
        stdout.contains("reviewable-edit readiness: not ready"),
        "{stdout}"
    );
    assert!(stdout.contains("0.144.1 or newer"), "{stdout}");
}
