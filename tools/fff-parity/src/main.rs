//! Driver for measuring successive queries against a single FFF index.
use anyhow::{Context, Result, ensure};
use fff_search::{
    FFFQuery, FilePicker, FilePickerOptions, FuzzyQuery, GitRecencyConfig, GrepMode,
    GrepSearchOptions, SharedFilePicker, SharedFrecency,
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Write},
    time::{Duration, Instant},
};

#[derive(Deserialize)]
struct Query {
    pattern: String,
    regex: bool,
}

fn serve_queries<Q: DeserializeOwned>(
    ready_us: u128,
    mut search: impl FnMut(Q) -> Result<Value>,
) -> Result<()> {
    let mut out = io::BufWriter::new(io::stdout().lock());
    writeln!(out, "{}", json!({"ready_us":ready_us}))?;
    out.flush()?;
    for line in io::stdin().lock().lines() {
        let response = search(serde_json::from_str(&line?)?)?;
        serde_json::to_writer(&mut out, &response)?;
        writeln!(out)?;
        out.flush()?;
    }
    Ok(())
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let root = args.next().context("expected search directory")?;
    let flags: Vec<_> = args.collect();
    if flags.iter().any(|arg| arg == "--red") {
        return red_driver(
            std::path::Path::new(&root),
            flags.iter().any(|arg| arg == "--wait-for-index"),
        );
    }
    let follow_symlinks = flags.iter().any(|arg| arg == "--follow");
    let shared = SharedFilePicker::default();
    let start = Instant::now();
    FilePicker::new_with_shared_state(
        shared.clone(),
        SharedFrecency::noop(),
        FilePickerOptions {
            base_path: root,
            follow_symlinks,
            watch: false,
            enable_content_indexing: true,
            git_recency: GitRecencyConfig {
                enabled: false,
                ..Default::default()
            },
            ..Default::default()
        },
    )?;
    ensure!(
        shared.wait_for_scan(Duration::from_secs(120)),
        "FFF scan timed out"
    );
    ensure!(
        shared.wait_for_indexing_complete(Duration::from_secs(120)),
        "FFF content indexing timed out"
    );
    serve_queries(start.elapsed().as_micros(), |Query { pattern, regex }| {
        let query = FFFQuery {
            raw_query: &pattern,
            fuzzy_query: FuzzyQuery::Text(&pattern),
            constraints: Vec::new(),
            location: None,
        };
        let guard = shared.read()?;
        let picker = guard.as_ref().context("FFF index unavailable")?;
        let start = Instant::now();
        let result = picker.grep(
            &query,
            &GrepSearchOptions {
                mode: if regex {
                    GrepMode::Regex
                } else {
                    GrepMode::PlainText
                },
                page_limit: 501,
                max_matches_per_file: 501,
                ..Default::default()
            },
        );
        let rows = result
            .matches
            .iter()
            .take(500)
            .map(|m| {
                json!({
                    "path": result.files[m.file_index].relative_path(picker),
                    "line": m.line_number - 1, "spans": m.match_byte_offsets.as_slice(),
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "matches": rows,
            "truncated": result.matches.len() > 500,
            "regex_error": result.regex_fallback_error,
            "elapsed_us": start.elapsed().as_micros()
        }))
    })
}

fn red_driver(root: &std::path::Path, wait: bool) -> Result<()> {
    let mut engine = red_workspace_search::WorkspaceSearch::default();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    serve_queries(0, |query| {
        let start = Instant::now();
        let first = std::sync::OnceLock::new();
        let mut result = match engine.search_with_preview(root, &query, &cancel, |_| {
            let _ = first.set(start.elapsed().as_micros());
        }) {
            Ok(result) => result,
            Err(err) => {
                return Ok(json!({
                    "matches": [], "truncated": false, "regex_error": err.to_string(),
                    "elapsed_us": start.elapsed().as_micros()
                }));
            }
        };
        while wait && !result.index_ready {
            ensure!(
                start.elapsed() < Duration::from_secs(60),
                "timed out waiting for the index"
            );
            std::thread::sleep(Duration::from_millis(5));
            result = engine.search(root, &query, &cancel)?;
        }
        Ok(json!({
            "matches": result.matches, "truncated": result.truncated,
            "first_us": first.get(), "indexed": result.index_ready,
            "regex_error": result.error, "elapsed_us": start.elapsed().as_micros()
        }))
    })
}
