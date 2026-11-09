# Olaz for Linux

**A native Linux client for [Zalo](https://zalo.me), written in Rust with [GPUI](https://www.gpui.rs).**

This is the Linux port of Olaz (the macOS app in this repository). It works the same way: it does **not**
re-implement Zalo's protocol. It runs the **official Zalo PC renderer** (`app.asar`) hidden inside WebKitGTK and
supplies in Rust everything Electron's preload and main process used to provide (IPC bridge, network proxy,
storage, crypto, notifications, file access). The interface is native GPUI, driven by the official client's state.

Zalo PC does not need to be installed. On first launch Olaz downloads a pinned release of the official Zalo PC
package from VNG, checks its SHA-256, unpacks it with 7-Zip and keeps only `app.asar` (and its `Info.plist`) in
`~/.local/share/olaz/ZaloRuntime/`.

> [!IMPORTANT]
> **Unofficial project, not affiliated with or endorsed by VNG Corporation.** "Zalo" is a trademark of VNG.
> Using an unofficial client may conflict with Zalo's terms of service; use it at your own risk with your own
> account. Zalo allows **one PC session per account**: quit other Zalo PC sessions while using Olaz.

## Features

- QR login (scan with Zalo on your phone), session restored on relaunch
- Chat: history, text / reply / mentions, reactions and reaction details, typing, read state, realtime updates
- Photos, files, stickers (animated on hover), polls, reminders, contact and link cards
- Video and voice messages in a built-in player (GStreamer)
- "Lưu về máy…" on files, photos, videos and voice: the system save dialog picks the folder
- Message actions: forward, copy, recall, delete with undo, pin bar, star
- Conversation actions: pin, mute, labels ("Phân loại"), hidden chats with PIN, move to "Khác"
- Contacts, friend requests, add friend, group creation, shared media, zCloud, My Cloud
- Phone → PC message sync, desktop notifications, dock badge, quick switcher (Ctrl+K), settings (Ctrl+,)
- Light / dark, accent colors, text size, chat wallpapers

Not available on Linux: voice/video **calls** (Zalo's call helper exists only for macOS and Windows).

## How it works

```
┌──────────────┐  IPC (length-prefixed JSON)  ┌───────────────────────────────┐
│ olaz (GPUI)  │ ◀──────────────────────────▶ │ olaz-webhost (WebKitGTK 4.1)  │
│  UI + engine │                              │  official Zalo renderer,      │
│  main process│ ◀─ media, network, storage ─ │  hidden views + host scripts  │
└──────────────┘                              └───────────────────────────────┘
```

| Crate | Role |
|---|---|
| `olaz` | the executable: wires engine, webhost and UI |
| `olaz-ui` | the GPUI interface (shell, chat, sheets, settings, player, …) |
| `olaz-engine` | `ZaloEngine`: the state the UI binds to, built from the official client's snapshots; no GPUI |
| `olaz-host` | webhost process management, the `$z*` bridge, network proxy |
| `olaz-webhost` | separate process hosting the official renderer in hidden WebKitGTK views |
| `olaz-main` | Electron main-process emulation: IPC channels, stores, secrets (Secret Service), shared worker |
| `olaz-media` | CDN media loader (cache, E2EE streams), downloads, GStreamer player |
| `olaz-runtime` | downloads and installs the official Zalo runtime |
| `olaz-core` | shared pieces: asar reader, scheme handler, bridge values, crypto, paths, logging |
| `olaz-ipc` | the webhost wire protocol |

`shared/host-scripts/` are the JavaScript adapters injected into the official renderer, copied verbatim from the
macOS app (`tools/sync-from-macos.sh` refreshes them), so both apps read Zalo's state the same way.

## Requirements

- Linux with Wayland or X11, a Zalo account and the Zalo app on your phone
- Rust 1.98 (pinned in `app/rust-toolchain.toml`; `rustup` installs it)
- System packages to build — on Debian/Ubuntu:

  ```bash
  sudo apt install build-essential pkg-config libwebkit2gtk-4.1-dev libxkbcommon-x11-dev libwayland-dev \
      libfontconfig1-dev libvulkan1 7zip
  ```

  (Fedora: `webkit2gtk4.1-devel libxkbcommon-x11-devel wayland-devel fontconfig-devel vulkan-loader` and `7zip` or `p7zip`)
- Optional, for videos and voice messages: GStreamer 1.x with `gst-plugins-good`, and `gst-libav` for H.264/AAC
  (Zalo videos). Without a decoder the player says so and offers to open the file in another app.

## Build & run

```bash
cd linux-port/app
cargo build --release
./target/release/olaz          # first launch downloads the Zalo runtime, then shows the login QR
```

`olaz` finds `olaz-webhost` next to itself. Data lives in the usual XDG folders:

| What | Where |
|---|---|
| account data, Zalo runtime | `~/.local/share/olaz/` |
| settings | `~/.config/olaz/settings.json` |
| media cache | `~/.cache/olaz/` |
| logs | `~/.local/state/olaz/logs/` |

Set `OLAZ_HOME=/some/dir` to keep everything under one folder (a separate profile).

## Packages

**AppImage** (uses the system's WebKitGTK, GStreamer and 7-Zip):

```bash
cd linux-port/app && cargo build --release --locked -p olaz -p olaz-webhost
cd .. && packaging/appimage/build.sh        # → dist/Olaz-x86_64.AppImage
```

**Flatpak** (GNOME 51 runtime; builds offline from `packaging/flatpak/cargo-sources.json`):

```bash
flatpak install --user flathub org.flatpak.Builder org.gnome.Sdk//51 org.freedesktop.Sdk.Extension.rust-stable//26.08
cd linux-port
packaging/flatpak/gen-cargo-sources.sh      # after Cargo.lock changes
flatpak run org.flatpak.Builder --user --install --force-clean build-flatpak packaging/flatpak/app.olaz.linux.yml
flatpak run app.olaz.linux
```

The app id is `app.olaz.linux`. H.264/AAC in the Flatpak come from `org.freedesktop.Platform.codecs-extra`.

## Development

```bash
cd linux-port/app
cargo test --workspace         # engine, crypto parity, media, player, UI logic, fixtures
```

- **Scene renders without an account.** Debug builds have `olaz --ui-render <scene>`: the UI built from the
  fixtures in `fixtures/` (fictional data, fixed clock, no network). `--ui-render --list` prints the scenes;
  `python3 tools/visual-run.py [prefix]` renders them all (light and dark) to `renders/` with a layout dump.
- **Driving a running debug build.** With `OLAZ_DEV_EVAL=1`, files in `<data>/olaz/dev/` control the app:
  `input.json` (synthetic clicks, keys, text), `open-conv.txt`, `eval.js` (runs in the official renderer).
  `tools/olaz-step.py` sends events and captures the window (`tools/x11-capture.py`, XWayland).
- **Checks:** `tools/sticker-hover-check.py` (animated stickers), `tools/wayland-render-shot.sh` (a scene on
  native Wayland in a private headless GNOME Shell).
- **Keeping up with the macOS app:** `tools/sync-from-macos.sh [--check]` re-copies the host scripts, JS modules,
  bridge table and UI assets and reports what changed on the Swift side since the last sync.

`app/vendor/` holds two patched crates: `zed-xim` (X11 input methods) and `gpui-component` (window border
rounded on all corners).

## License

Same as the repository (see `../LICENSE`). Bundled fonts (Inter, Nunito, Noto Color Emoji) are under the SIL Open
Font License; icons are from [Phosphor](https://phosphoricons.com) (MIT).
