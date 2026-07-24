//! Backend-neutral standard-library declarations and primitives for Husk.
//!
//! The native prelude is the single source of truth for the language-visible
//! signatures. Pure helpers in this crate are shared by runtime backends and
//! deliberately have no dependency on the interpreter or host application.

pub mod intrinsic;
pub mod number;

pub use intrinsic::{
    ArrayHigherOrderIntrinsic, ArrayIntrinsic, FloatIntrinsic, FunctionIntrinsic,
    FunctionResolution, IntegerIntrinsic, IntegerWidth, OptionIntrinsic, RangeIntrinsic,
    ReceiverKind, ReceiverMode, ResultIntrinsic, StdIntrinsic, StringIntrinsic, native_prelude,
};
