//! Opt-in performance instrumentation, enabled with the `RED_PERF`
//! environment variable. When disabled it costs one branch per span.

use std::time::Instant;

use once_cell::sync::Lazy;

use crate::log;

static PERF_ENABLED: Lazy<bool> = Lazy::new(|| std::env::var_os("RED_PERF").is_some());

pub(crate) fn enabled() -> bool {
    *PERF_ENABLED
}

/// Times a section of code and logs `[PERF] <label> <detail>: <µs>` on drop.
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
        if self.detail.is_empty() {
            log!("[PERF] {}: {}us", self.label, micros);
        } else {
            log!("[PERF] {} {}: {}us", self.label, self.detail, micros);
        }
    }
}
