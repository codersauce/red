use super::*;
use std::os::unix::fs::symlink;

fn snapshot(root: &Path) -> InlineContextSnapshot {
    InlineContextSnapshot {
        root: root.canonicalize().unwrap(),
        visible: BTreeMap::new(),
        allow_sensitive_paths: false,
    }
}

fn read(path: &str) -> InlineContextCall {
    InlineContextCall::ReadFile {
        path: path.into(),
        start_line: 1,
        line_count: 200,
    }
}

fn visible(content: &str) -> Option<VisibleText> {
    Some(VisibleText {
        content: content.into(),
        revision: 7,
        dirty: true,
    })
}

#[tokio::test]
async fn inline_context_reads_and_searches_unsaved_text_without_disk_fallback() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("main.c"), "disk-only needle\n").unwrap();
    let mut context = snapshot(root.path());
    context
        .visible
        .insert("main.c".into(), visible("unsaved needle\nsecond\n"));
    context
        .visible
        .insert("new.c".into(), visible("new needle\n"));
    let found = context
        .execute_read(InlineContextCall::SearchFiles {
            query: "needle".into(),
        })
        .unwrap();
    assert_eq!(found["matches"].as_array().unwrap().len(), 2);
    assert!(found.to_string().contains("unsaved needle"));
    assert!(!found.to_string().contains("disk-only"));
    let mut context = snapshot(root.path());
    context
        .visible
        .insert("main.c".into(), visible("unsaved\nsecond\n"));
    let value = context
        .execute(InlineContextCall::ReadFile {
            path: "main.c".into(),
            start_line: 2,
            line_count: 1,
        })
        .await
        .unwrap();
    assert_eq!(value["content"], "second\n");
    assert_eq!(value["source"], "editor");
    assert_eq!(value["revision"], 7);
    assert_eq!(value["unsaved"], true);
    let mut context = snapshot(root.path());
    context.visible.insert("main.c".into(), None);
    assert!(context
        .execute(read("main.c"))
        .await
        .unwrap_err()
        .to_string()
        .contains("open buffer"));
    assert_eq!(
        std::fs::read_to_string(root.path().join("main.c")).unwrap(),
        "disk-only needle\n"
    );
}

#[tokio::test]
async fn inline_context_rejects_unsafe_ignored_binary_and_oversized_files() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join(".gitignore"), "ignored.c\n").unwrap();
    for name in [
        "safe.c",
        "ignored.c",
        ".env",
        "credential.txt",
        "private.pem",
    ] {
        std::fs::write(root.path().join(name), "needle\n").unwrap();
    }
    std::fs::create_dir(root.path().join("secrets")).unwrap();
    std::fs::write(root.path().join("secrets/value.c"), "needle\n").unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    std::fs::write(root.path().join(".git/config"), "needle\n").unwrap();
    std::fs::write(root.path().join("binary.c"), b"needle\0data").unwrap();
    std::fs::write(root.path().join("large.c"), vec![b'x'; MAX_FILE_BYTES + 1]).unwrap();
    std::fs::write(outside.path().join("outside.c"), "needle\n").unwrap();
    symlink(outside.path().join("outside.c"), root.path().join("link.c")).unwrap();
    for path in [
        "../outside.c",
        "ignored.c",
        ".env",
        "credential.txt",
        "private.pem",
        "secrets/value.c",
        ".git/config",
        ".GIT/config",
        "binary.c",
        "large.c",
        "link.c",
    ] {
        assert!(
            snapshot(root.path()).execute(read(path)).await.is_err(),
            "{path}"
        );
    }
    let files = snapshot(root.path())
        .execute(InlineContextCall::ListFiles {})
        .await
        .unwrap();
    assert!(files["files"]
        .as_array()
        .unwrap()
        .contains(&json!("safe.c")));
    for forbidden in [
        "ignored.c",
        ".env",
        "credential.txt",
        "secrets/value.c",
        ".git/config",
        "link.c",
    ] {
        assert!(
            !files["files"]
                .as_array()
                .unwrap()
                .contains(&json!(forbidden)),
            "{forbidden}"
        );
    }

    let found = snapshot(root.path())
        .execute(InlineContextCall::SearchFiles {
            query: "needle".into(),
        })
        .await
        .unwrap();
    let matches = found["matches"].as_array().unwrap();
    assert!(matches.iter().any(|entry| entry["path"] == "safe.c"));
    for forbidden in [
        "ignored.c",
        ".env",
        "credential.txt",
        "private.pem",
        "secrets/value.c",
        ".git/config",
        "link.c",
    ] {
        assert!(
            !matches.iter().any(|entry| entry["path"] == forbidden),
            "searched restricted path {forbidden}"
        );
    }
}

#[test]
fn inline_workspace_reader_remains_anchored_and_rejects_replaced_symlinks() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("workspace");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("source.rs"), "original\n").unwrap();
    let reader = crate::codex::InlineWorkspaceReader::new(&root).unwrap();

    let original = directory.path().join("original");
    std::fs::rename(&root, &original).unwrap();
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("source.rs"), "replacement\n").unwrap();
    assert_eq!(
        reader.read("source.rs", MAX_FILE_BYTES).unwrap(),
        Some("original\n".to_string())
    );

    let outside = directory.path().join("outside.rs");
    std::fs::write(&outside, "private\n").unwrap();
    std::fs::remove_file(original.join("source.rs")).unwrap();
    symlink(&outside, original.join("source.rs")).unwrap();
    assert!(reader.read("source.rs", MAX_FILE_BYTES).unwrap().is_none());
    assert!(reader.read("../outside.rs", MAX_FILE_BYTES).is_err());
    assert!(reader.read("/etc/passwd", MAX_FILE_BYTES).is_err());
}

