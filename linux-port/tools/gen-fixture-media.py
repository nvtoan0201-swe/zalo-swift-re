#!/usr/bin/env python3
"""Generates the synthetic images referenced by fixtures/*.json (`fixture:<path>` URLs).

Deterministic (fixed seeds, no fonts, no timestamps in metadata) so re-running gives byte-identical files and the
macOS captures and Linux renders see exactly the same pixels. No real people or Zalo content.
"""
import pathlib
import random

from PIL import Image, ImageDraw

ROOT = pathlib.Path(__file__).resolve().parent.parent / "fixtures"


def gradient(size, top, bottom):
    w, h = size
    img = Image.new("RGB", size)
    px = img.load()
    for y in range(h):
        t = y / max(h - 1, 1)
        row = tuple(round(a + (b - a) * t) for a, b in zip(top, bottom))
        for x in range(w):
            px[x, y] = row
    return img


def avatar(name, top, bottom, shape_color):
    img = gradient((256, 256), top, bottom)
    d = ImageDraw.Draw(img)
    d.ellipse((78, 48, 178, 148), fill=shape_color)          # head
    d.ellipse((38, 160, 218, 340), fill=shape_color)         # shoulders
    img.save(ROOT / "avatars" / f"{name}.png", optimize=True)


def landscape():
    img = gradient((1600, 1067), (120, 170, 220), (250, 220, 180))
    d = ImageDraw.Draw(img)
    rng = random.Random(7)
    for i in range(3):                                         # hills
        base = 600 + i * 140
        color = (40 + i * 30, 110 + i * 25, 70 + i * 15)
        points = [(0, 1067)] + [(x, base - rng.randint(0, 120)) for x in range(0, 1700, 100)] + [(1600, 1067)]
        d.polygon(points, fill=color)
    d.ellipse((1180, 140, 1340, 300), fill=(255, 240, 200))   # sun
    img.save(ROOT / "media" / "photo-landscape.jpg", quality=88)


def sticker():
    img = Image.new("RGBA", (240, 240), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.ellipse((20, 20, 220, 220), fill=(255, 205, 60, 255), outline=(200, 140, 20, 255), width=6)
    d.ellipse((70, 80, 100, 115), fill=(60, 40, 20, 255))
    d.ellipse((140, 80, 170, 115), fill=(60, 40, 20, 255))
    d.arc((65, 100, 175, 185), start=20, end=160, fill=(60, 40, 20, 255), width=8)
    img.save(ROOT / "media" / "sticker.webp", lossless=True)


def sticker_sprite():
    """8 square frames side by side (the official `stickerSpriteUrl` layout): a ball moving across, one colour per frame."""
    frames, size = 8, 128
    img = Image.new("RGBA", (frames * size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    for i in range(frames):
        x = i * size + 14 + i * 10
        hue = [(255, 90, 90), (255, 160, 60), (240, 210, 40), (90, 200, 90), (60, 170, 230), (90, 110, 240), (170, 90, 230), (230, 80, 170)][i]
        d.ellipse((x, 34, x + 30, 64 + 30), fill=hue + (255,))
    img.save(ROOT / "media" / "sticker-sprite.png", optimize=True)


def sample_pdf():
    """A one-page PDF (fixed bytes) for the file message, so downloads and "Lưu về máy…" have something to fetch."""
    body = b"BT /F1 24 Tf 72 720 Td (Ke hoach chuyen di Da Lat) Tj ET"
    objs = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
        b"<< /Length %d >>\nstream\n" % len(body) + body + b"\nendstream",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    ]
    out = bytearray(b"%PDF-1.4\n")
    offsets = []
    for i, o in enumerate(objs, 1):
        offsets.append(len(out))
        out += b"%d 0 obj\n" % i + o + b"\nendobj\n"
    xref = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objs) + 1)
    out += b"".join(b"%010d 00000 n \n" % off for off in offsets)
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objs) + 1, xref)
    (ROOT / "media" / "ke-hoach.pdf").write_bytes(bytes(out))


def qr_like():
    """A QR-looking pattern (not a scannable code: fixtures must never log anybody in)."""
    n, cell = 33, 8
    rng = random.Random(42)
    img = Image.new("RGB", (n * cell + 32, n * cell + 32), "white")
    d = ImageDraw.Draw(img)

    def finder(cx, cy):
        for size, color in ((7, "black"), (5, "white"), (3, "black")):
            off = (7 - size) // 2
            d.rectangle((16 + (cx + off) * cell, 16 + (cy + off) * cell,
                         16 + (cx + off + size) * cell - 1, 16 + (cy + off + size) * cell - 1), fill=color)

    for y in range(n):
        for x in range(n):
            in_finder = (x < 8 and y < 8) or (x >= n - 8 and y < 8) or (x < 8 and y >= n - 8)
            if not in_finder and rng.random() < 0.48:
                d.rectangle((16 + x * cell, 16 + y * cell, 16 + x * cell + cell - 1, 16 + y * cell + cell - 1), fill="black")
    finder(0, 0), finder(n - 7, 0), finder(0, n - 7)
    img.save(ROOT / "media" / "qr-sample.png", optimize=True)


def main():
    (ROOT / "avatars").mkdir(parents=True, exist_ok=True)
    (ROOT / "media").mkdir(parents=True, exist_ok=True)
    avatar("minh-anh", (255, 183, 130), (230, 110, 90), (255, 240, 225))
    avatar("bao-ngoc", (150, 200, 255), (90, 120, 230), (235, 245, 255))
    landscape()
    sticker()
    sticker_sprite()
    sample_pdf()
    qr_like()
    print("fixture media written to", ROOT)


if __name__ == "__main__":
    main()
