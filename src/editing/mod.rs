//! Reusable modal text editing without editor, terminal, LSP, or plugin ownership.
//!
//! File-backed editor windows and embedded plugin text areas share the same motion
//! resolver and transactional replacement primitive. A [`TextArea`] owns only an
//! unnamed [`crate::buffer::Buffer`] and surface-local interaction state; application
//! commands, filesystem access, LSP notifications, and rendering remain host concerns.

mod line_hunks;
mod motion;
mod multi_selection;
mod reflow;
mod textarea;
mod transaction;

pub(crate) use line_hunks::{
    line_hunks, map_line, preview_line_text, unified_diff, DiffLine, DiffLineKind, DiffStat,
};
pub use motion::{CharacterMotion, MotionResolver, TextObjectKind, TextObjectScope};
pub(crate) use multi_selection::{CharRange, SelectionSet};
pub(crate) use reflow::{plain_line, reflow_text, ReflowLine};
pub use textarea::{EditState, RegisterContent, TextArea, TextAreaOutcome};
pub(crate) use transaction::apply_transactional_replacement;
