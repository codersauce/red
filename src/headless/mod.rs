//! Versioned local IPC for detachable editor sessions.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[cfg(unix)]
use std::{cell::Cell, rc::Rc};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::{
    io::{
        AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf,
        WriteHalf,
    },
    sync::Mutex,
};

#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};

/// First stable version of Red's detachable-core IPC protocol.
pub const IPC_PROTOCOL_VERSION: u32 = 3;

const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
/// Maximum aggregate size accepted for a chunked paste operation.
///
/// The owner rejects the paste once accumulated UTF-8 bytes exceed this
/// boundary and clears the pending paste state.
pub const MAX_PENDING_PASTE_BYTES: usize = 16 * 1024 * 1024;
const CLIENT_HEARTBEAT_LEASE: Duration = Duration::from_secs(15);
const CLIENT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const CLIENT_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const CLIENT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_TERMINAL_COLUMNS: u16 = 4096;
const MAX_TERMINAL_ROWS: u16 = 4096;
const MAX_TERMINAL_CELLS: usize = 12 * 1024;

/// Terminal-independent input sent by an attached client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputEvent {
    /// A normalized keyboard event.
    Key {
        /// Key identity independent of the terminal backend.
        code: KeyCode,
        /// Active modifiers; order has no semantic meaning.
        modifiers: Vec<KeyModifier>,
    },
    /// A complete paste payload.
    Paste {
        /// UTF-8 text to insert.
        text: String,
    },
    /// One frame of a paste split across protocol messages.
    PasteChunk {
        /// UTF-8 fragment to append to the pending paste.
        text: String,
        /// Whether this fragment completes and applies the paste.
        final_chunk: bool,
    },
    /// A terminal mouse event represented by Crossterm's portable DTO.
    Mouse {
        /// Mouse kind, location, and modifiers.
        event: crossterm::event::MouseEvent,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Terminal-independent key identity used by [`InputEvent::Key`].
pub enum KeyCode {
    /// Unicode character input.
    Character(char),
    /// Enter or Return.
    Enter,
    /// Delete the preceding character.
    Backspace,
    /// Escape.
    Escape,
    /// Forward tab.
    Tab,
    /// Reverse tab.
    BackTab,
    /// Numbered function key.
    Function(u8),
    /// Delete the character at the cursor.
    Delete,
    /// Move left.
    Left,
    /// Move right.
    Right,
    /// Move up.
    Up,
    /// Move down.
    Down,
    /// Move to the beginning of a line or region.
    Home,
    /// Move to the end of a line or region.
    End,
    /// Move one viewport page upward.
    PageUp,
    /// Move one viewport page downward.
    PageDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Keyboard modifier transmitted with a normalized key event.
pub enum KeyModifier {
    /// Control modifier.
    Control,
    /// Alt or Option modifier.
    Alt,
    /// Shift modifier.
    Shift,
}

/// Client-to-owner protocol messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Authenticate and attach a rendering client.
    Connect {
        /// Protocol version understood by the client.
        protocol_version: u32,
        /// Secret read from the owner-created token file.
        #[serde(default)]
        reconnect_token: String,
        /// Last rendered owner revision, used to avoid redundant line patches.
        last_revision: Option<u64>,
        /// Client viewport width.
        #[serde(default = "default_columns")]
        columns: u16,
        /// Client viewport height.
        #[serde(default = "default_rows")]
        rows: u16,
        /// Whether this client currently owns interactive focus.
        #[serde(default = "default_focused")]
        focused: bool,
    },
    /// Authenticate a control-only request to stop the owner process.
    StopControl {
        /// Protocol version understood by the controller.
        protocol_version: u32,
        /// Secret read from the owner-created token file.
        reconnect_token: String,
    },
    /// Apply one ordered input event.
    Input {
        /// Client sequence used to correlate the returned render.
        sequence: u64,
        /// Normalized input payload.
        event: InputEvent,
    },
    /// Update the client's viewport dimensions.
    Resize {
        /// Viewport width.
        columns: u16,
        /// Viewport height.
        rows: u16,
    },
    /// Update whether this attachment owns interactive focus.
    Focus {
        /// New focus state.
        focused: bool,
    },
    /// Renew the client's owner lease without changing editor state.
    Heartbeat,
    /// Close this attachment while leaving the owner alive.
    Detach,
    /// Stop the owner after authenticating through an attached connection.
    Stop,
}

const fn default_columns() -> u16 {
    80
}

const fn default_rows() -> u16 {
    24
}

const fn default_focused() -> bool {
    true
}

/// A complete logical line replacement in the next client frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinePatch {
    /// Zero-based screen row replaced by this patch.
    pub row: usize,
    /// Plain-text fallback for clients that do not consume styled spans.
    pub text: String,
    /// Run-length encoded cells. `text` remains as a plain fallback for older clients.
    #[serde(default)]
    pub spans: Vec<StyledSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyledSpan {
    /// Text cells sharing the accompanying style.
    pub text: String,
    /// Resolved display style for this run.
    pub style: crate::theme::Style,
}

/// Minimal render delta returned by the headless owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderDelta {
    /// Monotonic owner render revision represented by this frame.
    pub revision: u64,
    /// Complete replacements for changed logical screen rows.
    pub lines: Vec<LinePatch>,
    /// Cursor as `(character column, zero-based row)`.
    pub cursor: (usize, usize),
}

/// Owner-to-client protocol messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Successful handshake and initial render state.
    Connected {
        /// Protocol version spoken by the owner.
        protocol_version: u32,
        /// Initial full or revision-aware render.
        render: RenderDelta,
    },
    /// Render response correlated with an input sequence.
    Render {
        /// Sequence from the triggering [`ClientMessage::Input`].
        sequence: u64,
        /// Resulting render state.
        delta: RenderDelta,
    },
    /// Confirmation that the connection detached cleanly.
    Detached,
    /// Confirmation that the owner accepted a stop request.
    Stopped,
    /// Protocol or editor error safe to report to the client.
    Error {
        /// Human-readable failure detail.
        message: String,
    },
}

/// Small persistent owner used to validate detach mechanics.
#[derive(Debug, Clone)]
pub struct HeadlessOwner {
    lines: Vec<String>,
    cursor: (usize, usize),
    revision: u64,
    pending_paste: String,
}

impl HeadlessOwner {
    /// Creates a minimal owner initialized with `text` and a cursor at origin.
    #[must_use]
    pub fn new(text: &str) -> Self {
        let mut lines = text.split('\n').map(str::to_string).collect::<Vec<_>>();
        if lines.is_empty() {
            lines.push(String::new());
        }
        Self {
            lines,
            cursor: (0, 0),
            revision: 0,
            pending_paste: String::new(),
        }
    }

