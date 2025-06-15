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

3. Set up configuration:
```shell
mkdir -p ~/.config/red
cp default_config.toml ~/.config/red/config.toml
cp -R themes ~/.config/red
```

### Quick Start

Once installed, you can start editing files immediately:

```shell
red <file-to-edit>
```

## Configuration

Red uses a TOML configuration file located at `~/.config/red/config.toml`. Here are some key configuration options:

```toml
# Theme selection
theme = "github_dark"

# Editor settings
[editor]
line_numbers = true
indent_style = "spaces"
indent_size = 4
cursor_line = true

# LSP configuration
[lsp]
enabled = true

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

Red supports VSCode themes. Place `.json` theme files in `~/.config/red/themes/` and reference them in your config:

```toml
theme = "your_theme_name"  # without .json extension
```

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
- `/` - Search
- `n/N` - Next/previous search result

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
├── plugins/              # Built-in plugins
├── themes/               # Default themes
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
- **Plugins not loading**: Check that the plugin directory exists and plugins have correct permissions
- **Theme not found**: Verify the theme file exists in `~/.config/red/themes/` and is valid JSON

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
