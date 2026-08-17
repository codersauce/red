//! Owned, disposable files used by lessons that need real disk effects.

use std::path::{Path, PathBuf};

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
        let path = self.path(name);
        if !self.permits_file(&path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "invalid practice fixture path",
            ));
        }
        std::fs::write(path, contents)
    }

    /// Only a direct, ordinary file in this private directory may be saved.
    /// Reject symlinks as well as paths that escape through `..`.
    pub fn permits_file(&self, path: &Path) -> bool {
        let Some(parent) = path.parent() else {
            return false;
        };
        let Ok(root) = self.root.path().canonicalize() else {
            return false;
        };
        if parent.canonicalize().ok().as_ref() != Some(&root) {
            return false;
        }
        match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata.file_type().is_file(),
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        }
    }
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

    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_turn_a_practice_save_into_an_outside_write() {
        let workspace = PracticeWorkspace::new().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let link = workspace.path("practice.txt");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        assert!(!workspace.permits_file(&link));
    }
}
