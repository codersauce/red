# Red Editor Plugin System Documentation

## Overview

The Red editor features a powerful plugin system built on Deno Core runtime, allowing developers to extend the editor's functionality using JavaScript or TypeScript. Plugins run in a sandboxed environment with controlled access to editor APIs, ensuring security while providing flexibility.

## Architecture

### Core Components

The plugin system consists of three main modules located in `src/plugin/`:

1. **`runtime.rs`** - Manages the Deno JavaScript runtime in a separate thread
2. **`loader.rs`** - Handles module loading and TypeScript transpilation
3. **`registry.rs`** - Manages plugin lifecycle and communication between plugins and the editor

### Communication Model

The plugin system uses a bidirectional communication model:

```
Editor Thread <-> Plugin Registry <-> Plugin Runtime Thread <-> JavaScript Plugins
```

- **Editor to Plugin**: Through `PluginRegistry` methods and event dispatching
- **Plugin to Editor**: Via custom Deno ops and the global `ACTION_DISPATCHER`

## Plugin Development Guide

### Plugin Metadata

Plugins can include a `package.json` file to provide metadata:

```json
{
  "name": "my-plugin",
  "version": "1.0.0",
  "description": "A helpful plugin for Red editor",
  "author": "Your Name",
  "license": "MIT",
  "keywords": ["productivity", "tools"],
  "repository": {
    "type": "git",
    "url": "https://github.com/user/my-plugin"
  },
  "engines": {
    "red": ">=0.1.0"
  },
  "capabilities": {
    "commands": true,
    "events": true,
    "buffer_manipulation": false,
    "ui_components": true
  }
}
```

View loaded plugins with the `dp` keybinding or `ListPlugins` command.

### Creating a Plugin

1. Create a JavaScript or TypeScript file that exports an `activate` function:

**Plugin Lifecycle:**
- `activate(red)` - Called when the plugin is loaded
- `deactivate(red)` - Optional, called when the plugin is unloaded

```javascript
export async function activate(red) {
    // Initialize your plugin
}

export async function deactivate(red) {
    // Clean up resources (intervals, event listeners, etc.)
    await red.clearInterval(myInterval);
}
```

For TypeScript development with full type safety:
```typescript
/// <reference types="@red-editor/types" />

export async function activate(red: Red.RedAPI) {
    // Your plugin code with IntelliSense and type checking
}
```

For JavaScript:

```javascript
export async function activate(red) {
    // Plugin initialization code
    red.addCommand("MyCommand", async () => {
        // Command implementation
    });
}
```

2. Add the plugin to your `config.toml`:

```toml
[plugins]
my_plugin = "my_plugin.js"
```

3. Place the plugin file in `~/.config/red/plugins/`

### Plugin API Reference

The `red` object passed to the `activate` function provides the following APIs:

#### Command Registration
```javascript
red.addCommand(name: string, callback: async function)
```
Registers a new command that can be bound to keys or executed programmatically.

#### Event Subscription
```javascript
red.on(event: string, callback: function)
```
Subscribes to editor events. Available events include:
- `lsp:progress` - LSP progress notifications
- `editor:resize` - Editor window resize events
- `buffer:changed` - Buffer content changes (includes cursor position and buffer info)
- `picker:selected:${id}` - Picker selection events
- `mode:changed` - Editor mode changes (Normal, Insert, Visual, etc.)
- `cursor:moved` - Cursor position changes (may fire frequently)
- `file:opened` - File opened in a buffer
- `file:saved` - File saved from a buffer

#### Editor Information
```javascript
const info = await red.getEditorInfo()
```
Returns an object containing:
- `buffers` - Array of buffer information (id, name, path, language_id)
- `current_buffer_index` - Index of the active buffer
- `size` - Editor dimensions (rows, cols)
- `theme` - Current theme information

#### UI Interaction
```javascript
// Show a picker dialog
const selected = await red.pick(title: string, values: array)

// Open a buffer by name
red.openBuffer(name: string)

// Draw text at specific coordinates
red.drawText(x: number, y: number, text: string, style?: object)
```

#### Buffer Manipulation
```javascript
// Insert text at position
red.insertText(x: number, y: number, text: string)

// Delete text at position
red.deleteText(x: number, y: number, length: number)

// Replace text at position
red.replaceText(x: number, y: number, length: number, text: string)

// Get/set cursor position
const pos = await red.getCursorPosition()  // Returns {x, y}
red.setCursorPosition(x: number, y: number)

// Get buffer text
const text = await red.getBufferText(startLine?: number, endLine?: number)
```

