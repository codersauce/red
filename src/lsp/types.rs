use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::log;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDocumentPublishDiagnostics {
    pub uri: Option<String>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
pub struct DiagnosticCodeDescription {
    pub href: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticRelatedInformation {
    pub location: Location,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub capabilities: ServerCapabilities,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_info: Option<ServerInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position_encoding: Option<PositionEncodingKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_document_sync: Option<TextDocumentSyncOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection_range_provider: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hover_provider: Option<HoverProviderCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_provider: Option<CompletionOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature_help_provider: Option<SignatureHelpOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition_provider: Option<DefinitionProviderCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_definition_provider: Option<TypeDefinitionProviderCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub implementation_provider: Option<ImplementationProviderCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub references_provider: Option<ReferencesProviderCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_highlight_provider: Option<DocumentHighlightProviderCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_symbol_provider: Option<DocumentSymbolProviderCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_symbol_provider: Option<WorkspaceSymbolProviderCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_action_provider: Option<CodeActionProviderCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_lens_provider: Option<CodeLensOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_formatting_provider: Option<DocumentFormattingProviderCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_range_formatting_provider: Option<DocumentRangeFormattingProviderCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_on_type_formatting_provider: Option<DocumentOnTypeFormattingOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rename_provider: Option<RenameProviderCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folding_range_provider: Option<FoldingRangeProviderCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declaration_provider: Option<DeclarationProviderCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execute_command_provider: Option<ExecuteCommandOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceServerCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_hierarchy_provider: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_tokens_provider: Option<SemanticTokensOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inlay_hint_provider: Option<InlayHintOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PositionEncodingKind {
    #[serde(rename = "utf-8")]
    Utf8,
    #[serde(rename = "utf-16")]
    Utf16,
    #[serde(rename = "utf-32")]
    Utf32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDocumentSyncOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_close: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<TextDocumentSyncKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub will_save: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub will_save_wait_until: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub save: Option<SaveOptions>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "i32", into = "i32")]
pub enum TextDocumentSyncKind {
    None = 0,
    Full = 1,
    Incremental = 2,
}

impl From<i32> for TextDocumentSyncKind {
    fn from(value: i32) -> Self {
        match value {
            0 => TextDocumentSyncKind::None,
            1 => TextDocumentSyncKind::Full,
            2 => TextDocumentSyncKind::Incremental,
            _ => panic!("Invalid TextDocumentSyncKind value: {}", value),
        }
    }
}

impl From<TextDocumentSyncKind> for i32 {
    fn from(kind: TextDocumentSyncKind) -> i32 {
        kind as i32
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_text: Option<bool>,
}

// Base structure for various provider options that may include WorkDoneProgress
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkDoneProgressOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_done_progress: Option<bool>,
}

// Define provider option types using the base structure
pub type HoverOptions = WorkDoneProgressOptions;
pub type DeclarationOptions = WorkDoneProgressOptions;
pub type DefinitionOptions = WorkDoneProgressOptions;
pub type TypeDefinitionOptions = WorkDoneProgressOptions;
pub type ImplementationOptions = WorkDoneProgressOptions;
pub type ReferenceOptions = WorkDoneProgressOptions;
pub type DocumentHighlightOptions = WorkDoneProgressOptions;
pub type DocumentSymbolOptions = WorkDoneProgressOptions;
pub type WorkspaceSymbolOptions = WorkDoneProgressOptions;
pub type DocumentFormattingOptions = WorkDoneProgressOptions;
pub type DocumentRangeFormattingOptions = WorkDoneProgressOptions;
pub type FoldingRangeOptions = WorkDoneProgressOptions;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_done_progress: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_characters: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub all_commit_characters: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolve_provider: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_item: Option<CompletionOptionsCompletionItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionOptionsCompletionItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_details_support: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureHelpOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_done_progress: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_characters: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retrigger_characters: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeActionOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_done_progress: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_action_kinds: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolve_provider: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_done_progress: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prepare_provider: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteCommandOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_done_progress: Option<bool>,
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_folders: Option<WorkspaceFoldersServerCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_operations: Option<FileOperationsServerCapabilities>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFoldersServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supported: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_notifications: Option<ChangeNotificationsCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOperationsServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub did_create: Option<FileOperationRegistrationOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub will_create: Option<FileOperationRegistrationOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub did_rename: Option<FileOperationRegistrationOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub will_rename: Option<FileOperationRegistrationOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub did_delete: Option<FileOperationRegistrationOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub will_delete: Option<FileOperationRegistrationOptions>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOperationRegistrationOptions {
    pub filters: Vec<FileOperationFilter>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOperationFilter {
    pub scheme: Option<String>,
    pub pattern: FileOperationPattern,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOperationPattern {
    pub glob: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matches: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<FileOperationPatternOptions>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileOperationPatternKind {
    File,
    Folder,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOperationPatternOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore_case: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFolder {
    pub uri: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DynamicRegistrationCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSymbolClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_kind: Option<SymbolKindCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolKindCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_set: Option<Vec<SymbolKind>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
#[serde(rename_all = "camelCase")]
pub struct CompletionItemTag {
    pub value_set: Vec<CompletionItemTagKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CompletionItemTagKind {
    Deprecated = 1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionItemResolveSupport {
    pub properties: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InsertTextModeSupport {
    pub value_set: Vec<InsertTextMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InsertTextMode {
    AsIs = 1,
    AdjustIndentation = 2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionItemKindCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_set: Option<Vec<CompletionItemKind>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "i32", into = "i32")]
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

impl From<i32> for CompletionItemKind {
    fn from(value: i32) -> Self {
        match value {
            1 => CompletionItemKind::Text,
            2 => CompletionItemKind::Method,
            3 => CompletionItemKind::Function,
            4 => CompletionItemKind::Constructor,
            5 => CompletionItemKind::Field,
            6 => CompletionItemKind::Variable,
            7 => CompletionItemKind::Class,
            8 => CompletionItemKind::Interface,
            9 => CompletionItemKind::Module,
            10 => CompletionItemKind::Property,
            11 => CompletionItemKind::Unit,
            12 => CompletionItemKind::Value,
            13 => CompletionItemKind::Enum,
            14 => CompletionItemKind::Keyword,
            15 => CompletionItemKind::Snippet,
            16 => CompletionItemKind::Color,
            17 => CompletionItemKind::File,
            18 => CompletionItemKind::Reference,
            19 => CompletionItemKind::Folder,
            20 => CompletionItemKind::EnumMember,
            21 => CompletionItemKind::Constant,
            22 => CompletionItemKind::Struct,
            23 => CompletionItemKind::Event,
            24 => CompletionItemKind::Operator,
            25 => CompletionItemKind::TypeParameter,
            _ => panic!("Invalid CompletionItemKind value: {}", value),
        }
    }
}

impl From<CompletionItemKind> for i32 {
    fn from(kind: CompletionItemKind) -> i32 {
        kind as i32
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MarkupKind {
    PlainText,
    Markdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HoverClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_format: Option<Vec<MarkupKind>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureHelpClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature_information: Option<SignatureInformation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_support: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureInformation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation_format: Option<Vec<MarkupKind>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameter_information: Option<ParameterInformation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_parameter_support: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParameterInformation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_offset_support: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclarationClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_support: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DefinitionClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_support: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeDefinitionClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_support: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImplementationClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_support: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentHighlightClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSymbolClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_kind: Option<SymbolKindCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hierarchical_document_symbol_support: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentFormattingClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentRangeFormattingClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentOnTypeFormattingClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
pub enum PrepareSupportDefaultBehavior {
    Identifier = 1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticTagSupport {
    pub value_set: Vec<DiagnosticTag>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FoldingRangeClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range_limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_folding_only: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionRangeClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_done_progress: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_message: Option<ShowMessageRequestClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_document: Option<ShowDocumentClientCapabilities>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShowMessageRequestClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_action_item: Option<MessageActionItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageActionItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_properties_support: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShowDocumentClientCapabilities {
    pub support: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionResponse {
    pub is_incomplete: bool,
    pub items: Vec<CompletionResponseItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionResponseItem {
    pub label: String,
    pub kind: Option<CompletionItemKind>,
    pub detail: Option<String>,
    pub documentation: Option<Documentation>,
    pub deprecated: Option<bool>,
    pub preselect: Option<bool>,
    pub sort_text: Option<String>,
    pub filter_text: Option<String>,
    pub insert_text: Option<String>,
    pub insert_text_format: Option<InsertTextFormat>,
    pub text_edit: Option<TextEdit>,
    pub additional_text_edits: Option<Vec<TextEdit>>,
    pub command: Option<Command>,
    pub data: Option<Value>,
    pub commit_characters: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Documentation {
    String(String),
    MarkupContent(MarkupContent),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkupContent {
    pub kind: MarkupKind,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "i32", into = "i32")]
pub enum InsertTextFormat {
    PlainText = 1,
    Snippet = 2,
}

impl From<i32> for InsertTextFormat {
    fn from(value: i32) -> Self {
        match value {
            1 => InsertTextFormat::PlainText,
            2 => InsertTextFormat::Snippet,
            _ => panic!("Invalid InsertTextFormat value: {}", value),
        }
    }
}

impl From<InsertTextFormat> for i32 {
    fn from(kind: InsertTextFormat) -> i32 {
        kind as i32
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextEdit {
    pub range: Range,
    pub new_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Command {
    pub title: String,
    pub command: String,
    pub arguments: Option<Vec<Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HoverProviderCapability {
    Simple(bool),
    Options(HoverOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DeclarationProviderCapability {
    Simple(bool),
    Options(DeclarationOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DefinitionProviderCapability {
    Simple(bool),
    Options(DefinitionOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TypeDefinitionProviderCapability {
    Simple(bool),
    Options(TypeDefinitionOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ImplementationProviderCapability {
    Simple(bool),
    Options(ImplementationOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReferencesProviderCapability {
    Simple(bool),
    Options(ReferenceOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DocumentHighlightProviderCapability {
    Simple(bool),
    Options(DocumentHighlightOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DocumentSymbolProviderCapability {
    Simple(bool),
    Options(DocumentSymbolOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WorkspaceSymbolProviderCapability {
    Simple(bool),
    Options(WorkspaceSymbolOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CodeActionProviderCapability {
    Simple(bool),
    Options(CodeActionOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DocumentFormattingProviderCapability {
    Simple(bool),
    Options(DocumentFormattingOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DocumentRangeFormattingProviderCapability {
    Simple(bool),
    Options(DocumentRangeFormattingOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RenameProviderCapability {
    Simple(bool),
    Options(RenameOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FoldingRangeProviderCapability {
    Simple(bool),
    Options(FoldingRangeOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChangeNotificationsCapability {
    Simple(bool),
    String(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeLensOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolve_provider: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentOnTypeFormattingOptions {
    pub first_trigger_character: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub more_trigger_character: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticTokensLegend {
    pub token_types: Vec<String>,
    pub token_modifiers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticTokensOptions {
    pub legend: SemanticTokensLegend,
    pub range: Option<bool>,
    pub full: Option<SemanticTokensFull>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticTokensFull {
    pub delta: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InlayHintOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolve_provider: Option<bool>,
}
