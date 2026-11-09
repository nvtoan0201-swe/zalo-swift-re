#!/bin/sh
# Installs a release build into PREFIX: both binaries, the desktop entry, metainfo and icons.
# Usage: packaging/install.sh PREFIX [TARGET_DIR]   (TARGET_DIR defaults to app/target/release)
set -eu
here=$(cd "$(dirname "$0")" && pwd)
prefix=$1
target=${2:-$here/../app/target/release}
install -Dm755 "$target/olaz" "$prefix/bin/olaz"
install -Dm755 "$target/olaz-webhost" "$prefix/bin/olaz-webhost"
install -Dm644 "$here/app.olaz.linux.desktop" "$prefix/share/applications/app.olaz.linux.desktop"
install -Dm644 "$here/app.olaz.linux.metainfo.xml" "$prefix/share/metainfo/app.olaz.linux.metainfo.xml"
mkdir -p "$prefix/share/icons"
cp -r "$here/icons/hicolor" "$prefix/share/icons/"
