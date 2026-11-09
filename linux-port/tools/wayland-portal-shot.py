#!/usr/bin/env python3
"""Screenshot through xdg-desktop-portal (non-interactive), for tools/wayland-render-shot.sh: grants the permission in
the private session's temporary permission store first, then copies the result to argv[1]."""
import sys, shutil, urllib.parse
from gi.repository import Gio, GLib
out = sys.argv[1]
bus = Gio.bus_get_sync(Gio.BusType.SESSION)
# Pre-grant the (private, temporary) permission store entry for unsandboxed callers.
try:
    bus.call_sync("org.freedesktop.impl.portal.PermissionStore", "/org/freedesktop/impl/portal/PermissionStore",
                  "org.freedesktop.impl.portal.PermissionStore", "SetPermission",
                  GLib.Variant("(sbssas)", ("screenshot", True, "screenshot", "", ["yes"])), None, 0, 5000, None)
except Exception as e:
    print("permission:", e)
loop = GLib.MainLoop(); result = {}
def on_response(conn, sender, path, iface, signal, params, *a):
    code, res = params.unpack(); result["code"] = code; result["uri"] = res.get("uri"); loop.quit()
token = "olaz1"
sender = bus.get_unique_name()[1:].replace(".", "_")
handle = f"/org/freedesktop/portal/desktop/request/{sender}/{token}"
bus.signal_subscribe("org.freedesktop.portal.Desktop", "org.freedesktop.portal.Request", "Response", handle, None, 0, on_response)
bus.call_sync("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.Screenshot", "Screenshot",
              GLib.Variant("(sa{sv})", ("", {"interactive": GLib.Variant("b", False), "handle_token": GLib.Variant("s", token)})), None, 0, 10000, None)
GLib.timeout_add_seconds(20, loop.quit); loop.run()
print("response", result)
if result.get("code") == 0 and result.get("uri"):
    shutil.copy(urllib.parse.unquote(urllib.parse.urlparse(result["uri"]).path), out)
