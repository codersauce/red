//! Backend-neutral values that may cross Husk embedding and extension
//! boundaries.

use std::collections::BTreeMap;

/// An opaque reference to a resource owned by one extension instance.
///
/// Handles may be copied as ordinary Husk values, but the extension remains
/// the sole owner of the underlying resource. Consuming or dropping a resource
/// invalidates every copy of its handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResourceHandle {
    owner: u64,
    slot: u32,
    generation: u32,
}

impl ResourceHandle {
    /// Construct a handle for an extension-managed resource table.
    #[must_use]
    pub const fn new(owner: u64, slot: u32, generation: u32) -> Self {
        Self {
            owner,
            slot,
            generation,
        }
    }

    #[must_use]
    pub const fn owner(self) -> u64 {
        self.owner
    }

    #[must_use]
    pub const fn slot(self) -> u32 {
        self.slot
    }

    #[must_use]
    pub const fn generation(self) -> u32 {
        self.generation
    }
}

/// Boundary data that does not borrow an interpreter or native host.
///
/// Resource values are the sole exception to complete detachment: they carry
/// opaque, instance-scoped handles whose validity is checked by the owning
/// extension on every use.
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
    Resource {
        type_name: String,
        handle: ResourceHandle,
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
            Self::Resource { .. } => "resource",
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
