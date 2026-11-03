//! Selection-first editing kernel for Red's experimental Helix model.
//!
//! This module is the seed of the future `felix` crate: a headless library
//! with no terminal, LSP, or plugin dependencies. Red keeps the Vim engine
//! and calls into `felix` only when `editing_model` is `"helix"`. Extraction
//! into a standalone crate is intentionally not done yet.

mod selection;

pub use selection::{Range, Selection};
