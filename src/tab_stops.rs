use std::sync::Arc;

/// Tab stops measured from the start of a logical line. Variable widths
/// describe successive intervals, with the final interval repeated.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TabStops {
    width: usize,
    boundaries: Option<Arc<[usize]>>,
}

impl TabStops {
    pub(crate) fn new(width: usize, variable: &[usize]) -> Self {
        let Some(&last) = variable.last() else {
            return width.into();
        };
        let mut end = 0usize;
        let boundaries = variable[..variable.len() - 1]
            .iter()
            .map(|&width| {
                end = end.saturating_add(width.max(1));
                end
            })
            .collect();
        Self {
            width: last.max(1),
            boundaries: Some(boundaries),
        }
    }

    fn interval(&self, column: usize) -> (usize, usize) {
        let Some(boundaries) = self.boundaries.as_deref() else {
            return (0, self.width);
        };
        let index = boundaries.partition_point(|&stop| stop <= column);
        let start = index.checked_sub(1).map_or(0, |i| boundaries[i]);
        let width = boundaries.get(index).map_or(self.width, |end| end - start);
        (start, width)
    }

    pub(crate) fn interval_width(&self, column: usize) -> usize {
        self.interval(column).1
    }

    pub(crate) fn advance(&self, column: usize) -> usize {
        let (start, width) = self.interval(column);
        width - (column - start) % width
    }

    pub(crate) fn next(&self, column: usize) -> usize {
        column.saturating_add(self.advance(column))
    }

    pub(crate) fn at_or_before(&self, column: usize) -> usize {
        let (start, width) = self.interval(column);
        column - (column - start) % width
    }
}

impl From<usize> for TabStops {
    fn from(width: usize) -> Self {
        Self {
            width: width.max(1),
            boundaries: None,
        }
    }
}

impl From<&TabStops> for TabStops {
    fn from(stops: &TabStops) -> Self {
        stops.clone()
    }
}
