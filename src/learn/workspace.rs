//! Owned, disposable files used by lessons that need real disk effects.

use std::path::{Component, Path, PathBuf};

/// The directory is removed when its lesson ends. Paths are never resolved
/// relative to the user's working directory.
pub(crate) struct PracticeWorkspace {
    root: tempfile::TempDir,
}

impl PracticeWorkspace {
    pub fn new() -> std::io::Result<Self> {
        Ok(Self {
            root: tempfile::Builder::new().prefix("red-learn-").tempdir()?,
        })
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    pub fn root(&self) -> &Path {
        self.root.path()
    }

    pub fn write_fixture(&self, name: &str, contents: &str) -> std::io::Result<()> {
        let relative = Path::new(name);
        if !ordinary_relative_path(relative) {
            return Err(invalid_fixture_path());
        }
        let mut parent = self.root.path().to_path_buf();
        for component in relative.parent().into_iter().flat_map(Path::components) {
            parent.push(component);
            match std::fs::symlink_metadata(&parent) {
                Ok(metadata) if metadata.file_type().is_dir() => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::create_dir(&parent)?;
                }
                _ => return Err(invalid_fixture_path()),
            }
        }
        let path = self.path(name);
        if !self.permits_file(&path) {
            return Err(invalid_fixture_path());
        }
        std::fs::write(path, contents)
    }

    /// Only ordinary files beneath this private directory may be accessed.
    /// Reject traversal and symlinks in every component, including the leaf.
    pub fn permits_file(&self, path: &Path) -> bool {
        let Ok(root) = self.root.path().canonicalize() else {
            return false;
        };
        let Ok(relative) = path
            .strip_prefix(self.root.path())
            .or_else(|_| path.strip_prefix(&root))
        else {
            return false;
        };
        if !ordinary_relative_path(relative) {
            return false;
        }
        let mut checked = root;
        let mut components = relative.components().peekable();
        while let Some(component) = components.next() {
            checked.push(component);
            let leaf = components.peek().is_none();
            match std::fs::symlink_metadata(&checked) {
                Ok(metadata) if leaf => return metadata.file_type().is_file(),
                Ok(metadata) if metadata.file_type().is_dir() => {}
                Err(error) if leaf && error.kind() == std::io::ErrorKind::NotFound => return true,
                _ => return false,
            }
        }
        false
    }
}

fn ordinary_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn invalid_fixture_path() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "invalid practice fixture path",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_files_are_removed_and_outside_paths_are_refused() {
        let outside = tempfile::tempdir().unwrap();
        let file;
        {
            let workspace = PracticeWorkspace::new().unwrap();
            file = workspace.path("practice.txt");
            assert!(workspace.permits_file(&file));
            assert!(!workspace.permits_file(&outside.path().join("practice.txt")));
            assert!(!workspace.permits_file(&workspace.path("../outside.txt")));
            std::fs::write(&file, "practice").unwrap();
            assert!(workspace.permits_file(&file));
        }
        assert!(!file.exists());
    }

    #[test]
    fn nested_fixtures_remain_inside_the_owned_directory() {
        let workspace = PracticeWorkspace::new().unwrap();
        workspace.write_fixture("src/score.hk", "source\n").unwrap();
        workspace.write_fixture("tests/score.hk", "test\n").unwrap();
        assert!(workspace.permits_file(&workspace.path("src/score.hk")));
        assert!(workspace.permits_file(&workspace.path("src/new.hk")));
        assert!(!workspace.permits_file(&workspace.path("src/../tests/score.hk")));
        assert!(workspace.write_fixture("../outside.hk", "bad").is_err());
        assert!(workspace.write_fixture("/outside.hk", "bad").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_turn_a_practice_save_into_an_outside_write() {
        let workspace = PracticeWorkspace::new().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let link = workspace.path("practice.txt");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        assert!(!workspace.permits_file(&link));
        let nested = workspace.path("nested");
        std::os::unix::fs::symlink(outside.path().parent().unwrap(), &nested).unwrap();
        assert!(!workspace.permits_file(&nested.join("new.hk")));
        assert!(workspace.write_fixture("nested/new.hk", "bad").is_err());
    }
}
