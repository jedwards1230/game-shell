#!/bin/bash
# Launch test clients into the running gamescope prototype session from an SSH
# session (or from inside it). Reads /tmp/tv-shell-gamescope.env for DISPLAY /
# WAYLAND_DISPLAY so the clients land inside gamescope, not on a stray socket.
#
#   launch.sh overlay                 QML overlay tagged STEAM_OVERLAY + STEAM_INPUT_FOCUS
#   launch.sh x11 <id> [cmd...]       any X11 app; window is tagged STEAM_GAME=<id> by
#                                     WM_NAME once you pass --name <wm-name> (see below)
#   launch.sh moonlight [args...]     Moonlight on X11 (xcb), tagged STEAM_GAME=9003 and
#                                     made the base layer; HDR via gamescope WSI
#   launch.sh moonlight --wayland [args...]  the native-Wayland (xdg-shell) experiment;
#                                     no focus selector, and Moonlight-qt 6.1 crashed here
#   launch.sh xmessage <text>         the simplest possible X11 window
#
# For `x11`, the app is expected to set its own WM_NAME; pass `--name <wm-name>`
# BEFORE the command to have launch.sh tag that window with STEAM_GAME=<id>.
set -u

KIT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=dev/gamescope/lib.sh
. "$KIT/lib.sh"
ENV_FILE="${TV_SHELL_GS_ENV_FILE:-/tmp/tv-shell-gamescope.env}"
if [ -r "$ENV_FILE" ]; then
    # shellcheck source=/dev/null
    . "$ENV_FILE"
fi
if [ -z "${DISPLAY:-}" ]; then
    echo "launch.sh: no DISPLAY; is the gamescope session running?" >&2
    exit 2
fi

SHELL_APPID="${TV_SHELL_GS_SHELL_APPID:-9001}"
MOONLIGHT_APPID=9003
LOG_DIR=/tmp/tv-shell-gamescope-clients
mkdir -p "$LOG_DIR"

# Qt 6 only (see lib.sh); resolved lazily so the non-Qt verbs work on a box
# without it.
need_qml() {
    if ! QML_BIN="$(gs_resolve_qml6 2>&1)"; then
        echo "launch.sh: $QML_BIN" >&2
        exit 2
    fi
}

tag_by_name() { # tag_by_name <wm-name> <appid>
    for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
        sleep 0.5
        if "$KIT/focus.sh" tag "$1" "$2" >/dev/null 2>&1; then
            echo "tagged '$1' STEAM_GAME=$2"
            return 0
        fi
    done
    echo "WARN: no window named '$1' appeared in 10s; not tagged" >&2
    return 1
}

case "${1:-}" in
    overlay)
        need_qml
        export QT_QPA_PLATFORM=xcb
        export QT_WAYLAND_DISABLE_WINDOWDECORATION=1
        nohup "$QML_BIN" "$KIT/proto-overlay.qml" > "$LOG_DIR/overlay.log" 2>&1 &
        echo "overlay pid $!"
        for _ in 1 2 3 4 5 6 7 8 9 10; do
            sleep 0.5
            if xprop -name tv-shell-proto-overlay -f STEAM_OVERLAY 32c -set STEAM_OVERLAY 1 2>/dev/null; then
                xprop -name tv-shell-proto-overlay -f STEAM_INPUT_FOCUS 32c -set STEAM_INPUT_FOCUS 1
                echo "tagged overlay: STEAM_OVERLAY=1 STEAM_INPUT_FOCUS=1"
                echo "expect: panel visible over the current app, app keeps running, keys go to the panel"
                exit 0
            fi
        done
        echo "WARN: overlay window never appeared; see $LOG_DIR/overlay.log" >&2
        exit 1
        ;;
    x11)
        shift
        APPID="${1:?app id}"; shift
        NAME=""
        if [ "${1:-}" = "--name" ]; then NAME="$2"; shift 2; fi
        [ $# -ge 1 ] || { echo "launch.sh x11 <id> [--name <wm-name>] <cmd...>" >&2; exit 2; }
        export QT_QPA_PLATFORM=xcb
        export SDL_VIDEODRIVER=x11
        nohup "$@" > "$LOG_DIR/x11-$APPID.log" 2>&1 &
        echo "x11 app pid $! (log $LOG_DIR/x11-$APPID.log)"
        if [ -n "$NAME" ]; then
            tag_by_name "$NAME" "$APPID" && "$KIT/focus.sh" app "$APPID,$SHELL_APPID"
        else
            echo "not tagged (no --name); use focus.sh list + focus.sh window <xid> to show it"
        fi
        ;;
    moonlight)
        shift
        # gamescope's WSI layer is what lets Moonlight present HDR; gamescope
        # sets ENABLE_GAMESCOPE_WSI for its own children but not for us,
        # arriving over SSH. Both paths below keep it.
        export ENABLE_GAMESCOPE_WSI=1
        export QT_WAYLAND_DISABLE_WINDOWDECORATION=1
        MOONLIGHT_BIN="${TV_SHELL_GS_MOONLIGHT:-moonlight}"
        if [ "${1:-}" = "--wayland" ]; then
            shift
            # Native Wayland (xdg-shell) experiment. Measured 2026-09-05:
            # Moonlight-qt 6.1.0 SIGSEGVs ~7 s in, right after its decoder
            # self-test, and a Wayland window has no STEAM_GAME selector anyway.
            export QT_QPA_PLATFORM=wayland
            export SDL_VIDEODRIVER=wayland
            nohup "$MOONLIGHT_BIN" "$@" > "$LOG_DIR/moonlight.log" 2>&1 &
            echo "moonlight (wayland) pid $! (log $LOG_DIR/moonlight.log)"
            echo "Wayland-native windows have no STEAM_GAME selector; if it does not appear, run:"
            echo "  focus.sh list   # then focus.sh window <xid> is X11-only, so check GAMESCOPE_FOCUSABLE_APPS"
            exit 0
        fi
        # X11 (xcb) is the path that survives. SDL on x11 too, so the stream
        # window is an X11 window gamescope's SteamControlled policy can
        # select; the WSI layer reads GAMESCOPE_HDR_OUTPUT_FEEDBACK off the X11
        # root for HDR, so nothing is lost versus Wayland on that front.
        export QT_QPA_PLATFORM=xcb
        export SDL_VIDEODRIVER=x11
        nohup "$MOONLIGHT_BIN" "$@" > "$LOG_DIR/moonlight.log" 2>&1 &
        echo "moonlight (xcb) pid $! (log $LOG_DIR/moonlight.log)"
        tag_by_name Moonlight "$MOONLIGHT_APPID" && "$KIT/focus.sh" app "$MOONLIGHT_APPID,$SHELL_APPID"
        ;;
    xmessage)
        shift
        nohup xmessage -center "${*:-hello from gamescope}" > "$LOG_DIR/xmessage.log" 2>&1 &
        echo "xmessage pid $!"
        tag_by_name xmessage 9002 && "$KIT/focus.sh" app "9002,$SHELL_APPID"
        ;;
    *)
        sed -n '2,17p' "$0"
        exit 2
        ;;
esac
