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

[keys.normal]
" " = { "c" = { PluginCommand = "codex.open" } }
":" = { EnterMode = "Command" }
"Esc" = { EnterMode = "Normal" }
"Ctrl-w" = {
  "w" = "NextWindow",
  "W" = "PreviousWindow",
  "c" = "CloseWindow",
  "h" = "MoveWindowLeft",
  "j" = "MoveWindowDown",
  "k" = "MoveWindowUp",
  "l" = "MoveWindowRight"
}
"q" = { Quit = true }
"i" = { EnterMode = "Insert" }
"j" = "MoveDown"
"k" = "MoveUp"
"h" = "MoveLeft"
"l" = "MoveRight"

[keys.insert]
"Esc" = { EnterMode = "Normal" }
"Enter" = "InsertNewLine"
"Backspace" = "DeletePreviousChar"

[keys.command]
"Esc" = { EnterMode = "Normal" }
TOML

cd "$ROOT"
HOME="$VISUAL_HOME" cargo run --quiet -- "$@"
