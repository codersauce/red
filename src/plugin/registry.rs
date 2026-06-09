use serde_json::json;
use std::collections::HashMap;
use std::path::Path;

use crate::editor::EditorStateSnapshot;

use super::{PluginMetadata, Runtime};

pub struct PluginRegistry {
    plugins: Vec<(String, String)>,
    metadata: HashMap<String, PluginMetadata>,
    initialized: bool,
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
            metadata: HashMap::new(),
            initialized: false,
        }
    }

    pub fn add(&mut self, name: &str, path: &str) {
        self.plugins.push((name.to_string(), path.to_string()));

        // Try to load metadata from package.json in the plugin directory
        let plugin_path = Path::new(path);
        if let Some(dir) = plugin_path.parent() {
            let package_json = dir.join("package.json");
            if package_json.exists() {
                match PluginMetadata::from_file(&package_json) {
                    Ok(metadata) => {
                        self.metadata.insert(name.to_string(), metadata);
                    }
                    Err(e) => {
                        // If no package.json or invalid, create minimal metadata
                        crate::log!("Failed to load metadata for plugin {}: {}", name, e);
                        self.metadata
                            .insert(name.to_string(), PluginMetadata::minimal(name.to_string()));
                    }
                }
            } else {
                // No package.json, use minimal metadata
                self.metadata
                    .insert(name.to_string(), PluginMetadata::minimal(name.to_string()));
            }
        }
    }

    /// Get metadata for a specific plugin
    pub fn get_metadata(&self, name: &str) -> Option<&PluginMetadata> {
        self.metadata.get(name)
    }

    /// Get all plugin metadata
    pub fn all_metadata(&self) -> &HashMap<String, PluginMetadata> {
        &self.metadata
    }

    pub async fn initialize(&mut self, runtime: &mut Runtime) -> anyhow::Result<()> {
        self.ensure_plugin_files_exist()?;

        let mut code = r#"
            globalThis.plugins = {}; 
            globalThis.pluginInstances = {};
        "#
        .to_string();

        for (i, (name, plugin)) in self.plugins.iter().enumerate() {
            let plugin_specifier = plugin_import_specifier(plugin)?;
            code += &format!(
                r#"
                    import * as plugin_{i} from {};
                    const activate_{i} = plugin_{i}.activate;
                    const deactivate_{i} = plugin_{i}.deactivate || null;
                    const before_exit_{i} = plugin_{i}.beforeExit || null;
                    
                    globalThis.plugins['{name}'] = activate_{i};
                    
                    // Store plugin instance for lifecycle management
                    globalThis.pluginInstances['{name}'] = {{
                        activate: activate_{i},
                        deactivate: deactivate_{i},
                        beforeExit: before_exit_{i},
                        context: null
                    }};
                    
                    // Activate the plugin
                    globalThis.pluginInstances['{name}'].context = globalThis.createPluginContext('{name}');
                    if (activate_{i}) {{
                        Promise.resolve(activate_{i}(globalThis.pluginInstances['{name}'].context))
                            .catch((error) => globalThis.log(`Error activating plugin {name}:`, error));
                    }}
                "#,
                json!(plugin_specifier),
            );
        }

        runtime.add_module(&code).await?;
        self.initialized = true;

        Ok(())
    }

    fn ensure_plugin_files_exist(&self) -> anyhow::Result<()> {
        for (name, plugin) in &self.plugins {
            let path = Path::new(plugin);
            if path.is_file() {
                continue;
            }

            let issue = if path.exists() {
                "that path exists, but it is not a file"
            } else {
                "that file does not exist"
            };

            return Err(anyhow::anyhow!(
                "Could not load plugin `{}`.\n\nRed was asked to load this plugin file, but {}:\n  {}\n\nCheck the `{}` entry under `[plugins]` in your config.toml, or put the plugin file back at that path.",
                name,
                issue,
                path.display(),
                name
            ));
        }

        Ok(())
    }

    pub async fn execute(&mut self, runtime: &mut Runtime, command: &str) -> anyhow::Result<()> {
        let code = format!(
            r#"
                Promise.resolve(globalThis.execute({}))
                    .catch((error) => globalThis.log(`Error executing command {command}:`, error));
            "#,
            json!(command),
        );

        runtime.run(&code).await?;

        Ok(())
    }

    pub async fn notify(
        &self,
        runtime: &mut Runtime,
        event: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<()> {
        let code = format!(
            r#"
                globalThis.context.notify('{}', {});
            "#,
            event,
            json!(args)
        );

        runtime.run(&code).await?;

        Ok(())
    }

    pub async fn before_exit(
        &self,
        runtime: &mut Runtime,
        snapshot: EditorStateSnapshot,
    ) -> anyhow::Result<()> {
        if !self.initialized {
            return Ok(());
        }

        let code = format!(
            r#"
                (async () => {{
                    const state = {};
                    for (const [name, plugin] of Object.entries(globalThis.pluginInstances)) {{
                        if (plugin.beforeExit) {{
                            try {{
                                await plugin.beforeExit(plugin.context, state);
                                globalThis.log(`Plugin ${{name}} beforeExit completed`);
                            }} catch (error) {{
                                globalThis.log(`Error in beforeExit for plugin ${{name}}:`, error);
                            }}
                        }}
                    }}
                }})();
            "#,
            json!(snapshot)
        );

        runtime.run(&code).await
    }

    /// Deactivate all plugins (call their deactivate functions if available)
    pub async fn deactivate_all(&mut self, runtime: &mut Runtime) -> anyhow::Result<()> {
        if !self.initialized {
            return Ok(());
        }

        let code = r#"
            (async () => {
                for (const [name, plugin] of Object.entries(globalThis.pluginInstances)) {
                    if (plugin.deactivate) {
                        try {
                            await plugin.deactivate(plugin.context);
                            globalThis.log(`Plugin ${name} deactivated`);
                        } catch (error) {
                            globalThis.log(`Error deactivating plugin ${name}:`, error);
                        }
                    }
                }
                
                // Clear event subscriptions
                globalThis.context.eventSubscriptions = {};
                
                // Clear commands
                globalThis.context.commands = {};
                
                // Clear plugin instances
                globalThis.pluginInstances = {};
                globalThis.plugins = {};
            })();
        "#;

        runtime.run(code).await?;
        self.initialized = false;

        Ok(())
    }

    /// Reload all plugins (deactivate then reactivate)
    pub async fn reload(&mut self, runtime: &mut Runtime) -> anyhow::Result<()> {
        self.deactivate_all(runtime).await?;
        self.initialize(runtime).await?;
        Ok(())
    }
}

