//! Backend-neutral vocabulary for Red's agent.
//!
//! Apart from [`open`], which picks the backend, this module names no concrete
//! agent backend. Backends turn their wire formats into these types, and the
//! editor and plugins consume only these types. Serialization of a type
//! reproduces the JSON the plugin API has always carried for it.

pub mod backend;
pub mod events;
pub mod ids;
pub mod ledger;
#[cfg(test)]
pub(crate) mod testing;
pub mod tool_host;
pub mod types;
pub mod workspace;

use crate::{codex::CodexBackend, config::AgentConfig};
use backend::AgentBackend;
use ledger::Ledger;
use tool_host::ToolHost;

/// The agent backend the editor talks to, behind the protocol rules every
/// backend must keep. It never starts a process: the first command that needs
/// one does. `host` serves the agent's tool calls.
pub fn open(config: &AgentConfig, host: impl ToolHost) -> Box<dyn AgentBackend> {
    Box::new(Ledger::new(CodexBackend::new(config, host)))
}