    #[must_use]
    /// Returns the current render revision.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn snapshot(&self, last_revision: Option<u64>) -> RenderDelta {
        let lines = if last_revision == Some(self.revision) {
            Vec::new()
        } else {
            self.lines
                .iter()
                .enumerate()
                .map(|(row, text)| LinePatch {
                    row,
                    text: text.clone(),
                    spans: vec![StyledSpan {
                        text: text.clone(),
                        style: crate::theme::Style::default(),
                    }],
                })
                .collect()
        };
        RenderDelta {
            revision: self.revision,
            lines,
            cursor: self.cursor,
        }
    }

    fn apply(&mut self, event: InputEvent) -> anyhow::Result<RenderDelta> {
        let intermediate_paste = matches!(
            &event,
            InputEvent::PasteChunk {
                final_chunk: false,
                ..
            }
        );
        let changed_row = match event {
            InputEvent::Key { code, modifiers } => {
                self.pending_paste.clear();
                anyhow::ensure!(
                    modifiers.is_empty(),
                    "detach spike only accepts unmodified editing keys"
                );
                self.apply_key(code)?
            }
            InputEvent::Paste { text } => {
                self.pending_paste.clear();
                self.apply_paste(&text)
            }
            InputEvent::PasteChunk { text, final_chunk } => {
                let next_size = self.pending_paste.len().saturating_add(text.len());
                if next_size > MAX_PENDING_PASTE_BYTES {
                    self.pending_paste.clear();
                    anyhow::bail!(
                        "detach paste exceeds {MAX_PENDING_PASTE_BYTES} bytes before completion"
                    );
                }
                self.pending_paste.push_str(&text);
                if final_chunk {
                    let text = std::mem::take(&mut self.pending_paste);
                    self.apply_paste(&text)
                } else {
                    Vec::new()
                }
            }
            InputEvent::Mouse { event } => {
                self.pending_paste.clear();
                let row = usize::from(event.row).min(self.lines.len().saturating_sub(1));
                let column = usize::from(event.column).min(self.lines[row].chars().count());
                self.cursor = (column, row);
                Vec::new()
            }
        };
        if !intermediate_paste {
            self.revision = self.revision.saturating_add(1);
        }
        Ok(RenderDelta {
            revision: self.revision,
            lines: changed_row
                .into_iter()
                .map(|row| LinePatch {
                    row,
                    text: self.lines[row].clone(),
                    spans: vec![StyledSpan {
                        text: self.lines[row].clone(),
                        style: crate::theme::Style::default(),
                    }],
                })
                .collect(),
            cursor: self.cursor,
        })
    }

    fn apply_key(&mut self, code: KeyCode) -> anyhow::Result<Vec<usize>> {
        match code {
            KeyCode::Character(character) => {
                let (column, row) = self.cursor;
                let byte = char_to_byte(&self.lines[row], column);
                self.lines[row].insert(byte, character);
                self.cursor.0 += 1;
                Ok(vec![row])
            }
            KeyCode::Enter => {
                let (column, row) = self.cursor;
                let byte = char_to_byte(&self.lines[row], column);
                let suffix = self.lines[row].split_off(byte);
                self.lines.insert(row + 1, suffix);
                self.cursor = (0, row + 1);
                Ok(vec![row, row + 1])
            }
            KeyCode::Backspace => {
                let (column, row) = self.cursor;
                anyhow::ensure!(column > 0, "backspace at line start is outside spike scope");
                let start = char_to_byte(&self.lines[row], column - 1);
                let end = char_to_byte(&self.lines[row], column);
                self.lines[row].replace_range(start..end, "");
                self.cursor.0 -= 1;
                Ok(vec![row])
            }
            KeyCode::Tab => self.apply_key(KeyCode::Character('\t')),
            KeyCode::Delete => {
                let (column, row) = self.cursor;
                let line_len = self.lines[row].chars().count();
                if column >= line_len {
                    return Ok(Vec::new());
                }
                let start = char_to_byte(&self.lines[row], column);
                let end = char_to_byte(&self.lines[row], column + 1);
                self.lines[row].replace_range(start..end, "");
                Ok(vec![row])
            }
            KeyCode::Left => {
                self.cursor.0 = self.cursor.0.saturating_sub(1);
                Ok(Vec::new())
            }
            KeyCode::Right => {
                self.cursor.0 = (self.cursor.0 + 1).min(self.lines[self.cursor.1].chars().count());
                Ok(Vec::new())
            }
            KeyCode::Up => {
                self.cursor.1 = self.cursor.1.saturating_sub(1);
                self.cursor.0 = self.cursor.0.min(self.lines[self.cursor.1].chars().count());
                Ok(Vec::new())
            }
            KeyCode::Down => {
                self.cursor.1 = (self.cursor.1 + 1).min(self.lines.len().saturating_sub(1));
                self.cursor.0 = self.cursor.0.min(self.lines[self.cursor.1].chars().count());
                Ok(Vec::new())
            }
            KeyCode::Home => {
                self.cursor.0 = 0;
                Ok(Vec::new())
            }
            KeyCode::End => {
                self.cursor.0 = self.lines[self.cursor.1].chars().count();
                Ok(Vec::new())
            }
            KeyCode::Escape
            | KeyCode::BackTab
            | KeyCode::Function(_)
            | KeyCode::PageUp
            | KeyCode::PageDown => Ok(Vec::new()),
        }
    }

    fn apply_paste(&mut self, text: &str) -> Vec<usize> {
        let (column, row) = self.cursor;
        let byte = char_to_byte(&self.lines[row], column);
        self.lines[row].insert_str(byte, text);
        self.cursor.0 += text.chars().count();
        vec![row]
    }
}

fn char_to_byte(text: &str, character_index: usize) -> usize {
    text.char_indices()
        .nth(character_index)
        .map_or(text.len(), |(byte, _)| byte)
}

#[derive(Debug, Clone)]
/// Filesystem rendezvous points for one named detachable session.
pub struct SessionPaths {
    /// Unix-domain socket used for local IPC.
    pub socket: PathBuf,
    /// Private reconnect token used to authenticate clients.
    pub token: PathBuf,
    /// Owner process identifier used to reject stale or spoofed sockets.
    pub pid: PathBuf,
}

