use clap::Parser;

#[derive(Debug, Parser)]
pub struct Args {
    /// Root path
    #[clap(short, long)]
    pub root: Option<String>,

    /// Inline TOML config override. Can be provided multiple times.
    #[clap(short = 'c', long = "config-override", value_name = "TOML")]
    pub config_overrides: Vec<String>,

    /// Files to edit
    pub files: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
