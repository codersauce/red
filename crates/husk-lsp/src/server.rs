//! Husk language-server state and request handlers.

use std::collections::HashSet;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use husk_analysis::{
    AnalysisDiagnostic, DiagnosticSeverity as AnalysisDiagnosticSeverity, Document,
    Position as AnalysisPosition, Symbol, SymbolId, SymbolKind, Workspace, format_source,
};
use husk_ast::ItemKind;
use husk_lexer::{KEYWORDS, Lexer, TokenKind};
use husk_package::discover_manifest;
use husk_semantic::SemanticProfile;
use lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Position, Range};
use serde_json::{Map, Value, json};

use crate::dependencies::index_dependencies;
use crate::protocol::{read_message, write_message};
use crate::uri::{file_path, file_uri};

const TOKEN_TYPES: &[&str] = &[
    "namespace",
    "type",
    "enum",
    "struct",
    "typeParameter",
    "parameter",
    "variable",
    "property",
    "enumMember",
    "function",
    "method",
    "keyword",
    "string",
    "number",
    "operator",
    "comment",
];

/// Host-selected defaults for a Husk language-server process.
#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub profile: SemanticProfile,
    pub trusted_declarations: Vec<String>,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            profile: SemanticProfile::Native,
            trusted_declarations: Vec::new(),
        }
    }
}

/// Run one Husk LSP session over process standard input and output.
pub fn run_stdio(options: ServerOptions) -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = BufWriter::new(stdout.lock());
    let mut server = Server::new(options);
    while let Some(message) = read_message(&mut reader)? {
        let responses = server.handle(message);
        for response in responses {
            write_message(&mut writer, &response)?;
        }
        if server.exited {
            break;
        }
    }
    Ok(())
}

struct Server {
    options: ServerOptions,
    workspace: Option<Workspace>,
    shutdown_requested: bool,
    exited: bool,
    cancelled: HashSet<String>,
    dependency_diagnostics: Vec<String>,
}

impl Server {
    fn new(options: ServerOptions) -> Self {
        Self {
            options,
            workspace: None,
            shutdown_requested: false,
            exited: false,
            cancelled: HashSet::new(),
            dependency_diagnostics: Vec::new(),
        }
    }

    fn handle(&mut self, message: Value) -> Vec<Value> {
        let Some(object) = message.as_object() else {
            return vec![error_response(
                Value::Null,
                -32600,
                "JSON-RPC message must be an object",
            )];
        };
        let id = object.get("id").cloned();
        let method = object.get("method").and_then(Value::as_str);
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        match (id, method) {
            (Some(id), Some(method)) => {
                if self.cancelled.remove(&request_key(&id)) {
                    return vec![error_response(id, -32800, "request cancelled")];
                }
                vec![match self.handle_request(method, params) {
                    Ok(result) => success_response(id, result),
                    Err(error) => error_response(id, -32603, &format!("{error:#}")),
                }]
            }
            (None, Some(method)) => {
                self.handle_notification(method, params)
                    .unwrap_or_else(|error| {
                        vec![notification(
                            "window/logMessage",
                            json!({"type": 1, "message": format!("{error:#}")}),
                        )]
                    })
            }
            (Some(id), None) => vec![error_response(
                id,
                -32600,
                "JSON-RPC request omitted method",
            )],
            (None, None) => Vec::new(),
        }
    }

    fn handle_request(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        if method != "initialize" && self.workspace.is_none() {
            anyhow::bail!("Husk language server is not initialized");
        }
        match method {
            "initialize" => self.initialize(params),
            "shutdown" => {
                self.shutdown_requested = true;
                Ok(Value::Null)
            }
            "textDocument/diagnostic" => self.document_diagnostic(params),
            "workspace/diagnostic" => Ok(json!({"items": []})),
            "textDocument/completion" => self.completion(params),
            "completionItem/resolve" => Ok(params),
            "textDocument/hover" => self.hover(params),
            "textDocument/signatureHelp" => self.signature_help(params),
            "textDocument/definition"
            | "textDocument/declaration"
            | "textDocument/typeDefinition"
            | "textDocument/implementation" => self.definition(params),
            "textDocument/references" => self.references(params),
            "textDocument/documentHighlight" => self.document_highlight(params),
            "textDocument/prepareRename" => self.prepare_rename(params),
            "textDocument/rename" => self.rename(params),
            "textDocument/documentSymbol" => self.document_symbols(params),
            "workspace/symbol" => self.workspace_symbols(params),
            "textDocument/semanticTokens/full" => self.semantic_tokens(params),
            "textDocument/inlayHint" => self.inlay_hints(params),
            "textDocument/foldingRange" => self.folding_ranges(params),
            "textDocument/selectionRange" => self.selection_ranges(params),
            "textDocument/codeAction" => self.code_actions(params),
            "textDocument/formatting" => self.format_document(params, false),
            "textDocument/rangeFormatting" => self.format_document(params, true),
            "textDocument/prepareCallHierarchy" => self.prepare_call_hierarchy(params),
            "callHierarchy/incomingCalls" => self.incoming_calls(params),
            "callHierarchy/outgoingCalls" => self.outgoing_calls(params),
            "textDocument/documentLink" | "textDocument/colorPresentation" => Ok(json!([])),
            _ => anyhow::bail!("unsupported LSP request `{method}`"),
        }
    }

    fn handle_notification(&mut self, method: &str, params: Value) -> anyhow::Result<Vec<Value>> {
        match method {
            "initialized" => Ok(Vec::new()),
            "exit" => {
                self.exited = true;
                Ok(Vec::new())
            }
            "$/cancelRequest" => {
                if let Some(id) = params.get("id") {
                    self.cancelled.insert(request_key(id));
                }
                Ok(Vec::new())
            }
            "textDocument/didOpen" => {
                let item = params
                    .get("textDocument")
                    .context("didOpen omitted textDocument")?;
                let uri = required_str(item, "uri")?;
                let path = file_path(uri)?;
                let version = item
                    .get("version")
                    .and_then(Value::as_i64)
                    .and_then(|version| i32::try_from(version).ok())
                    .unwrap_or(1);
                let text = required_str(item, "text")?;
                self.workspace_mut()?
                    .update(&path, version, Arc::<str>::from(text))?;
                Ok(vec![self.publish_diagnostics(&path)?])
            }
            "textDocument/didChange" => {
                let document = params
                    .get("textDocument")
                    .context("didChange omitted textDocument")?;
                let uri = required_str(document, "uri")?;
                let path = file_path(uri)?;
                let version = document
                    .get("version")
                    .and_then(Value::as_i64)
                    .and_then(|version| i32::try_from(version).ok())
                    .context("didChange omitted a valid document version")?;
                let changes = params
                    .get("contentChanges")
                    .and_then(Value::as_array)
                    .context("didChange omitted contentChanges")?;
                self.apply_changes(&path, version, changes)?;
                Ok(vec![self.publish_diagnostics(&path)?])
            }
            "textDocument/didClose" => {
                let uri = text_document_uri(&params)?;
                let path = file_path(uri)?;
                self.workspace_mut()?.close(&path)?;
                Ok(vec![notification(
                    "textDocument/publishDiagnostics",
                    json!({"uri": uri, "diagnostics": []}),
                )])
            }
            "textDocument/didSave" | "workspace/didChangeWatchedFiles" => {
                self.workspace_mut()?.refresh_disk()?;
                Ok(Vec::new())
            }
            "workspace/didChangeConfiguration" => {
                if let Some(profile) = params
                    .pointer("/settings/husk/semanticProfile")
                    .and_then(Value::as_str)
                {
                    self.workspace_mut()?.set_profile(parse_profile(profile)?);
                }
                if let Some(flags) = params
                    .pointer("/settings/husk/cfgFlags")
                    .and_then(Value::as_array)
                {
                    self.workspace_mut()?.set_cfg_flags(
                        flags
                            .iter()
                            .filter_map(Value::as_str)
                            .map(ToString::to_string),
                    );
                }
                Ok(Vec::new())
            }
            _ => Ok(Vec::new()),
        }
    }