#### Action Execution
```javascript
red.execute(command: string, args?: any)
```
Executes any editor action programmatically.

#### Command Discovery
```javascript
// Get list of available plugin commands
const commands = red.getCommands()  // Returns array of command names
```

#### Configuration Access
```javascript
// Get configuration values
const theme = await red.getConfig("theme")  // Get specific config value
const allConfig = await red.getConfig()     // Get entire config object
```

Available configuration keys:
- `theme` - Current theme name
- `plugins` - Map of plugin names to paths
- `log_file` - Log file path
- `mouse_scroll_lines` - Lines to scroll with mouse wheel
- `show_diagnostics` - Whether to show diagnostics
- `keys` - Key binding configuration

#### Logging
```javascript
// Log with different levels
red.logDebug(...messages)   // Debug information
red.logInfo(...messages)    // General information
red.logWarn(...messages)    // Warnings
red.logError(...messages)   // Errors
red.log(...messages)        // Default (info level)

// Open log viewer in editor
red.viewLogs()
```

Log messages are written to the file specified in `config.toml` with timestamps and level indicators.

#### Timers
```javascript
// One-time timers
const timeoutId = await red.setTimeout(callback: function, delay: number)
await red.clearTimeout(timeoutId: string)

// Repeating intervals
const intervalId = await red.setInterval(callback: function, delay: number)
await red.clearInterval(intervalId: string)
```

Example:
```javascript
// Update status every second
const interval = await red.setInterval(() => {
  red.logDebug("Periodic update");
}, 1000);

// Clean up on deactivation
await red.clearInterval(interval);
```

### Example: Buffer Picker Plugin

Here's a complete example of a buffer picker plugin:

```javascript
export async function activate(red) {
    red.addCommand("BufferPicker", async () => {
        const info = await red.getEditorInfo();
        const buffers = info.buffers.map((buf) => ({
            id: buf.id,
            name: buf.name,
            path: buf.path,
            language: buf.language_id
        }));
        
        const bufferNames = buffers.map(b => b.name);
        const selected = await red.pick("Open Buffer", bufferNames);
        
        if (selected) {
            red.openBuffer(selected);
        }
    });
}
```

### Keybinding Configuration

To bind a plugin command to a key, add it to your `config.toml`:

```toml
[keys.normal." "]  # Space as leader key
"b" = { PluginCommand = "BufferPicker" }
```

## Implementation Details

### Runtime Environment

- **JavaScript Engine**: Deno Core v0.229.0
- **TypeScript Support**: Automatic transpilation via swc
- **Module Loading**: Supports local files, HTTP/HTTPS imports, and various file types (JS, TS, JSX, TSX, JSON)
- **Thread Isolation**: Plugins run in a separate thread for safety and performance

### Available Editor Actions

Plugins can trigger any editor action through `red.execute()`, including:

- Movement: `MoveUp`, `MoveDown`, `MoveLeft`, `MoveRight`
- Editing: `InsertString`, `DeleteLine`, `Undo`, `Redo`
- UI: `FilePicker`, `OpenPicker`, `CommandPalette`
- Buffer: `NextBuffer`, `PreviousBuffer`, `CloseBuffer`
- Mode changes: `NormalMode`, `InsertMode`, `VisualMode`

### TypeScript Development

Red provides full TypeScript support for plugin development:

1. **Type Definitions**: Install `@red-editor/types` for complete type safety
2. **IntelliSense**: Get autocomplete and documentation in your IDE
3. **Type Checking**: Catch errors at development time
4. **Automatic Transpilation**: TypeScript files are automatically compiled

Example with types:
```typescript
import type { RedAPI, BufferChangeEvent } from '@red-editor/types';

export async function activate(red: RedAPI) {
    red.on("buffer:changed", (data: BufferChangeEvent) => {
        // TypeScript knows data.cursor.x and data.cursor.y are numbers
        red.log(`Change at ${data.cursor.x}, ${data.cursor.y}`);
    });
}
```

### Module System

The plugin loader (`TsModuleLoader`) supports:

```javascript
// Local imports
import { helper } from "./utils.js";

// HTTP imports (Deno-style)
import { serve } from "https://deno.land/std/http/server.ts";

// JSON imports
import config from "./config.json";
```

