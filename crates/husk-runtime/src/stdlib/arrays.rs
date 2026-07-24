//! Native non-mutating array intrinsic implementations.

use std::sync::Arc;

use husk_stdlib::ArrayIntrinsic;

use super::{StdCall, option_value};
use crate::{Value, normalize_string_index, saturating_i64, value_to_log_string};

pub(super) fn call(operation: ArrayIntrinsic, call: &StdCall<'_>) -> anyhow::Result<Value> {
    let values = call.array_receiver()?;
    match operation {
        ArrayIntrinsic::Len => Ok(Value::Int(saturating_i64(values.len()))),
        ArrayIntrinsic::IsEmpty => Ok(Value::Bool(values.is_empty())),
        ArrayIntrinsic::Get => {
            let value = usize::try_from(call.integer(0)?)
                .ok()
                .and_then(|index| values.get(index))
                .cloned();
            Ok(option_value(value))
        }
        ArrayIntrinsic::First => Ok(option_value(values.first().cloned())),
        ArrayIntrinsic::Last => Ok(option_value(values.last().cloned())),
        ArrayIntrinsic::Slice => {
            let length = saturating_i64(values.len());
            let start = normalize_string_index(call.integer(0)?, length);
            let end = normalize_string_index(call.integer(1)?, length);
            let start = usize::try_from(start).unwrap_or(0);
            let end = usize::try_from(end.max(i64::try_from(start).unwrap_or(i64::MAX)))
                .unwrap_or(values.len());
            Ok(Value::Array(Arc::new(values[start..end].to_vec())))
        }
        ArrayIntrinsic::Join => Ok(Value::String(
            values
                .iter()
                .map(value_to_log_string)
                .collect::<Vec<_>>()
                .join(call.string(0)?),
        )),
        ArrayIntrinsic::IndexOf => {
            let needle = call.argument(0)?;
            Ok(Value::Int(
                values
                    .iter()
                    .position(|value| value == needle)
                    .map_or(-1, saturating_i64),
            ))
        }
        ArrayIntrinsic::LastIndexOf => {
            let needle = call.argument(0)?;
            Ok(Value::Int(
                values
                    .iter()
                    .rposition(|value| value == needle)
                    .map_or(-1, saturating_i64),
            ))
        }
        ArrayIntrinsic::Contains => Ok(Value::Bool(values.contains(call.argument(0)?))),
        ArrayIntrinsic::Iter => match call.receiver {
            Value::Array(values) => Ok(Value::Array(Arc::clone(values))),
            _ => unreachable!("array receiver was checked before dispatch"),
        },
        ArrayIntrinsic::Push
        | ArrayIntrinsic::Sort
        | ArrayIntrinsic::Reverse
        | ArrayIntrinsic::Pop
        | ArrayIntrinsic::Shift
        | ArrayIntrinsic::Unshift => {
            anyhow::bail!("mutable array intrinsic requires the instance heap")
        }
    }
}