    fn initialize(&mut self, params: Value) -> anyhow::Result<Value> {
        anyhow::ensure!(self.workspace.is_none(), "server is already initialized");
        let root = workspace_root(&params)?;
        let initialization = params
            .get("initializationOptions")
            .and_then(Value::as_object);
        let explicit_profile = initialization
            .and_then(|options| options.get("semanticProfile"))
            .and_then(Value::as_str)
            .map(parse_profile)
            .transpose()?;
        let loose_profile = initialization
            .and_then(|options| options.get("looseSemanticProfile"))
            .and_then(Value::as_str)
            .map(parse_profile)
            .transpose()?;
        let profile = explicit_profile.unwrap_or_else(|| {
            if discover_manifest(&root).is_ok() {
                SemanticProfile::Native
            } else {
                loose_profile.unwrap_or(self.options.profile)
            }
        });
        let mut workspace = Workspace::open(&root, profile)?;
        let mut declarations = self.options.trusted_declarations.clone();
        if let Some(extra) = initialization
            .and_then(|options| options.get("declarations"))
            .and_then(Value::as_array)
        {
            declarations.extend(
                extra
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToString::to_string),
            );
        }
        if !declarations.is_empty() {
            workspace.set_trusted_declaration_sources(declarations)?;
        }
        let dependencies = index_dependencies(&root);
        self.dependency_diagnostics = dependencies.diagnostics;
        workspace.set_external_modules(dependencies.modules)?;
        for (path, source) in dependencies.stubs {
            workspace.update(path, 0, Arc::<str>::from(source))?;
        }
        self.workspace = Some(workspace);

