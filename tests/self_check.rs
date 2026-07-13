use std::process::Command;

#[test]
fn self_check_reports_every_bundled_plugin_and_finishes_with_success() {
    let config = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_red"))
        .arg("--self-check")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("NO_COLOR", "1")
        .env("XDG_CONFIG_HOME", config.path())
        .output()
        .unwrap();

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
