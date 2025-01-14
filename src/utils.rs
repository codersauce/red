use std::{env, path::PathBuf};

/// Get the current working directory
pub fn get_workspace_path() -> PathBuf {
    env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// Convert to URI format (file:///path/to/workspace)
pub fn get_workspace_uri() -> String {
    format!("file://{}", get_workspace_path().display()).replace("\\", "/")
}
