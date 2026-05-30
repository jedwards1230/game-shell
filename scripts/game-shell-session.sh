#!/bin/bash
# Game Shell session wrapper — launched by SDDM via .desktop file.
# Starts the input daemon, then hands off to Hyprland which auto-starts Quickshell.

SHELL_DIR="${GAME_SHELL_DIR:-/opt/game-shell}"

export XDG_CURRENT_DESKTOP=Hyprland
export GAME_SHELL_TARGETS="${GAME_SHELL_TARGETS:-$SHELL_DIR/targets.yaml}"
export GAME_SHELL_SOCK="/run/user/$(id -u)/game-shell-input.sock"

# Start input daemon.
# The Python daemon remains the default. A drop-in Rust replacement lives in
# `rust/` (same socket, same wire protocol). Once it is built and installed on
# the target (`cargo build --release` -> `$SHELL_DIR/bin/game-shell-input`),
# switch the line below to:
#   "$SHELL_DIR/bin/game-shell-input" &
python3 "$SHELL_DIR/input/gamepad-input.py" &
INPUT_PID=$!

cleanup() {
    kill "$INPUT_PID" 2>/dev/null
    wait "$INPUT_PID" 2>/dev/null
}
trap cleanup EXIT

exec Hyprland -c "$SHELL_DIR/config/hyprland.conf"
