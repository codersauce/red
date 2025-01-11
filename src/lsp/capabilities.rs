use std::process;

use serde_json::json;

use super::types::*;

pub fn get_client_capabilities(workspace_uri: impl ToString) -> InitializeParams {
    let workspace_uri = workspace_uri.to_string();
    let text_document_capabilities = TextDocumentClientCapabilities::builder()
        .completion(
            CompletionClientCapabilities::builder()
                .completion_item(CompletionItem::builder().snippet_support(true).build())
                .build(),
        )
        .definition(
            DefinitionClientCapabilities::builder()
                .dynamic_registration(true)
                .link_support(false)
                .build(),
        )
        .synchronization(
            TextDocumentSyncClientCapabilities::builder()
                .dynamic_registration(true)
                .will_save(true)
                .will_save_wait_until(true)
                .did_save(true)
                .build(),
        )
        .hover(
            HoverClientCapabilities::builder()
                .dynamic_registration(true)
                .content_format(vec![MarkupKind::PlainText])
                .build(),
        )
        .formatting(
            DocumentFormattingClientCapabilities::builder()
                .dynamic_registration(true)
                .build(),
        )
        .document_symbol(
            DocumentSymbolClientCapabilities::builder()
                .dynamic_registration(true)
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
                .dynamic_registration(true)
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
                .build(),
        )
        .signature_help(
            SignatureHelpClientCapabilities::builder()
                .dynamic_registration(true)
                .signature_information(
                    SignatureInformation::builder()
                        .documentation_format(vec![MarkupKind::PlainText, MarkupKind::Markdown])
                        .parameter_information(
                            ParameterInformation::builder()
                                .label_offset_support(true)
                                .build(),
                        )
                        .active_parameter_support(true)
                        .build(),
                )
                .build(),
        )
        .document_highlight(
            DocumentHighlightClientCapabilities::builder()
                .dynamic_registration(true)
                .build(),
        )
        .document_link(
            DocumentLinkClientCapabilities::builder()
                .dynamic_registration(true)
                .tooltip_support(true)
                .build(),
        )
        .color_provider(
            DocumentColorClientCapabilities::builder()
                .dynamic_registration(true)
                .build(),
        )
        .folding_range(
            FoldingRangeClientCapabilities::builder()
                .dynamic_registration(true)
                .line_folding_only(true)
                .build(),
        )
        .semantic_tokens(
            SemanticTokensClientCapabilities::builder()
                .dynamic_registration(true)
                .requests(
                    SemanticTokensRequestClientCapabilities::builder()
                        .full(SemanticTokensFullValue::Full)
                        .build(),
                )
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
                .build(),
        )
        .inlay_hint(
            InlayHintClientCapabilities::builder()
                .dynamic_registration(true)
                .resolve_support(
                    InlayHintResolveSupport::builder()
                        .properties(vec![
                            "tooltip".to_string(),
                            "textEdits".to_string(),
                            "label.tooltip".to_string(),
                            "label.location".to_string(),
                            "label.command".to_string(),
                        ])
                        .build(),
                )
                .build(),
        )
        .diagnostic(
            DiagnosticClientCapabilities::builder()
                .dynamic_registration(true)
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
                .version_support(true)
                .code_description_support(true)
                .data_support(true)
                .build(),
        )
        .build();

    let window = WindowClientCapabilities::builder()
        .show_message(
            ShowMessageRequestClientCapabilities::builder()
                .message_action_item(
                    MessageActionItem::builder()
                        .additional_properties_support(true)
                        .build(),
                )
                .build(),
        )
        .show_document(
            ShowDocumentClientCapabilities::builder()
                .support(true)
                .build(),
        )
        .work_done_progress(true)
        .build();

    let workspace = WorkspaceClientCapabilities::builder()
        .symbol(
            WorkspaceSymbolClientCapabilities::builder()
                .dynamic_registration(true)
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
        .workspace_edit(
            WorkspaceEditClientCapabilities::builder()
                .document_changes(true)
                .resource_operations(vec![
                    ResourceOperationKind::Create,
                    ResourceOperationKind::Rename,
                    ResourceOperationKind::Delete,
                ])
                .build(),
        )
        .execute_command(
            ExecuteCommandClientCapabilities::builder()
                .dynamic_registration(true)
                .build(),
        )
        .diagnostics(
            DiagnosticWorkspaceClientCapabilities::builder()
                .refresh_support(true)
                .build(),
        )
        .build();

    InitializeParams::builder()
        .process_id(process::id().into())
        .client_info(ClientInfo::new("red", Some("0.1.0")))
        .root_uri(workspace_uri.clone())
        .workspace_folders(vec![WorkspaceFolder::new(workspace_uri.clone(), "red")])
        .capabilities(
            ClientCapabilities::builder()
                .text_document(text_document_capabilities)
                .window(window)
                .workspace(workspace)
                .build(),
        )
        .initialization_options(get_client_capabilities_initialization_options())
        .build()
}

fn get_client_capabilities_initialization_options() -> serde_json::Value {
    json!({
      "restartServerOnConfigChange": false,
      "showUnlinkedFileNotification": true,
      "showRequestFailedErrorNotification": true,
      "showDependenciesExplorer": true,
      "testExplorer": false,
      "initializeStopped": false,
      "runnables": {
        "extraEnv": null,
        "problemMatcher": [
          "$rustc"
        ],
        "askBeforeUpdateTest": true,
        "command": null,
        "extraArgs": [],
        "extraTestBinaryArgs": [
          "--show-output"
        ]
      },
      "statusBar": {
        "clickAction": "openLogs",
        "showStatusBar": {
          "documentSelector": [
            {
              "language": "rust"
            },
            {
              "pattern": "**/Cargo.toml"
            },
            {
              "pattern": "**/Cargo.lock"
            }
          ]
        }
      },
      "server": {
        "path": null,
        "extraEnv": null
      },
      "trace": {
        "server": "verbose",
        "extension": false
      },
      "debug": {
        "engine": "auto",
        "sourceFileMap": {
          "/rustc/<id>": "${env:USERPROFILE}/.rustup/toolchains/<toolchain-id>/lib/rustlib/src/rust"
        },
        "openDebugPane": false,
        "buildBeforeRestart": false,
        "engineSettings": {}
      },
      "typing": {
        "continueCommentsOnNewline": true,
        "excludeChars": "|<"
      },
      "diagnostics": {
        "previewRustcOutput": false,
        "useRustcErrorCode": false,
        "disabled": [],
        "enable": true,
        "experimental": {
          "enable": false
        },
        "remapPrefix": {},
        "styleLints": {
          "enable": false
        },
        "warningsAsHint": [],
        "warningsAsInfo": []
      },
      "assist": {
        "emitMustUse": false,
        "expressionFillDefault": "todo",
        "termSearch": {
          "borrowcheck": true,
          "fuel": 1800
        }
      },
      "cachePriming": {
        "enable": true,
        "numThreads": "physical"
      },
      "cargo": {
        "allTargets": true,
        "autoreload": true,
        "buildScripts": {
          "enable": true,
          "invocationStrategy": "per_workspace",
          "overrideCommand": null,
          "rebuildOnSave": true,
          "useRustcWrapper": true
        },
        "cfgs": {
          "miri": null,
          "debug_assertions": null
        },
        "extraArgs": [],
        "extraEnv": {},
        "features": [],
        "noDefaultFeatures": false,
        "sysroot": "discover",
        "sysrootSrc": null,
        "target": null,
        "targetDir": null
      },
      "cfg": {
        "setTest": true
      },
      "checkOnSave": true,
      "check": {
        "allTargets": null,
        "command": "clippy",
        "extraArgs": [],
        "extraEnv": {},
        "features": null,
        "ignore": [],
        "invocationStrategy": "per_workspace",
        "noDefaultFeatures": null,
        "overrideCommand": null,
        "targets": null,
        "workspace": true
      },
      "completion": {
        "addSemicolonToUnit": true,
        "autoimport": {
          "enable": true,
          "exclude": [
            {
              "path": "core::borrow::Borrow",
              "type": "methods"
            },
            {
              "path": "core::borrow::BorrowMut",
              "type": "methods"
            }
          ]
        },
        "autoself": {
          "enable": true
        },
        "callable": {
          "snippets": "fill_arguments"
        },
        "excludeTraits": [],
        "fullFunctionSignatures": {
          "enable": false
        },
        "hideDeprecated": false,
        "limit": null,
        "postfix": {
          "enable": true
        },
        "privateEditable": {
          "enable": false
        },
        "snippets": {
          "custom": {
            "Ok": {
              "postfix": "ok",
              "body": "Ok(${receiver})",
              "description": "Wrap the expression in a `Result::Ok`",
              "scope": "expr"
            },
            "Box::pin": {
              "postfix": "pinbox",
              "body": "Box::pin(${receiver})",
              "requires": "std::boxed::Box",
              "description": "Put the expression into a pinned `Box`",
              "scope": "expr"
            },
            "Arc::new": {
              "postfix": "arc",
              "body": "Arc::new(${receiver})",
              "requires": "std::sync::Arc",
              "description": "Put the expression into an `Arc`",
              "scope": "expr"
            },
            "Some": {
              "postfix": "some",
              "body": "Some(${receiver})",
              "description": "Wrap the expression in an `Option::Some`",
              "scope": "expr"
            },
            "Err": {
              "postfix": "err",
              "body": "Err(${receiver})",
              "description": "Wrap the expression in a `Result::Err`",
              "scope": "expr"
            },
            "Rc::new": {
              "postfix": "rc",
              "body": "Rc::new(${receiver})",
              "requires": "std::rc::Rc",
              "description": "Put the expression into an `Rc`",
              "scope": "expr"
            }
          }
        },
        "termSearch": {
          "enable": false,
          "fuel": 1000
        }
      },
      "files": {
        "excludeDirs": [],
        "watcher": "client"
      },
      "highlightRelated": {
        "breakPoints": {
          "enable": true
        },
        "closureCaptures": {
          "enable": true
        },
        "exitPoints": {
          "enable": true
        },
        "references": {
          "enable": true
        },
        "yieldPoints": {
          "enable": true
        }
      },
      "hover": {
        "actions": {
          "debug": {
            "enable": true
          },
          "enable": true,
          "gotoTypeDef": {
            "enable": true
          },
          "implementations": {
            "enable": true
          },
          "references": {
            "enable": false
          },
          "run": {
            "enable": true
          },
          "updateTest": {
            "enable": true
          }
        },
        "documentation": {
          "enable": true,
          "keywords": {
            "enable": true
          }
        },
        "links": {
          "enable": true
        },
        "maxSubstitutionLength": 20,
        "memoryLayout": {
          "alignment": "hexadecimal",
          "enable": true,
          "niches": false,
          "offset": "hexadecimal",
          "size": "both"
        },
        "show": {
          "enumVariants": 5,
          "fields": 5,
          "traitAssocItems": null
        }
      },
      "imports": {
        "granularity": {
          "enforce": false,
          "group": "crate"
        },
        "group": {
          "enable": true
        },
        "merge": {
          "glob": true
        },
        "preferNoStd": false,
        "preferPrelude": false,
        "prefix": "plain",
        "prefixExternPrelude": false
      },
      "inlayHints": {
        "bindingModeHints": {
          "enable": false
        },
        "chainingHints": {
          "enable": true
        },
        "closingBraceHints": {
          "enable": true,
          "minLines": 25
        },
        "closureCaptureHints": {
          "enable": false
        },
        "closureReturnTypeHints": {
          "enable": "never"
        },
        "closureStyle": "impl_fn",
        "discriminantHints": {
          "enable": "never"
        },
        "expressionAdjustmentHints": {
          "enable": "never",
          "hideOutsideUnsafe": false,
          "mode": "prefix"
        },
        "genericParameterHints": {
          "const": {
            "enable": true
          },
          "lifetime": {
            "enable": false
          },
          "type": {
            "enable": false
          }
        },
        "implicitDrops": {
          "enable": false
        },
        "lifetimeElisionHints": {
          "enable": "never",
          "useParameterNames": false
        },
        "maxLength": 25,
        "parameterHints": {
          "enable": true
        },
        "rangeExclusiveHints": {
          "enable": false
        },
        "reborrowHints": {
          "enable": "never"
        },
        "renderColons": true,
        "typeHints": {
          "enable": true,
          "hideClosureInitialization": false,
          "hideNamedConstructor": false
        }
      },
      "interpret": {
        "tests": false
      },
      "joinLines": {
        "joinAssignments": true,
        "joinElseIf": true,
        "removeTrailingComma": true,
        "unwrapTrivialBlock": true
      },
      "lens": {
        "debug": {
          "enable": true
        },
        "enable": true,
        "implementations": {
          "enable": true
        },
        "location": "above_name",
        "references": {
          "adt": {
            "enable": false
          },
          "enumVariant": {
            "enable": false
          },
          "method": {
            "enable": false
          },
          "trait": {
            "enable": false
          }
        },
        "run": {
          "enable": true
        },
        "updateTest": {
          "enable": true
        }
      },
      "linkedProjects": [],
      "lru": {
        "capacity": null,
        "query": {
          "capacities": {}
        }
      },
      "notifications": {
        "cargoTomlNotFound": true
      },
      "numThreads": null,
      "procMacro": {
        "attributes": {
          "enable": true
        },
        "enable": true,
        "ignored": {},
        "server": null
      },
      "references": {
        "excludeImports": false,
        "excludeTests": false
      },
      "rustc": {
        "source": null
      },
      "rustfmt": {
        "extraArgs": [],
        "overrideCommand": null,
        "rangeFormatting": {
          "enable": false
        }
      },
      "semanticHighlighting": {
        "doc": {
          "comment": {
            "inject": {
              "enable": true
            }
          }
        },
        "nonStandardTokens": true,
        "operator": {
          "enable": true,
          "specialization": {
            "enable": false
          }
        },
        "punctuation": {
          "enable": false,
          "separate": {
            "macro": {
              "bang": false
            }
          },
          "specialization": {
            "enable": false
          }
        },
        "strings": {
          "enable": true
        }
      },
      "signatureInfo": {
        "detail": "full",
        "documentation": {
          "enable": true
        }
      },
      "workspace": {
        "discoverConfig": null,
        "symbol": {
          "search": {
            "kind": "only_types",
            "limit": 128,
            "scope": "workspace"
          }
        }
      }
    })
}
