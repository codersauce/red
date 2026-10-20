//! Editor-owned file indexes. Completed snapshots survive picker lifetimes; workers
//! and query results share immutable batches instead of copying the workspace.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, Weak,
    },
    time::{Duration, Instant},
};

use crate::{log, workspace_paths::stream_workspace_files};

use super::{super::picker_items::PickerItems, prepare_file_picker_items, FilePickerVisibility};

const CACHE_AGE: Duration = Duration::from_secs(30);
const CACHE_BYTES: usize = 1024 * 1024 * 1024;
const CACHE_ROOTS: usize = 4;

type CacheKey = (PathBuf, FilePickerVisibility);

#[derive(Default)]
pub(crate) struct FilePickerCache {
    entries: Mutex<HashMap<CacheKey, CacheEntry>>,
}

struct CacheEntry {
    scan: Arc<FileScan>,
    previous: Option<Arc<FileScan>>,
    used: Instant,
    dirty: bool,
}

impl FilePickerCache {
    pub(super) fn acquire(
        self: &Arc<Self>,
        root: &Path,
        visibility: FilePickerVisibility,
        refresh: bool,
    ) -> ScanLease {
        let key = (
            root.canonicalize().unwrap_or_else(|_| root.to_path_buf()),
            visibility,
        );
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut fallback = None;
        let mut previous = None;
        if let Some(entry) = entries.get_mut(&key) {
            let data = entry
                .scan
                .data
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let fresh = data
                .finished
                .is_some_and(|finished| finished.elapsed() < CACHE_AGE);
            let running = !data.done && !entry.scan.cancelled.load(Ordering::Relaxed);
            if !refresh && !entry.dirty && (fresh || running) {
                entry.used = Instant::now();
                return ScanLease::new(
                    Arc::clone(&entry.scan),
                    entry.previous.as_ref().map(|scan| scan.snapshot()),
                    Arc::downgrade(self),
                );
            }
            if data.finished.is_some() {
                fallback = Some(data.snapshot());
                previous = Some(Arc::clone(&entry.scan));
            } else if let Some(scan) = &entry.previous {
                fallback = Some(scan.snapshot());
                previous = Some(Arc::clone(scan));
            }
        }
        let scan = Arc::new(FileScan::default());
        let lease = ScanLease::new(Arc::clone(&scan), fallback, Arc::downgrade(self));
        let retired = entries.insert(
            key.clone(),
            CacheEntry {
                scan: Arc::clone(&scan),
                previous,
                used: Instant::now(),
                dirty: false,
            },
        );
        drop(entries);
        if let Some(retired) = retired {
            rayon::spawn(move || drop(retired));
        }
        let cache = Arc::downgrade(self);
        std::thread::spawn(move || {
            let started = Instant::now();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                stream_workspace_files(
                    &key.0,
                    visibility.hidden,
                    visibility.ignored,
                    &scan.cancelled,
                    |paths| {
                        let items = prepare_file_picker_items(
                            paths.into_iter().map(|path| path.path).collect(),
                        );
                        let bytes = items
                            .iter()
                            .map(|item| {
                                std::mem::size_of_val(item)
                                    + item.id.len()
                                    + item.label.len()
                                    + item.annotation.as_ref().map_or(0, String::len)
                                    + 8
                            })
                            .sum::<usize>();
                        let mut data = scan.data.lock().unwrap_or_else(|error| error.into_inner());
                        if data.count == 0 {
                            log!("[file-picker] first batch after {:?}", started.elapsed());
                        }
                        data.count += items.len();
                        data.bytes += bytes;
                        data.chunks.push(items.into());
                        data.revision += 1;
                        !scan.cancelled.load(Ordering::Relaxed)
                    },
                )
            }));
            let error = match result {
                Ok(Ok(true)) => None,
                Ok(Ok(false)) => Some("File discovery cancelled".to_string()),
                Ok(Err(error)) => Some(error.to_string()),
                Err(_) => Some("File discovery worker panicked".to_string()),
            };
            let items = scan.snapshot().items;
            let empty_order = if error.is_none() {
                let mut order = (0..items.len()).collect::<Vec<_>>();
                order.sort_unstable_by(|left, right| items[*left].id.cmp(&items[*right].id));
                Some(Arc::new(order))
            } else {
                None
            };
            let mut data = scan.data.lock().unwrap_or_else(|error| error.into_inner());
            data.done = true;
            data.finished = error.is_none().then(Instant::now);
            data.error = error;
            data.empty_order = empty_order;
            data.revision += 1;
            log!(
                "[file-picker] discovery finished: files={} elapsed={:?} complete={}",
                data.count,
                started.elapsed(),
                data.finished.is_some()
            );
            let complete = data.finished.is_some();
            drop(data);
            if let Some(cache) = cache.upgrade() {
                if complete {
                    let mut entries = cache
                        .entries
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    let retired = entries
                        .get_mut(&key)
                        .filter(|entry| Arc::ptr_eq(&entry.scan, &scan))
                        .and_then(|entry| entry.previous.take());
                    drop(entries);
                    drop(retired);
                }
                cache.prune();
            }
        });
        self.prune();
        lease
    }

    /// Keep old results available, but force a refresh after editor-owned changes.
    pub(crate) fn invalidate(&self, path: &Path) {
        let path = canonical_event_path(path);
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for ((root, _), entry) in entries.iter_mut() {
            if path.starts_with(root) || root.starts_with(&path) {
                entry.dirty = true;
            }
        }
    }

    /// Editing an existing source file does not change the file index. Only
    /// new names and ignore-policy files require discovery to run again.
    pub(crate) fn file_saved(&self, path: &Path) {
        let path = canonical_event_path(path);
        if matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some(".gitignore" | ".ignore" | "exclude")
        ) {
            self.invalidate(path.parent().unwrap_or(&path));
            return;
        }
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for ((root, _), entry) in entries.iter_mut() {
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let relative = relative.to_string_lossy().replace('\\', "/");
            let snapshot = entry.scan.snapshot();
            let known = snapshot.empty_order.as_ref().is_some_and(|order| {
                order
                    .binary_search_by(|index| snapshot.items[*index].id.as_str().cmp(&relative))
                    .is_ok()
            });
            if !known {
                entry.dirty = true;
            }
        }
    }

    fn release(&self, scan: &Arc<FileScan>) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut retired = None;
        for entry in entries.values_mut() {
            if Arc::ptr_eq(&entry.scan, scan)
                && scan
                    .data
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .finished
                    .is_none()
            {
                if let Some(previous) = entry.previous.take() {
                    retired = Some(std::mem::replace(&mut entry.scan, previous));
                    entry.dirty = true;
                }
                break;
            }
        }
        drop(entries);
        if let Some(retired) = retired {
            rayon::spawn(move || drop(retired));
        }
        self.prune();
    }

    fn prune(&self) {
        let mut retired = Vec::new();
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        loop {
            let bytes = entries
                .values()
                .map(|entry| {
                    let bytes = entry
                        .scan
                        .data
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .bytes;
                    bytes
                        + entry.previous.as_ref().map_or(0, |scan| {
                            scan.data
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .bytes
                        })
                })
                .sum::<usize>();
            if entries.len() <= CACHE_ROOTS && bytes <= CACHE_BYTES {
                break;
            }
            let oldest = entries
                .iter()
                .filter(|(_, entry)| entry.scan.leases.load(Ordering::Relaxed) == 0)
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| key.clone());
            let Some(oldest) = oldest else {
                break;
            };
            retired.extend(entries.remove(&oldest));
        }
        drop(entries);
        if !retired.is_empty() {
            rayon::spawn(move || drop(retired));
        }
    }
}

