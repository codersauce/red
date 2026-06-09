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
- `activate(red)` - Called when the plugin is loaded. Async activation is
  supported, but startup does not wait for it to finish.
- `deactivate(red)` - Optional, called when the plugin is unloaded
- `beforeExit(red, state)` - Optional, awaited after quit succeeds and before
  plugin deactivation. `state` is the current editor session snapshot.

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
- `lsp:progress` - LSP progress notifications with raw `value`, flattened
  `kind`/`title`/`message`/`percentage`, and `lspClient` metadata
- `editor:resize` - Editor window resize events
- `buffer:changed` - Buffer content changes (includes cursor position and buffer info)
- `picker:selected:${id}` - Picker selection events
- `mode:changed` - Editor mode changes. Payload includes `from`, `to`,
  compatibility aliases `old_mode`/`new_mode`, and `cause`.
- `cursor:moved` - Cursor position changes (may fire frequently). Payload
  includes `from`, `to`, `mode`, `cause`, `viewportTop`, and `bufferIndex`,
  plus compatibility aliases `x`, `y`, `viewport_top`, and `buffer_index`.
- `search:highlighted` - A committed search activated visible highlights.
  Payload includes `term`, `direction`, and `source`.
- `search:cleared` - Search highlights were cleared. Payload includes `term`.
- `file:opened` - File opened in a buffer
- `file:saved` - File saved from a buffer
- `window:focused` - The active editor window changed
- `window:layoutChanged` - Window bounds or split layout changed
- `window:bufferChanged` - A window switched to another buffer
- `window:closed` - A window was closed
- `windowBar:action:${id}` - An actionable window-bar segment was clicked;
  payload includes the window, segment, and action IDs
- `theme:changed` - The active theme changed
- `editor:ready` - Plugins have loaded and startup work can begin

#### Editor Information
```javascript
const info = await red.getEditorInfo()
```
Returns an object containing:
- `buffers` - Array of buffer information (id, name, path, language_id)
- `current_buffer_index` - Index of the active buffer
- `size` - Editor dimensions (rows, cols)
- `theme` - Current theme information. `theme.colors` contains parsed VS Code
  workbench colors keyed by their original names, such as
  `gitDecoration.modifiedResourceForeground` or `list.highlightForeground`.

#### Session State
```javascript
const state = await red.getEditorState()
const result = await red.restoreEditorState(state)
```

The snapshot includes file-backed buffers, cursor and viewport positions, the
active buffer, cwd, and window split layout. Restore skips missing files and
returns `{ restored, openedFiles, skippedFiles, warnings }`.

#### Plugin Storage
```javascript
await red.storage.set("latest", state)
const state = await red.storage.get("latest")
await red.storage.delete("latest")
```

Storage is JSON, namespaced by plugin, and written under Red's config state
directory.

#### UI Interaction
```javascript
// Show a picker dialog
const selected = await red.pick(title: string, values: array)

// Open a buffer by name
red.openBuffer(name: string)

// Draw text at specific coordinates
red.drawText(x: number, y: number, text: string, style?: object)

// Add one persistent row of window-local UI above buffer content
red.ui.createWindowBar("breadcrumbs", {
  edge: "top",
  priority: 100,
  overflow: "truncate_left",
  truncateMarker: "…",
})
red.ui.updateWindowBar("breadcrumbs", windowId, [
  {
    id: "file",
    text: " main.rs",
    style: {
      semantic: {
        foreground: ["symbolIcon.fileForeground", "breadcrumb.foreground"],
        background: ["breadcrumb.background", "editor.background"],
      },
    },
    action: "open-file",
  },
])
red.ui.closeWindowBar("breadcrumbs")

// Create and update a persistent side panel
red.createPanel("tree", { side: "left", width: 32 })
red.updatePanel("tree", [{
  id: "/repo/src",
  path: "/repo/src",
  expanded: true,
  kind: "directory",
  segments: [
    { text: "│ ", style: mutedStyle },
    { text: " ", style: directoryStyle },
    { text: "src", style: directoryStyle }
  ],
  right_segments: [
    { text: "", style: modifiedStyle }
  ]
}])
red.focusPanel("tree")
red.focusEditor()
red.closePanel("tree")
red.onPanelEvent("tree", (event) => {
  // event.action is "up", "down", "expand", "collapse", "activate",
  // "toggle", "close", "refresh", or "select"
})
```

Panel rows are rendered by the editor and receive focused keyboard input
while the panel is active. Plugins can call `focusEditor()` to return input
to the editor after handling a panel action. Pressing `Esc` also returns
focus to the editor. Focused panels own normal-mode input and hide the editor
cursor; command and search prompts still receive input after `:` or `/`.

Rows are segment-based: `segments` render from the left, and
`right_segments` render flush-right when space allows. Segment text is clipped
by terminal display width, so plugins can use Unicode/Nerd Font glyphs safely.

#### Virtual Text Decorations
```javascript
const layout = await red.getViewportLayout()

red.setDecorations("inlay-hints", [{
  buffer_index: layout.bufferIndex,
  line: layout.rows[0].line,
  anchor: "eol",
  text: " => PathBuf",
  style: {
    fg: { Rgb: { r: 108, g: 112, b: 134 } },
    bg: null,
    bold: false,
    italic: true
  },
  priority: 1001
}])

red.clearDecorations("inlay-hints")
```

