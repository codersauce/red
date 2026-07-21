use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::Context;
use husk::{PackageLock, PackageManifest};
use semver::Version;

const IGNORE_ENTRY: &str = "/.husk/";

pub(crate) struct NewOptions<'a> {
    pub(crate) path: &'a Path,
    pub(crate) name: Option<&'a str>,
}

#[derive(Debug)]
pub(crate) struct NewOutput {
    pub(crate) root: PathBuf,
    pub(crate) name: String,
}

pub(crate) fn create(options: &NewOptions<'_>) -> anyhow::Result<NewOutput> {
    let parent = options
        .path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    anyhow::ensure!(
        parent.is_dir(),
        "project parent `{}` is not a directory",
        parent.display()
    );
    let root_existed = options.path.exists();
    if root_existed {
        let metadata = fs::symlink_metadata(options.path)
            .with_context(|| format!("inspect `{}`", options.path.display()))?;
        anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "project path `{}` is not a regular directory",
            options.path.display()
        );
    } else {
        fs::create_dir(options.path)
            .with_context(|| format!("create project directory `{}`", options.path.display()))?;
    }

    let result = create_in_root(options.path, options.name);
    if result.is_err() && !root_existed {
        fs::remove_dir(options.path).ok();
    }
    result
}

fn create_in_root(root: &Path, explicit_name: Option<&str>) -> anyhow::Result<NewOutput> {
    let root = root
        .canonicalize()
        .with_context(|| format!("resolve project directory `{}`", root.display()))?;
    let name = explicit_name
        .map(str::to_string)
        .or_else(|| {
            root.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })
        .context("could not infer a UTF-8 package name; pass `--name <NAME>`")?;
    let version = Version::new(0, 1, 0);
    let manifest_source = format!(
        "schema_version = 1\n\n[package]\nname = {}\nversion = \"0.1.0\"\nentry = \"src/main.hk\"\n",
        serde_json::to_string(&name).expect("serializing a string cannot fail")
    );
    PackageManifest::parse(&manifest_source).context("validate new Husk package")?;
    let lock_source = PackageLock::empty(name.clone(), version)
        .to_toml()
        .context("serialize initial Husk.lock")?;

    let manifest_path = root.join("Husk.toml");
    let lock_path = root.join("Husk.lock");
    let source_directory = root.join("src");
    let source_path = source_directory.join("main.hk");
    let ignore_path = root.join(".gitignore");
    for path in [&manifest_path, &lock_path, &source_path] {
        anyhow::ensure!(
            !path.exists(),
            "refusing to overwrite existing project file `{}`",
            path.display()
        );
    }
    let source_directory_existed = source_directory.exists();
    if source_directory_existed {
        let metadata = fs::symlink_metadata(&source_directory)
            .with_context(|| format!("inspect `{}`", source_directory.display()))?;
        anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "source path `{}` is not a regular directory",
            source_directory.display()
        );
    }
    let original_ignore = read_optional_regular_file(&ignore_path)?;
    let updated_ignore = updated_gitignore(original_ignore.as_deref())?;

    let mut created = Vec::new();
    let created_source_directory = if source_directory_existed {
        false
    } else {
        fs::create_dir(&source_directory)
            .with_context(|| format!("create `{}`", source_directory.display()))?;
        true
    };
    let publication = write_new_file(&manifest_path, manifest_source.as_bytes())
        .map(|()| created.push(manifest_path.clone()))
        .and_then(|()| {
            write_new_file(&lock_path, lock_source.as_bytes())?;
            created.push(lock_path.clone());
            Ok(())
        })
        .and_then(|()| {
            write_new_file(
                &source_path,
                b"fn main() {\n    std::println(\"Hello from Husk!\");\n}\n",
            )?;
            created.push(source_path.clone());
            Ok(())
        })
        .and_then(|()| replace_file(&ignore_path, updated_ignore.as_bytes()));
    if let Err(error) = publication {
        for path in created.iter().rev() {
            fs::remove_file(path).ok();
        }
        restore_optional_file(&ignore_path, original_ignore.as_deref()).ok();
        if created_source_directory {
            fs::remove_dir(&source_directory).ok();
        }
        return Err(error);
    }

    Ok(NewOutput { root, name })
}

fn updated_gitignore(existing: Option<&[u8]>) -> anyhow::Result<String> {
    let existing = match existing {
        Some(bytes) => std::str::from_utf8(bytes).context(".gitignore is not UTF-8")?,
        None => "",
    };
    if existing.lines().any(|line| line.trim() == IGNORE_ENTRY) {
        return Ok(existing.to_string());
    }
    let mut updated = existing.to_string();
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(IGNORE_ENTRY);
    updated.push('\n');
    Ok(updated)
}

fn read_optional_regular_file(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "project file `{}` is not a regular file",
                path.display()
            );
            fs::read(path)
                .with_context(|| format!("read `{}`", path.display()))
                .map(Some)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("inspect `{}`", path.display())),
    }
}

fn write_new_file(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create temporary file for `{}`", path.display()))?;
    temporary
        .write_all(contents)
        .with_context(|| format!("write `{}`", path.display()))?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)
        .with_context(|| format!("publish `{}`", path.display()))?;
    Ok(())
}

fn replace_file(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create replacement for `{}`", path.display()))?;
    temporary
        .write_all(contents)
        .with_context(|| format!("write replacement for `{}`", path.display()))?;
    if let Ok(metadata) = fs::metadata(path) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())
            .with_context(|| format!("preserve permissions for `{}`", path.display()))?;
    }
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("replace `{}`", path.display()))?;
    Ok(())
}

fn restore_optional_file(path: &Path, contents: Option<&[u8]>) -> anyhow::Result<()> {
    if let Some(contents) = contents {
        replace_file(path, contents)
    } else if path.exists() {
        fs::remove_file(path).with_context(|| format!("remove `{}`", path.display()))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_project_and_preserves_existing_gitignore() {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("hello");
        fs::create_dir(&project).unwrap();
        fs::write(project.join(".gitignore"), "notes.txt\n").unwrap();

        let output = create(&NewOptions {
            path: &project,
            name: None,
        })
        .unwrap();

        assert_eq!(output.name, "hello");
        assert!(project.join("Husk.toml").is_file());
        assert!(project.join("Husk.lock").is_file());
        assert!(project.join("src/main.hk").is_file());
        assert_eq!(
            fs::read_to_string(project.join(".gitignore")).unwrap(),
            "notes.txt\n/.husk/\n"
        );
    }

    #[test]
    fn refuses_to_overwrite_existing_project_files() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("Husk.toml"), "keep me").unwrap();

        let error = create(&NewOptions {
            path: directory.path(),
            name: Some("example"),
        })
        .unwrap_err();

        assert!(error.to_string().contains("refusing to overwrite"));
        assert_eq!(
            fs::read_to_string(directory.path().join("Husk.toml")).unwrap(),
            "keep me"
        );
        assert!(!directory.path().join("src").exists());
    }
}