impl SessionPaths {
    /// Derives safe session paths beneath `directory`.
    ///
    /// `name` must be a single component containing only ASCII letters,
    /// numbers, dash, underscore, or dot.
    pub fn new(directory: &Path, name: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !name.is_empty()
                && name
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character)),
            "detach session names may contain only letters, numbers, dash, underscore, and dot"
        );
        Ok(Self {
            socket: directory.join(format!("{name}.sock")),
            token: directory.join(format!("{name}.token")),
            pid: directory.join(format!("{name}.pid")),
        })
    }
}

#[cfg(unix)]
/// Bound owner endpoint and its private authentication material.
///
/// Dropping this value removes all rendezvous files.
pub struct BoundSession {
    listener: UnixListener,
    paths: SessionPaths,
    token: String,
}

#[cfg(unix)]
impl BoundSession {
    /// Returns the reconnect token required by clients.
    pub fn token(&self) -> &str {
        &self.token
    }
}

#[cfg(unix)]
impl Drop for BoundSession {
    fn drop(&mut self) {
        _ = std::fs::remove_file(&self.paths.socket);
        _ = std::fs::remove_file(&self.paths.token);
        _ = std::fs::remove_file(&self.paths.pid);
    }
}

#[cfg(unix)]
/// Creates the authenticated IPC endpoint for a named session.
///
/// The directory, socket, token, and PID file receive owner-only
/// permissions. Existing rendezvous files are removed only after verifying
/// that they do not identify a live owner.
pub fn bind_session(directory: &Path, name: &str) -> anyhow::Result<BoundSession> {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::create_dir_all(directory)?;
    anyhow::ensure!(
        !std::fs::symlink_metadata(directory)?
            .file_type()
            .is_symlink(),
        "detach session directory must not be a symlink"
    );
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    let paths = SessionPaths::new(directory, name)?;
    if std::fs::symlink_metadata(&paths.socket).is_ok() {
        let active = session_is_active(directory, name)?;
        anyhow::ensure!(!active, "detach session `{name}` is already running");
        _ = std::fs::remove_file(&paths.socket);
    }
    if std::fs::symlink_metadata(&paths.token).is_ok() {
        std::fs::remove_file(&paths.token)?;
    }
    if std::fs::symlink_metadata(&paths.pid).is_ok() {
        std::fs::remove_file(&paths.pid)?;
    }
    let listener = UnixListener::bind(&paths.socket)?;
    std::fs::set_permissions(&paths.socket, std::fs::Permissions::from_mode(0o600))?;
    let token = uuid::Uuid::new_v4().to_string();
    write_private_file(&paths.token, token.as_bytes())?;
    write_private_file(&paths.pid, std::process::id().to_string().as_bytes())?;
    Ok(BoundSession {
        listener,
        paths,
        token,
    })
}

#[cfg(unix)]
/// Checks whether a named session has a live, matching socket owner.
///
/// A PID file alone is insufficient: the socket must connect and its peer PID
/// must match the recorded owner.
pub fn session_is_active(directory: &Path, name: &str) -> anyhow::Result<bool> {
    use std::os::unix::fs::FileTypeExt as _;

    let paths = SessionPaths::new(directory, name)?;
    let socket_is_valid = std::fs::symlink_metadata(&paths.socket)
        .is_ok_and(|metadata| metadata.file_type().is_socket());
    let token_is_valid = std::fs::symlink_metadata(&paths.token)
        .is_ok_and(|metadata| metadata.file_type().is_file());
    let owner_pid = std::fs::read_to_string(&paths.pid)
        .ok()
        .and_then(|pid| pid.trim().parse::<i32>().ok())
        .filter(|pid| process_is_alive(*pid));
    let Some(owner_pid) = owner_pid.filter(|_| socket_is_valid && token_is_valid) else {
        return Ok(false);
    };
    let Ok(socket) = std::os::unix::net::UnixStream::connect(&paths.socket) else {
        return Ok(false);
    };
    Ok(socket_peer_pid(&socket).is_ok_and(|peer_pid| peer_pid == owner_pid))
}

#[cfg(unix)]
fn write_private_file(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    use std::{io::Write as _, os::unix::fs::OpenOptionsExt as _};

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn process_is_alive(pid: i32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), /*signal*/ None).is_ok()
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn socket_peer_pid(socket: &std::os::unix::net::UnixStream) -> std::io::Result<i32> {
    use std::{mem, os::fd::AsRawFd as _};

    let mut credentials = nix::libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = mem::size_of::<nix::libc::ucred>() as nix::libc::socklen_t;
    // SAFETY: the socket is connected and the output pointer and length describe a valid ucred.
    let result = unsafe {
        nix::libc::getsockopt(
            socket.as_raw_fd(),
            nix::libc::SOL_SOCKET,
            nix::libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(credentials).cast(),
            std::ptr::addr_of_mut!(length),
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if length as usize != mem::size_of::<nix::libc::ucred>() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "detach socket returned an invalid peer credential",
        ));
    }
    Ok(credentials.pid)
}

#[cfg(target_os = "macos")]
fn socket_peer_pid(socket: &std::os::unix::net::UnixStream) -> std::io::Result<i32> {
    use std::{mem, os::fd::AsRawFd as _};

    let mut pid: nix::libc::pid_t = 0;
    let mut length = mem::size_of::<nix::libc::pid_t>() as nix::libc::socklen_t;
    // SAFETY: the socket is connected and the output pointer and length describe a valid pid_t.
    let result = unsafe {
        nix::libc::getsockopt(
            socket.as_raw_fd(),
            nix::libc::SOL_LOCAL,
            nix::libc::LOCAL_PEEREPID,
            std::ptr::addr_of_mut!(pid).cast(),
            std::ptr::addr_of_mut!(length),
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if length as usize != mem::size_of::<nix::libc::pid_t>() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "detach socket returned an invalid peer credential",
        ));
    }
    Ok(pid)
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android", target_os = "macos"))
))]
fn socket_peer_pid(_socket: &std::os::unix::net::UnixStream) -> std::io::Result<i32> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "detach socket peer identity is unavailable on this platform",
    ))
}

