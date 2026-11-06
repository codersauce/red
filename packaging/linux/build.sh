#!/usr/bin/env bash
# Build native packages from an extracted Red release archive.
set -euo pipefail
if [[ $# != 3 || ! $1 =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]]; then
  echo "usage: $0 VERSION EXTRACTED_ARCHIVE OUTPUT_DIRECTORY" >&2
  exit 2
fi
config="$(cd "$(dirname "$0")" && pwd)/nfpm.yaml"
export RED_VERSION="$1"
mkdir -p "$3"
dest="$(cd "$3" && pwd)"
cd "$2"
for format in deb rpm; do
  nfpm package --config "$config" --packager "$format" --target "$dest/"
done
