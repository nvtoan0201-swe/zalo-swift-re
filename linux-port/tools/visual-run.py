#!/usr/bin/env python3
"""Renders the UI scenes of `olaz --ui-render` to PNG (no Zalo account, no network: fixture data, fixed clock).

    python3 -I tools/visual-run.py [ID_PREFIX] [--appearance light|dark|both] [--out DIR] [--now MS]

For each scene (`olaz --ui-render --list` order, filtered by prefix) and appearance: start the debug `olaz` with a
throwaway OLAZ_HOME and TZ=Asia/Ho_Chi_Minh, wait for its ready file, capture the window (XWayland), crop the
client-decoration paddings so the PNG is exactly the content area, and save `<out>/<id>-<appearance>.png` plus the
element geometry `<out>/<id>-<appearance>.layout.json`. Exit 1 when a scene fails to render.
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

ROOT = pathlib.Path(__file__).resolve().parent.parent
APP = ROOT / "app"
BIN = APP / "target/debug/olaz"
CAPTURE = ROOT / "tools/x11-capture.py"
DEFAULT_NOW = 1794200000000  # 2026-11-09 15:33:20 +07: the scenes' "now", so relative times are reproducible


def catalog():
    out = subprocess.run([str(BIN), "--ui-render", "--list"], capture_output=True, text=True)
    ids = [l.strip() for l in out.stdout.splitlines() if l.strip()]
    if not ids:
        sys.exit(f"cannot list scenes: {out.stderr.strip()}")
    return ids


def window_id(title):
    out = subprocess.run(["xwininfo", "-root", "-tree"], capture_output=True, text=True).stdout
    for line in out.splitlines():
        if f'"{title}"' in line:
            return line.split()[0]
    return None


def mean_luma(path):
    from PIL import Image

    im = Image.open(path).convert("L").resize((64, 64))
    return sum(im.getdata()) / 4096


def render(scene, appearance, now, out_png, log):
    home = tempfile.mkdtemp(prefix="olaz-render.", dir=pathlib.Path.home() / ".cache")
    ready = pathlib.Path(home) / "ready.json"
    env = dict(os.environ, TZ="Asia/Ho_Chi_Minh", OLAZ_HOME=home)
    env.pop("WAYLAND_DISPLAY", None)
    cmd = [str(BIN), "--ui-render", scene, "--appearance", appearance, "--now", str(now), "--ready", str(ready)]
    proc = subprocess.Popen(cmd, env=env, stdout=log, stderr=log)
    try:
        deadline = time.time() + 45
        while not ready.exists():
            if proc.poll() is not None:
                return f"olaz exited with {proc.returncode}"
            if time.time() > deadline:
                return "timed out waiting for the ready file"
            time.sleep(0.2)
        info = json.loads(ready.read_text())
        raw = pathlib.Path(home) / "raw.png"
        for _ in range(10):
            wid = window_id(info["title"])
            if wid and subprocess.run([sys.executable, "-I", str(CAPTURE), str(raw), "--id", wid], capture_output=True).returncode == 0:
                if mean_luma(raw) > 3:
                    break
            time.sleep(0.5)
        else:
            return "capture stayed blank (window covered?)"
        from PIL import Image

        im = Image.open(raw)
        p = info.get("padding", {})
        outer_w = info["width"] + p.get("left", 0) + p.get("right", 0)
        scale = im.width / outer_w
        box = (round(p.get("left", 0) * scale), round(p.get("top", 0) * scale), round((p.get("left", 0) + info["width"]) * scale), round((p.get("top", 0) + info["height"]) * scale))
        im.crop(box).save(out_png)
        layout = ready.with_suffix(".layout.json")
        if layout.exists():
            shutil.copy(layout, out_png.with_suffix(".layout.json"))
        return None
    finally:
        proc.terminate()
        try:
            proc.wait(5)
        except subprocess.TimeoutExpired:
            proc.kill()
        shutil.rmtree(home, ignore_errors=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("prefix", nargs="?", default="")
    ap.add_argument("--appearance", default="both", choices=["light", "dark", "both"])
    ap.add_argument("--out", default=str(ROOT / "renders"))
    ap.add_argument("--now", type=int, default=DEFAULT_NOW)
    a = ap.parse_args()
    if not BIN.exists():
        sys.exit(f"build first: (cd {APP} && cargo build)")
    out = pathlib.Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    now = a.now
    appearances = ["light", "dark"] if a.appearance == "both" else [a.appearance]
    failures = []
    renders = []
    with open(out / "render.log", "w") as log:
        for scene in [s for s in catalog() if s.startswith(a.prefix)]:
            for appearance in appearances:
                png = out / f"{scene}-{appearance}.png"
                err = render(scene, appearance, now, png, log)
                status = err or "ok"
                print(f"{scene}-{appearance}: {status}", flush=True)
                if err:
                    failures.append(f"{scene}-{appearance}: {err}")
                else:
                    renders.append(png)
    print(f"{len(renders)} renders in {out}")
    if failures:
        print("FAILED:\n  " + "\n  ".join(failures))
        sys.exit(1)


if __name__ == "__main__":
    main()
