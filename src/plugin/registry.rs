use serde_json::json;

use super::Runtime;

pub struct PluginRegistry {
    plugins: Vec<(String, String)>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
        }
    }

    pub fn add(&mut self, name: &str, path: &str) {
        self.plugins.push((name.to_string(), path.to_string()));
    }

    pub async fn initialize(&mut self, runtime: &mut Runtime) -> anyhow::Result<()> {
        let mut code = r#"
            globalThis.plugins = []; 
        "#
        .to_string();

        for (i, (name, plugin)) in self.plugins.iter().enumerate() {
            code += &format!(
                r#"
                    import {{ activate as activate_{i} }} from '{plugin}';
                    globalThis.plugins.{name} = activate_{i}(globalThis.context);
                "#,
            );
        }

        runtime.add_module(&code).await?;

        Ok(())
    }

    pub async fn execute(&mut self, runtime: &mut Runtime, command: &str) -> anyhow::Result<()> {
        let code = format!(
            r#"
                globalThis.execute('{command}');
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
}