#[cfg(unix)]
/// Runs the detachable editor owner until an authenticated client stops it.
///
/// Only one interactive client may be attached at a time. Background editor
/// work continues while detached, and pending paste state is cleared whenever
/// an attachment ends.
pub async fn serve_editor_session(
    session: &BoundSession,
    core: crate::editor::DetachedEditorCore,
) -> anyhow::Result<()> {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let _perf_session = crate::editor::perf::PerfSession::start();
            let core = Rc::new(tokio::sync::Mutex::new(core));
            let attached = Rc::new(Cell::new(false));
            let (stop_sender, mut stop_receiver) = tokio::sync::mpsc::unbounded_channel();
            let mut background_interval = tokio::time::interval(Duration::from_millis(10));
            background_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    Some(()) = stop_receiver.recv() => {
                        core.lock().await.shutdown().await;
                        return Ok(());
                    }
                    accepted = session.listener.accept() => {
                        let (stream, _) = accepted?;
                        if attached.get() {
                            tokio::task::spawn_local(reject_busy_connection(
                                stream,
                                session.token().to_string(),
                                stop_sender.clone(),
                            ));
                            continue;
                        }
                        attached.set(true);
                        let core = Rc::clone(&core);
                        let attached = Rc::clone(&attached);
                        let token = session.token().to_string();
                        let stop_sender = stop_sender.clone();
                        tokio::task::spawn_local(async move {
                            let stop = serve_editor_connection(stream, &token, Rc::clone(&core))
                                .await
                                .unwrap_or(false);
                            core.lock().await.clear_pending_paste();
                            attached.set(false);
                            if stop {
                                _ = stop_sender.send(());
                            }
                        });
                    }
                    _ = background_interval.tick() => {
                        let mut core = core.lock().await;
                        core.tick().await?;
                        if core.stopped() {
                            core.shutdown().await;
                            return Ok(());
                        }
                    }
                }
            }
        })
        .await
}

#[cfg(unix)]
async fn serve_editor_connection(
    stream: UnixStream,
    expected_token: &str,
    core: Rc<tokio::sync::Mutex<crate::editor::DetachedEditorCore>>,
) -> anyhow::Result<bool> {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    let handshake = tokio::time::timeout(
        CLIENT_HANDSHAKE_TIMEOUT,
        read_frame::<_, ClientMessage>(&mut reader),
    )
    .await
    .map_err(|_| anyhow::anyhow!("detach IPC connect handshake timed out"))??;
    if let Some(ClientMessage::StopControl {
        protocol_version,
        reconnect_token,
    }) = &handshake
    {
        if *protocol_version == IPC_PROTOCOL_VERSION && reconnect_token == expected_token {
            write_frame(&mut writer, &ServerMessage::Stopped).await?;
            return Ok(true);
        }
        write_frame(
            &mut writer,
            &ServerMessage::Error {
                message: "detach protocol version or reconnect token mismatch".to_string(),
            },
        )
        .await?;
        return Ok(false);
    }
    let Some(ClientMessage::Connect {
        protocol_version,
        reconnect_token,
        last_revision,
        columns,
        rows,
        focused,
    }) = handshake
    else {
        write_frame(
            &mut writer,
            &ServerMessage::Error {
                message: "first detach message must be a connect handshake".to_string(),
            },
        )
        .await?;
        return Ok(false);
    };
    if protocol_version != IPC_PROTOCOL_VERSION || reconnect_token != expected_token {
        write_frame(
            &mut writer,
            &ServerMessage::Error {
                message: "detach protocol version or reconnect token mismatch".to_string(),
            },
        )
        .await?;
        return Ok(false);
    }
    validate_terminal_size(columns, rows)?;
    {
        let mut core = core.lock().await;
        core.clear_pending_paste();
        core.resize(columns, rows).await?;
        core.focus(focused).await?;
    }
    let render = core.lock().await.snapshot(last_revision);
    let mut client_revision = render.revision;
    write_frame(
        &mut writer,
        &ServerMessage::Connected {
            protocol_version: IPC_PROTOCOL_VERSION,
            render,
        },
    )
    .await?;

    loop {
        let message = tokio::time::timeout(CLIENT_HEARTBEAT_LEASE, read_frame(&mut reader)).await;
        let Ok(message) = message else {
            return Ok(false);
        };
        let Some(message) = message? else {
            return Ok(false);
        };
        match message {
            ClientMessage::Input { sequence, event } => {
                let mut core = core.lock().await;
                let delta = core.input(event).await?;
                client_revision = delta.revision;
                let stopped = core.stopped();
                drop(core);
                write_frame(&mut writer, &ServerMessage::Render { sequence, delta }).await?;
                if stopped {
                    return Ok(true);
                }
            }
            ClientMessage::Resize { columns, rows } => {
                validate_terminal_size(columns, rows)?;
                let delta = core.lock().await.resize(columns, rows).await?;
                client_revision = delta.revision;
                write_frame(
                    &mut writer,
                    &ServerMessage::Render {
                        sequence: /*sequence*/ 0,
                        delta,
                    },
                )
                .await?;
            }
            ClientMessage::Focus { focused } => {
                let delta = core.lock().await.focus(focused).await?;
                client_revision = delta.revision;
                write_frame(
                    &mut writer,
                    &ServerMessage::Render {
                        sequence: /*sequence*/ 0,
                        delta,
                    },
                )
                .await?;
            }
            ClientMessage::Heartbeat => {
                let delta = core.lock().await.snapshot(Some(client_revision));
                client_revision = delta.revision;
                write_frame(
                    &mut writer,
                    &ServerMessage::Render {
                        sequence: /*sequence*/ 0,
                        delta,
                    },
                )
                .await?;
            }
            ClientMessage::Detach => {
                write_frame(&mut writer, &ServerMessage::Detached).await?;
                return Ok(false);
            }
            ClientMessage::Stop => {
                write_frame(&mut writer, &ServerMessage::Stopped).await?;
                return Ok(true);
            }
            ClientMessage::Connect { .. } => {
                write_frame(
                    &mut writer,
                    &ServerMessage::Error {
                        message: "connection is already initialized".to_string(),
                    },
                )
                .await?;
            }
            ClientMessage::StopControl { .. } => {
                write_frame(
                    &mut writer,
                    &ServerMessage::Error {
                        message: "connection is already initialized".to_string(),
                    },
                )
                .await?;
            }
        }
    }
}

#[cfg(unix)]
async fn reject_busy_connection(
    stream: UnixStream,
    expected_token: String,
    stop_sender: tokio::sync::mpsc::UnboundedSender<()>,
) {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    let message = tokio::time::timeout(
        Duration::from_secs(1),
        read_frame::<_, ClientMessage>(&mut reader),
    )
    .await;
    if let Ok(Ok(Some(ClientMessage::StopControl {
        protocol_version,
        reconnect_token,
    }))) = message
    {
        if protocol_version == IPC_PROTOCOL_VERSION && reconnect_token == expected_token {
            _ = write_frame(&mut writer, &ServerMessage::Stopped).await;
            _ = stop_sender.send(());
            return;
        }
    }
    _ = write_frame(
        &mut writer,
        &ServerMessage::Error {
            message: "detach session already has an attached client".to_string(),
        },
    )
    .await;
}

