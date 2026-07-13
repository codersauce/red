use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use path_absolutize::Absolutize;
use serde_json::Value;

use crate::{
    config::{LanguageServerConfig, LspConfig},
    highlighter::normalized_extension,
    log,
};

use super::{
    file_path, file_uri, Diagnostic, InboundMessage, LspClient, LspError, ParsedNotification,
    Range, RealLspClient, ServerCapabilities, ServerRequest,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentInfo {
    pub path: PathBuf,
    pub uri: String,
    pub language_id: String,
    pub workspace_root: PathBuf,
    pub server_name: String,
}

pub struct LspManager {
    config: LspConfig,
    clients: HashMap<String, RealLspClient>,
    failed_clients: HashSet<String>,
    opened_documents: HashSet<String>,
    next_client_poll: usize,
}

impl LspManager {
    pub fn new(config: LspConfig) -> Self {
        Self {
            config,
            clients: HashMap::new(),
            failed_clients: HashSet::new(),
            opened_documents: HashSet::new(),
            next_client_poll: 0,
        }
    }

    pub fn resolve_document(&self, file: &str) -> Option<DocumentInfo> {
        if !self.config.enabled {
            return None;
        }

        let extension = normalized_extension(file)?;
        let mut servers = self.config.servers.iter().collect::<Vec<_>>();
        servers.sort_by_key(|(name, _)| *name);
        let (server_name, server, document) =
            servers.into_iter().find_map(|(server_name, server)| {
                let document = server.documents().into_iter().find(|document| {
                    document.file_extensions.iter().any(|candidate| {
                        candidate
                            .trim_start_matches('.')
                            .eq_ignore_ascii_case(&extension)
                    })
                })?;
                Some((server_name, server, document))
            })?;

        let path = Path::new(file);
        let path = path.absolutize().ok()?.to_path_buf();
        let workspace_root = find_workspace_root(&path, server);
        let uri = file_uri(&path).ok()?;

        Some(DocumentInfo {
            path,
            uri,
            language_id: document.language_id,
            workspace_root,
            server_name: server_name.clone(),
        })
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
            let config = self
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
        }

        Ok(self.clients.get_mut(&key))
    }

    async fn client_for_file(
        &mut self,
        file: &str,
    ) -> Result<Option<&mut RealLspClient>, LspError> {
        let Some(document) = self.resolve_document(file) else {
            return Ok(None);
        };
        self.client_for_document(&document).await
    }

    fn client_for_uri_mut(&mut self, uri: &str) -> Option<&mut RealLspClient> {
        let file = file_path(uri).ok()?;
        let document = self.resolve_document(&file)?;
        let key = client_key(&document);
        self.clients.get_mut(&key)
    }

    fn first_client_mut(&mut self) -> Option<&mut RealLspClient> {
        self.clients.values_mut().next()
    }
}

fn client_key(document: &DocumentInfo) -> String {
    format!(
        "{}:{}",
        document.server_name,
        document.workspace_root.display()
    )
}

fn client_source_from_key(key: &str) -> (&str, &str) {
    key.split_once(':').unwrap_or((key, ""))
}

fn document_key(document: &DocumentInfo) -> String {
    format!("{}:{}", client_key(document), document.uri)
}

