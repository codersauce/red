//! Routing and lazy lifecycle management for language servers across workspace documents.
//!
//! [`LspManager`] matches configured document selectors, associates each opened document
//! with every matching client key, starts clients on demand, and forwards each feature
//! to one capable process. A document is opened once per managed lifecycle; later
//! changes reuse each client's independent version stream.
//!
//! Unsupported files are valid no-op targets for most editor operations. Process or
//! protocol failures remain errors so the editor can surface them without silently
//! pretending code intelligence succeeded.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::Future,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::Instant,
};

use path_absolutize::Absolutize;
use serde::Deserialize;
use serde_json::Value;

use crate::{
    config::{LanguageServerConfig, LspConfig},
    editor::perf,
    highlighter::normalized_extension,
    log,
};

use super::{
    client::{
        outbound_message_size, MAX_PENDING_LSP_BYTES, MAX_PENDING_LSP_MESSAGES, REQUEST_TIMEOUT,
    },
    file_path, file_uri, CodeActionProviderCapability, DeclarationProviderCapability,
    DefinitionProviderCapability, DocumentFormattingProviderCapability,
    DocumentHighlightProviderCapability, DocumentRangeFormattingProviderCapability,
    DocumentSymbolProviderCapability, FoldingRangeProviderCapability, HoverProviderCapability,
    ImplementationProviderCapability, InboundMessage, InlayHintProviderCapability, LspClient,
    LspError, LspRequestSource, OutboundMessage, ParsedNotification, RealLspClient,
    ReferencesProviderCapability, RenameProviderCapability, Request, ServerCapabilities,
    ServerRequest, TypeDefinitionProviderCapability, WorkspaceSymbolProviderCapability,
};

#[derive(Debug, Clone, PartialEq, Eq)]
/// Resolved language-server routing information for one file.
pub struct DocumentInfo {
    /// Absolute document path.
    pub path: PathBuf,
    /// Percent-encoded `file:` URI sent to the server.
    pub uri: String,
    /// Language identifier selected by the configured document selector.
    pub language_id: String,
    /// Workspace root found from configured root markers.
    pub workspace_root: PathBuf,
    /// Configuration key of the server that owns this document.
    pub server_name: String,
}

#[derive(Clone)]
struct DocumentSelector {
    server_name: String,
    language_id: String,
}

struct PendingDocumentRequest {
    file: String,
    target: Option<String>,
    request: Request,
    force: bool,
    bytes: usize,
}

fn supports_method(capabilities: &ServerCapabilities, method: &str) -> bool {
    macro_rules! provider_enabled {
        ($field:expr, $capability:ident) => {
            matches!(
                $field,
                Some($capability::Simple(true) | $capability::Options(_))
            )
        };
    }

    match method {
        "textDocument/completion" => capabilities.completion_provider.is_some(),
        "completionItem/resolve" => {
            capabilities
                .completion_provider
                .as_ref()
                .and_then(|provider| provider.resolve_provider)
                == Some(true)
        }
        "textDocument/hover" => provider_enabled!(
            capabilities.hover_provider.as_ref(),
            HoverProviderCapability
        ),
        "textDocument/signatureHelp" => capabilities.signature_help_provider.is_some(),
        "textDocument/definition" => provider_enabled!(
            capabilities.definition_provider.as_ref(),
            DefinitionProviderCapability
        ),
        "textDocument/typeDefinition" => provider_enabled!(
            capabilities.type_definition_provider.as_ref(),
            TypeDefinitionProviderCapability
        ),
        "textDocument/implementation" => provider_enabled!(
            capabilities.implementation_provider.as_ref(),
            ImplementationProviderCapability
        ),
        "textDocument/declaration" => provider_enabled!(
            capabilities.declaration_provider.as_ref(),
            DeclarationProviderCapability
        ),
        "textDocument/references" => provider_enabled!(
            capabilities.references_provider.as_ref(),
            ReferencesProviderCapability
        ),
        "textDocument/documentHighlight" => provider_enabled!(
            capabilities.document_highlight_provider.as_ref(),
            DocumentHighlightProviderCapability
        ),
        "textDocument/documentSymbol" => provider_enabled!(
            capabilities.document_symbol_provider.as_ref(),
            DocumentSymbolProviderCapability
        ),
        "workspace/symbol" => provider_enabled!(
            capabilities.workspace_symbol_provider.as_ref(),
            WorkspaceSymbolProviderCapability
        ),
        "textDocument/codeAction" => provider_enabled!(
            capabilities.code_action_provider.as_ref(),
            CodeActionProviderCapability
        ),
        "textDocument/formatting" => provider_enabled!(
            capabilities.document_formatting_provider.as_ref(),
            DocumentFormattingProviderCapability
        ),
        "textDocument/rangeFormatting" => provider_enabled!(
            capabilities.document_range_formatting_provider.as_ref(),
            DocumentRangeFormattingProviderCapability
        ),
        "textDocument/rename" => provider_enabled!(
            capabilities.rename_provider.as_ref(),
            RenameProviderCapability
        ),
        "textDocument/prepareRename" => matches!(
            capabilities.rename_provider.as_ref(),
            Some(RenameProviderCapability::Options(options))
                if options.prepare_provider == Some(true)
        ),
        "textDocument/foldingRange" => provider_enabled!(
            capabilities.folding_range_provider.as_ref(),
            FoldingRangeProviderCapability
        ),
        "textDocument/prepareCallHierarchy" => capabilities.call_hierarchy_provider == Some(true),
        "textDocument/semanticTokens/full" => capabilities
            .semantic_tokens_provider
            .as_ref()
            .and_then(|provider| provider.full.as_ref())
            .is_some_and(|full| !matches!(full, super::SemanticTokensFullCapability::Bool(false))),
        "textDocument/inlayHint" => provider_enabled!(
            capabilities.inlay_hint_provider.as_ref(),
            InlayHintProviderCapability
        ),
        "textDocument/diagnostic" => capabilities.diagnostic_provider.is_some(),
        "textDocument/selectionRange" => capabilities.selection_range_provider == Some(true),
        "textDocument/codeLens" => capabilities.code_lens_provider.is_some(),
        "textDocument/onTypeFormatting" => {
            capabilities.document_on_type_formatting_provider.is_some()
        }
        "workspace/executeCommand" => capabilities.execute_command_provider.is_some(),
        // Red does not yet model these advertised server capabilities. Preserve
        // their existing request behavior while routing modeled features strictly.
        "textDocument/documentLink" | "textDocument/documentColor" => true,
        // Custom methods are used by agent and plugin callers. They remain routable;
        // known editor methods above are capability-gated.
        _ => true,
    }
}

#[derive(Deserialize)]
struct CargoManifest {
    #[serde(default)]
    package: Option<CargoPackage>,
    #[serde(default)]
    workspace: Option<CargoWorkspace>,
}

#[derive(Deserialize)]
struct CargoPackage {
    #[serde(default)]
    workspace: Option<String>,
}

#[derive(Deserialize)]
struct CargoWorkspace {
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

async fn complete_shutdowns<I, F>(shutdowns: I) -> Result<(), LspError>
where
    I: IntoIterator<Item = F>,
    F: Future<Output = Result<(), LspError>>,
{
    let results = futures::future::join_all(shutdowns).await;
    for result in results {
        result?;
    }
    Ok(())
}

/// Lazily starts and routes documents across configured language servers.
pub struct LspManager {
    config: LspConfig,
    document_selectors: HashMap<String, Vec<DocumentSelector>>,
    filename_selectors: HashMap<String, Vec<DocumentSelector>>,
    clients: HashMap<String, RealLspClient>,
    client_poll_order: Vec<String>,
    failed_clients: HashSet<String>,
    opened_documents: HashSet<String>,
    document_clients: HashMap<String, Vec<String>>,
    pending_requests: VecDeque<PendingDocumentRequest>,
    pending_request_bytes: usize,
    pending_outcomes: VecDeque<(InboundMessage, Option<String>)>,
    cargo_workspace_roots: Mutex<HashMap<PathBuf, Option<PathBuf>>>,
    next_client_poll: usize,
}

impl LspManager {
    /// Builds routing tables without starting any server processes.
    pub fn new(config: LspConfig) -> Self {
        let mut document_selectors = HashMap::new();
        let mut filename_selectors = HashMap::new();
        if config.enabled {
            let mut servers = config.servers.iter().collect::<Vec<_>>();
            servers.sort_unstable_by_key(|(name, _)| *name);
            for (server_name, server) in servers {
                for document in server.documents() {
                    for extension in &document.file_extensions {
                        let selectors = document_selectors
                            .entry(extension.trim_start_matches('.').to_ascii_lowercase())
                            .or_insert_with(Vec::new);
                        if !selectors
                            .iter()
                            .any(|selector: &DocumentSelector| selector.server_name == *server_name)
                        {
                            selectors.push(DocumentSelector {
                                server_name: server_name.clone(),
                                language_id: document.language_id.clone(),
                            });
                        }
                    }
                    for filename in document.filenames {
                        let selectors = filename_selectors.entry(filename).or_insert_with(Vec::new);
                        if !selectors
                            .iter()
                            .any(|selector: &DocumentSelector| selector.server_name == *server_name)
                        {
                            selectors.push(DocumentSelector {
                                server_name: server_name.clone(),
                                language_id: document.language_id.clone(),
                            });
                        }
                    }
                }
            }
        }

        Self {
            config,
            document_selectors,
            filename_selectors,
            clients: HashMap::new(),
            client_poll_order: Vec::new(),
            failed_clients: HashSet::new(),
            opened_documents: HashSet::new(),
            document_clients: HashMap::new(),
            pending_requests: VecDeque::new(),
            pending_request_bytes: 0,
            pending_outcomes: VecDeque::new(),
            cargo_workspace_roots: Mutex::new(HashMap::new()),
            next_client_poll: 0,
        }
    }

    /// Resolves a file to its first configured server for compatibility.
    ///
    /// Returns `None` when LSP is disabled, the extension has no selector, or
    /// the file cannot be normalized safely.
    pub fn resolve_document(&self, file: &str) -> Option<DocumentInfo> {
        self.resolve_documents(file).into_iter().next()
    }

