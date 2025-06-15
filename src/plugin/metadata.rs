use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Plugin metadata structure based on package.json format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMetadata {
    /// Plugin name (required)
    pub name: String,
    
    /// Plugin version following semver
    #[serde(default = "default_version")]
    pub version: String,
    
    /// Plugin description
    pub description: Option<String>,
    
    /// Plugin author (name or name <email>)
    pub author: Option<String>,
    
    /// Plugin license
    pub license: Option<String>,
    
    /// Main entry point (defaults to index.js)
    #[serde(default = "default_main")]
    pub main: String,
    
    /// Plugin homepage URL
    pub homepage: Option<String>,
    
    /// Repository information
    pub repository: Option<Repository>,
    
    /// Keywords for plugin discovery
    #[serde(default)]
    pub keywords: Vec<String>,
    
    /// Red editor compatibility
    pub engines: Option<Engines>,
    
    /// Plugin dependencies (other plugins)
    #[serde(default)]
    pub dependencies: HashMap<String, String>,
    
    /// Red API version compatibility
    pub red_api_version: Option<String>,
    
    /// Plugin configuration schema
    pub config_schema: Option<serde_json::Value>,
    
    /// Activation events (when to load the plugin)
    #[serde(default)]
    pub activation_events: Vec<String>,
    
    /// Plugin capabilities
    #[serde(default)]
    pub capabilities: PluginCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    #[serde(rename = "type")]
    pub repo_type: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Engines {
    pub red: Option<String>,
    pub node: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginCapabilities {
    /// Whether the plugin provides commands
    #[serde(default)]
    pub commands: bool,
    
    /// Whether the plugin uses event handlers
    #[serde(default)]
    pub events: bool,
    
    /// Whether the plugin modifies buffers
    #[serde(default)]
    pub buffer_manipulation: bool,
    
    /// Whether the plugin provides UI components
    #[serde(default)]
    pub ui_components: bool,
    
    /// Whether the plugin integrates with LSP
    #[serde(default)]
    pub lsp_integration: bool,
}

fn default_version() -> String {
    "0.1.0".to_string()
}

fn default_main() -> String {
    "index.js".to_string()
}

impl PluginMetadata {
    /// Load metadata from a package.json file
    pub fn from_file(path: &std::path::Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let metadata: PluginMetadata = serde_json::from_str(&content)?;
        Ok(metadata)
    }
    
    /// Create minimal metadata with just a name
    pub fn minimal(name: String) -> Self {
        Self {
            name,
            version: default_version(),
            description: None,
            author: None,
            license: None,
            main: default_main(),
            homepage: None,
            repository: None,
            keywords: vec![],
            engines: None,
            dependencies: HashMap::new(),
            red_api_version: None,
            config_schema: None,
            activation_events: vec![],
            capabilities: PluginCapabilities::default(),
        }
    }
    
    /// Check if the plugin is compatible with the current Red version
    pub fn is_compatible(&self, red_version: &str) -> bool {
        if let Some(engines) = &self.engines {
            if let Some(required_red) = &engines.red {
                // Simple version check - could be enhanced with semver
                return required_red == "*" || red_version.starts_with(required_red);
            }
        }
        true // If no version specified, assume compatible
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_minimal_metadata() {
        let metadata = PluginMetadata::minimal("test-plugin".to_string());
        assert_eq!(metadata.name, "test-plugin");
        assert_eq!(metadata.version, "0.1.0");
        assert_eq!(metadata.main, "index.js");
    }
    
    #[test]
    fn test_deserialize_metadata() {
        let json = r#"{
            "name": "awesome-plugin",
            "version": "1.0.0",
            "description": "An awesome plugin for Red editor",
            "author": "John Doe <john@example.com>",
            "keywords": ["productivity", "tools"],
            "capabilities": {
                "commands": true,
                "events": true
            }
        }"#;
        
        let metadata: PluginMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(metadata.name, "awesome-plugin");
        assert_eq!(metadata.version, "1.0.0");
        assert_eq!(metadata.description, Some("An awesome plugin for Red editor".to_string()));
        assert_eq!(metadata.keywords.len(), 2);
        assert!(metadata.capabilities.commands);
        assert!(metadata.capabilities.events);
    }
}