        Ok(json!({
            "capabilities": {
                "positionEncoding": "utf-16",
                "textDocumentSync": {
                    "openClose": true,
                    "change": 2,
                    "save": true
                },
                "diagnosticProvider": {
                    "identifier": "husk",
                    "interFileDependencies": true,
                    "workspaceDiagnostics": false
                },
                "completionProvider": {
                    "resolveProvider": true,
                    "triggerCharacters": [".", ":", "<"]
                },
                "hoverProvider": true,
                "signatureHelpProvider": {
                    "triggerCharacters": ["(", ",", "<"],
                    "retriggerCharacters": [","]
                },
                "definitionProvider": true,
                "declarationProvider": true,
                "typeDefinitionProvider": true,
                "implementationProvider": true,
                "referencesProvider": true,
                "documentHighlightProvider": true,
                "documentSymbolProvider": true,
                "workspaceSymbolProvider": true,
                "renameProvider": {"prepareProvider": true},
                "semanticTokensProvider": {
                    "legend": {
                        "tokenTypes": TOKEN_TYPES,
                        "tokenModifiers": ["declaration", "definition", "readonly", "static", "defaultLibrary"]
                    },
                    "full": true,
                    "range": false
                },
                "inlayHintProvider": true,
                "foldingRangeProvider": true,
                "selectionRangeProvider": true,
                "codeActionProvider": {
                    "codeActionKinds": ["quickfix", "source.organizeImports"],
                    "resolveProvider": false
                },
                "documentFormattingProvider": true,
                "documentRangeFormattingProvider": true,
                "callHierarchyProvider": true
            },
            "serverInfo": {
                "name": "husk-lsp",
                "version": env!("CARGO_PKG_VERSION")
            }
        }))
    }

    fn document_diagnostic(&self, params: Value) -> anyhow::Result<Value> {
        let path = file_path(text_document_uri(&params)?)?;
        let diagnostics = self.diagnostics_for_path(&path)?;
        Ok(json!({
            "kind": "full",
            "items": diagnostics,
        }))
    }

    fn completion(&self, params: Value) -> anyhow::Result<Value> {
        let (path, byte) = self.document_position(&params)?;
        let document = self.document(&path)?;
        let (prefix, qualifier) = completion_context(document.text(), byte);
        let mut items = self
            .workspace()?
            .completions(&path, &prefix)
            .into_iter()
            .filter(|symbol| {
                qualifier.as_ref().is_none_or(|qualifier| {
                    symbol.container.as_deref() == Some(qualifier)
                        || symbol.qualified_name.starts_with(&format!("{qualifier}::"))
                })
            })
            .map(completion_item)
            .collect::<Vec<_>>();
        if qualifier.is_none() {
            items.extend(
                KEYWORDS
                    .iter()
                    .filter(|keyword| keyword.starts_with(&prefix))
                    .map(|keyword| {
                        json!({
                            "label": keyword,
                            "kind": 14,
                            "detail": "Husk keyword",
                            "sortText": format!("9-{keyword}")
                        })
                    }),
            );
        }
        Ok(json!({"isIncomplete": false, "items": items}))
    }

    fn hover(&self, params: Value) -> anyhow::Result<Value> {
        let (path, byte) = self.document_position(&params)?;
        let document = self.document(&path)?;
        if let Some(hover) = document.hover(byte) {
            let mut value = format!("```husk\n{}\n```", hover.signature);
            if let Some(docs) = &hover.docs {
                value.push_str("\n\n");
                value.push_str(docs);
            }
            return Ok(json!({
                "contents": {"kind": "markdown", "value": value}
            }));
        }
        let Some((_, symbol)) = self.symbol_at(&path, byte) else {
            return Ok(Value::Null);
        };
        let mut value = format!("```husk\n{}\n```", symbol.detail);
        if let Some(documentation) = &symbol.documentation {
            value.push_str("\n\n");
            value.push_str(documentation);
        }
        Ok(json!({
            "contents": {"kind": "markdown", "value": value},
            "range": self.range(document, &symbol.span)
        }))
    }

    fn signature_help(&self, params: Value) -> anyhow::Result<Value> {
        let (path, byte) = self.document_position(&params)?;
        let document = self.document(&path)?;
        let Some((name, active_parameter)) = call_context(document.text(), byte) else {
            return Ok(Value::Null);
        };
        let Some((_, symbol)) = self.workspace()?.symbol_named(&name) else {
            return Ok(Value::Null);
        };
        let parameters = signature_parameters(&symbol.detail);
        Ok(json!({
            "signatures": [{
                "label": symbol.detail,
                "documentation": symbol.documentation.as_ref().map(|documentation| json!({
                    "kind": "markdown",
                    "value": documentation
                })),
                "parameters": parameters.iter().map(|parameter| json!({"label": parameter})).collect::<Vec<_>>(),
                "activeParameter": active_parameter.min(parameters.len().saturating_sub(1))
            }],
            "activeSignature": 0,
            "activeParameter": active_parameter.min(parameters.len().saturating_sub(1))
        }))
    }

    fn definition(&self, params: Value) -> anyhow::Result<Value> {
        let (path, byte) = self.document_position(&params)?;
        let Some((definition_document, symbol)) = self.symbol_at(&path, byte) else {
            return Ok(Value::Null);
        };
        location(definition_document, &symbol.span)
    }

    fn references(&self, params: Value) -> anyhow::Result<Value> {
        let (path, byte) = self.document_position(&params)?;
        let Some((_, symbol)) = self.symbol_at(&path, byte) else {
            return Ok(json!([]));
        };
        let include_declaration = params
            .pointer("/context/includeDeclaration")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let mut locations = Vec::new();
        for occurrence in self.workspace()?.references(&symbol.id) {
            if !include_declaration && occurrence.is_definition {
                continue;
            }
            let document = self.document(&occurrence.path)?;
            locations.push(location(document, &occurrence.span)?);
        }
        if locations.len() <= usize::from(include_declaration) {
            locations.extend(self.textual_references(&symbol.name, include_declaration)?);
            deduplicate_locations(&mut locations);
        }
        Ok(Value::Array(locations))
    }

    fn document_highlight(&self, params: Value) -> anyhow::Result<Value> {
        let (path, byte) = self.document_position(&params)?;
        let Some((_, symbol)) = self.symbol_at(&path, byte) else {
            return Ok(json!([]));
        };
        let document = self.document(&path)?;
        let mut highlights = self
            .workspace()?
            .references(&symbol.id)
            .into_iter()
            .filter(|occurrence| occurrence.path == path)
            .map(|occurrence| {
                json!({
                    "range": self.range(document, &occurrence.span),
                    "kind": if occurrence.is_definition { 3 } else { 2 }
                })
            })
            .collect::<Vec<_>>();
        if highlights.len() <= 1 {
            highlights = word_occurrences(document.text(), &symbol.name)
                .into_iter()
                .map(|range| {
                    json!({
                        "range": self.range(document, &range),
                        "kind": if range == symbol.span { 3 } else { 2 }
                    })
                })
                .collect();
        }
        Ok(Value::Array(highlights))
    }

    fn prepare_rename(&self, params: Value) -> anyhow::Result<Value> {
        let (path, byte) = self.document_position(&params)?;
        let Some((document, symbol)) = self.symbol_at(&path, byte) else {
            return Ok(Value::Null);
        };
        anyhow::ensure!(
            !is_dependency_stub(document.path()),
            "external dependency symbols are read-only"
        );
        Ok(json!({
            "range": self.range(document, &symbol.span),
            "placeholder": symbol.name
        }))
    }

    fn rename(&self, params: Value) -> anyhow::Result<Value> {
        let (path, byte) = self.document_position(&params)?;
        let new_name = required_str(&params, "newName")?;
        anyhow::ensure!(
            husk_lexer::is_valid_identifier(new_name),
            "`{new_name}` is not a valid Husk identifier"
        );
        let Some((definition_document, symbol)) = self.symbol_at(&path, byte) else {
            return Ok(Value::Null);
        };
        anyhow::ensure!(
            !is_dependency_stub(definition_document.path()),
            "external dependency symbols are read-only"
        );
        let mut changes = Map::new();
        for occurrence in self.workspace()?.references(&symbol.id) {
            if is_dependency_stub(&occurrence.path) {
                continue;
            }
            let document = self.document(&occurrence.path)?;
            changes
                .entry(file_uri(document.path())?)
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .expect("workspace edit entry is initialized as an array")
                .push(json!({
                    "range": self.range(document, &occurrence.span),
                    "newText": new_name
                }));
        }
        Ok(json!({"changes": changes}))
    }

    fn document_symbols(&self, params: Value) -> anyhow::Result<Value> {
        let path = file_path(text_document_uri(&params)?)?;
        let document = self.document(&path)?;
        Ok(Value::Array(
            document
                .symbols()
                .iter()
                .filter(|symbol| {
                    !matches!(
                        symbol.kind,
                        SymbolKind::Variable | SymbolKind::Parameter | SymbolKind::TypeParameter
                    )
                })
                .map(|symbol| {
                    json!({
                        "name": symbol.name,
                        "detail": symbol.detail,
                        "kind": lsp_symbol_kind(symbol.kind),
                        "range": self.range(document, &symbol.full_span),
                        "selectionRange": self.range(document, &symbol.span)
                    })
                })
                .collect(),
        ))
    }

    fn workspace_symbols(&self, params: Value) -> anyhow::Result<Value> {
        let query = params.get("query").and_then(Value::as_str).unwrap_or("");
        let mut symbols = Vec::new();
        for (path, symbol) in self.workspace()?.workspace_symbols(query) {
            let document = self.document(path)?;
            symbols.push(json!({
                "name": symbol.name,
                "kind": lsp_symbol_kind(symbol.kind),
                "containerName": symbol.container,
                "location": location(document, &symbol.span)?
            }));
        }
        Ok(Value::Array(symbols))
    }

    fn semantic_tokens(&self, params: Value) -> anyhow::Result<Value> {
        let path = file_path(text_document_uri(&params)?)?;
        let document = self.document(&path)?;
        let mut absolute = Vec::<(u32, u32, u32, u32, u32)>::new();
        for token in Lexer::new(document.text()) {
            if matches!(token.kind, TokenKind::Eof) || token.span.range.is_empty() {
                continue;
            }
            let token_type = match &token.kind {
                TokenKind::Keyword(_) => 11,
                TokenKind::StringLiteral(_) => 12,
                TokenKind::IntLiteral(_) | TokenKind::FloatLiteral(_) => 13,
                TokenKind::Ident(_) => document
                    .symbols()
                    .iter()
                    .find(|symbol| symbol.span == token.span.range)
                    .map_or(6, |symbol| semantic_token_kind(symbol.kind)),
                TokenKind::LParen
                | TokenKind::RParen
                | TokenKind::LBrace
                | TokenKind::RBrace
                | TokenKind::Comma
                | TokenKind::Colon
                | TokenKind::ColonColon
                | TokenKind::Semicolon
                | TokenKind::Dot
                | TokenKind::LBracket
                | TokenKind::RBracket
                | TokenKind::Hash => continue,
                _ => 14,
            };
            let range = document.position_range(&token.span.range);
            if range.start.line != range.end.line {
                continue;
            }
            absolute.push((
                range.start.line,
                range.start.character,
                range.end.character.saturating_sub(range.start.character),
                token_type,
                0,
            ));
        }
        absolute.sort_unstable();
        let mut data = Vec::with_capacity(absolute.len() * 5);
        let mut previous_line = 0;
        let mut previous_start = 0;
        for (line, start, length, token_type, modifiers) in absolute {
            let delta_line = line.saturating_sub(previous_line);
            let delta_start = if delta_line == 0 {
                start.saturating_sub(previous_start)
            } else {
                start
            };
            data.extend([delta_line, delta_start, length, token_type, modifiers]);
            previous_line = line;
            previous_start = start;
        }
        Ok(json!({"data": data}))
    }

    fn inlay_hints(&self, params: Value) -> anyhow::Result<Value> {
        let path = file_path(text_document_uri(&params)?)?;
        let document = self.document(&path)?;
        let hints = document
            .symbols()
            .iter()
            .filter(|symbol| symbol.kind == SymbolKind::Variable)
            .filter_map(|symbol| {
                let hover = document.hover(symbol.span.start)?;
                let (_, ty) = hover.signature.split_once(':')?;
                Some(json!({
                    "position": to_lsp_position(document.position_range(&symbol.span).end),
                    "label": format!(": {}", ty.trim()),
                    "kind": 1,
                    "paddingLeft": false,
                    "paddingRight": true
                }))
            })
            .collect::<Vec<_>>();
        Ok(Value::Array(hints))
    }

    fn folding_ranges(&self, params: Value) -> anyhow::Result<Value> {
        let path = file_path(text_document_uri(&params)?)?;
        let document = self.document(&path)?;
        let ranges = document
            .syntax()
            .items
            .iter()
            .filter_map(|item| {
                let range = document.position_range(&item.span.range);
                (range.end.line > range.start.line).then(|| {
                    json!({
                        "startLine": range.start.line,
                        "startCharacter": range.start.character,
                        "endLine": range.end.line,
                        "endCharacter": range.end.character,
                        "kind": if matches!(item.kind, ItemKind::Use { .. }) { "imports" } else { "region" }
                    })
                })
            })
            .collect::<Vec<_>>();
        Ok(Value::Array(ranges))
    }

    fn selection_ranges(&self, params: Value) -> anyhow::Result<Value> {
        let path = file_path(text_document_uri(&params)?)?;
        let document = self.document(&path)?;
        let positions = params
            .get("positions")
            .and_then(Value::as_array)
            .context("selectionRange omitted positions")?;
        let end = document
            .position_range(&(document.text().len()..document.text().len()))
            .end;
        let document_range = json!({
            "start": {"line": 0, "character": 0},
            "end": to_lsp_position(end)
        });
        let mut selections = Vec::new();
        for position in positions {
            let position = parse_position(position)?;
            let byte = document
                .byte_offset(position)
                .context("selection position is not a UTF-16 boundary")?;
            let word = word_range(document.text(), byte).unwrap_or(byte..byte);
            let line = line_range(document.text(), byte);
            selections.push(json!({
                "range": self.range(document, &word),
                "parent": {
                    "range": self.range(document, &line),
                    "parent": {"range": document_range}
                }
            }));
        }
        Ok(Value::Array(selections))
    }

    fn code_actions(&self, params: Value) -> anyhow::Result<Value> {
        let path = file_path(text_document_uri(&params)?)?;
        let document = self.document(&path)?;
        let uri = file_uri(document.path())?;
        let mut actions = Vec::new();
        if let Some(diagnostics) = params
            .pointer("/context/diagnostics")
            .and_then(Value::as_array)
        {
            for diagnostic in diagnostics {
                let code = diagnostic.get("code").and_then(Value::as_str);
                let message = diagnostic
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if code == Some("HUSK-P0001") && message.starts_with("expected `;`") {
                    // Parser errors point at the unexpected next token. Insert
                    // after the preceding token, before any intervening comment
                    // or whitespace. Only trust a diagnostic for this revision.
                    let Some(current) = document.diagnostics().iter().find(|current| {
                        current.code == "HUSK-P0001"
                            && current.message == message
                            && self.range(document, &current.span) == diagnostic["range"]
                    }) else {
                        continue;
                    };
                    let Some(end) = Lexer::new(document.text())
                        .take_while(|token| token.span.range.end <= current.span.start)
                        .filter(|token| !matches!(token.kind, TokenKind::Eof))
                        .last()
                        .map(|token| token.span.range.end)
                    else {
                        continue;
                    };
                    let range = self.range(document, &(end..end));
                    actions.push(json!({
                        "title": "Insert missing semicolon",
                        "kind": "quickfix",
                        "isPreferred": true,
                        "diagnostics": [diagnostic],
                        "edit": {
                            "changes": {
                                uri.clone(): [{
                                    "range": range,
                                    "newText": ";"
                                }]
                            }
                        }
                    }));
                }
            }
        }
        if let Some((range, replacement)) = organize_imports(document) {
            actions.push(json!({
                "title": "Organize Husk imports",
                "kind": "source.organizeImports",
                "isPreferred": true,
                "edit": {
                    "changes": {
                        uri: [{
                            "range": self.range(document, &range),
                            "newText": replacement
                        }]
                    }
                }
            }));
        }
        Ok(Value::Array(actions))
    }

    fn format_document(&self, params: Value, range_only: bool) -> anyhow::Result<Value> {
        let path = file_path(text_document_uri(&params)?)?;
        let document = self.document(&path)?;
        let tab_size = params
            .get("options")
            .and_then(|options| options.get("tabSize"))
            .and_then(Value::as_u64)
            .and_then(|size| usize::try_from(size).ok())
            .unwrap_or(4);
        let formatted = format_source(document.text(), tab_size);
        if formatted == document.text() {
            return Ok(json!([]));
        }
        if range_only {
            let requested = params
                .get("range")
                .context("rangeFormatting omitted range")?;
            let start = parse_position(
                requested
                    .get("start")
                    .context("format range omitted start")?,
            )?;
            let end = parse_position(requested.get("end").context("format range omitted end")?)?;
            let last_line = if end.character == 0 && end.line > start.line {
                end.line - 1
            } else {
                end.line
            };
            let original_range = complete_line_range(document, start.line, last_line)?;
            let formatted_index = husk_analysis::LineIndex::new(&formatted);
            let formatted_start = formatted_index
                .byte_offset(
                    &formatted,
                    AnalysisPosition {
                        line: start.line,
                        character: 0,
                    },
                )
                .context("formatted range starts outside document")?;
            let formatted_end = formatted_line_end(&formatted, &formatted_index, last_line)?;
            return Ok(json!([{
                "range": self.range(document, &original_range),
                "newText": &formatted[formatted_start..formatted_end]
            }]));
        }
        Ok(json!([{
            "range": {
                "start": {"line": 0, "character": 0},
                "end": to_lsp_position(
                    document.position_range(&(document.text().len()..document.text().len())).end
                )
            },
            "newText": formatted
        }]))
    }

    fn prepare_call_hierarchy(&self, params: Value) -> anyhow::Result<Value> {
        let (path, byte) = self.document_position(&params)?;
        let Some((document, symbol)) = self.symbol_at(&path, byte) else {
            return Ok(json!([]));
        };
        if !matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method) {
            return Ok(json!([]));
        }
        Ok(Value::Array(vec![
            self.call_hierarchy_item(document, symbol)?,
        ]))
    }

    fn incoming_calls(&self, params: Value) -> anyhow::Result<Value> {
        let target_id = call_hierarchy_symbol_id(&params)?;
        let workspace = self.workspace()?;
        let Some((_, target)) = workspace.definition(&target_id) else {
            return Ok(json!([]));
        };
        let mut calls = Vec::new();
        for document in workspace.documents() {
            if is_dependency_stub(document.path()) {
                continue;
            }
            for caller in document
                .symbols()
                .iter()
                .filter(|symbol| matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method))
            {
                let mut ranges = call_occurrences(document.text(), &caller.full_span, &target.name);
                ranges.retain(|range| {
                    workspace
                        .symbol_at(document.path(), range.start)
                        .is_none_or(|resolved| resolved.id == target.id)
                });
                if ranges.is_empty() {
                    continue;
                }
                calls.push(json!({
                    "from": self.call_hierarchy_item(document, caller)?,
                    "fromRanges": ranges
                        .iter()
                        .map(|range| self.range(document, range))
                        .collect::<Vec<_>>()
                }));
            }
        }
        Ok(Value::Array(calls))
    }

    fn outgoing_calls(&self, params: Value) -> anyhow::Result<Value> {
        let caller_id = call_hierarchy_symbol_id(&params)?;
        let workspace = self.workspace()?;
        let Some((caller_document, caller)) = workspace.definition(&caller_id) else {
            return Ok(json!([]));
        };
        let mut calls = Vec::new();
        for target_document in workspace.documents() {
            for target in target_document
                .symbols()
                .iter()
                .filter(|symbol| matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method))
            {
                let mut ranges =
                    call_occurrences(caller_document.text(), &caller.full_span, &target.name);
                ranges.retain(|range| {
                    workspace
                        .symbol_at(caller_document.path(), range.start)
                        .is_none_or(|resolved| resolved.id == target.id)
                });
                if ranges.is_empty() {
                    continue;
                }
                calls.push(json!({
                    "to": self.call_hierarchy_item(target_document, target)?,
                    "fromRanges": ranges
                        .iter()
                        .map(|range| self.range(caller_document, range))
                        .collect::<Vec<_>>()
                }));
            }
        }
        Ok(Value::Array(calls))
    }

    fn call_hierarchy_item(&self, document: &Document, symbol: &Symbol) -> anyhow::Result<Value> {
        Ok(json!({
            "name": symbol.name,
            "kind": lsp_symbol_kind(symbol.kind),
            "uri": file_uri(document.path())?,
            "range": self.range(document, &symbol.full_span),
            "selectionRange": self.range(document, &symbol.span),
            "detail": symbol.detail,
            "data": {"symbolId": symbol.id.0}
        }))
    }

    fn apply_changes(
        &mut self,
        path: &Path,
        version: i32,
        changes: &[Value],
    ) -> anyhow::Result<()> {
        let current = self.document(path)?;
        if version <= current.version() {
            return Ok(());
        }
        let mut text = current.text().to_string();
        for change in changes {
            let replacement = required_str(change, "text")?;
            if let Some(range) = change.get("range") {
                let start =
                    parse_position(range.get("start").context("change range omitted start")?)?;
                let end = parse_position(range.get("end").context("change range omitted end")?)?;
                let index = husk_analysis::LineIndex::new(&text);
                let start = index
                    .byte_offset(&text, start)
                    .context("change start splits a UTF-16 scalar")?;
                let end = index
                    .byte_offset(&text, end)
                    .context("change end splits a UTF-16 scalar")?;
                anyhow::ensure!(start <= end, "change range is reversed");
                text.replace_range(start..end, replacement);
            } else {
                text.clear();
                text.push_str(replacement);
            }
        }
        self.workspace_mut()?
            .update(path, version, Arc::<str>::from(text))?;
        Ok(())
    }

    fn publish_diagnostics(&self, path: &Path) -> anyhow::Result<Value> {
        Ok(notification(
            "textDocument/publishDiagnostics",
            json!({
                "uri": file_uri(path)?,
                "version": self.document(path)?.version(),
                "diagnostics": self.diagnostics_for_path(path)?
            }),
        ))
    }

    fn diagnostics_for_path(&self, path: &Path) -> anyhow::Result<Vec<Diagnostic>> {
        let document = self.document(path)?;
        let mut diagnostics = document
            .diagnostics()
            .iter()
            .map(|diagnostic| to_lsp_diagnostic(document, diagnostic))
            .collect::<Vec<_>>();
        if !is_dependency_stub(path) {
            diagnostics.extend(
                self.workspace()?
                    .package_diagnostics()
                    .iter()
                    .map(|diagnostic| to_lsp_diagnostic(document, diagnostic)),
            );
            diagnostics.extend(
                self.dependency_diagnostics
                    .iter()
                    .map(|message| Diagnostic {
                        range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                        severity: Some(DiagnosticSeverity::WARNING),
                        code: Some(NumberOrString::String("HUSK-DEP0001".to_string())),
                        code_description: None,
                        source: Some("husk-lsp".to_string()),
                        message: message.clone(),
                        related_information: None,
                        tags: None,
                        data: None,
                    }),
            );
        }
        Ok(diagnostics)
    }

    fn document_position(&self, params: &Value) -> anyhow::Result<(PathBuf, usize)> {
        let path = file_path(text_document_uri(params)?)?;
        let position = parse_position(params.get("position").context("request omitted position")?)?;
        let byte = self
            .document(&path)?
            .byte_offset(position)
            .context("position splits a UTF-16 scalar or is outside the document")?;
        Ok((path, byte))
    }

    fn symbol_at(&self, path: &Path, byte: usize) -> Option<(&Document, &Symbol)> {
        let workspace = self.workspace.as_ref()?;
        if let Some(symbol) = workspace.symbol_at(path, byte) {
            return workspace.definition(&symbol.id);
        }
        let document = workspace.document(path)?;
        let word = word_at(document.text(), byte)?;
        let qualified =
            qualifier_before(document.text(), byte).map(|qualifier| format!("{qualifier}::{word}"));
        qualified
            .as_deref()
            .and_then(|qualified| workspace.symbol_named(qualified))
            .or_else(|| workspace.symbol_named(word))
    }

    fn textual_references(
        &self,
        name: &str,
        include_declaration: bool,
    ) -> anyhow::Result<Vec<Value>> {
        let mut locations = Vec::new();
        for document in self.workspace()?.documents() {
            if is_dependency_stub(document.path()) && !include_declaration {
                continue;
            }
            for range in word_occurrences(document.text(), name) {
                locations.push(location(document, &range)?);
            }
        }
        Ok(locations)
    }

    fn workspace(&self) -> anyhow::Result<&Workspace> {
        self.workspace
            .as_ref()
            .context("Husk language server is not initialized")
    }

    fn workspace_mut(&mut self) -> anyhow::Result<&mut Workspace> {
        self.workspace
            .as_mut()
            .context("Husk language server is not initialized")
    }

    fn document(&self, path: &Path) -> anyhow::Result<&Document> {
        self.workspace()?
            .document(path)
            .with_context(|| format!("Husk document `{}` is not indexed", path.display()))
    }

    fn range(&self, document: &Document, range: &std::ops::Range<usize>) -> Value {
        let range = document.position_range(range);
        json!({
            "start": to_lsp_position(range.start),
            "end": to_lsp_position(range.end)
        })
    }
}

