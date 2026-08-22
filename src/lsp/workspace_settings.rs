//! Bounded imports of safe rust-analyzer options from repository-local VS Code settings.
//!
//! Workspace settings are applied to a cloned server configuration, so one
//! repository cannot change another repository's language-server behavior.

use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

use json_comments::StripComments;
use path_absolutize::Absolutize;
use serde_json::{Map, Value};

use crate::{config::LanguageServerConfig, log};

const MAX_SETTINGS_BYTES: u64 = 1024 * 1024;
const MAX_RUSTFMT_ARGS: usize = 64;
const MAX_RUSTFMT_ARG_BYTES: usize = 8 * 1024;
const RUSTFMT_EXTRA_ARGS_KEY: &str = "rust-analyzer.rustfmt.extraArgs";
const CARGO_TARGET_DIR_KEY: &str = "rust-analyzer.cargo.targetDir";
const CACHE_PRIMING_ENABLE_KEY: &str = "rust-analyzer.cachePriming.enable";
const CHECK_WORKSPACE_KEY: &str = "rust-analyzer.check.workspace";
const CARGO_ALL_TARGETS_KEY: &str = "rust-analyzer.cargo.allTargets";
const WORKSPACE_FOLDER_PLACEHOLDER: &str = "${workspaceFolder}";

/// Merge safe project-local rustfmt settings without changing shared defaults.
pub(super) fn apply_workspace_settings(
    config: &mut LanguageServerConfig,
    workspace_root: &Path,
    language_id: &str,
) {
    if language_id != "rust" {
        return;
    }

    let Some(path) = find_settings_file(workspace_root) else {
        return;
    };
    let settings = match read_settings_file(&path) {
        Ok(settings) => settings,
        Err(error) => {
            log!(
                "[lsp] ignored workspace settings {}: {error}",
                path.display()
            );
            return;
        }
    };
    if let Some(project_args) = rustfmt_extra_args(&settings) {
        let existing_args = existing_rustfmt_args(config).cloned();
        let args = existing_args.unwrap_or(project_args);
        merge_setting(
            config,
            &["rustfmt", "extraArgs"],
            &["rust-analyzer", "rustfmt", "extraArgs"],
            &args,
            &path,
            "rustfmt extra arguments",
        );
    }

    if let Some(target_dir) = cargo_target_dir(&settings, &path) {
        merge_project_setting(
            config,
            &["cargo", "targetDir"],
            &["rust-analyzer", "cargo", "targetDir"],
            CARGO_TARGET_DIR_KEY,
            target_dir,
            &path,
            "Cargo target directory",
        );
    }

    merge_project_bool(
        config,
        &settings,
        CACHE_PRIMING_ENABLE_KEY,
        &["rust-analyzer", "cachePriming", "enable"],
        &["cachePriming", "enable"],
        &path,
    );
    merge_project_bool(
        config,
        &settings,
        CHECK_WORKSPACE_KEY,
        &["rust-analyzer", "check", "workspace"],
        &["check", "workspace"],
        &path,
    );
    merge_project_bool(
        config,
        &settings,
        CARGO_ALL_TARGETS_KEY,
        &["rust-analyzer", "cargo", "allTargets"],
        &["cargo", "allTargets"],
        &path,
    );
}

/// Avoid eager whole-workspace priming unless the user or repository opted back in.
pub(super) fn apply_fast_startup_defaults(config: &mut LanguageServerConfig, language_id: &str) {
    if language_id != "rust" {
        return;
    }

    let value = existing_setting(
        config,
        &["cachePriming", "enable"],
        &["rust-analyzer", "cachePriming", "enable"],
        CACHE_PRIMING_ENABLE_KEY,
    )
    .cloned()
    .unwrap_or(Value::Bool(false));
    merge_setting(
        config,
        &["cachePriming", "enable"],
        &["rust-analyzer", "cachePriming", "enable"],
        &value,
        Path::new("built-in rust-analyzer defaults"),
        "cache priming",
    );
}

