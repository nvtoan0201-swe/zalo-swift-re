#!/bin/sh
# Installs a release build into PREFIX: both binaries, the desktop entry, metainfo and icons.
# Usage: packaging/install.sh PREFIX [TARGET_DIR]   (TARGET_DIR defaults to app/target/release)
set -eu
here=$(cd "$(dirname "$0")" && pwd)
prefix=$1
target=${2:-$here/../app/target/release}
install -Dm755 "$target/olaz" "$prefix/bin/olaz"
install -Dm755 "$target/olaz-webhost" "$prefix/bin/olaz-webhost"
# Chromium-fingerprint network helper (fingerprint-helper/build.sh); optional, Olaz falls back to rustls without it.
helper=${OLAZ_FINGERPRINT_HELPER_BIN:-$here/../fingerprint-helper/build/olaz-fingerprint-helper}
if [ -x "$helper" ]; then
  install -Dm755 "$helper" "$prefix/bin/olaz-fingerprint-helper"
elif [ ! -x "$prefix/bin/olaz-fingerprint-helper" ]; then
  # (The Flatpak builds it in its own module, before this one.)
  echo "warning: $helper not built — run fingerprint-helper/build.sh" >&2
fi
install -Dm644 "$here/app.olaz.linux.desktop" "$prefix/share/applications/app.olaz.linux.desktop"
install -Dm644 "$here/app.olaz.linux.metainfo.xml" "$prefix/share/metainfo/app.olaz.linux.metainfo.xml"
mkdir -p "$prefix/share/icons"
cp -r "$here/icons/hicolor" "$prefix/share/icons/"
