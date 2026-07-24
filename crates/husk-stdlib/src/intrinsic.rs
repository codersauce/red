//! Typed, backend-neutral inventory for the Husk standard library.

use std::{collections::HashMap, sync::OnceLock};

/// The language-visible receiver family for an intrinsic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReceiverKind {
    Function,
    Any,
    String,
    I32,
    I64,
    F64,
    Bool,
    Array,
    Option,
    Result,
    Range,
}

impl ReceiverKind {
    /// Classify a resolved Husk type for indexed intrinsic lookup.
    #[must_use]
    pub fn from_type(ty: &str) -> Option<Self> {
        let ty = ty.trim();
        if ty.starts_with('[') {
            return Some(Self::Array);
        }
        match ty.split('<').next().unwrap_or(ty) {
            "String" => Some(Self::String),
            "i32" => Some(Self::I32),
            "i64" => Some(Self::I64),
            "f64" => Some(Self::F64),
            "bool" => Some(Self::Bool),
            "Option" => Some(Self::Option),
            "Result" => Some(Self::Result),
            "Range" => Some(Self::Range),
            _ => None,
        }
    }
}

/// How an intrinsic receives its language-level receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiverMode {
    Static,
    Shared,
    Mutable,
    Owned,
}

/// Free and associated standard-library functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FunctionIntrinsic {
    Println,
    Assert,
    AssertMessage,
    StringFromI32,
    StringFromI64,
    StringFromF64,
    StringFromBool,
    I64FromI32,
    F64FromI32,
    I32TryFromString,
    I32TryFromI64,
    I64TryFromString,
    F64TryFromString,
    F64TryFromI64,
}

/// String operations supplied by the native standard library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StringIntrinsic {
    Len,
    IsEmpty,
    Trim,
    TrimStart,
    TrimEnd,
    Split,
    SplitOnce,
    RsplitOnce,
    SplitWhitespace,
    Lines,
    CharAt,
    Slice,
    IndexOf,
    LastIndexOf,
    StartsWith,
    EndsWith,
    Contains,
    StripPrefix,
    StripSuffix,
    Replace,
    Repeat,
    ToLowercase,
    ToUppercase,
    IsAsciiDigit,
    ToDigit,
    Iter,
    Parse,
}

/// Declared width of an integer intrinsic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntegerWidth {
    I32,
    I64,
}

/// Integer operations shared by `i32` and `i64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntegerIntrinsic {
    ToString,
    Abs,
    Min,
    Max,
    Clamp,
    CheckedAdd,
    CheckedSub,
    CheckedMul,
    SaturatingAdd,
    SaturatingSub,
    SaturatingMul,
}

/// Floating-point operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloatIntrinsic {
    ToString,
    Floor,
    Ceil,
    Round,
    Abs,
    Min,
    Max,
    Clamp,
}

/// Array operations that do not invoke a Husk callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArrayIntrinsic {
    Len,
    IsEmpty,
    Get,
    First,
    Last,
    Push,
    Slice,
    Join,
    Sort,
    Reverse,
    IndexOf,
    LastIndexOf,
    Contains,
    Pop,
    Shift,
    Unshift,
    Iter,
}

/// Array operations that invoke a Husk callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArrayHigherOrderIntrinsic {
    Map,
    Filter,
    Some,
    Every,
    Reduce,
    ForEach,
    Find,
    FindIndex,
    FindLastIndex,
    SortBy,
}

/// Option operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OptionIntrinsic {
    IsSome,
    IsNone,
    UnwrapOr,
}

/// Result operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResultIntrinsic {
    IsOk,
    IsErr,
    UnwrapOr,
    Ok,
    Err,
}

/// Range operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RangeIntrinsic {
    Contains,
    IsEmpty,
    Iter,
}

/// Fully resolved, typed standard-library operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StdIntrinsic {
    Function(FunctionIntrinsic),
    String(StringIntrinsic),
    Integer(IntegerWidth, IntegerIntrinsic),
    Float(FloatIntrinsic),
    BoolToString,
    Array(ArrayIntrinsic),
    ArrayHigherOrder(ArrayHigherOrderIntrinsic),
    Option(OptionIntrinsic),
    Result(ResultIntrinsic),
    Range(RangeIntrinsic),
    Into,
    TryInto,
}

