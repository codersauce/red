//! Ctrl-a and Ctrl-x operate on the entire fragment of each selection.
use super::*;
use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, Timelike};
use std::borrow::Cow;

impl Plan {
    pub(super) fn increment(
        &mut self,
        text: RopeSlice<'_>,
        selections: &Selections,
        mut amount: i64,
        step_per_selection: i64,
    ) {
        let mut shift = 0isize;
        let mut ranges = Vec::with_capacity(selections.ranges().len());
        for &range in selections.ranges() {
            let start = range.start();
            let end = range.end();
            let fragment = Cow::<str>::from(text.slice(start..end));
            let anchor = start.saturating_add_signed(shift);
            if let Some(insert) =
                integer(&fragment, amount).or_else(|| date_time(&fragment, amount))
            {
                // Every recognized number and date format produces ASCII.
                shift += insert.len() as isize - (end - start) as isize;
                self.edits.push(Edit { start, end, insert });
            }
            ranges.push(Range {
                anchor,
                head: end.saturating_add_signed(shift),
            });
            amount = amount.saturating_add(step_per_selection);
        }
        if !self.edits.is_empty() {
            self.ranges = ranges;
            self.next.mode = Mode::Normal;
            self.history = History::Atomic;
        }
    }
}

fn integer(selected: &str, amount: i64) -> Option<String> {
    if !matches!(selected.as_bytes().first(), Some(b'0'..=b'9' | b'+' | b'-'))
        || selected.ends_with('_')
    {
        return None;
    }
    let (prefix, radix) = match selected.get(..2) {
        Some("0x") => ("0x", 16),
        Some("0b") => ("0b", 2),
        Some("0o") => ("0o", 8),
        _ => ("", 10),
    };
    let digits = &selected[prefix.len()..];
    let ungrouped = if digits.contains('_') {
        Cow::Owned(digits.replace('_', ""))
    } else {
        Cow::Borrowed(digits)
    };
    let width = ungrouped.len();
    let adjusted = if radix == 10 {
        let original = ungrouped.parse::<i128>().ok()?;
        let value = original.saturating_add(i128::from(amount));
        if ungrouped.starts_with('0') || ungrouped.starts_with("-0") {
            // Helix discounts existing separators when calculating decimal padding.
            let width = width
                .saturating_sub(digits.len() - width)
                .saturating_sub(usize::from(original < 0))
                + usize::from(value < 0);
            format!("{value:0width$}")
        } else {
            value.to_string()
        }
    } else {
        let original = u128::from_str_radix(&ungrouped, radix).ok()?;
        let value = (original as i128).saturating_add(i128::from(amount)).max(0);
        match radix {
            2 => format!("{value:0width$b}"),
            8 => format!("{value:0width$o}"),
            _ => {
                let uppercase = ungrouped.bytes().filter(u8::is_ascii_uppercase).count();
                let lowercase = ungrouped.bytes().filter(u8::is_ascii_lowercase).count();
                if uppercase > lowercase {
                    format!("{value:0width$X}")
                } else {
                    format!("{value:0width$x}")
                }
            }
        }
    };
    let formatted = if prefix.is_empty() {
        adjusted
    } else {
        format!("{prefix}{adjusted}")
    };
    Some(group_digits(selected, formatted, prefix.len()))
}

/// Reapply the groups from right to left. If a carry adds a group, reuse the
/// width of the last complete group on the left.
fn group_digits(original: &str, adjusted: String, prefix_len: usize) -> String {
    let Some((_, trailing_groups)) = original.split_once('_') else {
        return adjusted;
    };

    let mut groups = Vec::new();
    let mut tail = adjusted.as_str();
    let mut width = 0;
    for part in trailing_groups.rsplit('_') {
        width = part.len();
        if tail.len() <= width {
            break;
        }
        let (rest, group) = tail.split_at(tail.len() - width);
        groups.push(group);
        tail = rest;
    }
    if adjusted.len() + groups.len() > original.len() && width > 0 {
        while tail.len().saturating_sub(prefix_len) > width {
            let (rest, group) = tail.split_at(tail.len() - width);
            groups.push(group);
            tail = rest;
        }
    }
    let mut out = String::from(tail);
    for group in groups.iter().rev() {
        out.push('_');
        out.push_str(group);
    }
    out
}

fn date_time(source: &str, amount: i64) -> Option<String> {
    // These formats have fixed punctuation, field widths, and English names.
    if !(5..=19).contains(&source.len()) || !source.is_ascii() {
        return None;
    }
    for format in [
        "%Y-%m-%d %H:%M:%S",
        "%Y/%m/%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y/%m/%d %H:%M",
    ] {
        if let Ok(value) = NaiveDateTime::parse_from_str(source, format)
            && (0..=9999).contains(&value.year())
            && value.nanosecond() == 0
            && value.format(format).to_string() == source
        {
            return Some(
                value
                    .checked_add_signed(TimeDelta::try_minutes(amount)?)?
                    .format(format)
                    .to_string(),
            );
        }
    }
    for format in [
        "%Y-%m-%d",
        "%Y/%m/%d",
        "%a %b %d %Y",
        "%d-%b-%Y",
        "%Y %b %d",
        "%b %d, %Y",
    ] {
        if let Ok(value) = NaiveDate::parse_from_str(source, format)
            && (0..=9999).contains(&value.year())
            && value.format(format).to_string() == source
        {
            return Some(
                value
                    .checked_add_signed(TimeDelta::try_days(amount)?)?
                    .format(format)
                    .to_string(),
            );
        }
    }
    for format in [
        "%H:%M:%S",
        "%H:%M",
        "%-I:%M:%S %p",
        "%-I:%M %p",
        "%-I:%M:%S %P",
        "%-I:%M %P",
    ] {
        if let Ok(value) = NaiveTime::parse_from_str(source, format)
            && value.nanosecond() == 0
            && value.format(format).to_string() == source
        {
            let (next, _) = value.overflowing_add_signed(TimeDelta::try_minutes(amount)?);
            return Some(next.format(format).to_string());
        }
    }
    None
}
