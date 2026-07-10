//! Versioned local-IPC proof for a detachable headless editor owner.
//!
//! This is intentionally smaller than [`crate::editor::Editor`]. It proves the process
//! boundary—normalized input in, render deltas out—without pretending that terminal,
//! plugin, LSP, and persistence ownership have already been extracted.

use std::sync::Arc;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::{
    io::{
        AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf,
        WriteHalf,
    },
    sync::Mutex,
};

/// First stable version of Red's detachable-core IPC protocol.
pub const IPC_PROTOCOL_VERSION: u32 = 1;

const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Terminal-independent input sent by an attached client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputEvent {
    Key {
        code: KeyCode,
        modifiers: Vec<KeyModifier>,
    },
    Paste {
        text: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyCode {
    Character(char),
    Enter,
    Backspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyModifier {
    Control,
    Alt,
    Shift,
}

/// Client-to-owner protocol messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Connect {
        protocol_version: u32,
        last_revision: Option<u64>,
    },
    Input {
        sequence: u64,
        event: InputEvent,
    },
}

/// A complete logical line replacement in the next client frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinePatch {
    pub row: usize,
    pub text: String,
}

/// Minimal render delta returned by the headless owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderDelta {
    pub revision: u64,
    pub lines: Vec<LinePatch>,
    pub cursor: (usize, usize),
}

/// Owner-to-client protocol messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Connected {
        protocol_version: u32,
        render: RenderDelta,
    },
    Render {
        sequence: u64,
        delta: RenderDelta,
    },
    Error {
        message: String,
    },
}

/// Small persistent owner used to validate detach mechanics.
#[derive(Debug, Clone)]
pub struct HeadlessOwner {
    lines: Vec<String>,
    cursor: (usize, usize),
    revision: u64,
}

impl HeadlessOwner {
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
        }
    }

    #[must_use]
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
        let changed_row = match event {
            InputEvent::Key { code, modifiers } => {
                anyhow::ensure!(
                    modifiers.is_empty(),
                    "detach spike only accepts unmodified editing keys"
                );
                self.apply_key(code)?
            }
            InputEvent::Paste { text } => self.apply_paste(&text),
        };
        self.revision = self.revision.saturating_add(1);
        Ok(RenderDelta {
            revision: self.revision,
            lines: changed_row
                .into_iter()
                .map(|row| LinePatch {
                    row,
                    text: self.lines[row].clone(),
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
    }) = read_frame(&mut reader).await?
    else {
        anyhow::bail!("first detach message must be a connect handshake");
    };
    anyhow::ensure!(
        protocol_version == IPC_PROTOCOL_VERSION,
        "unsupported detach protocol version {protocol_version}"
    );
    let render = owner.lock().await.snapshot(last_revision);
    write_frame(
        &mut writer,
        &ServerMessage::Connected {
            protocol_version: IPC_PROTOCOL_VERSION,
            render,
        },
    )
    .await?;

    while let Some(message) = read_frame(&mut reader).await? {
        match message {
            ClientMessage::Input { sequence, event } => {
                let delta = owner.lock().await.apply(event)?;
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
        let (reader, mut writer) = tokio::io::split(stream);
        write_frame(
            &mut writer,
            &ClientMessage::Connect {
                protocol_version: IPC_PROTOCOL_VERSION,
                last_revision,
            },
        )
        .await?;
        let mut reader = BufReader::new(reader);
        let Some(ServerMessage::Connected {
            protocol_version,
            render,
        }) = read_frame(&mut reader).await?
        else {
            anyhow::bail!("detach owner did not return a connect response");
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
        match read_frame(&mut self.reader).await? {
            Some(ServerMessage::Render {
                sequence: response_sequence,
                delta,
            }) if response_sequence == sequence => Ok(delta),
            Some(ServerMessage::Error { message }) => anyhow::bail!(message),
            response => anyhow::bail!("unexpected detach response: {response:?}"),
        }
    }
}

async fn read_frame<R, T>(reader: &mut R) -> anyhow::Result<Option<T>>
where
    R: AsyncBufRead + Unpin,
    T: DeserializeOwned,
{
    let mut bytes = Vec::new();
    let count = reader.read_until(b'\n', &mut bytes).await?;
    if count == 0 {
        return Ok(None);
    }
    anyhow::ensure!(count <= MAX_FRAME_BYTES, "detach IPC frame is too large");
    Ok(Some(serde_json::from_slice(&bytes)?))
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
    writer.write_all(&bytes).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
                text: "λabc".into()
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
}