#[cfg(unix)]
/// Connects an interactive client to a named detachable session.
///
/// The reconnect token is read from the private rendezvous file and presented
/// during the protocol handshake.
pub async fn connect_session(
    directory: &Path,
    name: &str,
    last_revision: Option<u64>,
    size: (u16, u16),
) -> anyhow::Result<HeadlessClient<UnixStream>> {
    let paths = SessionPaths::new(directory, name)?;
    let token = std::fs::read_to_string(&paths.token)?;
    let stream = UnixStream::connect(&paths.socket).await?;
    HeadlessClient::connect_session(stream, token.trim(), last_revision, size, true).await
}

#[cfg(unix)]
/// Authenticates a control connection and asks a named owner to shut down.
pub async fn stop_session(directory: &Path, name: &str) -> anyhow::Result<()> {
    let paths = SessionPaths::new(directory, name)?;
    let token = std::fs::read_to_string(&paths.token)?;
    let stream = UnixStream::connect(&paths.socket).await?;
    let (reader, mut writer) = tokio::io::split(stream);
    write_frame(
        &mut writer,
        &ClientMessage::StopControl {
            protocol_version: IPC_PROTOCOL_VERSION,
            reconnect_token: token.trim().to_string(),
        },
    )
    .await?;
    let mut reader = BufReader::new(reader);
    match read_frame_with_timeout(
        &mut reader,
        CLIENT_HANDSHAKE_TIMEOUT,
        "detach IPC stop response timed out",
    )
    .await?
    {
        Some(ServerMessage::Stopped) => Ok(()),
        Some(ServerMessage::Error { message }) => anyhow::bail!(message),
        other => anyhow::bail!("unexpected stop response: {other:?}"),
    }
}

