#!/bin/bash
# gamescope's primary child for the prototype session (see session.sh).
#
# Runs INSIDE gamescope: DISPLAY (Xwayland), WAYLAND_DISPLAY and
# GAMESCOPE_WAYLAND_DISPLAY are set by gamescope. It launches the prototype
# shell as an X11 client, tags it so gamescope's focus policy will show it, and
# then waits forever so the session stays up (gamescope also has --keep-alive).
#
# It also writes an env file so launch.sh / focus.sh / measure.sh can be driven
# from an SSH session on another machine, which is how the measurements are
# taken: the couch has no keyboard and the prototype shell launches nothing.
set -u

KIT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENV_FILE="${TV_SHELL_GS_ENV_FILE:-/tmp/tv-shell-gamescope.env}"
SHELL_APPID="${TV_SHELL_GS_SHELL_APPID:-9001}"
SHELL_TITLE="tv-shell-proto"

log() { printf 'tv-shell-gamescope[client]: %s\n' "$*"; }

{
    printf 'export DISPLAY=%q\n' "${DISPLAY:-}"
    printf 'export WAYLAND_DISPLAY=%q\n' "${WAYLAND_DISPLAY:-}"
    printf 'export GAMESCOPE_WAYLAND_DISPLAY=%q\n' "${GAMESCOPE_WAYLAND_DISPLAY:-}"
    printf 'export XDG_RUNTIME_DIR=%q\n' "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    printf 'export XAUTHORITY=%q\n' "${XAUTHORITY:-}"
    printf 'export TV_SHELL_GS_SHELL_APPID=%q\n' "$SHELL_APPID"
} > "$ENV_FILE"
log "wrote $ENV_FILE (DISPLAY=${DISPLAY:-unset} WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-unset})"

# The prototype shell is a plain QML Window run by Qt's `qml` runtime on the
# xcb platform. X11 on purpose: gamescope's external focus control and the
# interactive overlay plane are X11 atoms (STEAM_GAME, STEAM_OVERLAY,
# GAMESCOPECTRL_BASELAYER_*). Set TV_SHELL_GS_QPA=wayland to measure the
# xdg-shell path instead (then focus.sh cannot select it).
export QT_QPA_PLATFORM="${TV_SHELL_GS_QPA:-xcb}"
export QT_WAYLAND_DISABLE_WINDOWDECORATION=1
export ENABLE_GAMESCOPE_WSI=1

QML_BIN="$(command -v qml || echo /usr/lib/qt6/bin/qml)"
"$QML_BIN" "$KIT/proto-shell.qml" &
SHELL_PID=$!
log "prototype shell pid=$SHELL_PID qpa=$QT_QPA_PLATFORM"

if [ "$QT_QPA_PLATFORM" = "xcb" ]; then
    # Tag the shell window with a game id so SteamControlled focus will consider
    # it, then make it the base layer. Retry: the window maps asynchronously.
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        sleep 0.5
        if "$KIT/focus.sh" tag "$SHELL_TITLE" "$SHELL_APPID" >/dev/null 2>&1; then
            "$KIT/focus.sh" app "$SHELL_APPID" || true
            log "tagged '$SHELL_TITLE' as app $SHELL_APPID and set it as base layer"
            break
        fi
    done
fi

# Keep the primary child alive. If the shell dies, relaunch it (crude
# supervisor; the v2 supervisor design is separate).
while true; do
    if ! kill -0 "$SHELL_PID" 2>/dev/null; then
        log "prototype shell exited; relaunching in 2s"
        sleep 2
        "$QML_BIN" "$KIT/proto-shell.qml" &
        SHELL_PID=$!
    fi
    sleep 5
done
