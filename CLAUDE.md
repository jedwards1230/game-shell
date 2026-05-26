# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

A 10-foot couch gaming UI built with [Quickshell](https://quickshell.org/) (QML) on Hyprland. Controller-navigable home screen for Moonlight game streaming, local app launching, and system settings. Designed for 4K@120Hz HDR on OLED displays.

## Architecture

```
SDDM → game-shell-session.sh → Hyprland (kiosk) → Quickshell (shell.qml)
                               └── gamepad-input.py (EVIOCGRAB → uinput)
```

- **shell.qml** — entry point: state machine (`idle` → `launching` → `streaming` → `reconnecting`) and process management
- **gamepad-input.py** — async daemon that grabs the gamepad exclusively via evdev, emits keyboard events via uinput, and listens on a Unix socket for `grab`/`release`/`subscribe` commands
- **Theme.qml** — singleton (must be `Item`, not `QtObject` — Quickshell can't host Process/Timer children in QtObject) with all colors, fonts, and layout constants. Dark/light/auto modes persisted to `~/.config/game-shell/settings.json`
- **components/qmldir** — component registry. New components must be added here or Quickshell won't find them

### File Layout

```
shell.qml                    # Entry point, state machine
components/
  Theme.qml                  # Singleton — colors, fonts, layout constants
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
  NavigationDrawer.qml           # Left nav drawer (Home, Settings, Force Quit, toggles)
  StreamOverlay.qml            # Reconnecting/error overlay
  qmldir                       # Component registry
config/
  hyprland.conf               # Monitor config (resolution, refresh, HDR, VRR)
  palette.md                  # Color palette documentation
  game-shell.desktop          # SDDM session file
  targets.yaml.example        # Example streaming targets (docs only)
input/
  gamepad-input.py            # Gamepad daemon
  requirements.txt            # Python deps (evdev)
scripts/
  game-shell-session.sh       # Session wrapper launched by SDDM
```

## Key Data Flows

- **Streaming targets**: Loaded from `/opt/game-shell/targets.json` at startup (single-line JSON — see gotchas). Managed in-UI via MoonlightSettings.
- **Settings persistence**: `~/.config/game-shell/settings.json` stores `themeMode` and `moonlightViewMode`. Add new fields to both `loadSettings` and `saveSettings` in Theme.qml.
- **Input daemon IPC**: QML sends `grab`/`release` via Unix socket (`/run/user/$UID/game-shell-input.sock`). Released during streaming so Moonlight gets raw input.
- **Settings panels**: SettingsPanel uses a Loader to swap between section components. Each section manages its own system calls via `Quickshell.Io.Process`.

## System Integration

| Tool | Used For |
|------|----------|
| `wpctl` | Audio volume/mute/sink switching (WirePlumber/PipeWire) |
| `bluetoothctl` | Bluetooth device scanning and pairing |
| `nmcli` | Network connection management |
| `hyprctl` | Monitor mode/scale changes, app launching, reload |
| `systemctl` | Power management (suspend/reboot/poweroff) |
| `moonlight` | Game streaming client (`stream`, `list`, `pair`) |

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
