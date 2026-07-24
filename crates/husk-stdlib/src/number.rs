//! Strict, backend-neutral numeric parsing primitives.

use std::{fmt, num::IntErrorKind};

/// A deterministic numeric-parsing failure that does not expose the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseNumberError {
    /// The input was empty.
    Empty,
    /// The input contained an invalid digit or representation.
    Invalid,
    /// The value was outside the target numeric range.
    Overflow,
    /// A floating-point value was not finite.
    NonFinite,
}

impl fmt::Display for ParseNumberError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "cannot parse an empty number",
            Self::Invalid => "invalid number",
            Self::Overflow => "number is outside the target range",
            Self::NonFinite => "number must be finite",
        })
    }
}

impl std::error::Error for ParseNumberError {}

/// Parse a complete decimal string as an `i32`.
///
/// Leading `+` and `-` are accepted. Whitespace, trailing characters, and
/// overflow are rejected.
pub fn parse_i32(value: &str) -> Result<i32, ParseNumberError> {
    value.parse().map_err(integer_error)
}

/// Parse a complete decimal string as an `i64`.
///
/// Leading `+` and `-` are accepted. Whitespace, trailing characters, and
/// overflow are rejected.
pub fn parse_i64(value: &str) -> Result<i64, ParseNumberError> {
    value.parse().map_err(integer_error)
}

/// Parse a complete finite floating-point string as an `f64`.
///
/// Whitespace, trailing characters, `NaN`, and infinities are rejected.
pub fn parse_f64(value: &str) -> Result<f64, ParseNumberError> {
    if value.is_empty() {
        return Err(ParseNumberError::Empty);
    }
    let parsed = value
        .parse::<f64>()
        .map_err(|_| ParseNumberError::Invalid)?;
    if parsed.is_finite() {
        Ok(parsed)
    } else {
        Err(ParseNumberError::NonFinite)
    }
}

fn integer_error(error: std::num::ParseIntError) -> ParseNumberError {
    match error.kind() {
        IntErrorKind::Empty => ParseNumberError::Empty,
        IntErrorKind::PosOverflow | IntErrorKind::NegOverflow => ParseNumberError::Overflow,
        _ => ParseNumberError::Invalid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_signed_decimal_integers_without_partial_matches() {
        assert_eq!(parse_i32("0"), Ok(0));
        assert_eq!(parse_i32("+42"), Ok(42));
        assert_eq!(parse_i32("-42"), Ok(-42));
        assert_eq!(parse_i32("2147483647"), Ok(i32::MAX));
        assert_eq!(parse_i32("-2147483648"), Ok(i32::MIN));
        assert_eq!(parse_i64("9223372036854775807"), Ok(i64::MAX));
        assert_eq!(parse_i64("-9223372036854775808"), Ok(i64::MIN));

        assert_eq!(parse_i32(""), Err(ParseNumberError::Empty));
        assert_eq!(parse_i32(" 42"), Err(ParseNumberError::Invalid));
        assert_eq!(parse_i32("42 "), Err(ParseNumberError::Invalid));
        assert_eq!(parse_i32("42x"), Err(ParseNumberError::Invalid));
        assert_eq!(parse_i32("2147483648"), Err(ParseNumberError::Overflow));
        assert_eq!(parse_i32("-2147483649"), Err(ParseNumberError::Overflow));
        assert_eq!(
            parse_i64("9223372036854775808"),
            Err(ParseNumberError::Overflow)
        );
    }

    #[test]
    fn parses_only_complete_finite_floats() {
        assert_eq!(parse_f64("0"), Ok(0.0));
        assert_eq!(parse_f64("-1.25e2"), Ok(-125.0));
        assert_eq!(parse_f64(""), Err(ParseNumberError::Empty));
        assert_eq!(parse_f64(" 1.25"), Err(ParseNumberError::Invalid));
        assert_eq!(parse_f64("1.25x"), Err(ParseNumberError::Invalid));
        assert_eq!(parse_f64("NaN"), Err(ParseNumberError::NonFinite));
        assert_eq!(parse_f64("inf"), Err(ParseNumberError::NonFinite));
        assert_eq!(parse_f64("1e9999"), Err(ParseNumberError::NonFinite));
    }
}