fn find_settings_file(workspace_root: &Path) -> Option<PathBuf> {
    let repository_root = workspace_root
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists());
    let settings_boundary = repository_root.unwrap_or(workspace_root);

    for ancestor in workspace_root.ancestors() {
        let candidate = ancestor.join(".vscode").join("settings.json");
        if candidate.is_file() {
            let canonical_boundary = fs::canonicalize(settings_boundary).ok()?;
            let canonical_candidate = fs::canonicalize(&candidate).ok()?;
            if !canonical_candidate.starts_with(canonical_boundary) {
                log!(
                    "[lsp] ignored workspace settings outside the workspace boundary: {}",
                    candidate.display()
                );
                return None;
            }
            return Some(candidate);
        }

        if repository_root.is_none() || repository_root == Some(ancestor) {
            break;
        }
    }

    None
}

fn read_settings_file(path: &Path) -> io::Result<Value> {
    let file = fs::File::open(path)?;
    if file.metadata()?.len() > MAX_SETTINGS_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "settings file exceeds the size limit",
        ));
    }

    let mut contents = Vec::new();
    file.take(MAX_SETTINGS_BYTES + 1)
        .read_to_end(&mut contents)?;
    if contents.len() as u64 > MAX_SETTINGS_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "settings file exceeds the size limit",
        ));
    }

    let mut uncommented = String::new();
    StripComments::new(contents.as_slice()).read_to_string(&mut uncommented)?;
    let normalized = strip_trailing_commas(&uncommented);
    serde_json::from_str(&normalized)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn strip_trailing_commas(contents: &str) -> String {
    let mut normalized = String::with_capacity(contents.len());
    let mut in_string = false;
    let mut escaped = false;

    for (index, character) in contents.char_indices() {
        if in_string {
            normalized.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }

        if character == '"' {
            in_string = true;
        } else if character == ',' {
            let next = contents[index + character.len_utf8()..]
                .trim_start()
                .chars()
                .next();
            if matches!(next, Some('}' | ']')) {
                continue;
            }
        }
        normalized.push(character);
    }

    normalized
}

fn rustfmt_extra_args(settings: &Value) -> Option<Value> {
    let args = settings
        .get(RUSTFMT_EXTRA_ARGS_KEY)
        .or_else(|| {
            settings
                .get("rust-analyzer")?
                .get("rustfmt")?
                .get("extraArgs")
        })?
        .as_array()?;
    if args.len() > MAX_RUSTFMT_ARGS {
        return None;
    }

    let mut total_bytes = 0usize;
    for arg in args {
        let arg = arg.as_str()?;
        if arg.contains('\0') {
            return None;
        }
        total_bytes = total_bytes.checked_add(arg.len())?;
        if total_bytes > MAX_RUSTFMT_ARG_BYTES {
            return None;
        }
    }

    Some(Value::Array(args.clone()))
}

fn cargo_target_dir(settings: &Value, settings_path: &Path) -> Option<Value> {
    let raw = project_setting(
        settings,
        CARGO_TARGET_DIR_KEY,
        &["rust-analyzer", "cargo", "targetDir"],
    )?
    .as_str()?;
    if raw.is_empty() || raw.contains('\0') {
        return None;
    }

    let workspace_folder = settings_path.parent()?.parent()?;
    let relative = if let Some(suffix) = raw.strip_prefix(WORKSPACE_FOLDER_PLACEHOLDER) {
        suffix.strip_prefix('/')?
    } else {
        if raw.contains("${") {
            return None;
        }
        raw
    };
    if relative.is_empty() {
        return None;
    }

    let workspace_folder = workspace_folder.absolutize().ok()?.to_path_buf();
    let target = Path::new(relative)
        .absolutize_from(&workspace_folder)
        .ok()?
        .to_path_buf();
    if !target.starts_with(&workspace_folder) || target == workspace_folder {
        return None;
    }

    Some(Value::String(target.to_string_lossy().into_owned()))
}

fn project_setting<'a>(settings: &'a Value, flat: &str, nested: &[&str]) -> Option<&'a Value> {
    settings
        .get(flat)
        .or_else(|| value_at_path(settings, nested))
}

