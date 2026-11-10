//! Domain types for agent payloads. They hold what the editor and plugins
//! read, not what any one backend sends. Where a backend passes through a
//! value Red never interprets, the type says so and keeps it as JSON.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A model and optional reasoning effort the user picked for new turns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSelection {
    pub model: String,
    #[serde(default)]
    pub effort: Option<String>,
}

/// The model a conversation currently runs with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub model: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
}

/// One reasoning effort a model supports.
///
/// Serialized with the backend's camelCase names, which the model picker in
/// the agent plugin reads. A field the backend did not report is left out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffortOption {
    pub reasoning_effort: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Backend-specific fields Red does not interpret, forwarded to plugins
    /// unchanged. Opaque JSON by nature.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A model the backend offers. Serialized with the backend's camelCase names,
/// which the model picker in the agent plugin reads. A field the backend did
/// not report is left out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOption {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_default: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported_reasoning_efforts: Option<Vec<EffortOption>>,
    /// Backend-specific fields Red does not interpret, forwarded to plugins
    /// unchanged. Opaque JSON by nature.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// What a model request asked for.
#[derive(Debug, Clone)]
pub enum ModelQuery {
    /// Resolve a workspace's new-conversation defaults without creating a thread.
    ReadDefault { cwd: PathBuf },
    /// List the selectable models.
    List,
    /// Change the model of a running conversation.
    Set {
        session_id: String,
        selection: ModelSelection,
    },
}

/// A finished model request.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelResult {
    Catalog(Vec<ModelOption>),
    Default(ModelInfo),
    /// The change was accepted. `confirmed` is the model the backend reports
    /// afterwards, when it reported one.
    Selected {
        confirmed: Option<ModelInfo>,
    },
}

impl Serialize for ModelResult {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;
        match self {
            Self::Catalog(models) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("models", models)?;
                map.end()
            }
            Self::Default(info) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("model_info", info)?;
                map.end()
            }
            Self::Selected { confirmed } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("accepted", &true)?;
                map.serialize_entry("model_info", confirmed)?;
                map.end()
            }
        }
    }
}

/// Which saved conversations a session list covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionScope {
    /// Conversations Red created in this workspace.
    Red,
    /// Any conversation recorded for this workspace.
    Workspace,
    /// Every conversation.
    All,
}

/// A request for the saved-session picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionQuery {
    pub cwd: PathBuf,
    pub scope: SessionScope,
    pub query: String,
    pub exclude_session_id: Option<String>,
    /// Also search conversation text, which can take many seconds. Otherwise
    /// a query matches titles.
    pub search_contents: bool,
}

/// A request for the latest exchange of one saved session.
#[derive(Debug, Clone)]
pub struct SessionPreviewRequest {
    pub cwd: PathBuf,
    pub session_id: String,
}

/// One saved conversation in the session picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub cwd: String,
    pub label: String,
    pub detail: String,
    pub snippet: String,
    pub source: String,
    pub time_group: String,
    pub is_red: bool,
    /// A Red thread from this workspace rejoins as itself; anything else
    /// continues in a new thread here.
    pub resumable: bool,
    pub updated_at: i64,
}

/// The most recent sessions that matched a list request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionPage {
    pub sessions: Vec<SessionSummary>,
    /// Older matches were left out.
    pub more: bool,
}

/// The latest exchange of a saved session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionPreview {
    pub session_id: String,
    pub prompt: String,
    pub response: String,
}

/// A saved conversation's model-visible history, newest turn last.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoredHistory {
    /// Only the newest turns were read, so earlier annotations may be missing.
    pub truncated: bool,
    pub turns: Vec<RestoredTurn>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoredTurn {
    /// `None` when the backend gave the turn no identifier.
    pub id: Option<String>,
    pub items: Vec<RestoredItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoredItem {
    User {
        id: Option<String>,
        /// What the person typed, without the context and recap Red sent with
        /// it. `None` when the message carried no text.
        text: Option<String>,
    },
    Agent {
        id: Option<String>,
        /// May be empty.
        text: String,
    },
    /// A completed, successful annotation tool call.
    Annotations(AnnotationChange),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnnotationChange {
    Added {
        /// `None` when the call reported no file.
        path: Option<String>,
        annotations: Vec<AnnotationDraft>,
    },
    Dismissed {
        ids: Vec<String>,
    },
}

/// An annotation as the backend reported it, before Red validates it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotationDraft {
    pub id: String,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub message: Option<String>,
}

/// Structured progress for one step of the active turn.
///
/// Serializes with a `session_update` discriminator, as the agent plugin
/// reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "session_update")]
pub enum Activity {
    #[serde(rename = "agent_thought_chunk")]
    Thought,
    #[serde(rename = "tool_call")]
    ToolStarted(ToolActivity),
    #[serde(rename = "tool_call_update")]
    ToolFinished(ToolActivity),
}

/// A tool call's bounded, human-readable presentation. It never carries raw
/// arguments or file contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolActivity {
    pub tool_call_id: String,
    pub kind: ToolKind,
    pub title: String,
    pub full_title: String,
    pub status: ToolStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolKind {
    List,
    Search,
    Read,
    Edit,
    Create,
    Inspect,
    Diagnostics,
    Rename,
    Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

/// A user prompt as the editor hands it to a backend. The backend turns it
/// into its own wire format; the editor never builds or parses marker text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// What the person typed.
    pub text: String,
    /// The document the person was looking at, when the prompt should see it.
    pub context: Option<EditorContext>,
    /// Transcript recap for a conversation that was interrupted or forked.
    pub recap: Option<String>,
}

/// Bounded editor context sent along with a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorContext {
    /// URI of the active document.
    pub uri: String,
    /// Bounded excerpt of its contents.
    pub text: String,
}
