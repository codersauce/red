//! Native numeric and conversion intrinsic implementations.

use husk_stdlib::{
    FloatIntrinsic, FunctionIntrinsic, IntegerIntrinsic, IntegerWidth,
    number::{parse_f64, parse_i32, parse_i64},
};

use super::{StdCall, option_value, result_value};
use crate::{Value, cast_value, value_to_log_string};

pub(super) fn call_function(
    intrinsic: FunctionIntrinsic,
    arguments: &[Value],
) -> anyhow::Result<Value> {
    anyhow::ensure!(
        arguments.len() == 1,
        "Husk conversion function expects one argument"
    );
    let value = &arguments[0];
    match intrinsic {
        FunctionIntrinsic::StringFromI32
        | FunctionIntrinsic::StringFromI64
        | FunctionIntrinsic::StringFromF64
        | FunctionIntrinsic::StringFromBool => Ok(Value::String(value_to_log_string(value))),
        FunctionIntrinsic::I64FromI32 => integer(value).map(Value::Int),
        FunctionIntrinsic::F64FromI32 => integer(value).map(|value| Value::Float(value as f64)),
        FunctionIntrinsic::I32TryFromString => Ok(result_value(
            parse_i32(string(value)?)
                .map(|value| Value::Int(i64::from(value)))
                .map_err(|error| error.to_string()),
        )),
        FunctionIntrinsic::I32TryFromI64 => Ok(result_value(
            i32::try_from(integer(value)?)
                .map(|value| Value::Int(i64::from(value)))
                .map_err(|_| "number is outside the target range".to_string()),
        )),
        FunctionIntrinsic::I64TryFromString => Ok(result_value(
            parse_i64(string(value)?)
                .map(Value::Int)
                .map_err(|error| error.to_string()),
        )),
        FunctionIntrinsic::F64TryFromString => Ok(result_value(
            parse_f64(string(value)?)
                .map(Value::Float)
                .map_err(|error| error.to_string()),
        )),
        FunctionIntrinsic::F64TryFromI64 => Ok(result_value(
            exact_f64(integer(value)?)
                .map(Value::Float)
                .map_err(str::to_owned),
        )),
        FunctionIntrinsic::Println
        | FunctionIntrinsic::Assert
        | FunctionIntrinsic::AssertMessage => {
            anyhow::bail!("Husk host intrinsic requires the runtime host adapter")
        }
    }
}

pub(super) fn call_integer(
    width: IntegerWidth,
    operation: IntegerIntrinsic,
    call: &StdCall<'_>,
) -> anyhow::Result<Value> {
    match width {
        IntegerWidth::I32 => integer_operation::<i32>(operation, call, "i32"),
        IntegerWidth::I64 => integer_operation::<i64>(operation, call, "i64"),
    }
}

trait HuskInteger: Copy + Ord {
    fn from_i64(value: i64) -> Option<Self>;
    fn into_i64(self) -> i64;
    fn checked_abs(self) -> Option<Self>;
    fn checked_add(self, other: Self) -> Option<Self>;
    fn checked_sub(self, other: Self) -> Option<Self>;
    fn checked_mul(self, other: Self) -> Option<Self>;
    fn saturating_add(self, other: Self) -> Self;
    fn saturating_sub(self, other: Self) -> Self;
    fn saturating_mul(self, other: Self) -> Self;
}

macro_rules! impl_husk_integer {
    ($integer:ty) => {
        impl HuskInteger for $integer {
            fn from_i64(value: i64) -> Option<Self> {
                Self::try_from(value).ok()
            }

            fn into_i64(self) -> i64 {
                i64::from(self)
            }

            fn checked_abs(self) -> Option<Self> {
                Self::checked_abs(self)
            }

            fn checked_add(self, other: Self) -> Option<Self> {
                Self::checked_add(self, other)
            }

            fn checked_sub(self, other: Self) -> Option<Self> {
                Self::checked_sub(self, other)
            }

            fn checked_mul(self, other: Self) -> Option<Self> {
                Self::checked_mul(self, other)
            }

            fn saturating_add(self, other: Self) -> Self {
                Self::saturating_add(self, other)
            }

            fn saturating_sub(self, other: Self) -> Self {
                Self::saturating_sub(self, other)
            }

            fn saturating_mul(self, other: Self) -> Self {
                Self::saturating_mul(self, other)
            }
        }
    };
}

impl_husk_integer!(i32);
impl_husk_integer!(i64);

