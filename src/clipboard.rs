//! Clipboard boundary shared by editor buffers, agent panels, and detached frontends.
//!
//! [`ClipboardProvider`] keeps platform clipboard failures outside editor logic.
//! [`NativeClipboardProvider`] delegates to the operating system, while the disabled and
//! in-memory implementations give headless or test callers predictable behavior.

use anyhow::Context;
use std::sync::{Arc, Mutex};

mod transport;
pub use transport::{default_max_bytes, deserialize_max_bytes, ClipboardBackend, SessionClipboard};

/// Detached clipboard IPC bound; preserves the existing plugin copy limit.
pub const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;

/// A terminal write is not an acknowledgment from the desktop clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyOutcome {
    Written,
    Sent,
    Deferred,
}

/// Whether this frontend can read a primary selection or only send it to its terminal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimarySelection {
    #[default]
    Unavailable,
    ReadWrite,
    WriteOnly,
}

/// An input carrying these capabilities can acknowledge writes before an edit commits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClipboardCapabilities {
    pub primary: PrimarySelection,
    pub system_write_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardTarget {
    System,
    Primary,
}

/// Fallible text clipboard operations required by the editor.
pub trait ClipboardProvider: Send {
    fn primary_selection(&self) -> PrimarySelection {
        PrimarySelection::Unavailable
    }

