# game-shell-input (Rust)

A drop-in Rust replacement for `input/gamepad-input.py`. Same Unix socket, same
newline-delimited wire protocol (`docs/IPC_PROTOCOL.md`) — the QML shell is
unchanged. Phase 1 of [#28](https://github.com/jedwards1230/game-shell/issues/28).

## What it does

Grabs a gamepad via `EVIOCGRAB`, emits keyboard/mouse through uinput, and serves
the `grab`/`release`/`status`/`subscribe`/`get-bindings`/`set-binding`/
`capture-next`/`capture-cancel`/`kbd-log` protocol. Discovers an **arbitrary**
controller via the SDL GUID + bundled `SDL_GameControllerDB` (not just the
hardcoded Xbox pad), falling back to any `BTN_SOUTH` device.

## Layout

| File | Role |
|------|------|
| `protocol.rs` | Command parse + Event/response wire strings (bare text) |
| `config.rs` | Kernel codes, name tables, bindings, `settings.json` I/O |
| `device.rs` | SDL GUID + DB matching, device/keyboard discovery |
| `state.rs` | Control messages + pure input logic (velocity, deadzone, combos) |
| `input.rs` | Linux input runtime (evdev/uinput) — single state owner |
| `ipc.rs` | Unix-socket server, `broadcast` event fan-out |
| `main.rs` | Runtime wiring + signals |

## Build & test

The full binary only links on **Linux** (`evdev`/`uinput` are kernel
interfaces). `evdev` is a Linux-only dependency, so the portable modules still
compile and test on macOS:

```bash
cargo test            # runs everywhere (protocol/config/device/state/ipc)
cargo build --release # Linux only -> target/release/game-shell-input
```

## Deploy (later, on game-client-1)

1. `cargo build --release`
2. Install `target/release/game-shell-input` to `/opt/game-shell/bin/`
3. Switch the launch line in `scripts/game-shell-session.sh` (see the comment there)

Honors `GAME_SHELL_SOCK`, `GAMEPAD_VENDOR`/`GAMEPAD_PRODUCT` (exact-pin override),
and `GAME_SHELL_GAMECONTROLLERDB` (fuller controller DB).

## Status

The Python daemon stays the default until this is hardware-verified. Phases 2–4
(config/app-discovery native, zbus/Bluetooth/WiFi/power, Hyprland/CEC/health)
are tracked in #28 and out of scope here.