fn merge_project_bool(
    config: &mut LanguageServerConfig,
    settings: &Value,
    flat: &str,
    dynamic_path: &[&str],
    initialization_path: &[&str],
    settings_path: &Path,
) {
    let Some(value) =
        project_setting(settings, flat, dynamic_path).filter(|value| value.is_boolean())
    else {
        return;
    };
    merge_project_setting(
        config,
        initialization_path,
        dynamic_path,
        flat,
        value.clone(),
        settings_path,
        flat,
    );
}

fn merge_project_setting(
    config: &mut LanguageServerConfig,
    initialization_path: &[&str],
    dynamic_path: &[&str],
    flat: &str,
    project_value: Value,
    settings_path: &Path,
    label: &str,
) {
    let value = existing_setting(config, initialization_path, dynamic_path, flat)
        .cloned()
        .unwrap_or(project_value);
    merge_setting(
        config,
        initialization_path,
        dynamic_path,
        &value,
        settings_path,
        label,
    );
}

fn merge_setting(
    config: &mut LanguageServerConfig,
    initialization_path: &[&str],
    dynamic_path: &[&str],
    value: &Value,
    source: &Path,
    label: &str,
) {
    if !insert_missing(
        &mut config.initialization_options,
        initialization_path,
        value,
    ) {
        log!(
            "[lsp] could not merge {label} into initialization options from {}",
            source.display()
        );
    }
    if !insert_missing(&mut config.settings, dynamic_path, value) {
        log!(
            "[lsp] could not merge {label} into dynamic settings from {}",
            source.display()
        );
    }
}

fn existing_setting<'a>(
    config: &'a LanguageServerConfig,
    initialization_path: &[&str],
    dynamic_path: &[&str],
    flat: &str,
) -> Option<&'a Value> {
    config
        .initialization_options
        .as_ref()
        .and_then(|value| value_at_path(value, initialization_path))
        .or_else(|| {
            config
                .settings
                .as_ref()
                .and_then(|value| value_at_path(value, dynamic_path))
        })
        .or_else(|| config.settings.as_ref().and_then(|value| value.get(flat)))
}

fn value_at_path<'a>(mut value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    for part in path {
        value = value.get(*part)?;
    }
    Some(value)
}

fn existing_rustfmt_args(config: &LanguageServerConfig) -> Option<&Value> {
    config
        .initialization_options
        .as_ref()
        .and_then(|options| options.get("rustfmt")?.get("extraArgs"))
        .or_else(|| {
            config
                .settings
                .as_ref()?
                .get("rust-analyzer")?
                .get("rustfmt")?
                .get("extraArgs")
        })
        .or_else(|| config.settings.as_ref()?.get(RUSTFMT_EXTRA_ARGS_KEY))
}

