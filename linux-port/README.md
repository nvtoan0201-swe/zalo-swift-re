# Olaz for Linux

**A native Linux client for [Zalo](https://zalo.me), written in Rust with [GPUI](https://www.gpui.rs).**

This is the Linux port of Olaz (the macOS app in this repository). Like the macOS app it does **not**
re-implement Zalo's protocol: it runs the **official Zalo PC renderer** (`app.asar`) hidden inside WebKitGTK and
supplies in Rust everything Electron's preload and main process used to provide (IPC bridge, network, storage,
crypto, notifications, file access). The interface is native GPUI, driven by the official client's state.

Zalo PC does not need to be installed. On first launch Olaz downloads a pinned release of the official Zalo PC
package from VNG, checks its SHA-256, unpacks it with 7-Zip and keeps only `app.asar` (and its `Info.plist`) in
`~/.local/share/olaz/ZaloRuntime/`.

> [!IMPORTANT]
> **Unofficial project, not affiliated with or endorsed by VNG Corporation.** "Zalo" is a trademark of VNG.
> Using an unofficial client may conflict with Zalo's terms of service; use it at your own risk with your own
> account. Zalo allows **one PC session per account**: quit other Zalo PC sessions while using Olaz.

## Features

- QR login (scan with Zalo on your phone, including the two-step trusted-device QR), session restored on relaunch
- Chat: history, text / reply / mentions, emoji picker, reactions, typing, read state, realtime updates
- Photos, files, stickers (animated on hover), polls, reminders, contact and link cards
- Video and voice messages in a built-in player (GStreamer)
- "Lưu về máy…" on files, photos, videos and voice
- Message actions: forward, copy, recall, delete with undo, pin bar, star
- Conversation actions: pin, mute, labels ("Phân loại"), hidden chats with PIN, move to "Khác", leave group
- Contacts, friend requests, add friend, group creation, shared media, zCloud, My Cloud
- Phone → PC message sync, desktop notifications, launcher badge, quick switcher (Ctrl+K), settings (Ctrl+,)
- Light / dark, accent colors, text size, chat wallpapers

