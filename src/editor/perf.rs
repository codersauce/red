//! Opt-in performance instrumentation for editor hot paths.
//!
//! `RED_PERF=summary` (or `RED_PERF=1`) records timings in memory and emits a
//! compact histogram when the editor exits. `RED_PERF=trace` also logs each
//! individual span for short, targeted investigations. `RED_PERF=bounded`
//! aggregates all timings by static span name into fixed-size buckets.

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::Instant,
};

use once_cell::sync::Lazy;

use crate::log;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Disabled,
    Summary,
    Bounded,
    Trace,
}

static MODE: Lazy<Mode> = Lazy::new(|| match std::env::var("RED_PERF") {
    Ok(value) if value.eq_ignore_ascii_case("bounded") => Mode::Bounded,
    Ok(value) if value.eq_ignore_ascii_case("trace") => Mode::Trace,
    Ok(value)
        if value == "1"
            || value.eq_ignore_ascii_case("true")
            || value.eq_ignore_ascii_case("summary") =>
    {
        Mode::Summary
    }
    _ => Mode::Disabled,
});

// Inclusive bucket bounds in microseconds. Keep sub-millisecond resolution for
// cursor work and exact counts above the 8, 16, and 32 ms interaction budgets.
const BOUNDS_US: [u64; 32] = [
    1,
    2,
    4,
    8,
    16,
    25,
    40,
    63,
    100,
    160,
    250,
    400,
    630,
    1_000,
    1_500,
    2_000,
    3_000,
    4_000,
    6_000,
    8_000,
    10_000,
    12_000,
    16_000,
    20_000,
    25_000,
    32_000,
    50_000,
    100_000,
    250_000,
    500_000,
    1_000_000,
    u64::MAX,
];

#[derive(Debug, Default, serde::Serialize)]
struct TimingBuckets {
    counts: [u64; 32],
    max_us: u64,
}

impl TimingBuckets {
    fn record(&mut self, elapsed: u128) {
        let us = u64::try_from(elapsed).unwrap_or(u64::MAX);
        self.counts[BOUNDS_US.partition_point(|&bound| bound < us)] += 1;
        self.max_us = self.max_us.max(us);
    }
}

#[derive(Debug, Default)]
struct Recorder {
    bounded: BTreeMap<&'static str, TimingBuckets>,
    timings: BTreeMap<String, Vec<u128>>,
    counters: BTreeMap<&'static str, u64>,
    gauges: BTreeMap<&'static str, u64>,
}

static TRACE_START: Lazy<Instant> = Lazy::new(Instant::now);
static NEXT_SPAN: AtomicU64 = AtomicU64::new(1);

static RECORDER: Lazy<Mutex<Recorder>> = Lazy::new(|| Mutex::new(Recorder::default()));

pub(crate) fn enabled() -> bool {
    *MODE != Mode::Disabled
}

pub(crate) fn increment(label: &'static str, amount: u64) {
    if !enabled() {
        return;
    }
    let mut recorder = RECORDER.lock().unwrap();
    *recorder.counters.entry(label).or_default() += amount;
}

pub(crate) fn gauge_max(label: &'static str, value: u64) {
    if !enabled() {
        return;
    }
    let mut recorder = RECORDER.lock().unwrap();
    let gauge = recorder.gauges.entry(label).or_default();
    *gauge = (*gauge).max(value);
}

/// Correlates editor dispatch with successful terminal flushes in short traces.
#[derive(Debug, Default)]
pub(super) struct InputTrace {
    input: u64,
    flush: u64,
    first_pending: Option<(u64, Instant)>,
    last_dispatch: Option<Instant>,
}

impl InputTrace {
    pub(super) fn start() -> Option<Self> {
        (*MODE == Mode::Trace).then(Self::default)
    }

    pub(super) fn dispatch(&mut self) {
        let now = Instant::now();
        self.input += 1;
        self.first_pending.get_or_insert((self.input, now));
        self.last_dispatch = Some(now);
        log!(
            "[PERF] dispatch input={} t={}us",
            self.input,
            TRACE_START.elapsed().as_micros()
        );
    }