fn canonical_event_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    // Resolve existing ancestors even when a deleted file no longer exists.
    let start = if absolute.is_dir() {
        absolute.as_path()
    } else {
        absolute.parent().unwrap_or(&absolute)
    };
    for ancestor in start.ancestors() {
        if let Ok(canonical) = ancestor.canonicalize() {
            return canonical.join(absolute.strip_prefix(ancestor).unwrap());
        }
    }
    absolute
}

/// Only UI subscriptions count as leases. A query worker must not keep its own
/// discovery alive after the dialog is closed.
pub(super) struct ScanLease {
    pub(super) scan: Arc<FileScan>,
    pub(super) fallback: Option<IndexSnapshot>,
    cache: Weak<FilePickerCache>,
}

impl ScanLease {
    fn new(
        scan: Arc<FileScan>,
        fallback: Option<IndexSnapshot>,
        cache: Weak<FilePickerCache>,
    ) -> Self {
        scan.leases.fetch_add(1, Ordering::Relaxed);
        Self {
            scan,
            fallback,
            cache,
        }
    }
}

impl Drop for ScanLease {
    fn drop(&mut self) {
        if self.scan.leases.fetch_sub(1, Ordering::Relaxed) == 1 {
            self.scan.cancelled.store(true, Ordering::Relaxed);
            if let Some(cache) = self.cache.upgrade() {
                cache.release(&self.scan);
            }
        }
    }
}

