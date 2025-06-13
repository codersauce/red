# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

### Build and Run
```bash
# Build the project
cargo build

# Build release version
cargo build --release

# Run the editor
cargo run -- <file>

# Install locally
cargo install --path .
```

### Testing
```bash
# Run all tests
cargo test

# Run a specific test
cargo test test_name

# Run tests with output
cargo test -- --nocapture
```

### Development
```bash
# Check code without building
cargo check

# Format code (if rustfmt is configured)
cargo fmt

# Run linter (if clippy is configured)
cargo clippy
```

## Architecture

Red is a modal text editor built in Rust, inspired by Vim. The codebase follows an event-driven architecture with async programming using tokio.

### Core Components

- **Editor State Machine**: The editor operates in different modes (Normal, Insert, Visual, Command). Mode transitions are handled in `src/editor.rs`.

- **Buffer Management**: Text is stored using the Ropey rope data structure for efficient manipulation. See `src/buffer.rs`.

- **Language Server Protocol**: LSP client implementation in `src/lsp/` provides IDE features. The client runs asynchronously and communicates with language servers.

- **Plugin System**: JavaScript plugins run in a sandboxed Deno runtime. Plugins are loaded from the `plugins/` directory and configured in `config.toml`.

- **UI Components**: Terminal UI built with crossterm. Reusable components in `src/ui/` include file picker, completion widget, and generic picker.

### Key Design Patterns

- **Async Event Loop**: Main loop in `src/main.rs` handles keyboard events, LSP messages, and plugin callbacks asynchronously.

- **Command Pattern**: All editor actions are commands that can be bound to keys. See `src/command.rs`.

- **Theme System**: VSCode themes are supported via JSON files in `~/.config/red/themes/`.

### Configuration

User configuration is read from `~/.config/red/config.toml`. Key bindings, theme selection, and plugin settings are configured here.

### Plugin Development

Plugins are JavaScript files that export an `activate` function:
```javascript
export function activate(red) {
    // Plugin initialization
}
```

The `red` object provides access to editor APIs for buffer manipulation, UI interaction, and event handling.

### Debugging

- Logs are written to the file specified in `config.toml` (default: `/tmp/red.log`)
- Debug commands available in normal mode:
  - `db` - Dump buffer state
  - `di` - Dump LSP diagnostics
  - `dc` - Dump LSP capabilities
  - `dh` - Dump command history