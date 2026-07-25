# tv-shell-input (Rust)

A Rust backend daemon replacing `input/gamepad-input.py` and the QML shell's
inline `python3`/shell-out parsers. Same Unix socket + newline-delimited wire
protocol (`docs/IPC_PROTOCOL.md`). Phases 1–4 of
[#28](https://github.com/jedwards1230/tv-shell/issues/28) (HDMI-CEC deployed
and verified on gaming-client via `cec.rs`/cec-rs with static-linked libcec).

The QML shell depends on this daemon for the Settings / app-discovery / system
pages as well as input; it is the sole backend (the Python
`gamepad-input.py` and its rollback were retired in #96). See
`scripts/tv-shell-session.sh`, which spawns the binary directly.

## What it does

Grabs a gamepad via `EVIOCGRAB`, emits keyboard/mouse through uinput, and serves
the `grab`/`release`/`status`/`subscribe`/`get-bindings`/`set-binding`/
`capture-next`/`capture-cancel`/`kbd-log` protocol. Discovers an **arbitrary**
controller via the SDL GUID + bundled `SDL_GameControllerDB` (not just the
hardcoded Xbox pad), falling back to any `BTN_SOUTH` device.

Phase 2 added stateless commands that move parsing/serialization out of the QML
shell's inline `python3` one-liners and into the daemon:

- `list-apps` — scans XDG `.desktop` entries via the cross-platform
  `freedesktop-desktop-entry` crate, returns a compact JSON array.
- `get-config` / `set-config` — the daemon is the sole writer of
  `settings.json` (read-modify-write, compact JSON).
- `record-launch` / `get-recents` — maintains the recents file.

The QML side opens the socket natively via `Quickshell.Io.Socket`
(`components/SocketClient.qml`, #97) and no longer parses `.desktop` files or
hand-formats config JSON; the old per-call `python3 -c` socket shims are gone.

## Layout

| File | Role |
|------|------|
| `protocol.rs` | Command parse + Event/response wire strings (bare text) |
| `config.rs` | Kernel codes, name tables, bindings, `settings.json` I/O |
| `apps.rs` | `.desktop` scan/parse → `list-apps` JSON (cross-platform) |
| `recents.rs` | Recents file I/O → `record-launch` / `get-recents` (cross-platform) |
| `device.rs` | SDL GUID + DB matching, fleet discovery, fd-ownership registry, stable wire ids, player-slot allocator |
| `controllerdb.rs` | Runtime fetch + cache of the upstream SDL_GameControllerDB (`controllerdb-refresh` IPC) |
| `state.rs` | Control messages + pure input logic (velocity, deadzone, combos) |
| `input.rs` | Linux input runtime (evdev/uinput) — single state owner; multi-pad `Fleet` |
| `bluetooth.rs` | **Linux-only.** BlueZ actor via `bluer` — scan/pair/connect/trust + `bt:*` events |
| `network.rs` | **Linux-only.** NetworkManager **read** actor via `zbus` — connectivity / AP list / `net:*` events |
| `power.rs` | **Linux-only.** logind suspend + UPower battery via `zbus` — `power:*` events |
| `hyprland.rs` | **Linux-only.** Hyprland actor over direct IPC sockets (no crate) — active-window/clients queries + `hypr:*` events |
| `cec.rs` | **Linux-only.** HDMI-CEC actor via `cec-rs`/libcec — `cec-scan`/`cec-device`/`cec-power-on`/`cec-power-off`/`cec-active-source` + `cec:*` events |
| `session.rs` | **Linux-only.** logind session-active watcher — releases/reacquires gamepad grab on VT-switch |
| `watch.rs` | **Linux-only.** inotify watcher for `settings.json` external edits (triggers `config:changed` broadcast) |
| `health.rs` | Sunshine session detection via `reqwest`/rustls (cross-platform) — `sunshine-status` |
| `moonlight.rs` | Moonlight local-config "forget" — creds-free client-side unpair (`moonlight-forget` IPC) |
| `notifications.rs` | Notification history persistence (`record-notification` / `get-notifications` / `set-notifications` IPC) |
| `plex.rs` | Plex hubs fetch for the home-screen Plex widget — `plex-hubs` IPC (cross-platform, stateless) |
| `service_health.rs` | Shared remote-service reachability vocabulary (`ServiceStatus`) + HTTP probe + background `health:*` poller (cross-platform) |
| `sidecar.rs` | Reusable remote-widget-**sidecar** HTTP client: base URL + bearer, reachability probe, typed + size-capped request helpers. The daemon is an HTTP *client* to a sidecar on another machine — **not** a process supervisor (no spawn/health-restart). Cross-platform; `steam` is the first consumer |
| `steam.rs` | Steam library/launch proxy to the `tv-shell-host` sidecar **over HTTP** (`steam-library`/`steam-launch`/`steam-bigpicture`/`steam-quit`/`steam-suspend` IPC). Deserializes host responses through the shared `tv-shell-protocol` types. Cross-platform, stateless |
| `wol.rs` | Wake-on-LAN magic-packet sender (`wol <host>` IPC) + the opt-in proactive wake of the ACTIVE steam host (`[steam].wake_active_host_on_start`, default off). MAC precedence: configured `[[steam.hosts]] mac` → `ip neigh` → persisted cache. Cross-platform |
| `system.rs` | System/storage status reads for the System settings page — `sys-status` / `storage-status` IPC |
| `session_env.rs` | Session-environment self-discovery (resolves `WAYLAND_DISPLAY`, `HYPRLAND_INSTANCE_SIGNATURE`, install root) |
| `daemon_config.rs` | Typed `~/.config/tv-shell/config.toml` parse + startup `validate()` — the single per-machine config source |
| `bridge_core.rs` | Shared action logic for the HTTP bridge and MCP server (intent dispatch, screenshot, status, log read) |
| `http.rs` | LAN HTTP/1.1 control bridge (`[http].bind`) — `POST /intent`, `POST /key`, `GET /screenshot`, `/dev/*` |
| `mcp.rs` | MCP server (`[mcp].bind`, `--features mcp`) — 14 tools over streamable-HTTP at `/mcp` |
| `ipc.rs` | Unix-socket server, `broadcast` event fan-out, D-Bus command routing |
| `main.rs` | Runtime wiring + signals + D-Bus actor spawn |

`apps.rs`, `recents.rs`, `health.rs`'s response parser, `steam.rs`'s
`normalize_library`, and `sidecar.rs`'s `from_parts` are pure Rust — fully
unit-tested on macOS.

### Widget sidecars (remote HTTP backends)

A home-screen widget that needs heavy backend logic ships a **sidecar process**.
`tv-shell-host` (the Steam library/launch backend) is the first and only one
today. The sidecar runs on a **different machine** (the gaming PC) and the daemon
reaches it **over HTTP on the LAN** with a bearer token — see
[`docs/HOST_SETUP.md`](../docs/HOST_SETUP.md). The daemon does **not** spawn,
health-restart, or otherwise supervise the sidecar's process lifecycle; "health"
is reachability (`GET /status`), not process management.

`sidecar.rs` owns the reusable client plumbing (base URL + bearer, reachability
probe, typed/size-capped request helpers) so a future widget backend reuses it
instead of re-deriving it. The daemon↔sidecar JSON contract is single-sourced in
the `tv-shell-protocol` crate (`LibraryResponse` / `StatusResponse` /
`LaunchRequest`): the host serializes those types, the daemon deserializes them,
so the wire shape can't silently drift.

## Gamepad fleet (input-unification Phase 4, #98/#101)

`input.rs` owns a `Fleet { pads: HashMap<RawFd, PadDevice>, slots: SlotAllocator }`.
Each physical pad is a `PadDevice` keyed by its event-stream fd, carrying its own
held buttons, stick calibration, per-pad timers, and a stable player slot. Shared
output resources (the virtual uinput keyboard/mouse, the broadcast bus, the remap
table, the capture state) live in a `Shared` struct borrowed by every per-pad
method, so there is still a single state owner and no `Arc<Mutex>` across `.await`.

- **Discovery** (`device::find_gamepads`) returns *all* DB-matched pads; the fleet
  dedups against pads it already holds by **device path** (an already-grabbed pad
  re-enumerates at the same path but a fresh fd, so fd-ownership alone can't dedup
  the physical pad). Foreign injectors (ydotoold) are rejected by the
  DB-match-or-reject gate; our own uinput devices are skipped by fd.
- **Hot-join/leave + stable slots (#101):** join → lowest-free slot
  (`SlotAllocator`) → grab → calibrate → `controller-wake` + `pad:connected`.
  Leave → free slot (reused in connection order) → drop virtual pad →
  `controller-disconnected` + `pad:disconnected`. P1 keeps slot 0 across a P2
  reconnect. `SlotAllocator` is pure and unit-tested.
- **Shared focus (shell mode):** combos / Home-hold are detected
  **per-pad-complete** — a single pad must hold the whole key set, so two pads
  each holding *half* a combo never trigger it. A fleet-level latch publishes
  `intent:home-hold` once even when two pads hold Home simultaneously. For a
  single connected pad this is byte-identical to the pre-fleet behavior.
- **Wire compat:** `status` reports the fleet aggregate (`connected` = any pad,
  `grabbed` = any pad) — identical to the old reply for one pad. `get-pads`
  exposes per-pad `{id,index,name,grabbed}` for the UI.

The per-pad `virtual_pad` / `battery` / `led_index` fields are reserved here and
wired by the Phase 5 game presenter and the Phase 5.5 rumble/battery/LED
ride-alongs.

## Phase 3 — D-Bus backbone (`bluetooth.rs` / `network.rs` / `power.rs`)

Each subsystem is a long-lived async actor on the IPC runtime owning a single
`bluer::Session` or `zbus::Connection` (single-owner; no `Arc<Mutex>` across
`.await`). They answer request/response query commands and stream `bt:*` /
`net:*` / `power:*` events onto the existing `subscribe` bus. This deletes the
QML shell-outs that *read* system state:

- **Bluetooth** (`bluer`/BlueZ): `bt-power-*`, `bt-scan-*`, `bt-list`,
  `bt-connect`/`bt-disconnect`/`bt-pair`/`bt-trust`.
- **Wi-Fi reads** (`zbus`/NetworkManager): `net-status`, `net-wifi-list`,
  `net-wifi-rescan`. Wi-Fi **join** stays an `nmcli` shell-out.
- **Power/idle** (`zbus`/logind + UPower): `power-can-suspend`, `power-suspend`,
  `power-battery` (graceful "no battery" on a desktop).

These three modules are `#[cfg(target_os = "linux")]` (D-Bus is Linux-only) —
they are excluded from the macOS build, so the rest of the crate still compiles
and unit-tests there. The protocol parsing/response builders for every Phase 3
command stay cross-platform and are unit-tested. **The D-Bus paths can only be
exercised on-device** (no D-Bus on macOS / CI); each actor degrades to `error:*`
replies if BlueZ/NetworkManager/logind/UPower is absent, never panicking the
daemon. See `docs/IPC_PROTOCOL.md` for the full command/event reference.

## Phase 4 — Hyprland + Sunshine (`hyprland.rs` / `health.rs`)

Two more subsystems replace the remaining QML *reads*:

- **Hyprland** (`hyprland.rs`): a `#[cfg(target_os = "linux")]` async actor (same
  single-owner shape as Phase 3) speaking Hyprland's IPC sockets directly (no
  crate — the `hyprland` 0.3.x crate hardcodes the legacy `/tmp/hypr` socket dir
  and can't reach Hyprland >= 0.40; see the module docs). It answers
  `hypr-active` (active window `{class,title,address}`), `hypr-clients`
  (`hyprctl clients -j`-equivalent `{class,title,address,workspace}` array), and
  `hypr-monitors` (monitor array incl. `currentFormat` + derived `hdr` bool) by
  sending `j/activewindow` / `j/clients` / `j/monitors` to the request socket,
  and streams `hypr:activewindow:<class>` / `hypr:fullscreen:<0|1>` events read
  from the `.socket2.sock` event stream. This replaces the `hyprctl clients -j`
  shell-out in `components/HyprctlClients.qml` and the `hyprctl monitors -j`
  READ in `DisplaySettings.qml`. User-triggered `hyprctl dispatch` *actions*
  stay in QML — except kiosk fullscreen enforcement (`force_fullscreen`): on
  every `openwindow` event the actor dispatches `focuswindow` + `fullscreen
  0 set` on that window's address, class-agnostic, because the static
  `windowrule = fullscreen` in `hyprland.conf` only applies if the new window
  also wins initial keyboard focus on the same map, which doesn't reliably
  happen on this kiosk when a second app opens.
- **Sunshine** (`health.rs`): the `sunshine-status <host> <port>` pre-flight check
  the shell runs before a Moonlight stream. It's **stateless and cross-platform**
  (a plain `ipc.rs` handler, not a Linux-only actor) over `reqwest` with
  `rustls-tls` + `danger_accept_invalid_certs` for Sunshine's self-signed HTTPS.
  The `/serverinfo` response **parser is a pure function unit-tested on macOS**;
  only the fetch needs the runtime. Returns
  `{online,paired,currentApp,httpsPort}`, replacing the inline Sunshine HTTP polls
  in `StreamManager.qml` / `StreamCard.qml`.

`hyprland.rs` is Linux-only (the Hyprland IPC socket); it's excluded from the
macOS build and verifiable only on-device. `health.rs` runs everywhere, but its
live fetch needs a reachable Sunshine host.

**HDMI-CEC lives in the daemon** (`cec.rs`, cec-rs/libcec, #94) — deployed and
verified on gaming-client with static-linked libcec (no system libcec dependency).
`AVController.qml` and `AVControlSettings.qml` use the daemon's `cec-*` IPC over
`SocketClient` (the `living-room-cec` shell-out chain is gone).

**CEC focus toggles:** opening the libcec connection no longer auto-claims the
active source (`activate_source(false)` in the builder). Daemon-start focus is
gated by `cecFocusOnStartup` (default `false`) and resume-from-sleep focus by
`cecFocusOnWake` (default `true`), both within the `[cec].lifecycle` master gate
(in `config.toml`). The manual `cec-active-source` IPC and standby-on-suspend are
unaffected.

## Network control surface (`http.rs` / `mcp.rs` / `bridge_core.rs`)

Beyond the owner-only Unix-socket IPC, the daemon can expose its
intent/key/screenshot/dev surface over the network. Two opt-in adapters share one
bearer token and a single action core (`bridge_core.rs`):

- **HTTP bridge** (`http.rs`, `[http].bind`): a hand-rolled HTTP/1.1
  listener — `POST /intent/<name>`, `POST /key/<name>`, `GET /screenshot`, and the
  `/dev/*` routes (status/logs/deploy/build/restart-shell/restart-daemon).
- **MCP server** (`mcp.rs`, `[mcp].bind`, `--features mcp`): the official
  `rmcp` 1.7.0 SDK over streamable-HTTP at `/mcp`, exposing 14 tools (the dev tools
  gated by `[mcp].dev`). Feature-gated only, not OS-gated — compiles on
  macOS.

Both are unset (closed) by default. Auth, endpoint/tool reference, env vars, and
security posture: **[`docs/CONTROL_SURFACE.md`](../docs/CONTROL_SURFACE.md)**.
`scripts/build-daemon.sh` defaults to `--features cec,mcp`.

## Build & test

The full binary only links on **Linux** (`evdev`/`uinput` are kernel
interfaces). `evdev` is a Linux-only dependency, so the portable modules still
compile and test on macOS:

```bash
cargo test            # runs everywhere (protocol/config/apps/recents/device/state/ipc)
cargo build --release # Linux only -> target/release/tv-shell-input
```

**Linux build deps:** the Phase 3 Bluetooth module uses `bluer`, which pulls in
`libdbus-sys`, so a Linux build needs the D-Bus headers + pkg-config:
`apt-get install libdbus-1-dev pkg-config` (Debian/CI) — on Arch/CachyOS
these come with the core `dbus`/`base-devel`. `zbus` (network/power) and
`reqwest`/`rustls-tls` (health) are pure Rust and need nothing; Hyprland IPC uses
raw Unix sockets (no crate, no system deps). The Phase 4 CEC module (`cec-rs`)
**static-links a bundled libcec** (the `cec` feature forwards `libcec-sys/static`,
#179): a prebuilt static libcec + p8-platform is fetched from
[ssalonen/libcec-static-builds](https://github.com/ssalonen/libcec-static-builds)
and linked into the binary, so there is **no system `libcec`/`libcec-dev`
dependency at build or runtime** (the host can manage/remove system libcec
freely). The static path needs no bindgen/cmake/clang — only `libudev-dev` +
`pkg-config` (for the libudev link hint) and network access at build time to
fetch the archive: `apt-get install libudev-dev pkg-config` (Debian/CI) — on
Arch/CachyOS these come with `systemd` / `base-devel`.

## Deploy

1. `cargo build --release`
2. Install `target/release/tv-shell-input` to `$TV_SHELL_DIR/bin/`
   (the install prefix — `/opt/tv-shell` is just a fallback default; the
   binary resolves its own root from `current_exe`. `TV_SHELL_INPUT_BIN`
   overrides the exact binary path for the re-exec / dev-override hook.)

`scripts/tv-shell-session.sh` spawns the binary directly — it is the sole
backend (the Python rollback was retired in #96).

Honors `TV_SHELL_DIR` (install root; also derived from `current_exe`),
`TV_SHELL_INPUT_BIN` (override the binary path used for the `/dev/restart-daemon`
re-exec), `TV_SHELL_SOCK`, `GAMEPAD_VENDOR`/`GAMEPAD_PRODUCT` (exact-pin
override), and `TV_SHELL_GAMECONTROLLERDB` (fuller controller DB).

## Logging & debugging

Logs go to the **systemd journal** when the daemon runs as a `systemd --user`
unit (or any journal-attached context — auto-detected via `JOURNAL_STREAM`,
overridable with `[observability].log_journal = true`/`false` in `config.toml`),
else to **stderr**. Either way they ride `tracing`, filtered by the `RUST_LOG`
env var (default `info`). Read a unit's logs with
`journalctl --user -u tv-shell-input`. Tiers:

| Level | Shows |
|-------|-------|
| `info` (default) | startup/actors, pad join/leave + grab, **presenter transitions** (Shell/Keyboard/Game/Handoff, incl. follow-focus by window class + virtual-pad create/destroy — the grab-handoff that's most bug-prone), warnings/errors |
| `debug` | every **published broadcast event** at the `publish()` chokepoint — `intent:*`, combos, `pad:*`, `input-mode:*`, `controller-wake`, status pushes |
| `trace` | every **raw evdev event** from each pad (slot + type/code/value) and every `emit_key`/`emit_mouse_button` to the shared virtual devices |

```bash
# Full input pipeline trace for one run (scoped to the input module):
RUST_LOG=tv_shell_input::input=trace "$TV_SHELL_DIR/bin/tv-shell-input"
# High-level event flow only (intents/combos/pad events), less noise:
RUST_LOG=tv_shell_input::input=debug ...
```

The live **`subscribe`** IPC stream is the other debug surface — it shows the
daemon's outbound events in real time without restarting:

```bash
printf 'subscribe\n' | socat - UNIX-CONNECT:"$TV_SHELL_SOCK"
```

Inject a control-surface intent by hand (e.g. to test the keyboard escape path):

```bash
printf 'intent home\n' | socat - UNIX-CONNECT:"$TV_SHELL_SOCK"   # -> ok
```

## Metrics

The daemon exports Prometheus/OpenMetrics two ways: an **auth-exempt `GET /metrics`**
on the HTTP bridge, and/or an atomically-written **node_exporter textfile**
(`[observability].metrics_textfile`; refresh `[observability].metrics_interval`,
default 15s; unset = off). One shared renderer feeds both (`metrics.rs`). Series are all
`tv_shell_*`: `build_info{sha,branch,version}` (current deployment), dev-action
counters (`deploy_total{outcome}`, `build_total`, `restart_*_total`),
input/intent/transition/pad counters, and cpu/mem/load/temp gauges (the gauges are
a convenience — prefer node_exporter for host resources). Full catalogue in
[`../docs/OBSERVABILITY.md`](../docs/OBSERVABILITY.md).

## Status

Phase 3 (zbus/Bluetooth/Wi-Fi-read/power) and Phase 4 (Hyprland + Sunshine
`health`) **require on-device verification** — the Linux-only modules (D-Bus,
`hyprland`) don't compile or run on macOS/CI, and `health`'s live fetch needs a
reachable Sunshine host. **HDMI-CEC** (`cec.rs`, #94) is deployed and verified on
gaming-client (static-linked libcec, no CEC hardware on CI).
