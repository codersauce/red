# red - Rusty Editor

[![CI](https://github.com/codersauce/red/actions/workflows/ci.yml/badge.svg)](https://github.com/codersauce/red/actions/workflows/ci.yml)
[![Plugin System Check](https://github.com/codersauce/red/actions/workflows/plugin-check.yml/badge.svg)](https://github.com/codersauce/red/actions/workflows/plugin-check.yml)
[![Release](https://github.com/codersauce/red/actions/workflows/release.yml/badge.svg)](https://github.com/codersauce/red/actions/workflows/release.yml)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Discord](https://img.shields.io/badge/Discord-Join%20us-7289DA?logo=discord&logoColor=white)](https://discord.gg/5PWvAUNRHU)

A modern, modal text editor built in Rust with minimal dependencies. Red combines the power of modal editing with modern features like Language Server Protocol support, async operations, and a JavaScript plugin system.

![red screenshot](docs/screenshot.png)

## Features

- **Modal Editing**: Vim-inspired modal interface with Normal, Insert, Visual, and Command modes
- **Language Server Protocol**: Full LSP support for intelligent code completion, diagnostics, and more
- **Async Architecture**: Built on Tokio for responsive, non-blocking operations
- **Plugin System**: Extend functionality with JavaScript plugins running in a sandboxed Deno runtime
- **Syntax Highlighting**: Tree-sitter based syntax highlighting for accurate code coloring
- **Theme Support**: VSCode theme compatibility - use your favorite themes
- **Self-Contained**: Default config, themes, and plugins are bundled into the binary - no setup required
- **Minimal Dependencies**: Built from scratch with a focus on simplicity and performance
- **Cross-Platform**: Works on Linux, macOS, and Windows

## Requirements

- Rust 1.70 or newer
- Cargo (comes with Rust)
- Git

## Current Status

This editor is being actively built on a series of streams and videos published to my CoderSauce YouTube channel here:

https://youtube.com/@CoderSauce

It is my intention to keep it stable starting at the first alpha release, but there are no guarantees. As such, use it at your discretion. Bad things can happen to your files, so don't use it yet for anything critical.

If you want to collaborate or discuss red's features, usage or anything, join our Discord:

https://discord.gg/5PWvAUNRHU

## Installation

### From Source (Recommended)

1. Clone the repository:
```shell
git clone https://github.com/codersauce/red.git
cd red
```

2. Build and install:
```shell
cargo install --path .
```

That's it. No configuration step is needed - the default config, themes, and plugins are bundled into the binary.

### Quick Start

Once installed, you can start editing files immediately:

```shell
red <file-to-edit>
```

On the first interactive run, Red offers to create a starter config at `~/.config/red/config.toml`. You can decline (or run non-interactively) and Red launches with its embedded defaults - a config file is entirely optional.

## Configuration

Red works out of the box with sensible embedded defaults. To customize it, use a TOML configuration file at `~/.config/red/config.toml`.

Your config is layered **on top of** the embedded defaults: you only need to write the settings you want to change, and everything else keeps its default value. The starter config that Red offers to create on first run is a commented template to get you going - it is not the source of truth for defaults, so deleting it (or any setting in it) simply falls back to the built-in behavior.

Here are some key configuration options:

```toml
# Theme selection (theme filename, including the .json extension)
theme = "mocha.json"

# Editor settings
[editor]
line_numbers = true
indent_style = "spaces"
indent_size = 4
cursor_line = true

# LSP configuration
[lsp]
enabled = true

# Built-in syntax highlighting covers Rust, Markdown, JavaScript,
# TypeScript/TSX, JSON, TOML, YAML, Python, and Bash. LSP defaults are
# provided for common language-server-backed file types and start only when a
# matching file is opened.

[lsp.servers.typescript]
command = "typescript-language-server"
args = ["--stdio"]
root_markers = ["package.json", "tsconfig.json", "jsconfig.json", ".git"]

[[lsp.servers.typescript.documents]]
language_id = "typescript"
file_extensions = ["ts"]

[[lsp.servers.typescript.documents]]
language_id = "typescriptreact"
file_extensions = ["tsx"]

[[lsp.servers.typescript.documents]]
language_id = "javascript"
file_extensions = ["js", "mjs", "cjs"]

# Search settings
[search]
incsearch = true
hlsearch = true
wrapscan = true
ignorecase = false
smartcase = false

# Plugin settings
[plugins]
enabled = true
directory = "~/.config/red/plugins"

# Logging
[debug]
log_file = "/tmp/red.log"
log_level = "info"
```

### Themes

Red ships with a collection of VSCode-compatible themes bundled into the binary - they work without any files on disk. Reference a theme in your config:

```toml
theme = "your_theme_name.json"  # the theme's filename
```

To add your own theme, place a `.json` theme file in `~/.config/red/themes/`. Run `red --runtime-files` to see every theme Red can currently load.

## Bundled Plugins and Themes

Red's default plugins and themes are embedded in the binary, so a fresh install has everything it needs and upgrades automatically pick up newer bundled versions. Nothing is copied to your config directory unless you ask for it.

### Seeing what's available

```shell
red --runtime-files
```

This lists every plugin and theme Red can see and where each one comes from (your config directory, `$RED_RUNTIME`, or the embedded assets). When the same filename exists in more than one place, the listing shows which source wins.

### Overriding a bundled asset

Files in your config directory take precedence over bundled ones with the same filename. For example, `~/.config/red/plugins/fidget.js` replaces the bundled `fidget.js`.

To start from the bundled version, *eject* a copy into your config directory:

```shell
red --eject plugins/fidget.js   # copy a bundled plugin for editing
red --eject themes/mocha.json   # copy a bundled theme for editing
red --eject fidget.js           # the plugins/ or themes/ prefix is optional
```

Eject refuses to overwrite an existing file; use `red --eject-force <asset>` to replace your copy with the bundled version.

Keep in mind that an ejected file shadows the bundled one permanently - if a later Red release improves that plugin or theme, your copy still wins. Delete the file from your config directory to go back to the bundled version.

### Advanced: `$RED_RUNTIME`

Packagers and developers working from a source checkout can point `$RED_RUNTIME` at a directory containing `plugins/` and `themes/` subdirectories. Assets are resolved in this order:

1. Your config directory (e.g. `~/.config/red/plugins/foo.js`)
2. `$RED_RUNTIME/plugins/foo.js` or `$RED_RUNTIME/themes/foo.json`
3. The assets embedded in the binary

Normal users don't need to set this - the embedded assets cover everyday use.

## Key Bindings

Red uses Vim-style modal editing. Here are the essential key bindings:

### Normal Mode
- `i` - Enter Insert mode
- `v` - Enter Visual mode
- `:` - Enter Command mode
- `h/j/k/l` - Move left/down/up/right
- `w/b` - Move forward/backward by word
- `0/$` - Move to beginning/end of line
- `gg/G` - Go to first/last line
- `dd` - Delete line
- `yy` - Copy line
- `p` - Paste
- `u` - Undo
- `Ctrl+r` - Redo
- `/` - Forward search with live preview and highlighted matches
- `?` - Backward search with live preview and highlighted matches
- `n/N` - Repeat search in the same/opposite direction

Search patterns use Rust regex syntax. The bundled `cool_search` plugin clears
search highlights automatically after you move away from a committed match or
enter Insert mode. `:noh` or `:nohlsearch` still clears the current search
highlights manually until the next search.

### Insert Mode
- `Esc` - Return to Normal mode
- All regular typing works as expected

### Visual Mode
- `Esc` - Return to Normal mode
- `d` - Delete selection
- `y` - Copy selection
- `>/<` - Indent/unindent selection

### Command Mode
- `:w` - Save file
- `:q` - Quit
- `:wq` - Save and quit
- `:e <file>` - Open file
- `:set <option>` - Set editor option

## Development

### Building from Source

```shell
# Clone the repository
git clone https://github.com/codersauce/red.git
cd red

# Build debug version
cargo build

# Build release version
cargo build --release

# Run tests
cargo test

# Run with debug logging
RUST_LOG=debug cargo run -- test.txt
```

### Project Structure

```
red/
├── src/
│   ├── main.rs           # Entry point and event loop
│   ├── editor.rs         # Core editor state machine
│   ├── buffer.rs         # Text buffer management
│   ├── lsp/              # Language Server Protocol implementation
│   ├── ui/               # Terminal UI components
│   └── plugins/          # Plugin system
├── plugins/              # Built-in plugins (bundled into the binary)
├── themes/               # Default themes (bundled into the binary)
└── tests/                # Integration tests
```

### Contributing

Contributions are welcome! Please feel free to submit a Pull Request. For major changes, please open an issue first to discuss what you would like to change.

1. Fork the repository
2. Create your feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add some amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## Troubleshooting

### Debug Mode

Enable debug logging to troubleshoot issues:

```toml
[debug]
log_file = "/tmp/red.log"
log_level = "debug"
```

Then check the log file:
```shell
tail -f /tmp/red.log
```

### Common Issues

- **LSP not working**: Ensure the language server for your file type is installed and in your PATH
- **Plugins not loading**: Run `red --runtime-files` to check that the plugin is visible and which source (config directory, `$RED_RUNTIME`, or embedded) is being used
- **Theme not found**: Run `red --runtime-files` to confirm the theme name; custom themes in `~/.config/red/themes/` must be valid JSON
- **A bundled plugin/theme behaves like an old version**: An ejected copy in your config directory shadows the bundled one - delete it or re-eject with `red --eject-force`

## Reporting Issues

If you find any issues, please report them at:

https://github.com/codersauce/red/issues/

Check existing issues first to avoid duplicates.

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## Acknowledgments

- Built with love for the Rust community
- Inspired by Vim, Neovim, and Helix
- Special thanks to all contributors and the CoderSauce community

Thank you for trying Red! ❤️
