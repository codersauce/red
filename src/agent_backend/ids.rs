//! Identifiers and snapshot vocabulary shared by every agent backend.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Red's id for one agent conversation.
///
/// Opaque to the editor: it is never parsed or sliced, only compared with
/// other `SessionId`s. A backend mints it when a conversation is created and
/// reports it in `SessionEvent::Created`. It stays stable across restarts and
/// equals the agent's own conversation id, so snapshots written before the
/// backend interface existed still resume.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    /// The id as the backend and the plugin API spell it.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Wraps an id read from a backend or from a persisted snapshot. Editor
    /// code receives ids; it does not make them.
    pub fn from_raw(raw: String) -> Self {
        Self(raw)
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Red's id for one prompt and the turn it starts.
///
/// The editor mints it per prompt. It is a real UUID because Claude Code
/// requires one to echo a prompt back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TurnId(Uuid);

impl TurnId {
    /// A fresh random turn id.
    pub fn mint() -> Self {
        Self(Uuid::new_v4())
    }

    /// Wraps an existing UUID.
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// The underlying UUID.
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl fmt::Display for TurnId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// Correlates one query with its `Event::Reply`. The editor picks it; it is
/// the plugin request id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QueryId(pub i64);

/// The agent behind a conversation. Backends are added here, and the compiler
/// then points at every `match` that needs a new arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    Codex,
}

impl BackendKind {
    /// What snapshots written before the backend field existed meant.
    pub fn codex() -> Self {
        Self::Codex
    }

    /// Display name for messages shown to the user.
    pub fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
        }
    }
}

/// Compatibility stamp a backend writes into a snapshot, today the tool
/// catalog version. The editor stores and returns it; only the backend
/// interprets it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContractStamp(u32);

impl ContractStamp {
    /// Wraps a backend's stamp value.
    pub fn new(value: u32) -> Self {
        Self(value)
    }

    /// The stamp value.
    pub fn get(self) -> u32 {
        self.0
    }
}

/// What the editor persists to bring a conversation back later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSession {
    /// Snapshots from before the backend interface existed omit this.
    #[serde(default = "BackendKind::codex")]
    pub backend: BackendKind,
    pub session: SessionId,
    /// The stamp the backend recorded when it last wrote this snapshot.
    #[serde(default)]
    pub contract: Option<ContractStamp>,
}

/// How a saved session can come back under the current backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Continuity {
    /// `restore` rejoins the same session; its id and history are unchanged.
    Rejoin,
    /// `restore` imports the history into a new session, so
    /// `SessionEvent::Restored.session` differs from the requested id.
    Fork,
    /// The snapshot belongs to another backend and `restore` would fail.
    Foreign,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_id_displays_like_a_hyphenated_uuid() {
        let uuid = Uuid::new_v4();
        assert_eq!(TurnId::from_uuid(uuid).to_string(), uuid.to_string());
        assert_eq!(TurnId::from_uuid(uuid).as_uuid(), uuid);
    }

    #[test]
    fn session_id_serializes_as_a_bare_string() {
        let id = SessionId::from_raw("thread-1".to_string());
        assert_eq!(serde_json::to_string(&id).unwrap(), r#""thread-1""#);
        assert_eq!(id.to_string(), "thread-1");
        assert_eq!(id.as_str(), "thread-1");
        assert_eq!(
            serde_json::from_str::<SessionId>(r#""thread-1""#).unwrap(),
            id
        );
    }

    #[test]
    fn saved_session_without_a_backend_or_stamp_is_a_codex_snapshot() {
        let saved: SavedSession = serde_json::from_str(r#"{"session":"thread-1"}"#).unwrap();
        assert_eq!(
            saved,
            SavedSession {
                backend: BackendKind::Codex,
                session: SessionId::from_raw("thread-1".to_string()),
                contract: None,
            }
        );
    }

    #[test]
    fn saved_session_round_trips_its_stamp_as_a_number() {
        let saved = SavedSession {
            backend: BackendKind::Codex,
            session: SessionId::from_raw("thread-1".to_string()),
            contract: Some(ContractStamp::new(7)),
        };
        let json = serde_json::to_value(&saved).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"backend": "codex", "session": "thread-1", "contract": 7})
        );
        assert_eq!(serde_json::from_value::<SavedSession>(json).unwrap(), saved);
        assert_eq!(ContractStamp::new(7).get(), 7);
    }

    #[test]
    fn backend_kind_names_the_agent_for_the_user() {
        assert_eq!(BackendKind::Codex.name(), "Codex");
    }
}
