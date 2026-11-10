//! Backend-neutral vocabulary for Red's agent.
//!
//! This module is a leaf: it names no concrete agent backend. Backends turn
//! their wire formats into these types, and the editor and plugins consume
//! only these types. Serialization of a type reproduces the JSON the plugin
//! API has always carried for it.

pub mod backend;
pub mod events;
pub mod ids;
pub mod tool_host;
pub mod types;
pub mod workspace;
