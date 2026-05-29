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
        let mut code = r#"
            globalThis.plugins = {}; 
            globalThis.pluginInstances = {};
        "#
        .to_string();

        for (i, (name, plugin)) in self.plugins.iter().enumerate() {
            code += &format!(
                r#"
                    import * as plugin_{i} from '{plugin}';
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
            );
        }

        runtime.add_module(&code).await?;
        self.initialized = true;

        Ok(())
    }

    pub async fn execute(&mut self, runtime: &mut Runtime, command: &str) -> anyhow::Result<()> {
        let code = format!(
            r#"
                (async () => {{
                    return await globalThis.execute('{command}');
                }})();
            "#,
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
                globalThis.context.commandOwners = {};
                globalThis.context.commandMetadata = {};
                
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
