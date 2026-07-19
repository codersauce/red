//! Backend-neutral values that may cross Husk embedding and extension
//! boundaries.

use std::collections::BTreeMap;

/// Detached data that does not borrow an interpreter, native host, or Wasm
/// store.
#[derive(Debug, Clone, PartialEq)]
pub enum OwnedValue {
    Unit,
    Null,
    Bool(bool),
    I32(i32),
    I64(i64),
    F64(f64),
    String(String),
    Bytes(Vec<u8>),
    List(Vec<OwnedValue>),
    Tuple(Vec<OwnedValue>),
    Range {
        start: i64,
        end: i64,
        inclusive: bool,
    },
    Record(BTreeMap<String, OwnedValue>),
    Struct {
        type_name: String,
        fields: BTreeMap<String, OwnedValue>,
    },
    Variant {
        type_name: String,
        case: String,
        fields: Vec<OwnedValue>,
    },
    Json(serde_json::Value),
}

impl OwnedValue {
    /// A stable, human-readable category for conversion diagnostics.
    #[must_use]
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Unit => "unit",
            Self::Null => "null",
            Self::Bool(_) => "bool",
            Self::I32(_) => "i32",
            Self::I64(_) => "i64",
            Self::F64(_) => "f64",
            Self::String(_) => "String",
            Self::Bytes(_) => "bytes",
            Self::List(_) => "list",
            Self::Tuple(_) => "tuple",
            Self::Range { .. } => "range",
            Self::Record(_) => "record",
            Self::Struct { .. } => "struct",
            Self::Variant { .. } => "variant",
            Self::Json(_) => "Json",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::OwnedValue;

    #[test]
    fn category_names_do_not_depend_on_payloads() {
        assert_eq!(OwnedValue::I32(42).kind_name(), "i32");
        assert_eq!(
            OwnedValue::List(vec![OwnedValue::Bool(true)]).kind_name(),
            "list"
        );
    }
}
