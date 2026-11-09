#!/usr/bin/env python3
"""Drive the running dev app: raise it, dispatch synthetic input (dev input channel), then capture a non-black screenshot.

usage: olaz-step.py <out.png> '<json events>'   (coordinates in logical px; see crates/olaz-ui/src/dev_input.rs)
"""
import json, os, subprocess, sys, time
from PIL import Image

dev = os.path.expanduser("~/.cache/olaz-dev/home/data/olaz/dev")
out, events = sys.argv[1], json.loads(sys.argv[2]) if len(sys.argv) > 2 else []
here = os.path.dirname(os.path.abspath(__file__))

def send(evs):
    open(os.path.join(dev, "input.json.tmp"), "w").write(json.dumps(evs))
    os.replace(os.path.join(dev, "input.json.tmp"), os.path.join(dev, "input.json"))
    for _ in range(100):
        time.sleep(0.1)
        if not os.path.exists(os.path.join(dev, "input.json")):
            break
    time.sleep(0.2 + 0.05 * len(evs))

send([{"t": "raise"}, {"t": "wait", "ms": 300}] + events + [{"t": "wait", "ms": 500}])
for attempt in range(6):
    subprocess.run([sys.executable, os.path.join(here, "x11-capture.py"), out, "--name", "Olaz"], capture_output=True)
    im = Image.open(out).convert("L")
    mean = sum(im.resize((64, 64)).getdata()) / 4096
    if mean > 3:
        print(f"ok {out} mean={mean:.0f}")
        sys.exit(0)
    send([{"t": "raise"}, {"t": "wait", "ms": 300}])
print("black (window covered)")
sys.exit(1)
