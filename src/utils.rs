//! Small path helpers shared by configuration, buffers, and workspace-aware features.
//!
//! User-home expansion is explicit and fallible. Workspace discovery prefers the Git
//! root containing the current directory and falls back to the current directory, so it
//! is a convenience boundary rather than a security boundary for untrusted paths.

use std::{
    env,
    path::{Path, PathBuf},
};

pub fn expand_user_path(path: &str) -> anyhow::Result<PathBuf> {
    let Some(home) = home_dir() else {
        return Err(anyhow::anyhow!("home directory not found"));
    };

    expand_user_path_with_home(path, &home)
}

pub fn expand_user_path_with_home(path: &str, home: &Path) -> anyhow::Result<PathBuf> {
    if path == "~" {
        return Ok(home.to_path_buf());
    }

    if let Some(rest) = path.strip_prefix("~/") {
        let mut path = home.to_path_buf();
        for component in rest.split('/').filter(|component| !component.is_empty()) {
            path.push(component);
        }
        return Ok(path);
    }

    if path.starts_with('~') {
        return Err(anyhow::anyhow!("unsupported home path {:?}", path));
    }

    Ok(PathBuf::from(path))
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
}

/// Get the current working directory
pub fn get_workspace_path() -> PathBuf {
    env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// Convert to URI format (file:///path/to/workspace)
pub fn get_workspace_uri() -> String {
    format!("file://{}", get_workspace_path().display()).replace("\\", "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_bare_tilde_to_home() {
        let home = PathBuf::from("/tmp/red-home");

        assert_eq!(expand_user_path_with_home("~", &home).unwrap(), home);
    }

    #[test]
    fn expands_tilde_slash_under_home() {
        let home = PathBuf::from("/tmp/red-home");

        assert_eq!(
            expand_user_path_with_home("~/config.toml", &home).unwrap(),
            home.join("config.toml")
        );
        assert_eq!(
            expand_user_path_with_home("~/nested/config.toml", &home).unwrap(),
            home.join("nested").join("config.toml")
        );
    }

    #[test]
    fn leaves_non_tilde_paths_unchanged() {
        let home = PathBuf::from("/tmp/red-home");

        assert_eq!(
            expand_user_path_with_home("src/main.rs", &home).unwrap(),
            PathBuf::from("src/main.rs")
        );
    }

    #[test]
    fn rejects_other_users_home_syntax() {
        let home = PathBuf::from("/tmp/red-home");

        assert!(expand_user_path_with_home("~other/config.toml", &home).is_err());
    }
}
