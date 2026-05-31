#!/bin/bash
# Game Shell session wrapper — launched by SDDM via .desktop file.
# Starts the input daemon, then hands off to Hyprland which auto-starts Quickshell.

SHELL_DIR="${GAME_SHELL_DIR:-/opt/game-shell}"

export XDG_CURRENT_DESKTOP=Hyprland
export GAME_SHELL_TARGETS="${GAME_SHELL_TARGETS:-$SHELL_DIR/targets.yaml}"
export GAME_SHELL_SOCK="/run/user/$(id -u)/game-shell-input.sock"

# Start the Rust input/backend daemon. It is the sole backend: gamepad
# grab/release, settings I/O, app discovery, Bluetooth/network/power, Hyprland
# reads, and Sunshine pre-flight all flow through it. Build with
# `cargo build --release` and install to `$SHELL_DIR/bin/game-shell-input`.
"$SHELL_DIR/bin/game-shell-input" &
INPUT_PID=$!

cleanup() {
    kill "$INPUT_PID" 2>/dev/null
    wait "$INPUT_PID" 2>/dev/null
}
trap cleanup EXIT

exec Hyprland -c "$SHELL_DIR/config/hyprland.conf"
