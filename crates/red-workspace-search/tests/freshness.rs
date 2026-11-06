//! Verify that a warmed filter never hides changed, newly visible, or replaced files.
use red_workspace_search::{Query, Visibility, WorkspaceSearch};
use std::fs;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

#[test]
fn warmed_search_tracks_replaced_files_and_reloads_ignore_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for n in 0..100 {
        fs::write(
            root.join(n.to_string()),
            if n < 10 { "needle" } else { "absent" },
        )
        .unwrap();
    }
    fs::write(root.join(".rgignore"), "hidden-by-rule\n").unwrap();
    fs::write(root.join("hidden-by-rule"), "needle").unwrap();
    let mut engine = WorkspaceSearch::default();
    let cancel = AtomicBool::new(false);
    let query = Query {
        pattern: "needle".into(),
        regex: true,
        visibility: Visibility::default(),
    };
    let start = Instant::now();
    loop {
        let result = engine.search(root, &query, &cancel).unwrap();
        assert_eq!(result.matches.len(), 10);
        if result.index_ready {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "index did not finish"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    fs::write(root.join("replacement"), "needle").unwrap();
    fs::remove_file(root.join("99")).unwrap();
    fs::rename(root.join("replacement"), root.join("99")).unwrap();
    fs::write(root.join("98"), "needle").unwrap();
    fs::remove_file(root.join("0")).unwrap();
    fs::write(root.join(".rgignore"), "").unwrap();
    assert_eq!(
        engine.search(root, &query, &cancel).unwrap().matches.len(),
        12
    );
    assert!(
        engine
            .search(root, &query, &AtomicBool::new(true))
            .unwrap()
            .matches
            .is_empty()
    );
}

#[test]
fn rewriting_a_large_file_during_search_keeps_the_worker_usable() {
    use std::thread;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("changing");
    let content = "0123456789abcdef\n".repeat(70_000) + "tail position\n";
    fs::write(&path, &content).unwrap();
    let query = Query {
        pattern: "tail position".into(),
        regex: false,
        visibility: Visibility::default(),
    };
    let mut engine = WorkspaceSearch::default();
    let cancel = AtomicBool::new(false);
    // Fixed iterations guarantee writer teardown even if the reader fails.
    thread::scope(|scope| {
        let writer = scope.spawn(|| {
            for _ in 0..24 {
                fs::write(&path, &content).unwrap();
                thread::yield_now();
            }
        });
        loop {
            engine.search(temp.path(), &query, &cancel).unwrap();
            if writer.is_finished() {
                break;
            }
        }
    });
    let result = engine.search(temp.path(), &query, &cancel).unwrap();
    assert_eq!(result.matches.len(), 1);
    assert_eq!(result.matches[0].line, 70_000);
    assert_eq!(result.matches[0].spans, [[0, 13]]);
}

#[test]
fn first_row_is_published_once_and_survives_the_limit() {
    let root = tempfile::tempdir().unwrap();
    for n in 0..8 {
        fs::write(root.path().join(n.to_string()), "match\n".repeat(100)).unwrap();
    }
    let query = Query {
        pattern: "match".into(),
        regex: true,
        visibility: Visibility::default(),
    };
    let first = std::sync::OnceLock::new();
    let result = WorkspaceSearch::default()
        .search_with_preview(root.path(), &query, &AtomicBool::new(false), |row| {
            first.set((row.path.clone(), row.line)).unwrap();
        })
        .unwrap();
    assert!(result.truncated);
    assert_eq!(result.matches.len(), 500);
    assert_eq!(
        first.get(),
        Some(&(result.matches[0].path.clone(), result.matches[0].line))
    );
}