fn workspace_root(params: &Value) -> anyhow::Result<PathBuf> {
    if let Some(uri) = params
        .get("workspaceFolders")
        .and_then(Value::as_array)
        .and_then(|folders| folders.first())
        .and_then(|folder| folder.get("uri"))
        .and_then(Value::as_str)
    {
        return file_path(uri);
    }
    if let Some(uri) = params.get("rootUri").and_then(Value::as_str) {
        return file_path(uri);
    }
    if let Some(path) = params.get("rootPath").and_then(Value::as_str) {
        return Ok(PathBuf::from(path));
    }
    std::env::current_dir().context("resolve default LSP workspace")
}

fn parse_profile(profile: &str) -> anyhow::Result<SemanticProfile> {
    match profile {
        "native" => Ok(SemanticProfile::Native),
        "legacyJavaScript" | "legacy_javascript" | "red" => Ok(SemanticProfile::LegacyJavaScript),
        _ => anyhow::bail!("unknown Husk semantic profile `{profile}`"),
    }
}

fn text_document_uri(params: &Value) -> anyhow::Result<&str> {
    params
        .get("textDocument")
        .and_then(|document| document.get("uri"))
        .and_then(Value::as_str)
        .context("request omitted textDocument.uri")
}

fn required_str<'a>(object: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    object
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("request omitted string `{key}`"))
}