fn insert_missing(root: &mut Option<Value>, path: &[&str], value: &Value) -> bool {
    let Some((last, parents)) = path.split_last() else {
        return false;
    };
    let mut current = root.get_or_insert_with(|| Value::Object(Map::new()));
    for part in parents {
        let Some(object) = current.as_object_mut() else {
            return false;
        };
        current = object
            .entry((*part).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    let Some(object) = current.as_object_mut() else {
        return false;
    };
    object
        .entry((*last).to_string())
        .or_insert_with(|| value.clone());
    true
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, time::Duration};

    use serde_json::json;
    use tempfile::TempDir;

    use crate::{
        config::LspConfig,
        lsp::{
            apply_text_edits, InboundMessage, LspClient, LspManager, OutboundMessage,
            RealLspClient, ServerRequest, TextEdit,
        },
    };

    use super::*;

    fn rust_server() -> LanguageServerConfig {
        LanguageServerConfig {
            command: "rustup".to_string(),
            args: vec![
                "run".to_string(),
                "stable".to_string(),
                "rust-analyzer".to_string(),
            ],
            language_id: "rust".to_string(),
            file_extensions: vec!["rs".to_string()],
            filenames: Vec::new(),
            documents: Vec::new(),
            root_markers: vec!["Cargo.toml".to_string(), ".git".to_string()],
            env: HashMap::new(),
            initialization_options: None,
            settings: None,
            workspace_name: None,
        }
    }

    fn repository() -> (TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(".git")).unwrap();
        let workspace = root.path().join("codex-rs/core");
        fs::create_dir_all(&workspace).unwrap();
        (root, workspace)
    }

    fn write_settings(root: &Path, settings: &str) -> PathBuf {
        let directory = root.join(".vscode");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("settings.json");
        fs::write(&path, settings).unwrap();
        path
    }

    fn assert_args(config: &LanguageServerConfig, expected: Value) {
        assert_eq!(
            config.initialization_options.as_ref().unwrap()["rustfmt"]["extraArgs"],
            expected
        );
        assert_eq!(
            config.settings.as_ref().unwrap()["rust-analyzer"]["rustfmt"]["extraArgs"],
            expected
        );
    }

    #[test]
    fn loads_git_root_settings_for_nested_cargo_workspaces() {
        let (repository, workspace) = repository();
        write_settings(
            repository.path(),
            r#"{
                // VS Code settings are JSONC, not strict JSON.
                "rust-analyzer.rustfmt.extraArgs": [
                    "--config",
                    "imports_granularity=Item",
                ],
                "[rust]": { "editor.formatOnSave": true, },
            }"#,
        );

        let mut config = rust_server();
        apply_workspace_settings(&mut config, &workspace, "rust");

        assert_args(&config, json!(["--config", "imports_granularity=Item"]));
    }

    #[test]
    fn recognizes_linked_worktree_git_files() {
        let (repository, workspace) = repository();
        fs::remove_dir(repository.path().join(".git")).unwrap();
        fs::write(repository.path().join(".git"), "gitdir: /elsewhere").unwrap();
        write_settings(
            repository.path(),
            r#"{"rust-analyzer.rustfmt.extraArgs":["--edition", "2024"]}"#,
        );

        let mut config = rust_server();
        apply_workspace_settings(&mut config, &workspace, "rust");

        assert_args(&config, json!(["--edition", "2024"]));
    }

    #[test]
    fn nearest_workspace_settings_take_precedence() {
        let (repository, workspace) = repository();
        write_settings(
            repository.path(),
            r#"{"rust-analyzer.rustfmt.extraArgs":["--edition", "2021"]}"#,
        );
        write_settings(
            workspace.parent().unwrap(),
            r#"{"rust-analyzer.rustfmt.extraArgs":["--edition", "2024"]}"#,
        );

        let mut config = rust_server();
        apply_workspace_settings(&mut config, &workspace, "rust");

        assert_args(&config, json!(["--edition", "2024"]));
    }

    #[test]
    fn does_not_search_beyond_the_git_repository() {
        let parent = tempfile::tempdir().unwrap();
        write_settings(
            parent.path(),
            r#"{"rust-analyzer.rustfmt.extraArgs":["--edition", "2024"]}"#,
        );
        let repository = parent.path().join("project");
        let workspace = repository.join("crate");
        fs::create_dir_all(repository.join(".git")).unwrap();
        fs::create_dir_all(&workspace).unwrap();

        let mut config = rust_server();
        apply_workspace_settings(&mut config, &workspace, "rust");

        assert!(config.initialization_options.is_none());
        assert!(config.settings.is_none());
    }

    #[test]
    fn does_not_search_parent_directories_without_a_git_boundary() {
        let parent = tempfile::tempdir().unwrap();
        write_settings(
            parent.path(),
            r#"{"rust-analyzer.rustfmt.extraArgs":["--edition", "2024"]}"#,
        );
        let workspace = parent.path().join("crate");
        fs::create_dir(&workspace).unwrap();

        let mut config = rust_server();
        apply_workspace_settings(&mut config, &workspace, "rust");

        assert!(config.initialization_options.is_none());
    }

    #[test]
    fn accepts_workspace_root_settings_without_git() {
        let root = tempfile::tempdir().unwrap();
        write_settings(
            root.path(),
            r#"{"rust-analyzer.rustfmt.extraArgs":["--edition", "2024"]}"#,
        );

        let mut config = rust_server();
        apply_workspace_settings(&mut config, root.path(), "rust");

        assert_args(&config, json!(["--edition", "2024"]));
    }

    #[test]
    fn jsonc_normalization_preserves_strings_and_escaped_quotes() {
        let (repository, workspace) = repository();
        write_settings(
            repository.path(),
            r#"{
                "ignored.url": "https://example.test/path/,]",
                "ignored.quote": "escaped \\\" ,} stays in a string",
                /* Comments and trailing commas are both legal in VS Code. */
                "rust-analyzer.rustfmt.extraArgs": ["--config", "imports_granularity=Item",],
            }"#,
        );

        let mut config = rust_server();
        apply_workspace_settings(&mut config, &workspace, "rust");

        assert_args(&config, json!(["--config", "imports_granularity=Item"]));
    }

    #[test]
    fn accepts_nested_rust_analyzer_settings() {
        let (repository, workspace) = repository();
        write_settings(
            repository.path(),
            r#"{"rust-analyzer":{"rustfmt":{"extraArgs":["--edition","2024"]}}}"#,
        );

        let mut config = rust_server();
        apply_workspace_settings(&mut config, &workspace, "rust");

        assert_args(&config, json!(["--edition", "2024"]));
    }

    #[test]
    fn preserves_explicit_initialization_options_and_other_settings() {
        let (repository, workspace) = repository();
        write_settings(
            repository.path(),
            r#"{"rust-analyzer.rustfmt.extraArgs":["--edition","2024"]}"#,
        );
        let mut config = rust_server();
        config.initialization_options = Some(json!({
            "cargo": { "allFeatures": true },
            "rustfmt": { "extraArgs": ["--edition", "2021"] },
        }));
        config.settings = Some(json!({ "unrelated": true }));

        apply_workspace_settings(&mut config, &workspace, "rust");

        assert_args(&config, json!(["--edition", "2021"]));
        assert_eq!(
            config.initialization_options.as_ref().unwrap()["cargo"]["allFeatures"],
            json!(true)
        );
        assert_eq!(config.settings.as_ref().unwrap()["unrelated"], json!(true));
    }

    #[test]
    fn preserves_explicit_dynamic_settings() {
        let (repository, workspace) = repository();
        write_settings(
            repository.path(),
            r#"{"rust-analyzer.rustfmt.extraArgs":["--edition","2024"]}"#,
        );
        let mut config = rust_server();
        config.settings = Some(json!({
            "rust-analyzer": { "rustfmt": { "extraArgs": ["--edition", "2021"] } }
        }));

        apply_workspace_settings(&mut config, &workspace, "rust");

        assert_args(&config, json!(["--edition", "2021"]));
    }

    #[test]
    fn imports_repository_local_target_dir_and_workload_settings() {
        let (repository, workspace) = repository();
        write_settings(
            repository.path(),
            r#"{
                "rust-analyzer.cargo.targetDir": "${workspaceFolder}/codex-rs/target/rust-analyzer",
                "rust-analyzer.cachePriming.enable": true,
                "rust-analyzer.check.workspace": false,
                "rust-analyzer.cargo.allTargets": false
            }"#,
        );

        let mut config = rust_server();
        apply_workspace_settings(&mut config, &workspace, "rust");

        let target = repository
            .path()
            .join("codex-rs/target/rust-analyzer")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            config.initialization_options,
            Some(json!({
                "cargo": { "targetDir": target, "allTargets": false },
                "cachePriming": { "enable": true },
                "check": { "workspace": false },
            }))
        );
        assert_eq!(
            config.settings,
            Some(json!({
                "rust-analyzer": {
                    "cargo": { "targetDir": target, "allTargets": false },
                    "cachePriming": { "enable": true },
                    "check": { "workspace": false },
                }
            }))
        );
    }

    #[test]
    fn rejects_target_dirs_outside_the_settings_workspace() {
        for target_dir in ["../outside", "/tmp/outside", "${workspaceFolder}"] {
            let (repository, workspace) = repository();
            write_settings(
                repository.path(),
                &json!({ CARGO_TARGET_DIR_KEY: target_dir }).to_string(),
            );
            let mut config = rust_server();

            apply_workspace_settings(&mut config, &workspace, "rust");

            assert!(config.initialization_options.is_none(), "{target_dir}");
            assert!(config.settings.is_none(), "{target_dir}");
        }
    }

    #[test]
    fn explicit_target_dir_and_workload_settings_take_precedence() {
        let (repository, workspace) = repository();
        write_settings(
            repository.path(),
            r#"{
                "rust-analyzer.cargo.targetDir": "target/project",
                "rust-analyzer.cachePriming.enable": false
            }"#,
        );
        let mut config = rust_server();
        config.initialization_options = Some(json!({
            "cargo": { "targetDir": "/trusted/user/target" },
            "cachePriming": { "enable": true },
        }));

        apply_workspace_settings(&mut config, &workspace, "rust");

        assert_eq!(
            config.initialization_options.as_ref().unwrap()["cargo"]["targetDir"],
            json!("/trusted/user/target")
        );
        assert_eq!(
            config.initialization_options.as_ref().unwrap()["cachePriming"]["enable"],
            json!(true)
        );
        assert_eq!(
            config.settings.as_ref().unwrap()["rust-analyzer"]["cargo"]["targetDir"],
            json!("/trusted/user/target")
        );
        assert_eq!(
            config.settings.as_ref().unwrap()["rust-analyzer"]["cachePriming"]["enable"],
            json!(true)
        );
    }

    #[test]
    fn fast_startup_disables_cache_priming_without_overriding_explicit_values() {
        let mut config = rust_server();
        apply_fast_startup_defaults(&mut config, "rust");
        assert_eq!(
            config.initialization_options.as_ref().unwrap()["cachePriming"]["enable"],
            json!(false)
        );
        assert_eq!(
            config.settings.as_ref().unwrap()["rust-analyzer"]["cachePriming"]["enable"],
            json!(false)
        );

        let mut explicit = rust_server();
        explicit.initialization_options = Some(json!({
            "cachePriming": { "enable": true }
        }));
        apply_fast_startup_defaults(&mut explicit, "rust");
        assert_eq!(
            explicit.initialization_options.as_ref().unwrap()["cachePriming"]["enable"],
            json!(true)
        );
        assert_eq!(
            explicit.settings.as_ref().unwrap()["rust-analyzer"]["cachePriming"]["enable"],
            json!(true)
        );
    }

    #[test]
    fn ignores_non_rust_documents() {
        let (repository, workspace) = repository();
        write_settings(
            repository.path(),
            r#"{"rust-analyzer.rustfmt.extraArgs":["--edition","2024"]}"#,
        );

        let mut config = rust_server();
        apply_workspace_settings(&mut config, &workspace, "python");

        assert!(config.initialization_options.is_none());
        assert!(config.settings.is_none());
    }

    #[test]
    fn ignores_unsafe_and_unrelated_workspace_settings() {
        let (repository, workspace) = repository();
        write_settings(
            repository.path(),
            r#"{
                "rust-analyzer.rustfmt.extraArgs": ["--edition", "2024"],
                "rust-analyzer.rustfmt.overrideCommand": ["/tmp/evil"],
                "rust-analyzer.server.path": "/tmp/evil",
                "rust-analyzer.cargo.extraEnv": { "RUSTC_WRAPPER": "/tmp/evil" }
            }"#,
        );

        let mut config = rust_server();
        apply_workspace_settings(&mut config, &workspace, "rust");

        assert_eq!(
            config.initialization_options,
            Some(json!({ "rustfmt": { "extraArgs": ["--edition", "2024"] } }))
        );
        assert_eq!(
            config.settings,
            Some(json!({ "rust-analyzer": { "rustfmt": { "extraArgs": ["--edition", "2024"] } } }))
        );
    }

    #[test]
    fn rejects_malformed_and_invalid_argument_shapes() {
        for settings in [
            "{this is not json}",
            r#"{"rust-analyzer.rustfmt.extraArgs":"--edition"}"#,
            r#"{"rust-analyzer.rustfmt.extraArgs":["--edition",42]}"#,
            r#"{"rust-analyzer.rustfmt.extraArgs":["bad\u0000arg"]}"#,
        ] {
            let (repository, workspace) = repository();
            write_settings(repository.path(), settings);

            let mut config = rust_server();
            apply_workspace_settings(&mut config, &workspace, "rust");

            assert!(config.initialization_options.is_none(), "{settings}");
            assert!(config.settings.is_none(), "{settings}");
        }
    }

    #[test]
    fn rejects_oversized_settings_and_argument_lists() {
        let (repository, workspace) = repository();
        let path = write_settings(repository.path(), "");
        fs::write(&path, vec![b' '; MAX_SETTINGS_BYTES as usize + 1]).unwrap();

        let mut config = rust_server();
        apply_workspace_settings(&mut config, &workspace, "rust");
        assert!(config.initialization_options.is_none());

        let args = vec!["--edition"; MAX_RUSTFMT_ARGS + 1];
        fs::write(&path, json!({ RUSTFMT_EXTRA_ARGS_KEY: args }).to_string()).unwrap();
        apply_workspace_settings(&mut config, &workspace, "rust");
        assert!(config.initialization_options.is_none());

        let large_arg = "x".repeat(MAX_RUSTFMT_ARG_BYTES + 1);
        fs::write(
            &path,
            json!({ RUSTFMT_EXTRA_ARGS_KEY: [large_arg] }).to_string(),
        )
        .unwrap();
        apply_workspace_settings(&mut config, &workspace, "rust");
        assert!(config.initialization_options.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_settings_symlinked_outside_the_repository() {
        let (repository, workspace) = repository();
        let outside = tempfile::tempdir().unwrap();
        let external = outside.path().join("settings.json");
        fs::write(
            &external,
            r#"{"rust-analyzer.rustfmt.extraArgs":["--edition","2024"]}"#,
        )
        .unwrap();
        let vscode = repository.path().join(".vscode");
        fs::create_dir(&vscode).unwrap();
        std::os::unix::fs::symlink(external, vscode.join("settings.json")).unwrap();

        let mut config = rust_server();
        apply_workspace_settings(&mut config, &workspace, "rust");

        assert!(config.initialization_options.is_none());
        assert!(config.settings.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_settings_symlinked_outside_a_workspace_without_git() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let external = outside.path().join("settings.json");
        fs::write(
            &external,
            r#"{"rust-analyzer.cargo.targetDir":"target/rust-analyzer"}"#,
        )
        .unwrap();
        let vscode = workspace.path().join(".vscode");
        fs::create_dir(&vscode).unwrap();
        std::os::unix::fs::symlink(external, vscode.join("settings.json")).unwrap();
        let mut config = rust_server();

        apply_workspace_settings(&mut config, workspace.path(), "rust");

        assert!(config.initialization_options.is_none());
        assert!(config.settings.is_none());
    }

    #[test]
    fn settings_from_one_repository_do_not_leak_into_another() {
        let (first_repository, first_workspace) = repository();
        write_settings(
            first_repository.path(),
            r#"{"rust-analyzer.rustfmt.extraArgs":["--edition","2024"]}"#,
        );
        let (_second_repository, second_workspace) = repository();
        let global = rust_server();

        let mut first = global.clone();
        apply_workspace_settings(&mut first, &first_workspace, "rust");
        let mut second = global.clone();
        apply_workspace_settings(&mut second, &second_workspace, "rust");

        assert_args(&first, json!(["--edition", "2024"]));
        assert!(second.initialization_options.is_none());
        assert!(second.settings.is_none());
        assert!(global.initialization_options.is_none());
    }

    #[tokio::test]
    async fn project_settings_reach_initialize_and_workspace_configuration() {
        let (repository, workspace) = repository();
        write_settings(
            repository.path(),
            r#"{"rust-analyzer.rustfmt.extraArgs":["--config","imports_granularity=Item"]}"#,
        );
        let mut config = rust_server();
        apply_workspace_settings(&mut config, &workspace, "rust");

        let (request_tx, mut request_rx) = tokio::sync::mpsc::channel(2);
        let (response_tx, response_rx) = tokio::sync::mpsc::channel(1);
        let mut client =
            RealLspClient::with_test_channels(request_tx, response_rx, config, workspace);

        client.initialize().await.unwrap();
        let Some(OutboundMessage::Request(request)) = request_rx.recv().await else {
            panic!("expected initialize request");
        };
        assert_eq!(request.method, "initialize");
        assert_eq!(
            request.params["initializationOptions"]["rustfmt"]["extraArgs"],
            json!(["--config", "imports_granularity=Item"])
        );

        response_tx
            .send(InboundMessage::ServerRequest(ServerRequest {
                id: json!(42),
                method: "workspace/configuration".to_string(),
                params: json!({
                    "items": [
                        { "section": "rust-analyzer.rustfmt.extraArgs" },
                        { "section": "rust-analyzer" },
                    ]
                }),
                source: None,
            }))
            .await
            .unwrap();
        assert!(client.recv_response().await.unwrap().is_none());
        let Some(OutboundMessage::Response(response)) = request_rx.recv().await else {
            panic!("expected workspace configuration response");
        };
        assert_eq!(
            response.result,
            Some(json!([
                ["--config", "imports_granularity=Item"],
                { "rustfmt": { "extraArgs": ["--config", "imports_granularity=Item"] } },
            ]))
        );
    }

    #[tokio::test]
    async fn real_rust_analyzer_formats_nested_workspace_with_project_settings() {
        if std::env::var_os("RED_RUN_REAL_LSP_TESTS").is_none() {
            return;
        }

        let (repository, member) = repository();
        let workspace = member.parent().unwrap();
        fs::write(
            workspace.join("Cargo.toml"),
            "[workspace]\nmembers = [\"core\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        fs::write(
            member.join("Cargo.toml"),
            "[package]\nname = \"format_probe\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::write(
            workspace.join("rustfmt.toml"),
            "edition = \"2024\"\nimports_granularity = \"Item\"\n",
        )
        .unwrap();
        write_settings(
            repository.path(),
            r#"{
                // Keep each imported item on its own line.
                "rust-analyzer.rustfmt.extraArgs": ["--config", "imports_granularity=Item",],
            }"#,
        );

        let source = "use std::{fmt::Display, io::Write};\n\nfn main() {}\n";
        let source_directory = member.join("src");
        fs::create_dir(&source_directory).unwrap();
        let file = source_directory.join("main.rs");
        fs::write(&file, source).unwrap();
        let file = file.to_string_lossy().into_owned();
        let mut manager = LspManager::new(LspConfig {
            enabled: true,
            format_on_save: true,
            servers: HashMap::from([("rust".to_string(), rust_server())]),
        });

        manager.did_open(&file, source).await.unwrap();
        let request_id = manager
            .format_document_with_options(&file, 4, true)
            .await
            .unwrap();
        assert_ne!(request_id, 0);

        let formatted = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Some((message, method)) = manager.recv_response().await.unwrap() {
                    match message {
                        InboundMessage::Message(response)
                            if method.as_deref() == Some("textDocument/formatting") =>
                        {
                            let edits: Vec<TextEdit> =
                                serde_json::from_value(response.result).unwrap();
                            break apply_text_edits(source, &edits).unwrap();
                        }
                        InboundMessage::ProcessingError(error) => {
                            panic!("rust-analyzer failed: {error}");
                        }
                        InboundMessage::RequestError { error, .. } => {
                            panic!("rust-analyzer rejected the formatting request: {error}");
                        }
                        _ => {}
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("rust-analyzer must return a formatting response");

        assert_eq!(
            formatted,
            "use std::fmt::Display;\nuse std::io::Write;\n\nfn main() {}\n"
        );
        manager.shutdown().await.unwrap();
    }
}
