#!/usr/bin/env python3
"""Copies the Phosphor SVGs that app/crates/olaz-ui/assets/icon-map.json uses into shared/ui-assets/icons/phosphor/<weight>/.

    npm pack @phosphor-icons/core && tar xzf phosphor-icons-core-*.tgz      # in an empty scratch folder
    python3 -I linux-port/tools/sync-icons.py <scratch>/package

Only the used icons are copied (plus the package LICENSE and a VERSION file); stale ones are removed.
"""
import json
import pathlib
import shutil
import sys

PORT = pathlib.Path(__file__).resolve().parent.parent
MAP = PORT / "app/crates/olaz-ui/assets/icon-map.json"
DEST = PORT / "shared/ui-assets/icons/phosphor"


def names(entry):
    yield entry["icon"], entry["weight"]
    if "badge" in entry:
        yield from names(entry["badge"])
    if "glyph" in entry:
        yield entry["glyph"], "fill"


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    package = pathlib.Path(sys.argv[1])
    assets = package / "assets"
    version = json.loads((package / "package.json").read_text())["version"]
    wanted = set()
    for entry in json.loads(MAP.read_text())["symbols"].values():
        wanted.update(names(entry))
    if DEST.exists():
        shutil.rmtree(DEST)
    for name, weight in sorted(wanted):
        file = f"{name}.svg" if weight == "regular" else f"{name}-{weight}.svg"
        src = assets / weight / file
        if not src.exists():
            sys.exit(f"missing {src} — run tools/gen-icon-map.py --phosphor {assets}")
        (DEST / weight).mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, DEST / weight / file)
    shutil.copy2(package / "LICENSE", DEST / "LICENSE")
    (DEST / "VERSION").write_text(f"@phosphor-icons/core {version}\n")
    print(f"copied {len(wanted)} icons (Phosphor {version}) to {DEST.relative_to(PORT)}")


if __name__ == "__main__":
    main()