fn parse_position(value: &Value) -> anyhow::Result<AnalysisPosition> {
    let line = value
        .get("line")
        .and_then(Value::as_u64)
        .and_then(|line| u32::try_from(line).ok())
        .context("position omitted a valid line")?;
    let character = value
        .get("character")
        .and_then(Value::as_u64)
        .and_then(|character| u32::try_from(character).ok())
        .context("position omitted a valid character")?;
    Ok(AnalysisPosition { line, character })
}

fn to_lsp_position(position: AnalysisPosition) -> Value {
    json!({"line": position.line, "character": position.character})
}

fn to_lsp_diagnostic(document: &Document, diagnostic: &AnalysisDiagnostic) -> Diagnostic {
    let range = document.position_range(&diagnostic.span);
    Diagnostic {
        range: Range::new(
            Position::new(range.start.line, range.start.character),
            Position::new(range.end.line, range.end.character),
        ),
        severity: Some(match diagnostic.severity {
            AnalysisDiagnosticSeverity::Error => DiagnosticSeverity::ERROR,
            AnalysisDiagnosticSeverity::Warning => DiagnosticSeverity::WARNING,
            AnalysisDiagnosticSeverity::Information => DiagnosticSeverity::INFORMATION,
        }),
        code: Some(NumberOrString::String(diagnostic.code.clone())),
        code_description: None,
        source: Some(diagnostic.source.to_string()),
        message: diagnostic.message.clone(),
        related_information: None,
        tags: None,
        data: None,
    }
}

