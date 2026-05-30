#!/bin/bash
# super-intent.sh — keyboard control-surface bridge for the game-shell.
#
# Hyprland binds inject a shell intent by passing its name as $1:
#   bare Super        -> super-intent.sh menu        (toggle the nav drawer)
#   Super + Escape    -> super-intent.sh home        (return-to-shell escape)
#   Super + Backspace -> super-intent.sh home-hold   (reset to home)
#
# It writes `intent <name>` to the input daemon's control surface, which
# validates the name against the closed vocabulary and re-broadcasts
# `intent:<name>` to the QML shell. The keyboard never reaches the daemon's
# evdev layer — this socket bridge is the whole keyboard->shell path, so it must
# be robust at early boot when the daemon socket may not exist yet: retry a few
# times with a short backoff. Uses socat or nc -U (whichever is installed).

INTENT="${1:-menu}"
SOCK="${GAME_SHELL_SOCK:-/run/user/$(id -u)/game-shell-input.sock}"

send_intent() {
    if command -v socat >/dev/null 2>&1; then
        printf 'intent %s\n' "$INTENT" | socat - "UNIX-CONNECT:$SOCK" >/dev/null 2>&1
    elif command -v nc >/dev/null 2>&1; then
        printf 'intent %s\n' "$INTENT" | nc -U -w1 "$SOCK" >/dev/null 2>&1
    else
        return 127
    fi
}

# Retry briefly so a press during the daemon's startup window still lands.
for _ in 1 2 3 4 5; do
    if [ -S "$SOCK" ] && send_intent; then
        exit 0
    fi
    sleep 0.2
done

exit 1
