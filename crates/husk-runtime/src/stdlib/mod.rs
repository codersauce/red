//! Typed adapters for native Husk standard-library intrinsics.

mod arrays;
mod numbers;
mod strings;
mod variants;

use husk_stdlib::{FunctionIntrinsic, StdIntrinsic};

use crate::Value;

pub(super) struct StdCall<'a> {
    receiver: &'a Value,
    arguments: &'a [Value],
    type_arguments: &'a [String],
    result_type: Option<&'a str>,
    operation: &'static str,
    max_value_bytes: usize,
}

impl<'a> StdCall<'a> {
    fn new(
        intrinsic: StdIntrinsic,
        receiver: &'a Value,
        arguments: &'a [Value],
        type_arguments: &'a [String],
        result_type: Option<&'a str>,
        max_value_bytes: usize,
    ) -> anyhow::Result<Self> {
        let descriptor = intrinsic
            .descriptor()
            .ok_or_else(|| anyhow::anyhow!("standard-library intrinsic has no descriptor"))?;
        anyhow::ensure!(
            arguments.len() == descriptor.arity(),
            "{} expects {} argument{}, got {}",
            descriptor.name,
            descriptor.arity(),
            if descriptor.arity() == 1 { "" } else { "s" },
            arguments.len()
        );
        Ok(Self {
            receiver,
            arguments,
            type_arguments,
            result_type,
            operation: descriptor.name,
            max_value_bytes,
        })
    }

    fn string_receiver(&self) -> anyhow::Result<&str> {
        match self.receiver {
            Value::String(value) => Ok(value),
            value => anyhow::bail!(
                "{} expects a String receiver, got {}",
                self.operation,
                value.kind_name()
            ),
        }
    }

    fn integer_receiver(&self) -> anyhow::Result<i64> {
        match self.receiver {
            Value::Int(value) => Ok(*value),
            value => anyhow::bail!(
                "{} expects an integer receiver, got {}",
                self.operation,
                value.kind_name()
            ),
        }
    }

    fn float_receiver(&self) -> anyhow::Result<f64> {
        match self.receiver {
            Value::Float(value) => Ok(*value),
            value => anyhow::bail!(
                "{} expects an f64 receiver, got {}",
                self.operation,
                value.kind_name()
            ),
        }
    }

    fn array_receiver(&self) -> anyhow::Result<&[Value]> {
        match self.receiver {
            Value::Array(values) => Ok(values),
            value => anyhow::bail!(
                "{} expects an array receiver, got {}",
                self.operation,
                value.kind_name()
            ),
        }
    }

    fn argument(&self, index: usize) -> anyhow::Result<&Value> {
        self.arguments
            .get(index)
            .ok_or_else(|| anyhow::anyhow!("{} is missing argument {}", self.operation, index + 1))
    }

    fn string(&self, index: usize) -> anyhow::Result<&str> {
        match self.argument(index)? {
            Value::String(value) => Ok(value),
            value => anyhow::bail!(
                "{} argument {} must be a String, got {}",
                self.operation,
                index + 1,
                value.kind_name()
            ),
        }
    }

    fn integer(&self, index: usize) -> anyhow::Result<i64> {
        match self.argument(index)? {
            Value::Int(value) => Ok(*value),
            value => anyhow::bail!(
                "{} argument {} must be an integer, got {}",
                self.operation,
                index + 1,
                value.kind_name()
            ),
        }
    }

    fn float(&self, index: usize) -> anyhow::Result<f64> {
        match self.argument(index)? {
            Value::Float(value) => Ok(*value),
            value => anyhow::bail!(
                "{} argument {} must be an f64, got {}",
                self.operation,
                index + 1,
                value.kind_name()
            ),
        }
    }

    fn target_type(&self) -> Option<&str> {
        self.type_arguments.first().map(String::as_str).or_else(|| {
            let result = self.result_type?;
            if matches!(result, "i32" | "i64" | "f64" | "String") {
                return Some(result);
            }
            result
                .strip_prefix("Result<")
                .and_then(|result| result.split_once(','))
                .map(|(target, _)| target.trim())
        })
    }

    fn ensure_output_size(&self, bytes: usize) -> anyhow::Result<()> {
        anyhow::ensure!(
            bytes <= self.max_value_bytes,
            "{} output exceeds {} bytes",
            self.operation,
            self.max_value_bytes
        );
        Ok(())
    }
}

pub(super) fn call_function(
    intrinsic: FunctionIntrinsic,
    arguments: &[Value],
) -> anyhow::Result<Value> {
    numbers::call_function(intrinsic, arguments)
}

pub(super) fn call_method(
    intrinsic: StdIntrinsic,
    receiver: &Value,
    arguments: &[Value],
    type_arguments: &[String],
    result_type: Option<&str>,
    max_value_bytes: usize,
) -> anyhow::Result<Value> {
    let call = StdCall::new(
        intrinsic,
        receiver,
        arguments,
        type_arguments,
        result_type,
        max_value_bytes,
    )?;
    match intrinsic {
        StdIntrinsic::String(operation) => strings::call(operation, &call),
        StdIntrinsic::Integer(width, operation) => numbers::call_integer(width, operation, &call),
        StdIntrinsic::Float(operation) => numbers::call_float(operation, &call),
        StdIntrinsic::BoolToString => numbers::call_bool_to_string(&call),
        StdIntrinsic::Array(operation) => arrays::call(operation, &call),
        StdIntrinsic::Option(operation) => variants::call_option(operation, &call),
        StdIntrinsic::Result(operation) => variants::call_result(operation, &call),
        StdIntrinsic::Range(operation) => variants::call_range(operation, &call),
        StdIntrinsic::Into => numbers::call_into(&call),
        StdIntrinsic::TryInto => numbers::call_try_into(&call),
        StdIntrinsic::Function(_) | StdIntrinsic::ArrayHigherOrder(_) => {
            anyhow::bail!("standard-library operation requires a runtime callback adapter")
        }
    }
}

pub(super) fn option_value(value: Option<Value>) -> Value {
    match value {
        Some(value) => crate::enum_variant_value("Option", "Some", vec![value]),
        None => crate::enum_variant_value("Option", "None", Vec::new()),
    }
}

pub(super) fn result_value(value: Result<Value, String>) -> Value {
    match value {
        Ok(value) => crate::enum_variant_value("Result", "Ok", vec![value]),
        Err(error) => crate::enum_variant_value("Result", "Err", vec![Value::String(error)]),
    }
}
