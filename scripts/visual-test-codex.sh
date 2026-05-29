#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VISUAL_HOME="${RED_VISUAL_HOME:-/tmp/red-codex-visual-home}"
CONFIG_DIR="$VISUAL_HOME/.config/red"

mkdir -p "$CONFIG_DIR/themes"
cp "$ROOT/themes/mocha.json" "$CONFIG_DIR/themes/mocha.json"

cat > "$CONFIG_DIR/config.toml" <<'TOML'
theme = "mocha.json"
log_file = "/tmp/red-codex-visual.log"
show_diagnostics = false

[lsp]
enabled = false
TOML

if [[ -n "${RED_CODEX_APP_SERVER_ENDPOINT:-}" ]]; then
  cat >> "$CONFIG_DIR/config.toml" <<TOML
[codex]
app_server_endpoint = "$RED_CODEX_APP_SERVER_ENDPOINT"

TOML
fi

cat >> "$CONFIG_DIR/config.toml" <<'TOML'
[keys.normal]
" " = { "c" = { PluginCommand = "codex.open" } }
":" = { EnterMode = "Command" }
"Esc" = { EnterMode = "Normal" }
"q" = { Quit = true }
"i" = { EnterMode = "Insert" }
"j" = "MoveDown"
"k" = "MoveUp"
"h" = "MoveLeft"
"l" = "MoveRight"

[keys.normal."Ctrl-w"]
"w" = "NextWindow"
"W" = "PreviousWindow"
"c" = "CloseWindow"
"h" = "MoveWindowLeft"
"j" = "MoveWindowDown"
"k" = "MoveWindowUp"
"l" = "MoveWindowRight"

[keys.insert]
"Esc" = { EnterMode = "Normal" }
"Enter" = "InsertNewLine"
"Backspace" = "DeletePreviousChar"

[keys.command]
"Esc" = { EnterMode = "Normal" }
TOML

cd "$ROOT"
cargo build --quiet
HOME="$VISUAL_HOME" "$ROOT/target/debug/red" "$@"