/// Serve one attached client over any ordered local byte stream.
///
/// # Errors
///
/// Returns an error for invalid framing, version mismatch, disconnect, or invalid input.
pub async fn serve_connection<S>(stream: S, owner: Arc<Mutex<HeadlessOwner>>) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    let Some(ClientMessage::Connect {
        protocol_version,
        last_revision,
        columns,
        rows,
        ..
    }) = read_frame_with_timeout(
        &mut reader,
        CLIENT_HANDSHAKE_TIMEOUT,
        "detach IPC connect handshake timed out",
    )
    .await?
    else {
        anyhow::bail!("first detach message must be a connect handshake");
    };
    anyhow::ensure!(
        protocol_version == IPC_PROTOCOL_VERSION,
        "unsupported detach protocol version {protocol_version}"
    );
    validate_terminal_size(columns, rows)?;
    let render = owner.lock().await.snapshot(last_revision);
    let mut client_revision = render.revision;
    write_frame(
        &mut writer,
        &ServerMessage::Connected {
            protocol_version: IPC_PROTOCOL_VERSION,
            render,
        },
    )
    .await?;

    while let Some(message) = read_frame_with_timeout(
        &mut reader,
        CLIENT_HEARTBEAT_LEASE,
        "detach IPC client heartbeat timed out",
    )
    .await?
    {
        match message {
            ClientMessage::Input { sequence, event } => {
                let delta = owner.lock().await.apply(event)?;
                client_revision = delta.revision;
                write_frame(&mut writer, &ServerMessage::Render { sequence, delta }).await?;
            }
            ClientMessage::Connect { .. } => {
                write_frame(
                    &mut writer,
                    &ServerMessage::Error {
                        message: "connection is already initialized".to_string(),
                    },
                )
                .await?;
            }
            ClientMessage::StopControl { .. } => {
                write_frame(
                    &mut writer,
                    &ServerMessage::Error {
                        message: "control messages are not supported by this owner".to_string(),
                    },
                )
                .await?;
            }
            ClientMessage::Resize { columns, rows } => {
                validate_terminal_size(columns, rows)?;
                let delta = owner.lock().await.snapshot(/*last_revision*/ None);
                client_revision = delta.revision;
                write_frame(
                    &mut writer,
                    &ServerMessage::Render {
                        sequence: /*sequence*/ 0,
                        delta,
                    },
                )
                .await?;
            }
            ClientMessage::Focus { .. } => {
                let delta = owner.lock().await.snapshot(/*last_revision*/ None);
                client_revision = delta.revision;
                write_frame(
                    &mut writer,
                    &ServerMessage::Render {
                        sequence: /*sequence*/ 0,
                        delta,
                    },
                )
                .await?;
            }
            ClientMessage::Heartbeat => {
                let delta = owner.lock().await.snapshot(Some(client_revision));
                client_revision = delta.revision;
                write_frame(
                    &mut writer,
                    &ServerMessage::Render {
                        sequence: /*sequence*/ 0,
                        delta,
                    },
                )
                .await?;
            }
            ClientMessage::Detach => {
                write_frame(&mut writer, &ServerMessage::Detached).await?;
                return Ok(());
            }
            ClientMessage::Stop => {
                write_frame(&mut writer, &ServerMessage::Stopped).await?;
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Request/response client for a single attached terminal.
pub struct HeadlessClient<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    reader: BufReader<ReadHalf<S>>,
    writer: WriteHalf<S>,
    next_sequence: u64,
    /// Render returned by the successful connection handshake.
    pub initial_render: RenderDelta,
}

impl<S> HeadlessClient<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Connect and negotiate the exact IPC protocol version.
    ///
    /// # Errors
    ///
    /// Returns an error for transport failure, protocol mismatch, or an invalid reply.
    pub async fn connect(stream: S, last_revision: Option<u64>) -> anyhow::Result<Self> {
        Self::connect_session(stream, "", last_revision, (80, 24), true).await
    }

    /// Connects with explicit authentication, viewport, and focus state.
    ///
    /// `last_revision` lets a reconnecting client request an empty initial
    /// line set when it already holds the owner's current render.
    pub async fn connect_session(
        stream: S,
        reconnect_token: &str,
        last_revision: Option<u64>,
        size: (u16, u16),
        focused: bool,
    ) -> anyhow::Result<Self> {
        validate_terminal_size(size.0, size.1)?;
        let (reader, mut writer) = tokio::io::split(stream);
        write_frame(
            &mut writer,
            &ClientMessage::Connect {
                protocol_version: IPC_PROTOCOL_VERSION,
                reconnect_token: reconnect_token.to_string(),
                last_revision,
                columns: size.0,
                rows: size.1,
                focused,
            },
        )
        .await?;
        let mut reader = BufReader::new(reader);
        let (protocol_version, render) = match read_frame_with_timeout(
            &mut reader,
            CLIENT_HANDSHAKE_TIMEOUT,
            "detach IPC connect response timed out",
        )
        .await?
        {
            Some(ServerMessage::Connected {
                protocol_version,
                render,
            }) => (protocol_version, render),
            Some(ServerMessage::Error { message }) => anyhow::bail!(message),
            _ => anyhow::bail!("detach owner did not return a connect response"),
        };
        anyhow::ensure!(
            protocol_version == IPC_PROTOCOL_VERSION,
            "detach owner selected unsupported protocol version {protocol_version}"
        );
        Ok(Self {
            reader,
            writer,
            next_sequence: 1,
            initial_render: render,
        })
    }

    /// Submit one normalized event and wait for its correlated render delta.
    ///
    /// # Errors
    ///
    /// Returns an error for transport failure or an invalid/out-of-order reply.
    pub async fn input(&mut self, event: InputEvent) -> anyhow::Result<RenderDelta> {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        write_frame(&mut self.writer, &ClientMessage::Input { sequence, event }).await?;
        match read_frame_with_timeout(
            &mut self.reader,
            CLIENT_RESPONSE_TIMEOUT,
            "detach IPC input response timed out",
        )
        .await?
        {
            Some(ServerMessage::Render {
                sequence: response_sequence,
                delta,
            }) if response_sequence == sequence => Ok(delta),
            Some(ServerMessage::Error { message }) => anyhow::bail!(message),
            response => anyhow::bail!("unexpected detach response: {response:?}"),
        }
    }

    /// Updates the remote viewport size and returns the resulting render.
    pub async fn resize(&mut self, columns: u16, rows: u16) -> anyhow::Result<RenderDelta> {
        validate_terminal_size(columns, rows)?;
        write_frame(&mut self.writer, &ClientMessage::Resize { columns, rows }).await?;
        self.expect_control_render().await
    }

    /// Updates interactive focus and returns the resulting render.
    pub async fn focus(&mut self, focused: bool) -> anyhow::Result<RenderDelta> {
        write_frame(&mut self.writer, &ClientMessage::Focus { focused }).await?;
        self.expect_control_render().await
    }

    /// Renews the owner lease and obtains any render newer than the client state.
    pub async fn heartbeat(&mut self) -> anyhow::Result<RenderDelta> {
        write_frame(&mut self.writer, &ClientMessage::Heartbeat).await?;
        self.expect_control_render().await
    }

    /// Closes this attachment while leaving the remote editor owner running.
    pub async fn detach(&mut self) -> anyhow::Result<()> {
        write_frame(&mut self.writer, &ClientMessage::Detach).await?;
        match read_frame_with_timeout(
            &mut self.reader,
            CLIENT_HANDSHAKE_TIMEOUT,
            "detach IPC detach response timed out",
        )
        .await?
        {
            Some(ServerMessage::Detached) => Ok(()),
            other => anyhow::bail!("unexpected detach response: {other:?}"),
        }
    }

    /// Requests shutdown through this authenticated attachment.
    pub async fn stop(&mut self) -> anyhow::Result<()> {
        write_frame(&mut self.writer, &ClientMessage::Stop).await?;
        match read_frame_with_timeout(
            &mut self.reader,
            CLIENT_HANDSHAKE_TIMEOUT,
            "detach IPC stop response timed out",
        )
        .await?
        {
            Some(ServerMessage::Stopped) => Ok(()),
            other => anyhow::bail!("unexpected stop response: {other:?}"),
        }
    }

    async fn expect_control_render(&mut self) -> anyhow::Result<RenderDelta> {
        match read_frame_with_timeout(
            &mut self.reader,
            CLIENT_RESPONSE_TIMEOUT,
            "detach IPC control response timed out",
        )
        .await?
        {
            Some(ServerMessage::Render { sequence: 0, delta }) => Ok(delta),
            Some(ServerMessage::Error { message }) => anyhow::bail!(message),
            other => anyhow::bail!("unexpected control response: {other:?}"),
        }
    }
}

async fn read_frame<R, T>(reader: &mut R) -> anyhow::Result<Option<T>>
where
    R: AsyncBufRead + Unpin,
    T: DeserializeOwned,
{
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            anyhow::ensure!(bytes.is_empty(), "detach IPC frame ended before newline");
            return Ok(None);
        }
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        anyhow::ensure!(
            bytes.len().saturating_add(consumed) <= MAX_FRAME_BYTES,
            "detach IPC frame is too large"
        );
        let complete = available[consumed - 1] == b'\n';
        bytes.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if complete {
            break;
        }
    }
    Ok(Some(serde_json::from_slice(&bytes)?))
}

async fn read_frame_with_timeout<R, T>(
    reader: &mut R,
    duration: Duration,
    message: &'static str,
) -> anyhow::Result<Option<T>>
where
    R: AsyncBufRead + Unpin,
    T: DeserializeOwned,
{
    tokio::time::timeout(duration, read_frame(reader))
        .await
        .map_err(|_| anyhow::anyhow!(message))?
}

fn validate_terminal_size(columns: u16, rows: u16) -> anyhow::Result<()> {
    anyhow::ensure!(
        columns > 0 && rows > 0,
        "detach terminal size must be non-zero"
    );
    anyhow::ensure!(
        columns <= MAX_TERMINAL_COLUMNS
            && rows <= MAX_TERMINAL_ROWS
            && usize::from(columns) * usize::from(rows) <= MAX_TERMINAL_CELLS,
        "detach terminal size is too large"
    );
    Ok(())
}

