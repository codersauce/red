use serde_json::json;

use super::Runtime;

pub struct PluginRegistry {
    plugins: Vec<(String, String)>,
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
            initialized: false,
        }
    }

    pub fn add(&mut self, name: &str, path: &str) {
        self.plugins.push((name.to_string(), path.to_string()));
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
                    
                    globalThis.plugins['{name}'] = activate_{i};
                    
                    // Store plugin instance for lifecycle management
                    globalThis.pluginInstances['{name}'] = {{
                        activate: activate_{i},
                        deactivate: deactivate_{i},
                        context: null
                    }};
                    
                    // Activate the plugin
                    globalThis.pluginInstances['{name}'].context = activate_{i}(globalThis.context);
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
                            await plugin.deactivate();
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
