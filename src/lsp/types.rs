use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::log;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextDocumentPublishDiagnostics {
    pub uri: Option<String>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub range: Range,
    pub severity: Option<DiagnosticSeverity>,
    pub code: Option<DiagnosticCode>,
    // pub code_description: Option<DiagnosticCodeDescription>,
    // pub source: Option<String>,
    pub message: String,
    pub related_information: Option<Vec<DiagnosticRelatedInformation>>,
    pub data: Option<Value>,
    pub tags: Option<Vec<DiagnosticTag>>,
}

impl Diagnostic {
    pub fn is_for(&self, uri: &str) -> bool {
        let Some(ref related_infos) = self.related_information else {
            return true;
        };

        related_infos.iter().any(|ri| ri.location.uri == uri)
    }

    pub fn affected_lines(&self) -> Vec<usize> {
        let Range { start, end } = &self.range;
        log!("Affected lines: {:?}", start.line..=end.line);
        (start.line..=end.line).collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub line: usize,
    pub character: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "i32", into = "i32")]
pub enum DiagnosticSeverity {
    Error = 1,
    Warning = 2,
    Information = 3,
    Hint = 4,
}

impl From<DiagnosticSeverity> for i32 {
    fn from(severity: DiagnosticSeverity) -> i32 {
        severity as i32
    }
}

