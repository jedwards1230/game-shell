# TV Shell

Quickshell + Hyprland couch gaming shell for Moonlight streaming.

A minimal 10-foot UI for a dedicated streaming box — controller-navigable home screen, exclusive gamepad input, auto-reconnect, and AV control.

## Install

```bash
sudo ./scripts/install-deps.sh   # Hyprland, Quickshell, Rust, build libs
sudo ./scripts/install.sh        # build the daemon + lay down the install tree
```

Then pick **TV Shell (Wayland)** in your display manager. Full walkthrough,
config, and prefix options in [docs/INSTALL.md](docs/INSTALL.md).

## Architecture

```
SDDM → tv-shell-session.sh → Hyprland (kiosk) → Quickshell (shell.qml)
       │                                  │  ▲ intent:* control-surface + Keys
       │  Super* → super-intent.sh ───────┘  │ (keyboard → intents: menu/home/reset)
                               └── tv-shell-input (Rust daemon: EVIOCGRAB the
                                   gamepad fleet → per-player uinput; backend IPC)
```

- **Quickshell** renders the UI via QML on Hyprland's Wayland compositor
- **tv-shell-input** (Rust daemon, `daemon/`) grabs the gamepad exclusively, emits keyboard/mouse via uinput, and serves the backend IPC (settings, app discovery, Bluetooth/network/power, Hyprland, Sunshine). It is the sole input/backend daemon, supervised as a `systemd --user` unit (journald logging, with a bare-process fallback)
- **tv-shell-panel** (Rust binary, `panel/`) — an optional LAN-only web control panel (axum + htmx), installed and enabled by default on `127.0.0.1:8091`. Gives a browser-based recovery path (dashboard, settings, tools, logs, dev deploy/restart, system updates) even when the daemon is wedged. See [docs/PANEL.md](docs/PANEL.md)
- **Moonlight** streams games from a Sunshine host
- **end-game-session** script handles session teardown (deployed separately via Ansible); HDMI-CEC is owned by the daemon's `cec-*` IPC (`--features cec`)

## Structure

```
shell/                   # QML shell — Quickshell config root (-c tv-shell)
  shell.qml              # Entry point — state machine + process management
  components/            # QML UI components
    HomeScreen.qml       # Target grid with controller navigation
    StreamCard.qml       # Individual streaming target card
    QuickActions.qml     # Top-right quick actions (volume, network, theme, power)
    StreamOverlay.qml    # Streaming/reconnecting overlay
    SettingsPanel.qml    # Left sidebar + right content loader (~12 pages)
    SettingsButton.qml   # Focusable button widget
    Theme.qml            # Colors, fonts, layout constants
daemon/                  # Rust backend daemon (tv-shell-input) — sole backend
  src/                   # input/uinput, IPC, config, apps, bluetooth, network, power, hyprland, health
panel/                   # Rust web control panel (tv-shell-panel) — LAN-only, axum + htmx (see docs/PANEL.md)
config/
  hyprland.conf          # Kiosk compositor config (sources hyprland-local.conf)
  tv-shell.desktop     # Wayland session entry (installer rewrites Exec to prefix)
  targets.json.example   # Copy-runnable streaming targets (single-line JSON)
  targets.yaml.example   # Annotated streaming-target field reference (docs only)
  config.toml.example    # Per-machine daemon options (HTTP/MCP, CEC, Plex, Steam)
  hyprland.conf.example  # Per-machine display/HDR override example
packaging/               # PKGBUILD / install layout (see #147)
scripts/
  install.sh             # Build daemon + install tree + register session
  install-deps.sh        # Install system dependencies (distro-aware)
  tv-shell-session.sh  # Session wrapper (starts input daemon + Hyprland)
```

## Configuration

Per-machine config lives under `~/.config/tv-shell/` and is seeded from the
`config/*.example` files on install — see [docs/INSTALL.md](docs/INSTALL.md#3-configure).
Streaming targets are stored in `targets.json` as single-line JSON (managed in-UI
via MoonlightSettings; `config/targets.yaml.example` documents the fields). Daemon
options (LAN bridge, CEC, Plex/Steam widgets) and the web panel's bind address go
in the optional typed `config.toml` (`config/config.toml.example`, `[panel]`
section); the shared bearer token lives in a separate `0600` file referenced by
`[http] token_file`. Optional MQTT state publishing + Home Assistant discovery
(daemon `[mqtt]`, sidecar `TV_SHELL_MQTT_*`) is documented in
[docs/MQTT.md](docs/MQTT.md).

## Observability

The daemon logs to the **systemd journal** when run as a `systemd --user` unit
(`journalctl --user -u tv-shell-input`; quickshell output under `-t
tv-shell-quickshell`), falling back to stderr otherwise. It also exports
**Prometheus metrics** — an auth-exempt `GET /metrics` on the optional LAN bridge
and/or a node_exporter textfile (`[observability].metrics_textfile` in
`config.toml`). See
[docs/OBSERVABILITY.md](docs/OBSERVABILITY.md) and
[docs/SYSTEMD_SETUP.md](docs/SYSTEMD_SETUP.md).

## Controls

| Button | Action |
|--------|--------|
| D-pad | Navigate |
| A | Select / Launch |
| B | Back |
| Y | Actions (context menu) |
| Home + B (3s) | End session (AV shutdown) |

## Requirements

- Hyprland
- Quickshell
- Rust toolchain (to build `tv-shell-input`)
- Moonlight Qt (built from source)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for build, test, lint, and PR instructions.

## License

TV Shell is free software licensed under the **GNU General Public License v3.0
(GPL-3.0)**. See [LICENSE](LICENSE) for the full terms.
