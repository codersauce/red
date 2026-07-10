use clap::Parser;

#[derive(Debug, Parser)]
#[command(version)]
pub struct Args {
    /// Root path
    #[clap(short, long)]
    pub root: Option<String>,

    /// Inline TOML config override. Can be provided multiple times.
    #[clap(short = 'c', long = "config-override", value_name = "TOML")]
    pub config_overrides: Vec<String>,

    /// List runtime files from user config, $RED_RUNTIME, and embedded assets.
    #[clap(long = "runtime-files")]
    pub runtime_files: bool,

    /// Validate the embedded runtime and assets, then exit.
    #[clap(long = "self-check", hide = true)]
    pub self_check: bool,

    /// Report ACP adapter, authentication, and protocol prerequisites without installing anything.
    #[clap(long = "agent-check")]
    pub agent_check: bool,

    /// Replace an editor target with RED_PROCESS_EDITOR_CONTENT and exit.
    #[clap(long = "process-editor-replace", hide = true)]
    pub process_editor_replace: bool,

    /// Copy a bundled/runtime asset into the user config directory for editing.
    /// Accepts `plugins/name.hk`, `themes/name.json`, or a bare plugin/theme file name.
    #[clap(long = "eject", value_name = "ASSET", conflicts_with = "eject_force")]
    pub eject: Option<String>,

    /// Copy a bundled/runtime asset into the user config directory, overwriting an existing user file.
    /// Accepts `plugins/name.hk`, `themes/name.json`, or a bare plugin/theme file name.
    #[clap(long = "eject-force", value_name = "ASSET")]
    pub eject_force: Option<String>,

    /// Files to edit
    pub files: Vec<String>,
}

impl Args {
    pub fn utility_requested(&self) -> bool {
        self.self_check
            || self.agent_check
            || self.runtime_files
            || self.eject.is_some()
            || self.eject_force.is_some()
            || self.process_editor_replace
    }

    pub fn validate_utility_args(&self) -> anyhow::Result<()> {
        if self.process_editor_replace {
            anyhow::ensure!(
                self.files.len() == 1,
                "process editor requires exactly one target file"
            );
        } else if self.utility_requested() && !self.files.is_empty() {
            anyhow::bail!("runtime utility flags cannot be used with files to edit");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    #[test]
    fn parses_repeated_config_overrides() {
        let args = Args::try_parse_from([
            "red",
            "-c",
            r#"theme = "nightfox.json""#,
            "--config-override",
            r#"keys.normal."Ctrl-t" = { PluginCommand = "LspDocumentSymbols" }"#,
            "src/editor.rs",
        ])
        .unwrap();

        assert_eq!(args.config_overrides.len(), 2);
        assert_eq!(args.files, vec!["src/editor.rs"]);
    }

    #[test]
    fn parses_runtime_utility_flags() {
        let args = Args::try_parse_from(["red", "--runtime-files"]).unwrap();
        assert!(args.runtime_files);
        assert!(args.utility_requested());

        let args = Args::try_parse_from(["red", "--self-check"]).unwrap();
        assert!(args.self_check);
        assert!(args.utility_requested());

        let args = Args::try_parse_from(["red", "--agent-check"]).unwrap();
        assert!(args.agent_check);
        assert!(args.utility_requested());

        let args = Args::try_parse_from(["red", "--eject", "plugins/fidget.hk"]).unwrap();
        assert_eq!(args.eject.as_deref(), Some("plugins/fidget.hk"));

        let args = Args::try_parse_from(["red", "--eject-force", "themes/mocha.json"]).unwrap();
        assert_eq!(args.eject_force.as_deref(), Some("themes/mocha.json"));

        let args = Args::try_parse_from(["red", "--process-editor-replace", "todo"]).unwrap();
        assert!(args.process_editor_replace);
        assert!(args.validate_utility_args().is_ok());
    }

    #[test]
    fn utility_flags_reject_files_to_edit() {
        let args = Args::try_parse_from(["red", "--runtime-files", "src/main.rs"]).unwrap();
        assert!(args.validate_utility_args().is_err());

        let args = Args::try_parse_from(["red", "--self-check", "src/main.rs"]).unwrap();
        assert!(args.validate_utility_args().is_err());
    }

    #[test]
    fn parses_version_flag() {
        let err = Args::try_parse_from(["red", "--version"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::DisplayVersion);
    }
}
