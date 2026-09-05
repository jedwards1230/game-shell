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
# shellcheck source=dev/gamescope/lib.sh
. "$KIT/lib.sh"
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

# Qt 6 only (see lib.sh). The env file above is written first on purpose:
# the SSH-side tools still work against a session whose shell never came up.
if ! QML_BIN="$(gs_resolve_qml6 2>&1)"; then
    log "FATAL: $QML_BIN"
    log "install qt6-declarative (or set TV_SHELL_GS_QML) and restart the session"
    exit 1
fi
log "qml runtime: $QML_BIN ($(gs_qml_version "$QML_BIN"))"

# Tag the shell window with a game id so SteamControlled focus will consider
# it, then make it the base layer. The window maps asynchronously, so this
# polls (gs_tag_pid, lib.sh). A relaunched shell is a NEW X11 window, so this
# must run after every launch or focus.sh / launch.sh cannot select the shell
# again after its first crash. Tagging is by pid, not by title alone: the
# title is only a lookup hint, and a window carrying it is tagged only when its
# _NET_WM_PID is THIS shell's pid, so a previous instance's window that is
# still being torn down can never be the one that gets tagged.
tag_shell() {
    [ "$QT_QPA_PLATFORM" = "xcb" ] || return 0
    local out
    if out="$(TV_SHELL_GS_POLL_SECS="${TV_SHELL_GS_POLL_SECS:-0.5}" \
            gs_tag_pid "$SHELL_PID" "$SHELL_APPID" --timeout 10 --expect 1 --name "$SHELL_TITLE" 2>&1)"; then
        "$KIT/focus.sh" app "$SHELL_APPID" || true
        log "tagged '$SHELL_TITLE' (pid $SHELL_PID) as app $SHELL_APPID and set it as base layer: $out"
        return 0
    fi
    log "WARN: no window of the shell (pid $SHELL_PID) appeared within 10s; focus.sh cannot select it: $out"
}

launch_shell() {
    "$QML_BIN" "$KIT/proto-shell.qml" &
    SHELL_PID=$!
    LAUNCHED_AT=$SECONDS
    log "prototype shell pid=$SHELL_PID qpa=$QT_QPA_PLATFORM"
    tag_shell
}

launch_shell

# Keep the primary child alive. If the shell dies, relaunch it (crude
# supervisor; the v2 supervisor design is separate).
#
# Backoff: every relaunch re-tags the new window and re-asserts it as the base
# layer, which stomps on any focus test running from SSH. A shell that dies
# within FAST_EXIT_SECS of launch FAST_EXIT_LIMIT times in a row is not going
# to come up (a broken runtime, a QML error), so the retry interval stretches
# to BACKOFF_SECS. It never stops relaunching: a fixed runtime is picked up on
# the next attempt.
FAST_EXIT_SECS=10
FAST_EXIT_LIMIT=3
BACKOFF_SECS=60
fast_exits=0
while true; do
    wait "$SHELL_PID"
    rc=$?
    alive=$((SECONDS - LAUNCHED_AT))
    if [ "$alive" -lt "$FAST_EXIT_SECS" ]; then
        fast_exits=$((fast_exits + 1))
    else
        fast_exits=0
    fi
    if [ "$fast_exits" -ge "$FAST_EXIT_LIMIT" ]; then
        delay=$BACKOFF_SECS
        log "prototype shell exited rc=$rc after ${alive}s ($fast_exits fast exits in a row); backing off, relaunching in ${delay}s"
    else
        delay=2
        log "prototype shell exited rc=$rc after ${alive}s; relaunching in ${delay}s"
    fi
    sleep "$delay"
    launch_shell
done
