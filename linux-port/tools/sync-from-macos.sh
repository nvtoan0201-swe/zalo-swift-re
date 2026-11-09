#!/usr/bin/env bash
# Re-copies everything the Linux port takes verbatim from the macOS app (host scripts, JS modules, bridge table, UI
# assets) and re-extracts the scripts kept inline in Swift. The macOS tree is only read.
#
#   linux-port/tools/sync-from-macos.sh           sync (writes linux-port/)
#   linux-port/tools/sync-from-macos.sh --check   report drift only; exit 1 when linux-port/ is out of date
#
# After a sync: review the changes, port the logic changes the drift shows to Rust, then record the macOS
# commit in linux-port/.last-sync (done automatically on a successful sync).
set -euo pipefail

PORT="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$PORT/.." && pwd)"
SRC="$REPO/Sources/Olaz"
CHECK=0
[[ "${1:-}" == "--check" ]] && CHECK=1

# source → destination (directories are mirrored; files copied)
COPIES=(
  "$SRC/HostScripts/|shared/host-scripts/"
  "$SRC/JSModules/|shared/js-modules/"
  "$SRC/Resources/bridge-table.json|shared/resources/bridge-table.json"
  "$SRC/Resources/Olaz/|shared/ui-assets/"
  "$REPO/Assets/olaz.icns|shared/ui-assets/olaz.icns"
)
# Files that exist only on the Linux side inside mirrored folders (never deleted by the mirror).
LINUX_ONLY=(--exclude linux/ --exclude tests/ --exclude olaz.icns --exclude fonts/ --exclude icons/ --exclude console-hook.js --exclude dal-worker-prelude.js --exclude dal-worker-debug-eval.js)

drift=0
for entry in "${COPIES[@]}"; do
  from="${entry%%|*}"; to="$PORT/${entry##*|}"
  if [[ "$from" == */ ]]; then
    if (( CHECK )); then
      out="$(rsync -rcn --delete --itemize-changes "${LINUX_ONLY[@]}" "$from" "$to" | grep -v '^\.d' || true)"
      [[ -n "$out" ]] && { echo "drift: ${entry##*|}"; echo "$out" | sed 's/^/    /'; drift=1; }
    else
      mkdir -p "$to"
      rsync -rc --delete "${LINUX_ONLY[@]}" "$from" "$to"
    fi
  else
    if ! cmp -s "$from" "$to"; then
      if (( CHECK )); then echo "drift: ${entry##*|}"; drift=1; else mkdir -p "$(dirname "$to")"; cp "$from" "$to"; fi
    fi
  fi
done

if (( CHECK )); then
  python3 -I "$PORT/tools/extract-inline-scripts.py" --check || drift=1
else
  python3 -I "$PORT/tools/extract-inline-scripts.py"
  for f in "$PORT"/shared/host-scripts/*.js; do node --check "$f"; done
fi

# What changed on the macOS side since the last sync (logic and tests to port).
if git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1; then
  head_sha="$(git -C "$REPO" rev-parse HEAD)"
  last="$(cat "$PORT/.last-sync" 2>/dev/null || true)"
  if [[ -n "$last" && "$last" != "$head_sha" ]]; then
    echo "macOS changes since last sync ($last..$head_sha):"
    git -C "$REPO" diff --stat "$last" "$head_sha" -- Sources Tests | sed 's/^/    /'
  fi
  (( CHECK )) || { (( drift )) || echo "$head_sha" > "$PORT/.last-sync"; }
fi

if (( CHECK )); then
  (( drift )) && { echo "linux-port is out of date with the macOS sources"; exit 1; }
  echo "linux-port is in sync"
else
  (( drift )) && { echo "sync finished with errors"; exit 1; }
  echo "sync done"
fi
