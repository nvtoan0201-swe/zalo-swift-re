#!/usr/bin/env python3
"""Capture an X11 (XWayland) window of a running app to PNG — used by the Linux visual checks.

usage: x11-capture.py <out.png> [--name SUBSTR] [--id 0x...]
Without --name/--id the largest top-level managed client window is captured.
"""
import argparse, subprocess, re, sys
import gi
gi.require_version("Gdk", "3.0")
gi.require_version("GdkX11", "3.0")
from gi.repository import Gdk, GdkX11

def windows():
    out = subprocess.run(["xwininfo", "-root", "-tree"], capture_output=True, text=True).stdout
    for line in out.splitlines():
        m = re.match(r'\s+(0x[0-9a-f]+) (".*?"|\(has no name\)): \((.*?)\)\s+(\d+)x(\d+)', line)
        if m:
            yield int(m.group(1), 16), m.group(2).strip('"'), m.group(3), int(m.group(4)), int(m.group(5))

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--name")
    ap.add_argument("--id")
    a = ap.parse_args()
    import os
    os.environ.pop("WAYLAND_DISPLAY", None)
    Gdk.set_allowed_backends("x11")
    display = Gdk.Display.open(os.environ.get("DISPLAY", ":0"))
    if a.id:
        wid = int(a.id, 16)
    else:
        cands = [w for w in windows() if "mutter" not in w[2] and "ibus" not in w[1] and w[3] > 50 and w[4] > 50]
        if a.name:
            cands = [w for w in cands if a.name.lower() in w[1].lower()]
        if not cands:
            sys.exit("no window found")
        wid = max(cands, key=lambda w: w[3] * w[4])[0]
    win = GdkX11.X11Window.foreign_new_for_display(display, wid)
    w, h = win.get_width(), win.get_height()
    pb = Gdk.pixbuf_get_from_window(win, 0, 0, w, h)
    if pb is None:
        sys.exit("capture failed")
    pb.savev(a.out, "png", [], [])
    print(f"{hex(wid)} {w}x{h} -> {a.out}")

main()
