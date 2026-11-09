#!/usr/bin/env bash
# Downloads the bundled UI fonts (all SIL OFL 1.1) into shared/ui-assets/fonts/ and verifies them against
# shared/ui-assets/fonts/SHA256SUMS. The fonts are committed (with their licenses); run this to refresh them.
# First run without SHA256SUMS records it (review the sources then).
#
#   linux-port/tools/fetch-fonts.sh
set -euo pipefail

PORT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$PORT/shared/ui-assets/fonts"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

INTER_ZIP="https://github.com/rsms/inter/releases/download/v4.1/Inter-4.1.zip"
NUNITO="https://raw.githubusercontent.com/google/fonts/main/ofl/nunito/Nunito%5Bwght%5D.ttf"
NUNITO_OFL="https://raw.githubusercontent.com/google/fonts/main/ofl/nunito/OFL.txt"
EMOJI="https://raw.githubusercontent.com/google/fonts/main/ofl/notocoloremoji/NotoColorEmoji-Regular.ttf"
EMOJI_OFL="https://raw.githubusercontent.com/google/fonts/main/ofl/notocoloremoji/OFL.txt"

mkdir -p "$DEST"
curl -fsSL "$INTER_ZIP" -o "$WORK/inter.zip"
unzip -q -o "$WORK/inter.zip" -d "$WORK/inter"
cp "$WORK/inter/InterVariable.ttf" "$WORK/inter/InterVariable-Italic.ttf" "$DEST/"
cp "$WORK/inter/LICENSE.txt" "$DEST/Inter-LICENSE.txt"
curl -fsSL "$NUNITO" -o "$DEST/Nunito[wght].ttf"
curl -fsSL "$NUNITO_OFL" -o "$DEST/Nunito-OFL.txt"
curl -fsSL "$EMOJI" -o "$DEST/NotoColorEmoji-Regular.ttf"
curl -fsSL "$EMOJI_OFL" -o "$DEST/NotoColorEmoji-OFL.txt"

cd "$DEST"
if [[ -f SHA256SUMS ]]; then
  sha256sum -c --quiet SHA256SUMS && echo "fonts verified"
else
  sha256sum -- *.ttf > SHA256SUMS
  echo "recorded SHA256SUMS (first run) — commit it"
fi
