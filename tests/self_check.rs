use std::{
    path::Path,
    process::{Command, Output},
};

fn self_check_command(config: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_red"));
    configure_self_check(&mut command, config);
    command
}

fn configure_self_check(command: &mut Command, config: &Path) {
    command
        .arg("--self-check")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("NO_COLOR", "1")
        .env("XDG_CONFIG_HOME", config);
}

#[test]
fn self_check_reports_every_bundled_plugin_and_finishes_with_success() {
    let config = tempfile::tempdir().unwrap();
    assert_self_check_report(self_check_command(config.path()).output().unwrap());
}

#[cfg(unix)]
#[test]
fn self_check_succeeds_with_a_small_main_stack() {
    let config = tempfile::tempdir().unwrap();
    // Exercise the executable's main stack, not the larger Rust test-thread
    // stack. Windows already runs the ordinary self-check with its native
    // executable stack limit. Use a fresh shell's main thread: changing
    // RLIMIT_STACK in a test worker's pre_exec hook can fail on macOS.
    let mut command = Command::new("/bin/sh");
    command
        .args([
            "-c",
            "ulimit -s 1024 && exec \"$@\"",
            "red-self-check-stack",
        ])
        .arg(env!("CARGO_BIN_EXE_red"));
    configure_self_check(&mut command, config.path());
    assert_self_check_report(command.output().unwrap());
}

fn assert_self_check_report(output: Output) {
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "self-check failed with {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
    assert!(
        !stdout.contains('\u{1b}'),
        "NO_COLOR self-check output contained an ANSI escape: {stdout:?}"
    );

    let lines = stdout.lines().collect::<Vec<_>>();
    assert_eq!(lines.last(), Some(&"red self-check ok"), "{stdout}");
    let plugins = lines
        .iter()
        .filter_map(|line| line.strip_prefix("plugin "))
        .collect::<Vec<_>>();
    assert!(
        plugins.len() >= 2,
        "expected multiline plugin status output, got:\n{stdout}"
    );

    let expected = [
        "agent",
        "barbecue",
        "buffer_picker",
        "cool_search",
        "fidget",
        "git",
        "indent_guides",
        "inlay_hints",
        "lsp_symbols",
        "neotree",
        "project_search",
        "session_restore",
        "theme_browser",
    ];
    for plugin in expected {
        assert!(
            plugins
                .iter()
                .any(|status| status.strip_suffix(": active") == Some(plugin)),
            "missing active status for bundled plugin `{plugin}`:\n{stdout}"
        );
    }

    let unhealthy = plugins
        .iter()
        .filter(|status| !status.ends_with(": active"))
        .copied()
        .collect::<Vec<_>>();
    assert!(
        unhealthy.is_empty(),
        "self-check reported unhealthy plugin statuses: {unhealthy:?}\n{stdout}"
    );
}
