#!/bin/sh
# Run an intermediate Felix build with isolated config and disposable documents.
set -eu
binary=${1:?pass an absolute path to a Helix-enabled Red binary}
case "$binary" in /*) ;; *) echo 'expected an absolute binary path' >&2; exit 2;; esac
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
mkdir -p "$dir/config" "$dir/data" "$dir/state" "$dir/cache"
mkdir -p "$dir/config/red"
printf '%s\n' 'editing_model = "helix"' 'disable_ai = true' 'splash = false' 'show_whats_new = false' 'fetch_release_notes = false' '[lsp]' 'enabled = false' > "$dir/config/red/config.toml"
printf 'foo foo\n' > "$dir/one.txt"
printf 'second\n' > "$dir/two.txt"
cd "$dir"
XDG_CONFIG_HOME="$dir/config" XDG_DATA_HOME="$dir/data" XDG_STATE_HOME="$dir/state" XDG_CACHE_HOME="$dir/cache" "$binary" -c editing_model=helix one.txt two.txt
