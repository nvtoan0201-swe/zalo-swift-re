#!/bin/sh
# Builds fingerprint-helper/build/olaz-fingerprint-helper (upstream's FingerprintHelper, Chromium 108's TLS + HTTP/2).
# packaging/install.sh installs it next to `olaz`; without it Olaz uses its built-in (rustls) stack.
# `--vendor` also refreshes vendor/ for offline (Flatpak) builds.
set -eu
cd "$(dirname "$0")"
command -v go >/dev/null || { echo "warning: Go toolchain not found — the fingerprint helper is not built" >&2; exit 0; }
# Never under /tmp (per-user quota): module and build caches stay in ~/.cache unless set.
export GOPATH="${GOPATH:-$HOME/.cache/olaz-dev/go}" GOCACHE="${GOCACHE:-$HOME/.cache/olaz-dev/go-build}"
export GOTMPDIR="${GOTMPDIR:-$HOME/.cache/olaz-dev/gotmp}" CGO_ENABLED=0
mkdir -p "$GOTMPDIR" build
if [ "${1:-}" = "--vendor" ]; then go mod vendor; fi
go build -trimpath -ldflags="-s -w" -o build/olaz-fingerprint-helper .
echo "built $(pwd)/build/olaz-fingerprint-helper"
