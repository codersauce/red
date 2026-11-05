//! Literal file arguments for Helix's horizontal split command.

use std::ops::Range;

use crate::config::EditingModel;

pub(crate) fn names(model: EditingModel) -> &'static [&'static str] {
    if cfg!(feature = "helix") && model == EditingModel::Helix {
        &["hs", "hsplit"]
    } else {
        &[]
    }
}

pub(crate) struct PathArgument {
    pub(crate) span: Range<usize>,
    pub(crate) value: String,
    pub(crate) unterminated: bool,
}

/// Keep filename spans for completion. A doubled closing quote represents a
/// literal quote; ordinary backslashes in paths are preserved.
pub(crate) fn paths(line: &str) -> impl Iterator<Item = PathArgument> + '_ {
    let mut chars = line.char_indices().peekable();
    std::iter::from_fn(move || {
        while chars.next_if(|(_, ch)| matches!(ch, ' ' | '\t')).is_some() {}
        let &(start, first) = chars.peek()?;
        let mut value = String::new();
        let mut unterminated = false;
        if matches!(first, '\'' | '"' | '`') {
            chars.next();
            unterminated = true;
            while let Some((_, ch)) = chars.next() {
                if ch == first && chars.next_if(|(_, next)| *next == first).is_none() {
                    unterminated = false;
                    break;
                }
                value.push(ch);
            }
        } else {
            while let Some((_, ch)) = chars.next_if(|(_, ch)| !matches!(ch, ' ' | '\t')) {
                if cfg!(unix) && ch == '\\' {
                    if let Some((_, escaped)) = chars.next_if(|(_, next)| {
                        matches!(next, ' ' | '\t')
                            || value.is_empty() && matches!(next, '\'' | '"' | '`' | '%')
                    }) {
                        value.push(escaped);
                        continue;
                    }
                }
                value.push(ch);
            }
        }
        Some(PathArgument {
            span: start..chars.peek().map_or(line.len(), |(index, _)| *index),
            value,
            unterminated,
        })
    })
}

pub(crate) fn quote_path(path: String) -> String {
    if path.contains([' ', '\t']) || path.starts_with(['\'', '"', '`', '%']) {
        format!("'{}'", path.replace('\'', "''"))
    } else {
        path
    }
}