impl From<i32> for DiagnosticSeverity {
    fn from(value: i32) -> Self {
        match value {
            1 => DiagnosticSeverity::Error,
            2 => DiagnosticSeverity::Warning,
            3 => DiagnosticSeverity::Information,
            4 => DiagnosticSeverity::Hint,
            _ => panic!("Invalid DiagnosticSeverity value: {}", value),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DiagnosticCode {
    Int(usize),
    String(String),
}

impl DiagnosticCode {
    pub fn as_string(&self) -> String {
        match self {
            DiagnosticCode::Int(i) => i.to_string(),
            DiagnosticCode::String(s) => s.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticCodeDescription {
    pub href: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticRelatedInformation {
    pub location: Location,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Location {
    pub uri: String,
    pub range: Range,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "i32", into = "i32")]
pub enum DiagnosticTag {
    Unnecessary = 1,
    Deprecated = 2,
}

impl From<i32> for DiagnosticTag {
    fn from(value: i32) -> Self {
        match value {
            1 => DiagnosticTag::Unnecessary,
            2 => DiagnosticTag::Deprecated,
            _ => panic!("Invalid DiagnosticTag value: {}", value),
            // Or handle invalid values differently based on your needs
        }
    }
}

impl From<DiagnosticTag> for i32 {
    fn from(tag: DiagnosticTag) -> i32 {
        tag as i32
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct InitializeParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_info: Option<ClientInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initialization_options: Option<Value>,
    pub capabilities: ClientCapabilities,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<TraceValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_folders: Option<Vec<WorkspaceFolder>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TraceValue {
    Off,
    Messages,
    Verbose,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceFolder {
    pub uri: String,
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct ClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_document: Option<TextDocumentClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply_edit: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_edit: Option<WorkspaceEditClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub did_change_configuration: Option<DynamicRegistrationCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub did_change_watched_files: Option<DynamicRegistrationCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<WorkspaceSymbolClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execute_command: Option<DynamicRegistrationCapability>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceEditClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_changes: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_operations: Option<Vec<ResourceOperationKind>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_handling: Option<FailureHandlingKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceOperationKind {
    Create,
    Rename,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FailureHandlingKind {
    Abort,
    Transactional,
    TextOnlyTransactional,
    Undo,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DynamicRegistrationCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceSymbolClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_kind: Option<SymbolKindCapability>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SymbolKindCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_set: Option<Vec<SymbolKind>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SymbolKind {
    File = 1,
    Module = 2,
    Namespace = 3,
    Package = 4,
    Class = 5,
    Method = 6,
    Property = 7,
    Field = 8,
    Constructor = 9,
    Enum = 10,
    Interface = 11,
    Function = 12,
    Variable = 13,
    Constant = 14,
    String = 15,
    Number = 16,
    Boolean = 17,
    Array = 18,
    Object = 19,
    Key = 20,
    Null = 21,
    EnumMember = 22,
    Struct = 23,
    Event = 24,
    Operator = 25,
    TypeParameter = 26,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TextDocumentClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synchronization: Option<TextDocumentSyncClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion: Option<CompletionClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hover: Option<HoverClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature_help: Option<SignatureHelpClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declaration: Option<DeclarationClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<DefinitionClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_definition: Option<TypeDefinitionClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub implementation: Option<ImplementationClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub references: Option<ReferenceClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_highlight: Option<DocumentHighlightClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_symbol: Option<DocumentSymbolClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formatting: Option<DocumentFormattingClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range_formatting: Option<DocumentRangeFormattingClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_type_formatting: Option<DocumentOnTypeFormattingClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rename: Option<RenameClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publish_diagnostics: Option<PublishDiagnosticsClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folding_range: Option<FoldingRangeClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection_range: Option<SelectionRangeClientCapabilities>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TextDocumentSyncClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub will_save: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub will_save_wait_until: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub did_save: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CompletionClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_item: Option<CompletionItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_item_kind: Option<CompletionItemKindCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_support: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct CompletionItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet_support: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_characters_support: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation_format: Option<Vec<MarkupKind>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated_support: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preselect_support: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_support: Option<CompletionItemTag>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub insert_replace_support: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolve_support: Option<CompletionItemResolveSupport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub insert_text_mode_support: Option<InsertTextModeSupport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_details_support: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionItemTag {
    pub value_set: Vec<CompletionItemTagKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CompletionItemTagKind {
    Deprecated = 1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionItemResolveSupport {
    pub properties: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsertTextModeSupport {
    pub value_set: Vec<InsertTextMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InsertTextMode {
    AsIs = 1,
    AdjustIndentation = 2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionItemKindCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_set: Option<Vec<CompletionItemKind>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CompletionItemKind {
    Text = 1,
    Method = 2,
    Function = 3,
    Constructor = 4,
    Field = 5,
    Variable = 6,
    Class = 7,
    Interface = 8,
    Module = 9,
    Property = 10,
    Unit = 11,
    Value = 12,
    Enum = 13,
    Keyword = 14,
    Snippet = 15,
    Color = 16,
    File = 17,
    Reference = 18,
    Folder = 19,
    EnumMember = 20,
    Constant = 21,
    Struct = 22,
    Event = 23,
    Operator = 24,
    TypeParameter = 25,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MarkupKind {
    PlainText,
    Markdown,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HoverClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_format: Option<Vec<MarkupKind>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SignatureHelpClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature_information: Option<SignatureInformation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_support: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SignatureInformation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation_format: Option<Vec<MarkupKind>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameter_information: Option<ParameterInformation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_parameter_support: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ParameterInformation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_offset_support: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeclarationClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_support: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DefinitionClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_support: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TypeDefinitionClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_support: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ImplementationClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_support: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReferenceClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DocumentHighlightClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DocumentSymbolClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_kind: Option<SymbolKindCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hierarchical_document_symbol_support: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DocumentFormattingClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DocumentRangeFormattingClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DocumentOnTypeFormattingClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RenameClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prepare_support: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prepare_support_default_behavior: Option<PrepareSupportDefaultBehavior>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub honors_change_annotations: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PrepareSupportDefaultBehavior {
    Identifier = 1,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PublishDiagnosticsClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related_information: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_support: Option<DiagnosticTagSupport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_support: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_description_support: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_support: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DiagnosticTagSupport {
    pub value_set: Vec<DiagnosticTag>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FoldingRangeClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range_limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_folding_only: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SelectionRangeClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WindowClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_done_progress: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_message: Option<ShowMessageRequestClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_document: Option<ShowDocumentClientCapabilities>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ShowMessageRequestClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_action_item: Option<MessageActionItem>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MessageActionItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_properties_support: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ShowDocumentClientCapabilities {
    pub support: bool,
}