Decorations are persistent virtual text owned by a namespace. `anchor: "column"`
draws at `column` and is the default. `anchor: "eol"` draws after the rendered
source line, and `anchor: "right_align"` draws flush-right in the editor content
area. Decorations are rendered after source text and draw only their own glyphs.

`getViewportLayout()` returns the active window rows, buffer index, content
width, cursor, and indentation metadata so plugins can generate decorations only
for visible content.

#### LSP Helpers
```javascript
const result = await red.lsp.inlayHints({ visible: true })

if (result.ok) {
  for (const hint of result.hints) {
    // hint.position.line, hint.position.character, hint.label, hint.kind
  }
}
```

`red.lsp.inlayHints()` asks the language server for `textDocument/inlayHint` on
the current file-backed buffer. Pass `{ visible: true }` to request only visible
lines, or pass an explicit LSP `range`.

#### Filesystem
```javascript
const { entries, error } = await red.listDirectory(".")
const git = await red.getGitStatus(".")
const watchId = red.watchDirectory(".", async (snapshot) => {
  // snapshot has the same shape as listDirectory()
})
red.unwatchDirectory(watchId)
red.openFile("src/main.rs")
```

`listDirectory` returns entries sorted with directories before files. Plugins
do not receive arbitrary filesystem access; they request directory listings
and directory watches through editor-owned APIs.

`getGitStatus` returns `{ root, statuses, error }` for the Git repository that
contains the requested path. Each status has `path`, `absolute_path`, and a
normalized `status` such as `modified`, `untracked`, `ignored`, or `conflict`.

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

#### Windows, Window Bars, and Theme Styles

`red.getWindows()` returns session-stable window IDs together with each
window's bounds, content bounds, buffer path and revision, cursor, UTF-16
`lspPosition`, and viewport. Window bars reserve one row inside each window;
the host adjusts buffer rendering, cursor placement, overlays, and mouse hit
testing for that inset. Only the highest-priority bar is visible in a window.

Window-bar text is a list of independently styled segments. Long content is
clipped on a grapheme boundary according to the configured overflow direction.
An actionable segment emits `windowBar:action:${id}` when clicked.

Resolve concrete colors from the active theme when a component needs ordered
fallbacks:

```javascript
const style = await red.theme.resolveStyle({
  foreground: [
    "symbolIcon.functionForeground",
    "scope:entity.name.function",
    "breadcrumb.foreground",
    "editor.foreground",
  ],
  background: ["breadcrumb.background", "editor.background"],
  bold: true,
})
```

Workbench color keys are tried from left to right. TextMate scopes use a
`scope:` prefix. The top-level `red.createWindowBar`, `red.updateWindowBar`,
`red.closeWindowBar`, and `red.resolveThemeStyle` names remain available for
compatibility.

#### Action Execution
```javascript
red.execute(command: string, args?: any)
red.clearSearchHighlight()
```
Executes any editor action programmatically.
`red.clearSearchHighlight()` is a convenience wrapper for the
`ClearSearchHighlight` action.

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

#### Structured Pickers

```javascript
const picker = red.createPicker("Find", items, {
  externalFilter: true,
  onQuery(query) {},
  onAction(action, item) {},
  actions: [{ key: "Ctrl-v", action: "open_vertical" }],
});
picker.updateItems(nextItems);
picker.updateStatus("12 matches");
const selected = await picker.result;
```

Picker items use stable `id` values and may include `label`, an `annotation`
rendered immediately after it, `detail`, plugin `data`, and character-based
`matches` or `detailMatches` ranges. File-location previews may provide UTF-8
byte `matches` ranges on their focused line. Red renders these fields with the
current theme's result, gutter, line-highlight, and find-match colors. Updating
items preserves the selection when its ID is still present. The legacy `pick`
and `pickLive` APIs remain available.

#### Process Execution

Processes are shell-free and disabled unless the plugin has an exact command
allowlist in `config.toml`:

```toml
[plugin_permissions.my_plugin]
process = ["rg"]
```

```javascript
const process = red.spawnProcess({
  command: "rg",
  args: ["--json", "needle"],
  cwd: ".",
  onStdout(line) {},
  onStderr(line) {},
  onExit({ code }) {},
});
process.kill();
```

Output callbacks receive newline-stripped lines. Red permits at most four
active processes per plugin and terminates remaining children when the plugin
runtime shuts down.

#### Opening Locations

```javascript
red.openLocation(
  { path: "src/main.rs", line: 9, column: 4, columnEncoding: "utf8-byte" },
  { target: "vertical" },
);
```

Lines and columns are zero-based; columns are UTF-8 byte offsets. Targets are
`current`, `horizontal`, or `vertical`. Location jumps reuse loaded buffers and
participate in editor jump history.

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

### Built-in Project Search

The bundled `project_search.js` plugin uses ripgrep JSON output and the
structured picker API. The default binding is `Space g`. It provides live
smart-case search, regex/literal and filesystem toggles, split opening,
embedded previews, per-directory history, and export to a persistent results
panel. It requires `rg` on `PATH` and this permission:

```toml
[plugin_permissions.project_search]
process = ["rg"]
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
        const {
            token,
            kind,
            message,
            title,
            percentage,
            lspClient,
            value, // Raw LSP WorkDoneProgress value
        } = data;
        const group = lspClient?.name ?? "LSP";
        
        if (kind === "begin") {
            const fullMessage = percentage !== undefined 
                ? `${group}: ${title}: ${message} (${percentage}%)`
                : `${group}: ${title}: ${message}`;
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
- Process execution requires an exact per-plugin executable allowlist and does
  not invoke a shell
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
