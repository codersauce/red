//! Reusable modal text editing without editor, terminal, LSP, or plugin ownership.
//!
//! File-backed editor windows and embedded plugin text areas share the same motion
//! resolver and transactional replacement primitive. A [`TextArea`] owns only an
//! unnamed [`crate::buffer::Buffer`] and surface-local interaction state; application
//! commands, filesystem access, LSP notifications, and rendering remain host concerns.

mod motion;
mod reflow;
mod textarea;
mod transaction;

pub(crate) use motion::text_object_kind_for_key;
pub use motion::{CharacterMotion, MotionResolver, TextObjectKind, TextObjectScope};
pub(crate) use reflow::{plain_line, reflow_text, ReflowLine};
pub use textarea::{EditState, RegisterContent, TextArea, TextAreaOutcome};
pub(crate) use transaction::apply_transactional_replacement;