/// One declaration in the standard-library inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntrinsicDescriptor {
    pub intrinsic: StdIntrinsic,
    pub receiver: ReceiverKind,
    pub receiver_mode: ReceiverMode,
    pub name: &'static str,
    pub parameters: &'static [&'static str],
}

impl IntrinsicDescriptor {
    #[must_use]
    pub const fn arity(self) -> usize {
        self.parameters.len()
    }
}

/// Result of resolving a possibly overloaded associated function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionResolution {
    Missing,
    Unique(StdIntrinsic),
    Ambiguous,
}

macro_rules! define_stdlib {
    (
        functions {
            $( $function:expr => $function_name:literal, [$($function_parameter:literal),* $(,)?], $function_signature:literal; )*
        }
        methods {
            $(
                $header:literal, $receiver:ident {
                    $( $intrinsic:expr => $mode:ident, $name:literal, [$($parameter:literal),* $(,)?], $signature:literal; )*
                }
            )*
        }
        pseudo {
            $( $pseudo:expr => $pseudo_receiver:ident, $pseudo_mode:ident, $pseudo_name:literal, [$($pseudo_parameter:literal),* $(,)?]; )*
        }
    ) => {
        /// Complete inventory used by semantic prelude generation and runtime lookup.
        pub const INTRINSICS: &[IntrinsicDescriptor] = &[
            $(IntrinsicDescriptor {
                intrinsic: $function,
                receiver: ReceiverKind::Function,
                receiver_mode: ReceiverMode::Static,
                name: $function_name,
                parameters: &[$($function_parameter),*],
            },)*
            $($(IntrinsicDescriptor {
                intrinsic: $intrinsic,
                receiver: ReceiverKind:: $receiver,
                receiver_mode: ReceiverMode:: $mode,
                name: $name,
                parameters: &[$($parameter),*],
            },)*)*
            $(IntrinsicDescriptor {
                intrinsic: $pseudo,
                receiver: ReceiverKind:: $pseudo_receiver,
                receiver_mode: ReceiverMode:: $pseudo_mode,
                name: $pseudo_name,
                parameters: &[$($pseudo_parameter),*],
            },)*
        ];

        /// Canonical declarations injected into every native Husk compilation.
        #[must_use]
        pub fn native_prelude() -> &'static str {
            static PRELUDE: OnceLock<String> = OnceLock::new();
            PRELUDE.get_or_init(|| {
                let mut source = String::from(include_str!("../prelude/native.hk"));
                source.push_str(concat!(
                    "\nextern \"husk\" {\n    struct Json;\n",
                    $("    ", $function_signature, "\n",)*
                    "}\n"
                ));
                $(source.push_str(concat!(
                    "\n", $header, " {\n",
                    $("    #[intrinsic]\n    ", $signature, "\n",)*
                    "}\n"
                ));)*
                source
            })
        }
    };
}

