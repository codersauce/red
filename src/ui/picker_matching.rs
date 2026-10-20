//! Shared fuzzy matching for picker rows that represent filesystem paths.

use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};

use super::picker::PickerFilterHighlights;

const FILENAME_MATCH_BONUS: i64 = 1;

#[derive(Clone, Copy)]
pub(super) struct PathCandidate<'a> {
    path: &'a str,
    filename: &'a str,
    parent: Option<&'a str>,
}

impl<'a> PathCandidate<'a> {
    pub(super) fn new(path: &'a str, filename: &'a str, parent: Option<&'a str>) -> Self {
        Self {
            path,
            filename,
            parent,
        }
    }

    pub(super) fn from_path(path: &'a str) -> Self {
        let Some(separator) = path.rfind(['/', '\\']) else {
            return Self::new(path, path, None);
        };
        let (parent, filename) = path.split_at(separator);

        Self::new(path, &filename[1..], (!parent.is_empty()).then_some(parent))
    }

    pub(super) fn has_parent(self) -> bool {
        self.parent.is_some()
    }
}

pub(super) struct PathMatch {
    pub(super) score: i64,
    pub(super) filename_score: Option<i64>,
}

pub(super) fn match_path(
    matcher: &SkimMatcherV2,
    candidate: PathCandidate<'_>,
    query: &str,
) -> Option<PathMatch> {
    if !fuzzy_subsequence_matches(candidate.filename, query) {
        if !fuzzy_subsequence_matches(candidate.path, query) {
            return None;
        }
        return Some(PathMatch {
            score: matcher.fuzzy_match(candidate.path, query)?,
            filename_score: None,
        });
    }

    let filename_score = matcher
        .fuzzy_match(candidate.filename, query)?
        .saturating_add(FILENAME_MATCH_BONUS);
    let query_matches_parent = candidate
        .parent
        .is_some_and(|parent| fuzzy_subsequence_matches(parent, query));
    let score = if query_matches_parent {
        matcher
            .fuzzy_match(candidate.path, query)?
            .max(filename_score)
    } else {
        filename_score
    };

    Some(PathMatch {
        score,
        filename_score: Some(filename_score),
    })
}

pub(super) fn path_match_highlights(
    matcher: &SkimMatcherV2,
    candidate: PathCandidate<'_>,
    query: &str,
) -> PickerFilterHighlights {
    let Some((filename_score, filename_indices)) = matcher.fuzzy_indices(candidate.filename, query)
    else {
        return matcher
            .fuzzy_indices(candidate.path, query)
            .map(|(_, indices)| split_path_highlights(candidate, indices))
            .unwrap_or_default();
    };
    let filename_score = filename_score.saturating_add(FILENAME_MATCH_BONUS);
    let query_matches_parent = candidate
        .parent
        .is_some_and(|parent| fuzzy_subsequence_matches(parent, query));

    if query_matches_parent {
        if let Some((path_score, path_indices)) = matcher.fuzzy_indices(candidate.path, query) {
            if path_score > filename_score {
                return split_path_highlights(candidate, path_indices);
            }
        }
    }

    PickerFilterHighlights {
        label: indices_to_ranges(filename_indices),
        annotation: Vec::new(),
    }
}

fn split_path_highlights(
    candidate: PathCandidate<'_>,
    indices: Vec<usize>,
) -> PickerFilterHighlights {
    let filename_start = candidate
        .path
        .chars()
        .count()
        .saturating_sub(candidate.filename.chars().count());
    let parent_len = candidate
        .parent
        .map(|parent| parent.chars().count())
        .unwrap_or_default();
    let mut filename_indices = Vec::new();
    let mut parent_indices = Vec::new();

    for index in indices {
        if index >= filename_start {
            filename_indices.push(index - filename_start);
        } else if index < parent_len {
            parent_indices.push(index);
        }
    }

    PickerFilterHighlights {
        label: indices_to_ranges(filename_indices),
        annotation: indices_to_ranges(parent_indices),
    }
}

fn indices_to_ranges(indices: Vec<usize>) -> Vec<[usize; 2]> {
    let mut ranges: Vec<[usize; 2]> = Vec::new();
    for index in indices {
        if let Some(last) = ranges.last_mut() {
            if last[1] == index {
                last[1] += 1;
                continue;
            }
        }
        ranges.push([index, index + 1]);
    }
    ranges
}

fn fuzzy_subsequence_matches(candidate: &str, query: &str) -> bool {
    let case_sensitive = query
        .bytes()
        .any(|character| character.is_ascii_uppercase());
    if candidate.is_ascii() && query.is_ascii() {
        let mut expected = query.bytes();
        let Some(mut next) = expected.next() else {
            return true;
        };
        for character in candidate.bytes() {
            let matches = if case_sensitive {
                character == next
            } else {
                character.eq_ignore_ascii_case(&next)
            };
            if !matches {
                continue;
            }
            let Some(following) = expected.next() else {
                return true;
            };
            next = following;
        }
        return false;
    }

    let mut query = query.chars();
    let Some(mut expected) = query.next() else {
        return true;
    };

    for character in candidate.chars() {
        let matches = if case_sensitive {
            character == expected
        } else {
            character.eq_ignore_ascii_case(&expected)
        };
        if !matches {
            continue;
        }
        let Some(next) = query.next() else {
            return true;
        };
        expected = next;
    }

    false
}
