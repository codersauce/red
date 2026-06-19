//! Opt-in performance instrumentation for editor hot paths.
//!
//! `RED_PERF=summary` (or `RED_PERF=1`) records timings in memory and emits a
//! compact histogram when the editor exits. `RED_PERF=trace` also logs each
//! individual span for short, targeted investigations.

use std::{collections::BTreeMap, sync::Mutex, time::Instant};

use once_cell::sync::Lazy;

use crate::log;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Disabled,
    Summary,
    Trace,
}

static MODE: Lazy<Mode> = Lazy::new(|| match std::env::var("RED_PERF") {
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

#[derive(Debug, Default)]
struct Recorder {
    timings: BTreeMap<String, Vec<u128>>,
    counters: BTreeMap<String, u64>,
    gauges: BTreeMap<String, u64>,
}

static RECORDER: Lazy<Mutex<Recorder>> = Lazy::new(|| Mutex::new(Recorder::default()));

pub(crate) fn enabled() -> bool {
    *MODE != Mode::Disabled
}

pub(crate) fn increment(label: impl Into<String>, amount: u64) {
    if !enabled() {
        return;
    }
    let mut recorder = RECORDER.lock().unwrap();
    *recorder.counters.entry(label.into()).or_default() += amount;
}

pub(crate) fn gauge_max(label: impl Into<String>, value: u64) {
    if !enabled() {
        return;
    }
    let mut recorder = RECORDER.lock().unwrap();
    let gauge = recorder.gauges.entry(label.into()).or_default();
    *gauge = (*gauge).max(value);
}

/// Emits one summary for the current process and clears recorded samples.
pub(crate) fn emit_summary() {
    if !enabled() {
        return;
    }

    let mut recorder = RECORDER.lock().unwrap();
    if recorder.timings.is_empty() && recorder.counters.is_empty() && recorder.gauges.is_empty() {
        return;
    }

    log!("[PERF] summary begin");
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
        Some(Self {
            label,
            detail: detail.into(),
            start: Instant::now(),
        })
    }
}

impl Drop for PerfSpan {
    fn drop(&mut self) {
        let micros = self.start.elapsed().as_micros();
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
            log!("[PERF] {key}: {micros}us");
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
