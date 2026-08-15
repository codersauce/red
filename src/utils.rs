//! Small path helpers shared by configuration, buffers, and workspace-aware features.
//!
//! User-home expansion is explicit and fallible. Workspace discovery prefers the Git
//! root containing the current directory and falls back to the current directory, so it
//! is a convenience boundary rather than a security boundary for untrusted paths.

use std::{
    env,
    path::{Path, PathBuf},
};

use path_absolutize::Absolutize;

pub fn expand_user_path(path: &str) -> anyhow::Result<PathBuf> {
    let Some(home) = home_dir() else {
        return Err(anyhow::anyhow!("home directory not found"));
    };

    expand_user_path_with_home(path, &home)
}

/// Expands a user path and makes it absolute without resolving symlinks.
///
/// Keeping the lexical path preserves the spelling the user opened while giving file
/// buffers a stable identity across workspace-relative and absolute open requests.
pub fn normalized_file_path(path: &str) -> anyhow::Result<PathBuf> {
    Ok(expand_user_path(path)?.absolutize()?.into_owned())
}

/// Returns whether two paths identify the same file.
///
/// Lexical equality also covers files that do not exist yet. When both paths exist,
/// filesystem identity additionally folds symlink, hard-link, and case aliases.
pub fn same_file_path(left: &Path, right: &Path) -> bool {
    let Ok(left) = left.absolutize() else {
        return false;
    };
    let Ok(right) = right.absolutize() else {
        return false;
    };

    left == right || same_file::is_same_file(left.as_ref(), right.as_ref()).unwrap_or(false)
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
    fn normalizes_relative_file_paths_without_resolving_symlinks() {
        let relative = PathBuf::from("src/main.rs");

        assert_eq!(
            normalized_file_path(relative.to_str().unwrap()).unwrap(),
            std::env::current_dir().unwrap().join(relative)
        );
    }

    #[test]
    fn matches_relative_and_absolute_file_paths() {
        let relative = Path::new("src/main.rs");
        let absolute = std::env::current_dir().unwrap().join(relative);

        assert!(same_file_path(relative, &absolute));
    }

    #[cfg(unix)]
    #[test]
    fn matches_symlink_aliases_without_rewriting_the_path() {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("original.rs");
        let alias = directory.path().join("alias.rs");
        std::fs::write(&original, "fn main() {}\n").unwrap();
        std::os::unix::fs::symlink(&original, &alias).unwrap();

        assert!(same_file_path(&original, &alias));
        assert_eq!(
            normalized_file_path(alias.to_str().unwrap()).unwrap(),
            alias
        );
    }

    #[test]
    fn rejects_other_users_home_syntax() {
        let home = PathBuf::from("/tmp/red-home");

        assert!(expand_user_path_with_home("~other/config.toml", &home).is_err());
    }
}
