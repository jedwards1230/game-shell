#!/bin/bash
# Game Shell session wrapper — launched by SDDM via .desktop file.
# Starts the input daemon, then hands off to Hyprland which auto-starts Quickshell.

SHELL_DIR="${GAME_SHELL_DIR:-/opt/game-shell}"

export XDG_CURRENT_DESKTOP=Hyprland
export GAME_SHELL_TARGETS="${GAME_SHELL_TARGETS:-$SHELL_DIR/targets.yaml}"
export GAME_SHELL_SOCK="/run/user/$(id -u)/game-shell-input.sock"

# Start the input/backend daemon: prefer the Rust binary, fall back to Python.
#
# IMPORTANT: the QML shell now sends backend commands the Python daemon does NOT
# implement (get-config/set-config, list-apps, record-launch/get-recents, bt-*,
# net-*, power-*, hypr-*, sunshine-status). The Rust daemon (`rust/`) answers all
# of them; gamepad-input.py answers only the original input commands. With Python
# running, gamepad input still works, but the Settings / app-discovery / system
# pages degrade (they get `unknown` replies) until the Rust daemon is present.
#
# Cutover = just build + install the binary; no edit here. Build on the target
# with `cargo build --release` and install to `$SHELL_DIR/bin/game-shell-input`;
# this script then auto-uses it on the next session. Python stays only as a
# rollback (delete/rename the binary to fall back).
if [ -x "$SHELL_DIR/bin/game-shell-input" ]; then
    "$SHELL_DIR/bin/game-shell-input" &
    INPUT_PID=$!
    # If the Rust daemon dies immediately (startup/config error), fall back to
    # the Python daemon — otherwise the session would run with no input daemon
    # at all, the most likely cutover failure mode.
    sleep 1
    if ! kill -0 "$INPUT_PID" 2>/dev/null; then
        echo "game-shell-session: Rust daemon exited at startup; falling back to gamepad-input.py" >&2
        python3 "$SHELL_DIR/input/gamepad-input.py" &
        INPUT_PID=$!
    fi
else
    python3 "$SHELL_DIR/input/gamepad-input.py" &
    INPUT_PID=$!
fi

cleanup() {
    kill "$INPUT_PID" 2>/dev/null
    wait "$INPUT_PID" 2>/dev/null
}
trap cleanup EXIT

exec Hyprland -c "$SHELL_DIR/config/hyprland.conf"
