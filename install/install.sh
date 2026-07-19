#!/bin/sh

set -eu

repository="codersauce/red"
releases_url="${RED_RELEASES_URL:-https://github.com/$repository/releases}"
version="${RED_VERSION:-latest}"
install_dir="${RED_INSTALL_DIR:-${HOME:-}/.local/bin}"
os="${RED_INSTALLER_OS:-$(uname -s)}"
arch="${RED_INSTALLER_ARCH:-$(uname -m)}"

fail() {
  printf 'red installer: %s\n' "$*" >&2
  exit 1
}

if [ -z "$install_dir" ]; then
  fail 'HOME is not set; set RED_INSTALL_DIR to choose an installation directory'
fi

case "$os:$arch" in
  Darwin:arm64|Darwin:aarch64)
    target="aarch64-apple-darwin"
    ;;
  Darwin:x86_64|Darwin:amd64)
    target="x86_64-apple-darwin"
    ;;
  Linux:x86_64|Linux:amd64)
    if command -v ldd >/dev/null 2>&1 && ldd --version 2>&1 | grep -qi musl; then
      fail 'musl Linux is not supported yet; install Red from source with Cargo'
    fi
    target="x86_64-unknown-linux-gnu"
    ;;
  Linux:arm64|Linux:aarch64)
    fail 'Linux ARM64 is not supported yet; install Red from source with Cargo'
    ;;
  *)
    fail "unsupported platform: $os $arch"
    ;;
esac

archive="red-$target.tar.gz"
case "$version" in
  latest)
    download_base="$releases_url/latest/download"
    ;;
  v*)
    download_base="$releases_url/download/$version"
    ;;
  *)
    download_base="$releases_url/download/v$version"
    ;;
esac

command -v curl >/dev/null 2>&1 || fail 'curl is required'
command -v tar >/dev/null 2>&1 || fail 'tar is required'

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/red-install.XXXXXX")" ||
  fail 'could not create a temporary directory'
staged_binary=""

cleanup() {
  rm -rf "$tmp_dir"
  if [ -n "$staged_binary" ]; then
    rm -f "$staged_binary"
  fi
}
trap cleanup EXIT HUP INT TERM

printf 'Downloading Red %s for %s...\n' "$version" "$target"
curl --fail --location --silent --show-error \
  "$download_base/$archive" --output "$tmp_dir/$archive" ||
  fail "could not download $archive"
curl --fail --location --silent --show-error \
  "$download_base/SHA256SUMS.txt" --output "$tmp_dir/SHA256SUMS.txt" ||
  fail 'could not download SHA256SUMS.txt'

expected_checksum="$(
  awk -v archive="$archive" '
    $2 == archive || $2 == "*" archive { print $1; exit }
  ' "$tmp_dir/SHA256SUMS.txt"
)"
[ -n "$expected_checksum" ] || fail "checksum for $archive was not published"

if command -v sha256sum >/dev/null 2>&1; then
  actual_checksum="$(sha256sum "$tmp_dir/$archive" | awk '{ print $1 }')"
elif command -v shasum >/dev/null 2>&1; then
  actual_checksum="$(shasum -a 256 "$tmp_dir/$archive" | awk '{ print $1 }')"
else
  fail 'sha256sum or shasum is required'
fi

[ "$actual_checksum" = "$expected_checksum" ] ||
  fail "checksum mismatch for $archive"

mkdir "$tmp_dir/extracted"
tar -xzf "$tmp_dir/$archive" -C "$tmp_dir/extracted" ||
  fail "could not extract $archive"
[ -f "$tmp_dir/extracted/red" ] ||
  fail 'release archive did not contain the red binary'

mkdir -p "$install_dir"
staged_binary="$install_dir/.red.install.$$"
cp "$tmp_dir/extracted/red" "$staged_binary"
chmod 755 "$staged_binary"
mv -f "$staged_binary" "$install_dir/red"
staged_binary=""

"$install_dir/red" --version
NO_COLOR=1 "$install_dir/red" --self-check

case ":${PATH:-}:" in
  *":$install_dir:"*) ;;
  *)
    printf '\nAdd %s to PATH to run red from any directory.\n' "$install_dir"
    ;;
esac

printf '\nRed is installed at %s/red.\n' "$install_dir"
printf 'Agent support is optional: install Codex CLI, then run codex login.\n'
