use crate::{Range, Selections};
use regex_automata::{Input, meta::Regex, util::syntax};
use ropey::RopeSlice;
use std::borrow::Cow;

#[derive(Debug, Clone, Copy)]
pub(crate) enum Query {
    Select,
    Split,
    Keep,
    Remove,
    Forward,
    Backward,
}

impl Query {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Select => "select",
            Self::Split => "split",
            Self::Keep => "keep",
            Self::Remove => "remove",
            Self::Forward | Self::Backward => "search",
        }
    }

    pub(crate) fn compile(pattern: &str) -> Option<Regex> {
        if pattern.is_empty() {
            return None;
        }
        Regex::builder()
            .syntax(
                syntax::Config::new()
                    .multi_line(true)
                    .case_insensitive(!pattern.chars().any(char::is_uppercase)),
            )
            .build(pattern)
            .ok()
    }

    pub(crate) fn apply(
        self,
        text: RopeSlice<'_>,
        initial: &Selections,
        regex: &Regex,
        extend: bool,
    ) -> Option<Selections> {
        let contents = Cow::<str>::from(text);
        let input = Input::new(contents.as_bytes());
        let mut ranges = Vec::new();
        if matches!(self, Self::Forward | Self::Backward) {
            let primary = initial.primary();
            let found = if matches!(self, Self::Forward) {
                regex
                    .find(
                        input
                            .clone()
                            .span(text.char_to_byte(primary.end())..text.len_bytes()),
                    )
                    .or_else(|| regex.find(input))
            } else {
                let before = text.char_to_byte(primary.start());
                regex
                    .find_iter(input.clone().span(0..before))
                    .last()
                    .or_else(|| regex.find_iter(input.span(before..text.len_bytes())).last())
            }?;
            if found.end() == 0 {
                return None;
            }
            let range = primary.with_bounds(
                text.byte_to_char(found.start()),
                text.byte_to_char(found.end()),
            );
            ranges.clone_from(&initial.ranges);
            let index = if extend {
                ranges.push(range);
                ranges.len() - 1
            } else {
                ranges[initial.primary] = range;
                initial.primary
            };
            return Selections::new(text, ranges, index);
        }
        for &r in initial.ranges() {
            let scope = input
                .clone()
                .span(text.char_to_byte(r.start())..text.char_to_byte(r.end()));
            if matches!(self, Self::Keep | Self::Remove) {
                if regex.is_match(scope) != matches!(self, Self::Remove) {
                    ranges.push(r);
                }
                continue;
            }
            let mut start = r.start();
            for m in regex.find_iter(scope) {
                let from = text.byte_to_char(m.start());
                let to = text.byte_to_char(m.end());
                if matches!(self, Self::Split) {
                    ranges.push(Range {
                        anchor: start,
                        head: from,
                    });
                    start = to;
                } else if from != r.end() || to != from {
                    ranges.push(Range {
                        anchor: from,
                        head: to,
                    });
                }
            }
            if matches!(self, Self::Split) && (start < r.end() || r.start() == r.end()) {
                ranges.push(Range {
                    anchor: start,
                    head: r.end(),
                });
            }
        }
        Selections::new(text, ranges, 0)
    }
}
