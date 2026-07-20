use std::process;

use serde_json::{json, Value};

use super::types::*;

pub fn get_client_capabilities(workspace_uri: impl ToString) -> InitializeParams {
    get_client_capabilities_with_options(workspace_uri, "red", Value::Null)
}

pub fn get_client_capabilities_with_options(
    workspace_uri: impl ToString,
    workspace_name: impl ToString,
    initialization_options: Value,
) -> InitializeParams {
    let workspace_uri = workspace_uri.to_string();
    let workspace_name = workspace_name.to_string();
    let text_document_capabilities = TextDocumentClientCapabilities::builder()
        .synchronization(
            TextDocumentSyncClientCapabilities::builder()
                .dynamic_registration(false)
                .will_save(false)
                .will_save_wait_until(false)
                .did_save(false)
                .build(),
        )
        .completion(
            CompletionClientCapabilities::builder()
                .dynamic_registration(false)
                .context_support(true)
                .completion_item(
                    CompletionItem::builder()
                        .snippet_support(true)
                        .commit_characters_support(true)
                        .documentation_format(vec![MarkupKind::Plaintext, MarkupKind::Markdown])
                        .deprecated_support(true)
                        .preselect_support(true)
                        .tag_support(
                            CompletionItemTag::builder()
                                .value_set(vec![CompletionItemTagKind::Deprecated])
                                .build(),
                        )
                        .insert_replace_support(false)
                        .insert_text_mode_support(
                            InsertTextModeSupport::builder()
                                .value_set(vec![
                                    InsertTextMode::AsIs,
                                    InsertTextMode::AdjustIndentation,
                                ])
                                .build(),
                        )
                        .label_details_support(false)
                        .build(),
                )
                .insert_text_mode(InsertTextMode::AsIs)
                .build(),
        )
        .hover(
            HoverClientCapabilities::builder()
                .dynamic_registration(false)
                .content_format(vec![MarkupKind::Markdown, MarkupKind::Plaintext])
                .build(),
        )
        .signature_help(
            SignatureHelpClientCapabilities::builder()
                .dynamic_registration(false)
                .signature_information(
                    SignatureInformation::builder()
                        .documentation_format(vec![MarkupKind::Plaintext, MarkupKind::Markdown])
                        .parameter_information(
                            ParameterInformation::builder()
                                .label_offset_support(true)
                                .build(),
                        )
                        .active_parameter_support(true)
                        .build(),
                )
                .context_support(true)
                .build(),
        )
        .definition(
            DefinitionClientCapabilities::builder()
                .dynamic_registration(false)
                .link_support(false)
                .build(),
        )
        .references(
            ReferenceClientCapabilities::builder()
                .dynamic_registration(false)
                .build(),
        )
        .document_highlight(
            DocumentHighlightClientCapabilities::builder()
                .dynamic_registration(false)
                .build(),
        )
        .document_symbol(
            DocumentSymbolClientCapabilities::builder()
                .dynamic_registration(false)
                .symbol_kind(
                    SymbolKindCapability::builder()
                        .value_set(vec![
                            SymbolKind::File,
                            SymbolKind::Module,
                            SymbolKind::Namespace,
                            SymbolKind::Package,
                            SymbolKind::Class,
                            SymbolKind::Method,
                            SymbolKind::Property,
                            SymbolKind::Field,
                            SymbolKind::Constructor,
                            SymbolKind::Enum,
                            SymbolKind::Interface,
                            SymbolKind::Function,
                            SymbolKind::Variable,
                            SymbolKind::Constant,
                            SymbolKind::String,
                            SymbolKind::Number,
                            SymbolKind::Boolean,
                            SymbolKind::Array,
                            SymbolKind::Object,
                            SymbolKind::Key,
                            SymbolKind::Null,
                            SymbolKind::EnumMember,
                            SymbolKind::Struct,
                            SymbolKind::Event,
                            SymbolKind::Operator,
                            SymbolKind::TypeParameter,
                        ])
                        .build(),
                )
                .hierarchical_document_symbol_support(true)
                .build(),
        )
        .code_action(
            CodeActionClientCapabilities::builder()
                .dynamic_registration(false)
                .is_preferred_support(true)
                .disabled_support(true)
                .data_support(true)
                .code_action_literal_support(
                    CodeActionLiteralSupport::builder()
                        .code_action_kind(
                            CodeActionKindCapability::builder()
                                .value_set(vec![
                                    CodeActionKind::QuickFix,
                                    CodeActionKind::Refactor,
                                    CodeActionKind::RefactorExtract,
                                    CodeActionKind::RefactorInline,
                                    CodeActionKind::RefactorRewrite,
                                    CodeActionKind::Source,
                                    CodeActionKind::SourceOrganizeImports,
                                    CodeActionKind::SourceFixAll,
                                ])
                                .build(),
                        )
                        .build(),
                )
                .honors_change_annotations(false)
                .build(),
        )
        .code_lens(
            CodeLensClientCapabilities::builder()
                .dynamic_registration(false)
                .build(),
        )
        .formatting(
            DocumentFormattingClientCapabilities::builder()
                .dynamic_registration(false)
                .build(),
        )
        .range_formatting(
            DocumentRangeFormattingClientCapabilities::builder()
                .dynamic_registration(false)
                .ranges_support(false)
                .build(),
        )
        .on_type_formatting(
            DocumentOnTypeFormattingClientCapabilities::builder()
                .dynamic_registration(false)
                .build(),
        )
        .rename(
            RenameClientCapabilities::builder()
                .dynamic_registration(false)
                .prepare_support(true)
                .prepare_support_default_behavior(PrepareSupportDefaultBehavior::Identifier)
                .honors_change_annotations(false)
                .build(),
        )
        .document_link(
            DocumentLinkClientCapabilities::builder()
                .dynamic_registration(false)
                .tooltip_support(true)
                .build(),
        )
        .type_definition(
            TypeDefinitionClientCapabilities::builder()
                .dynamic_registration(false)
                .link_support(false)
                .build(),
        )
        .implementation(
            ImplementationClientCapabilities::builder()
                .dynamic_registration(false)
                .link_support(false)
                .build(),
        )
        .color_provider(
            DocumentColorClientCapabilities::builder()
                .dynamic_registration(false)
                .build(),
        )
        .folding_range(
            FoldingRangeClientCapabilities::builder()
                .dynamic_registration(false)
                .range_limit(5000)
                .line_folding_only(true)
                .folding_range_kind(
                    FoldingRangeKindCapability::builder()
                        .value_set(vec![
                            FoldingRangeKind::Comment,
                            FoldingRangeKind::Imports,
                            FoldingRangeKind::Region,
                        ])
                        .build(),
                )
                .folding_range(
                    FoldingRangeCapability::builder()
                        .collapsed_text(true)
                        .build(),
                )
                .build(),
        )
        .declaration(
            DeclarationClientCapabilities::builder()
                .dynamic_registration(false)
                .link_support(false)
                .build(),
        )
        .selection_range(
            SelectionRangeClientCapabilities::builder()
                .dynamic_registration(false)
                .build(),
        )
        .call_hierarchy(
            CallHierarchyClientCapabilities::builder()
                .dynamic_registration(false)
                .build(),
        )
        .semantic_tokens(
            SemanticTokensClientCapabilities::builder()
                .dynamic_registration(false)
                .token_types(vec![
                    "namespace".to_string(),
                    "type".to_string(),
                    "class".to_string(),
                    "enum".to_string(),
                    "interface".to_string(),
                    "struct".to_string(),
                    "typeParameter".to_string(),
                    "parameter".to_string(),
                    "variable".to_string(),
                    "property".to_string(),
                    "enumMember".to_string(),
                    "event".to_string(),
                    "function".to_string(),
                    "method".to_string(),
                    "macro".to_string(),
                    "keyword".to_string(),
                    "modifier".to_string(),
                    "comment".to_string(),
                    "string".to_string(),
                    "number".to_string(),
                    "regexp".to_string(),
                    "operator".to_string(),
                ])
                .token_modifiers(vec![
                    "declaration".to_string(),
                    "definition".to_string(),
                    "readonly".to_string(),
                    "static".to_string(),
                    "deprecated".to_string(),
                    "abstract".to_string(),
                    "async".to_string(),
                    "modification".to_string(),
                    "documentation".to_string(),
                    "defaultLibrary".to_string(),
                ])
                .formats(vec![TokensFormat::Relative])
                .requests(
                    SemanticTokensRequestClientCapabilities::builder()
                        .full(SemanticTokensFullValue::Delta(true))
                        .range(true)
                        .build(),
                )
                .multiline_token_support(false)
                .overlapping_token_support(false)
                .sever_cancel_support(true)
                .arguments_syntax_tree(false)
                .build(),
        )
        .linked_editing_range(
            LinkedEditingRangeClientCapabilities::builder()
                .dynamic_registration(false)
                .build(),
        )
        .type_hierarchy(
            TypeHierarchyClientCapabilities::builder()
                .dynamic_registration(false)
                .build(),
        )
        .inline_value(
            InlineValueClientCapabilities::builder()
                .dynamic_registration(false)
                .build(),
        )
        .inlay_hint(
            InlayHintClientCapabilities::builder()
                .dynamic_registration(false)
                .build(),
        )
        .diagnostic(
            DiagnosticClientCapabilities::builder()
                .dynamic_registration(false)
                .related_document_support(false)
                .build(),
        )
        .publish_diagnostics(
            PublishDiagnosticsClientCapabilities::builder()
                .related_information(true)
                .tag_support(
                    DiagnosticTagSupport::builder()
                        .value_set(vec![DiagnosticTag::Unnecessary, DiagnosticTag::Deprecated])
                        .build(),
                )
                .version_support(false)
                .code_description_support(true)
                .data_support(true)
                .build(),
        )
        .build();

    let window = WindowClientCapabilities::builder()
        .show_document(
            ShowDocumentClientCapabilities::builder()
                .support(false)
                .build(),
        )
        .work_done_progress(false)
        .build();

    let workspace = WorkspaceClientCapabilities::builder()
        .apply_edit(true)
        .symbol(
            WorkspaceSymbolClientCapabilities::builder()
                .dynamic_registration(false)
                .symbol_kind(
                    SymbolKindCapability::builder()
                        .value_set(vec![
                            SymbolKind::File,
                            SymbolKind::Module,
                            SymbolKind::Namespace,
                            SymbolKind::Package,
                            SymbolKind::Class,
                            SymbolKind::Method,
                            SymbolKind::Property,
                            SymbolKind::Field,
                            SymbolKind::Constructor,
                            SymbolKind::Enum,
                            SymbolKind::Interface,
                            SymbolKind::Function,
                            SymbolKind::Variable,
                            SymbolKind::Constant,
                            SymbolKind::String,
                            SymbolKind::Number,
                            SymbolKind::Boolean,
                            SymbolKind::Array,
                            SymbolKind::Object,
                            SymbolKind::Key,
                            SymbolKind::Null,
                            SymbolKind::EnumMember,
                            SymbolKind::Struct,
                            SymbolKind::Event,
                            SymbolKind::Operator,
                            SymbolKind::TypeParameter,
                        ])
                        .build(),
                )
                .build(),
        )
        .workspace_edit(workspace_edit_client_capabilities())
        .execute_command(
            ExecuteCommandClientCapabilities::builder()
                .dynamic_registration(false)
                .build(),
        )
        .diagnostics(
            DiagnosticWorkspaceClientCapabilities::builder()
                .refresh_support(false)
                .build(),
        )
        .build();

    InitializeParams::builder()
        .process_id(process::id().into())
        .client_info(ClientInfo::new("red", Some("0.1.0")))
        .root_uri(workspace_uri.clone())
        .workspace_folders(vec![WorkspaceFolder::new(
            workspace_uri.clone(),
            workspace_name,
        )])
        .capabilities(
            ClientCapabilities::builder()
                .text_document(text_document_capabilities)
                .window(window)
                .workspace(workspace)
                .general(GeneralClientCapabilities {
                    stale_request_support: None,
                    regular_expressions: None,
                    markdown: None,
                    position_encodings: Some(vec![PositionEncodingKind::Utf16]),
                })
                .experimental(json!({
                    "hoverActions": true
                }))
                .build(),
        )
        .initialization_options(initialization_options)
        .build()
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn workspace_edit_client_capabilities() -> WorkspaceEditClientCapabilities {
    WorkspaceEditClientCapabilities::builder()
        .document_changes(true)
        .resource_operations(vec![
            ResourceOperationKind::Create,
            ResourceOperationKind::Rename,
            ResourceOperationKind::Delete,
        ])
        .failure_handling(FailureHandlingKind::Transactional)
        .build()
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android", target_vendor = "apple"))
))]
fn workspace_edit_client_capabilities() -> WorkspaceEditClientCapabilities {
    WorkspaceEditClientCapabilities::builder()
        .document_changes(true)
        .resource_operations(vec![
            ResourceOperationKind::Create,
            ResourceOperationKind::Delete,
        ])
        .failure_handling(FailureHandlingKind::Transactional)
        .build()
}

