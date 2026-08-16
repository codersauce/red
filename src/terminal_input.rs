//! Bounded, order-preserving terminal event collection shared by both clients.

use crate::editor::perf;
use crossterm::event::{self, Event};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const RESIZE_BATCH_BUDGET: Duration = Duration::from_millis(16);
const RESIZE_QUIET_PERIOD: Duration = Duration::from_millis(2);
pub(crate) const RESIZE_EVENTS_PER_BATCH: usize = 64;

/// Reads the next ready terminal event while collapsing only adjacent
/// resize notifications. A short, bounded collection window also catches
/// sizes that arrive while a terminal divider is still moving. A key,
/// paste, or other event ends the run without changing input order.
pub fn read_ready_event(pending_events: &mut VecDeque<Event>) -> anyhow::Result<Option<Event>> {
    let first = if let Some(event) = pending_events.pop_front() {
        event
    } else {
        if !event::poll(Duration::from_millis(0))? {
            return Ok(None);
        }
        event::read()?
    };

    if !matches!(first, Event::Resize(_, _)) {
        return Ok(Some(first));
    }

    read_resize_batch(first, pending_events, RESIZE_BATCH_BUDGET, |timeout| {
        if event::poll(timeout)? {
            Ok(Some(event::read()?))
        } else {
            Ok(None)
        }
    })
    .map(Some)
}

pub(crate) fn read_resize_batch(
    first: Event,
    pending_events: &mut VecDeque<Event>,
    budget: Duration,
    mut read: impl FnMut(Duration) -> anyhow::Result<Option<Event>>,
) -> anyhow::Result<Event> {
    let started = Instant::now();
    let queued_before = pending_events.len();
    let mut latest = coalesce_resize_run(first, pending_events);
    let mut count = 1 + queued_before - pending_events.len();
    while pending_events.is_empty() && count < RESIZE_EVENTS_PER_BATCH {
        let remaining = budget.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break;
        }
        // An isolated resize should not pay the full frame budget. Keep
        // collecting only while more sizes arrive in the same short burst.
        match read(remaining.min(RESIZE_QUIET_PERIOD))? {
            Some(event @ Event::Resize(_, _)) => {
                latest = event;
                count += 1;
            }
            Some(event) => {
                pending_events.push_front(event);
                break;
            }
            None => break,
        }
    }
    perf::increment("resize:batches", 1);
    perf::increment("resize:events_coalesced", count.saturating_sub(1) as u64);
    Ok(latest)
}

pub(crate) fn coalesce_resize_run(first: Event, pending_events: &mut VecDeque<Event>) -> Event {
    let Event::Resize(mut width, mut height) = first else {
        return first;
    };

    while let Some(Event::Resize(next_width, next_height)) = pending_events.front() {
        width = *next_width;
        height = *next_height;
        pending_events.pop_front();
    }

    Event::Resize(width, height)
}