async fn write_frame<W, T>(writer: &mut W, value: &T) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let bytes = serde_json::to_vec(value)?;
    anyhow::ensure!(
        bytes.len() < MAX_FRAME_BYTES,
        "detach IPC frame is too large"
    );
    tokio::time::timeout(CLIENT_WRITE_TIMEOUT, async {
        writer.write_all(&bytes).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await
    })
    .await
    .map_err(|_| anyhow::anyhow!("detach IPC client write timed out"))??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_oversized_frames_before_the_delimiter_arrives() {
        let bytes = vec![b'x'; MAX_FRAME_BYTES + 1];
        let mut reader = BufReader::with_capacity(64, bytes.as_slice());

        let error = read_frame::<_, serde_json::Value>(&mut reader)
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("detach IPC frame is too large"));
    }

    #[tokio::test]
    async fn rejects_unterminated_frames() {
        let bytes = br#"{"type":"heartbeat"}"#;
        let mut reader = BufReader::new(bytes.as_slice());

        let error = read_frame::<_, ClientMessage>(&mut reader)
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("detach IPC frame ended before newline"));
    }

    #[tokio::test]
    async fn silent_peers_do_not_hold_client_reads_forever() {
        let (_writer, reader) = tokio::io::duplex(64);
        let mut reader = BufReader::new(reader);

        let error = read_frame_with_timeout::<_, ServerMessage>(
            &mut reader,
            Duration::from_millis(20),
            "test response timed out",
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(error.contains("test response timed out"));
    }

    #[test]
    fn validates_terminal_dimensions_and_total_cell_count() {
        assert!(validate_terminal_size(80, 24).is_ok());
        assert!(validate_terminal_size(0, 24).is_err());
        assert!(validate_terminal_size(80, 0).is_err());
        assert!(validate_terminal_size(MAX_TERMINAL_COLUMNS + 1, 1).is_err());
        assert!(validate_terminal_size(2048, 1024).is_err());
    }

    #[test]
    fn maximum_terminal_frame_fits_the_advertised_frame_budget() {
        use crate::{color::Color, theme::Style};

        let style = Style {
            fg: Some(Color::Rgba {
                r: 255,
                g: 255,
                b: 255,
                a: 255,
            }),
            bg: Some(Color::Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            }),
            bold: true,
            italic: true,
        };
        let row_width = usize::from(MAX_TERMINAL_COLUMNS);
        let rows = MAX_TERMINAL_CELLS / row_width;
        let message = ServerMessage::Connected {
            protocol_version: IPC_PROTOCOL_VERSION,
            render: RenderDelta {
                revision: 1,
                lines: (0..rows)
                    .map(|row| LinePatch {
                        row,
                        text: "\u{1}".repeat(row_width),
                        spans: (0..row_width)
                            .map(|column| StyledSpan {
                                text: "\u{1}".to_string(),
                                style: Style {
                                    bold: column % 2 == 0,
                                    ..style.clone()
                                },
                            })
                            .collect(),
                    })
                    .collect(),
                cursor: (0, 0),
            },
        };

        assert_eq!(row_width * rows, MAX_TERMINAL_CELLS);
        assert!(serde_json::to_vec(&message).unwrap().len() < MAX_FRAME_BYTES);
    }

    #[test]
    fn paste_chunks_apply_once_when_the_final_chunk_arrives() {
        let mut owner = HeadlessOwner::new("end");

        let first = owner
            .apply(InputEvent::PasteChunk {
                text: "first ".to_string(),
                final_chunk: false,
            })
            .unwrap();
        let second = owner
            .apply(InputEvent::PasteChunk {
                text: "second ".to_string(),
                final_chunk: true,
            })
            .unwrap();

        assert_eq!(first.revision, 0);
        assert!(first.lines.is_empty());
        assert_eq!(second.revision, 1);
        assert_eq!(second.lines[0].text, "first second end");
    }

    #[test]
    fn oversized_pending_paste_is_rejected_and_cleared() {
        let mut owner = HeadlessOwner::new("end");
        owner.pending_paste = "x".repeat(MAX_PENDING_PASTE_BYTES);

        let error = owner
            .apply(InputEvent::PasteChunk {
                text: "x".to_string(),
                final_chunk: false,
            })
            .unwrap_err()
            .to_string();

        assert!(error.contains("detach paste exceeds"));
        assert!(owner.pending_paste.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stale_private_files_are_recreated_without_following_symlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt as _};

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("outside-token");
        std::fs::write(&target, "leave-me-alone").unwrap();
        let paths = SessionPaths::new(directory.path(), "stale").unwrap();
        symlink(&target, &paths.token).unwrap();
        std::fs::write(&paths.pid, "not-a-pid").unwrap();
        std::fs::set_permissions(&paths.pid, std::fs::Permissions::from_mode(0o666)).unwrap();

        let session = bind_session(directory.path(), "stale").unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "leave-me-alone");
        assert!(!std::fs::symlink_metadata(&session.paths.token)
            .unwrap()
            .file_type()
            .is_symlink());
        for path in [&session.paths.token, &session.paths.pid] {
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_live_but_unrelated_pid_does_not_keep_a_stale_detach_socket_active() {
        let directory = tempfile::tempdir().unwrap();
        let paths = SessionPaths::new(directory.path(), "stale-owner").unwrap();
        let stale_listener = std::os::unix::net::UnixListener::bind(&paths.socket).unwrap();
        let unrelated_pid = nix::unistd::getppid().as_raw();
        assert_ne!(unrelated_pid, std::process::id() as i32);
        std::fs::write(&paths.token, "stale-token").unwrap();
        std::fs::write(&paths.pid, unrelated_pid.to_string()).unwrap();

        assert!(!session_is_active(directory.path(), "stale-owner").unwrap());
        let replacement = bind_session(directory.path(), "stale-owner").unwrap();
        assert!(session_is_active(directory.path(), "stale-owner").unwrap());

        drop(replacement);
        drop(stale_listener);
    }

    #[tokio::test]
    async fn duplex_stream_accepts_input_and_returns_a_render_delta() {
        const DUPLEX_CAPACITY: usize = 4096;
        let owner = Arc::new(Mutex::new(HeadlessOwner::new("abc")));
        let (client_stream, server_stream) = tokio::io::duplex(DUPLEX_CAPACITY);
        let server_owner = Arc::clone(&owner);
        let server = tokio::spawn(async move {
            serve_connection(server_stream, server_owner).await.unwrap();
        });
        let mut client = HeadlessClient::connect(client_stream, /*last_revision*/ None)
            .await
            .unwrap();
        assert_eq!(client.initial_render.lines[0].text, "abc");

        let delta = client
            .input(InputEvent::Key {
                code: KeyCode::Character('λ'),
                modifiers: Vec::new(),
            })
            .await
            .unwrap();
        assert_eq!(delta.revision, 1);
        assert_eq!(
            delta.lines,
            [LinePatch {
                row: 0,
                text: "λabc".into(),
                spans: vec![StyledSpan {
                    text: "λabc".into(),
                    style: crate::theme::Style::default(),
                }],
            }]
        );
        assert_eq!(delta.cursor, (1, 0));

        drop(client);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn reconnect_at_current_revision_avoids_a_full_repaint() {
        const DUPLEX_CAPACITY: usize = 4096;
        const CURRENT_REVISION: u64 = 7;
        let owner = Arc::new(Mutex::new(HeadlessOwner::new("abc")));
        owner.lock().await.revision = CURRENT_REVISION;
        let (client_stream, server_stream) = tokio::io::duplex(DUPLEX_CAPACITY);
        let server_owner = Arc::clone(&owner);
        let server = tokio::spawn(async move {
            serve_connection(server_stream, server_owner).await.unwrap();
        });

        let client = HeadlessClient::connect(client_stream, Some(CURRENT_REVISION))
            .await
            .unwrap();
        assert!(client.initial_render.lines.is_empty());
        assert_eq!(client.initial_render.revision, CURRENT_REVISION);

        drop(client);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn heartbeat_returns_changes_produced_by_background_owner_work() {
        const DUPLEX_CAPACITY: usize = 4096;

        let owner = Arc::new(Mutex::new(HeadlessOwner::new("before")));
        let server_owner = Arc::clone(&owner);
        let (client_stream, server_stream) = tokio::io::duplex(DUPLEX_CAPACITY);
        let server = tokio::spawn(async move {
            serve_connection(server_stream, server_owner).await.unwrap();
        });
        let mut client = HeadlessClient::connect(client_stream, /*last_revision*/ None)
            .await
            .unwrap();
        owner
            .lock()
            .await
            .apply(InputEvent::Paste {
                text: "after ".to_string(),
            })
            .unwrap();

        let delta = client.heartbeat().await.unwrap();

        assert_eq!(delta.lines[0].text, "after before");
        drop(client);
        server.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_socket_proves_the_local_ipc_boundary() {
        use tokio::net::{UnixListener, UnixStream};

        let socket = std::env::temp_dir().join(format!("red-detach-{}.sock", uuid::Uuid::new_v4()));
        let listener = UnixListener::bind(&socket).unwrap();
        let owner = Arc::new(Mutex::new(HeadlessOwner::new("ready")));
        let server_owner = Arc::clone(&owner);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            serve_connection(stream, server_owner).await.unwrap();
        });

        let stream = UnixStream::connect(&socket).await.unwrap();
        let mut client = HeadlessClient::connect(stream, /*last_revision*/ None)
            .await
            .unwrap();
        let delta = client
            .input(InputEvent::Paste {
                text: "!".to_string(),
            })
            .await
            .unwrap();
        assert_eq!(delta.lines[0].text, "!ready");

        drop(client);
        server.await.unwrap();
        std::fs::remove_file(socket).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn production_editor_owner_survives_client_drop_and_reattach() {
        use std::os::unix::fs::PermissionsExt as _;

        use crate::{
            buffer::Buffer,
            config::Config,
            editor::{DetachedEditorCore, Editor, ACTION_DISPATCHER, PLUGIN_DISPATCHER_TEST_LOCK},
            lsp::LspManager,
            theme::Theme,
        };

        let _dispatcher_guard = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        while ACTION_DISPATCHER.try_recv_request().is_some() {}
        let config = Config::from_user_toml_with_overrides("", &[]).unwrap();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let editor = Editor::with_size(
            lsp,
            40,
            10,
            config,
            Theme::default(),
            vec![Buffer::new(None, "base\n".to_string())],
        )
        .unwrap();
        let core = DetachedEditorCore::new(editor).await.unwrap();
        let directory = tempfile::tempdir().unwrap();
        let session = bind_session(directory.path(), "work").unwrap();
        assert_eq!(
            std::fs::metadata(&session.paths.socket)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        let server = serve_editor_session(&session, core);
        let client = async {
            let mut first = connect_session(directory.path(), "work", None, (40, 10))
                .await
                .unwrap();
            let busy = match connect_session(directory.path(), "work", None, (40, 10)).await {
                Ok(_) => panic!("second client unexpectedly attached"),
                Err(error) => error,
            };
            assert!(busy.to_string().contains("already has an attached client"));
            first
                .input(InputEvent::Key {
                    code: KeyCode::Character('i'),
                    modifiers: Vec::new(),
                })
                .await
                .unwrap();
            first
                .input(InputEvent::Paste {
                    text: "kept ".to_string(),
                })
                .await
                .unwrap();
            first
                .input(InputEvent::PasteChunk {
                    text: "must-not-cross-clients ".to_string(),
                    final_chunk: false,
                })
                .await
                .unwrap();
            first
                .input(InputEvent::Key {
                    code: KeyCode::Escape,
                    modifiers: Vec::new(),
                })
                .await
                .unwrap();
            drop(first); // Simulate a terminal or SSH connection disappearing.
            tokio::time::sleep(Duration::from_millis(20)).await;

            let mut second = connect_session(directory.path(), "work", None, (40, 10))
                .await
                .unwrap();
            assert!(
                second
                    .initial_render
                    .lines
                    .iter()
                    .any(|line| line.text.contains("kept base")),
                "restored rows: {:?}",
                second.initial_render.lines
            );
            second
                .input(InputEvent::Key {
                    code: KeyCode::Character('i'),
                    modifiers: Vec::new(),
                })
                .await
                .unwrap();
            let applied = second
                .input(InputEvent::PasteChunk {
                    text: "fresh ".to_string(),
                    final_chunk: true,
                })
                .await
                .unwrap();
            assert!(applied.lines.iter().any(|line| line.text.contains("fresh")));
            assert!(!applied
                .lines
                .iter()
                .any(|line| line.text.contains("must-not-cross-clients")));
            stop_session(directory.path(), "work").await.unwrap();
            drop(second);
        };
        let (server_result, ()) = tokio::join!(server, client);
        server_result.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn idle_editor_owner_accepts_authenticated_stop_control() {
        use crate::{
            buffer::Buffer,
            config::Config,
            editor::{DetachedEditorCore, Editor, ACTION_DISPATCHER, PLUGIN_DISPATCHER_TEST_LOCK},
            lsp::LspManager,
            theme::Theme,
        };

        let _dispatcher_guard = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        while ACTION_DISPATCHER.try_recv_request().is_some() {}
        let config = Config::from_user_toml_with_overrides("", &[]).unwrap();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let editor = Editor::with_size(
            lsp,
            40,
            10,
            config,
            Theme::default(),
            vec![Buffer::new(None, "idle\n".to_string())],
        )
        .unwrap();
        let core = DetachedEditorCore::new(editor).await.unwrap();
        let directory = tempfile::tempdir().unwrap();
        let session = bind_session(directory.path(), "idle").unwrap();

        let server = serve_editor_session(&session, core);
        let stop = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            stop_session(directory.path(), "idle").await.unwrap();
        };
        let (server_result, ()) = tokio::join!(server, stop);

        server_result.unwrap();
    }
}
