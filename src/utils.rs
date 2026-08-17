//! Small path helpers shared by configuration, buffers, and workspace-aware features.
//!
//! User-home expansion is explicit and fallible. Workspace discovery prefers the Git
//! root containing the current directory and falls back to the current directory, so it
//! is a convenience boundary rather than a security boundary for untrusted paths.

use std::{
    env,
    path::{Component, Path, PathBuf},
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

/// Formats a file path as breadcrumb labels without changing its identity.
/// Working-directory-relative paths take precedence over home abbreviation.
/// This is lexical: missing files and symlink spellings are preserved.
pub(crate) fn breadcrumb_path_components(
    path: &Path,
    cwd: &Path,
    home: Option<&Path>,
) -> Vec<String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    if cwd.is_absolute() {
        if let Some(relative) = strip_display_prefix(&absolute, cwd) {
            return display_components(relative);
        }
    }
    if let Some(relative) = home
        .filter(|home| home.is_absolute())
        .and_then(|home| strip_display_prefix(&absolute, home))
    {
        let mut parts = vec!["~".to_string()];
        parts.extend(display_components(relative));
        return parts;
    }
    display_components(&absolute)
}

fn strip_display_prefix<'a>(path: &'a Path, base: &Path) -> Option<&'a Path> {
    let mut components = path.components();
    for expected in base.components() {
        let actual = components.next()?;
        #[cfg(windows)]
        let matches = actual
            .as_os_str()
            .as_encoded_bytes()
            .eq_ignore_ascii_case(expected.as_os_str().as_encoded_bytes());
        #[cfg(not(windows))]
        let matches = actual == expected;
        if !matches {
            return None;
        }
    }
    Some(components.as_path())
}

fn display_components(path: &Path) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut has_prefix = false;
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Prefix(prefix) => {
                parts.push(prefix.as_os_str().to_string_lossy().into_owned());
                has_prefix = true;
            }
            Component::RootDir if has_prefix => {
                parts.last_mut().unwrap().push(std::path::MAIN_SEPARATOR);
            }
            component => parts.push(component.as_os_str().to_string_lossy().into_owned()),
        }
    }
    parts
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
    fn breadcrumbs_prefer_cwd_then_home_and_match_whole_components() {
        let root = std::env::temp_dir();
        let home = root.join("breadcrumb-home");
        let cwd = home.join("project");
        let format = |path: &Path| breadcrumb_path_components(path, &cwd, Some(&home));
        assert_eq!(format(&cwd.join("src/main.rs")), ["src", "main.rs"]);
        assert_eq!(format(Path::new("src/main.rs")), ["src", "main.rs"]);
        assert_eq!(
            format(&home.join("other/main.rs")),
            ["~", "other", "main.rs"]
        );
        assert_eq!(format(&home), ["~"]);
        assert_ne!(format(&root.join("breadcrumb-home-other/main.rs"))[0], "~");
        assert_eq!(
            breadcrumb_path_components(&home.join("main.rs"), &cwd, None),
            display_components(&home.join("main.rs"))
        );
        assert_eq!(
            format(&home.join("missing/日本語.rs")),
            ["~", "missing", "日本語.rs"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn breadcrumbs_preserve_unix_roots_and_backslashes_in_names() {
        assert_eq!(
            breadcrumb_path_components(Path::new("/var/data/a\\b.rs"), Path::new("/work"), None),
            ["/", "var", "data", "a\\b.rs"]
        );
    }

    #[cfg(windows)]
    #[test]
    fn breadcrumbs_handle_windows_drives_unc_and_home_case() {
        let cwd = Path::new(r"C:\Users\Alice\project");
        let home = Some(Path::new(r"C:\Users\Alice"));
        assert_eq!(
            breadcrumb_path_components(Path::new(r"c:\users\ALICE\other\main.rs"), cwd, home),
            ["~", "other", "main.rs"]
        );
        assert_eq!(
            breadcrumb_path_components(Path::new(r"D:\src\main.rs"), cwd, home),
            ["D:\\", "src", "main.rs"]
        );
        assert_eq!(
            breadcrumb_path_components(Path::new(r"\\server\share\src\main.rs"), cwd, home),
            ["\\\\server\\share\\", "src", "main.rs"]
        );
    }

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
