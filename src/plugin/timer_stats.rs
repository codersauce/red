use std::collections::HashMap;
use std::sync::Mutex;

lazy_static::lazy_static! {
    static ref TIMER_STATS: Mutex<HashMap<String, TimerUsage>> = Mutex::new(HashMap::new());
}

#[derive(Debug, Clone)]
pub struct TimerUsage {
    pub active_timeouts: usize,
    pub active_intervals: usize,
    pub total_created: usize,
    pub total_cleared: usize,
}

impl Default for TimerUsage {
    fn default() -> Self {
        Self {
            active_timeouts: 0,
            active_intervals: 0,
            total_created: 0,
            total_cleared: 0,
        }
    }
}

pub fn record_timer_created(plugin_name: &str, is_interval: bool) {
    let mut stats = TIMER_STATS.lock().unwrap();
    let usage = stats.entry(plugin_name.to_string()).or_default();

    if is_interval {
        usage.active_intervals += 1;
    } else {
        usage.active_timeouts += 1;
    }
    usage.total_created += 1;
}

pub fn record_timer_cleared(plugin_name: &str, is_interval: bool) {
    let mut stats = TIMER_STATS.lock().unwrap();
    if let Some(usage) = stats.get_mut(plugin_name) {
        if is_interval {
            usage.active_intervals = usage.active_intervals.saturating_sub(1);
        } else {
            usage.active_timeouts = usage.active_timeouts.saturating_sub(1);
        }
        usage.total_cleared += 1;
    }
}

pub fn get_timer_stats() -> HashMap<String, TimerUsage> {
    TIMER_STATS.lock().unwrap().clone()
}

pub fn log_timer_stats() {
    let stats = TIMER_STATS.lock().unwrap();

    crate::log!("=== Timer Usage Statistics ===");
    for (plugin, usage) in stats.iter() {
        crate::log!(
            "{}: {} active timeouts, {} active intervals (total created: {}, cleared: {})",
            plugin,
            usage.active_timeouts,
            usage.active_intervals,
            usage.total_created,
            usage.total_cleared
        );
    }

    let total_active: usize = stats
        .values()
        .map(|u| u.active_timeouts + u.active_intervals)
        .sum();
    crate::log!("Total active timers: {}", total_active);
}
