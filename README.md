# Game Shell

Quickshell + Hyprland couch gaming shell for Moonlight streaming.

A minimal 10-foot UI for a dedicated streaming box — controller-navigable home screen, exclusive gamepad input, auto-reconnect, and AV control.

## Architecture

```
SDDM → game-shell-session.sh → Hyprland (kiosk) → Quickshell (shell.qml)
                               └── game-shell-input (Rust daemon: EVIOCGRAB → uinput
                                   + backend IPC)
```

- **Quickshell** renders the UI via QML on Hyprland's Wayland compositor
- **game-shell-input** (Rust daemon, `rust/`) grabs the gamepad exclusively, emits keyboard/mouse via uinput, and serves the backend IPC (settings, app discovery, Bluetooth/network/power, Hyprland, Sunshine). It is the sole input/backend daemon
- **Moonlight** streams games from a Sunshine host
- **living-room-cec** and **end-game-session** scripts handle AV control (deployed separately via Ansible)

## Structure

```
shell.qml                # Entry point — state machine + process management
components/              # QML UI components
  HomeScreen.qml         # Target grid with controller navigation
  StreamCard.qml         # Individual streaming target card
  StatusBar.qml          # Clock, IP, state indicator
  StreamOverlay.qml      # Streaming/reconnecting overlay
  SettingsPanel.qml      # Audio + power controls
  SettingsButton.qml     # Focusable button widget
  Theme.qml              # Colors, fonts, layout constants
rust/                    # Rust backend daemon (game-shell-input) — sole backend
  src/                   # input/uinput, IPC, config, apps, bluetooth, network, power, hyprland, health
config/
  hyprland.conf          # Kiosk compositor config
  game-shell.desktop     # SDDM wayland session entry
  targets.yaml.example   # Example streaming targets
scripts/
  game-shell-session.sh  # Session wrapper (starts input daemon + Hyprland)
```

## Configuration

Streaming targets are defined in `targets.yaml`:

```yaml
targets:
  - name: Desktop
    host: 192.168.8.10
    app: Desktop
    resolution: 3840x2160
    fps: 120
    codec: HEVC
    hdr: true
```

## Controls

| Button | Action |
|--------|--------|
| D-pad | Navigate |
| A | Select / Launch |
| B | Back / Settings |
| Y | Tab (future) |
| Home + B (3s) | End session (AV shutdown) |

## Requirements

- Hyprland
- Quickshell
- Rust toolchain (to build `game-shell-input`)
- Moonlight Qt (built from source)

## License

Game Shell is free software licensed under the **GNU General Public License v3.0
(GPL-3.0)**. See [LICENSE](LICENSE) for the full terms.
