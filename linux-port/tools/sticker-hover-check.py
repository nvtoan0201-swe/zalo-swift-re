#!/usr/bin/env python3
"""Checks that a sprite-sheet sticker animates while hovered in the sticker picker (StickerPicker.swift `animatesOnHover`).

    python3 -I tools/sticker-hover-check.py [--out DIR]

Renders the Linux-only scene `L-sticker-hover` (fixtures/stickers.json: an 8-frame sprite sticker, 100 ms per frame)
with a throwaway OLAZ_HOME and the dev input channel, then:
  1. captures the first sticker cell twice without hover → must be identical (stills do not move);
  2. moves the pointer onto it and captures 6 times ~70 ms apart → at least 3 distinct pictures (it plays);
  3. moves the pointer away → identical again (it stops).
Exit 1 when any step fails. Saves the captures as a strip in --out.
"""
import argparse
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time

from PIL import Image

ROOT = pathlib.Path(__file__).resolve().parent.parent
BIN = ROOT / "app/target/debug/olaz"
CAPTURE = ROOT / "tools/x11-capture.py"
CELL = (10, 58, 82, 130)  # first grid cell in points within the picker: grid padding 10, under the 48-pt search bar


def window_id(title):
    out = subprocess.run(["xwininfo", "-root", "-tree"], capture_output=True, text=True).stdout
    for line in out.splitlines():
        if f'"{title}"' in line:
            return line.split()[0]
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=str(ROOT / "ui-reference/linux"))
    a = ap.parse_args()
    out = pathlib.Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    home = pathlib.Path(tempfile.mkdtemp(prefix="olaz-hover.", dir=pathlib.Path.home() / ".cache"))
    ready = home / "ready.json"
    env = dict(os.environ, TZ="Asia/Ho_Chi_Minh", OLAZ_HOME=str(home), OLAZ_DEV_EVAL="1")
    env.pop("WAYLAND_DISPLAY", None)
    proc = subprocess.Popen([str(BIN), "--ui-render", "L-sticker-hover", "--ready", str(ready)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    failures = []
    try:
        deadline = time.time() + 45
        while not ready.exists():
            if proc.poll() is not None or time.time() > deadline:
                sys.exit("render did not become ready")
            time.sleep(0.2)
        info = json.loads(ready.read_text())
        pad = info.get("padding", {})
        dev = home / "data/olaz/dev"
        dev.mkdir(parents=True, exist_ok=True)
        wid = window_id(info["title"])
        raw = home / "raw.png"

        def send(events):
            (dev / "input.out.txt").unlink(missing_ok=True)
            (dev / "input.json").write_text(json.dumps(events))
            for _ in range(50):
                if (dev / "input.out.txt").exists():
                    return
                time.sleep(0.1)
            failures.append("dev input not processed")

        def cell():
            subprocess.run([sys.executable, "-I", str(CAPTURE), str(raw), "--id", wid], capture_output=True, check=True)
            im = Image.open(raw).convert("RGB")
            scale = im.width / (info["width"] + pad.get("left", 0) + pad.get("right", 0))
            x0, y0, x1, y1 = [round((v + (pad.get("left", 0) if i % 2 == 0 else pad.get("top", 0))) * scale) for i, v in enumerate(CELL)]
            return im.crop((x0, y0, x1, y1))

        cx, cy = (CELL[0] + CELL[2]) / 2 + pad.get("left", 0), (CELL[1] + CELL[3]) / 2 + pad.get("top", 0)
        send([{"t": "move", "x": 300 + pad.get("left", 0), "y": 300 + pad.get("top", 0)}])
        time.sleep(0.5)
        still = [cell(), (time.sleep(0.35), cell())[1]]
        if still[0].tobytes() != still[1].tobytes():
            failures.append("the sticker moves without hover")
        # A pointer resting on the sticker: the move is re-sent every 80 ms for ~6 s (the dev channel plays the batch
        # while we capture), so stray real pointer motion on the desktop cannot end the hover.
        hold = []
        for _ in range(75):
            hold += [{"t": "move", "x": cx, "y": cy}, {"t": "wait", "ms": 80}]
        (dev / "input.out.txt").unlink(missing_ok=True)
        (dev / "input.json").write_text(json.dumps(hold))
        time.sleep(1.0)  # batch picked up + sprite sheet load
        playing = []
        for i in range(6):
            playing.append(cell())
            time.sleep(0.05 + 0.03 * i)  # varying phase against the 100-ms frames
        for _ in range(100):  # let the hold batch finish before the next input
            if (dev / "input.out.txt").exists():
                break
            time.sleep(0.1)
        distinct = len({p.tobytes() for p in playing})
        if distinct < 3:
            failures.append(f"hovered sticker shows {distinct} distinct pictures (expected ≥ 3)")
        send([{"t": "move", "x": 300 + pad.get("left", 0), "y": 300 + pad.get("top", 0)}])
        time.sleep(0.4)
        after = [cell(), (time.sleep(0.35), cell())[1]]
        if after[0].tobytes() != after[1].tobytes():
            failures.append("the sticker keeps moving after the pointer left")
        strip = still + playing + after
        sheet = Image.new("RGB", (sum(p.width for p in strip) + 4 * len(strip), strip[0].height), (80, 80, 80))
        x = 0
        for p in strip:
            sheet.paste(p, (x, 0))
            x += p.width + 4
        sheet.save(out / "L-sticker-hover-strip.png")
        print(f"still: {'ok' if still[0].tobytes() == still[1].tobytes() else 'moving'}; hovered: {distinct} distinct pictures in 6 captures; after: {'ok' if after[0].tobytes() == after[1].tobytes() else 'moving'}")
    finally:
        proc.terminate()
        try:
            proc.wait(5)
        except subprocess.TimeoutExpired:
            proc.kill()
        shutil.rmtree(home, ignore_errors=True)
    if failures:
        print("FAILED: " + "; ".join(failures))
        sys.exit(1)
    print("sticker hover animation: pass")


if __name__ == "__main__":
    main()
