//! Serialized plugin locations and explicit conversion at the editor navigation boundary.
//!
//! Plugin locations use zero-based lines and declare whether columns are UTF-8 bytes or
//! UTF-16 code units. UTF-8 bytes remain the compatibility default. The editor validates
//! boundaries before converting to its grapheme cursor; passing a scalar or display
//! column under the default encoding can select the wrong text for non-ASCII input.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PluginLocation {
    pub path: String,
    pub line: usize,
    pub column: usize,
    #[serde(default)]
    pub column_encoding: LocationColumnEncoding,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum LocationColumnEncoding {
    #[default]
    Utf8Byte,
    #[serde(rename = "utf-16")]
    Utf16,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OpenLocationTarget {
    #[default]
    Current,
    Horizontal,
    Vertical,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn location_uses_utf8_byte_columns_by_default() {
        let location: PluginLocation = serde_json::from_value(serde_json::json!({
            "path": "src/main.rs",
            "line": 4,
            "column": 7
        }))
        .unwrap();

        assert_eq!(location.column_encoding, LocationColumnEncoding::Utf8Byte);
    }

    #[test]
    fn targets_use_lowercase_api_names() {
        let target: OpenLocationTarget = serde_json::from_str("\"horizontal\"").unwrap();
        assert_eq!(target, OpenLocationTarget::Horizontal);
    }

    #[test]
    fn location_accepts_utf16_columns() {
        let location: PluginLocation = serde_json::from_value(serde_json::json!({
            "path": "src/main.rs",
            "line": 4,
            "column": 7,
            "column_encoding": "utf-16"
        }))
        .unwrap();

        assert_eq!(location.column_encoding, LocationColumnEncoding::Utf16);
    }
}
