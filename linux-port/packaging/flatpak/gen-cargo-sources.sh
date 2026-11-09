#!/bin/sh
# Regenerates cargo-sources.json (offline crate sources for flatpak-builder) from app/Cargo.lock.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
gen=$(mktemp -d)
trap 'rm -rf "$gen"' EXIT
curl -sSL -o "$gen/flatpak-cargo-generator.py" \
  https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py
python3 -m venv "$gen/venv"
"$gen/venv/bin/pip" install -q aiohttp tomlkit PyYAML
"$gen/venv/bin/python" -I "$gen/flatpak-cargo-generator.py" "$here/../../app/Cargo.lock" -o "$here/cargo-sources.json"
