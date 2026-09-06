#!/bin/bash
# gamescope's primary child for the prototype session (see session.sh).
#
# Runs INSIDE gamescope: DISPLAY (Xwayland), WAYLAND_DISPLAY and
# GAMESCOPE_WAYLAND_DISPLAY are set by gamescope. It launches ONE primary
# client as an X11 client, tags it so gamescope's focus policy will show it,
# makes it the base layer, and then supervises it forever so the session stays
# up (gamescope also has --keep-alive).
#
# Which client, TV_SHELL_GS_CLIENT:
#   proto      (default) the prototype QML shell: a deliberately tiny window
#              that launches nothing. This is the measurement rig — leaving it
#              the default keeps the kit's bench behaviour unchanged.
#   moonlight  Moonlight-qt's own GUI, which IS a couch-navigable grid of the
#              streaming host's apps. The interim "boot straight into a
#              streaming client" session, until the v2 core exists. See README.
#
# It also writes an env file so launch.sh / focus.sh / measure.sh can be driven
# from an SSH session on another machine, which is how the measurements are
# taken: the couch has no keyboard and the prototype shell launches nothing.
set -u

KIT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=dev/gamescope/lib.sh
. "$KIT/lib.sh"
ENV_FILE="${TV_SHELL_GS_ENV_FILE:-/tmp/tv-shell-gamescope.env}"
CLIENT="${TV_SHELL_GS_CLIENT:-proto}"
SHELL_APPID="${TV_SHELL_GS_SHELL_APPID:-9001}"
SHELL_TITLE="tv-shell-proto"
# The same id launch.sh tags Moonlight with, so a session-launched Moonlight
# and an SSH-launched one are the same app to gamescope's focus policy.
MOONLIGHT_APPID=9003
MOONLIGHT_BIN="${TV_SHELL_GS_MOONLIGHT:-moonlight}"
LOG_DIR="${TV_SHELL_GS_LOG_DIR:-/tmp/tv-shell-gamescope-clients}"
MOONLIGHT_LOG="$LOG_DIR/moonlight.log"
# Extra Moonlight args, word-split (the same contract as TV_SHELL_GS_EXTRA).
# Empty means the GUI grid; `stream <host> <app>` boots straight into a stream.
# shellcheck disable=SC2206 # word-splitting is the documented contract
MOONLIGHT_ARGS=(${TV_SHELL_GS_MOONLIGHT_ARGS:-})
# How long the moonlight window watch runs (see tag_moonlight).
TAG_TIMEOUT="${TV_SHELL_GS_TAG_TIMEOUT:-86400}"

log() { printf 'tv-shell-gamescope[client]: %s\n' "$*"; }

{
    printf 'export DISPLAY=%q\n' "${DISPLAY:-}"
    printf 'export WAYLAND_DISPLAY=%q\n' "${WAYLAND_DISPLAY:-}"
    printf 'export GAMESCOPE_WAYLAND_DISPLAY=%q\n' "${GAMESCOPE_WAYLAND_DISPLAY:-}"
    printf 'export XDG_RUNTIME_DIR=%q\n' "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    printf 'export XAUTHORITY=%q\n' "${XAUTHORITY:-}"
    printf 'export TV_SHELL_GS_SHELL_APPID=%q\n' "$SHELL_APPID"
    printf 'export TV_SHELL_GS_CLIENT=%q\n' "$CLIENT"
} > "$ENV_FILE"
log "wrote $ENV_FILE (DISPLAY=${DISPLAY:-unset} WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-unset})"

case "$CLIENT" in
    proto|moonlight) log "primary child: $CLIENT" ;;
    *)
        log "FATAL: TV_SHELL_GS_CLIENT='$CLIENT' is not one of: proto, moonlight"
        exit 2
        ;;
esac

# --- proto: the prototype QML shell -----------------------------------------

# The prototype shell is a plain QML Window run by Qt's `qml` runtime on the
# xcb platform. X11 on purpose: gamescope's external focus control and the
# interactive overlay plane are X11 atoms (STEAM_GAME, STEAM_OVERLAY,
# GAMESCOPECTRL_BASELAYER_*). Set TV_SHELL_GS_QPA=wayland to measure the
# xdg-shell path instead (then focus.sh cannot select it).
setup_proto() {
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
}

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
            gs_tag_pid "$CLIENT_PID" "$SHELL_APPID" --timeout 10 --expect 1 --name "$SHELL_TITLE" 2>&1)"; then
        "$KIT/focus.sh" app "$SHELL_APPID" || true
        log "tagged '$SHELL_TITLE' (pid $CLIENT_PID) as app $SHELL_APPID and set it as base layer: $out"
        return 0
    fi
    log "WARN: no window of the shell (pid $CLIENT_PID) appeared within 10s; focus.sh cannot select it: $out"
}

