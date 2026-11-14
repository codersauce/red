use crate::{config::IndentationConfig, tab_stops::TabStops};

#[derive(Debug, Clone)]
pub(super) struct Indentation {
    pub shift_width: usize,
    pub tab_width: usize,
    pub tab_stops: TabStops,
    pub expand_tab: bool,
    soft_stops: Option<TabStops>,
    shift_follows_tab: bool,
    soft_follows_shift: bool,
    soft_stops_configured: bool,
    smart_tab: bool,
    round_shifts: bool,
    copy_indent: bool,
    preserve_indent: bool,
}

impl Indentation {
    pub fn resolve(baseline: usize, config: &IndentationConfig) -> Self {
        let tab_width = config.tabstop.unwrap_or(baseline);
        let tab_stops = TabStops::new(tab_width, config.vartabstop.as_deref().unwrap_or_default());
        let shift_follows_tab = config.shiftwidth == Some(0);
        // Consumers with a single width (LSP and the plugin viewport) use
        // the shift width at the start of the line.
        let shift_width = if shift_follows_tab {
            tab_stops.interval_width(0)
        } else {
            config.shiftwidth.unwrap_or(baseline)
        };
        let soft_width = match config.softtabstop {
            Some(width) if width < 0 => shift_width,
            Some(width) => width as usize,
            None => baseline,
        };
        let variable_soft = config.varsofttabstop.as_deref().unwrap_or_default();
        let soft_follows_shift =
            config.softtabstop.is_some_and(|width| width < 0) && variable_soft.is_empty();
        let soft_stops = (soft_width != 0 || !variable_soft.is_empty())
            .then(|| TabStops::new(soft_width, variable_soft));

        Self {
            shift_width,
            tab_width: tab_stops.interval_width(0),
            tab_stops,
            expand_tab: config.expandtab.unwrap_or(true),
            soft_stops,
            shift_follows_tab,
            soft_follows_shift,
            soft_stops_configured: config.softtabstop.is_some() || !variable_soft.is_empty(),
            smart_tab: config.smarttab.unwrap_or(false),
            round_shifts: config.shiftround.unwrap_or(false),
            copy_indent: config.copyindent.unwrap_or(false),
            preserve_indent: config.preserveindent.unwrap_or(false),
        }
    }

    pub fn shift_width_at(&self, column: usize) -> usize {
        if self.shift_follows_tab {
            self.tab_stops.interval_width(column)
        } else {
            self.shift_width
        }
    }

    pub fn shift(&self, column: usize, count: u16, right: bool) -> usize {
        let width = self.shift_width_at(column);
        if self.round_shifts {
            let stops = self.shift_stops();
            let mut column = column;
            for _ in 0..count.max(1) {
                column = if right {
                    stops.next(column)
                } else {
                    stops.at_or_before(column.saturating_sub(1))
                };
            }
            column
        } else {
            let amount = width.saturating_mul(usize::from(count.max(1)));
            if right {
                column.saturating_add(amount)
            } else {
                column.saturating_sub(amount)
            }
        }
    }

    fn shift_stops(&self) -> TabStops {
        if self.shift_follows_tab {
            self.tab_stops.clone()
        } else {
            self.shift_width.into()
        }
    }

    pub fn editing_stops(&self, leading: bool, column: usize) -> Option<TabStops> {
        if (self.smart_tab && leading) || self.soft_follows_shift {
            Some(self.shift_width_at(column).into())
        } else {
            self.soft_stops.clone()
        }
    }

    pub fn backspace_stops(&self, leading: bool, column: usize) -> Option<TabStops> {
        if leading || self.soft_stops_configured {
            self.editing_stops(leading, column)
        } else {
            None
        }
    }

    pub fn whitespace(&self, mut start: usize, end: usize) -> String {
        if self.expand_tab {
            return " ".repeat(end.saturating_sub(start));
        }
        let mut text = String::new();
        while start < end {
            let next = self.tab_stops.next(start);
            if next > end {
                break;
            }
            text.push('\t');
            start = next;
        }
        text.extend(std::iter::repeat_n(' ', end.saturating_sub(start)));
        text
    }

    pub fn whitespace_for_columns(&self, columns: usize) -> String {
        self.whitespace(0, columns)
    }

    pub fn autoindent(&self, source: &str) -> String {
        if self.copy_indent {
            source.to_owned()
        } else {
            self.whitespace_for_columns(crate::unicode_utils::display_width_with_tabs(
                source,
                &self.tab_stops,
            ))
        }
    }

    pub fn update_indent(&self, existing: &str, target: usize, autoindent: bool) -> String {
        if !(self.preserve_indent || autoindent && self.copy_indent) {
            return self.whitespace_for_columns(target);
        }
        let mut column = 0;
        let mut end = 0;
        for (index, grapheme) in
            unicode_segmentation::UnicodeSegmentation::grapheme_indices(existing, true)
        {
            let next = if grapheme == "\t" {
                self.tab_stops.next(column)
            } else {
                column + crate::unicode_utils::display_width(grapheme)
            };
            if next > target {
                break;
            }
            column = next;
            end = index + grapheme.len();
        }
        let mut text = existing[..end].to_owned();
        text.push_str(&self.whitespace(column, target));
        text
    }
}
