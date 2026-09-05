use std::process::Command;

#[test]
fn first_launch_creates_config_directory_before_opening_default_log() {
    let config_home = tempfile::tempdir().unwrap();
    let config_dir = config_home.path().join("red");

    // A non-terminal run stops at raw-mode setup, after runtime config has loaded.
    Command::new(env!("CARGO_BIN_EXE_red"))
        .env("XDG_CONFIG_HOME", config_home.path())
        .output()
        .unwrap();

    assert!(config_dir.join("red.log").is_file());
    assert!(!config_dir.join("config.toml").exists());
}