launch_proto() {
    "$QML_BIN" "$KIT/proto-shell.qml" &
    CLIENT_PID=$!
    log "prototype shell pid=$CLIENT_PID qpa=$QT_QPA_PLATFORM"
    tag_shell
}

# --- moonlight: the streaming client as the session's primary child ---------

setup_moonlight() {
    # Same environment launch.sh's `moonlight` verb uses (lib.sh), so the
    # session path and the SSH path cannot drift.
    gs_moonlight_x11_env
    mkdir -p "$LOG_DIR"
    if ! command -v "$MOONLIGHT_BIN" >/dev/null 2>&1 && [ ! -x "$MOONLIGHT_BIN" ]; then
        log "FATAL: no Moonlight binary '$MOONLIGHT_BIN'"
        log "install moonlight-qt (or set TV_SHELL_GS_MOONLIGHT) and restart the session"
        exit 1
    fi
}

# Base layer first, then a watch that keeps tagging every window of the pid.
#
# The base layer goes first because gamescope switches to it the moment a
# window carrying the appid is tagged, so there is no window in which an
# untagged Moonlight is on screen with the wrong base layer.
#
# The watch runs in the BACKGROUND and for the life of the client, unlike the
# shell's one-shot tag, because Moonlight's stream window is a SECOND X11
# window of the same pid that only exists once a stream starts — which here
# means whenever the person on the couch picks a game, minutes after launch. A
# one-shot `--expect 1` would tag the GUI grid and stop, leaving the stream
# window with no STEAM_GAME and therefore unselectable by gamescope's
# SteamControlled policy. gs_tag_pid ends by itself when the pid is gone, so
# the watch dies with the client it belongs to.
#
# A relaunched Moonlight is a NEW X11 window (same reason as tag_shell), so
# this runs after every launch, and the previous watch is ended first.
tag_moonlight() {
    [ -z "${TAG_PID:-}" ] || kill "$TAG_PID" 2>/dev/null
    TAG_PID=""
    "$KIT/focus.sh" app "$MOONLIGHT_APPID" || true
    log "moonlight (pid $CLIENT_PID) is app $MOONLIGHT_APPID and the base layer; watching its windows for ${TAG_TIMEOUT}s"
    gs_tag_pid "$CLIENT_PID" "$MOONLIGHT_APPID" --timeout "$TAG_TIMEOUT" \
        --class moonlight --name Moonlight --log "$MOONLIGHT_LOG" &
    TAG_PID=$!
}

launch_moonlight() {
    "$MOONLIGHT_BIN" ${MOONLIGHT_ARGS[@]+"${MOONLIGHT_ARGS[@]}"} > "$MOONLIGHT_LOG" 2>&1 &
    CLIENT_PID=$!
    log "moonlight pid=$CLIENT_PID qpa=$QT_QPA_PLATFORM args=[${MOONLIGHT_ARGS[*]:-}] (log $MOONLIGHT_LOG)"
    tag_moonlight
}

# --- supervisor -------------------------------------------------------------

launch_client() {
    case "$CLIENT" in
        proto) launch_proto ;;
        moonlight) launch_moonlight ;;
    esac
    LAUNCHED_AT=$SECONDS
}

case "$CLIENT" in
    proto) setup_proto ;;
    moonlight) setup_moonlight ;;
esac

launch_client

# Keep the primary child alive. If it dies, relaunch it (crude supervisor; the
# v2 supervisor design is separate). Relaunching is the right behaviour for a
# streaming appliance too: quitting Moonlight should put you back on its grid,
# not on a black screen.
#
# Backoff: every relaunch re-tags the new window and re-asserts it as the base
# layer, which stomps on any focus test running from SSH. A client that dies
# within FAST_EXIT_SECS of launch FAST_EXIT_LIMIT times in a row is not going
# to come up (a broken runtime, a QML error, a Moonlight that cannot open the
# display), so the retry interval stretches to BACKOFF_SECS and the loop cannot
# hot-spin. It never stops relaunching: a fixed runtime is picked up on the
# next attempt.
FAST_EXIT_SECS=10
FAST_EXIT_LIMIT=3
BACKOFF_SECS=60
fast_exits=0
while true; do
    wait "$CLIENT_PID"
    rc=$?
    alive=$((SECONDS - LAUNCHED_AT))
    if [ "$alive" -lt "$FAST_EXIT_SECS" ]; then
        fast_exits=$((fast_exits + 1))
    else
        fast_exits=0
    fi
    if [ "$fast_exits" -ge "$FAST_EXIT_LIMIT" ]; then
        delay=$BACKOFF_SECS
        log "$CLIENT exited rc=$rc after ${alive}s ($fast_exits fast exits in a row); backing off, relaunching in ${delay}s"
    else
        delay=2
        log "$CLIENT exited rc=$rc after ${alive}s; relaunching in ${delay}s"
    fi
    sleep "$delay"
    launch_client
done
