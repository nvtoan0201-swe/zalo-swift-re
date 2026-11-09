#!/bin/sh
# Builds Olaz-x86_64.AppImage from a release build. Uses the system WebKitGTK 4.1, GStreamer and 7-Zip
# (not bundled); appimagetool is fetched into ~/.cache/olaz-packaging when missing.
# Usage: packaging/appimage/build.sh [OUT_DIR]   (needs: cd app && cargo build --release --locked -p olaz -p olaz-webhost)
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$here/../..
out=${1:-$root/dist}
appdir=$(mktemp -d)/Olaz.AppDir
trap 'rm -rf "$(dirname "$appdir")"' EXIT

"$root/packaging/install.sh" "$appdir/usr" "$root/app/target/release"
cp "$root/packaging/app.olaz.linux.desktop" "$appdir/"
cp "$appdir/usr/share/icons/hicolor/256x256/apps/app.olaz.linux.png" "$appdir/app.olaz.linux.png"
ln -s app.olaz.linux.png "$appdir/.DirIcon"
cat > "$appdir/AppRun" <<'RUN'
#!/bin/sh
here=$(dirname "$(readlink -f "$0")")
exec "$here/usr/bin/olaz" "$@"
RUN
chmod +x "$appdir/AppRun"

tool=$(command -v appimagetool || true)
if [ -z "$tool" ]; then
  tool=$HOME/.cache/olaz-packaging/appimagetool-x86_64.AppImage
  if [ ! -x "$tool" ]; then
    mkdir -p "$(dirname "$tool")"
    curl -sSL -o "$tool" https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
    chmod +x "$tool"
  fi
fi
mkdir -p "$out"
ARCH=x86_64 "$tool" --no-appstream "$appdir" "$out/Olaz-x86_64.AppImage"
echo "$out/Olaz-x86_64.AppImage"
