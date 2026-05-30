# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

A 10-foot couch gaming UI built with [Quickshell](https://quickshell.org/) (QML) on Hyprland. Controller-navigable home screen for Moonlight game streaming, local app launching, and system settings. Designed for 4K@120Hz HDR on OLED displays.

## Architecture

```
SDDM → game-shell-session.sh → Hyprland (kiosk) → Quickshell (shell.qml)
                               └── game-shell-input (Rust daemon: EVIOCGRAB → uinput
                                   + backend IPC; falls back to gamepad-input.py)
```

- **shell.qml** — entry point: state machine (`idle` → `launching` → `streaming` → `reconnecting`) and process management
- **game-shell-input** (Rust daemon, `rust/`) — the primary backend: grabs the gamepad exclusively via evdev, emits keyboard/mouse via uinput, and serves the full Unix-socket IPC (input `grab`/`release`/`subscribe`, settings, app discovery, Bluetooth/network/power, Hyprland reads, Sunshine). `input/gamepad-input.py` is the input-only rollback the session script falls back to (build/install the binary to switch; remove/rename it to roll back)
- **Theme.qml** — singleton (must be `Item`, not `QtObject` — Quickshell can't host Process/Timer children in QtObject) with all colors, fonts, and layout constants. Dark/light/auto mode state is read from `SettingsStore`
- **SettingsStore.qml** — singleton (also `Item`, for the same reason) that owns all QML-side settings I/O for `~/.config/game-shell/settings.json` and the binding IPC (get/set/capture). Single source of truth for the settings schema
- **components/qmldir** — component registry. New components must be added here or Quickshell won't find them

### File Layout

```
shell.qml                    # Entry point, state machine
components/
  Theme.qml                  # Singleton — colors, fonts, layout constants
  SettingsStore.qml          # Singleton — centralized settings I/O + binding IPC
  HomeScreen.qml              # Hero clock, app rows, status icons
  AppCard.qml                 # Icon-centric app tile (Freedesktop icons)
  StreamCard.qml              # Moonlight streaming target card
  StatusIcons.qml             # Top-right floating icons (volume, network, theme, settings)
  SettingsPanel.qml           # Left sidebar + right content loader
  {Audio,Bluetooth,Network,Display,Power}Settings.qml
  MoonlightSettings.qml       # Server management (add/remove/configure)
  AppearanceSettings.qml      # Theme mode selector (auto/light/dark)
  SettingsButton.qml           # Reusable button component
  MarqueeText.qml              # Scrolling text for long names
  Drawer.qml                    # Reusable slide-in drawer (any edge)
  NavigationDrawer.qml           # Left nav drawer (Home, Settings)
  StreamOverlay.qml            # Reconnecting/error overlay
  qmldir                       # Component registry
config/
  hyprland.conf               # Monitor config (resolution, refresh, HDR, VRR)
  palette.md                  # Color palette documentation
  game-shell.desktop          # SDDM session file
  targets.yaml.example        # Example streaming targets (docs only)
rust/                        # Rust backend daemon (game-shell-input) — primary
  src/                       # input/uinput, ipc, config, apps, bluetooth, network, power, hyprland, health
  README.md                  # daemon architecture + phase notes
input/
  gamepad-input.py            # Input-only Python daemon — rollback fallback
  requirements.txt            # Python deps (evdev)
scripts/
  game-shell-session.sh       # Session wrapper launched by SDDM
```

## Key Data Flows

- **Streaming targets**: Loaded from `/opt/game-shell/targets.json` at startup (single-line JSON — see gotchas). Managed in-UI via MoonlightSettings. Optional `sunshineUser`/`sunshinePass`/`sunshinePort` fields enable pre-flight session detection via the Sunshine API — when present, the shell checks for active sessions before streaming and offers Resume/Quit/Cancel if a different app is running. Credentials should be injected by the deployment system, not committed.
- **Settings persistence**: `~/.config/game-shell/settings.json` stores `themeMode`, `streamingViewMode`, `controllerDebug` (QML-owned) and `keyBindings` (daemon-owned). The **daemon is the sole writer** — `SettingsStore` reads via `get-config` and hands QML-owned keys to `set-config` (read-modify-write), so QML never formats config JSON itself. All QML-side I/O is centralized in the `SettingsStore` singleton — add new settings there (a property + load/save handling), not in Theme.qml. Theme delegates to SettingsStore.
- **App discovery & recents**: `AppDiscoveryManager` (apps via `list-apps`) and `RecentsTracker` (`get-recents` / `record-launch`) read JSON straight from the daemon, which owns the `.desktop` scanning (`freedesktop-desktop-entry` crate) and recents file. QML no longer parses `.desktop` files. The QML side still uses a thin `python3` one-liner only as a Unix-socket client.
- **Input daemon IPC**: See [docs/IPC_PROTOCOL.md](docs/IPC_PROTOCOL.md) for the full protocol specification. QML sends commands via Unix socket; the daemon streams events to subscribers.
- **Settings panels**: SettingsPanel uses a Loader to swap between section components. Each section manages its own system calls via `Quickshell.Io.Process`.

## System Integration

| Tool | Used For |
|------|----------|
| input daemon (IPC) | Gamepad grab/release, settings I/O (`get/set-config`), app discovery (`list-apps`), recents (`get/record`), Bluetooth (`bt-*`), network reads (`net-*`), suspend/battery (`power-*`), Hyprland reads (`hypr-active`/`hypr-clients` + `hypr:*` events), Sunshine pre-flight (`sunshine-status`) — see [docs/IPC_PROTOCOL.md](docs/IPC_PROTOCOL.md) |
| `wpctl` | Audio volume/mute/sink switching (WirePlumber/PipeWire) |
| `nmcli` | WiFi *join* only (`device wifi connect`) — reads go through the daemon's `net-*` IPC |
| `hyprctl` | Monitor mode/scale changes, app launching, reload, one-shot `dispatch` actions — window/client *reads* go through the daemon's `hypr-*` IPC |
| `systemctl` | Reboot/poweroff one-shots — suspend goes through the daemon's `power-suspend` IPC |
| `cec-client` | HDMI-CEC TV/AVR control (`AVController.qml`) — **deferred**, still a shell-out (follow-up) |
| `moonlight` | Game streaming client (`stream`, `list`, `pair`) |

The daemon's `bt-*` (BlueZ/`bluer`), `net-*` (NetworkManager/`zbus`, read-only),
and `power-*` (logind + UPower/`zbus`) IPC commands are the **D-Bus backbone**
(Phase 3, #28). **Phase 4** adds Hyprland reads (`hypr-active`/`hypr-clients` +
`hypr:activewindow`/`hypr:fullscreen` events via direct Hyprland IPC sockets,
replacing the `hyprctl clients -j` shell-out and feeding `AppLifecycleManager.qml`) and
Sunshine session detection (`sunshine-status <host> <port>` via `reqwest`/rustls
against Sunshine's self-signed `/serverinfo`, replacing the inline HTTP polls in
`StreamManager.qml`/`StreamCard.qml`). These replace the
QML shell-outs/HTTP polls that *read* system state. The Linux-only modules
(D-Bus, Hyprland) are unverifiable on macOS/CI and need on-device verification on
game-client-1; `sunshine-status` runs cross-platform but its live fetch needs a
reachable host (its response parser is pure and unit-tested). What deliberately
stays a shell-out: Wi-Fi **join** (`nmcli`), audio (`wpctl`), one-shot compositor
*actions* (`hyprctl dispatch`), reboot/poweroff (`systemctl`), and **HDMI-CEC**
(`cec-client` in `AVController.qml`, deferred as a follow-up).

## Development

No build step — QML is interpreted by Quickshell at runtime. Deploy by syncing files to the target machine's install directory and restarting Quickshell.

### Deploy Cycle

```bash
# 1. Push changes to git
git push origin <branch>

# 2. Pull on target device
ssh <device> "cd /opt/game-shell && git pull"

# 3. Restart Quickshell (find Hyprland signature first)
SIG=$(ls /run/user/1000/hypr/ | tail -1)
WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR=/run/user/1000 \
  HYPRLAND_INSTANCE_SIGNATURE=$SIG hyprctl dispatch exec 'killall quickshell'
sleep 1
WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR=/run/user/1000 \
  HYPRLAND_INSTANCE_SIGNATURE=$SIG quickshell -c game-shell &

# 4. Screenshot for verification
WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR=/run/user/1000 grim /tmp/screenshot.png
```

### Python Input Daemon

```bash
pip install evdev  # or: pip install -r input/requirements.txt
```

Requires Linux with evdev and uinput access. Auto-discovers gamepad by vendor/product ID (defaults: Xbox controller `045e:028e`, configurable via `GAMEPAD_VENDOR`/`GAMEPAD_PRODUCT` env vars).

### QA Screenshots

Catalog of views/overlays/states to capture for visual QA (and how to reach each):

@docs/qa-screenshot-views.md

## Design Constraints

- **10-foot UI at 4K**: All font sizes and layout constants in Theme.qml are sized for couch-distance reading. Don't shrink them.
- **Controller-first navigation**: Every interactive element must be reachable via D-pad (arrow keys) and activatable with A (Enter). B (Escape) always goes back. Focus management is critical — use `KeyNavigation` chains and `Keys.on*Pressed` handlers.
- **Palette rules**: See `config/palette.md`. Never use gold for text. Crimson for focus/active states. Ember for secondary interactive elements. All overlay backdrops use `Qt.rgba(0, 0, 0, 0.7-0.85)`.
- **No build tooling**: No bundler, no compiler, no package manager for QML. Files are deployed as-is.
- **Distribution agnostic**: This repo has no knowledge of specific infrastructure, deployment tools, or host management. It's a standalone QML shell that runs on any Linux system with Hyprland + Quickshell.

## Gotchas

- **SplitParser reads line-by-line**: Any JSON loaded via `cat` + `SplitParser` must be single-line. Never pretty-print `targets.json` or `settings.json`.
- **Theme.qml is an Item, not QtObject**: Quickshell 0.3.0 can't host Process/Timer children inside QtObject. The singleton uses Item as its root type.
- **`image://icon/` for Freedesktop icons**: Use `Image { source: "image://icon/" + iconName }` to load icons from the system theme. Falls back to nothing if the icon doesn't exist — provide a letter-initial fallback.
- **qmldir must list new components**: Quickshell won't auto-discover them. Add a line like `MyComponent 1.0 MyComponent.qml`.
- **WAYLAND_DISPLAY may vary**: Usually `wayland-1` but try `wayland-0` if grim/hyprctl fails.
- **Hyprland instance signature**: Multiple instances may exist in `/run/user/1000/hypr/`; use `tail -1` for the latest.
- **Theme property renames cascade**: `Theme.text` → `Theme.textPrimary` will also hit `Theme.textDim` producing `Theme.textPrimaryDim`. Replace longest matches first.

## CI

- **QML formatting**: `qmlformat` (Qt 6.8) enforced via GitHub Actions. On PRs, unformatted files are auto-fixed and pushed. On main, unformatted files fail the check.
