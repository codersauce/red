use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use path_absolutize::Absolutize;
use serde_json::Value;

use crate::config::{LanguageServerConfig, LspConfig};

use super::{
    Diagnostic, InboundMessage, LspClient, LspError, Range, RealLspClient, ServerCapabilities,
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
}

impl LspManager {
    pub fn new(config: LspConfig) -> Self {
        Self {
            config,
            clients: HashMap::new(),
        }
    }

    pub fn resolve_document(&self, file: &str) -> Option<DocumentInfo> {
        if !self.config.enabled {
            return None;
        }

        let path = Path::new(file);
        let extension = path.extension()?.to_string_lossy().to_ascii_lowercase();
        let (server_name, server) = self.config.servers.iter().find(|(_, server)| {
            server.file_extensions.iter().any(|candidate| {
                candidate
                    .trim_start_matches('.')
                    .eq_ignore_ascii_case(&extension)
            })
        })?;

        let path = path.absolutize().ok()?.to_path_buf();
        let workspace_root = find_workspace_root(&path, server);
        let uri = file_uri(&path);

        Some(DocumentInfo {
            path,
            uri,
            language_id: server.language_id.clone(),
            workspace_root,
            server_name: server_name.clone(),
        })
    }

    async fn client_for_file(
        &mut self,
        file: &str,
    ) -> Result<Option<&mut RealLspClient>, LspError> {
        let Some(document) = self.resolve_document(file) else {
            return Ok(None);
        };
        let key = client_key(&document);

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

            let mut client = RealLspClient::start(config, document.workspace_root).await?;
            client.initialize().await?;
            self.clients.insert(key.clone(), client);
        }

        Ok(self.clients.get_mut(&key))
    }

    fn client_for_uri_mut(&mut self, uri: &str) -> Option<&mut RealLspClient> {
        let file = uri.strip_prefix("file://").unwrap_or(uri);
        let document = self.resolve_document(file)?;
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

fn file_uri(path: &Path) -> String {
    format!("file://{}", path.to_string_lossy())
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
        if let Some(client) = self.client_for_file(file).await? {
            client.did_open(file, contents).await?;
        }
        Ok(())
    }

    async fn did_change(&mut self, file: &str, contents: &str) -> Result<(), LspError> {
        if let Some(client) = self.client_for_file(file).await? {
            client.did_change(file, contents).await?;
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
    ) -> Result<i64, LspError> {
        if let Some(client) = self.client_for_uri_mut(file_uri) {
            return client.request_completion(file_uri, line, character).await;
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
        for client in self.clients.values_mut() {
            if let Some(message) = client.recv_response().await? {
                return Ok(Some(message));
            }
        }
        Ok(None)
    }

    fn get_server_capabilities(&self) -> Option<&ServerCapabilities> {
        self.clients
            .values()
            .find_map(|client| client.get_server_capabilities())
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

    use crate::config::LanguageServerConfig;

    use super::*;

    fn server(language_id: &str, extensions: &[&str]) -> LanguageServerConfig {
        LanguageServerConfig {
            command: "mock-lsp".to_string(),
            args: Vec::new(),
            language_id: language_id.to_string(),
            file_extensions: extensions.iter().map(|ext| ext.to_string()).collect(),
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
            servers: HashMap::from([
                ("rust".to_string(), server("rust", &["rs"])),
                ("python".to_string(), server("python", &["py"])),
            ]),
        });

        let document = manager.resolve_document("example.py").unwrap();
        assert_eq!(document.language_id, "python");
        assert_eq!(document.server_name, "python");
        assert_eq!(document.uri, format!("file://{}", document.path.display()));
    }

    #[test]
    fn unresolved_language_returns_none() {
        let manager = LspManager::new(LspConfig {
            enabled: true,
            servers: HashMap::from([("rust".to_string(), server("rust", &["rs"]))]),
        });

        assert!(manager.resolve_document("README.md").is_none());
    }

    #[test]
    fn disabled_lsp_returns_none() {
        let manager = LspManager::new(LspConfig {
            enabled: false,
            servers: HashMap::from([("rust".to_string(), server("rust", &["rs"]))]),
        });

        assert!(manager.resolve_document("src/main.rs").is_none());
    }
}
