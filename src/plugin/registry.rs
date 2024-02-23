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

        for (name, plugin) in &self.plugins {
            code += &format!(
                r#"
                    import {{ activate }} from '{plugin}';
                    globalThis.plugins.{name} = activate();
                "#,
            );
        }

        runtime.run(&code).await?;

        Ok(())
    }
}

// #[cfg(test)]
// mod tests {
//     use super::*;
//
//     #[test]
//     fn test_registry_init() {
//         let mut registry = PluginRegistry::new();
//         registry.add("start", "/home/fcoury/.config/red/plugins/start.js");
//         registry.initialize().unwrap();
//     }
// }
