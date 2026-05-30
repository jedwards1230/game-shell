# game-shell-input (Rust)

A Rust backend daemon replacing `input/gamepad-input.py` and the QML shell's
inline `python3`/shell-out parsers. Same Unix socket + newline-delimited wire
protocol (`docs/IPC_PROTOCOL.md`). Phases 1–4 of
[#28](https://github.com/jedwards1230/game-shell/issues/28) (CEC deferred).

Note: the QML now depends on this daemon for the Settings / app-discovery /
system pages — the Python daemon only answers the original input commands. See
`scripts/game-shell-session.sh` (prefer-Rust-else-Python launcher).

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
| `state.rs` | Control messages + pure input logic (velocity, deadzone, combos) |
| `input.rs` | Linux input runtime (evdev/uinput) — single state owner; multi-pad `Fleet` |
| `bluetooth.rs` | **Linux-only.** BlueZ actor via `bluer` — scan/pair/connect/trust + `bt:*` events |
| `network.rs` | **Linux-only.** NetworkManager **read** actor via `zbus` — connectivity / AP list / `net:*` events |
| `power.rs` | **Linux-only.** logind suspend + UPower battery via `zbus` — `power:*` events |
| `hyprland.rs` | **Linux-only.** Hyprland actor over direct IPC sockets (no crate) — active-window/clients queries + `hypr:*` events |
| `health.rs` | Sunshine session detection via `reqwest`/rustls (cross-platform) — `sunshine-status` |
| `ipc.rs` | Unix-socket server, `broadcast` event fan-out, D-Bus command routing |
| `main.rs` | Runtime wiring + signals + D-Bus actor spawn |

`apps.rs`, `recents.rs`, and `health.rs`'s response parser are pure Rust —
fully unit-tested on macOS.

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
  `combo:home-hold` once even when two pads hold Home simultaneously. For a
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
  `hypr-active` (active window `{class,title,address}`) and `hypr-clients`
  (`hyprctl clients -j`-equivalent `{class,title,address,workspace}` array) by
  sending `j/activewindow` / `j/clients` to the request socket, and streams
  `hypr:activewindow:<class>` / `hypr:fullscreen:<0|1>` events read from the
  `.socket2.sock` event stream. This replaces the `hyprctl clients -j` shell-out
  in `components/HyprctlClients.qml` and feeds `AppLifecycleManager.qml`. One-shot
  `hyprctl dispatch` *actions* stay in QML.
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

**HDMI-CEC was deferred** — `AVController.qml` / `AVControlSettings.qml` still
shell out to `cec-client` and were intentionally left untouched. That subsystem
is a follow-up.

## Build & test

The full binary only links on **Linux** (`evdev`/`uinput` are kernel
interfaces). `evdev` is a Linux-only dependency, so the portable modules still
compile and test on macOS:

```bash
cargo test            # runs everywhere (protocol/config/apps/recents/device/state/ipc)
cargo build --release # Linux only -> target/release/game-shell-input
```

**Linux build deps:** the Phase 3 Bluetooth module uses `bluer`, which pulls in
`libdbus-sys`, so a Linux build needs the D-Bus headers + pkg-config:
`apt-get install libdbus-1-dev pkg-config` (Debian/CI) — on Arch / game-client-1
these come with the core `dbus`/`base-devel`. `zbus` (network/power) and
`reqwest`/`rustls-tls` (health) are pure Rust and need nothing; Hyprland IPC uses
raw Unix sockets (no crate, no system deps).

## Deploy

1. `cargo build --release`
2. Install `target/release/game-shell-input` to `/opt/game-shell/bin/`

No launch-line edit is needed: `scripts/game-shell-session.sh` auto-prefers the
binary on the next session when it's present, and falls back to the Python daemon
if the binary is absent or exits immediately at startup.

Honors `GAME_SHELL_SOCK`, `GAMEPAD_VENDOR`/`GAMEPAD_PRODUCT` (exact-pin override),
and `GAME_SHELL_GAMECONTROLLERDB` (fuller controller DB).

## Status

The Python daemon stays the default until this is hardware-verified. Phase 3
(zbus/Bluetooth/Wi-Fi-read/power) and Phase 4 (Hyprland + Sunshine `health`) are
implemented above but **require on-device verification** — the Linux-only modules
(D-Bus, `hyprland`) don't compile or run on macOS/CI, and `health`'s live fetch
needs a reachable Sunshine host. **HDMI-CEC remains deferred** (still a
`cec-client` shell-out in `AVController.qml`) as a follow-up.