define_stdlib! {
    functions {
        StdIntrinsic::Function(FunctionIntrinsic::Println) => "println", ["String"], "fn println(value: String);";
        StdIntrinsic::Function(FunctionIntrinsic::Assert) => "assert", ["bool"], "fn assert(condition: bool);";
        StdIntrinsic::Function(FunctionIntrinsic::AssertMessage) => "assert_msg", ["bool", "String"], "fn assert_msg(condition: bool, message: String);";
    }
    methods {
        "impl From<i32> for String", Function {
            StdIntrinsic::Function(FunctionIntrinsic::StringFromI32) => Static, "String::from", ["i32"], "fn from(value: i32) -> String {}";
        }
        "impl From<i64> for String", Function {
            StdIntrinsic::Function(FunctionIntrinsic::StringFromI64) => Static, "String::from", ["i64"], "fn from(value: i64) -> String {}";
        }
        "impl From<f64> for String", Function {
            StdIntrinsic::Function(FunctionIntrinsic::StringFromF64) => Static, "String::from", ["f64"], "fn from(value: f64) -> String {}";
        }
        "impl From<bool> for String", Function {
            StdIntrinsic::Function(FunctionIntrinsic::StringFromBool) => Static, "String::from", ["bool"], "fn from(value: bool) -> String {}";
        }
        "impl From<i32> for i64", Function {
            StdIntrinsic::Function(FunctionIntrinsic::I64FromI32) => Static, "i64::from", ["i32"], "fn from(value: i32) -> i64 {}";
        }
        "impl From<i32> for f64", Function {
            StdIntrinsic::Function(FunctionIntrinsic::F64FromI32) => Static, "f64::from", ["i32"], "fn from(value: i32) -> f64 {}";
        }
        "impl TryFrom<String> for i32", Function {
            StdIntrinsic::Function(FunctionIntrinsic::I32TryFromString) => Static, "i32::try_from", ["String"], "fn try_from(value: String) -> Result<i32, String> {}";
        }
        "impl TryFrom<i64> for i32", Function {
            StdIntrinsic::Function(FunctionIntrinsic::I32TryFromI64) => Static, "i32::try_from", ["i64"], "fn try_from(value: i64) -> Result<i32, String> {}";
        }
        "impl TryFrom<String> for i64", Function {
            StdIntrinsic::Function(FunctionIntrinsic::I64TryFromString) => Static, "i64::try_from", ["String"], "fn try_from(value: String) -> Result<i64, String> {}";
        }
        "impl TryFrom<String> for f64", Function {
            StdIntrinsic::Function(FunctionIntrinsic::F64TryFromString) => Static, "f64::try_from", ["String"], "fn try_from(value: String) -> Result<f64, String> {}";
        }
        "impl TryFrom<i64> for f64", Function {
            StdIntrinsic::Function(FunctionIntrinsic::F64TryFromI64) => Static, "f64::try_from", ["i64"], "fn try_from(value: i64) -> Result<f64, String> {}";
        }
        "impl<T> Option<T>", Option {
            StdIntrinsic::Option(OptionIntrinsic::IsSome) => Shared, "is_some", [], "fn is_some(&self) -> bool {}";
            StdIntrinsic::Option(OptionIntrinsic::IsNone) => Shared, "is_none", [], "fn is_none(&self) -> bool {}";
            StdIntrinsic::Option(OptionIntrinsic::UnwrapOr) => Owned, "unwrap_or", ["T"], "fn unwrap_or(self, fallback: T) -> T {}";
        }
        "impl<T, E> Result<T, E>", Result {
            StdIntrinsic::Result(ResultIntrinsic::IsOk) => Shared, "is_ok", [], "fn is_ok(&self) -> bool {}";
            StdIntrinsic::Result(ResultIntrinsic::IsErr) => Shared, "is_err", [], "fn is_err(&self) -> bool {}";
            StdIntrinsic::Result(ResultIntrinsic::UnwrapOr) => Owned, "unwrap_or", ["T"], "fn unwrap_or(self, fallback: T) -> T {}";
            StdIntrinsic::Result(ResultIntrinsic::Ok) => Owned, "ok", [], "fn ok(self) -> Option<T> {}";
            StdIntrinsic::Result(ResultIntrinsic::Err) => Owned, "err", [], "fn err(self) -> Option<E> {}";
        }
        "impl String", String {
            StdIntrinsic::String(StringIntrinsic::Len) => Shared, "len", [], "fn len(&self) -> i32 {}";
            StdIntrinsic::String(StringIntrinsic::IsEmpty) => Shared, "is_empty", [], "fn is_empty(&self) -> bool {}";
            StdIntrinsic::String(StringIntrinsic::Trim) => Shared, "trim", [], "fn trim(&self) -> String {}";
            StdIntrinsic::String(StringIntrinsic::TrimStart) => Shared, "trim_start", [], "fn trim_start(&self) -> String {}";
            StdIntrinsic::String(StringIntrinsic::TrimEnd) => Shared, "trim_end", [], "fn trim_end(&self) -> String {}";
            StdIntrinsic::String(StringIntrinsic::Split) => Shared, "split", ["String"], "fn split(&self, separator: String) -> [String] {}";
            StdIntrinsic::String(StringIntrinsic::SplitOnce) => Shared, "split_once", ["String"], "fn split_once(&self, delimiter: String) -> Option<(String, String)> {}";
            StdIntrinsic::String(StringIntrinsic::RsplitOnce) => Shared, "rsplit_once", ["String"], "fn rsplit_once(&self, delimiter: String) -> Option<(String, String)> {}";
            StdIntrinsic::String(StringIntrinsic::SplitWhitespace) => Shared, "split_whitespace", [], "fn split_whitespace(&self) -> [String] {}";
            StdIntrinsic::String(StringIntrinsic::Lines) => Shared, "lines", [], "fn lines(&self) -> [String] {}";
            StdIntrinsic::String(StringIntrinsic::CharAt) => Shared, "char_at", ["i32"], "fn char_at(&self, index: i32) -> String {}";
            StdIntrinsic::String(StringIntrinsic::Slice) => Shared, "slice", ["i32", "i32"], "fn slice(&self, start: i32, end: i32) -> String {}";
            StdIntrinsic::String(StringIntrinsic::Slice) => Shared, "substring", ["i32", "i32"], "fn substring(&self, start: i32, end: i32) -> String {}";
            StdIntrinsic::String(StringIntrinsic::IndexOf) => Shared, "index_of", ["String"], "fn index_of(&self, search: String) -> i32 {}";
            StdIntrinsic::String(StringIntrinsic::LastIndexOf) => Shared, "last_index_of", ["String"], "fn last_index_of(&self, search: String) -> i32 {}";
            StdIntrinsic::String(StringIntrinsic::StartsWith) => Shared, "starts_with", ["String"], "fn starts_with(&self, prefix: String) -> bool {}";
            StdIntrinsic::String(StringIntrinsic::EndsWith) => Shared, "ends_with", ["String"], "fn ends_with(&self, suffix: String) -> bool {}";
            StdIntrinsic::String(StringIntrinsic::Contains) => Shared, "contains", ["String"], "fn contains(&self, search: String) -> bool {}";
            StdIntrinsic::String(StringIntrinsic::Contains) => Shared, "includes", ["String"], "fn includes(&self, search: String) -> bool {}";
            StdIntrinsic::String(StringIntrinsic::StripPrefix) => Shared, "strip_prefix", ["String"], "fn strip_prefix(&self, prefix: String) -> Option<String> {}";
            StdIntrinsic::String(StringIntrinsic::StripSuffix) => Shared, "strip_suffix", ["String"], "fn strip_suffix(&self, suffix: String) -> Option<String> {}";
            StdIntrinsic::String(StringIntrinsic::Replace) => Shared, "replace", ["String", "String"], "fn replace(&self, pattern: String, replacement: String) -> String {}";
            StdIntrinsic::String(StringIntrinsic::Repeat) => Shared, "repeat", ["i32"], "fn repeat(&self, count: i32) -> String {}";
            StdIntrinsic::String(StringIntrinsic::ToLowercase) => Shared, "to_lowercase", [], "fn to_lowercase(&self) -> String {}";
            StdIntrinsic::String(StringIntrinsic::ToUppercase) => Shared, "to_uppercase", [], "fn to_uppercase(&self) -> String {}";
            StdIntrinsic::String(StringIntrinsic::ToLowercase) => Shared, "to_lower_case", [], "fn to_lower_case(&self) -> String {}";
            StdIntrinsic::String(StringIntrinsic::ToUppercase) => Shared, "to_upper_case", [], "fn to_upper_case(&self) -> String {}";
            StdIntrinsic::String(StringIntrinsic::IsAsciiDigit) => Shared, "is_ascii_digit", [], "fn is_ascii_digit(&self) -> bool {}";
            StdIntrinsic::String(StringIntrinsic::ToDigit) => Shared, "to_digit", ["i32"], "fn to_digit(&self, radix: i32) -> Option<i32> {}";
            StdIntrinsic::String(StringIntrinsic::Iter) => Shared, "iter", [], "fn iter(&self) -> [String] {}";
            StdIntrinsic::String(StringIntrinsic::Iter) => Owned, "into_iter", [], "fn into_iter(self) -> [String] {}";
        }
        "impl i32", I32 {
            StdIntrinsic::Integer(IntegerWidth::I32, IntegerIntrinsic::ToString) => Shared, "to_string", [], "fn to_string(&self) -> String {}";
            StdIntrinsic::Integer(IntegerWidth::I32, IntegerIntrinsic::Abs) => Shared, "abs", [], "fn abs(&self) -> i32 {}";
            StdIntrinsic::Integer(IntegerWidth::I32, IntegerIntrinsic::Min) => Shared, "min", ["i32"], "fn min(&self, other: i32) -> i32 {}";
            StdIntrinsic::Integer(IntegerWidth::I32, IntegerIntrinsic::Max) => Shared, "max", ["i32"], "fn max(&self, other: i32) -> i32 {}";
            StdIntrinsic::Integer(IntegerWidth::I32, IntegerIntrinsic::Clamp) => Shared, "clamp", ["i32", "i32"], "fn clamp(&self, minimum: i32, maximum: i32) -> i32 {}";
            StdIntrinsic::Integer(IntegerWidth::I32, IntegerIntrinsic::CheckedAdd) => Shared, "checked_add", ["i32"], "fn checked_add(&self, other: i32) -> Option<i32> {}";
            StdIntrinsic::Integer(IntegerWidth::I32, IntegerIntrinsic::CheckedSub) => Shared, "checked_sub", ["i32"], "fn checked_sub(&self, other: i32) -> Option<i32> {}";
            StdIntrinsic::Integer(IntegerWidth::I32, IntegerIntrinsic::CheckedMul) => Shared, "checked_mul", ["i32"], "fn checked_mul(&self, other: i32) -> Option<i32> {}";
            StdIntrinsic::Integer(IntegerWidth::I32, IntegerIntrinsic::SaturatingAdd) => Shared, "saturating_add", ["i32"], "fn saturating_add(&self, other: i32) -> i32 {}";
            StdIntrinsic::Integer(IntegerWidth::I32, IntegerIntrinsic::SaturatingSub) => Shared, "saturating_sub", ["i32"], "fn saturating_sub(&self, other: i32) -> i32 {}";
            StdIntrinsic::Integer(IntegerWidth::I32, IntegerIntrinsic::SaturatingMul) => Shared, "saturating_mul", ["i32"], "fn saturating_mul(&self, other: i32) -> i32 {}";
        }
        "impl i64", I64 {
            StdIntrinsic::Integer(IntegerWidth::I64, IntegerIntrinsic::ToString) => Shared, "to_string", [], "fn to_string(&self) -> String {}";
            StdIntrinsic::Integer(IntegerWidth::I64, IntegerIntrinsic::Abs) => Shared, "abs", [], "fn abs(&self) -> i64 {}";
            StdIntrinsic::Integer(IntegerWidth::I64, IntegerIntrinsic::Min) => Shared, "min", ["i64"], "fn min(&self, other: i64) -> i64 {}";
            StdIntrinsic::Integer(IntegerWidth::I64, IntegerIntrinsic::Max) => Shared, "max", ["i64"], "fn max(&self, other: i64) -> i64 {}";
            StdIntrinsic::Integer(IntegerWidth::I64, IntegerIntrinsic::Clamp) => Shared, "clamp", ["i64", "i64"], "fn clamp(&self, minimum: i64, maximum: i64) -> i64 {}";
            StdIntrinsic::Integer(IntegerWidth::I64, IntegerIntrinsic::CheckedAdd) => Shared, "checked_add", ["i64"], "fn checked_add(&self, other: i64) -> Option<i64> {}";
            StdIntrinsic::Integer(IntegerWidth::I64, IntegerIntrinsic::CheckedSub) => Shared, "checked_sub", ["i64"], "fn checked_sub(&self, other: i64) -> Option<i64> {}";
            StdIntrinsic::Integer(IntegerWidth::I64, IntegerIntrinsic::CheckedMul) => Shared, "checked_mul", ["i64"], "fn checked_mul(&self, other: i64) -> Option<i64> {}";
            StdIntrinsic::Integer(IntegerWidth::I64, IntegerIntrinsic::SaturatingAdd) => Shared, "saturating_add", ["i64"], "fn saturating_add(&self, other: i64) -> i64 {}";
            StdIntrinsic::Integer(IntegerWidth::I64, IntegerIntrinsic::SaturatingSub) => Shared, "saturating_sub", ["i64"], "fn saturating_sub(&self, other: i64) -> i64 {}";
            StdIntrinsic::Integer(IntegerWidth::I64, IntegerIntrinsic::SaturatingMul) => Shared, "saturating_mul", ["i64"], "fn saturating_mul(&self, other: i64) -> i64 {}";
        }
        "impl f64", F64 {
            StdIntrinsic::Float(FloatIntrinsic::ToString) => Shared, "to_string", [], "fn to_string(&self) -> String {}";
            StdIntrinsic::Float(FloatIntrinsic::Floor) => Shared, "floor", [], "fn floor(&self) -> f64 {}";
            StdIntrinsic::Float(FloatIntrinsic::Ceil) => Shared, "ceil", [], "fn ceil(&self) -> f64 {}";
            StdIntrinsic::Float(FloatIntrinsic::Round) => Shared, "round", [], "fn round(&self) -> f64 {}";
            StdIntrinsic::Float(FloatIntrinsic::Abs) => Shared, "abs", [], "fn abs(&self) -> f64 {}";
            StdIntrinsic::Float(FloatIntrinsic::Min) => Shared, "min", ["f64"], "fn min(&self, other: f64) -> f64 {}";
            StdIntrinsic::Float(FloatIntrinsic::Max) => Shared, "max", ["f64"], "fn max(&self, other: f64) -> f64 {}";
            StdIntrinsic::Float(FloatIntrinsic::Clamp) => Shared, "clamp", ["f64", "f64"], "fn clamp(&self, minimum: f64, maximum: f64) -> f64 {}";
        }
        "impl bool", Bool {
            StdIntrinsic::BoolToString => Shared, "to_string", [], "fn to_string(&self) -> String {}";
        }
        "impl<T> [T]", Array {
            StdIntrinsic::Array(ArrayIntrinsic::Len) => Shared, "len", [], "fn len(&self) -> i32 {}";
            StdIntrinsic::Array(ArrayIntrinsic::IsEmpty) => Shared, "is_empty", [], "fn is_empty(&self) -> bool {}";
            StdIntrinsic::Array(ArrayIntrinsic::Get) => Shared, "get", ["i32"], "fn get(&self, index: i32) -> Option<T> {}";
            StdIntrinsic::Array(ArrayIntrinsic::First) => Shared, "first", [], "fn first(&self) -> Option<T> {}";
            StdIntrinsic::Array(ArrayIntrinsic::Last) => Shared, "last", [], "fn last(&self) -> Option<T> {}";
            StdIntrinsic::Array(ArrayIntrinsic::Push) => Mutable, "push", ["T"], "fn push(&mut self, value: T) {}";
            StdIntrinsic::Array(ArrayIntrinsic::Slice) => Shared, "slice", ["i32", "i32"], "fn slice(&self, start: i32, end: i32) -> [T] {}";
            StdIntrinsic::Array(ArrayIntrinsic::Join) => Shared, "join", ["String"], "fn join(&self, separator: String) -> String {}";
            StdIntrinsic::Array(ArrayIntrinsic::Sort) => Mutable, "sort", [], "fn sort(&mut self) {}";
            StdIntrinsic::Array(ArrayIntrinsic::Reverse) => Mutable, "reverse", [], "fn reverse(&mut self) {}";
            StdIntrinsic::Array(ArrayIntrinsic::IndexOf) => Shared, "index_of", ["T"], "fn index_of(&self, value: T) -> i32 {}";
            StdIntrinsic::Array(ArrayIntrinsic::LastIndexOf) => Shared, "last_index_of", ["T"], "fn last_index_of(&self, value: T) -> i32 {}";
            StdIntrinsic::Array(ArrayIntrinsic::Contains) => Shared, "contains", ["T"], "fn contains(&self, value: T) -> bool {}";
            StdIntrinsic::Array(ArrayIntrinsic::Contains) => Shared, "includes", ["T"], "fn includes(&self, value: T) -> bool {}";
            StdIntrinsic::Array(ArrayIntrinsic::Pop) => Mutable, "pop", [], "fn pop(&mut self) -> Option<T> {}";
            StdIntrinsic::Array(ArrayIntrinsic::Shift) => Mutable, "shift", [], "fn shift(&mut self) -> Option<T> {}";
            StdIntrinsic::Array(ArrayIntrinsic::Unshift) => Mutable, "unshift", ["T"], "fn unshift(&mut self, value: T) {}";
            StdIntrinsic::Array(ArrayIntrinsic::Iter) => Shared, "iter", [], "fn iter(&self) -> [T] {}";
            StdIntrinsic::Array(ArrayIntrinsic::Iter) => Owned, "into_iter", [], "fn into_iter(self) -> [T] {}";
        }
        "impl<T> Range<T>", Range {
            StdIntrinsic::Range(RangeIntrinsic::Contains) => Shared, "contains", ["T"], "fn contains(&self, value: T) -> bool {}";
            StdIntrinsic::Range(RangeIntrinsic::IsEmpty) => Shared, "is_empty", [], "fn is_empty(&self) -> bool {}";
            StdIntrinsic::Range(RangeIntrinsic::Iter) => Shared, "iter", [], "fn iter(&self) -> Range<T> {}";
            StdIntrinsic::Range(RangeIntrinsic::Iter) => Owned, "into_iter", [], "fn into_iter(self) -> Range<T> {}";
        }
    }
    pseudo {
        StdIntrinsic::String(StringIntrinsic::Parse) => String, Shared, "parse", [];
        StdIntrinsic::Into => Any, Owned, "into", [];
        StdIntrinsic::TryInto => Any, Owned, "try_into", [];
        StdIntrinsic::ArrayHigherOrder(ArrayHigherOrderIntrinsic::Map) => Array, Shared, "map", ["callback"];
        StdIntrinsic::ArrayHigherOrder(ArrayHigherOrderIntrinsic::Filter) => Array, Shared, "filter", ["callback"];
        StdIntrinsic::ArrayHigherOrder(ArrayHigherOrderIntrinsic::Some) => Array, Shared, "some", ["callback"];
        StdIntrinsic::ArrayHigherOrder(ArrayHigherOrderIntrinsic::Every) => Array, Shared, "every", ["callback"];
        StdIntrinsic::ArrayHigherOrder(ArrayHigherOrderIntrinsic::Reduce) => Array, Shared, "reduce", ["callback"];
        StdIntrinsic::ArrayHigherOrder(ArrayHigherOrderIntrinsic::ForEach) => Array, Shared, "forEach", ["callback"];
        StdIntrinsic::ArrayHigherOrder(ArrayHigherOrderIntrinsic::ForEach) => Array, Shared, "for_each", ["callback"];
        StdIntrinsic::ArrayHigherOrder(ArrayHigherOrderIntrinsic::Find) => Array, Shared, "find", ["callback"];
        StdIntrinsic::ArrayHigherOrder(ArrayHigherOrderIntrinsic::FindIndex) => Array, Shared, "findIndex", ["callback"];
        StdIntrinsic::ArrayHigherOrder(ArrayHigherOrderIntrinsic::FindIndex) => Array, Shared, "find_index", ["callback"];
        StdIntrinsic::ArrayHigherOrder(ArrayHigherOrderIntrinsic::FindLastIndex) => Array, Shared, "findLastIndex", ["callback"];
        StdIntrinsic::ArrayHigherOrder(ArrayHigherOrderIntrinsic::FindLastIndex) => Array, Shared, "find_last_index", ["callback"];
        StdIntrinsic::ArrayHigherOrder(ArrayHigherOrderIntrinsic::SortBy) => Array, Mutable, "sortBy", ["callback"];
        StdIntrinsic::ArrayHigherOrder(ArrayHigherOrderIntrinsic::SortBy) => Array, Mutable, "sort_by", ["callback"];
    }
}

