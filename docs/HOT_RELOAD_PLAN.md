# Hot Reload Implementation Plan for Red Editor Plugin System

## Overview

This document outlines the implementation plan for adding hot reload capabilities to the Red editor plugin system. Hot reloading will allow plugins to be automatically reloaded when their source files change, without requiring an editor restart.

## Current Architecture Analysis

### Current State
- Plugins are loaded once during editor startup in the `run()` method
- Each plugin runs in a shared JavaScript runtime environment
- Plugin registry has a `reload()` method that deactivates and reactivates all plugins
- No file watching mechanism exists currently
- Plugins are loaded from `~/.config/red/plugins/` directory

### Key Components
- **PluginRegistry** (`src/plugin/registry.rs`): Manages plugin lifecycle
- **Runtime** (`src/plugin/runtime.rs`): Deno-based JavaScript runtime
- **Editor** (`src/editor.rs`): Main editor loop and plugin initialization

## Implementation Plan

### 1. File Watcher System (`src/plugin/watcher.rs`)

Create a new module for watching plugin files:

```rust
use notify::{Watcher, RecursiveMode, watcher, DebouncedEvent};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;
use std::path::PathBuf;

pub struct PluginWatcher {
    watcher: Box<dyn Watcher>,
    rx: Receiver<DebouncedEvent>,
    watched_plugins: HashMap<PathBuf, String>, // path -> plugin_name
}

impl PluginWatcher {
    pub fn new(debounce_ms: u64) -> Result<Self> {
        let (tx, rx) = channel();
        let watcher = watcher(tx, Duration::from_millis(debounce_ms))?;
        
        Ok(Self {
            watcher: Box::new(watcher),
            rx,
            watched_plugins: HashMap::new(),
        })
    }
    
    pub fn watch_plugin(&mut self, name: &str, path: &Path) -> Result<()> {
        self.watcher.watch(path, RecursiveMode::NonRecursive)?;
        self.watched_plugins.insert(path.to_path_buf(), name.to_string());
        Ok(())
    }
    
    pub fn check_changes(&mut self) -> Vec<(String, PathBuf)> {
        let mut changes = Vec::new();
        while let Ok(event) = self.rx.try_recv() {
            match event {
                DebouncedEvent::Write(path) | DebouncedEvent::Create(path) => {
                    if let Some(name) = self.watched_plugins.get(&path) {
                        changes.push((name.clone(), path));
                    }
                }
                _ => {}
            }
        }
        changes
    }
}
```

### 2. Update Plugin Registry (`src/plugin/registry.rs`)

#### 2.1 Add File Tracking

```rust
pub struct PluginRegistry {
    plugins: Vec<(String, String)>,
    metadata: HashMap<String, PluginMetadata>,
    file_paths: HashMap<String, PathBuf>,  // New: track actual file paths
    last_modified: HashMap<String, SystemTime>,  // New: track modification times
    initialized: bool,
}
```

#### 2.2 Implement Single Plugin Reload

```rust
impl PluginRegistry {
    /// Reload a single plugin
    pub async fn reload_plugin(&mut self, name: &str, runtime: &mut Runtime) -> anyhow::Result<()> {
        // 1. Deactivate the plugin
        self.deactivate_plugin(name, runtime).await?;
        
        // 2. Clear from module cache
        let clear_cache_code = format!(r#"
            // Clear module cache for the plugin
            delete globalThis.plugins['{}'];
            delete globalThis.pluginInstances['{}'];
            
            // Notify plugin it's being reloaded
            globalThis.context.notify('plugin:reloading', {{ name: '{}' }});
        "#, name, name, name);
        
        runtime.run(&clear_cache_code).await?;
        
        // 3. Re-read metadata if package.json exists
        if let Some(path) = self.file_paths.get(name) {
            if let Some(dir) = path.parent() {
                let package_json = dir.join("package.json");
                if package_json.exists() {
                    if let Ok(metadata) = PluginMetadata::from_file(&package_json) {
                        self.metadata.insert(name.to_string(), metadata);
                    }
                }
            }
        }
        
        // 4. Re-load the plugin
        if let Some((idx, (plugin_name, plugin_path))) = self.plugins.iter().enumerate().find(|(_, (n, _))| n == name) {
            let code = format!(r#"
                import * as plugin_{idx}_new from '{}?t={}';
                const activate_{idx}_new = plugin_{idx}_new.activate;
                const deactivate_{idx}_new = plugin_{idx}_new.deactivate || null;
                
                globalThis.plugins['{}'] = activate_{idx}_new;
                globalThis.pluginInstances['{}'] = {{
                    activate: activate_{idx}_new,
                    deactivate: deactivate_{idx}_new,
                    context: null
                }};
                
                // Activate the reloaded plugin
                globalThis.pluginInstances['{}'].context = activate_{idx}_new(globalThis.context);
                
                // Notify plugin it's been reloaded
                globalThis.context.notify('plugin:reloaded', {{ name: '{}' }});
            "#, plugin_path, SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis(),
                plugin_name, plugin_name, plugin_name, plugin_name);
            
            runtime.run(&code).await?;
        }
        
        Ok(())
    }
    
    async fn deactivate_plugin(&mut self, name: &str, runtime: &mut Runtime) -> anyhow::Result<()> {
        let code = format!(r#"
            (async () => {{
                const plugin = globalThis.pluginInstances['{}'];
                if (plugin && plugin.deactivate) {{
                    try {{
                        await plugin.deactivate();
                        globalThis.log(`Plugin {} deactivated for reload`);
                    }} catch (error) {{
                        globalThis.log(`Error deactivating plugin {} for reload:`, error);
                    }}
                }}
                
                // Clear this plugin's commands and event listeners
                for (const [cmd, fn] of Object.entries(globalThis.context.commands)) {{
                    // We need a way to track which commands belong to which plugin
                }
            }})();
        "#, name, name, name);
        
        runtime.run(&code).await?;
        Ok(())
    }
}
```

