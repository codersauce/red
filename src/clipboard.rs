//! Replaceable clipboard boundary for native operation, disabled environments, and tests.
//!
//! [`ClipboardProvider`] keeps platform clipboard failures outside editor logic.
//! [`NativeClipboardProvider`] delegates to the operating system, while the disabled and
//! in-memory implementations give headless or test callers predictable behavior.

use anyhow::Context;
use std::sync::{Arc, Mutex};

/// Fallible text clipboard operations required by the editor.
pub trait ClipboardProvider: Send {
    /// Reads the current text value, returning `None` when no text is available.
    fn get_text(&mut self) -> anyhow::Result<Option<String>>;
    /// Replaces the current text value.
    fn set_text(&mut self, text: &str) -> anyhow::Result<()>;
}

/// Operating-system clipboard provider backed by `arboard`.
pub struct NativeClipboardProvider {
    clipboard: arboard::Clipboard,
}

impl NativeClipboardProvider {
    /// Connects to the platform clipboard service.
    ///
    /// Construction fails when no supported display or clipboard service is available.
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            clipboard: arboard::Clipboard::new()
                .context("failed to initialize system clipboard")?,
        })
    }
}

impl ClipboardProvider for NativeClipboardProvider {
    fn get_text(&mut self) -> anyhow::Result<Option<String>> {
        match self.clipboard.get_text() {
            Ok(text) => Ok(Some(text)),
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(error) => Err(error).context("failed to read system clipboard"),
        }
    }

    fn set_text(&mut self, text: &str) -> anyhow::Result<()> {
        self.clipboard
            .set_text(text.to_string())
            .context("failed to write system clipboard")
    }
}

/// Provider that treats clipboard operations as explicitly unavailable no-ops.
pub struct DisabledClipboardProvider;

impl ClipboardProvider for DisabledClipboardProvider {
    fn get_text(&mut self) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    fn set_text(&mut self, _text: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
/// Deterministic shared-memory clipboard used by tests and embedded callers.
pub struct MemoryClipboardProvider {
    text: Arc<Mutex<Option<String>>>,
}

impl MemoryClipboardProvider {
    /// Creates an in-memory clipboard containing `text`.
    pub fn with_text(text: impl Into<String>) -> Self {
        let provider = Self::default();
        provider.set_shared_text(Some(text.into()));
        provider
    }

    /// Returns shared storage for assertions or coordination with a test peer.
    pub fn shared_text(&self) -> Arc<Mutex<Option<String>>> {
        self.text.clone()
    }

    fn set_shared_text(&self, text: Option<String>) {
        if let Ok(mut current) = self.text.lock() {
            *current = text;
        }
    }
}

impl ClipboardProvider for MemoryClipboardProvider {
    fn get_text(&mut self) -> anyhow::Result<Option<String>> {
        Ok(self.text.lock().ok().and_then(|text| text.clone()))
    }

    fn set_text(&mut self, text: &str) -> anyhow::Result<()> {
        self.set_shared_text(Some(text.to_string()));
        Ok(())
    }
}

impl From<Arc<Mutex<Option<String>>>> for MemoryClipboardProvider {
    fn from(text: Arc<Mutex<Option<String>>>) -> Self {
        Self { text }
    }
}
