#!/bin/bash
# Native-Wayland screenshot of a `--ui-render` scene (no XWayland): a private headless GNOME Shell with its own D-Bus
# session, runtime and data dirs (nothing touches the user's session or permission store), the scene on its Wayland
# display, and a screenshot through that session's xdg-desktop-portal.
#   dbus-run-session -- tools/wayland-render-shot.sh OUT.png SCENE app/target/debug/olaz
# (the portal also drops a copy in ~/Screenshot*.png; remove it afterwards)
set -u
OUT=$1; SCENE=$2; APP=$3
export XDG_RUNTIME_DIR=$(mktemp -d -p $HOME/.cache olaz-wlrt.XXXX); chmod 700 $XDG_RUNTIME_DIR
export XDG_DATA_HOME=$XDG_RUNTIME_DIR/data XDG_CONFIG_HOME=$XDG_RUNTIME_DIR/config XDG_CACHE_HOME=$XDG_RUNTIME_DIR/cache; mkdir -p $XDG_DATA_HOME $XDG_CONFIG_HOME $XDG_CACHE_HOME
unset DISPLAY WAYLAND_DISPLAY
gnome-shell --headless --no-x11 --virtual-monitor 1500x1000 --wayland-display olaz-wl > $XDG_RUNTIME_DIR/shell.log 2>&1 &
SHELL_PID=$!
for i in $(seq 60); do [ -S $XDG_RUNTIME_DIR/olaz-wl ] && break; sleep 0.5; done
[ -S $XDG_RUNTIME_DIR/olaz-wl ] || { echo "no wayland socket"; tail -20 $XDG_RUNTIME_DIR/shell.log; kill $SHELL_PID; exit 1; }
sleep 3
H=$(mktemp -d -p $HOME/.cache olaz-c.XXXX)
WAYLAND_DISPLAY=olaz-wl TZ=Asia/Ho_Chi_Minh OLAZ_HOME=$H $APP --ui-render $SCENE --ready $H/r.json > $H/app.log 2>&1 &
APP_PID=$!
for i in $(seq 80); do [ -s $H/r.json ] && break; sleep 0.5; done
[ -s $H/r.json ] || { echo "app not ready"; tail -20 $H/app.log; }
sleep 1
WAYLAND_DISPLAY=olaz-wl python3 -I "$(dirname "$0")/wayland-portal-shot.py" $OUT 2>&1 | tail -3
cat $H/r.json; echo
kill $APP_PID $SHELL_PID 2>/dev/null; sleep 1; kill -9 $SHELL_PID 2>/dev/null
rm -rf $H $XDG_RUNTIME_DIR