#[derive(Default)]
pub(super) struct FileScan {
    data: Mutex<ScanData>,
    cancelled: AtomicBool,
    leases: AtomicUsize,
}

impl FileScan {
    pub(super) fn snapshot(&self) -> IndexSnapshot {
        self.data
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .snapshot()
    }
}

#[derive(Default)]
struct ScanData {
    chunks: Vec<Arc<[super::super::PickerItem]>>,
    count: usize,
    bytes: usize,
    revision: u64,
    done: bool,
    finished: Option<Instant>,
    error: Option<String>,
    empty_order: Option<Arc<Vec<usize>>>,
}

impl ScanData {
    fn snapshot(&self) -> IndexSnapshot {
        IndexSnapshot {
            items: PickerItems::from_chunks(self.chunks.clone()),
            revision: self.revision,
            done: self.done,
            error: self.error.clone(),
            empty_order: self.empty_order.clone(),
        }
    }
}

#[derive(Clone)]
pub(super) struct IndexSnapshot {
    pub(super) items: PickerItems,
    pub(super) revision: u64,
    pub(super) done: bool,
    pub(super) error: Option<String>,
    pub(super) empty_order: Option<Arc<Vec<usize>>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finish(lease: &ScanLease) -> IndexSnapshot {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = lease.scan.snapshot();
            if snapshot.done {
                return snapshot;
            }
            assert!(Instant::now() < deadline, "discovery did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn cache_reuses_complete_indexes_and_coalesces_live_requests() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.py"), "").unwrap();
        let cache = Arc::new(FilePickerCache::default());
        let first = cache.acquire(root.path(), FilePickerVisibility::default(), false);
        let second = cache.acquire(root.path(), FilePickerVisibility::default(), false);
        assert!(Arc::ptr_eq(&first.scan, &second.scan));
        drop(second);
        assert!(!first.scan.cancelled.load(Ordering::Relaxed));
        let snapshot = finish(&first);
        assert_eq!(snapshot.items.len(), 1);
        let scan = Arc::clone(&first.scan);
        drop(first);
        let reopened = cache.acquire(root.path(), FilePickerVisibility::default(), false);
        assert!(Arc::ptr_eq(&scan, &reopened.scan));
        assert!(reopened.scan.snapshot().done);
    }

    #[test]
    fn refresh_and_invalidation_keep_old_results_until_replacement_finishes() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.py"), "").unwrap();
        let cache = Arc::new(FilePickerCache::default());
        let first = cache.acquire(root.path(), FilePickerVisibility::default(), false);
        finish(&first);
        std::fs::write(root.path().join("b.py"), "").unwrap();
        cache.invalidate(&root.path().join("b.py"));
        let next = cache.acquire(root.path(), FilePickerVisibility::default(), false);
        assert!(!Arc::ptr_eq(&first.scan, &next.scan));
        assert_eq!(next.fallback.as_ref().unwrap().items.len(), 1);
        assert_eq!(finish(&next).items.len(), 2);
        let refreshed = cache.acquire(root.path(), FilePickerVisibility::default(), true);
        assert!(!Arc::ptr_eq(&next.scan, &refreshed.scan));
        assert_eq!(refreshed.fallback.as_ref().unwrap().items.len(), 2);
    }

    #[test]
    fn cache_expires_and_separates_roots_and_visibility() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.py"), "").unwrap();
        std::fs::write(root.path().join(".hidden.py"), "").unwrap();
        let cache = Arc::new(FilePickerCache::default());
        let first = cache.acquire(root.path(), FilePickerVisibility::default(), false);
        finish(&first);
        first.scan.data.lock().unwrap().finished = Some(Instant::now() - CACHE_AGE);
        let expired = cache.acquire(root.path(), FilePickerVisibility::default(), false);
        assert!(!Arc::ptr_eq(&first.scan, &expired.scan));
        let hidden = cache.acquire(
            root.path(),
            FilePickerVisibility {
                hidden: true,
                ignored: true,
            },
            false,
        );
        assert_eq!(finish(&hidden).items.len(), 2);
        let other = cache.acquire(other.path(), FilePickerVisibility::default(), false);
        assert_eq!(finish(&other).items.len(), 0);
    }

    #[test]
    fn closing_last_subscriber_cancels_and_oversized_cache_entries_are_evicted() {
        let root = tempfile::tempdir().unwrap();
        let cache = Arc::new(FilePickerCache::default());
        let lease = cache.acquire(root.path(), FilePickerVisibility::default(), false);
        finish(&lease);
        let scan = Arc::clone(&lease.scan);
        scan.data.lock().unwrap().bytes = CACHE_BYTES + 1;
        cache.prune();
        assert_eq!(cache.entries.lock().unwrap().len(), 1);
        drop(lease);
        assert!(scan.cancelled.load(Ordering::Relaxed));
        assert!(cache.entries.lock().unwrap().is_empty());
    }

    #[test]
    fn failed_scans_are_terminal_and_are_not_reused() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing");
        let cache = Arc::new(FilePickerCache::default());
        let first = cache.acquire(&missing, FilePickerVisibility::default(), false);
        assert!(finish(&first).error.is_some());
        std::fs::create_dir(&missing).unwrap();
        let retry = cache.acquire(&missing, FilePickerVisibility::default(), false);
        assert!(!Arc::ptr_eq(&first.scan, &retry.scan));
        assert!(finish(&retry).error.is_none());
    }

    #[test]
    fn saves_invalidate_new_names_and_ignore_rules_but_not_existing_contents() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("file.py");
        std::fs::write(&file, "").unwrap();
        let cache = Arc::new(FilePickerCache::default());
        let first = cache.acquire(root.path(), FilePickerVisibility::default(), false);
        finish(&first);
        cache.file_saved(&file);
        let unchanged = cache.acquire(root.path(), FilePickerVisibility::default(), false);
        assert!(Arc::ptr_eq(&first.scan, &unchanged.scan));
        let created = root.path().join("new.py");
        std::fs::write(&created, "").unwrap();
        cache.file_saved(&created);
        let refreshed = cache.acquire(root.path(), FilePickerVisibility::default(), false);
        assert!(!Arc::ptr_eq(&first.scan, &refreshed.scan));
        finish(&refreshed);
        std::fs::remove_file(&created).unwrap();
        cache.invalidate(&created);
        let deleted = cache.acquire(root.path(), FilePickerVisibility::default(), false);
        assert_eq!(finish(&deleted).items.len(), 1);
        cache.file_saved(&root.path().join(".gitignore"));
        let ignored = cache.acquire(root.path(), FilePickerVisibility::default(), false);
        assert!(!Arc::ptr_eq(&deleted.scan, &ignored.scan));
    }

    #[test]
    fn cancelled_refresh_preserves_the_last_complete_index() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("file.py"), "").unwrap();
        let cache = Arc::new(FilePickerCache::default());
        let complete = cache.acquire(root.path(), FilePickerVisibility::default(), false);
        finish(&complete);
        let key = (
            root.path().canonicalize().unwrap(),
            FilePickerVisibility::default(),
        );
        // An unpublished scan makes the cancellation race deterministic.
        let pending = Arc::new(FileScan::default());
        {
            let mut entries = cache.entries.lock().unwrap();
            let entry = entries.get_mut(&key).unwrap();
            entry.previous = Some(Arc::clone(&complete.scan));
            entry.scan = Arc::clone(&pending);
        }
        let refresh = cache.acquire(root.path(), FilePickerVisibility::default(), false);
        assert!(Arc::ptr_eq(&refresh.scan, &pending));
        assert_eq!(refresh.fallback.as_ref().unwrap().items.len(), 1);
        drop(refresh);
        assert!(pending.cancelled.load(Ordering::Relaxed));
        {
            let entries = cache.entries.lock().unwrap();
            let entry = entries.get(&key).unwrap();
            assert!(Arc::ptr_eq(&entry.scan, &complete.scan));
            assert!(entry.dirty);
        }
        let reopened = cache.acquire(root.path(), FilePickerVisibility::default(), false);
        assert_eq!(reopened.fallback.as_ref().unwrap().items.len(), 1);
        assert_eq!(finish(&reopened).items.len(), 1);
    }
}