fn plugin_import_specifier(plugin: &str) -> anyhow::Result<String> {
    Ok(deno_core::resolve_url_or_path(plugin, Path::new("."))?.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{PluginRequest, ACTION_DISPATCHER, PLUGIN_DISPATCHER_TEST_LOCK};
    use serde_json::Value;
    use std::time::Duration;

    fn drain_plugin_requests() {
        while ACTION_DISPATCHER.try_recv_request().is_some() {}
    }

    #[tokio::test]
    async fn initialize_explains_missing_plugin_file() {
        let mut registry = PluginRegistry::new();
        let missing_path = std::env::temp_dir().join(format!(
            "red-plugin-that-is-not-here-{}.js",
            uuid::Uuid::new_v4()
        ));
        registry.add("missing", missing_path.to_string_lossy().as_ref());

        let mut runtime = Runtime::new();
        let error = registry.initialize(&mut runtime).await.unwrap_err();
        let message = error.to_string();

        assert!(message.contains("Could not load plugin `missing`."));
        assert!(message.contains("that file does not exist"));
        assert!(message.contains("`[plugins]`"));
    }

    #[test]
    fn plugin_import_specifier_uses_file_url() {
        let plugin_path = std::env::temp_dir().join("red-plugin-import-specifier.js");
        let specifier = plugin_import_specifier(plugin_path.to_string_lossy().as_ref()).unwrap();

        assert!(specifier.starts_with("file://"));
        assert!(specifier.ends_with("red-plugin-import-specifier.js"));
    }

    #[tokio::test]
    async fn plugin_command_yields_while_awaiting_editor_response() {
        let _guard = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_plugin_requests();

        let plugin_path = std::env::temp_dir().join(format!(
            "red-async-panel-command-{}.js",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &plugin_path,
            r#"
                export function activate(red) {
                    red.addCommand("AsyncPanel", async () => {
                        red.createPanel("tree", { side: "left", width: 10 });
                        const cwd = await red.getConfig("cwd");
                        red.updatePanel("tree", [{
                            id: "root",
                            path: cwd,
                            kind: "directory",
                            segments: [{ text: String(cwd) }],
                        }]);
                    });
                }
            "#,
        )
        .unwrap();

        let mut registry = PluginRegistry::new();
        registry.add("async_panel", plugin_path.to_string_lossy().as_ref());
        let mut runtime = Runtime::new();
        registry.initialize(&mut runtime).await.unwrap();

        tokio::time::timeout(
            Duration::from_millis(250),
            registry.execute(&mut runtime, "AsyncPanel"),
        )
        .await
        .expect("plugin command should not block the editor while awaiting config")
        .unwrap();

        assert!(matches!(
            ACTION_DISPATCHER.try_recv_request(),
            Some(PluginRequest::CreatePanel { id, .. }) if id == "tree"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.try_recv_request(),
            Some(PluginRequest::GetConfig { key }) if key.as_deref() == Some("cwd")
        ));

        registry
            .notify(
                &mut runtime,
                "config:value",
                json!({ "value": "/tmp/red-workspace" }),
            )
            .await
            .unwrap();

        assert!(matches!(
            ACTION_DISPATCHER.try_recv_request(),
            Some(PluginRequest::UpdatePanel { id, rows })
                if id == "tree"
                    && rows.len() == 1
                    && rows[0].segments[0].text == "/tmp/red-workspace"
        ));

        let _ = std::fs::remove_file(plugin_path);
    }

    #[tokio::test]
    async fn lsp_navigation_commands_use_runtime_lsp_api() {
        let _guard = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_plugin_requests();

        let plugin_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("plugins")
            .join("lsp_symbols.ts");
        let mut registry = PluginRegistry::new();
        registry.add("lsp_symbols", plugin_path.to_string_lossy().as_ref());
        let mut runtime = Runtime::new();
        registry.initialize(&mut runtime).await.unwrap();

        registry
            .execute(&mut runtime, "LspWorkspaceSymbols")
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.try_recv_request(),
            Some(PluginRequest::GetConfig { key: Some(key) }) if key == "plugin_config"
        ));
        registry
            .notify(
                &mut runtime,
                "config:value",
                json!({
                    "value": {
                        "lsp_symbols": {
                            "icons": { "overrides": { "function": "FN" } }
                        }
                    }
                }),
            )
            .await
            .unwrap();
        let picker_id = match ACTION_DISPATCHER.try_recv_request() {
            Some(PluginRequest::OpenDynamicPicker {
                title,
                id,
                items,
                options,
            }) => {
                assert_eq!(title.as_deref(), Some("Workspace Symbols"));
                assert!(items.is_empty());
                assert!(options.external_filter);
                id
            }
            _ => panic!("expected the workspace symbol picker to open"),
        };
        assert!(matches!(
            ACTION_DISPATCHER.try_recv_request(),
            Some(PluginRequest::UpdatePickerStatus { id, .. }) if id == picker_id
        ));
        let workspace_request_id = match ACTION_DISPATCHER.try_recv_request() {
            Some(PluginRequest::WorkspaceSymbols { request_id, query }) => {
                assert_eq!(query, "");
                request_id
            }
            _ => panic!("expected a workspace symbols request"),
        };
        registry
            .notify(
                &mut runtime,
                &format!("lsp:workspace_symbols:{workspace_request_id}"),
                json!({
                    "ok": true,
                    "symbols": [{
                        "name": "render",
                        "kind": 12,
                        "kindName": "Function",
                        "file": "/tmp/project/src/main.rs",
                        "range": {
                            "start": { "line": 3, "character": 2 },
                            "end": { "line": 3, "character": 8 }
                        },
                        "selectionRange": {
                            "start": { "line": 3, "character": 2 },
                            "end": { "line": 3, "character": 8 }
                        },
                        "depth": 0
                    }]
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.try_recv_request(),
            Some(PluginRequest::UpdatePickerItems { id, items })
                if id == picker_id
                    && items.len() == 1
                    && items[0].label == "FN render"
                    && items[0].kind.as_deref() == Some("Function")
                    && items[0].preview.is_some()
        ));
        assert!(matches!(
            ACTION_DISPATCHER.try_recv_request(),
            Some(PluginRequest::UpdatePickerStatus { id, .. }) if id == picker_id
        ));
        registry
            .notify(
                &mut runtime,
                &format!("picker:cancelled:{picker_id}"),
                Value::Null,
            )
            .await
            .unwrap();

        registry
            .execute(&mut runtime, "LspReferences")
            .await
            .unwrap();
        let references_request_id = match ACTION_DISPATCHER.try_recv_request() {
            Some(PluginRequest::References {
                request_id,
                include_declaration,
            }) => {
                assert!(include_declaration);
                request_id
            }
            _ => panic!("expected a references request"),
        };
        assert!(matches!(
            ACTION_DISPATCHER.try_recv_request(),
            Some(PluginRequest::GetConfig { key: Some(key) }) if key == "plugin_config"
        ));
        registry
            .notify(&mut runtime, "config:value", json!({ "value": {} }))
            .await
            .unwrap();
        registry
            .notify(
                &mut runtime,
                &format!("lsp:references:{references_request_id}"),
                json!({
                    "ok": true,
                    "file": "/tmp/project/src/main.rs",
                    "position": { "line": 1, "character": 4 },
                    "references": [{
                        "file": "/tmp/project/src/lib.rs",
                        "range": {
                            "start": { "line": 8, "character": 2 },
                            "end": { "line": 8, "character": 6 }
                        }
                    }]
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.try_recv_request(),
            Some(PluginRequest::OpenLocation { location, target })
                if location.path == "/tmp/project/src/lib.rs"
                    && location.line == 8
                    && location.column == 2
                    && location.column_encoding == crate::plugin::LocationColumnEncoding::Utf16
                    && target == crate::plugin::OpenLocationTarget::Current
        ));
    }
}
