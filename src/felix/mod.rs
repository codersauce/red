//! Selection-first editing kernel for Red's experimental Helix model.
//!
//! This module is the seed of the future `felix` crate: a headless library
//! with no terminal, LSP, or plugin dependencies. Red keeps the Vim engine
//! and calls into `felix` only when `editing_model` is `"helix"`. Extraction
//! into a standalone crate is intentionally not done yet.

mod command;
mod keymap;
mod selection;
mod text;

pub use command::Command;
pub use keymap::{extend_command, motion_for, normal_keymap, KeyNode};
pub use selection::{Range, Selection};
pub use text::{
    align_edits, apply_motion, copy_range_on_line, extend_line_below, goto_line_motion, join_span,
    line_bounds_range, pair_for, regex_keep, regex_select, regex_split, replacement, select_all,
    shrink_to_line_bounds, split_lines, surround, transform_case, trim_ranges, CaseChange,
    SurroundOp, TextEdit,
};