    /// Resolves every server attachment for a file in stable configuration-name order.
    pub fn resolve_documents(&self, file: &str) -> Vec<DocumentInfo> {
        let _span = perf::PerfSpan::start("lsp:resolve_document");
        if !self.config.enabled {
            return Vec::new();
        }

        let filename = Path::new(file).file_name().and_then(|name| name.to_str());
        let selectors = filename
            .and_then(|name| self.filename_selectors.get(name))
            .or_else(|| {
                normalized_extension(file)
                    .and_then(|extension| self.document_selectors.get(&extension))
            });
        let Some(selectors) = selectors else {
            return Vec::new();
        };

        let path = Path::new(file);
        let path = if path.is_absolute() {
            match path.absolutize_from(path) {
                Ok(path) => path.into_owned(),
                Err(_) => return Vec::new(),
            }
        } else {
            match path.absolutize() {
                Ok(path) => path.into_owned(),
                Err(_) => return Vec::new(),
            }
        };
        let Ok(uri) = file_uri(&path) else {
            return Vec::new();
        };

        selectors
            .iter()
            .filter_map(|selector| {
                let server = self.config.servers.get(&selector.server_name)?;
                Some(DocumentInfo {
                    path: path.clone(),
                    uri: uri.clone(),
                    language_id: selector.language_id.clone(),
                    workspace_root: find_workspace_root(
                        &path,
                        server,
                        &selector.language_id,
                        &self.cargo_workspace_roots,
                    ),
                    server_name: selector.server_name.clone(),
                })
            })
            .collect()
    }

    async fn client_for_document(
        &mut self,
        document: &DocumentInfo,
    ) -> Result<Option<&mut RealLspClient>, LspError> {
        let key = client_key(document);
        if self.failed_clients.contains(&key) {
            return Ok(None);
        }

        if !self.clients.contains_key(&key) {
            let mut config = self
                .config
                .servers
                .get(&document.server_name)
                .cloned()
                .ok_or_else(|| {
                    LspError::ProtocolError(format!(
                        "missing LSP config for server {}",
                        document.server_name
                    ))
                })?;
            super::workspace_settings::apply_workspace_settings(
                &mut config,
                &document.workspace_root,
                &document.language_id,
            );
            super::workspace_settings::apply_fast_startup_defaults(
                &mut config,
                &document.language_id,
            );

            let mut client =
                match RealLspClient::start(config, document.workspace_root.clone()).await {
                    Ok(client) => client,
                    Err(err) => {
                        log!("[lsp] failed to start client {}: {}", key, err);
                        self.failed_clients.insert(key);
                        return Ok(None);
                    }
                };
            if let Err(err) = client.initialize().await {
                log!("[lsp] failed to initialize client {}: {}", key, err);
                self.failed_clients.insert(key);
                return Ok(None);
            }
            self.clients.insert(key.clone(), client);
            let index = self
                .client_poll_order
                .binary_search(&key)
                .unwrap_or_else(|index| index);
            self.client_poll_order.insert(index, key.clone());
        }

        Ok(self.clients.get_mut(&key))
    }

    fn remember_document_client(&mut self, file: &str, key: String) {
        let keys = self
            .document_clients
            .entry(document_route_key(file))
            .or_default();
        if !keys.contains(&key) {
            keys.push(key);
            keys.sort_unstable();
        }
    }

    fn candidate_keys_for_file(&self, file: &str) -> Vec<String> {
        if let Some(keys) = self.document_clients.get(&document_route_key(file)) {
            return keys.clone();
        }
        self.resolve_documents(file)
            .iter()
            .map(client_key)
            .collect()
    }

    fn ready_client_key(&self, file: &str, method: &str, force: bool) -> Option<String> {
        self.candidate_keys_for_file(file).into_iter().find(|key| {
            if self.failed_clients.contains(key) {
                return false;
            }
            self.clients.get(key).is_some_and(|client| {
                client.server_status_for_file(file) == "ready"
                    && (force
                        || client
                            .get_server_capabilities()
                            .is_some_and(|capabilities| supports_method(capabilities, method)))
            })
        })
    }

    fn has_starting_candidate(&self, file: &str) -> bool {
        self.candidate_keys_for_file(file).into_iter().any(|key| {
            self.clients
                .get(&key)
                .is_some_and(|client| client.server_status_for_file(file) == "starting")
        })
    }

    async fn start_clients_for_file(&mut self, file: &str) -> Result<(), LspError> {
        for document in self.resolve_documents(file) {
            self.client_for_document(&document).await?;
        }
        Ok(())
    }

    fn request_source(&self, key: &str, file: &str) -> Option<LspRequestSource> {
        Some(LspRequestSource {
            key: key.to_string(),
            instance: self.clients.get(key)?.server_instance_for_file(file)?,
        })
    }

    async fn dispatch_prepared_request(
        &mut self,
        key: &str,
        file: &str,
        mut request: Request,
        force: bool,
    ) -> Result<(), LspError> {
        request.source = self.request_source(key, file);
        let client = self.clients.get_mut(key).ok_or_else(|| {
            LspError::ProtocolError(format!(
                "language server disappeared before request {} was dispatched",
                request.id
            ))
        })?;
        client.send_prepared_request(request, force).await?;
        Ok(())
    }

    fn enqueue_request(
        &mut self,
        file: &str,
        target: Option<String>,
        request: Request,
        force: bool,
    ) -> Result<(), LspError> {
        let bytes = outbound_message_size(&OutboundMessage::Request(request.clone()));
        let total = self.pending_request_bytes.saturating_add(bytes);
        if self.pending_requests.len() >= MAX_PENDING_LSP_MESSAGES || total > MAX_PENDING_LSP_BYTES
        {
            return Err(LspError::ProtocolError(format!(
                "language-server routing did not settle before its pending queue exceeded {MAX_PENDING_LSP_MESSAGES} requests or {MAX_PENDING_LSP_BYTES} bytes"
            )));
        }
        self.pending_request_bytes = total;
        self.pending_requests.push_back(PendingDocumentRequest {
            file: document_route_key(file),
            target,
            request,
            force,
            bytes,
        });
        Ok(())
    }

    fn retire_pending_requests_for_file(&mut self, file: &str) {
        let file = document_route_key(file);
        let pending = self.pending_requests.len();
        for _ in 0..pending {
            let Some(request) = self.pending_requests.pop_front() else {
                break;
            };
            if request.file != file {
                self.pending_requests.push_back(request);
                continue;
            }
            self.pending_request_bytes = self.pending_request_bytes.saturating_sub(request.bytes);
            self.pending_outcomes.push_back((
                InboundMessage::RequestError {
                    id: request.request.id,
                    error: LspError::ProtocolError(
                        "document closed before the language-server request was dispatched"
                            .to_string(),
                    ),
                },
                Some(request.request.method),
            ));
        }
    }

    async fn route_prepared_request(
        &mut self,
        file: &str,
        request: Request,
        force: bool,
    ) -> Result<bool, LspError> {
        self.start_clients_for_file(file).await?;
        if let Some(key) = self.ready_client_key(file, &request.method, force) {
            self.dispatch_prepared_request(&key, file, request, force)
                .await?;
            return Ok(true);
        }
        if self.has_starting_candidate(file) {
            self.enqueue_request(file, None, request, force)?;
            return Ok(true);
        }
        let candidates = self.candidate_keys_for_file(file);
        if let Some(key) = candidates.iter().find(|key| {
            self.clients
                .get(*key)
                .is_some_and(|client| client.server_status_for_file(file) == "failed")
        }) {
            self.dispatch_prepared_request(key, file, request, force)
                .await?;
            return Ok(true);
        }
        if candidates
            .iter()
            .any(|key| self.failed_clients.contains(key))
        {
            return Err(LspError::ProtocolError(
                "language server unavailable".to_string(),
            ));
        }
        Ok(false)
    }

    async fn drain_pending_requests(&mut self) {
        let pending = self.pending_requests.len();
        for _ in 0..pending {
            let Some(queued) = self.pending_requests.pop_front() else {
                break;
            };
            self.pending_request_bytes = self.pending_request_bytes.saturating_sub(queued.bytes);
            let method = queued.request.method.clone();
            let id = queued.request.id;
            if Instant::now().duration_since(queued.request.timestamp) > REQUEST_TIMEOUT {
                self.pending_outcomes.push_back((
                    InboundMessage::RequestError {
                        id,
                        error: LspError::RequestTimeout(REQUEST_TIMEOUT),
                    },
                    Some(method),
                ));
                continue;
            }
            let key = queued.target.clone().filter(|key| {
                self.clients.get(key).is_some_and(|client| {
                    client.server_status_for_file(&queued.file) == "ready"
                        && (queued.force
                            || client
                                .get_server_capabilities()
                                .is_some_and(|caps| supports_method(caps, &method)))
                })
            });
            let key = key.or_else(|| {
                queued
                    .target
                    .is_none()
                    .then(|| self.ready_client_key(&queued.file, &method, queued.force))?
            });
            if let Some(key) = key {
                if let Err(error) = self
                    .dispatch_prepared_request(&key, &queued.file, queued.request, queued.force)
                    .await
                {
                    self.pending_outcomes
                        .push_back((InboundMessage::RequestError { id, error }, Some(method)));
                }
                continue;
            }
            let still_starting = queued.target.as_ref().map_or_else(
                || self.has_starting_candidate(&queued.file),
                |key| {
                    self.clients.get(key).is_some_and(|client| {
                        client.server_status_for_file(&queued.file) == "starting"
                    })
                },
            );
            if still_starting {
                self.pending_request_bytes =
                    self.pending_request_bytes.saturating_add(queued.bytes);
                self.pending_requests.push_back(queued);
                continue;
            }
            self.pending_outcomes.push_back((
                InboundMessage::RequestError {
                    id,
                    error: LspError::ProtocolError(format!(
                        "no language server supports {method} for {}",
                        queued.file
                    )),
                },
                Some(method),
            ));
        }
    }

    fn first_client_mut(&mut self) -> Option<&mut RealLspClient> {
        self.clients.values_mut().next()
    }
}

pub(crate) fn client_key(document: &DocumentInfo) -> String {
    format!(
        "{}:{}",
        document.server_name,
        document.workspace_root.display()
    )
}

fn client_source_from_key<'a>(key: &'a str, config: &LspConfig) -> (&'a str, &'a str) {
    let server_name = config
        .servers
        .keys()
        .filter(|server| {
            key.strip_prefix(server.as_str())
                .is_some_and(|workspace| workspace.starts_with(':'))
        })
        .max_by_key(|server| server.len());
    match server_name {
        Some(server) => (&key[..server.len()], &key[server.len() + 1..]),
        None => key.split_once(':').unwrap_or((key, "")),
    }
}

fn document_key(document: &DocumentInfo) -> String {
    format!("{}:{}", client_key(document), document.uri)
}

fn document_route_key(file: &str) -> String {
    Path::new(file)
        .absolutize()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| file.to_string())
}