fn find_workspace_root(path: &Path, server: &LanguageServerConfig) -> PathBuf {
    let start = path.parent().unwrap_or(path);

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

#[async_trait::async_trait]
impl LspClient for LspManager {
    async fn initialize(&mut self) -> Result<(), LspError> {
        Ok(())
    }

    async fn did_open(&mut self, file: &str, contents: &str) -> Result<(), LspError> {
        let Some(document) = self.resolve_document(file) else {
            return Ok(());
        };
        let key = document_key(&document);
        if self.opened_documents.contains(&key) {
            return Ok(());
        }

        let Some(client) = self.client_for_document(&document).await? else {
            return Ok(());
        };
        client
            .did_open_with_language_id(file, contents, &document.language_id)
            .await?;
        self.opened_documents.insert(key);
        Ok(())
    }

    async fn did_change(&mut self, file: &str, contents: String) -> Result<(), LspError> {
        let Some(document) = self.resolve_document(file) else {
            return Ok(());
        };
        let key = document_key(&document);
        let needs_open = !self.opened_documents.contains(&key);
        let Some(client) = self.client_for_document(&document).await? else {
            return Ok(());
        };

        if needs_open {
            client
                .did_open_with_language_id(file, &contents, &document.language_id)
                .await?;
        }
        let result = client.did_change(file, contents).await;
        if needs_open {
            self.opened_documents.insert(key);
        }
        result
    }

    async fn did_close(&mut self, file: &str) -> Result<(), LspError> {
        let Some(document) = self.resolve_document(file) else {
            return Ok(());
        };
        self.opened_documents.remove(&document_key(&document));
        let key = client_key(&document);
        if let Some(client) = self.clients.get_mut(&key) {
            client.did_close(file).await?;
        }
        Ok(())
    }

    async fn will_save(&mut self, file: &str) -> Result<(), LspError> {
        if let Some(client) = self.client_for_file(file).await? {
            client.will_save(file).await?;
        }
        Ok(())
    }

    async fn hover(&mut self, file: &str, x: usize, y: usize) -> Result<i64, LspError> {
        if let Some(client) = self.client_for_file(file).await? {
            return client.hover(file, x, y).await;
        }
        Ok(0)
    }

    async fn goto_definition(&mut self, file: &str, x: usize, y: usize) -> Result<i64, LspError> {
        if let Some(client) = self.client_for_file(file).await? {
            return client.goto_definition(file, x, y).await;
        }
        Ok(0)
    }

    async fn completion(&mut self, file: &str, x: usize, y: usize) -> Result<i64, LspError> {
        if let Some(client) = self.client_for_file(file).await? {
            return client.completion(file, x, y).await;
        }
        Ok(0)
    }

    async fn format_document(&mut self, file: &str) -> Result<i64, LspError> {
        if let Some(client) = self.client_for_file(file).await? {
            return client.format_document(file).await;
        }
        Ok(0)
    }

    async fn format_document_with_options(
        &mut self,
        file: &str,
        tab_size: usize,
        insert_spaces: bool,
    ) -> Result<i64, LspError> {
        if let Some(client) = self.client_for_file(file).await? {
            return client
                .format_document_with_options(file, tab_size, insert_spaces)
                .await;
        }
        Ok(0)
    }

    async fn document_symbols(&mut self, file: &str) -> Result<i64, LspError> {
        if let Some(client) = self.client_for_file(file).await? {
            return client.document_symbols(file).await;
        }
        Ok(0)
    }

    async fn code_action(
        &mut self,
        file: &str,
        range: Range,
        diagnostics: Vec<Diagnostic>,
    ) -> Result<i64, LspError> {
        if let Some(client) = self.client_for_file(file).await? {
            return client.code_action(file, range, diagnostics).await;
        }
        Ok(0)
    }

    async fn signature_help(&mut self, file: &str, x: usize, y: usize) -> Result<i64, LspError> {
        if let Some(client) = self.client_for_file(file).await? {
            return client.signature_help(file, x, y).await;
        }
        Ok(0)
    }

    async fn rename(
        &mut self,
        file: &str,
        x: usize,
        y: usize,
        new_name: &str,
    ) -> Result<i64, LspError> {
        if let Some(client) = self.client_for_file(file).await? {
            return client.rename(file, x, y, new_name).await;
        }
        Ok(0)
    }

    async fn document_highlight(
        &mut self,
        file: &str,
        x: usize,
        y: usize,
    ) -> Result<i64, LspError> {
        if let Some(client) = self.client_for_file(file).await? {
            return client.document_highlight(file, x, y).await;
        }
        Ok(0)
    }

    async fn document_link(&mut self, file: &str) -> Result<i64, LspError> {
        if let Some(client) = self.client_for_file(file).await? {
            return client.document_link(file).await;
        }
        Ok(0)
    }

    async fn document_color(&mut self, file: &str) -> Result<i64, LspError> {
        if let Some(client) = self.client_for_file(file).await? {
            return client.document_color(file).await;
        }
        Ok(0)
    }

    async fn folding_range(&mut self, file: &str) -> Result<i64, LspError> {
        if let Some(client) = self.client_for_file(file).await? {
            return client.folding_range(file).await;
        }
        Ok(0)
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
        if let Some(client) = self.client_for_file(file).await? {
            return client.workspace_symbol(query).await;
        }
        Ok(0)
    }

    async fn references(
        &mut self,
        file: &str,
        x: usize,
        y: usize,
        include_declaration: bool,
    ) -> Result<i64, LspError> {
        if let Some(client) = self.client_for_file(file).await? {
            return client.references(file, x, y, include_declaration).await;
        }
        Ok(0)
    }

    async fn call_hierarchy_prepare(
        &mut self,
        file: &str,
        x: usize,
        y: usize,
    ) -> Result<i64, LspError> {
        if let Some(client) = self.client_for_file(file).await? {
            return client.call_hierarchy_prepare(file, x, y).await;
        }
        Ok(0)
    }

    async fn semantic_tokens_full(&mut self, file: &str) -> Result<i64, LspError> {
        if let Some(client) = self.client_for_file(file).await? {
            return client.semantic_tokens_full(file).await;
        }
        Ok(0)
    }

    async fn inlay_hint(&mut self, file: &str, range: Range) -> Result<i64, LspError> {
        if let Some(client) = self.client_for_file(file).await? {
            return client.inlay_hint(file, range).await;
        }
        Ok(0)
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
        if let Some(client) = self.client_for_file(file).await? {
            return client.send_request(method, params, force).await;
        }
        Ok(0)
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
        if let Some(client) = self.client_for_uri_mut(file_uri) {
            return client
                .request_completion(file_uri, line, character, trigger_character)
                .await;
        }
        Ok(0)
    }

    async fn request_diagnostics(&mut self, file_uri: &str) -> Result<Option<i64>, LspError> {
        if let Some(client) = self.client_for_uri_mut(file_uri) {
            return client.request_diagnostics(file_uri).await;
        }
        Ok(None)
    }

    async fn recv_response(
        &mut self,
    ) -> Result<Option<(InboundMessage, Option<String>)>, LspError> {
        let mut keys = self.clients.keys().cloned().collect::<Vec<_>>();
        keys.sort();
        if keys.is_empty() {
            return Ok(None);
        }
        let start = self.next_client_poll % keys.len();
        for offset in 0..keys.len() {
            let index = (start + offset) % keys.len();
            let client_key = &keys[index];
            let Some(client) = self.clients.get_mut(client_key) else {
                continue;
            };
            if let Some((mut message, method)) = client.recv_response().await? {
                self.next_client_poll = (index + 1) % keys.len();
                if let InboundMessage::Notification(ParsedNotification::Progress(progress)) =
                    &mut message
                {
                    let (server_name, workspace_root) = client_source_from_key(client_key);
                    progress.enrich(server_name, workspace_root);
                }
                if let InboundMessage::ServerRequest(request) = &mut message {
                    request.source = Some(client_key.clone());
                }
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
        let document = self.resolve_document(file)?;
        self.clients
            .get(&client_key(&document))?
            .get_server_capabilities()
    }

    fn supports_document_formatting(&self, file: &str) -> bool {
        let Some(document) = self.resolve_document(file) else {
            return false;
        };
        self.clients
            .get(&client_key(&document))
            .is_some_and(|client| client.supports_document_formatting(file))
    }

    fn document_version(&self, file: &str) -> Option<i64> {
        let document = self.resolve_document(file)?;
        self.clients
            .get(&client_key(&document))?
            .document_version(file)
    }

    fn workspace_root_for_file(&self, file: &str) -> Option<PathBuf> {
        self.resolve_document(file)
            .map(|document| document.workspace_root)
    }

    fn workspace_root_for_request(&self, request: &ServerRequest) -> Option<PathBuf> {
        request
            .source
            .as_deref()
            .and_then(|source| self.clients.get(source))
            .and_then(|client| client.workspace_root_for_request(request))
    }

    async fn respond_workspace_edit(
        &mut self,
        request: &ServerRequest,
        applied: bool,
        failure_reason: Option<&str>,
    ) -> Result<(), LspError> {
        let Some(source) = request.source.as_deref() else {
            return Err(LspError::ProtocolError(
                "LSP workspace edit request is missing its server source".to_string(),
            ));
        };
        let Some(client) = self.clients.get_mut(source) else {
            return Err(LspError::ProtocolError(format!(
                "LSP workspace edit server is no longer available: {source}"
            )));
        };
        client
            .respond_workspace_edit(request, applied, failure_reason)
            .await
    }

    async fn shutdown(&mut self) -> Result<(), LspError> {
        for client in self.clients.values_mut() {
            client.shutdown().await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::{
        config::{LanguageDocumentConfig, LanguageServerConfig},
        lsp::OutboundMessage,
    };

    use super::*;

    fn server(language_id: &str, extensions: &[&str]) -> LanguageServerConfig {
        LanguageServerConfig {
            command: "mock-lsp".to_string(),
            args: Vec::new(),
            language_id: language_id.to_string(),
            file_extensions: extensions.iter().map(|ext| ext.to_string()).collect(),
            documents: Vec::new(),
            root_markers: vec![".git".to_string()],
            env: HashMap::new(),
            initialization_options: None,
            workspace_name: None,
        }
    }

    fn multi_document_server(documents: &[(&str, &[&str])]) -> LanguageServerConfig {
        LanguageServerConfig {
            command: "mock-lsp".to_string(),
            args: Vec::new(),
            language_id: String::new(),
            file_extensions: Vec::new(),
            documents: documents
                .iter()
                .map(|(language_id, extensions)| LanguageDocumentConfig {
                    language_id: language_id.to_string(),
                    file_extensions: extensions.iter().map(|ext| ext.to_string()).collect(),
                })
                .collect(),
            root_markers: vec![".git".to_string()],
            env: HashMap::new(),
            initialization_options: None,
            workspace_name: None,
        }
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
    fn unresolved_language_returns_none() {
        let manager = LspManager::new(LspConfig {
            enabled: true,
            format_on_save: false,
            servers: HashMap::from([("rust".to_string(), server("rust", &["rs"]))]),
        });

        assert!(manager.resolve_document("README.md").is_none());
    }

    #[test]
    fn overlapping_servers_resolve_deterministically_by_name() {
        let manager = LspManager::new(LspConfig {
            enabled: true,
            format_on_save: false,
            servers: HashMap::from([
                ("zeta".to_string(), server("zeta", &["rs"])),
                ("alpha".to_string(), server("alpha", &["rs"])),
            ]),
        });

        let document = manager.resolve_document("example.rs").unwrap();
        assert_eq!(document.server_name, "alpha");
        assert_eq!(document.language_id, "alpha");
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
        assert_eq!(
            methods,
            [
                "textDocument/didOpen",
                "textDocument/didChange",
                "textDocument/didChange"
            ]
        );
        assert_eq!(manager.opened_documents.len(), 1);
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