Not available on Linux: voice/video **calls** (Zalo's call helper exists only for macOS and Windows).

## Requirements

- Linux with Wayland or X11, a Zalo account and the Zalo app on your phone
- Rust 1.98 (pinned in `app/rust-toolchain.toml`; `rustup` installs it)
- Go 1.27+ for the network fingerprint helper (optional, see below)
- System packages — on Debian/Ubuntu:

  ```bash
  sudo apt install build-essential pkg-config libwebkit2gtk-4.1-dev libxkbcommon-x11-dev libwayland-dev \
      libfontconfig1-dev libvulkan1 7zip
  ```

  Fedora: `webkit2gtk4.1-devel libxkbcommon-x11-devel wayland-devel fontconfig-devel vulkan-loader 7zip`
- Optional, for videos and voice messages: GStreamer 1.x with `gst-plugins-good`, and `gst-libav` for H.264/AAC.
  Without a decoder the player says so and offers to open the file in another app.

## Build & run

```bash
cd linux-port
fingerprint-helper/build.sh        # → fingerprint-helper/build/olaz-fingerprint-helper (needs Go; optional)
cd app
cargo build --release
./target/release/olaz              # first launch downloads the Zalo runtime, then shows the login QR
```

`olaz` finds `olaz-webhost` and `olaz-fingerprint-helper` next to itself (a development build also finds the
helper in `fingerprint-helper/build/`). Data lives in the usual XDG folders:

| What | Where |
|---|---|
| account data, Zalo runtime, databases | `~/.local/share/olaz/` |
| settings | `~/.config/olaz/settings.json` |
| media cache | `~/.cache/olaz/` |
| logs | `~/.local/state/olaz/logs/engine.log` |

Environment variables:

| Variable | Effect |
|---|---|
| `OLAZ_HOME=/some/dir` | keep everything under one folder (a separate profile) |
| `OLAZ_STORAGE=idb\|sqlite` | force the database backend for this run |
| `OLAZ_FINGERPRINT_HELPER=<path>\|off` | use another helper binary, or none |

Run the tests with `cargo test --workspace` in `app/`.

## Packages

**AppImage** (uses the system's WebKitGTK, GStreamer and 7-Zip):

```bash
cd linux-port/app && cargo build --release --locked -p olaz -p olaz-webhost
cd .. && fingerprint-helper/build.sh && packaging/appimage/build.sh     # → dist/Olaz-x86_64.AppImage
```

**Flatpak** (GNOME 51 runtime; builds offline from `packaging/flatpak/cargo-sources.json` and the helper's
vendored Go modules):

```bash
flatpak install --user flathub org.flatpak.Builder org.gnome.Sdk//51 \
    org.freedesktop.Sdk.Extension.rust-stable//26.08 org.freedesktop.Sdk.Extension.golang//26.08
cd linux-port
packaging/flatpak/gen-cargo-sources.sh      # after Cargo.lock changes
fingerprint-helper/build.sh --vendor        # Go modules for the offline helper build
flatpak run org.flatpak.Builder --user --install --force-clean build-flatpak packaging/flatpak/app.olaz.linux.yml
flatpak run app.olaz.linux
```

If `flatpak-builder` fails with "Failure spawning rofiles-fuse", add `--disable-rofiles-fuse`. The app id is
`app.olaz.linux`; H.264/AAC in the Flatpak come from `org.freedesktop.Platform.codecs-extra`.

**Install into a prefix** (what both packages use): `packaging/install.sh <prefix> [target-dir]`.

## How it works

```
┌────────────────────┐  IPC (length-prefixed JSON)  ┌───────────────────────────────┐
│ olaz (GPUI)        │ ◀──────────────────────────▶ │ olaz-webhost (WebKitGTK 4.1)  │
│  UI + engine       │                              │  official Zalo renderer,      │
│  main process      │ ◀─ bridge, network, storage ─│  hidden views + host scripts  │
└─────────┬──────────┘                              └───────────────────────────────┘
          │ unix socket
┌─────────▼──────────────────┐
│ olaz-fingerprint-helper    │  HTTPS / WSS to Zalo with Chromium 108's TLS + HTTP/2
└────────────────────────────┘
```

- **Official renderer, native UI.** `olaz-webhost` loads the official `app.asar` pages (login, main, the hidden
  shared worker) in WebKitGTK views that are never shown. Host scripts injected into those pages
  (`shared/host-scripts/`, the same files as the macOS app) read the official client's Redux store and modules
  and post compact snapshots; `olaz-engine` turns them into the state the GPUI interface renders. Actions (send,
  react, pin…) call the official client's own functions, so the wire protocol is always Zalo's.
- **Electron emulation.** The pages expect Electron's preload globals (`$znode`, `$zelectron`, `$zdb`, …) and a
  main process behind IPC. `olaz-host` provides the bridge (the members of `shared/resources/bridge-table.json`)
  and `olaz-main` answers the main-process channels: electron-store files, secrets (Secret Service), file
  system, crypto, downloads, notifications, the shared-worker router.
- **Network.** The renderer's XHR / fetch / WebSocket traffic is routed to Rust (`olaz-host::net`) with a
  cookie jar shared with media loading. Every request carries the official client's identity: Chromium 108's
  client-hint and `sec-fetch-*` headers, its multipart boundary, the runtime's own client version, the real host
  name and network interfaces. Connections go through `olaz-fingerprint-helper` (Go, `tls-client`'s Chrome 108
  profile — the macOS app's helper), so the TLS ClientHello and HTTP/2 settings match the official Electron
  22 client; without the helper Olaz uses `reqwest` / `tokio-tungstenite` over rustls.
- **Storage.** Zalo PC 26.10 keeps its databases in SQLite behind a data access layer that talks to an Electron
  utility process over a MessagePort. `sqlite-host.js` plays that process inside each page and `olaz-main`'s
  SQLite host (rusqlite, node-sqlite3 semantics, one thread per database) executes the statements. Fresh
  installs start on SQLite; installs that already hold IndexedDB data stay there until Settings ▸ Tài khoản ▸
  Engine ▸ "Chuyển sang SQLite…" (logs out; history returns with the phone → PC sync).
- **Media.** `olaz-media` loads CDN images and files (memory + disk cache, end-to-end-encrypted streams),
  manages downloads and plays video / voice with GStreamer.
- **Runtime.** `olaz-runtime` downloads and verifies the pinned Zalo PC release; `olaz-core` reads the asar and
  serves it through the `zalo-app://` scheme, patching the few spots a non-Electron host needs.

| Crate / folder | Role |
|---|---|
| `app/crates/olaz` | the executable: wires engine, webhost and UI |
| `app/crates/olaz-ui` | GPUI interface (shell, chat, sheets, settings, player, …) |
| `app/crates/olaz-engine` | `ZaloEngine`: view state built from the official client's snapshots; no GPUI |
| `app/crates/olaz-host` | webhost process, the `$z*` bridge, network proxy, fingerprint-helper client |
| `app/crates/olaz-webhost` | separate process hosting the official renderer in hidden WebKitGTK views |
| `app/crates/olaz-main` | Electron main-process emulation: IPC channels, stores, secrets, SQLite host |
| `app/crates/olaz-media` | CDN media loader, downloads, GStreamer player |
| `app/crates/olaz-runtime` | downloads and installs the official Zalo runtime |
| `app/crates/olaz-core` | asar reader, scheme handler, bridge values, crypto, cookies, client fingerprint |
| `app/crates/olaz-ipc` | the webhost wire protocol |
| `app/vendor/` | patched crates: `zed-xim` (X11 input methods), `gpui-component` (rounded window frame), `gpui-pre-wgpu` (color emoji fallback) |
| `fingerprint-helper/` | the Go TLS / HTTP-2 fingerprint helper |
| `shared/` | host scripts, bridge table and UI assets shared with the macOS app |
| `packaging/` | desktop file, metainfo, icons, AppImage and Flatpak recipes |

**Technologies:** Rust 2024 · GPUI + gpui-component (wgpu, Vulkan) · WebKitGTK 4.1 · tokio · reqwest / rustls ·
tokio-tungstenite · rusqlite · GStreamer · Secret Service (oo7) · Go (`bogdanfinn/tls-client`, utls) ·
Flatpak / AppImage.

## License

Same as the repository (see `../LICENSE`). Bundled fonts (Inter, Nunito, Noto Color Emoji) are under the SIL Open
Font License; icons are from [Phosphor](https://phosphoricons.com) (MIT).
