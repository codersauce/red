//! The `red` binary applies `-c`/`--config` overrides before it prints config.

use std::fs;
use std::process::Command;

fn red(config_home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_red"))
        .args(args)
        .env("XDG_CONFIG_HOME", config_home)
        .env("HOME", config_home)
        .output()
        .expect("red binary")
}

#[test]
fn print_config_applies_repeated_cli_overrides_over_the_config_file() {
    let home = tempfile::tempdir().unwrap();
    let config_dir = home.path().join("red");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        "theme = \"red.json\"\nscrolloff = 9\nrelative_line_numbers = false\n",
    )
    .unwrap();

    let output = red(
        home.path(),
        &[
            "--print-config",
            "-c",
            "theme=onedark.json",
            "--config",
            "languages.rust.indent_width=2",
            "-c",
            "scrolloff=8",
            "-c",
            "scrolloff=4",
            "-c",
            "relative_line_numbers=true",
            "-c",
            "disabled_plugins=[\"fidget\"]",
        ],
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        output.status.success(),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("precedence: embedded defaults < config.toml < -c/--config"));
    assert!(stdout.contains("-c theme=onedark.json"));
    assert!(stdout.contains("theme = \"onedark.json\""));
    assert!(!stdout.contains("theme = \"red.json\""));
    assert!(stdout.contains("scrolloff = 4"));
    assert!(stdout.contains("indent_width = 2"));
    assert!(stdout.contains("relative_line_numbers = true"));
    assert!(stdout.contains("fidget"));
    assert!(!stdout.contains("relative_line_numbers = false"));
}

#[test]
fn print_config_names_an_unknown_key_and_suggests_a_close_match() {
    let home = tempfile::tempdir().unwrap();
    fs::create_dir_all(home.path().join("red")).unwrap();

    let output = red(
        home.path(),
        &[
            "--print-config",
            "-c",
            "themee=onedark",
            "-c",
            "scrolloff=4",
        ],
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !output.status.success(),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("-c/--config 'themee=onedark'"),
        "{combined}"
    );
    assert!(combined.contains("unknown key 'themee'"), "{combined}");
    assert!(combined.contains("did you mean 'theme'"), "{combined}");
    assert!(combined.contains("config override #1"), "{combined}");
}

#[test]
fn print_config_rejects_a_malformed_override() {
    let home = tempfile::tempdir().unwrap();
    fs::create_dir_all(home.path().join("red")).unwrap();

    let output = red(home.path(), &["--print-config", "--config", "nope"]);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("-c/--config 'nope'"), "{stderr}");
    assert!(stderr.contains("key=value"), "{stderr}");
}