impl StdIntrinsic {
    /// Look up a native method without runtime string dispatch.
    #[must_use]
    pub fn resolve_method(receiver_type: &str, method: &str) -> Option<Self> {
        static METHODS: OnceLock<HashMap<(ReceiverKind, &'static str), StdIntrinsic>> =
            OnceLock::new();
        let receiver = ReceiverKind::from_type(receiver_type)?;
        let methods = METHODS.get_or_init(|| {
            INTRINSICS
                .iter()
                .filter(|descriptor| descriptor.receiver != ReceiverKind::Function)
                .map(|descriptor| ((descriptor.receiver, descriptor.name), descriptor.intrinsic))
                .collect()
        });
        methods
            .get(&(receiver, method))
            .or_else(|| methods.get(&(ReceiverKind::Any, method)))
            .copied()
    }

    /// Resolve an overloaded free or associated function using semantic argument types.
    #[must_use]
    pub fn resolve_function(path: &str, argument_types: &[Option<&str>]) -> FunctionResolution {
        static FUNCTIONS: OnceLock<HashMap<&'static str, Vec<&'static IntrinsicDescriptor>>> =
            OnceLock::new();
        let functions = FUNCTIONS.get_or_init(|| {
            let mut functions: HashMap<&'static str, Vec<&'static IntrinsicDescriptor>> =
                HashMap::new();
            for descriptor in INTRINSICS
                .iter()
                .filter(|descriptor| descriptor.receiver == ReceiverKind::Function)
            {
                functions
                    .entry(descriptor.name)
                    .or_default()
                    .push(descriptor);
            }
            functions
        });
        let Some(candidates) = functions.get(path) else {
            return FunctionResolution::Missing;
        };
        let mut matches = candidates.iter().filter(|descriptor| {
            descriptor.parameters.len() == argument_types.len()
                && descriptor
                    .parameters
                    .iter()
                    .zip(argument_types)
                    .all(|(expected, actual)| actual.is_none_or(|actual| actual == *expected))
        });
        let Some(first) = matches.next() else {
            return FunctionResolution::Missing;
        };
        if matches.next().is_some() {
            FunctionResolution::Ambiguous
        } else {
            FunctionResolution::Unique(first.intrinsic)
        }
    }

    /// Return the canonical descriptor for this operation.
    #[must_use]
    pub fn descriptor(self) -> Option<IntrinsicDescriptor> {
        INTRINSICS
            .iter()
            .find(|descriptor| descriptor.intrinsic == self)
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_prelude_contains_the_canonical_safe_contracts() {
        let prelude = native_prelude();
        assert!(prelude.contains("fn pop(&mut self) -> Option<T> {}"));
        assert!(prelude.contains("fn shift(&mut self) -> Option<T> {}"));
        assert!(prelude.contains("fn sort(&mut self) {}"));
        assert!(prelude.contains("fn reverse(&mut self) {}"));
        assert!(prelude.contains("impl TryFrom<i64> for f64"));
        assert!(!prelude.contains("impl From<i64> for f64"));
    }

    #[test]
    fn method_lookup_is_typed_and_keeps_compatibility_aliases() {
        assert_eq!(
            StdIntrinsic::resolve_method("String", "trim"),
            Some(StdIntrinsic::String(StringIntrinsic::Trim))
        );
        assert_eq!(
            StdIntrinsic::resolve_method("[i32]", "pop"),
            Some(StdIntrinsic::Array(ArrayIntrinsic::Pop))
        );
        assert_eq!(
            StdIntrinsic::resolve_method("Result<i32, String>", "ok"),
            Some(StdIntrinsic::Result(ResultIntrinsic::Ok))
        );
        assert_eq!(
            StdIntrinsic::resolve_method("[String]", "findLastIndex"),
            Some(StdIntrinsic::ArrayHigherOrder(
                ArrayHigherOrderIntrinsic::FindLastIndex
            ))
        );
        assert_eq!(
            StdIntrinsic::resolve_method("i64", "into"),
            Some(StdIntrinsic::Into)
        );
        assert_eq!(StdIntrinsic::resolve_method("String", "missing"), None);
    }

    #[test]
    fn associated_function_lookup_uses_the_resolved_parameter_type() {
        assert_eq!(
            StdIntrinsic::resolve_function("String::from", &[Some("i32")]),
            FunctionResolution::Unique(StdIntrinsic::Function(FunctionIntrinsic::StringFromI32))
        );
        assert_eq!(
            StdIntrinsic::resolve_function("f64::try_from", &[Some("i64")]),
            FunctionResolution::Unique(StdIntrinsic::Function(FunctionIntrinsic::F64TryFromI64))
        );
        assert_eq!(
            StdIntrinsic::resolve_function("String::from", &[None]),
            FunctionResolution::Ambiguous
        );
        assert_eq!(
            StdIntrinsic::resolve_function("String::missing", &[]),
            FunctionResolution::Missing
        );
    }

    #[test]
    fn inventory_does_not_define_conflicting_method_or_function_signatures() {
        let mut methods = HashMap::new();
        let mut functions = HashMap::new();
        for descriptor in INTRINSICS {
            if descriptor.receiver == ReceiverKind::Function {
                assert!(
                    functions
                        .insert(
                            (descriptor.name, descriptor.parameters),
                            descriptor.intrinsic
                        )
                        .is_none(),
                    "duplicate function signature: {}({})",
                    descriptor.name,
                    descriptor.parameters.join(", ")
                );
            } else {
                assert!(
                    methods
                        .insert((descriptor.receiver, descriptor.name), descriptor.intrinsic)
                        .is_none(),
                    "duplicate method signature: {:?}::{}",
                    descriptor.receiver,
                    descriptor.name
                );
            }
        }
    }
}