fn completion_item(symbol: &Symbol) -> Value {
    let function = matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method);
    let insert_text = function.then(|| format!("{}(${{1}})", symbol.name));
    json!({
        "label": symbol.name,
        "kind": lsp_completion_kind(symbol.kind),
        "detail": symbol.detail,
        "documentation": symbol.documentation.as_ref().map(|documentation| json!({
            "kind": "markdown",
            "value": documentation
        })),
        "insertText": insert_text,
        "insertTextFormat": if function { 2 } else { 1 },
        "filterText": symbol.name,
        "sortText": format!("1-{}", symbol.name),
        "data": {"symbolId": symbol.id.0}
    })
}

fn lsp_completion_kind(kind: SymbolKind) -> u32 {
    match kind {
        SymbolKind::Function => 3,
        SymbolKind::Method => 2,
        SymbolKind::Struct => 22,
        SymbolKind::Enum => 13,
        SymbolKind::Variant => 20,
        SymbolKind::Field | SymbolKind::Property => 5,
        SymbolKind::Module => 9,
        SymbolKind::Variable | SymbolKind::Parameter => 6,
        SymbolKind::Constant => 21,
        SymbolKind::TypeParameter | SymbolKind::TypeAlias | SymbolKind::Trait => 25,
    }
}

fn lsp_symbol_kind(kind: SymbolKind) -> u32 {
    match kind {
        SymbolKind::Module => 2,
        SymbolKind::Function => 12,
        SymbolKind::Method => 6,
        SymbolKind::Struct => 23,
        SymbolKind::Field => 8,
        SymbolKind::Enum => 10,
        SymbolKind::Variant => 22,
        SymbolKind::TypeAlias => 5,
        SymbolKind::Trait => 11,
        SymbolKind::Variable | SymbolKind::Parameter => 13,
        SymbolKind::TypeParameter => 26,
        SymbolKind::Property => 7,
        SymbolKind::Constant => 14,
    }
}