#[cfg(not(unix))]
fn workspace_edit_client_capabilities() -> WorkspaceEditClientCapabilities {
    WorkspaceEditClientCapabilities::builder()
        .document_changes(false)
        .failure_handling(FailureHandlingKind::TextOnlyTransactional)
        .build()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn advertises_utf16_position_encoding() {
        let params = get_client_capabilities("file:///tmp");
        let encodings = params
            .capabilities
            .general
            .and_then(|general| general.position_encodings);

        assert_eq!(serde_json::to_value(encodings).unwrap(), json!(["utf-16"]));
    }

    #[test]
    fn prefers_markdown_hover_content_with_plaintext_fallback() {
        let params = serde_json::to_value(get_client_capabilities("file:///tmp")).unwrap();

        assert_eq!(
            params["capabilities"]["textDocument"]["hover"]["contentFormat"],
            json!(["markdown", "plaintext"])
        );
        assert_eq!(
            params["capabilities"]["experimental"]["hoverActions"],
            json!(true)
        );
    }

    #[test]
    fn advertises_only_supported_workspace_edit_capabilities() {
        let params = serde_json::to_value(get_client_capabilities("file:///tmp")).unwrap();
        let capabilities = &params["capabilities"];
        let workspace_edit = &capabilities["workspace"]["workspaceEdit"];

        assert_eq!(
            serde_json::to_value(FailureHandlingKind::TextOnlyTransactional).unwrap(),
            json!("textOnlyTransactional")
        );
        assert_eq!(capabilities["workspace"]["applyEdit"], json!(true));
        assert_eq!(workspace_edit["documentChanges"], json!(cfg!(unix)));
        assert_eq!(
            workspace_edit["failureHandling"],
            if cfg!(unix) {
                json!("transactional")
            } else {
                json!("textOnlyTransactional")
            }
        );
        #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
        assert_eq!(
            workspace_edit["resourceOperations"],
            json!(["create", "rename", "delete"])
        );
        #[cfg(all(
            unix,
            not(any(target_os = "linux", target_os = "android", target_vendor = "apple"))
        ))]
        assert_eq!(
            workspace_edit["resourceOperations"],
            json!(["create", "delete"])
        );
        #[cfg(not(unix))]
        assert!(workspace_edit.get("resourceOperations").is_none());
        assert_eq!(
            capabilities["textDocument"]["codeAction"]["honorsChangeAnnotations"],
            json!(false)
        );
        assert_eq!(
            capabilities["textDocument"]["rename"]["honorsChangeAnnotations"],
            json!(false)
        );
    }

    #[test]
    fn does_not_advertise_unimplemented_dynamic_registration_or_save_lifecycle() {
        fn collect_dynamic_flags(value: &Value, flags: &mut Vec<bool>) {
            match value {
                Value::Object(values) => {
                    for (key, value) in values {
                        if key == "dynamicRegistration" {
                            flags.push(value.as_bool().unwrap_or(true));
                        }
                        collect_dynamic_flags(value, flags);
                    }
                }
                Value::Array(values) => values
                    .iter()
                    .for_each(|value| collect_dynamic_flags(value, flags)),
                _ => {}
            }
        }

        let params = serde_json::to_value(get_client_capabilities("file:///tmp")).unwrap();
        let capabilities = &params["capabilities"];
        let synchronization = &capabilities["textDocument"]["synchronization"];
        let mut flags = Vec::new();
        collect_dynamic_flags(capabilities, &mut flags);

        assert!(!flags.is_empty());
        assert!(flags.into_iter().all(|flag| !flag));
        assert_eq!(synchronization["willSave"], json!(false));
        assert_eq!(synchronization["willSaveWaitUntil"], json!(false));
        assert_eq!(synchronization["didSave"], json!(false));
        assert_eq!(
            capabilities["textDocument"]["completion"]["completionItem"]["insertReplaceSupport"],
            json!(false)
        );
        assert!(capabilities["textDocument"]["completion"]["completionItem"]
            .get("resolveSupport")
            .is_none());
        assert!(capabilities["textDocument"]["completion"]
            .get("completionList")
            .is_none());
        assert!(capabilities["textDocument"]["codeAction"]
            .get("resolveSupport")
            .is_none());
        assert_eq!(
            capabilities["window"]["showDocument"]["support"],
            json!(false)
        );
        assert_eq!(capabilities["window"]["workDoneProgress"], json!(false));
        assert!(capabilities["window"].get("showMessage").is_none());
        assert_eq!(
            capabilities["workspace"]["diagnostics"]["refreshSupport"],
            json!(false)
        );
    }
}
