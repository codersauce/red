use std::{fs, path::PathBuf, process::Command};

fn bundled_red() -> (tempfile::TempDir, PathBuf) {
    let bundle = tempfile::tempdir().unwrap();
    let red = bundle
        .path()
        .join(if cfg!(windows) { "red.exe" } else { "red" });
    let adapter = bundle.path().join(if cfg!(windows) {
        "red_openai_acp.exe"
    } else {
        "red_openai_acp"
    });
    let codex_adapter = bundle.path().join(if cfg!(windows) {
        "red_codex_acp.exe"
    } else {
        "red_codex_acp"
    });
    fs::copy(env!("CARGO_BIN_EXE_red"), &red).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_red_openai_acp"), &adapter).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_red_codex_acp"), &codex_adapter).unwrap();
    (bundle, red)
}

#[test]
fn agent_check_discovers_the_bundled_codex_companion_and_installed_cli() {
    let (bundle, red) = bundled_red();
    let config = tempfile::tempdir().unwrap();
    let path = tempfile::tempdir().unwrap();
    let codex = path
        .path()
        .join(if cfg!(windows) { "codex.exe" } else { "codex" });
    fs::copy(env!("CARGO_BIN_EXE_red"), &codex).unwrap();

    let output = Command::new(red)
        .args(["--agent-check", "--strict", "-c", "agent.adapter=\"codex\""])
        .env("NO_COLOR", "1")
        .env("XDG_CONFIG_HOME", config.path())
        .env_remove("OPENAI_API_KEY")
        .env("PATH", path.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "Codex agent-check failed with {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
    assert!(stdout.contains("adapter: codex"), "{stdout}");
    assert!(
        stdout.contains("reviewable-edit readiness: ready"),
        "{stdout}"
    );
    assert!(stdout.contains("codex login"), "{stdout}");
    assert!(
        stdout.contains(&bundle.path().join("red_codex_acp").display().to_string())
            || stdout.contains(
                &bundle
                    .path()
                    .join("red_codex_acp.exe")
                    .display()
                    .to_string()
            ),
        "{stdout}"
    );
}

#[test]
fn agent_check_discovers_a_bundled_companion_without_path() {
    let (bundle, red) = bundled_red();
    let config = tempfile::tempdir().unwrap();
    let output = Command::new(red)
        .arg("--agent-check")
        .env("NO_COLOR", "1")
        .env("XDG_CONFIG_HOME", config.path())
        .env("OPENAI_API_KEY", "test-secret-that-must-not-be-rendered")
        .env("PATH", "")
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "agent-check failed with {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
    assert!(
        stdout.contains("reviewable-edit readiness: ready"),
        "{stdout}"
    );
    assert!(
        stdout.contains(&bundle.path().join("red_openai_acp").display().to_string())
            || stdout.contains(
                &bundle
                    .path()
                    .join("red_openai_acp.exe")
                    .display()
                    .to_string()
            ),
        "{stdout}"
    );
    assert!(!stdout.contains("test-secret-that-must-not-be-rendered"));
}

#[test]
fn agent_check_is_informational_unless_strict_is_requested() {
    let (_bundle, red) = bundled_red();
    let config = tempfile::tempdir().unwrap();
    let informational = Command::new(&red)
        .arg("--agent-check")
        .env("NO_COLOR", "1")
        .env("XDG_CONFIG_HOME", config.path())
        .env_remove("OPENAI_API_KEY")
        .env("PATH", "")
        .output()
        .unwrap();
    let strict = Command::new(&red)
        .args(["--agent-check", "--strict"])
        .env("NO_COLOR", "1")
        .env("XDG_CONFIG_HOME", config.path())
        .env_remove("OPENAI_API_KEY")
        .env("PATH", "")
        .output()
        .unwrap();

    let informational_stdout = String::from_utf8(informational.stdout).unwrap();
    let strict_stdout = String::from_utf8(strict.stdout).unwrap();
    let strict_stderr = String::from_utf8_lossy(&strict.stderr);
    assert!(informational.status.success(), "{informational_stdout}");
    assert!(
        informational_stdout.contains("reviewable-edit readiness: not ready"),
        "{informational_stdout}"
    );
    assert!(!strict.status.success(), "{strict_stdout}");
    assert!(
        strict_stdout.contains("reviewable-edit readiness: not ready"),
        "{strict_stdout}"
    );
    assert!(
        strict_stdout.contains("Required adapter credential OPENAI_API_KEY is not set"),
        "{strict_stdout}"
    );
    assert!(
        strict_stderr.contains("ACP reviewable-edit readiness check failed"),
        "{strict_stderr}"
    );
}
