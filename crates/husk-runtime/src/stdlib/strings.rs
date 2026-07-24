//! Native string intrinsic implementations.

use std::sync::Arc;

use husk_stdlib::{StringIntrinsic, number::parse_f64, number::parse_i32, number::parse_i64};

use super::{StdCall, option_value, result_value};
use crate::{Value, saturating_i64, slice_string};

pub(super) fn call(operation: StringIntrinsic, call: &StdCall<'_>) -> anyhow::Result<Value> {
    let value = call.string_receiver()?;
    match operation {
        StringIntrinsic::Len => Ok(Value::Int(saturating_i64(value.chars().count()))),
        StringIntrinsic::IsEmpty => Ok(Value::Bool(value.is_empty())),
        StringIntrinsic::Trim => Ok(Value::String(value.trim().to_owned())),
        StringIntrinsic::TrimStart => Ok(Value::String(value.trim_start().to_owned())),
        StringIntrinsic::TrimEnd => Ok(Value::String(value.trim_end().to_owned())),
        StringIntrinsic::Split => {
            let separator = call.string(0)?;
            let values = if separator.is_empty() {
                value
                    .chars()
                    .map(|character| Value::String(character.to_string()))
                    .collect()
            } else {
                value
                    .split(separator)
                    .map(|part| Value::String(part.to_owned()))
                    .collect()
            };
            Ok(Value::Array(Arc::new(values)))
        }
        StringIntrinsic::SplitOnce => split_once(value.split_once(call.string(0)?)),
        StringIntrinsic::RsplitOnce => split_once(value.rsplit_once(call.string(0)?)),
        StringIntrinsic::SplitWhitespace => Ok(Value::Array(Arc::new(
            value
                .split_whitespace()
                .map(|part| Value::String(part.to_owned()))
                .collect(),
        ))),
        StringIntrinsic::Lines => Ok(Value::Array(Arc::new(
            value
                .lines()
                .map(|line| Value::String(line.to_owned()))
                .collect(),
        ))),
        StringIntrinsic::CharAt => {
            let character = usize::try_from(call.integer(0)?)
                .ok()
                .and_then(|index| value.chars().nth(index))
                .map_or_else(String::new, |character| character.to_string());
            Ok(Value::String(character))
        }
        StringIntrinsic::Slice => Ok(Value::String(slice_string(
            value,
            call.integer(0)?,
            call.integer(1)?,
        ))),
        StringIntrinsic::IndexOf => Ok(string_index(value, value.find(call.string(0)?))),
        StringIntrinsic::LastIndexOf => Ok(string_index(value, value.rfind(call.string(0)?))),
        StringIntrinsic::StartsWith => Ok(Value::Bool(value.starts_with(call.string(0)?))),
        StringIntrinsic::EndsWith => Ok(Value::Bool(value.ends_with(call.string(0)?))),
        StringIntrinsic::Contains => Ok(Value::Bool(value.contains(call.string(0)?))),
        StringIntrinsic::StripPrefix => Ok(option_value(
            value
                .strip_prefix(call.string(0)?)
                .map(|value| Value::String(value.to_owned())),
        )),
        StringIntrinsic::StripSuffix => Ok(option_value(
            value
                .strip_suffix(call.string(0)?)
                .map(|value| Value::String(value.to_owned())),
        )),
        StringIntrinsic::Replace => replace(value, call),
        StringIntrinsic::Repeat => repeat(value, call),
        StringIntrinsic::ToLowercase => Ok(Value::String(value.to_lowercase())),
        StringIntrinsic::ToUppercase => Ok(Value::String(value.to_uppercase())),
        StringIntrinsic::IsAsciiDigit => {
            let mut characters = value.chars();
            Ok(Value::Bool(matches!(
                (characters.next(), characters.next()),
                (Some(character), None) if character.is_ascii_digit()
            )))
        }
        StringIntrinsic::ToDigit => {
            let radix = u32::try_from(call.integer(0)?)
                .ok()
                .filter(|radix| (2..=36).contains(radix));
            let mut characters = value.chars();
            let digit = match (characters.next(), characters.next(), radix) {
                (Some(character), None, Some(radix)) => character.to_digit(radix),
                _ => None,
            };
            Ok(option_value(
                digit.map(|digit| Value::Int(i64::from(digit))),
            ))
        }
        StringIntrinsic::Iter => Ok(Value::Array(Arc::new(
            value
                .chars()
                .map(|character| Value::String(character.to_string()))
                .collect(),
        ))),
        StringIntrinsic::Parse => parse(value, call),
    }
}

fn split_once(parts: Option<(&str, &str)>) -> anyhow::Result<Value> {
    Ok(option_value(parts.map(|(before, after)| {
        Value::Tuple(Arc::new(vec![
            Value::String(before.to_owned()),
            Value::String(after.to_owned()),
        ]))
    })))
}

fn string_index(value: &str, byte_index: Option<usize>) -> Value {
    Value::Int(byte_index.map_or(-1, |index| saturating_i64(value[..index].chars().count())))
}

fn replace(value: &str, call: &StdCall<'_>) -> anyhow::Result<Value> {
    let pattern = call.string(0)?;
    let replacement = call.string(1)?;
    let occurrences = value.matches(pattern).count();
    let removed = pattern
        .len()
        .checked_mul(occurrences)
        .ok_or_else(|| anyhow::anyhow!("String::replace output size overflowed"))?;
    let inserted = replacement
        .len()
        .checked_mul(occurrences)
        .ok_or_else(|| anyhow::anyhow!("String::replace output size overflowed"))?;
    let output = value
        .len()
        .checked_sub(removed)
        .and_then(|retained| retained.checked_add(inserted))
        .ok_or_else(|| anyhow::anyhow!("String::replace output size overflowed"))?;
    call.ensure_output_size(output)?;
    Ok(Value::String(value.replace(pattern, replacement)))
}

fn repeat(value: &str, call: &StdCall<'_>) -> anyhow::Result<Value> {
    let count = usize::try_from(call.integer(0)?)
        .map_err(|_| anyhow::anyhow!("String::repeat requires a non-negative count"))?;
    let output = value
        .len()
        .checked_mul(count)
        .ok_or_else(|| anyhow::anyhow!("String::repeat output size overflowed"))?;
    call.ensure_output_size(output)?;
    Ok(Value::String(value.repeat(count)))
}

fn parse(value: &str, call: &StdCall<'_>) -> anyhow::Result<Value> {
    let target = call
        .target_type()
        .ok_or_else(|| anyhow::anyhow!("String::parse has no resolved target type"))?;
    let parsed = match target {
        "i32" => parse_i32(value).map(|value| Value::Int(i64::from(value))),
        "i64" => parse_i64(value).map(Value::Int),
        "f64" => parse_f64(value).map(Value::Float),
        target => anyhow::bail!("String::parse does not support target `{target}`"),
    };
    Ok(result_value(parsed.map_err(|error| error.to_string())))
}
