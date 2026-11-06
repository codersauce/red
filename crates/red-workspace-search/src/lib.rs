//! Workspace content search with an FFF index and byte-accurate line matching.
mod index;
mod stamp;

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result};
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, sinks::Bytes};
use ignore::{DirEntry, WalkBuilder, WalkState};
use serde::{Deserialize, Serialize};

const ROW_LIMIT: usize = 500;
const DISPLAY_BYTES: usize = 2048;
const SPANS_PER_LINE: usize = 256;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Visibility {
    pub hidden: bool,
    pub ignored: bool,
    pub follow: bool,
}

impl Visibility {
    fn walker(self, root: &Path) -> WalkBuilder {
        let mut builder = WalkBuilder::new(root);
        builder
            .standard_filters(!self.ignored)
            .hidden(!self.hidden)
            .follow_links(self.follow)
            .filter_entry(|entry| {
                entry.depth() == 0 || (entry.file_name() != ".git" && entry.file_name() != ".bare")
            });
        if !self.ignored {
            builder.add_custom_ignore_filename(".rgignore");
        }
        builder
    }
}

#[derive(Debug, Deserialize)]
pub struct Query {
    pub pattern: String,
    pub regex: bool,
    #[serde(flatten)]
    pub visibility: Visibility,
}

#[derive(Debug, Serialize)]
pub struct SearchMatch {
    pub path: PathBuf,
    pub line: u64,
    pub detail: String,
    pub spans: Vec<[usize; 2]>,
    pub detail_matches: Vec<[usize; 2]>,
}

#[derive(Debug, Default, Serialize)]
pub struct Outcome {
    pub matches: Vec<SearchMatch>,
    pub truncated: bool,
    pub error: Option<String>,
    pub index_ready: bool,
}

#[derive(Default)]
pub struct WorkspaceSearch {
    index: index::Cache,
}

impl WorkspaceSearch {
    /// Search under the captured cwd. Call from one background worker per editor.
    pub fn search(&mut self, root: &Path, query: &Query, cancel: &AtomicBool) -> Result<Outcome> {
        self.search_with_preview(root, query, cancel, |_| {})
    }

    /// Publish the first row immediately, then return the complete bounded result.
    pub fn search_with_preview(
        &mut self,
        root: &Path,
        query: &Query,
        cancel: &AtomicBool,
        first_row: impl Fn(&SearchMatch) + Sync,
    ) -> Result<Outcome> {
        if query.pattern.is_empty() {
            return Ok(Outcome::default());
        }
        let matcher = RegexMatcherBuilder::new()
            .case_smart(true)
            .multi_line(true)
            .line_terminator(Some(b'\n'))
            .ban_byte(Some(0))
            .fixed_strings(!query.regex)
            .build(&query.pattern)?;
        let index = self.index.get(root, query.visibility)?;
        let search = SearchRun {
            root,
            matcher,
            candidates: index.and_then(|index| index.candidates(query).map(|bits| (index, bits))),
            cancel,
            full: AtomicBool::new(false),
            first_row: &first_row,
            outcome: Mutex::new(Outcome {
                index_ready: index.is_some(),
                ..Outcome::default()
            }),
        };
        let mut walker = query.visibility.walker(root);
        walker.threads(std::thread::available_parallelism().map_or(2, |n| n.get().min(8)));
        walker.build_parallel().run(|| {
            let search = &search;
            let mut searcher = SearcherBuilder::new()
                .line_number(true)
                .binary_detection(BinaryDetection::quit(0))
                .build();
            Box::new(move |entry| {
                if search.stopped() {
                    return WalkState::Quit;
                }
                if let Err(err) = search.scan(&mut searcher, entry) {
                    search
                        .outcome
                        .lock()
                        .expect("workspace result collection poisoned")
                        .error
                        .get_or_insert_with(|| format!("{err:#}"));
                }
                WalkState::Continue
            })
        });
        Ok(search
            .outcome
            .into_inner()
            .expect("workspace result collection poisoned"))
    }
}

struct CancellableReader<'a> {
    file: File,
    cancel: &'a AtomicBool,
}

impl Read for CancellableReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Ok(0);
        }
        self.file.read(buf)
    }
}

struct SearchRun<'a> {
    root: &'a Path,
    matcher: RegexMatcher,
    candidates: Option<(&'a index::Index, Vec<u64>)>,
    cancel: &'a AtomicBool,
    full: AtomicBool,
    first_row: &'a (dyn Fn(&SearchMatch) + Sync),
    outcome: Mutex<Outcome>,
}

impl SearchRun<'_> {
    fn stopped(&self) -> bool {
        self.cancel.load(Ordering::Relaxed) || self.full.load(Ordering::Relaxed)
    }

    fn scan(&self, searcher: &mut Searcher, entry: Result<DirEntry, ignore::Error>) -> Result<()> {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type().is_some_and(|t| t.is_file())
            || self
                .candidates
                .as_ref()
                .is_some_and(|(index, bits)| index.can_skip(path, bits))
        {
            return Ok(());
        }
        let relative = path.strip_prefix(self.root)?;
        let context = || path.display().to_string();
        let file = File::open(path).with_context(context)?;
        searcher
            .search_reader(
                &self.matcher,
                CancellableReader {
                    file,
                    cancel: self.cancel,
                },
                Bytes(|number, line| self.matched(relative, number, line)),
            )
            .with_context(context)
    }

    fn matched(&self, path: &Path, line_number: u64, bytes: &[u8]) -> io::Result<bool> {
        if self.stopped() {
            return Ok(false);
        }
        let mut partial_spans = false;
        let line = bytes.strip_suffix(b"\n").unwrap_or(bytes);
        let mut spans = Vec::new();
        self.matcher
            .find_iter(line, |m| {
                if spans.len() == SPANS_PER_LINE {
                    partial_spans = true;
                    return false;
                }
                spans.push([m.start(), m.end()]);
                true
            })
            .map_err(io::Error::other)?;
        let first = spans.first().map_or(0, |s| s[0]);
        let mut start = first.saturating_sub(DISPLAY_BYTES / 2);
        while start > 0 && line.get(start).is_some_and(|b| b & 0xc0 == 0x80) {
            start -= 1;
        }
        let mut end = (start + DISPLAY_BYTES).min(line.len());
        while end < line.len() && end > start && line[end] & 0xc0 == 0x80 {
            end -= 1;
        }
        let detail = String::from_utf8_lossy(&line[start..end])
            .trim_end_matches('\r')
            .to_owned();
        let char_index = |offset: usize| {
            String::from_utf8_lossy(&line[start..offset.clamp(start, end)])
                .chars()
                .count()
        };
        let detail_matches = spans
            .iter()
            .filter(|s| s[0] < end && s[1] > start)
            .map(|s| [char_index(s[0]), char_index(s[1])])
            .collect();
        let row = SearchMatch {
            path: path.to_owned(),
            line: line_number - 1,
            detail,
            detail_matches,
            spans,
        };
        let mut result = self
            .outcome
            .lock()
            .expect("workspace result collection poisoned");
        if result.matches.len() == ROW_LIMIT {
            result.truncated = true;
            self.full.store(true, Ordering::Relaxed);
            return Ok(false);
        }
        if result.matches.is_empty() {
            (self.first_row)(&row);
        }
        result.truncated |= partial_spans;
        result.matches.push(row);
        Ok(true)
    }
}
