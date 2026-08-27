//! Latest-query-only matching over shared discovery snapshots. Neither discovery
//! updates nor new keystrokes run a workspace-sized filter on the editor thread.

use std::{
    cmp::Ordering,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering},
        Arc, Condvar, Mutex,
    },
    time::{Duration, Instant},
};

use fuzzy_matcher::skim::SkimMatcherV2;
use rayon::prelude::*;

use super::super::picker_items::PickerItems;
use super::{
    file_match_score,
    index::{IndexSnapshot, ScanLease},
    prepare_file_picker_items, FilePickerQuery,
};

#[derive(Clone, Copy)]
struct RankedFile {
    index: usize,
    score: i64,
}

pub(super) struct SearchResult {
    pub(super) generation: u64,
    pub(super) items: PickerItems,
    pub(super) order: Vec<usize>,
    pub(super) done: bool,
    pub(super) refreshing: bool,
    pub(super) discovered: usize,
    pub(super) error: Option<String>,
    query: String,
    ranked: Arc<Vec<RankedFile>>,
}

impl SearchResult {
    /// The old selection may have moved since the request was sent. Locate the
    /// currently displayed path in logarithmic time using the same ranking key.
    pub(super) fn selection(&self, path: &str) -> Option<usize> {
        let item = prepare_file_picker_items(vec![path.to_string()]).pop()?;
        let score = if self.query.is_empty() {
            0
        } else {
            file_match_score(&SkimMatcherV2::default(), &item, &self.query)?
        };
        self.ranked
            .binary_search_by(|rank| {
                compare_keys(
                    rank.score,
                    &self.items[rank.index].id,
                    score,
                    path,
                    self.query.is_empty(),
                )
            })
            .ok()
    }
}

fn compare_keys(
    left_score: i64,
    left: &str,
    right_score: i64,
    right: &str,
    empty: bool,
) -> Ordering {
    if empty {
        return left.cmp(right);
    }
    right_score
        .cmp(&left_score)
        .then_with(|| left.len().cmp(&right.len()))
        .then_with(|| left.cmp(right))
}

#[derive(Default)]
struct Request {
    generation: u64,
    query: String,
}

#[derive(Default)]
struct SharedSearch {
    request: Mutex<Request>,
    wake: Condvar,
    generation: AtomicU64,
    cancelled: AtomicBool,
    result: Mutex<Option<SearchResult>>,
}

pub(super) struct FileSearch {
    shared: Arc<SharedSearch>,
}

impl FileSearch {
    pub(super) fn new(lease: &ScanLease) -> Self {
        let shared = Arc::new(SharedSearch::default());
        let worker = Arc::clone(&shared);
        let scan = Arc::clone(&lease.scan);
        let fallback = lease.fallback.clone();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_search(&worker, || scan.snapshot(), fallback);
            }));
            if result.is_err() && !worker.cancelled.load(AtomicOrdering::Relaxed) {
                let snapshot = scan.snapshot();
                *worker
                    .result
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(SearchResult {
                    generation: worker.generation.load(AtomicOrdering::Relaxed),
                    items: snapshot.items,
                    order: Vec::new(),
                    done: true,
                    refreshing: false,
                    discovered: 0,
                    error: Some("File matching worker panicked".to_string()),
                    query: String::new(),
                    ranked: Arc::new(Vec::new()),
                });
            }
        });
        Self { shared }
    }

    pub(super) fn request(&self, query: &str) -> u64 {
        let mut request = self
            .shared
            .request
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if request.query != query {
            request.query = query.to_string();
            request.generation = request.generation.wrapping_add(1);
            self.shared
                .generation
                .store(request.generation, AtomicOrdering::Relaxed);
            self.shared.wake.notify_one();
        }
        request.generation
    }

    pub(super) fn take_result(&self) -> Option<SearchResult> {
        self.shared
            .result
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }
}

impl Drop for FileSearch {
    fn drop(&mut self) {
        self.shared.cancelled.store(true, AtomicOrdering::Relaxed);
        self.shared.wake.notify_one();
    }
}