    pub(super) fn flushed(
        &mut self,
        cursor: (usize, usize),
        viewport: (usize, usize),
        buffer: u64,
        revision: u64,
    ) {
        self.flush += 1;
        let pending = self.first_pending.take().zip(self.last_dispatch.take());
        if let Some(((first, oldest), newest)) = pending {
            let now = Instant::now();
            log!("[PERF] flush id={} inputs={}..={} oldest_us={} newest_us={} cursor={cursor:?} viewport={viewport:?} buffer={buffer} revision={revision}",
                self.flush, first, self.input, (now-oldest).as_micros(), (now-newest).as_micros());
        } else {
            log!("[PERF] flush id={} inputs=none cursor={cursor:?} viewport={viewport:?} buffer={buffer} revision={revision}", self.flush);
        }
    }
}

/// Emits one summary for the current process and clears recorded samples.
pub(crate) fn emit_summary() {
    if !enabled() {
        return;
    }

    let mut recorder = RECORDER.lock().unwrap();
    if recorder.bounded.is_empty()
        && recorder.timings.is_empty()
        && recorder.counters.is_empty()
        && recorder.gauges.is_empty()
    {
        return;
    }

    log!("[PERF] summary begin");
    if *MODE == Mode::Bounded {
        let report = serde_json::json!({
            "upper_bounds_us": BOUNDS_US,
            "dropped_samples": 0,
            "timings": recorder.bounded,
            "counters": recorder.counters,
            "gauges": recorder.gauges,
        });
        log!("[PERF] bounded {report}");
    }
    for (label, samples) in &mut recorder.timings {
        samples.sort_unstable();
        let count = samples.len();
        let p50 = percentile(samples, 50);
        let p95 = percentile(samples, 95);
        let p99 = percentile(samples, 99);
        let max = samples.last().copied().unwrap_or(0);
        log!(
            "[PERF] timing {label}: count={count} p50={p50}us p95={p95}us p99={p99}us max={max}us"
        );
    }
    for (label, value) in &recorder.counters {
        log!("[PERF] counter {label}: {value}");
    }
    for (label, value) in &recorder.gauges {
        log!("[PERF] gauge {label}: {value}");
    }
    log!("[PERF] summary end");
    *recorder = Recorder::default();
}

fn percentile(samples: &[u128], percentile: usize) -> u128 {
    if samples.is_empty() {
        return 0;
    }
    let index = (samples.len() - 1) * percentile / 100;
    samples[index]
}

/// Emits a performance summary when an editor session ends.
pub(crate) struct PerfSession;

impl PerfSession {
    pub(crate) fn start() -> Option<Self> {
        enabled().then_some(Self)
    }
}

impl Drop for PerfSession {
    fn drop(&mut self) {
        emit_summary();
    }
}

/// Times a section of code and records it for the session summary.
pub(crate) struct PerfSpan {
    id: u64,
    label: &'static str,
    detail: String,
    start: Instant,
}

impl PerfSpan {
    pub(crate) fn start(label: &'static str) -> Option<Self> {
        Self::with_detail(label, String::new())
    }

    pub(crate) fn with_detail(label: &'static str, detail: impl Into<String>) -> Option<Self> {
        if !enabled() {
            return None;
        }
        let id = NEXT_SPAN.fetch_add(1, Ordering::Relaxed);
        let detail = detail.into();
        if *MODE == Mode::Trace {
            log!(
                "[PERF] begin id={id} t={}us thread={:?} {label} {detail}",
                TRACE_START.elapsed().as_micros(),
                std::thread::current().id()
            );
        }
        Some(Self {
            id,
            label,
            detail,
            start: Instant::now(),
        })
    }
}

impl Drop for PerfSpan {
    fn drop(&mut self) {
        let micros = self.start.elapsed().as_micros();
        if *MODE == Mode::Bounded {
            RECORDER
                .lock()
                .unwrap()
                .bounded
                .entry(self.label)
                .or_default()
                .record(micros);
            return;
        }
        let key = if self.detail.is_empty() {
            self.label.to_string()
        } else {
            format!("{} {}", self.label, self.detail)
        };
        RECORDER
            .lock()
            .unwrap()
            .timings
            .entry(key.clone())
            .or_default()
            .push(micros);
        if *MODE == Mode::Trace {
            log!(
                "[PERF] end id={} t={}us {key}: {micros}us",
                self.id,
                TRACE_START.elapsed().as_micros()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::percentile;

    #[test]
    fn percentile_uses_stable_nearest_rank() {
        let samples = [1, 2, 3, 4, 100];
        assert_eq!(percentile(&samples, 50), 3);
        assert_eq!(percentile(&samples, 95), 4);
        assert_eq!(percentile(&samples, 99), 4);
    }
}
