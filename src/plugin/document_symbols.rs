//! Native containment lookup for plugin-owned document-symbol breadcrumbs.
//!
//! The VM receives only the containing ancestry, so large files do not require
//! a truncated scan or a larger per-callback instruction budget. Original Husk
//! records are retained, including their nominal types and navigation ranges.

use std::collections::{HashMap, HashSet};

use husk_runtime::Value;

type Position = (i64, i64);

struct Candidate<'a> {
    index: usize,
    id: &'a str,
    parent: Option<&'a str>,
    depth: i64,
    start: Position,
    end: Position,
}

fn field<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    match value {
        Value::Object(fields) | Value::Struct { fields, .. } => fields.get(name),
        _ => None,
    }
}

fn integer(value: &Value) -> Option<i64> {
    match value {
        Value::Int(value) => Some(*value),
        _ => None,
    }
}

fn position(value: &Value) -> Option<Position> {
    Some((
        integer(field(value, "line")?)?,
        integer(field(value, "character")?)?,
    ))
}

fn optional_string(value: &Value) -> Option<&str> {
    match value {
        Value::Variant { case, fields, .. } if case == "Some" => {
            fields.first().and_then(Value::as_str)
        }
        value => value.as_str(),
    }
}

pub(super) fn chain(symbols: &[Value], cursor: &Value, file: &str) -> Vec<Value> {
    let Some(cursor) = position(cursor) else {
        return Vec::new();
    };
    let containing: Vec<_> = symbols
        .iter()
        .enumerate()
        .filter_map(|(index, symbol)| {
            if field(symbol, "file")?.as_str()? != file {
                return None;
            }
            let range = field(symbol, "range")?;
            let start = position(field(range, "start")?)?;
            let end = position(field(range, "end")?)?;
            (start <= cursor && cursor < end).then(|| Candidate {
                index,
                id: field(symbol, "id").and_then(Value::as_str).unwrap_or(""),
                parent: field(symbol, "parent_id").and_then(optional_string),
                depth: field(symbol, "depth").and_then(integer).unwrap_or(0),
                start,
                end,
            })
        })
        .collect();
    let Some(mut current) = containing.iter().max_by(|left, right| {
        left.depth
            .cmp(&right.depth)
            .then_with(|| left.start.cmp(&right.start))
            .then_with(|| right.end.cmp(&left.end))
    }) else {
        return Vec::new();
    };
    let by_id: HashMap<_, _> = containing
        .iter()
        .map(|symbol| (symbol.id, symbol))
        .collect();
    let mut visited = HashSet::new();
    let mut chain = Vec::new();
    while visited.insert(current.index) {
        chain.push(symbols[current.index].clone());
        let Some(parent) = current.parent.and_then(|id| by_id.get(id)) else {
            break;
        };
        current = parent;
    }
    chain.reverse();
    chain
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn symbol(
        id: &str,
        parent: Option<&str>,
        depth: usize,
        start: Position,
        end: Position,
    ) -> Value {
        Value::from_json(json!({
            "id": id, "parent_id": parent, "depth": depth, "file": "main.rs",
            "range": {
                "start": { "line": start.0, "character": start.1 },
                "end": { "line": end.0, "character": end.1 }
            }
        }))
    }

    fn ids(symbols: &[Value], cursor: Position) -> Vec<String> {
        chain(
            symbols,
            &Value::from_json(json!({
                "line": cursor.0, "character": cursor.1
            })),
            "main.rs",
        )
        .iter()
        .map(|symbol| field(symbol, "id").unwrap().as_str().unwrap().to_string())
        .collect()
    }

    #[test]
    fn uses_half_open_utf16_ranges_and_preserves_ancestry() {
        let symbols = [
            symbol("outer", None, 0, (0, 0), (9, 0)),
            symbol("inner", Some("outer"), 1, (2, 2), (2, 7)),
        ];
        assert_eq!(ids(&symbols, (2, 2)), ["outer", "inner"]);
        assert_eq!(ids(&symbols, (2, 6)), ["outer", "inner"]);
        assert_eq!(ids(&symbols, (2, 7)), ["outer"]);
        assert!(ids(&symbols, (9, 0)).is_empty());
    }

    #[test]
    fn flat_symbols_choose_the_smallest_containing_range() {
        let symbols = [
            symbol("inner", None, 0, (2, 0), (3, 0)),
            symbol("outer", None, 0, (0, 0), (9, 0)),
        ];
        assert_eq!(ids(&symbols, (2, 1)), ["inner"]);
        assert!(chain(
            &symbols,
            &Value::from_json(json!({
                "line": 2, "character": 1
            })),
            "other.rs"
        )
        .is_empty());
    }

    #[test]
    fn malformed_parent_cycles_terminate() {
        let symbols = [
            symbol("a", Some("b"), 0, (0, 0), (9, 0)),
            symbol("b", Some("a"), 1, (1, 0), (8, 0)),
        ];
        assert_eq!(ids(&symbols, (2, 0)), ["a", "b"]);
    }
}
