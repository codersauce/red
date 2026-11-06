//! FFF indexes owned bytes. File-backed mappings are unsafe for files another process can truncate.
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use crate::stamp::Stamp;
use anyhow::Result;
use fff_search::bigram_filter::{BigramFilter, BigramIndexBuilder};

use crate::{Query, Visibility};

const INDEX_FILES: usize = 8192;
const INDEX_BYTES: usize = 64 * 1024 * 1024;
const MAX_INDEXED_FILE: usize = 512 * 1024;

/// The first query can search immediately while a single background thread
/// builds the reusable filter. New queries do not cancel a compatible build.
#[derive(Default)]
pub(super) struct Cache {
    index: Option<Index>,
    building: Option<Build>,
}

struct Build {
    root: PathBuf,
    visibility: Visibility,
    cancel: Arc<AtomicBool>,
    completed: mpsc::Receiver<Option<Index>>,
}

impl Drop for Build {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

impl Cache {
    pub(super) fn get(&mut self, root: &Path, visibility: Visibility) -> Result<Option<&Index>> {
        if let Some(build) = &self.building {
            if build.root != root || build.visibility != visibility {
                build.cancel.store(true, Ordering::Release);
            }
            match build.completed.try_recv() {
                Ok(index) => {
                    self.index = index;
                    self.building = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.building = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self
            .index
            .as_ref()
            .is_some_and(|i| i.root != root || i.visibility != visibility)
        {
            self.index = None;
        }
        if self.building.is_none() && self.index.as_ref().is_none_or(Index::needs_refresh) {
            let root = root.to_path_buf();
            let scan_root = root.clone();
            let cancel = Arc::new(AtomicBool::new(false));
            let signal = Arc::clone(&cancel);
            let (sender, completed) = mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("workspace-content-index".into())
                .spawn(move || {
                    let _ = sender.send(Index::build(&scan_root, visibility, &signal));
                })?;
            self.building = Some(Build {
                root,
                visibility,
                cancel,
                completed,
            });
        }
        Ok(self.index.as_ref())
    }
}

pub(super) struct Index {
    root: PathBuf,
    visibility: Visibility,
    files: HashMap<PathBuf, (usize, Stamp)>,
    filter: BigramFilter,
    changed: AtomicBool,
    built: Instant,
}

impl Index {
    fn needs_refresh(&self) -> bool {
        self.built.elapsed() >= Duration::from_secs(60)
            || (self.changed.load(Ordering::Relaxed)
                && self.built.elapsed() >= Duration::from_secs(2))
    }

    fn build(root: &Path, visibility: Visibility, cancel: &AtomicBool) -> Option<Self> {
        let builder = BigramIndexBuilder::new(INDEX_FILES);
        let skip = BigramIndexBuilder::new(INDEX_FILES);
        let mut files = HashMap::new();
        let mut bytes_read = 0;
        let mut bytes = Vec::new();
        for entry in visibility.walker(root).build().take(INDEX_FILES).flatten() {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            if bytes_read >= INDEX_BYTES {
                break;
            }
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let Ok(file) = File::open(entry.path()) else {
                continue;
            };
            let Some(stamp) = Stamp::file(&file) else {
                continue;
            };
            if stamp.len > MAX_INDEXED_FILE as u64 {
                continue;
            }
            bytes.clear();
            if (&file)
                .take(MAX_INDEXED_FILE as u64 + 1)
                .read_to_end(&mut bytes)
                .is_err()
            {
                continue;
            }
            bytes_read += bytes.len();
            // Unicode folding and BOM decoding can match characters absent from the raw bytes.
            if bytes.len() as u64 != stamp.len
                || !bytes.is_ascii()
                || bytes.contains(&0)
                || Stamp::file(&file).as_ref() != Some(&stamp)
            {
                continue;
            }
            let slot = files.len();
            builder.add_file_content(&skip, slot, &bytes);
            files.insert(entry.into_path(), (slot, stamp));
        }
        let mut filter = builder.compress(None);
        filter.set_skip_index(skip.compress(None));
        Some(Self {
            root: root.to_owned(),
            visibility,
            files,
            filter,
            changed: AtomicBool::new(false),
            built: Instant::now(),
        })
    }

    pub(super) fn candidates(&self, query: &Query) -> Option<Vec<u64>> {
        if !query.pattern.is_ascii()
            || (query.regex && regex::escape(&query.pattern) != query.pattern)
        {
            return None;
        }
        self.filter.query(query.pattern.as_bytes())
    }

    pub(super) fn can_skip(&self, path: &Path, bits: &[u64]) -> bool {
        let Some((slot, stamp)) = self.files.get(path) else {
            return false;
        };
        if BigramFilter::is_candidate(bits, *slot) {
            return false;
        }
        if Stamp::path(path).as_ref() == Some(stamp) {
            return true;
        }
        self.changed.store(true, Ordering::Relaxed);
        false
    }
}