fn semantic_token_kind(kind: SymbolKind) -> u32 {
    match kind {
        SymbolKind::Module => 0,
        SymbolKind::TypeAlias | SymbolKind::Trait => 1,
        SymbolKind::Enum => 2,
        SymbolKind::Struct => 3,
        SymbolKind::TypeParameter => 4,
        SymbolKind::Parameter => 5,
        SymbolKind::Variable | SymbolKind::Constant => 6,
        SymbolKind::Field | SymbolKind::Property => 7,
        SymbolKind::Variant => 8,
        SymbolKind::Function => 9,
        SymbolKind::Method => 10,
    }
}

fn location(document: &Document, range: &std::ops::Range<usize>) -> anyhow::Result<Value> {
    let range = document.position_range(range);
    Ok(json!({
        "uri": file_uri(document.path())?,
        "range": {
            "start": to_lsp_position(range.start),
            "end": to_lsp_position(range.end)
        }
    }))
}

fn completion_context(source: &str, byte: usize) -> (String, Option<String>) {
    let range = word_range(source, byte).unwrap_or(byte..byte);
    let prefix = source[range.start..byte.min(range.end)].to_string();
    let qualifier = qualifier_before(source, range.start).map(ToString::to_string);
    (prefix, qualifier)
}

fn qualifier_before(source: &str, byte: usize) -> Option<&str> {
    let prefix = source[..byte.min(source.len())].trim_end();
    let before_separator = prefix.strip_suffix("::")?;
    let start = before_separator
        .char_indices()
        .rev()
        .find_map(|(index, character)| {
            (!character.is_ascii_alphanumeric() && character != '_' && character != ':')
                .then_some(index + character.len_utf8())
        })
        .unwrap_or(0);
    let qualifier = before_separator[start..].trim_matches(':');
    (!qualifier.is_empty()).then_some(qualifier)
}

fn word_at(source: &str, byte: usize) -> Option<&str> {
    let range = word_range(source, byte)?;
    source.get(range)
}

fn word_range(source: &str, byte: usize) -> Option<std::ops::Range<usize>> {
    let byte = byte.min(source.len());
    let mut start = byte;
    while start > 0 {
        let character = source[..start].chars().next_back()?;
        if !character.is_ascii_alphanumeric() && character != '_' {
            break;
        }
        start -= character.len_utf8();
    }
    let mut end = byte;
    while end < source.len() {
        let character = source[end..].chars().next()?;
        if !character.is_ascii_alphanumeric() && character != '_' {
            break;
        }
        end += character.len_utf8();
    }
    (start < end).then_some(start..end)
}

fn word_occurrences(source: &str, name: &str) -> Vec<std::ops::Range<usize>> {
    source
        .match_indices(name)
        .filter_map(|(start, _)| {
            let end = start + name.len();
            let left = source[..start].chars().next_back();
            let right = source[end..].chars().next();
            let boundary = |character: Option<char>| {
                character
                    .is_none_or(|character| !character.is_ascii_alphanumeric() && character != '_')
            };
            (boundary(left) && boundary(right)).then_some(start..end)
        })
        .collect()
}

fn line_range(source: &str, byte: usize) -> std::ops::Range<usize> {
    let byte = byte.min(source.len());
    let start = source[..byte].rfind('\n').map_or(0, |index| index + 1);
    let end = source[byte..]
        .find('\n')
        .map_or(source.len(), |index| byte + index);
    start..end
}

fn complete_line_range(
    document: &Document,
    start_line: u32,
    end_line: u32,
) -> anyhow::Result<std::ops::Range<usize>> {
    let start = document
        .byte_offset(AnalysisPosition {
            line: start_line,
            character: 0,
        })
        .context("format range starts outside document")?;
    let end = formatted_line_end(document.text(), document.line_index(), end_line)?;
    Ok(start..end)
}

fn formatted_line_end(
    source: &str,
    index: &husk_analysis::LineIndex,
    line: u32,
) -> anyhow::Result<usize> {
    index
        .byte_offset(
            source,
            AnalysisPosition {
                line: line.saturating_add(1),
                character: 0,
            },
        )
        .or_else(|| {
            let last = index.position(source, source.len());
            (line == last.line).then_some(source.len())
        })
        .context("format range ends outside document")
}

fn call_context(source: &str, byte: usize) -> Option<(String, usize)> {
    let prefix = &source[..byte.min(source.len())];
    let mut open_parens = Vec::new();
    for token in Lexer::new(prefix) {
        match token.kind {
            TokenKind::LParen => open_parens.push(token.span.range.start),
            TokenKind::RParen => {
                open_parens.pop();
            }
            _ => {}
        }
    }
    let open = open_parens.pop()?;
    let name_end = open;
    let name_range = word_range(source, name_end)?;
    if name_range.end != name_end {
        return None;
    }
    let arguments = &prefix[open + 1..];
    let mut depth = 0usize;
    let mut active = 0usize;
    for token in Lexer::new(arguments) {
        match token.kind {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                depth = depth.saturating_add(1);
            }
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                depth = depth.saturating_sub(1);
            }
            TokenKind::Comma if depth == 0 => active = active.saturating_add(1),
            _ => {}
        }
    }
    Some((source[name_range].to_string(), active))
}

fn call_hierarchy_symbol_id(params: &Value) -> anyhow::Result<SymbolId> {
    params
        .pointer("/item/data/symbolId")
        .and_then(Value::as_str)
        .map(|id| SymbolId(id.to_string()))
        .context("call hierarchy item omitted its Husk symbol identity")
}

fn call_occurrences(
    source: &str,
    scope: &std::ops::Range<usize>,
    name: &str,
) -> Vec<std::ops::Range<usize>> {
    let start = scope.start.min(source.len());
    let end = scope.end.min(source.len());
    if start >= end {
        return Vec::new();
    }
    word_occurrences(&source[start..end], name)
        .into_iter()
        .filter_map(|range| {
            let range = start + range.start..start + range.end;
            let before = source[..range.start].trim_end();
            let declaration = before.strip_suffix("fn").is_some_and(|prefix| {
                prefix
                    .chars()
                    .next_back()
                    .is_none_or(|character| !character.is_ascii_alphanumeric() && character != '_')
            });
            let after = source[range.end..end].trim_start();
            (!declaration && (after.starts_with('(') || after.starts_with("::<"))).then_some(range)
        })
        .collect()
}

