#!/bin/sh

set -eu

installer="${1:-install/install.sh}"
root="$(mktemp -d "${TMPDIR:-/tmp}/red-installer-test.XXXXXX")"

cleanup() {
  rm -rf "$root"
}
trap cleanup EXIT HUP INT TERM

release_dir="$root/releases/download/v9.9.9"
payload_dir="$root/payload"
mkdir -p "$release_dir" "$payload_dir"

cat > "$payload_dir/red" <<'EOF'
#!/bin/sh
case "${1:-}" in
  --version) printf 'red 9.9.9\n' ;;
  --self-check) printf 'red self-check ok\n' ;;
  *) exit 1 ;;
esac
EOF
chmod +x "$payload_dir/red"
tar -C "$payload_dir" -czf "$release_dir/red-x86_64-unknown-linux-gnu.tar.gz" red

if command -v sha256sum >/dev/null 2>&1; then
  checksum="$(sha256sum "$release_dir/red-x86_64-unknown-linux-gnu.tar.gz" | awk '{ print $1 }')"
else
  checksum="$(shasum -a 256 "$release_dir/red-x86_64-unknown-linux-gnu.tar.gz" | awk '{ print $1 }')"
fi
printf '%s  %s\n' "$checksum" "red-x86_64-unknown-linux-gnu.tar.gz" > "$release_dir/SHA256SUMS.txt"

install_dir="$root/install dir"
RED_VERSION=9.9.9 \
RED_INSTALL_DIR="$install_dir" \
RED_RELEASES_URL="file://$root/releases" \
RED_INSTALLER_OS=Linux \
RED_INSTALLER_ARCH=x86_64 \
sh "$installer"

"$install_dir/red" --self-check | grep -qx 'red self-check ok'

# Reinstalling over an existing binary must remain safe.
RED_VERSION=v9.9.9 \
RED_INSTALL_DIR="$install_dir" \
RED_RELEASES_URL="file://$root/releases" \
RED_INSTALLER_OS=Linux \
RED_INSTALLER_ARCH=amd64 \
sh "$installer"

printf '%064d  %s\n' 0 "red-x86_64-unknown-linux-gnu.tar.gz" > "$release_dir/SHA256SUMS.txt"
if RED_VERSION=9.9.9 \
  RED_INSTALL_DIR="$root/bad-checksum" \
  RED_RELEASES_URL="file://$root/releases" \
  RED_INSTALLER_OS=Linux \
  RED_INSTALLER_ARCH=x86_64 \
  sh "$installer"; then
  printf 'checksum mismatch unexpectedly succeeded\n' >&2
  exit 1
fi

if RED_INSTALLER_OS=Linux RED_INSTALLER_ARCH=aarch64 sh "$installer"; then
  printf 'unsupported architecture unexpectedly succeeded\n' >&2
  exit 1
fi
