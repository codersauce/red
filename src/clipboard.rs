//! Clipboard boundary shared by editor buffers, agent panels, and detached frontends.
//!
//! [`ClipboardProvider`] keeps platform clipboard failures outside editor logic.
//! [`NativeClipboardProvider`] delegates to the operating system, while the disabled and
//! in-memory implementations give headless or test callers predictable behavior.

use anyhow::Context;
use std::sync::{Arc, Mutex};

mod transport;
pub use transport::{
    default_max_bytes, deserialize_max_bytes, ClipboardBackend, Passthrough, SessionClipboard,
};

/// Bound shared by command output and detach IPC, independent of configured limits.
pub const MAX_CLIPBOARD_BYTES: usize = 256 * 1024;

/// A terminal write is not an acknowledgment from the desktop clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyOutcome {
    Written,
    Sent,
    Deferred,
}

/// Fallible text clipboard operations required by the editor.
pub trait ClipboardProvider: Send {
    /// Reports whether a copy route is configured, not whether delivery was acknowledged.
    fn is_available(&self) -> bool {
        true
    }
    /// Reads the current text value, returning `None` when no text is available.
    fn get_text(&mut self) -> anyhow::Result<Option<String>>;
    /// The detached bridge requests text only when an editing action actually pastes.
    fn read_text(&mut self) -> futures::future::BoxFuture<'_, anyhow::Result<Option<String>>> {
        Box::pin(async move { self.get_text() })
    }
    /// Replaces the current text value.
    fn set_text(&mut self, text: &str) -> anyhow::Result<()>;

    /// Writes through the same serialized output stream as terminal rendering.
    fn copy(
        &mut self,
        text: &str,
        _output: &mut dyn std::io::Write,
    ) -> anyhow::Result<CopyOutcome> {
        anyhow::ensure!(
            self.is_available(),
            "clipboard unavailable; text kept in Red's register"
        );
        self.set_text(text)?;
        Ok(CopyOutcome::Written)
    }
}

/// Per-input bridge for the detached owner. Effects are consumed once, never snapshotted.
#[derive(Clone, Default)]
pub(crate) struct ClientClipboard {
    state: Arc<Mutex<ClientClipboardState>>,
}

#[derive(Default)]
struct ClientClipboardState {
    text: Option<String>,
    writes: Vec<String>,
    accepting: bool,
    reader: Option<tokio::sync::mpsc::UnboundedSender<ClipboardReadRequest>>,
}

pub(crate) type ClipboardReadRequest = tokio::sync::oneshot::Sender<Result<Option<String>, String>>;

impl ClientClipboard {
    #[cfg(unix)]
    pub fn set_reader(&self, reader: tokio::sync::mpsc::UnboundedSender<ClipboardReadRequest>) {
        self.state.lock().unwrap().reader = Some(reader);
    }
    pub fn begin_input(&self, text: Option<String>) {
        let mut state = self.state.lock().unwrap();
        state.text = text;
        state.writes.clear();
        state.accepting = true;
    }

    pub fn finish_input(&self) -> Vec<String> {
        let mut state = self.state.lock().unwrap();
        state.accepting = false;
        state.text = None;
        state.reader = None;
        std::mem::take(&mut state.writes)
    }
}

impl ClipboardProvider for ClientClipboard {
    fn get_text(&mut self) -> anyhow::Result<Option<String>> {
        Ok(self.state.lock().unwrap().text.clone())
    }

    fn read_text(&mut self) -> futures::future::BoxFuture<'_, anyhow::Result<Option<String>>> {
        let state = self.state.lock().unwrap();
        // A macro may yank and paste in one input; that new register takes precedence
        // until its write is delivered at the end of the input transaction.
        if state.text.is_some() || state.reader.is_none() {
            let text = state.text.clone();
            return Box::pin(async move { Ok(text) });
        }
        let reader = state.reader.clone().unwrap();
        drop(state);
        Box::pin(async move {
            let (reply, receiver) = tokio::sync::oneshot::channel();
            reader
                .send(reply)
                .map_err(|_| anyhow::anyhow!("clipboard client disconnected"))?;
            tokio::time::timeout(std::time::Duration::from_secs(3), receiver)
                .await
                .context("clipboard client read timed out")?
                .context("clipboard client disconnected")?
                .map_err(anyhow::Error::msg)
        })
    }

    fn set_text(&mut self, text: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            text.len() <= MAX_CLIPBOARD_BYTES,
            "clipboard payload exceeds the 256 KiB safety limit; text kept in Red's register"
        );
        let mut state = self.state.lock().unwrap();
        anyhow::ensure!(
            state.accepting,
            "clipboard delivery requires an attached user input"
        );
        state.text = Some(text.to_owned());
        // Only the last write in one input matters; macros must not grow the queue unboundedly.
        state.writes.clear();
        state.writes.push(text.to_owned());
        Ok(())
    }

    fn copy(
        &mut self,
        text: &str,
        _output: &mut dyn std::io::Write,
    ) -> anyhow::Result<CopyOutcome> {
        self.set_text(text)?;
        Ok(CopyOutcome::Deferred)
    }
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
    fn is_available(&self) -> bool {
        false
    }

    fn get_text(&mut self) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    fn set_text(&mut self, _text: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn copy(
        &mut self,
        _text: &str,
        _output: &mut dyn std::io::Write,
    ) -> anyhow::Result<CopyOutcome> {
        // This provider is also the explicit no-side-effects test boundary.
        Ok(CopyOutcome::Written)
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
