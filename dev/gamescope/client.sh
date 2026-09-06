#!/bin/bash
# gamescope's primary child for the prototype session (see session.sh).
#
# Runs INSIDE gamescope: DISPLAY (Xwayland), WAYLAND_DISPLAY and
# GAMESCOPE_WAYLAND_DISPLAY are set by gamescope. It launches ONE primary
# client as an X11 client INSIDE ITS OWN app-steam-app<id>-<pid>.scope — which
# is how gamescope's focus policy identifies an app (lib.sh gs_scope_run) —
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
# How long the one-shot repair pass waits for Moonlight's first window (see
# tag_moonlight). Seconds, not the day it used to be: the pass no longer has to
# outlive the client waiting for a stream window the scope already identifies.
TAG_TIMEOUT="${TV_SHELL_GS_TAG_TIMEOUT:-30}"

log() { printf 'tv-shell-gamescope[client]: %s\n' "$*"; }

{
    printf 'export DISPLAY=%q\n' "${DISPLAY:-}"
    printf 'export WAYLAND_DISPLAY=%q\n' "${WAYLAND_DISPLAY:-}"
    printf 'export GAMESCOPE_WAYLAND_DISPLAY=%q\n' "${GAMESCOPE_WAYLAND_DISPLAY:-}"
    printf 'export XDG_RUNTIME_DIR=%q\n' "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    # launch.sh arrives over SSH with no session bus of its own, and
    # `systemd-run --user` (how every app gets its identifying cgroup scope)
    # needs one. Carry the session's.
    printf 'export DBUS_SESSION_BUS_ADDRESS=%q\n' \
        "${DBUS_SESSION_BUS_ADDRESS:-unix:path=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/bus}"
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

# The primary child is launched inside its own app-steam-app<id>-<pid>.scope,
# which is how gamescope identifies an app (lib.sh gs_scope_run). Checked once,
# up front and fatally: without a session bus there is no scope, and without a
# scope the client renders into a session that will never make it the focus.
if ! gs_scope_ready; then
    log "FATAL: cannot create a systemd scope for the primary child (see the error above)"
    exit 2
fi

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

# The REPAIR pass for the shell's own window: the scope already gives it app
# id $SHELL_APPID, so this only writes the STEAM_GAME override and reports what
# appeared. It stays bounded and one-shot. A window carrying the title is
# touched only when its _NET_WM_PID is THIS shell's pid, so a previous
# instance's window still being torn down can never be the one that gets
# tagged.
tag_shell() {
    [ "$QT_QPA_PLATFORM" = "xcb" ] || return 0
    local out
    if out="$(TV_SHELL_GS_POLL_SECS="${TV_SHELL_GS_POLL_SECS:-0.5}" \
            gs_tag_pid "$CLIENT_PID" "$SHELL_APPID" --timeout 10 --expect 1 --name "$SHELL_TITLE" 2>&1)"; then
        log "tagged '$SHELL_TITLE' (pid $CLIENT_PID) as app $SHELL_APPID (base layer $SHELL_APPID, set at launch): $out"
        return 0
    fi
    log "WARN: no window of the shell (pid $CLIENT_PID) appeared within 10s: $out"
}

launch_proto() {
    # Base layer first: the app id comes from the scope name, so it is known
    # before the process exists and cannot race a tag.
    "$KIT/focus.sh" app "$SHELL_APPID" || true
    gs_scope_run "$SHELL_APPID" "$QML_BIN" "$KIT/proto-shell.qml" &
    CLIENT_PID=$!
    log "prototype shell pid=$CLIENT_PID qpa=$QT_QPA_PLATFORM scope=app-steam-app$SHELL_APPID-$CLIENT_PID.scope"
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

# The repair pass for Moonlight, bounded and one-shot — deliberately NOT the
# day-long background watcher this used to run.
#
# That watcher existed because Moonlight's stream window is a SECOND X11 window
# of the same pid, created whenever the person on the couch picks a game,
# minutes after launch: a one-shot `--expect 1` tagged the GUI grid and
# stopped, leaving the stream window untagged and unselectable. Scope
# identification retires that whole problem. The stream window belongs to the
# same process, in the same cgroup, so gamescope resolves it to
# $MOONLIGHT_APPID the moment it is created — no tag, no watch, nothing to
# arrive late. Keeping a day-long watcher whose only job is now redundant would
# be a process that looks like it is doing something and is not; the bench run
# on 2026-09-06 logged exactly that line ("watching its windows for 86400s")
# while nothing was ever tagged, because with no scope the atom it discovers
# windows through was empty. So it is gone.
#
# What remains is a short pass over the FIRST window, for its report and for
# the STEAM_GAME override if that window's scope did not resolve.
tag_moonlight() {
    local out
    if out="$(gs_tag_pid "$CLIENT_PID" "$MOONLIGHT_APPID" --timeout "$TAG_TIMEOUT" --expect 1 \
            --class moonlight --name Moonlight --log "$MOONLIGHT_LOG" 2>&1)"; then
        log "moonlight (pid $CLIENT_PID) is app $MOONLIGHT_APPID (base layer $MOONLIGHT_APPID, set at launch): $out"
        return 0
    fi
    log "WARN: no window of moonlight (pid $CLIENT_PID) appeared within ${TAG_TIMEOUT}s: $out"
}

launch_moonlight() {
    # Base layer first — the app id is fixed by the scope name at launch.
    "$KIT/focus.sh" app "$MOONLIGHT_APPID" || true
    gs_scope_run "$MOONLIGHT_APPID" "$MOONLIGHT_BIN" \
        ${MOONLIGHT_ARGS[@]+"${MOONLIGHT_ARGS[@]}"} > "$MOONLIGHT_LOG" 2>&1 &
    CLIENT_PID=$!
    log "moonlight pid=$CLIENT_PID qpa=$QT_QPA_PLATFORM args=[${MOONLIGHT_ARGS[*]:-}] scope=app-steam-app$MOONLIGHT_APPID-$CLIENT_PID.scope (log $MOONLIGHT_LOG)"
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
