//! Language Server Protocol support for Husk.

mod dependencies;
mod protocol;
mod server;
mod uri;

pub use server::{ServerOptions, run_stdio};