fn signature_parameters(signature: &str) -> Vec<String> {
    let Some((_, after_open)) = signature.split_once('(') else {
        return Vec::new();
    };
    let Some((parameters, _)) = after_open.rsplit_once(')') else {
        return Vec::new();
    };
    parameters
        .split(',')
        .map(str::trim)
        .filter(|parameter| !parameter.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn organize_imports(document: &Document) -> Option<(std::ops::Range<usize>, String)> {
    let mut imports = document
        .syntax()
        .items
        .iter()
        .filter_map(|item| {
            matches!(item.kind, ItemKind::Use { .. }).then_some(item.span.range.clone())
        })
        .collect::<Vec<_>>();
    if imports.len() < 2 {
        return None;
    }
    imports.sort_by_key(|range| range.start);
    let start = imports.first()?.start;
    let end = imports.last()?.end;
    let mut lines = imports
        .iter()
        .filter_map(|range| document.text().get(range.clone()))
        .map(str::trim)
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let original = lines.clone();
    lines.sort();
    lines.dedup();
    if lines == original {
        return None;
    }
    Some((start..end, format!("{}\n", lines.join("\n"))))
}

fn is_dependency_stub(path: &Path) -> bool {
    path.components()
        .collect::<Vec<_>>()
        .windows(2)
        .any(|components| {
            components[0].as_os_str() == ".husk" && components[1].as_os_str() == "lsp"
        })
}

fn deduplicate_locations(locations: &mut Vec<Value>) {
    let mut seen = HashSet::new();
    locations.retain(|location| {
        serde_json::to_string(location).is_ok_and(|location| seen.insert(location))
    });
}

fn request_key(id: &Value) -> String {
    serde_json::to_string(id).unwrap_or_else(|_| "null".to_string())
}

fn success_response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_response(id: Value, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message}
    })
}

fn notification(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn semicolon_quickfix_inserts_before_the_unexpected_token_and_rejects_stale_diagnostics() {
        for (source, preceding) in [
            (
                "fn main() {\n    let score = 42\n    std::println(\"Score updated\");\n}\n",
                "42",
            ),
            (
                "fn main() {\n    let message = \"😀\" // keep this comment\n    std::println(message);\n}\n",
                "\"😀\"",
            ),
            ("fn main() { let score = 42 }\n", "42"),
        ] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("main.hk");
            fs::write(&path, source).unwrap();
            let uri = file_uri(&path).unwrap();
            let mut server = Server::new(ServerOptions::default());
            server.handle(json!({"jsonrpc":"2.0", "id":1, "method":"initialize",
                "params":{"rootUri":file_uri(root.path()).unwrap()}}));
            let published = server.handle(json!({"jsonrpc":"2.0", "method":"textDocument/didOpen",
                "params":{"textDocument":{"uri":uri,"languageId":"husk","version":1,"text":source}}}));
            let diagnostic = published[0]["params"]["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .find(|diagnostic| {
                    diagnostic["code"] == "HUSK-P0001"
                        && diagnostic["message"]
                            .as_str()
                            .is_some_and(|message| message.starts_with("expected `;`"))
                })
                .unwrap()
                .clone();
            let params = json!({"textDocument":{"uri":uri},"range":diagnostic["range"],
                "context":{"diagnostics":[diagnostic]}});
            let actions = server.code_actions(params.clone()).unwrap();
            let action = actions
                .as_array()
                .unwrap()
                .iter()
                .find(|action| action["title"] == "Insert missing semicolon")
                .unwrap();
            let changes = action["edit"]["changes"].as_object().unwrap();
            assert_eq!(changes.len(), 1);
            let (target, edits) = changes.iter().next().unwrap();
            assert_eq!(
                file_path(target).unwrap().canonicalize().unwrap(),
                path.canonicalize().unwrap()
            );
            let edit = &edits[0];
            assert_eq!(edit["range"]["start"], edit["range"]["end"]);
            let position = parse_position(&edit["range"]["start"]).unwrap();
            let byte = server
                .document(&path)
                .unwrap()
                .byte_offset(position)
                .unwrap();
            assert_eq!(
                byte,
                source.find(preceding).unwrap() + preceding.len(),
                "{source}"
            );
            let mut fixed = source.to_owned();
            fixed.insert(byte, ';');
            let published = server.handle(json!({"jsonrpc":"2.0","method":"textDocument/didChange",
                "params":{"textDocument":{"uri":uri,"version":2},"contentChanges":[{"text":fixed}]}}));
            assert!(
                published[0]["params"]["diagnostics"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|diagnostic| diagnostic["code"] != "HUSK-P0001"),
                "{published:?}"
            );
            assert!(
                server
                    .code_actions(params)
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|action| action["title"] != "Insert missing semicolon")
            );
        }
    }

    #[test]
    fn completion_context_handles_flat_external_paths() {
        assert_eq!(
            completion_context("serde_json::from", "serde_json::from".len()),
            ("from".to_string(), Some("serde_json".to_string()))
        );
    }

    #[test]
    fn word_search_requires_identifier_boundaries() {
        assert_eq!(
            word_occurrences("value value2 prevalue value", "value"),
            vec![0..5, 22..27]
        );
    }

    #[test]
    fn call_context_counts_top_level_arguments() {
        assert_eq!(
            call_context("build(1, nested(2, 3), ", 23),
            Some(("build".to_string(), 2))
        );
    }

    #[test]
    fn json_rpc_session_serves_editor_features_from_an_unsaved_revision() {
        let root = tempfile::tempdir().expect("create LSP workspace");
        let path = root.path().join("main.hk");
        fs::write(&path, "fn stale() {}\n").expect("write on-disk fixture");
        let root_uri = file_uri(root.path()).expect("encode workspace URI");
        let uri = file_uri(&path).expect("encode document URI");
        let source = "fn helper(value: i32) -> i32 { value }\nfn main() { hel }\n";
        let mut server = Server::new(ServerOptions::default());

        let initialized = server.handle(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"rootUri": root_uri}
        }));
        assert_eq!(
            initialized[0].pointer("/result/capabilities/positionEncoding"),
            Some(&json!("utf-16"))
        );

        let published = server.handle(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "husk",
                    "version": 7,
                    "text": source
                }
            }
        }));
        assert_eq!(published[0].pointer("/params/version"), Some(&json!(7)));

        let completion = server.handle(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/completion",
            "params": {
                "textDocument": {"uri": uri},
                "position": {"line": 1, "character": 15}
            }
        }));
        assert!(
            completion[0]
                .pointer("/result/items")
                .and_then(Value::as_array)
                .is_some_and(|items| {
                    items
                        .iter()
                        .any(|item| item.get("label") == Some(&json!("helper")))
                })
        );

        let hover = server.handle(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "textDocument/hover",
            "params": {
                "textDocument": {"uri": uri},
                "position": {"line": 0, "character": 4}
            }
        }));
        assert!(
            hover[0]
                .pointer("/result/contents/value")
                .and_then(Value::as_str)
                .is_some_and(|value| value.contains("fn helper(value: i32) -> i32"))
        );

        assert_eq!(
            fs::read_to_string(&path).expect("read disk fixture"),
            "fn stale() {}\n",
            "the LSP overlay must not modify the file"
        );
    }
}