### Error Handling

- Plugin errors are captured and converted to Rust `Result` types
- Errors are displayed in the editor's status line with JavaScript stack traces
- Use log levels for appropriate error reporting:
  - `red.logError()` for errors
  - `red.logWarn()` for warnings
  - `red.logInfo()` for general information
  - `red.logDebug()` for detailed debugging

Example error handling:
```javascript
try {
  await riskyOperation();
} catch (error) {
  red.logError("Operation failed:", error.message);
  red.logDebug("Stack trace:", error.stack);
}
```

## Advanced Examples

### LSP Progress Monitor (fidget.js)

This plugin displays LSP progress notifications:

```javascript
export function activate(red) {
    const messageStack = [];
    const timers = {};

    red.on("lsp:progress", (data) => {
        const { token, kind, message, title, percentage } = data;
        
        if (kind === "begin") {
            const fullMessage = percentage !== undefined 
                ? `${title}: ${message} (${percentage}%)`
                : `${title}: ${message}`;
            messageStack.push({ token, message: fullMessage });
        } else if (kind === "end") {
            const index = messageStack.findIndex(m => m.token === token);
            if (index !== -1) {
                messageStack.splice(index, 1);
            }
        }
        
        renderMessages();
    });

    function renderMessages() {
        const info = red.getEditorInfo();
        const baseY = info.size.rows - messageStack.length - 2;
        
        messageStack.forEach((msg, index) => {
            red.drawText(2, baseY + index, msg.message, {
                fg: "yellow",
                modifiers: ["bold"]
            });
        });
    }
}
```

### Event-Driven Plugin

```javascript
export function activate(red) {
    // React to buffer changes
    red.on("buffer:changed", (data) => {
        red.log("Buffer changed:", data.buffer_id);
    });
    
    // React to editor resize
    red.on("editor:resize", (data) => {
        red.log(`New size: ${data.cols}x${data.rows}`);
    });
    
    // Custom picker with event handling
    red.addCommand("CustomPicker", async () => {
        const id = Date.now();
        const options = ["Option 1", "Option 2", "Option 3"];
        
        red.on(`picker:selected:${id}`, (selection) => {
            red.log("User selected:", selection);
        });
        
        red.execute("OpenPicker", {
            id,
            title: "Choose an option",
            values: options
        });
    });
}
```

## Limitations and Considerations

### Testing Plugins

Red includes a comprehensive testing framework for plugin development:

```javascript
// my-plugin.test.js
describe('My Plugin', () => {
  test('should register command', async (red) => {
    expect(red.hasCommand('MyCommand')).toBe(true);
  });
});
```

Run tests with:
```bash
node test-harness/test-runner.js my-plugin.js my-plugin.test.js
```

See [test-harness/README.md](../test-harness/README.md) for complete documentation.

### Current Limitations

1. **Shared Runtime**: All plugins share the same JavaScript runtime context
2. **Plugin Management**: No built-in plugin installation/removal commands
3. **Inter-plugin Communication**: Limited ability for plugins to communicate with each other
4. **File System Access**: No direct filesystem APIs (must use editor buffer operations)
5. **Hot Reload**: Requires editor restart for plugin changes

### Security Considerations

- Plugins run in a sandboxed Deno environment
- No direct filesystem access (must use editor APIs)
- Limited to provided operation APIs
- Network access through Deno's permission system

### Performance Considerations

- Plugins run in a separate thread to avoid blocking the editor
- Heavy computations should be done asynchronously
- Use `setTimeout` for deferred operations to avoid blocking

## Future Enhancements

Areas identified for potential improvement:

1. **Plugin Management**
   - Plugin installation/removal commands
   - Version management
   - Dependency resolution

2. **Developer Experience**
   - Better error messages with stack traces
   - Plugin development mode with hot reload
   - Built-in plugin testing framework

3. **API Enhancements**
   - More granular buffer manipulation APIs
   - File system access with permissions
   - Plugin-to-plugin communication

4. **Documentation**
   - Interactive plugin command documentation
   - API reference generation
   - Plugin marketplace/registry

## Conclusion

The Red editor's plugin system provides a robust foundation for extending editor functionality while maintaining security and performance. By leveraging Deno's runtime and a well-designed API, developers can create powerful plugins that integrate seamlessly with the editor's core functionality.

For questions or contributions to the plugin system, please refer to the main Red editor repository and its contribution guidelines.