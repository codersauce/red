use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct InitializeParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_id: Option<u64>,
    // client_info: Option<ClientInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initialization_options: Option<serde_json::Value>,
    pub capabilities: ClientCapabilities,
    // trace: Option<TraceOption>,
    // workspace_folders: Option<Vec<WorkspaceFolder>>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct ClientCapabilities {
    // workspace: Option<WorkspaceClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_document: Option<TextDocumentClientCapabilities>,
    // window: Option<WindowClientCapabilities>,
    // experimental: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TextDocumentClientCapabilities {
    pub completion: Option<CompletionClientCapabilities>,
    // syncrhonization: Option<TextDocumentSyncClientCapabilities>,
    // hover: Option<HoverClientCapabilities>,
    // signature_help: Option<SignatureHelpClientCapabilities>,
    // declaration: Option<DeclarationClientCapabilities>,
    // definition: Option<DefinitionClientCapabilities>,
    // type_definition: Option<TypeDefinitionClientCapabilities>,
    // implementation: Option<ImplementationClientCapabilities>,
    // references: Option<ReferencesClientCapabilities>,
    // document_highlight: Option<DocumentHighlightClientCapabilities>,
    // document_symbol: Option<DocumentSymbolClientCapabilities>,
    // code_action: Option<CodeActionClientCapabilities>,
    // code_lens: Option<CodeLensClientCapabilities>,
    // document_link: Option<DocumentLinkClientCapabilities>,
    // color_provider: Option<ColorProviderClientCapabilities>,
    // formatting: Option<FormattingClientCapabilities>,
    // range_formatting: Option<RangeFormattingClientCapabilities>,
    // on_type_formatting: Option<OnTypeFormattingClientCapabilities>,
    // rename: Option<RenameClientCapabilities>,
    // publish_diagnostics: Option<PublishDiagnosticsClientCapabilities>,
    // folding_range: Option<FoldingRangeClientCapabilities>,
    // selection_range: Option<SelectionRangeClientCapabilities>,
    // linked_editing_range: Option<LinkedEditingRangeClientCapabilities>,
    // call_hierarchy: Option<CallHierarchyClientCapabilities>,
    // semantic_tokens: Option<SemanticTokensClientCapabilities>,
    // moniker: Option<MonikerClientCapabilities>,
    // type_hierarchy: Option<TypeHierarchyClientCapabilities>,
    // inline_value: Option<InlineValueClientCapabilities>,
    // inlay_hint: Option<InlayHintClientCapabilities>,
    // diagnostic: Option<DiagnosticClientCapabilities>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CompletionClientCapabilities {
    // dynamic_registration: Option<bool>,
    pub completion_item: Option<CompletionItem>,
    // completion_item_kind: Option<CompletionItemKindCapabilities>,
    // context_support: Option<bool>,
    // insert_text_mode: Option<InsertTextMode>,
    // completion_list: Option<CompletionListCapabilities>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct CompletionItem {
    pub snippet_support: Option<bool>,
    pub commit_characters_support: Option<bool>,
    pub documentation_format: Option<Vec<MarkupKind>>,
    pub deprecated_support: Option<bool>,
    pub preselect_support: Option<bool>,
    pub tag_support: Option<CompletionTag>,
    pub insert_replace_support: Option<bool>,
    pub resolve_support: Option<CompletionResolveSupport>,
    pub insert_text_mode_support: Option<InsertTextMode>,
    pub label_details_support: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CompletionTag {
    value_set: Vec<CompletionItemTag>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CompletionResolveSupport {
    properties: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InsertTextMode {
    value_set: Vec<InsertTextMode>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum CompletionItemTag {
    Deprecated, // export const Deprecated = 1;
}

#[derive(Debug, Serialize, Deserialize)]
pub enum MarkupKind {
    PlainText,
    Markdown,
}
