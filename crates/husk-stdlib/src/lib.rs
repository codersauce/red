//! Backend-neutral standard-library declarations and primitives for Husk.
//!
//! The native prelude is the single source of truth for the language-visible
//! signatures. Pure helpers in this crate are shared by runtime backends and
//! deliberately have no dependency on the interpreter or host application.

pub mod number;

/// Canonical declarations injected into every native Husk compilation.
pub const NATIVE_PRELUDE: &str = include_str!("../prelude/native.hk");