fn run_search(
    shared: &SharedSearch,
    snapshot: impl Fn() -> IndexSnapshot,
    mut fallback: Option<IndexSnapshot>,
) {
    let mut previous_query = String::new();
    let mut previous_count = 0;
    let mut previous_generation = None;
    let mut previous_revision = None;
    let mut previous_fallback = false;
    let mut ranked = Vec::<RankedFile>::new();
    let mut published = Instant::now() - Duration::from_secs(1);
    loop {
        if shared.cancelled.load(AtomicOrdering::Relaxed) {
            return;
        }
        let (generation, query) = {
            let request = shared
                .request
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            (
                request.generation,
                FilePickerQuery::parse(&request.query).path.to_string(),
            )
        };
        let live = snapshot();
        if live.done && live.error.is_none() {
            fallback = None;
        }
        let using_fallback = fallback.is_some() && (!live.done || live.error.is_some());
        let current = if using_fallback {
            fallback.as_ref().unwrap()
        } else {
            &live
        };
        let changed =
            previous_generation != Some(generation) || previous_revision != Some(live.revision);
        if !changed
            || (previous_generation == Some(generation)
                && published.elapsed() < Duration::from_millis(100))
        {
            wait_for_query(shared);
            continue;
        }
        let stale = || {
            shared.cancelled.load(AtomicOrdering::Relaxed)
                || shared.generation.load(AtomicOrdering::Relaxed) != generation
        };
        let started = Instant::now();
        let same_index =
            previous_fallback == using_fallback && previous_count <= current.items.len();
        let same_query = same_index && previous_query == query;
        let refining =
            same_index && !previous_query.is_empty() && query.starts_with(&previous_query);
        let compare = |left: &RankedFile, right: &RankedFile| {
            compare_keys(
                left.score,
                &current.items[left.index].id,
                right.score,
                &current.items[right.index].id,
                query.is_empty(),
            )
        };
        let mut next =
            if let Some(order) = current.empty_order.as_ref().filter(|_| query.is_empty()) {
                order
                    .iter()
                    .map(|index| RankedFile {
                        index: *index,
                        score: 0,
                    })
                    .collect()
            } else {
                let mut existing = if same_query {
                    ranked.clone()
                } else if refining {
                    ranked
                        .par_iter()
                        .map_init(SkimMatcherV2::default, |matcher, rank| {
                            if stale() {
                                return None;
                            }
                            file_match_score(matcher, &current.items[rank.index], &query).map(
                                |score| RankedFile {
                                    index: rank.index,
                                    score,
                                },
                            )
                        })
                        .flatten()
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                let start = if same_query || refining {
                    previous_count
                } else {
                    0
                };
                let mut added = current
                    .items
                    .par_entries_from(start)
                    .map_init(SkimMatcherV2::default, |matcher, (index, item)| {
                        if stale() {
                            return None;
                        }
                        let score = if query.is_empty() {
                            Some(0)
                        } else {
                            file_match_score(matcher, item, &query)
                        };
                        score.map(|score| RankedFile { index, score })
                    })
                    .flatten()
                    .collect::<Vec<_>>();
                if stale() {
                    continue;
                }
                added.sort_unstable_by(compare);
                if !same_query {
                    existing.sort_unstable_by(compare);
                }
                merge_ranked(existing, added, compare)
            };
        if stale() {
            continue;
        }
        // Current indexes are append-only, except when a stale cache is replaced.
        previous_count = current.items.len();
        previous_query = query.clone();
        previous_generation = Some(generation);
        previous_revision = Some(live.revision);
        previous_fallback = using_fallback;
        let result = SearchResult {
            generation,
            items: current.items.clone(),
            order: next.iter().map(|rank| rank.index).collect(),
            done: live.done,
            refreshing: using_fallback && !live.done,
            discovered: live.items.len(),
            error: live.error.clone(),
            query,
            ranked: Arc::new(next.clone()),
        };
        crate::log!(
            "[file-picker] query ready: generation={} files={} matches={} elapsed={:?}",
            generation,
            current.items.len(),
            result.order.len(),
            started.elapsed()
        );
        std::mem::swap(&mut ranked, &mut next);
        *shared
            .result
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(result);
        published = Instant::now();
    }
}

fn wait_for_query(shared: &SharedSearch) {
    let request = shared
        .request
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _ = shared.wake.wait_timeout(request, Duration::from_millis(25));
}

fn merge_ranked(
    left: Vec<RankedFile>,
    right: Vec<RankedFile>,
    compare: impl Fn(&RankedFile, &RankedFile) -> Ordering,
) -> Vec<RankedFile> {
    let mut merged = Vec::with_capacity(left.len() + right.len());
    let mut left = left.into_iter().peekable();
    let mut right = right.into_iter().peekable();
    while let (Some(a), Some(b)) = (left.peek(), right.peek()) {
        merged.push(if compare(a, b).is_le() {
            left.next().unwrap()
        } else {
            right.next().unwrap()
        });
    }
    merged.extend(left);
    merged.extend(right);
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Harness {
        search: FileSearch,
        index: Arc<Mutex<IndexSnapshot>>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl Harness {
        fn new(paths: &[&str], done: bool) -> Self {
            let index = Arc::new(Mutex::new(IndexSnapshot {
                items: prepare_file_picker_items(
                    paths.iter().map(|path| path.to_string()).collect(),
                )
                .into(),
                revision: 1,
                done,
                error: None,
                empty_order: None,
            }));
            let shared = Arc::new(SharedSearch::default());
            let worker_state = Arc::clone(&shared);
            let worker_index = Arc::clone(&index);
            let worker = std::thread::spawn(move || {
                run_search(&worker_state, || worker_index.lock().unwrap().clone(), None)
            });
            Self {
                search: FileSearch { shared },
                index,
                worker: Some(worker),
            }
        }

        fn result(&self, generation: u64, count: usize) -> SearchResult {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(result) = self.search.take_result() {
                    if result.generation == generation && result.items.len() == count {
                        return result;
                    }
                }
                assert!(Instant::now() < deadline, "query did not finish");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            self.search
                .shared
                .cancelled
                .store(true, AtomicOrdering::Relaxed);
            self.search.shared.wake.notify_one();
            self.worker.take().unwrap().join().unwrap();
        }
    }

    fn paths(result: &SearchResult) -> Vec<&str> {
        result
            .order
            .iter()
            .map(|index| result.items[*index].id.as_str())
            .collect()
    }

    #[test]
    fn partial_results_include_new_files_without_losing_existing_matches() {
        let harness = Harness::new(&["z/kafka.py", "other.py"], false);
        let generation = harness.search.request("kafka");
        let first = harness.result(generation, 2);
        assert!(!first.done);
        assert_eq!(paths(&first), ["z/kafka.py"]);
        {
            let mut index = harness.index.lock().unwrap();
            index.items = prepare_file_picker_items(vec![
                "z/kafka.py".into(),
                "other.py".into(),
                "a/kafka.py".into(),
            ])
            .into();
            index.revision += 1;
            index.done = true;
        }
        let next = harness.result(generation, 3);
        assert!(next.done);
        assert_eq!(paths(&next), ["a/kafka.py", "z/kafka.py"]);
        assert_eq!(next.selection("z/kafka.py"), Some(1));
        assert_eq!(next.selection("other.py"), None);
    }

    #[test]
    fn asynchronous_matching_preserves_rankings_for_refinement_backspace_and_line_suffixes() {
        let files = [
            "kafka/mod.rs",
            "src/kafka.py",
            "a/kafka.py",
            "z/kafka.py",
            "é space.py",
            "src/main.py",
        ];
        let harness = Harness::new(&files, true);
        let matcher = SkimMatcherV2::default();
        for query in ["", "k", "ka", "kafka", "kafka:42", "ka", "", "src", "é"] {
            let generation = harness.search.request(query);
            let result = harness.result(generation, files.len());
            let query = FilePickerQuery::parse(query).path;
            let mut expected =
                prepare_file_picker_items(files.iter().map(|path| path.to_string()).collect())
                    .into_iter()
                    .filter_map(|item| {
                        let score = if query.is_empty() {
                            Some(0)
                        } else {
                            file_match_score(&matcher, &item, query)
                        }?;
                        Some((score, item.id))
                    })
                    .collect::<Vec<_>>();
            expected.sort_unstable_by(|(a, left), (b, right)| {
                compare_keys(*a, left, *b, right, query.is_empty())
            });
            assert_eq!(
                paths(&result),
                expected
                    .iter()
                    .map(|(_, path)| path.as_str())
                    .collect::<Vec<_>>(),
                "query {query:?}"
            );
        }
    }

    #[test]
    fn rapid_query_changes_publish_only_the_current_generation() {
        let harness = Harness::new(&["alpha.py", "beta.py"], true);
        harness.search.request("alpha");
        harness.search.request("missing");
        let generation = harness.search.request("beta");
        assert_eq!(paths(&harness.result(generation, 2)), ["beta.py"]);
    }
}