    fn read_primary(&mut self) -> futures::future::BoxFuture<'_, anyhow::Result<Option<String>>> {
        Box::pin(async { anyhow::bail!("Primary selection clipboard is unavailable") })
    }

    /// Synchronous providers write before returning. A detached provider waits for
    /// the frontend's acknowledgment before the editor applies a destructive edit.
    fn copy_primary(
        &mut self,
        _text: &str,
        _output: &mut dyn std::io::Write,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async { anyhow::bail!("Primary selection clipboard is unavailable") })
    }

    /// Copy to the system clipboard before applying an edit. Older attached clients
    /// deliver system writes with the next render, as in the original IPC protocol.
    fn copy_system_for_edit(
        &mut self,
        text: &str,
        output: &mut dyn std::io::Write,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(std::future::ready(self.copy(text, output).map(|_| ())))
    }

    /// Reports whether a copy route is configured, not whether delivery was acknowledged.
    fn is_available(&self) -> bool {
        true
    }
    /// True for a configured write-only clipboard route. Its last successful yank
    /// is the only value the editor can paste until the terminal sends a paste event.
    fn uses_saved_yank(&self) -> bool {
        false
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

/// Attachment-owned clipboard bridge. Writes are consumed once, never snapshotted.
#[derive(Clone, Default)]
pub(crate) struct ClientClipboard {
    state: Arc<Mutex<ClientClipboardState>>,
}

#[derive(Default)]
struct ClientClipboardState {
    text: Option<String>,
    write: Option<String>,
    accepting: bool,
    reader: Option<tokio::sync::mpsc::UnboundedSender<ClipboardRequest>>,
    capabilities: Option<ClipboardCapabilities>,
}

pub(crate) enum ClipboardOperation {
    ReadSystem,
    ReadPrimary,
    Write(ClipboardTarget, String),
}

pub(crate) struct ClipboardRequest {
    pub operation: ClipboardOperation,
    pub reply: tokio::sync::oneshot::Sender<Result<Option<String>, String>>,
}

impl ClientClipboard {
    #[cfg(unix)]
    pub fn set_reader(
        &self,
        reader: tokio::sync::mpsc::UnboundedSender<ClipboardRequest>,
        capabilities: Option<ClipboardCapabilities>,
    ) {
        let mut state = self.state.lock().unwrap();
        state.reader = Some(reader);
        state.capabilities = capabilities;
    }
    pub fn begin_input(&self, text: Option<String>) {
        let mut state = self.state.lock().unwrap();
        state.text = text.or_else(|| state.write.clone());
        state.accepting = true;
    }

    pub fn finish_input(&self) {
        let mut state = self.state.lock().unwrap();
        state.text = None;
        state.reader = None;
        state.capabilities = None;
    }

    pub fn set_attached(&self, attached: bool) {
        let mut state = self.state.lock().unwrap();
        state.accepting = attached;
        if !attached {
            state.write = None;
            state.text = None;
            state.reader = None;
            state.capabilities = None;
        }
    }

    fn request(
        &self,
        operation: ClipboardOperation,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<Option<String>>> {
        let reader = self.state.lock().unwrap().reader.clone();
        Box::pin(async move {
            let reader = reader.context("clipboard operation requires an attached client")?;
            let (reply, receiver) = tokio::sync::oneshot::channel();
            reader
                .send(ClipboardRequest { operation, reply })
                .map_err(|_| anyhow::anyhow!("clipboard client disconnected"))?;
            tokio::time::timeout(std::time::Duration::from_secs(3), receiver)
                .await
                .context("clipboard client timed out")?
                .context("clipboard client disconnected")?
                .map_err(anyhow::Error::msg)
        })
    }

    fn request_write(
        &self,
        target: ClipboardTarget,
        text: &str,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
        if text.len() > MAX_CLIPBOARD_BYTES {
            return Box::pin(async { anyhow::bail!("clipboard write exceeds 1 MiB") });
        }
        let delivery = self.request(ClipboardOperation::Write(target, text.to_owned()));
        Box::pin(async move { delivery.await.map(|_| ()) })
    }

    pub fn take_write(&self) -> Option<String> {
        self.state.lock().unwrap().write.take()
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
        if state.text.is_some()
            || state.reader.is_none()
            || state.capabilities.is_some_and(|c| c.system_write_only)
        {
            let text = state.text.clone();
            return Box::pin(async move { Ok(text) });
        }
        drop(state);
        self.request(ClipboardOperation::ReadSystem)
    }

    fn primary_selection(&self) -> PrimarySelection {
        self.state
            .lock()
            .unwrap()
            .capabilities
            .unwrap_or_default()
            .primary
    }

    fn read_primary(&mut self) -> futures::future::BoxFuture<'_, anyhow::Result<Option<String>>> {
        match self.primary_selection() {
            PrimarySelection::Unavailable => {
                Box::pin(async { anyhow::bail!("Primary selection clipboard is unavailable") })
            }
            PrimarySelection::WriteOnly => Box::pin(async { Ok(None) }),
            PrimarySelection::ReadWrite => self.request(ClipboardOperation::ReadPrimary),
        }
    }

    fn copy_primary(
        &mut self,
        text: &str,
        _: &mut dyn std::io::Write,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
        if self.primary_selection() == PrimarySelection::Unavailable {
            return Box::pin(async { anyhow::bail!("primary selection clipboard is unavailable") });
        }
        self.request_write(ClipboardTarget::Primary, text)
    }

    fn uses_saved_yank(&self) -> bool {
        self.state
            .lock()
            .unwrap()
            .capabilities
            .is_some_and(|c| c.system_write_only)
    }

    fn copy_system_for_edit(
        &mut self,
        text: &str,
        output: &mut dyn std::io::Write,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
        if self.state.lock().unwrap().capabilities.is_none() {
            return Box::pin(std::future::ready(self.copy(text, output).map(|_| ())));
        }
        let delivery = self.request_write(ClipboardTarget::System, text);
        let state = self.state.clone();
        Box::pin(async move {
            delivery.await?;
            // A confirmed write supersedes older deferred writes from this input.
            let mut state = state.lock().unwrap();
            state.write = None;
            state.text = None;
            Ok(())
        })
    }

    fn set_text(&mut self, text: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            text.len() <= MAX_CLIPBOARD_BYTES,
            "clipboard payload exceeds the 1 MiB safety limit; text kept in Red's register"
        );
        let mut state = self.state.lock().unwrap();
        anyhow::ensure!(
            state.accepting,
            "clipboard delivery requires an attached client"
        );
        state.text = Some(text.to_owned());
        // Only the last write in one input matters; macros must not grow the queue unboundedly.
        state.write = Some(text.to_owned());
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
    #[cfg(target_os = "linux")]
    fn primary_selection(&self) -> PrimarySelection {
        PrimarySelection::ReadWrite
    }

    #[cfg(target_os = "linux")]
    fn read_primary(&mut self) -> futures::future::BoxFuture<'_, anyhow::Result<Option<String>>> {
        use arboard::{GetExtLinux, LinuxClipboardKind};
        let value = match self
            .clipboard
            .get()
            .clipboard(LinuxClipboardKind::Primary)
            .text()
        {
            Ok(text) => Ok(Some(text)),
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(error) => Err(error).context("failed to read primary selection"),
        };
        Box::pin(std::future::ready(value))
    }

    #[cfg(target_os = "linux")]
    fn copy_primary(
        &mut self,
        text: &str,
        _: &mut dyn std::io::Write,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
        use arboard::{LinuxClipboardKind, SetExtLinux};
        let result = self
            .clipboard
            .set()
            .clipboard(LinuxClipboardKind::Primary)
            .text(text)
            .context("failed to write primary selection");
        Box::pin(std::future::ready(result))
    }

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
    primary: Arc<Mutex<Option<String>>>,
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
    fn primary_selection(&self) -> PrimarySelection {
        PrimarySelection::ReadWrite
    }
    fn read_primary(&mut self) -> futures::future::BoxFuture<'_, anyhow::Result<Option<String>>> {
        Box::pin(std::future::ready(Ok(self.primary.lock().unwrap().clone())))
    }
    fn copy_primary(
        &mut self,
        text: &str,
        _: &mut dyn std::io::Write,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
        *self.primary.lock().unwrap() = Some(text.to_owned());
        Box::pin(std::future::ready(Ok(())))
    }

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
        Self {
            text,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn confirmed_system_write_supersedes_an_earlier_deferred_copy_only_on_success() {
        let mut owner = ClientClipboard::default();
        owner.set_attached(true);
        let (tx, mut requests) = tokio::sync::mpsc::unbounded_channel();
        owner.set_reader(tx, Some(ClipboardCapabilities::default()));
        for accepted in [false, true] {
            owner.set_text("deferred").unwrap();
            let delivery = owner.copy_system_for_edit("new", &mut std::io::sink());
            let client = async {
                let request = requests.recv().await.unwrap();
                assert!(
                    matches!(request.operation, ClipboardOperation::Write(ClipboardTarget::System, text) if text == "new")
                );
                request
                    .reply
                    .send(if accepted {
                        Ok(None)
                    } else {
                        Err("denied".into())
                    })
                    .unwrap();
            };
            let (result, ()) = tokio::join!(delivery, client);
            assert_eq!(result.is_ok(), accepted);
            assert_eq!(
                owner.take_write().as_deref(),
                (!accepted).then_some("deferred")
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "run with xvfb-run on an isolated display"]
    async fn native_primary_selection_is_independent_of_system_clipboard() {
        let mut owner = NativeClipboardProvider::new().unwrap();
        let mut peer = NativeClipboardProvider::new().unwrap();
        owner.set_text("system").unwrap();
        owner
            .copy_primary("primary 👻", &mut std::io::sink())
            .await
            .unwrap();
        assert_eq!(
            peer.read_primary().await.unwrap().as_deref(),
            Some("primary 👻")
        );
        peer.copy_primary("external", &mut std::io::sink())
            .await
            .unwrap();
        assert_eq!(
            owner.read_primary().await.unwrap().as_deref(),
            Some("external")
        );
        assert_eq!(owner.get_text().unwrap().as_deref(), Some("system"));
    }

    #[test]
    fn detached_background_copies_require_an_attachment_and_never_replay() {
        let mut clipboard = ClientClipboard::default();
        assert!(clipboard.set_text("unattached").is_err());
        clipboard.set_attached(true);
        clipboard.set_text("first").unwrap();
        clipboard.set_text("latest").unwrap();
        clipboard.begin_input(None);
        assert_eq!(clipboard.get_text().unwrap().as_deref(), Some("latest"));
        clipboard.finish_input();
        assert_eq!(clipboard.take_write().as_deref(), Some("latest"));
        assert!(clipboard.take_write().is_none());
        clipboard.set_text("discard on disconnect").unwrap();
        clipboard.set_attached(false);
        clipboard.set_attached(true);
        assert!(clipboard.take_write().is_none());
    }

    #[test]
    fn detached_copy_preserves_the_existing_one_mib_plugin_limit() {
        let mut clipboard = ClientClipboard::default();
        clipboard.set_attached(true);
        clipboard
            .set_text(&"x".repeat(MAX_CLIPBOARD_BYTES))
            .unwrap();
        assert!(clipboard
            .set_text(&"x".repeat(MAX_CLIPBOARD_BYTES + 1))
            .is_err());
        assert_eq!(clipboard.take_write().unwrap().len(), MAX_CLIPBOARD_BYTES);
    }
}