fn integer_operation<T: HuskInteger>(
    operation: IntegerIntrinsic,
    call: &StdCall<'_>,
    type_name: &str,
) -> anyhow::Result<Value> {
    let value = T::from_i64(call.integer_receiver()?)
        .ok_or_else(|| anyhow::anyhow!("{type_name} receiver is outside the declared range"))?;
    let argument = |index| {
        T::from_i64(call.integer(index)?)
            .ok_or_else(|| anyhow::anyhow!("{type_name} argument is outside the declared range"))
    };
    let integer = |value: T| Value::Int(value.into_i64());
    match operation {
        IntegerIntrinsic::ToString => Ok(Value::String(value.into_i64().to_string())),
        IntegerIntrinsic::Abs => value
            .checked_abs()
            .map(integer)
            .ok_or_else(|| anyhow::anyhow!("{type_name}::abs overflowed")),
        IntegerIntrinsic::Min => Ok(integer(value.min(argument(0)?))),
        IntegerIntrinsic::Max => Ok(integer(value.max(argument(0)?))),
        IntegerIntrinsic::Clamp => {
            let minimum = argument(0)?;
            let maximum = argument(1)?;
            anyhow::ensure!(
                minimum <= maximum,
                "{type_name}::clamp minimum exceeds maximum"
            );
            Ok(integer(value.clamp(minimum, maximum)))
        }
        IntegerIntrinsic::CheckedAdd => {
            Ok(option_value(value.checked_add(argument(0)?).map(integer)))
        }
        IntegerIntrinsic::CheckedSub => {
            Ok(option_value(value.checked_sub(argument(0)?).map(integer)))
        }
        IntegerIntrinsic::CheckedMul => {
            Ok(option_value(value.checked_mul(argument(0)?).map(integer)))
        }
        IntegerIntrinsic::SaturatingAdd => Ok(integer(value.saturating_add(argument(0)?))),
        IntegerIntrinsic::SaturatingSub => Ok(integer(value.saturating_sub(argument(0)?))),
        IntegerIntrinsic::SaturatingMul => Ok(integer(value.saturating_mul(argument(0)?))),
    }
}

pub(super) fn call_float(operation: FloatIntrinsic, call: &StdCall<'_>) -> anyhow::Result<Value> {
    let value = call.float_receiver()?;
    match operation {
        FloatIntrinsic::ToString => Ok(Value::String(value_to_log_string(call.receiver))),
        FloatIntrinsic::Floor => Ok(Value::Float(value.floor())),
        FloatIntrinsic::Ceil => Ok(Value::Float(value.ceil())),
        FloatIntrinsic::Round => Ok(Value::Float(value.round())),
        FloatIntrinsic::Abs => Ok(Value::Float(value.abs())),
        FloatIntrinsic::Min => Ok(Value::Float(value.min(call.float(0)?))),
        FloatIntrinsic::Max => Ok(Value::Float(value.max(call.float(0)?))),
        FloatIntrinsic::Clamp => {
            let minimum = call.float(0)?;
            let maximum = call.float(1)?;
            anyhow::ensure!(
                !minimum.is_nan() && !maximum.is_nan(),
                "f64::clamp bounds must not be NaN"
            );
            anyhow::ensure!(minimum <= maximum, "f64::clamp minimum exceeds maximum");
            Ok(Value::Float(value.clamp(minimum, maximum)))
        }
    }
}

pub(super) fn call_bool_to_string(call: &StdCall<'_>) -> anyhow::Result<Value> {
    match call.receiver {
        Value::Bool(value) => Ok(Value::String(value.to_string())),
        value => anyhow::bail!(
            "to_string expects a bool receiver, got {}",
            value.kind_name()
        ),
    }
}

pub(super) fn call_into(call: &StdCall<'_>) -> anyhow::Result<Value> {
    let target = call
        .target_type()
        .ok_or_else(|| anyhow::anyhow!("conversion has no resolved target type"))?;
    cast_value(call.receiver.clone(), target)
}

pub(super) fn call_try_into(call: &StdCall<'_>) -> anyhow::Result<Value> {
    let target = call
        .target_type()
        .ok_or_else(|| anyhow::anyhow!("conversion has no resolved target type"))?;
    let value = call.integer_receiver()?;
    let converted = match target {
        "i32" => i32::try_from(value)
            .map(|value| Value::Int(i64::from(value)))
            .map_err(|_| "number is outside the target range".to_string()),
        "f64" => exact_f64(value).map(Value::Float).map_err(str::to_owned),
        target => anyhow::bail!("integer::try_into does not support target `{target}`"),
    };
    Ok(result_value(converted))
}

fn string(value: &Value) -> anyhow::Result<&str> {
    match value {
        Value::String(value) => Ok(value),
        value => anyhow::bail!("conversion expects a String, got {}", value.kind_name()),
    }
}

fn integer(value: &Value) -> anyhow::Result<i64> {
    match value {
        Value::Int(value) => Ok(*value),
        value => anyhow::bail!("conversion expects an integer, got {}", value.kind_name()),
    }
}

fn exact_f64(value: i64) -> Result<f64, &'static str> {
    let converted = value as f64;
    if converted < i64::MAX as f64 && converted as i64 == value {
        Ok(converted)
    } else {
        Err("number cannot be represented exactly as f64")
    }
}
