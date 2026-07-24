//! Native Option, Result, and Range intrinsic implementations.

use husk_stdlib::{OptionIntrinsic, RangeIntrinsic, ResultIntrinsic};

use super::{StdCall, option_value};
use crate::Value;

pub(super) fn call_option(operation: OptionIntrinsic, call: &StdCall<'_>) -> anyhow::Result<Value> {
    let (case, fields) = variant(call.receiver, "Option")?;
    let is_some = case == "Some" && fields.len() == 1;
    match operation {
        OptionIntrinsic::IsSome => Ok(Value::Bool(is_some)),
        OptionIntrinsic::IsNone => Ok(Value::Bool(!is_some)),
        OptionIntrinsic::UnwrapOr => Ok(if is_some {
            fields[0].clone()
        } else {
            call.argument(0)?.clone()
        }),
    }
}

pub(super) fn call_result(operation: ResultIntrinsic, call: &StdCall<'_>) -> anyhow::Result<Value> {
    let (case, fields) = variant(call.receiver, "Result")?;
    let is_ok = case == "Ok" && fields.len() == 1;
    match operation {
        ResultIntrinsic::IsOk => Ok(Value::Bool(is_ok)),
        ResultIntrinsic::IsErr => Ok(Value::Bool(!is_ok)),
        ResultIntrinsic::UnwrapOr => Ok(if is_ok {
            fields[0].clone()
        } else {
            call.argument(0)?.clone()
        }),
        ResultIntrinsic::Ok => Ok(option_value(is_ok.then(|| fields[0].clone()))),
        ResultIntrinsic::Err => Ok(option_value(
            (case == "Err" && fields.len() == 1).then(|| fields[0].clone()),
        )),
    }
}

pub(super) fn call_range(operation: RangeIntrinsic, call: &StdCall<'_>) -> anyhow::Result<Value> {
    let Value::Range {
        start,
        end,
        inclusive,
    } = call.receiver
    else {
        anyhow::bail!("range intrinsic expects a Range receiver");
    };
    match operation {
        RangeIntrinsic::Contains => {
            let value = call.integer(0)?;
            Ok(Value::Bool(if *inclusive {
                *start <= value && value <= *end
            } else {
                *start <= value && value < *end
            }))
        }
        RangeIntrinsic::IsEmpty => Ok(Value::Bool(if *inclusive {
            start > end
        } else {
            start >= end
        })),
        RangeIntrinsic::Iter => Ok(call.receiver.clone()),
    }
}

fn variant<'a>(value: &'a Value, expected: &str) -> anyhow::Result<(&'a str, &'a [Value])> {
    match value {
        Value::Variant {
            type_name,
            case,
            fields,
        } if type_name == expected => Ok((case, fields)),
        value => anyhow::bail!(
            "{expected} intrinsic expects a {expected} receiver, got {}",
            value.kind_name()
        ),
    }
}