### 3. Update Editor (`src/editor.rs`)

#### 3.1 Add New Actions

```rust
pub enum Action {
    // ... existing actions ...
    ReloadPlugin(String),    // Reload a specific plugin by name
    ReloadAllPlugins,        // Reload all plugins
    ToggleHotReload,         // Enable/disable hot reload
    ShowReloadStatus,        // Show hot reload status
}
```

#### 3.2 Add Watcher Management

```rust
use crate::plugin::watcher::PluginWatcher;

pub struct Editor {
    // ... existing fields ...
    plugin_watcher: Option<PluginWatcher>,
    hot_reload_enabled: bool,
}
```

#### 3.3 Update Main Loop

In the `run()` method, after initializing plugins:

```rust
// Initialize hot reload if enabled
if self.config.dev.unwrap_or_default().hot_reload {
    let mut watcher = PluginWatcher::new(
        self.config.dev.unwrap_or_default().hot_reload_delay.unwrap_or(100)
    )?;
    
    // Watch all loaded plugin files
    for (name, path) in &self.config.plugins {
        let full_path = Config::path("plugins").join(path);
        watcher.watch_plugin(name, &full_path)?;
    }
    
    self.plugin_watcher = Some(watcher);
    self.hot_reload_enabled = true;
}
```

In the main event loop:

```rust
// Check for plugin file changes
if let Some(watcher) = &mut self.plugin_watcher {
    if self.hot_reload_enabled {
        let changes = watcher.check_changes();
        for (plugin_name, path) in changes {
            log!("Plugin file changed: {} ({})", plugin_name, path.display());
            
            // Reload the plugin
            match self.plugin_registry.reload_plugin(&plugin_name, &mut runtime).await {
                Ok(_) => {
                    self.last_error = Some(format!("Plugin '{}' reloaded", plugin_name));
                }
                Err(e) => {
                    self.last_error = Some(format!("Failed to reload plugin '{}': {}", plugin_name, e));
                }
            }
        }
    }
}
```

### 4. Update Runtime Communication

#### 4.1 Add New PluginRequest Variant

```rust
pub enum PluginRequest {
    // ... existing variants ...
    PluginFileChanged { plugin_name: String, file_path: String },
    GetPluginState { plugin_name: String },
    SetPluginState { plugin_name: String, state: Value },
}
```

### 5. JavaScript Runtime Updates (`src/plugin/runtime.js`)

#### 5.1 Improve Module Management

```javascript
// Track which commands belong to which plugin
let pluginCommands = {};
let pluginEventHandlers = {};

class RedContext {
    constructor() {
        this.commands = {};
        this.eventSubscriptions = {};
        this.pluginStates = {}; // Store state between reloads
    }
    
    addCommand(name, command, pluginName) {
        this.commands[name] = command;
        
        // Track which plugin owns this command
        if (pluginName) {
            if (!pluginCommands[pluginName]) {
                pluginCommands[pluginName] = [];
            }
            pluginCommands[pluginName].push(name);
        }
    }
    
    clearPluginCommands(pluginName) {
        const commands = pluginCommands[pluginName] || [];
        for (const cmd of commands) {
            delete this.commands[cmd];
        }
        delete pluginCommands[pluginName];
    }
    
    // State preservation for hot reload
    savePluginState(pluginName, state) {
        this.pluginStates[pluginName] = state;
    }
    
    getPluginState(pluginName) {
        return this.pluginStates[pluginName];
    }
}
```

#### 5.2 Add Reload Events

```javascript
// Allow plugins to handle reload events
export function onBeforeReload(red) {
    // Plugin can return state to preserve
    return { /* state to preserve */ };
}

export function onAfterReload(red, previousState) {
    // Plugin can restore state after reload
}
```