#[tokio::test]
async fn inline_context_listing_preserves_nested_ignores_and_path_boundaries() {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q"]);
    std::fs::write(root.path().join(".gitignore"), "*.tmp\n!kept.tmp\n").unwrap();
    std::fs::write(root.path().join("ignored.tmp"), "ignored\n").unwrap();
    std::fs::write(root.path().join("kept.tmp"), "visible\n").unwrap();
    std::fs::create_dir(root.path().join("nested")).unwrap();
    std::fs::write(root.path().join("nested/.ignore"), "ignored.rs\n").unwrap();
    std::fs::write(root.path().join("nested/ignored.rs"), "ignored\n").unwrap();
    std::fs::write(root.path().join("nested/visible.rs"), "visible\n").unwrap();
    std::fs::create_dir(root.path().join("Credentials")).unwrap();
    std::fs::write(root.path().join("Credentials/value.rs"), "private\n").unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("outside.rs"), "private\n").unwrap();
    symlink(outside.path(), root.path().join("outside-link")).unwrap();
    symlink(
        outside.path().join("outside.rs"),
        root.path().join("outside.rs"),
    )
    .unwrap();

    let files = snapshot(root.path())
        .execute(InlineContextCall::ListFiles {})
        .await
        .unwrap();
    let files = files["files"].as_array().unwrap();
    for allowed in ["kept.tmp", "nested/visible.rs"] {
        assert!(files.contains(&json!(allowed)), "missing {allowed}");
    }
    for forbidden in [
        "ignored.tmp",
        "nested/ignored.rs",
        "Credentials/value.rs",
        ".git/config",
        ".GIT/config",
        "outside-link/outside.rs",
        "outside.rs",
    ] {
        assert!(!files.contains(&json!(forbidden)), "disclosed {forbidden}");
    }
}

#[tokio::test]
async fn inline_context_listing_rejects_symlinked_workspace_roots() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("workspace");
    std::fs::create_dir(&root).unwrap();
    let alias = directory.path().join("alias");
    symlink(&root, &alias).unwrap();
    let context = InlineContextSnapshot {
        root: alias,
        visible: BTreeMap::new(),
        allow_sensitive_paths: false,
    };

    let error = context
        .execute(InlineContextCall::ListFiles {})
        .await
        .unwrap_err();
    assert!(error.to_string().contains("symlink"), "{error}");
}

#[tokio::test]
async fn inline_context_can_read_sensitive_workspace_files_after_explicit_consent() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join(".env"), "TOKEN=example\n").unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    std::fs::write(root.path().join(".git/config"), "private\n").unwrap();
    let consented = || {
        let mut context = snapshot(root.path());
        context.allow_sensitive_paths = true;
        context
    };

    let value = consented().execute(read(".env")).await.unwrap();
    assert_eq!(value["content"], "TOKEN=example\n");
    let files = consented()
        .execute(InlineContextCall::ListFiles {})
        .await
        .unwrap();
    assert!(files["files"].as_array().unwrap().contains(&json!(".env")));
    assert!(consented().execute(read(".git/config")).await.is_err());
}

#[tokio::test]
async fn inline_context_bounds_reads_and_reports_truncation() {
    let root = tempfile::tempdir().unwrap();
    let source = (0..250)
        .map(|line| format!("line {line}\n"))
        .collect::<String>();
    std::fs::write(root.path().join("many.c"), source).unwrap();
    let result = snapshot(root.path()).execute(read("many.c")).await.unwrap();
    assert_eq!(result["next_line"], 201);
    assert_eq!(result["truncated"], true);
    std::fs::write(root.path().join("long.c"), "é".repeat(MAX_TEXT_BYTES)).unwrap();
    let result = snapshot(root.path()).execute(read("long.c")).await.unwrap();
    assert!(result["content"].as_str().unwrap().len() <= MAX_TEXT_BYTES);
    assert_eq!(result["line_truncated"], true);
    for (name, args) in [
        ("write_file", json!({"path":"many.c"})),
        ("open_file", json!({"path":"many.c"})),
        ("read_file", json!({"path":"many.c","start_line":0})),
        ("read_file", json!({"path":"many.c","line_count":201})),
        (
            "read_git_diff",
            json!({"path":"many.c","revision":"HEAD~1"}),
        ),
        ("search_files", json!({"query":""})),
    ] {
        assert!(InlineContextCall::parse(name, args).is_err(), "{name}");
    }
}

fn git(root: &Path, args: &[&str]) {
    assert!(
        std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap()
            .status
            .success(),
        "{args:?}"
    );
}

#[tokio::test]
async fn inline_context_git_diff_uses_head_and_unsaved_buffer_from_a_subdirectory() {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q"]);
    std::fs::create_dir(root.path().join("src")).unwrap();
    let path = root.path().join("src/main.c");
    std::fs::write(&path, "committed\n").unwrap();
    git(root.path(), &["add", "src/main.c"]);
    git(
        root.path(),
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-qm",
            "test: add base",
        ],
    );
    std::fs::write(&path, "saved later\n").unwrap();
    let mut context = snapshot(&root.path().join("src"));
    context
        .visible
        .insert("main.c".into(), visible("unsaved latest\n"));
    let result = context
        .execute(InlineContextCall::ReadGitDiff {
            path: "main.c".into(),
        })
        .await
        .unwrap();
    assert!(result["diff"].as_str().unwrap().contains("-committed"));
    assert!(result["diff"].as_str().unwrap().contains("+unsaved latest"));
    assert!(!result["diff"].as_str().unwrap().contains("saved later"));
    assert_eq!(result["revision"], 7);
    assert_eq!(result["base_commit"].as_str().unwrap().len(), 40);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "saved later\n");
}