fn find_workspace_root(
    path: &Path,
    server: &LanguageServerConfig,
    language_id: &str,
    cargo_workspace_roots: &Mutex<HashMap<PathBuf, Option<PathBuf>>>,
) -> PathBuf {
    let start = path.parent().unwrap_or(path);
    let marker_root = marker_workspace_root(start, server);

    if language_id == "rust" {
        if let Some(workspace_root) = cargo_workspace_root(start, server, cargo_workspace_roots) {
            return workspace_root;
        }
    }

    marker_root
}

fn marker_workspace_root(start: &Path, server: &LanguageServerConfig) -> PathBuf {
    for ancestor in start.ancestors() {
        if server
            .root_markers
            .iter()
            .any(|marker| ancestor.join(marker).exists())
        {
            return ancestor.to_path_buf();
        }
    }

    std::env::current_dir().unwrap_or_else(|_| start.to_path_buf())
}

fn cargo_workspace_root(
    start: &Path,
    server: &LanguageServerConfig,
    cache: &Mutex<HashMap<PathBuf, Option<PathBuf>>>,
) -> Option<PathBuf> {
    let manifest = start
        .ancestors()
        .map(|ancestor| ancestor.join("Cargo.toml"))
        .find(|candidate| candidate.is_file())?;
    let manifest = std::fs::canonicalize(&manifest).unwrap_or(manifest);

    if let Some(cached) = cache
        .lock()
        .ok()
        .and_then(|roots| roots.get(&manifest).cloned())
    {
        perf::increment("lsp:cargo_workspace_cache_hits", 1);
        return cached;
    }

    perf::increment("lsp:cargo_workspace_cache_misses", 1);
    let resolved = discover_cargo_workspace(&manifest, start).or_else(|| {
        perf::increment("lsp:cargo_workspace_process_fallbacks", 1);
        locate_cargo_workspace(&manifest, start, server)
    });
    if let Ok(mut roots) = cache.lock() {
        roots.insert(manifest, resolved.clone());
    }
    resolved
}

fn discover_cargo_workspace(manifest: &Path, document_directory: &Path) -> Option<PathBuf> {
    let _span = perf::PerfSpan::start("lsp:cargo_workspace_manifest_scan");
    let document_directory = std::fs::canonicalize(document_directory)
        .unwrap_or_else(|_| document_directory.to_path_buf());
    let member_manifest = read_cargo_manifest(manifest)?;
    if member_manifest
        .package
        .as_ref()
        .and_then(|package| package.workspace.as_ref())
        .is_some()
    {
        return None;
    }

    let member_root = manifest.parent()?;
    let repository_root = document_directory
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists());

    for ancestor in member_root.ancestors() {
        if repository_root.is_some_and(|repository| !ancestor.starts_with(repository)) {
            break;
        }

        let workspace_manifest = ancestor.join("Cargo.toml");
        if !workspace_manifest.is_file() {
            continue;
        }
        let parsed = if workspace_manifest == manifest {
            &member_manifest
        } else {
            &read_cargo_manifest(&workspace_manifest)?
        };
        let Some(workspace) = parsed.workspace.as_ref() else {
            continue;
        };
        if ancestor == member_root {
            return validated_cargo_workspace_root(ancestor, &document_directory);
        }

        let relative_member = member_root.strip_prefix(ancestor).ok()?;
        if workspace.exclude.iter().any(|excluded| {
            cargo_pattern_is_ambiguous(excluded) || relative_member == Path::new(excluded)
        }) {
            return None;
        }
        if workspace
            .members
            .iter()
            .any(|member| relative_member == Path::new(member))
        {
            return validated_cargo_workspace_root(ancestor, &document_directory);
        }
        return None;
    }

    validated_cargo_workspace_root(member_root, &document_directory)
}

fn read_cargo_manifest(path: &Path) -> Option<CargoManifest> {
    toml::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn cargo_pattern_is_ambiguous(pattern: &str) -> bool {
    pattern.contains(['*', '?', '[', '{'])
}

fn locate_cargo_workspace(
    manifest: &Path,
    document_directory: &Path,
    server: &LanguageServerConfig,
) -> Option<PathBuf> {
    let _span = perf::PerfSpan::start("lsp:cargo_locate_project");
    let mut command = cargo_command_for_server(server);
    let output = command
        .args([
            "locate-project",
            "--workspace",
            "--message-format",
            "plain",
            "--manifest-path",
        ])
        .arg(manifest)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let workspace_manifest = std::str::from_utf8(&output.stdout).ok()?.trim();
    if workspace_manifest.is_empty() {
        return None;
    }
    let workspace_manifest = PathBuf::from(workspace_manifest);
    if workspace_manifest
        .file_name()
        .and_then(|name| name.to_str())
        != Some("Cargo.toml")
        || !workspace_manifest.is_file()
    {
        return None;
    }
    validated_cargo_workspace_root(workspace_manifest.parent()?, document_directory)
}

fn validated_cargo_workspace_root(
    workspace_root: &Path,
    document_directory: &Path,
) -> Option<PathBuf> {
    let workspace_root = std::fs::canonicalize(workspace_root).ok()?;
    let document_directory = std::fs::canonicalize(document_directory)
        .unwrap_or_else(|_| document_directory.to_path_buf());
    if !document_directory.starts_with(&workspace_root) {
        return None;
    }

    let repository_root = document_directory
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .and_then(|root| std::fs::canonicalize(root).ok());
    if repository_root
        .as_ref()
        .is_some_and(|repository_root| !workspace_root.starts_with(repository_root))
    {
        return None;
    }

    Some(workspace_root)
}

fn cargo_command_for_server(server: &LanguageServerConfig) -> Command {
    let rustup = Path::new(&server.command)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "rustup" || name == "rustup.exe");
    if rustup
        && server
            .args
            .first()
            .is_some_and(|argument| argument == "run")
    {
        if let Some(toolchain) = server.args.get(1) {
            let mut command = Command::new(&server.command);
            command.args(["run", toolchain, "cargo"]);
            return command;
        }
    }
    Command::new("cargo")
}

#[async_trait::async_trait]
impl LspClient for LspManager {
    async fn reconfigure(&mut self, config: LspConfig) -> Result<Vec<String>, LspError> {
        let replacement = Self::new(config);
        let changed = self
            .config
            .servers
            .keys()
            .chain(replacement.config.servers.keys())
            .filter(|name| self.config.servers.get(*name) != replacement.config.servers.get(*name))
            .cloned()
            .collect::<HashSet<_>>();
        let disabled = self.config.enabled != replacement.config.enabled;
        let replaced_keys = self
            .clients
            .keys()
            .filter(|key| disabled || changed.contains(client_source_from_key(key, &self.config).0))
            .cloned()
            .collect::<Vec<_>>();
        let replaced_clients = replaced_keys.iter().cloned().collect::<HashSet<_>>();
        let mut affected = Vec::new();
        let mut retained_documents = HashMap::new();
        let mut removed_documents = HashSet::new();
        let mut closes = Vec::new();

        for (file, current_keys) in &self.document_clients {
            let previous = self.resolve_documents(file);
            let next = replacement.resolve_documents(file);
            let next_keys = next.iter().map(client_key).collect::<HashSet<_>>();
            let retained = current_keys
                .iter()
                .filter(|key| {
                    next_keys.contains(*key) && !replaced_clients.contains(*key) && !disabled
                })
                .cloned()
                .collect::<Vec<_>>();
            if disabled || previous != next || retained.len() != current_keys.len() {
                affected.push(file.clone());
            }
            for document in previous {
                let key = client_key(&document);
                if !next_keys.contains(&key) || replaced_clients.contains(&key) || disabled {
                    removed_documents.insert(document_key(&document));
                    if !replaced_clients.contains(&key) {
                        closes.push((file.clone(), key));
                    }
                }
            }
            if !retained.is_empty() {
                retained_documents.insert(file.clone(), retained);
            }
        }

        affected.sort_unstable();
        let affected_set = affected.iter().collect::<HashSet<_>>();
        let mut retained_requests = VecDeque::new();
        while let Some(pending) = self.pending_requests.pop_front() {
            if affected_set.contains(&pending.file) {
                self.pending_request_bytes =
                    self.pending_request_bytes.saturating_sub(pending.bytes);
                self.pending_outcomes.push_back((
                    InboundMessage::RequestError {
                        id: pending.request.id,
                        error: LspError::ProtocolError(
                            "language-server routing changed before the request was dispatched"
                                .to_string(),
                        ),
                    },
                    Some(pending.request.method),
                ));
            } else {
                retained_requests.push_back(pending);
            }
        }
        self.pending_requests = retained_requests;
        for (file, key) in closes {
            if let Some(client) = self.clients.get_mut(&key) {
                if let Err(error) = client.did_close(&file).await {
                    log!("[lsp] could not close rerouted document {file}: {error}");
                }
            }
        }
        for key in replaced_keys {
            if let Some(mut client) = self.clients.remove(&key) {
                if let Err(error) = client.shutdown().await {
                    log!("[lsp] could not shut down replaced client {key}: {error}");
                }
            }
        }
        self.failed_clients.retain(|key| {
            !disabled && !changed.contains(client_source_from_key(key, &self.config).0)
        });
        self.opened_documents.retain(|document| {
            !disabled
                && !removed_documents.contains(document)
                && !changed.contains(client_source_from_key(document, &self.config).0)
        });
        self.document_clients = retained_documents;
        self.config = replacement.config;
        self.document_selectors = replacement.document_selectors;
        self.filename_selectors = replacement.filename_selectors;
        self.cargo_workspace_roots = replacement.cargo_workspace_roots;
        self.client_poll_order
            .retain(|key| self.clients.contains_key(key));
        self.next_client_poll = 0;
        Ok(affected)
    }

    async fn initialize(&mut self) -> Result<(), LspError> {
        Ok(())
    }