### 6. Configuration Updates

Add to `config.toml`:

```toml
[dev]
# Enable hot reload in development
hot_reload = true

# Delay before reloading after file change (milliseconds)
hot_reload_delay = 100

# File patterns to watch (glob patterns)
hot_reload_watch = ["*.js", "*.ts", "package.json"]

# Show reload notifications
hot_reload_notifications = true
```

### 7. Error Handling Strategy

1. **Graceful Degradation**: If reload fails, keep the old version running
2. **Error Reporting**: Show clear error messages in the editor status line
3. **Rollback**: Ability to rollback to previous version if new version crashes
4. **Logging**: Detailed logs for debugging reload issues

```rust
impl PluginRegistry {
    pub async fn reload_plugin_safe(&mut self, name: &str, runtime: &mut Runtime) -> Result<()> {
        // Save current state
        let backup_state = self.backup_plugin_state(name, runtime).await?;
        
        // Try to reload
        match self.reload_plugin(name, runtime).await {
            Ok(_) => Ok(()),
            Err(e) => {
                // Restore previous state
                self.restore_plugin_state(name, backup_state, runtime).await?;
                Err(e)
            }
        }
    }
}
```

### 8. Development Mode Features

Create a special development mode that provides:

1. **Reload Statistics**: Show reload count, time taken, success rate
2. **Debug Information**: Detailed logs of what's being reloaded
3. **Performance Monitoring**: Track reload performance
4. **State Inspector**: View plugin state between reloads

### 9. Testing Strategy

1. **Unit Tests**: Test individual components (watcher, reload logic)
2. **Integration Tests**: Test full reload cycle
3. **Error Scenarios**: Test various failure modes
4. **Performance Tests**: Ensure reload is fast enough

## Usage Examples

### Manual Reload Commands

```vim
:reload-plugin buffer-picker     " Reload specific plugin
:reload-all-plugins             " Reload all plugins
:toggle-hot-reload              " Enable/disable hot reload
:show-reload-status            " Show current hot reload status
```

### Keybindings

```toml
[keys.normal]
"<leader>pr" = { ReloadPlugin = "current" }  # Reload current plugin
"<leader>pR" = "ReloadAllPlugins"           # Reload all plugins
"<leader>ph" = "ToggleHotReload"            # Toggle hot reload
```

### Plugin Development Workflow

```javascript
// example-plugin.js
let state = {
    counter: 0,
    lastAction: null
};

export async function activate(red) {
    // Restore state after reload
    const previousState = red.getPluginState('example-plugin');
    if (previousState) {
        state = previousState;
        red.log('Plugin reloaded, state restored');
    }
    
    red.addCommand('IncrementCounter', () => {
        state.counter++;
        state.lastAction = new Date();
        red.log(`Counter: ${state.counter}`);
    });
}

export async function onBeforeReload(red) {
    // Save state before reload
    red.savePluginState('example-plugin', state);
    return state;
}

export async function deactivate(red) {
    red.log('Plugin deactivating...');
}
```

## Benefits

1. **Faster Development Cycle**: No need to restart editor for plugin changes
2. **State Preservation**: Maintain plugin state across reloads
3. **Better Error Recovery**: Graceful handling of reload failures
4. **Improved Developer Experience**: Immediate feedback on code changes

## Challenges and Solutions

### Challenge 1: Module Cache
**Problem**: JavaScript modules are cached and won't reload
**Solution**: Add timestamp query parameter to force re-import

### Challenge 2: Event Listener Accumulation
**Problem**: Event listeners may accumulate on reload
**Solution**: Track listeners per plugin and clean up on deactivate

### Challenge 3: Timer/Interval Cleanup
**Problem**: Timers may continue running after reload
**Solution**: Force cleanup in deactivate, track all timers per plugin

### Challenge 4: Circular Dependencies
**Problem**: Plugins may have circular dependencies
**Solution**: Detect and warn about circular dependencies

## Implementation Timeline

1. **Phase 1** (2-3 days): Basic file watcher and reload infrastructure
2. **Phase 2** (2-3 days): State preservation and error handling
3. **Phase 3** (1-2 days): UI integration and commands
4. **Phase 4** (1-2 days): Testing and refinement
5. **Phase 5** (1 day): Documentation and examples

## Future Enhancements

1. **Dependency Tracking**: Reload dependent plugins automatically
2. **Partial Reload**: Reload only changed functions/components
3. **Hot Module Replacement**: True HMR without losing state
4. **Plugin Profiling**: Performance analysis during reload
5. **Remote Reload**: Reload plugins over network for remote development

## Conclusion

This hot reload implementation will significantly improve the plugin development experience for Red editor. By providing automatic reloading with state preservation and proper error handling, developers can iterate quickly on their plugins without the friction of constant editor restarts.