#!/usr/bin/env python3
"""Exports the app icon (shared/ui-assets/AppIcon-c.png) into a hicolor tree: <prefix>/share/icons/hicolor/<n>x<n>/apps/app.olaz.linux.png.

The checked-in copy lives in packaging/icons (`python3 packaging/make-icons.py packaging/icons`), so package builds need no Pillow."""
import pathlib
import sys

from PIL import Image

ROOT = pathlib.Path(__file__).resolve().parent.parent
SIZES = (16, 24, 32, 48, 64, 128, 256, 512)


def main(prefix: str) -> None:
    src = Image.open(ROOT / "shared/ui-assets/AppIcon-c.png").convert("RGBA")
    for n in SIZES:
        out = pathlib.Path(prefix) / f"hicolor/{n}x{n}/apps/app.olaz.linux.png"
        out.parent.mkdir(parents=True, exist_ok=True)
        src.resize((n, n), Image.LANCZOS).save(out, optimize=True)


if __name__ == "__main__":
    main(sys.argv[1])
