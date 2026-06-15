use anyhow::Context;
use std::sync::{Arc, Mutex};

pub trait ClipboardProvider: Send {
    fn get_text(&mut self) -> anyhow::Result<Option<String>>;
    fn set_text(&mut self, text: &str) -> anyhow::Result<()>;
}

pub struct NativeClipboardProvider {
    clipboard: arboard::Clipboard,
}

impl NativeClipboardProvider {
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
pub struct MemoryClipboardProvider {
    text: Arc<Mutex<Option<String>>>,
}

impl MemoryClipboardProvider {
    pub fn with_text(text: impl Into<String>) -> Self {
        let provider = Self::default();
        provider.set_shared_text(Some(text.into()));
        provider
    }

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