    async fn did_open(&mut self, file: &str, contents: &str) -> Result<(), LspError> {
        let documents = self.resolve_documents(file);
        let mut first_error = None;
        for document in documents {
            let document_key = document_key(&document);
            let client_key = client_key(&document);
            if self.opened_documents.contains(&document_key) {
                self.remember_document_client(file, client_key);
                continue;
            }
            let result = match self.client_for_document(&document).await {
                Ok(Some(client)) => {
                    client
                        .did_open_with_language_id(file, contents, &document.language_id)
                        .await
                }
                Ok(None) => continue,
                Err(error) => Err(error),
            };
            match result {
                Ok(()) => {
                    self.opened_documents.insert(document_key);
                    self.remember_document_client(file, client_key);
                }
                Err(error) => {
                    log!("[lsp] could not open {file} on {client_key}: {error}");
                    first_error.get_or_insert(error);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn did_change(&mut self, file: &str, contents: String) -> Result<(), LspError> {
        let Some(keys) = self
            .document_clients
            .get(&document_route_key(file))
            .cloned()
        else {
            return self.did_open(file, &contents).await;
        };
        let mut first_error = None;
        for key in keys {
            if let Some(client) = self.clients.get_mut(&key) {
                if let Err(error) = client.did_change(file, contents.clone()).await {
                    log!("[lsp] could not change {file} on {key}: {error}");
                    first_error.get_or_insert(error);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn did_change_edits(
        &mut self,
        file: &str,
        change: super::DocumentChange,
    ) -> Result<(), LspError> {
        let Some(keys) = self
            .document_clients
            .get(&document_route_key(file))
            .cloned()
        else {
            return self.did_open(file, &change.after.to_string()).await;
        };
        let mut first_error = None;
        for key in keys {
            if let Some(client) = self.clients.get_mut(&key) {
                if let Err(error) = client.did_change_edits(file, change.clone()).await {
                    log!("[lsp] could not change {file} on {key}: {error}");
                    first_error.get_or_insert(error);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn did_save(&mut self, file: &str, contents: &str) -> Result<(), LspError> {
        let mut first_error = self.did_open(file, contents).await.err();
        let keys = self
            .document_clients
            .get(&document_route_key(file))
            .cloned()
            .unwrap_or_default();
        for key in keys {
            if let Some(client) = self.clients.get_mut(&key) {
                if let Err(error) = client.did_save(file, contents).await {
                    log!("[lsp] could not save {file} on {key}: {error}");
                    first_error.get_or_insert(error);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn did_close(&mut self, file: &str) -> Result<(), LspError> {
        self.retire_pending_requests_for_file(file);
        let keys = self
            .document_clients
            .remove(&document_route_key(file))
            .unwrap_or_default();
        let uri = file_uri(file).ok();
        let mut first_error = None;
        for key in keys {
            if let Some(uri) = &uri {
                self.opened_documents.remove(&format!("{key}:{uri}"));
            }
            if let Some(client) = self.clients.get_mut(&key) {
                if let Err(error) = client.did_close(file).await {
                    log!("[lsp] could not close {file} on {key}: {error}");
                    first_error.get_or_insert(error);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn will_save(&mut self, file: &str) -> Result<(), LspError> {
        let keys = self
            .document_clients
            .get(&document_route_key(file))
            .cloned()
            .unwrap_or_default();
        let mut first_error = None;
        for key in keys {
            if let Some(client) = self.clients.get_mut(&key) {
                if let Err(error) = client.will_save(file).await {
                    log!("[lsp] could not send willSave for {file} on {key}: {error}");
                    first_error.get_or_insert(error);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn workspace_symbol(&mut self, query: &str) -> Result<i64, LspError> {
        if let Some(client) = self.first_client_mut() {
            return client.workspace_symbol(query).await;
        }
        Ok(0)
    }

    async fn workspace_symbol_for_file(
        &mut self,
        file: &str,
        query: &str,
    ) -> Result<i64, LspError> {
        self.send_request_for_file(
            file,
            "workspace/symbol",
            serde_json::json!({ "query": query }),
            false,
        )
        .await
    }

    async fn format_document_with_options(
        &mut self,
        file: &str,
        tab_size: usize,
        insert_spaces: bool,
    ) -> Result<i64, LspError> {
        self.send_request_for_file(
            file,
            "textDocument/formatting",
            serde_json::json!({
                "textDocument": { "uri": file_uri(file)? },
                "options": {
                    "tabSize": tab_size,
                    "insertSpaces": insert_spaces,
                    "trimTrailingWhitespace": true,
                    "insertFinalNewline": true,
                    "trimFinalNewlines": true,
                }
            }),
            false,
        )
        .await
    }

    async fn signature_help_with_context(
        &mut self,
        file: &str,
        x: usize,
        y: usize,
        context: Option<super::SignatureHelpContext>,
    ) -> Result<i64, LspError> {
        let mut params = super::document_position_params(file, x, y)?;
        if let Some(context) = context {
            params["context"] = serde_json::to_value(context)?;
        }
        self.send_request_for_file(file, "textDocument/signatureHelp", params, false)
            .await
    }

    async fn send_request(
        &mut self,
        method: &str,
        params: Value,
        force: bool,
    ) -> Result<i64, LspError> {
        if let Some(client) = self.first_client_mut() {
            return client.send_request(method, params, force).await;
        }
        Ok(0)
    }

    async fn send_request_for_file(
        &mut self,
        file: &str,
        method: &str,
        params: Value,
        force: bool,
    ) -> Result<i64, LspError> {
        let request = Request::new(method, params);
        let id = request.id;
        let routed = self.route_prepared_request(file, request, force).await?;
        Ok(if routed { id } else { 0 })
    }

    async fn send_request_for_source(
        &mut self,
        source: &str,
        method: &str,
        params: Value,
        force: bool,
    ) -> Result<i64, LspError> {
        if let Some(client) = self.clients.get_mut(source) {
            return client.send_request(method, params, force).await;
        }
        Err(LspError::ProtocolError(format!(
            "LSP request source is no longer available: {source}"
        )))
    }

    async fn code_actions(
        &mut self,
        file: &str,
        range: super::Range,
        diagnostics: Vec<(String, Vec<super::Diagnostic>)>,
    ) -> Result<Vec<i64>, LspError> {
        self.start_clients_for_file(file).await?;
        let uri = file_uri(file)?;
        let mut ids = Vec::new();
        let mut first_error = None;
        for key in self.request_sources_for_file(file, "textDocument/codeAction") {
            let context = diagnostics
                .iter()
                .find(|(source, _)| source == &key)
                .map(|(_, diagnostics)| diagnostics.clone())
                .unwrap_or_default();
            let request = Request::new(
                "textDocument/codeAction",
                serde_json::json!({
                    "textDocument": { "uri": uri },
                    "range": range,
                    "context": { "diagnostics": context },
                }),
            );
            let id = request.id;
            let status = self
                .clients
                .get(&key)
                .map(|client| client.server_status_for_file(file));
            let result = match status {
                Some("ready") => {
                    self.dispatch_prepared_request(&key, file, request, false)
                        .await
                }
                Some("starting") => self.enqueue_request(file, Some(key.clone()), request, false),
                _ => continue,
            };
            match result {
                Ok(()) => ids.push(id),
                Err(error) => {
                    log!("[lsp] could not request code actions from {key}: {error}");
                    first_error.get_or_insert(error);
                }
            }
        }
        if ids.is_empty() {
            if let Some(error) = first_error {
                return Err(error);
            }
        }
        Ok(ids)
    }

    async fn send_notification(
        &mut self,
        method: &str,
        params: Value,
        force: bool,
    ) -> Result<(), LspError> {
        for client in self.clients.values_mut() {
            client
                .send_notification(method, params.clone(), force)
                .await?;
        }
        Ok(())
    }

    async fn request_completion(
        &mut self,
        file_uri: &str,
        line: usize,
        character: usize,
        trigger_character: Option<char>,
    ) -> Result<i64, LspError> {
        let file = file_path(file_uri)?;
        let context = trigger_character.map_or_else(
            || serde_json::json!({ "triggerKind": 1 }),
            |trigger| {
                serde_json::json!({
                    "triggerKind": 2,
                    "triggerCharacter": trigger.to_string(),
                })
            },
        );
        self.send_request_for_file(
            &file,
            "textDocument/completion",
            serde_json::json!({
                "textDocument": { "uri": file_uri },
                "position": { "line": line, "character": character },
                "context": context,
            }),
            false,
        )
        .await
    }

    async fn request_diagnostics(&mut self, file_uri: &str) -> Result<Option<i64>, LspError> {
        let file = file_path(file_uri)?;
        self.start_clients_for_file(&file).await?;
        let mut first = None;
        for key in self.candidate_keys_for_file(&file) {
            let supports = self.clients.get(&key).is_some_and(|client| {
                client.server_status_for_file(&file) == "ready"
                    && client
                        .get_server_capabilities()
                        .is_some_and(|caps| supports_method(caps, "textDocument/diagnostic"))
            });
            if !supports {
                continue;
            }
            if let Some(id) = self
                .clients
                .get_mut(&key)
                .expect("ready diagnostic client exists")
                .request_diagnostics(file_uri)
                .await?
            {
                first.get_or_insert(id);
            }
        }
        Ok(first)
    }

    async fn recv_response(
        &mut self,
    ) -> Result<Option<(InboundMessage, Option<String>)>, LspError> {
        self.drain_pending_requests().await;
        if let Some(outcome) = self.pending_outcomes.pop_front() {
            return Ok(Some(outcome));
        }
        if self.client_poll_order.len() != self.clients.len() {
            self.client_poll_order.clear();
            self.client_poll_order.extend(self.clients.keys().cloned());
            self.client_poll_order.sort_unstable();
        }
        let client_count = self.client_poll_order.len();
        if client_count == 0 {
            return Ok(None);
        }
        let start = self.next_client_poll % client_count;
        for offset in 0..client_count {
            let index = (start + offset) % client_count;
            let client_key = self.client_poll_order[index].clone();
            let incoming = match self.clients.get_mut(&client_key) {
                Some(client) => match client.recv_response().await {
                    Ok(incoming) => incoming,
                    Err(error) => {
                        let source = self
                            .clients
                            .get(&client_key)
                            .map(|client| client.instance_id())
                            .map(|instance| LspRequestSource {
                                key: client_key.clone(),
                                instance,
                            });
                        self.failed_clients.insert(client_key.clone());
                        self.clients.remove(&client_key);
                        self.client_poll_order.remove(index);
                        self.next_client_poll = 0;
                        let message = match source {
                            Some(source) => InboundMessage::ClientFailure { source, error },
                            None => InboundMessage::ProcessingError(error),
                        };
                        return Ok(Some((message, None)));
                    }
                },
                None => continue,
            };
            if let Some((message, method)) = incoming {
                self.next_client_poll = (index + 1) % client_count;
                let source = self
                    .clients
                    .get(&client_key)
                    .map(|client| client.instance_id())
                    .map(|instance| LspRequestSource {
                        key: client_key.clone(),
                        instance,
                    });
                if matches!(message, InboundMessage::ProcessingError(_)) {
                    self.failed_clients.insert(client_key.clone());
                }
                let mut message = match (message, source.clone()) {
                    (InboundMessage::ProcessingError(error), Some(source)) => {
                        InboundMessage::ClientFailure { source, error }
                    }
                    (message, _) => message,
                };
                if let InboundMessage::Notification(ParsedNotification::Progress(progress)) =
                    &mut message
                {
                    let (server_name, workspace_root) =
                        client_source_from_key(&client_key, &self.config);
                    progress.enrich(server_name, workspace_root);
                }
                if let InboundMessage::Notification(ParsedNotification::PublishDiagnostics(
                    diagnostics,
                )) = &mut message
                {
                    diagnostics.source = source.clone();
                }
                if let InboundMessage::Message(response) = &mut message {
                    if let Some(request) = &mut response.request {
                        request.source = source.clone();
                    }
                }
                if let InboundMessage::Error(error) = &mut message {
                    if let Some(request) = &mut error.request {
                        request.source = source.clone();
                    }
                }
                if let InboundMessage::ServerRequest(request) = &mut message {
                    request.source = source;
                }
                self.drain_pending_requests().await;
                return Ok(Some((message, method)));
            }
        }
        Ok(None)
    }

    fn get_server_capabilities(&self) -> Option<&ServerCapabilities> {
        self.clients
            .values()
            .find_map(|client| client.get_server_capabilities())
    }

    fn server_capabilities_for_file(&self, file: &str) -> Option<&ServerCapabilities> {
        self.candidate_keys_for_file(file)
            .into_iter()
            .find_map(|key| self.clients.get(&key)?.get_server_capabilities())
    }

    fn server_capabilities_for_method(
        &self,
        file: &str,
        method: &str,
    ) -> Option<&ServerCapabilities> {
        let key = self.ready_client_key(file, method, false)?;
        self.clients.get(&key)?.get_server_capabilities()
    }

    fn server_capabilities_for_source(
        &self,
        source: &LspRequestSource,
    ) -> Option<&ServerCapabilities> {
        self.request_source_is_current(source)
            .then(|| self.clients.get(&source.key)?.get_server_capabilities())?
    }

    fn request_source_for_file(&self, file: &str, method: &str) -> Option<LspRequestSource> {
        let key = self.ready_client_key(file, method, false)?;
        self.request_source(&key, file)
    }

    fn request_sources_for_file(&self, file: &str, method: &str) -> Vec<String> {
        self.candidate_keys_for_file(file)
            .into_iter()
            .filter(|key| {
                if self.failed_clients.contains(key) {
                    return false;
                }
                self.clients.get(key).is_some_and(|client| {
                    client.server_status_for_file(file) == "starting"
                        || client
                            .get_server_capabilities()
                            .is_some_and(|capabilities| supports_method(capabilities, method))
                })
            })
            .collect()
    }

    fn attachment_sources_for_file(&self, file: &str) -> Vec<String> {
        self.candidate_keys_for_file(file)
            .into_iter()
            .filter(|key| !self.failed_clients.contains(key))
            .collect()
    }

    fn request_source_is_current(&self, source: &LspRequestSource) -> bool {
        !self.failed_clients.contains(&source.key)
            && self
                .clients
                .get(&source.key)
                .and_then(|client| client.server_instance_for_file(""))
                == Some(source.instance)
    }

    fn request_source_owns_file(&self, source: &LspRequestSource, file: &str) -> bool {
        self.request_source_is_current(source)
            && self
                .document_clients
                .get(&document_route_key(file))
                .is_some_and(|keys| keys.contains(&source.key))
    }

    fn document_version_for_source(&self, file: &str, source: &LspRequestSource) -> Option<i64> {
        self.request_source_is_current(source)
            .then(|| self.clients.get(&source.key)?.document_version(file))?
    }

    fn workspace_root_for_source(&self, source: &LspRequestSource) -> Option<PathBuf> {
        self.request_source_is_current(source)
            .then(|| self.clients.get(&source.key)?.workspace_root_for_file(""))?
    }

    fn server_name_for_file(&self, file: &str) -> Option<String> {
        let key = self.candidate_keys_for_file(file).into_iter().next()?;
        let (server_name, _) = client_source_from_key(&key, &self.config);
        Some(server_name.to_string())
    }

    fn server_status_for_file(&self, file: &str) -> &'static str {
        let keys = self.candidate_keys_for_file(file);
        if keys.is_empty() {
            return "unsupported";
        }
        let statuses = keys.iter().map(|key| {
            if self.failed_clients.contains(key) {
                "failed"
            } else {
                self.clients
                    .get(key)
                    .map_or("not_started", |client| client.server_status_for_file(file))
            }
        });
        statuses
            .clone()
            .find(|status| *status == "ready")
            .or_else(|| statuses.clone().find(|status| *status == "starting"))
            .or_else(|| statuses.clone().find(|status| *status == "not_started"))
            .unwrap_or("failed")
    }

    fn server_instance_for_file(&self, file: &str) -> Option<u64> {
        let key = self.candidate_keys_for_file(file).into_iter().find(|key| {
            self.clients
                .get(key)
                .is_some_and(|client| client.server_status_for_file(file) == "ready")
        })?;
        self.clients.get(&key)?.server_instance_for_file(file)
    }

    async fn cancel_request_for_file(&mut self, file: &str, id: i64) -> Result<(), LspError> {
        if let Some(index) = self
            .pending_requests
            .iter()
            .position(|pending| pending.request.id == id)
        {
            if let Some(pending) = self.pending_requests.remove(index) {
                self.pending_request_bytes =
                    self.pending_request_bytes.saturating_sub(pending.bytes);
            }
            return Ok(());
        }
        // Cancellation must never start another process or reach an unrelated peer.
        for key in self.candidate_keys_for_file(file) {
            let owns_request = self
                .clients
                .get(&key)
                .is_some_and(|client| client.owns_request(id));
            if owns_request {
                if let Some(client) = self.clients.get_mut(&key) {
                    client.cancel_request_for_file(file, id).await?;
                }
                break;
            }
        }
        Ok(())
    }

    fn supports_document_formatting(&self, file: &str) -> bool {
        self.ready_client_key(file, "textDocument/formatting", false)
            .is_some()
    }

    fn supports_range_formatting(&self, file: &str) -> bool {
        self.ready_client_key(file, "textDocument/rangeFormatting", false)
            .is_some()
    }

    fn requires_incremental_change(&self, file: &str) -> bool {
        self.candidate_keys_for_file(file).into_iter().any(|key| {
            self.clients
                .get(&key)
                .and_then(|client| client.get_server_capabilities())
                .is_some_and(|capabilities| {
                    matches!(
                        capabilities
                            .text_document_sync
                            .as_ref()
                            .and_then(|sync| sync.change_kind()),
                        Some(super::TextDocumentSyncKind::Incremental)
                    )
                })
        })
    }

    fn document_version(&self, file: &str) -> Option<i64> {
        self.candidate_keys_for_file(file)
            .into_iter()
            .find_map(|key| self.clients.get(&key)?.document_version(file))
    }

    fn workspace_root_for_file(&self, file: &str) -> Option<PathBuf> {
        if let Some(root) = self
            .candidate_keys_for_file(file)
            .into_iter()
            .find_map(|key| self.clients.get(&key)?.workspace_root_for_file(file))
        {
            return Some(root);
        }
        self.resolve_document(file)
            .map(|document| document.workspace_root)
    }

    fn workspace_root_for_request(&self, request: &ServerRequest) -> Option<PathBuf> {
        request
            .source
            .as_ref()
            .and_then(|source| self.clients.get(&source.key))
            .and_then(|client| client.workspace_root_for_request(request))
    }

    async fn respond_workspace_edit(
        &mut self,
        request: &ServerRequest,
        applied: bool,
        failure_reason: Option<&str>,
    ) -> Result<(), LspError> {
        let Some(source) = request.source.as_ref() else {
            return Err(LspError::ProtocolError(
                "LSP workspace edit request is missing its server source".to_string(),
            ));
        };
        let Some(client) = self.clients.get_mut(&source.key) else {
            return Err(LspError::ProtocolError(format!(
                "LSP workspace edit server is no longer available: {}",
                source.key
            )));
        };
        client
            .respond_workspace_edit(request, applied, failure_reason)
            .await
    }

    async fn shutdown(&mut self) -> Result<(), LspError> {
        complete_shutdowns(self.clients.values_mut().map(|client| client.shutdown())).await
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        fs,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        },
        time::Duration,
    };

    use crate::{
        config::{LanguageDocumentConfig, LanguageServerConfig},
        lsp::{Diagnostic, OutboundMessage, Position, Range},
    };

    use super::*;

    fn server(language_id: &str, extensions: &[&str]) -> LanguageServerConfig {
        LanguageServerConfig {
            command: "mock-lsp".to_string(),
            args: Vec::new(),
            language_id: language_id.to_string(),
            file_extensions: extensions.iter().map(|ext| ext.to_string()).collect(),
            filenames: Vec::new(),
            documents: Vec::new(),
            root_markers: vec![".git".to_string()],
            env: HashMap::new(),
            initialization_options: None,
            settings: None,
            workspace_name: None,
        }
    }

    fn multi_document_server(documents: &[(&str, &[&str])]) -> LanguageServerConfig {
        LanguageServerConfig {
            command: "mock-lsp".to_string(),
            args: Vec::new(),
            language_id: String::new(),
            file_extensions: Vec::new(),
            filenames: Vec::new(),
            documents: documents
                .iter()
                .map(|(language_id, extensions)| LanguageDocumentConfig {
                    language_id: language_id.to_string(),
                    file_extensions: extensions.iter().map(|ext| ext.to_string()).collect(),
                    filenames: Vec::new(),
                })
                .collect(),
            root_markers: vec![".git".to_string()],
            env: HashMap::new(),
            initialization_options: None,
            settings: None,
            workspace_name: None,
        }
    }

    #[tokio::test]
    async fn shutdowns_are_driven_concurrently() {
        let started = Arc::new(AtomicUsize::new(0));
        let shutdowns = (0..2).map(|_| {
            let started = Arc::clone(&started);
            async move {
                started.fetch_add(1, Ordering::SeqCst);
                tokio::time::timeout(Duration::from_millis(100), async {
                    while started.load(Ordering::SeqCst) < 2 {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .map_err(|_| {
                    LspError::ProtocolError("shutdown futures ran sequentially".to_string())
                })?;
                Ok(())
            }
        });

        complete_shutdowns(shutdowns).await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_error_does_not_cancel_other_clients() {
        let completed = Arc::new(AtomicBool::new(false));
        let shutdowns = (0..2).map(|index| {
            let completed = Arc::clone(&completed);
            async move {
                if index == 0 {
                    Err(LspError::ProtocolError("expected failure".to_string()))
                } else {
                    tokio::task::yield_now().await;
                    completed.store(true, Ordering::SeqCst);
                    Ok(())
                }
            }
        });

        assert!(complete_shutdowns(shutdowns).await.is_err());
        assert!(completed.load(Ordering::SeqCst));
    }

    #[test]
    fn resolves_configured_language_by_extension() {
        let manager = LspManager::new(LspConfig {
            enabled: true,
            format_on_save: false,
            servers: HashMap::from([
                ("rust".to_string(), server("rust", &["rs"])),
                ("python".to_string(), server("python", &["py"])),
            ]),
        });

        let document = manager.resolve_document("example.py").unwrap();
        assert_eq!(document.language_id, "python");
        assert_eq!(document.server_name, "python");
        assert_eq!(document.uri, file_uri(&document.path).unwrap());
    }

    #[test]
    fn resolves_fish_files_to_the_default_fish_language_server() {
        let manager = LspManager::new(LspConfig::default());

        let document = manager.resolve_document("config.fish").unwrap();

        assert_eq!(document.language_id, "fish");
        assert_eq!(document.server_name, "fish");
        assert_eq!(document.uri, file_uri(&document.path).unwrap());
    }

    #[test]
    fn unresolved_language_returns_none() {
        let manager = LspManager::new(LspConfig {
            enabled: true,
            format_on_save: false,
            servers: HashMap::from([("rust".to_string(), server("rust", &["rs"]))]),
        });

        assert!(manager.resolve_document("README.md").is_none());
    }

    #[test]
    fn rust_members_share_the_cargo_workspace_root() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(".git")).unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"core\", \"tui\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        let mut files = Vec::new();
        for member in ["core", "tui"] {
            let directory = root.path().join(member).join("src");
            fs::create_dir_all(&directory).unwrap();
            fs::write(
                root.path().join(member).join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{member}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"
                ),
            )
            .unwrap();
            let file = directory.join("lib.rs");
            fs::write(&file, "pub fn example() {}\n").unwrap();
            files.push(file);
        }
        let mut rust = server("rust", &["rs"]);
        rust.command = root.path().join("rustup").to_string_lossy().into_owned();
        rust.args = vec![
            "run".to_string(),
            "stable".to_string(),
            "rust-analyzer".to_string(),
        ];
        rust.root_markers = vec!["Cargo.toml".to_string(), ".git".to_string()];
        let glancer = crate::config::Config::load_user_toml(
            include_str!("../../examples/rust-glancer.toml"),
            Path::new("/tmp/config.toml"),
            &[],
        )
        .unwrap()
        .config
        .lsp
        .servers
        .remove("rust")
        .unwrap();
        for config in [rust, glancer] {
            let manager = LspManager::new(LspConfig {
                enabled: true,
                format_on_save: false,
                servers: HashMap::from([("rust".to_string(), config)]),
            });

            let core = manager
                .resolve_document(files[0].to_string_lossy().as_ref())
                .unwrap();
            let tui = manager
                .resolve_document(files[1].to_string_lossy().as_ref())
                .unwrap();
            let expected = fs::canonicalize(root.path()).unwrap();

            assert_eq!(core.workspace_root, expected);
            assert_eq!(tui.workspace_root, expected);
            assert_eq!(client_key(&core), client_key(&tui));
        }
    }

    #[test]
    fn cargo_workspace_discovery_does_not_escape_a_nested_repository() {
        let outer = tempfile::tempdir().unwrap();
        fs::write(
            outer.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"nested/member\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        let repository = outer.path().join("nested");
        let member = repository.join("member");
        fs::create_dir_all(repository.join(".git")).unwrap();
        fs::create_dir_all(member.join("src")).unwrap();
        fs::write(
            member.join("Cargo.toml"),
            "[package]\nname = \"member\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let file = member.join("src/lib.rs");
        fs::write(&file, "pub fn example() {}\n").unwrap();
        let mut rust = server("rust", &["rs"]);
        rust.root_markers = vec!["Cargo.toml".to_string(), ".git".to_string()];
        let manager = LspManager::new(LspConfig {
            enabled: true,
            format_on_save: false,
            servers: HashMap::from([("rust".to_string(), rust)]),
        });

        let document = manager
            .resolve_document(file.to_string_lossy().as_ref())
            .unwrap();

        assert_eq!(document.workspace_root, fs::canonicalize(member).unwrap());
    }

    #[test]
    fn overlapping_servers_resolve_all_matches_by_name() {
        let manager = LspManager::new(LspConfig {
            enabled: true,
            format_on_save: false,
            servers: HashMap::from([
                ("zeta".to_string(), server("zeta", &["rs"])),
                ("alpha".to_string(), server("alpha", &["rs"])),
            ]),
        });

        let documents = manager.resolve_documents("example.rs");
        assert_eq!(documents.len(), 2);
        assert_eq!(documents[0].server_name, "alpha");
        assert_eq!(documents[0].language_id, "alpha");
        assert_eq!(documents[1].server_name, "zeta");
        assert_eq!(documents[1].language_id, "zeta");
    }

    #[test]
    fn resolves_document_selector_language_by_extension() {
        let manager = LspManager::new(LspConfig {
            enabled: true,
            format_on_save: false,
            servers: HashMap::from([(
                "web".to_string(),
                multi_document_server(&[
                    ("typescript", &["ts"]),
                    ("typescriptreact", &["tsx"]),
                    ("javascript", &["js"]),
                    ("javascriptreact", &["jsx"]),
                ]),
            )]),
        });

        let document = manager.resolve_document("component.TSX").unwrap();
        assert_eq!(document.language_id, "typescriptreact");
        assert_eq!(document.server_name, "web");
    }

    #[test]
    fn exact_filenames_take_priority_over_extension_selectors() {
        let mut filename_server = server("container", &[]);
        filename_server.documents = vec![LanguageDocumentConfig {
            language_id: "dockerfile".to_string(),
            file_extensions: Vec::new(),
            filenames: vec!["Container.toml".to_string()],
        }];
        let manager = LspManager::new(LspConfig {
            enabled: true,
            format_on_save: false,
            servers: HashMap::from([
                ("container".to_string(), filename_server),
                ("toml".to_string(), server("toml", &["toml"])),
            ]),
        });

        let document = manager.resolve_document("Container.toml").unwrap();
        assert_eq!(document.server_name, "container");
        assert_eq!(document.language_id, "dockerfile");
        assert_eq!(
            manager
                .resolve_document("container.toml")
                .unwrap()
                .server_name,
            "toml"
        );
    }

    #[test]
    fn exact_filename_tier_keeps_all_exact_matches() {
        let mut first = server("first", &[]);
        first.filenames = vec!["Container.toml".to_string()];
        let mut second = server("second", &[]);
        second.filenames = vec!["Container.toml".to_string()];
        let manager = LspManager::new(LspConfig {
            enabled: true,
            format_on_save: false,
            servers: HashMap::from([
                ("beta".to_string(), second),
                ("alpha".to_string(), first),
                ("toml".to_string(), server("toml", &["toml"])),
            ]),
        });

        let documents = manager.resolve_documents("Container.toml");
        assert_eq!(
            documents
                .iter()
                .map(|document| document.server_name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "beta"]
        );
    }

    #[tokio::test]
    async fn reconfigure_rebuilds_exact_filename_routing() {
        let mut manager = LspManager::new(LspConfig::default());
        let mut server = server("dockerfile", &[]);
        server.filenames = vec!["Dockerfile".to_string()];
        let config = LspConfig {
            enabled: true,
            format_on_save: false,
            servers: HashMap::from([("docker".to_string(), server)]),
        };

        assert!(manager.resolve_document("Dockerfile").is_none());
        assert!(manager.reconfigure(config).await.unwrap().is_empty());
        assert_eq!(
            manager.resolve_document("Dockerfile").unwrap().language_id,
            "dockerfile"
        );
    }

    #[tokio::test]
    async fn reconfigure_preserves_unmodified_running_clients_and_open_documents() {
        let root = std::env::current_dir().unwrap();
        let rust = server("rust", &["rs"]);
        let mut manager = LspManager::new(LspConfig {
            enabled: true,
            format_on_save: false,
            servers: HashMap::from([("rust".to_string(), rust.clone())]),
        });
        let file = root.join("preserved.rs").to_string_lossy().into_owned();
        let document = manager.resolve_document(&file).unwrap();
        let key = client_key(&document);
        let (request_tx, _request_rx) = tokio::sync::mpsc::channel(1);
        let (_response_tx, response_rx) = tokio::sync::mpsc::channel(1);
        manager.clients.insert(
            key.clone(),
            RealLspClient::with_test_channels(request_tx, response_rx, rust.clone(), root),
        );
        manager
            .document_clients
            .insert(file.clone(), vec![key.clone()]);
        manager.opened_documents.insert(document_key(&document));

        let affected = manager
            .reconfigure(LspConfig {
                enabled: true,
                format_on_save: false,
                servers: HashMap::from([
                    ("rust".to_string(), rust),
                    ("python".to_string(), server("python", &["py"])),
                ]),
            })
            .await
            .unwrap();

        assert!(affected.is_empty());
        assert!(manager.clients.contains_key(&key));
        assert_eq!(manager.document_clients.get(&file), Some(&vec![key]));
        assert!(manager.opened_documents.contains(&document_key(&document)));
        assert_eq!(
            manager.resolve_document("example.py").unwrap().language_id,
            "python"
        );
    }

    #[tokio::test]
    async fn reconfigure_reopens_documents_when_an_exact_filename_takes_precedence() {
        let root = std::env::current_dir().unwrap();
        let toml = server("toml", &["toml"]);
        let mut manager = LspManager::new(LspConfig {
            enabled: true,
            format_on_save: false,
            servers: HashMap::from([("toml".to_string(), toml.clone())]),
        });
        let file = root.join("Container.toml").to_string_lossy().into_owned();
        let document = manager.resolve_document(&file).unwrap();
        let key = client_key(&document);
        let (request_tx, mut request_rx) = tokio::sync::mpsc::channel(1);
        let (_response_tx, response_rx) = tokio::sync::mpsc::channel(1);
        manager.clients.insert(
            key.clone(),
            RealLspClient::with_test_channels(request_tx, response_rx, toml.clone(), root),
        );
        manager
            .document_clients
            .insert(file.clone(), vec![key.clone()]);
        manager.opened_documents.insert(document_key(&document));

        let mut container = server("dockerfile", &[]);
        container.filenames = vec!["Container.toml".to_string()];
        let affected = manager
            .reconfigure(LspConfig {
                enabled: true,
                format_on_save: false,
                servers: HashMap::from([
                    ("toml".to_string(), toml),
                    ("container".to_string(), container),
                ]),
            })
            .await
            .unwrap();

        assert_eq!(affected, std::slice::from_ref(&file));
        assert!(manager.clients.contains_key(&key));
        assert!(!manager.document_clients.contains_key(&file));
        assert!(!manager.opened_documents.contains(&document_key(&document)));
        assert_eq!(
            manager.resolve_document(&file).unwrap().server_name,
            "container"
        );
        let OutboundMessage::Notification(notification) = request_rx.try_recv().unwrap() else {
            panic!("expected the preserved client to receive a close notification");
        };
        assert_eq!(notification.method, "textDocument/didClose");
    }

    #[tokio::test]
    async fn reconfigure_commits_new_routing_when_old_client_shutdown_fails() {
        let root = std::env::current_dir().unwrap();
        let rust = server("rust", &["rs"]);
        let mut manager = LspManager::new(LspConfig {
            enabled: true,
            format_on_save: false,
            servers: HashMap::from([("rust".to_string(), rust.clone())]),
        });
        let file = root.join("replacement.rs").to_string_lossy().into_owned();
        let document = manager.resolve_document(&file).unwrap();
        let key = client_key(&document);
        let (request_tx, request_rx) = tokio::sync::mpsc::channel(1);
        drop(request_rx);
        let (_response_tx, response_rx) = tokio::sync::mpsc::channel(1);
        manager.clients.insert(
            key.clone(),
            RealLspClient::with_test_channels(request_tx, response_rx, rust.clone(), root),
        );
        manager
            .document_clients
            .insert(file.clone(), vec![key.clone()]);
        manager.opened_documents.insert(document_key(&document));

        let mut replacement = rust;
        replacement.command = "replacement-lsp".to_string();
        let affected = manager
            .reconfigure(LspConfig {
                enabled: true,
                format_on_save: false,
                servers: HashMap::from([("rust".to_string(), replacement)]),
            })
            .await
            .unwrap();

        assert_eq!(affected, std::slice::from_ref(&file));
        assert_eq!(manager.config.servers["rust"].command, "replacement-lsp");
        assert!(!manager.clients.contains_key(&key));
        assert!(!manager.document_clients.contains_key(&file));
        assert!(!manager.opened_documents.contains(&document_key(&document)));
    }

    #[tokio::test]
    async fn reconfigure_replaces_servers_whose_names_contain_colons() {
        let root = std::env::current_dir().unwrap();
        let typescript = server("typescript", &["ts"]);
        let server_name = "typescript:eslint";
        let mut manager = LspManager::new(LspConfig {
            enabled: true,
            format_on_save: false,
            servers: HashMap::from([(server_name.to_string(), typescript.clone())]),
        });
        let file = root.join("example.ts").to_string_lossy().into_owned();
        let document = manager.resolve_document(&file).unwrap();
        let key = client_key(&document);
        let (request_tx, request_rx) = tokio::sync::mpsc::channel(1);
        drop(request_rx);
        let (_response_tx, response_rx) = tokio::sync::mpsc::channel(1);
        manager.clients.insert(
            key.clone(),
            RealLspClient::with_test_channels(request_tx, response_rx, typescript.clone(), root),
        );
        manager
            .document_clients
            .insert(file.clone(), vec![key.clone()]);
        manager.opened_documents.insert(document_key(&document));

        let mut replacement = typescript;
        replacement.command = "replacement-eslint".to_string();
        let affected = manager
            .reconfigure(LspConfig {
                enabled: true,
                format_on_save: false,
                servers: HashMap::from([(server_name.to_string(), replacement)]),
            })
            .await
            .unwrap();

        assert_eq!(affected, std::slice::from_ref(&file));
        assert!(!manager.clients.contains_key(&key));
        assert!(!manager.document_clients.contains_key(&file));
        assert!(!manager.opened_documents.contains(&document_key(&document)));
    }

    #[test]
    fn client_source_preserves_colon_names_and_windows_workspace_roots() {
        let config = LspConfig {
            enabled: true,
            format_on_save: false,
            servers: HashMap::from([
                ("typescript".to_string(), server("typescript", &["ts"])),
                (
                    "typescript:eslint".to_string(),
                    server("typescript", &["tsx"]),
                ),
            ]),
        };

        assert_eq!(
            client_source_from_key("typescript:eslint:C:\\workspace", &config),
            ("typescript:eslint", "C:\\workspace")
        );
        assert_eq!(
            client_source_from_key("typescript:/workspace", &config),
            ("typescript", "/workspace")
        );
    }

    #[test]
    fn disabled_lsp_returns_none() {
        let manager = LspManager::new(LspConfig {
            enabled: false,
            format_on_save: false,
            servers: HashMap::from([("rust".to_string(), server("rust", &["rs"]))]),
        });

        assert!(manager.resolve_document("src/main.rs").is_none());
    }

    #[tokio::test]
    async fn did_change_opens_a_document_once_and_reuses_its_client() {
        let root = std::env::current_dir().unwrap();
        let server_config = server("rust", &["rs"]);
        let mut manager = LspManager::new(LspConfig {
            enabled: true,
            format_on_save: false,
            servers: HashMap::from([("rust".to_string(), server_config.clone())]),
        });
        let file = root
            .join("manager-change.rs")
            .to_string_lossy()
            .into_owned();
        let document = manager.resolve_document(&file).unwrap();
        let (request_tx, mut request_rx) = tokio::sync::mpsc::channel(4);
        let (_response_tx, response_rx) = tokio::sync::mpsc::channel(1);
        manager.clients.insert(
            client_key(&document),
            RealLspClient::with_test_channels(request_tx, response_rx, server_config, root),
        );

        manager.did_change(&file, "one".to_string()).await.unwrap();
        manager.did_change(&file, "two".to_string()).await.unwrap();

        let mut methods = Vec::new();
        while let Ok(OutboundMessage::Notification(notification)) = request_rx.try_recv() {
            methods.push(notification.method);
        }
        assert_eq!(methods, ["textDocument/didOpen", "textDocument/didChange"]);
        assert_eq!(manager.opened_documents.len(), 1);
        assert_eq!(manager.document_clients.len(), 1);
    }

    #[tokio::test]
    async fn document_lifecycle_reaches_every_matching_client() {
        let root = std::env::current_dir().unwrap();
        let alpha = server("rust", &["rs"]);
        let beta = server("rust", &["rs"]);
        let mut manager = LspManager::new(LspConfig {
            enabled: true,
            format_on_save: false,
            servers: HashMap::from([
                ("alpha".to_string(), alpha.clone()),
                ("beta".to_string(), beta.clone()),
            ]),
        });
        let file = root
            .join("manager-overlap.rs")
            .to_string_lossy()
            .into_owned();
        let documents = manager.resolve_documents(&file);
        let mut receivers = Vec::new();
        for (document, config) in documents.iter().zip([alpha, beta]) {
            let (request_tx, request_rx) = tokio::sync::mpsc::channel(8);
            let (_response_tx, response_rx) = tokio::sync::mpsc::channel(1);
            manager.clients.insert(
                client_key(document),
                RealLspClient::with_test_channels(request_tx, response_rx, config, root.clone()),
            );
            receivers.push(request_rx);
        }

        manager.did_open(&file, "one").await.unwrap();
        manager.did_change(&file, "two".to_string()).await.unwrap();
        manager.did_close(&file).await.unwrap();

        for receiver in &mut receivers {
            let mut methods = Vec::new();
            while let Ok(OutboundMessage::Notification(notification)) = receiver.try_recv() {
                methods.push(notification.method);
            }
            assert_eq!(
                methods,
                [
                    "textDocument/didOpen",
                    "textDocument/didChange",
                    "textDocument/didClose",
                ]
            );
        }
        assert!(!manager.document_clients.contains_key(&file));
    }

    #[tokio::test]
    async fn document_features_route_to_the_first_capable_attachment() {
        let root = std::env::current_dir().unwrap();
        let eslint = server("typescript", &["ts"]);
        let typescript = server("typescript", &["ts"]);
        let mut manager = LspManager::new(LspConfig {
            enabled: true,
            format_on_save: false,
            servers: HashMap::from([
                ("eslint".to_string(), eslint.clone()),
                ("typescript".to_string(), typescript.clone()),
            ]),
        });
        let file = root.join("manager-route.ts").to_string_lossy().into_owned();
        let documents = manager.resolve_documents(&file);
        let (eslint_tx, mut eslint_rx) = tokio::sync::mpsc::channel(4);
        let (_eslint_response_tx, eslint_response_rx) = tokio::sync::mpsc::channel(1);
        let mut eslint_client =
            RealLspClient::with_test_channels(eslint_tx, eslint_response_rx, eslint, root.clone());
        eslint_client.set_test_capabilities(
            serde_json::from_value(serde_json::json!({
                "codeActionProvider": true,
                "renameProvider": true
            }))
            .unwrap(),
        );
        manager
            .clients
            .insert(client_key(&documents[0]), eslint_client);

        let (typescript_tx, mut typescript_rx) = tokio::sync::mpsc::channel(4);
        let (_typescript_response_tx, typescript_response_rx) = tokio::sync::mpsc::channel(1);
        let mut typescript_client = RealLspClient::with_test_channels(
            typescript_tx,
            typescript_response_rx,
            typescript,
            root,
        );
        typescript_client.set_test_capabilities(
            serde_json::from_value(serde_json::json!({
                "documentSymbolProvider": true,
                "completionProvider": { "triggerCharacters": ["."] },
                "signatureHelpProvider": { "triggerCharacters": ["("] },
                "renameProvider": { "prepareProvider": true },
                "diagnosticProvider": {
                    "interFileDependencies": true,
                    "workspaceDiagnostics": false
                }
            }))
            .unwrap(),
        );
        manager
            .clients
            .insert(client_key(&documents[1]), typescript_client);

        let request_id = manager.document_symbols(&file).await.unwrap();
        assert!(eslint_rx.try_recv().is_err());
        let OutboundMessage::Request(request) = typescript_rx.try_recv().unwrap() else {
            panic!("expected document symbols request");
        };
        assert_eq!(request.id, request_id);
        assert_eq!(request.method, "textDocument/documentSymbol");
        assert_eq!(request.source.unwrap().key, client_key(&documents[1]));
        assert!(manager
            .server_capabilities_for_file(&file)
            .unwrap()
            .completion_provider
            .is_none());
        assert_eq!(
            manager
                .server_capabilities_for_method(&file, "textDocument/completion")
                .and_then(|capabilities| capabilities.completion_provider.as_ref())
                .and_then(|provider| provider.trigger_characters.as_ref()),
            Some(&vec![".".to_string()])
        );
        assert_eq!(
            manager
                .server_capabilities_for_method(&file, "textDocument/signatureHelp")
                .and_then(|capabilities| capabilities.signature_help_provider.as_ref())
                .and_then(|provider| provider.trigger_characters.as_ref()),
            Some(&vec!["(".to_string()])
        );
        assert_eq!(
            manager
                .request_source_for_file(&file, "textDocument/prepareRename")
                .unwrap()
                .key,
            client_key(&documents[1])
        );
        assert_eq!(
            manager
                .request_source_for_file(&file, "textDocument/publishDiagnostics")
                .unwrap()
                .key,
            client_key(&documents[0])
        );
        assert_eq!(
            manager
                .request_source_for_file(&file, "textDocument/diagnostic")
                .unwrap()
                .key,
            client_key(&documents[1])
        );
    }

    #[tokio::test]
    async fn diagnostic_source_covers_unopened_workspace_files_and_survives_failure() {
        let root = std::env::current_dir().unwrap();
        let config = server("typescript", &["ts"]);
        let mut manager = LspManager::new(LspConfig {
            enabled: true,
            format_on_save: false,
            servers: HashMap::from([("typescript".to_string(), config.clone())]),
        });
        let file = root.join("manager-open.ts").to_string_lossy().into_owned();
        let unopened = root
            .join("manager-unopened.ts")
            .to_string_lossy()
            .into_owned();
        let document = manager.resolve_document(&file).unwrap();
        let key = client_key(&document);
        let (request_tx, _request_rx) = tokio::sync::mpsc::channel(1);
        let (response_tx, response_rx) = tokio::sync::mpsc::channel(2);
        manager.clients.insert(
            key.clone(),
            RealLspClient::with_test_channels(request_tx, response_rx, config, root),
        );
        let source = manager.request_source(&key, &file).unwrap();
        let instance = source.instance;

        assert!(!manager.request_source_owns_file(&source, &unopened));
        assert!(manager.request_source_owns_workspace_file(&source, &unopened));

        response_tx
            .send(InboundMessage::ProcessingError(LspError::ProtocolError(
                "expected failure".to_string(),
            )))
            .await
            .unwrap();
        response_tx
            .send(InboundMessage::Notification(
                ParsedNotification::PublishDiagnostics(
                    serde_json::from_value(serde_json::json!({
                        "uri": file_uri(&unopened).unwrap(),
                        "diagnostics": []
                    }))
                    .unwrap(),
                ),
            ))
            .await
            .unwrap();

        let InboundMessage::ClientFailure { source, .. } =
            manager.recv_response().await.unwrap().unwrap().0
        else {
            panic!("expected managed client failure");
        };
        assert_eq!(source.key, key);
        assert_eq!(source.instance, instance);

        let InboundMessage::Notification(ParsedNotification::PublishDiagnostics(report)) =
            manager.recv_response().await.unwrap().unwrap().0
        else {
            panic!("expected buffered diagnostics");
        };
        assert_eq!(report.source.as_ref().unwrap().instance, instance);
        assert!(!manager.request_source_is_current(report.source.as_ref().unwrap()));
    }

    #[tokio::test]
    async fn closing_a_document_retires_its_queued_requests_across_path_aliases() {
        let mut manager = LspManager::new(LspConfig::default());
        let relative = "manager-close-pending.ts";
        let absolute = document_route_key(relative);
        let request = Request::new(
            "textDocument/hover",
            serde_json::json!({ "textDocument": { "uri": file_uri(&absolute).unwrap() } }),
        );
        let request_id = request.id;
        manager
            .enqueue_request(relative, None, request, false)
            .unwrap();

        manager.did_close(&absolute).await.unwrap();

        assert!(manager.pending_requests.is_empty());
        assert_eq!(manager.pending_request_bytes, 0);
        assert!(matches!(
            manager.pending_outcomes.pop_front(),
            Some((InboundMessage::RequestError { id, .. }, Some(method)))
                if id == request_id && method == "textDocument/hover"
        ));
    }

    #[tokio::test]
    async fn document_versions_are_scoped_to_the_originating_attachment() {
        let root = std::env::current_dir().unwrap();
        let eslint = server("typescript", &["ts"]);
        let typescript = server("typescript", &["ts"]);
        let mut manager = LspManager::new(LspConfig {
            enabled: true,
            format_on_save: false,
            servers: HashMap::from([
                ("eslint".to_string(), eslint.clone()),
                ("typescript".to_string(), typescript.clone()),
            ]),
        });
        let file = root
            .join("manager-versions.ts")
            .to_string_lossy()
            .into_owned();
        let documents = manager.resolve_documents(&file);
        let mut request_receivers = Vec::new();
        for (document, config) in documents.iter().zip([eslint, typescript]) {
            let (request_tx, request_rx) = tokio::sync::mpsc::channel(8);
            let (_response_tx, response_rx) = tokio::sync::mpsc::channel(1);
            manager.clients.insert(
                client_key(document),
                RealLspClient::with_test_channels(request_tx, response_rx, config, root.clone()),
            );
            request_receivers.push(request_rx);
        }
        manager.did_open(&file, "one").await.unwrap();
        let typescript_key = client_key(&documents[1]);
        manager
            .clients
            .get_mut(&typescript_key)
            .unwrap()
            .did_change(&file, "two".to_string())
            .await
            .unwrap();
        let eslint_source = manager
            .request_source(&client_key(&documents[0]), &file)
            .unwrap();
        let typescript_source = manager.request_source(&typescript_key, &file).unwrap();

        assert_eq!(
            manager.document_version_for_source(&file, &eslint_source),
            Some(1)
        );
        assert_eq!(
            manager.document_version_for_source(&file, &typescript_source),
            Some(2)
        );
    }

    #[tokio::test]
    async fn code_actions_reach_every_capable_attachment_with_owned_diagnostics() {
        let root = std::env::current_dir().unwrap();
        let alpha = server("typescript", &["ts"]);
        let beta = server("typescript", &["ts"]);
        let mut manager = LspManager::new(LspConfig {
            enabled: true,
            format_on_save: false,
            servers: HashMap::from([
                ("alpha".to_string(), alpha.clone()),
                ("beta".to_string(), beta.clone()),
            ]),
        });
        let file = root
            .join("manager-code-actions.ts")
            .to_string_lossy()
            .into_owned();
        let documents = manager.resolve_documents(&file);
        let mut receivers = Vec::new();
        for (document, config) in documents.iter().zip([alpha, beta]) {
            let (request_tx, request_rx) = tokio::sync::mpsc::channel(4);
            let (_response_tx, response_rx) = tokio::sync::mpsc::channel(1);
            let mut client =
                RealLspClient::with_test_channels(request_tx, response_rx, config, root.clone());
            client.set_test_capabilities(
                serde_json::from_value(serde_json::json!({ "codeActionProvider": true })).unwrap(),
            );
            manager.clients.insert(client_key(document), client);
            receivers.push(request_rx);
        }
        let diagnostics = documents
            .iter()
            .enumerate()
            .map(|(index, document)| {
                let diagnostic: Diagnostic = serde_json::from_value(serde_json::json!({
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 0, "character": 1 }
                    },
                    "message": format!("diagnostic-{index}")
                }))
                .unwrap();
                (client_key(document), vec![diagnostic])
            })
            .collect();

        let ids = manager
            .code_actions(
                &file,
                Range {
                    start: Position {
                        line: 0,
                        character: 0,
                    },
                    end: Position {
                        line: 0,
                        character: 0,
                    },
                },
                diagnostics,
            )
            .await
            .unwrap();

        assert_eq!(ids.len(), 2);
        for (index, receiver) in receivers.iter_mut().enumerate() {
            let OutboundMessage::Request(request) = receiver.try_recv().unwrap() else {
                panic!("expected code-action request");
            };
            assert_eq!(request.id, ids[index]);
            assert_eq!(request.method, "textDocument/codeAction");
            assert_eq!(
                request.params["context"]["diagnostics"][0]["message"],
                format!("diagnostic-{index}")
            );
            assert_eq!(request.source.unwrap().key, client_key(&documents[index]));
        }
    }

    #[tokio::test]
    async fn a_chatty_language_server_cannot_starve_another_client() {
        let root = std::env::current_dir().unwrap();
        let alpha = server("alpha", &["rs"]);
        let beta = server("beta", &["py"]);
        let mut manager = LspManager::new(LspConfig {
            enabled: true,
            format_on_save: false,
            servers: HashMap::from([
                ("alpha".to_string(), alpha.clone()),
                ("beta".to_string(), beta.clone()),
            ]),
        });
        let (alpha_request_tx, _alpha_request_rx) = tokio::sync::mpsc::channel(1);
        let (alpha_response_tx, alpha_response_rx) = tokio::sync::mpsc::channel(4);
        let (beta_request_tx, _beta_request_rx) = tokio::sync::mpsc::channel(1);
        let (beta_response_tx, beta_response_rx) = tokio::sync::mpsc::channel(2);
        manager.clients.insert(
            format!("alpha:{}", root.display()),
            RealLspClient::with_test_channels(
                alpha_request_tx,
                alpha_response_rx,
                alpha,
                root.clone(),
            ),
        );
        manager.clients.insert(
            format!("beta:{}", root.display()),
            RealLspClient::with_test_channels(
                beta_request_tx,
                beta_response_rx,
                beta,
                root.clone(),
            ),
        );
        for method in ["alpha/one", "alpha/two"] {
            alpha_response_tx
                .send(InboundMessage::UnknownNotification(
                    super::super::Notification {
                        method: method.to_string(),
                        params: serde_json::Value::Null,
                    },
                ))
                .await
                .unwrap();
        }
        beta_response_tx
            .send(InboundMessage::UnknownNotification(
                super::super::Notification {
                    method: "beta/one".to_string(),
                    params: serde_json::Value::Null,
                },
            ))
            .await
            .unwrap();

        let first = manager.recv_response().await.unwrap().unwrap().0;
        let second = manager.recv_response().await.unwrap().unwrap().0;

        assert!(matches!(first, InboundMessage::UnknownNotification(_)));
        let InboundMessage::UnknownNotification(second) = second else {
            panic!("expected beta notification");
        };
        assert_eq!(second.method, "beta/one");
    }
}
