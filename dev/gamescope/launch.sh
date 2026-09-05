#!/bin/bash
# Launch test clients into the running gamescope prototype session from an SSH
# session (or from inside it). Reads /tmp/tv-shell-gamescope.env for DISPLAY /
# WAYLAND_DISPLAY so the clients land inside gamescope, not on a stray socket.
#
#   launch.sh overlay                 QML overlay tagged STEAM_OVERLAY + STEAM_INPUT_FOCUS
#   launch.sh x11 <id> [cmd...]       any X11 app; every window of its pid is tagged
#                                     STEAM_GAME=<id> as it appears (pass --name <wm-name>
#                                     BEFORE the command as an extra lookup hint)
#   launch.sh apps <host>             what the streaming host is running now, and the exact
#                                     app names (quoted: leading spaces are part of them)
#   launch.sh moonlight [--quit] [args...]
#                                     Moonlight on X11 (xcb); every window of its pid is
#                                     tagged STEAM_GAME=9003 as it appears (the stream
#                                     window comes 5-20 s after launch) and the base layer
#                                     is 9003 over the shell; HDR via gamescope WSI.
#                                     `stream <host> <app>` is refused when the host is
#                                     already running a DIFFERENT app (Moonlight would hang
#                                     on an invisible "quit it?" dialog); --quit ends that
#                                     app first, explicitly, and is never implied.
#                                     GAMESCOPE_WSI_FORCE_BYPASS=1 in the environment is
#                                     passed through (try it when Moonlight logs
#                                     "hdr formats exposed to client: false")
#   launch.sh moonlight --wayland [args...]  the native-Wayland (xdg-shell) experiment;
#                                     no focus selector, and Moonlight-qt 6.1 crashed here
#   launch.sh xmessage <text>         the simplest possible X11 window
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
MOONLIGHT_BIN="${TV_SHELL_GS_MOONLIGHT:-moonlight}"
# Moonlight-qt's own host/app cache (QSettings INI): the only local source of
# the exact app names and of the app id -> name mapping Sunshine reports.
MOONLIGHT_CONF="${TV_SHELL_GS_MOONLIGHT_CONF:-${XDG_CONFIG_HOME:-$HOME/.config}/Moonlight Game Streaming Project/Moonlight.conf}"
SUNSHINE_PORT="${TV_SHELL_GS_SUNSHINE_PORT:-47989}"

# Qt 6 only (see lib.sh); resolved lazily so the non-Qt verbs work on a box
# without it.
need_qml() {
    if ! QML_BIN="$(gs_resolve_qml6 2>&1)"; then
        echo "launch.sh: $QML_BIN" >&2
        exit 2
    fi
}

# --- streaming-host helpers (K7/K8) ------------------------------------------

# sunshine_serverinfo <host> -> the unpaired /serverinfo XML (one line), or 1.
sunshine_serverinfo() {
    command -v curl >/dev/null 2>&1 || return 1
    curl -s --max-time 5 "http://$1:$SUNSHINE_PORT/serverinfo?uniqueid=0123456789ABCDEF" | tr -d '\n'
}

# xml_field <xml> <tag> -> the first <tag>...</tag> text, or "".
xml_field() {
    printf '%s\n' "$1" | sed -n "s/.*<$2>\([^<]*\)<\/$2>.*/\1/p" | head -1
}

# conf_unquote <value> -> QSettings quotes strings with leading/trailing
# whitespace (" Desktop"); strip one pair, keep the space.
conf_unquote() {
    local s="$1"
    case "$s" in
        \"*\") s="${s#\"}"; s="${s%\"}" ;;
    esac
    printf '%s\n' "$s"
}

# conf_host_index <name> -> the [hosts] index whose hostname/name/customname
# is <name>, or "".
conf_host_index() {
    [ -r "$MOONLIGHT_CONF" ] || return 0
    local n v
    while read -r n v; do
        if [ "$(conf_unquote "$v")" = "$1" ]; then
            printf '%s\n' "$n"
            return 0
        fi
    done < <(sed -n 's/^\([0-9][0-9]*\)\\\(hostname\|customname\|name\)=\(.*\)$/\1 \3/p' "$MOONLIGHT_CONF")
}

# conf_app_name <host-index|""> <app-id> -> the cached name of that app, or 1.
# An empty host index searches every host.
conf_app_name() {
    [ -r "$MOONLIGHT_CONF" ] || return 1
    local n="${1:-[0-9]+}" line prefix
    line="$(grep -E "^${n}\\\\apps\\\\[0-9]+\\\\id=$2\$" "$MOONLIGHT_CONF" | head -1)"
    [ -n "$line" ] || return 1
    prefix="${line%id=*}"
    line="$(grep -F "${prefix}name=" "$MOONLIGHT_CONF" | head -1)"
    [ -n "$line" ] || return 1
    conf_unquote "${line#*name=}"
}

# conf_app_names <host-index> -> every cached app name of that host, one per line.
conf_app_names() {
    [ -r "$MOONLIGHT_CONF" ] && [ -n "$1" ] || return 0
    local line
    while IFS= read -r line; do
        conf_unquote "${line#*=}"
    done < <(grep -E "^$1\\\\apps\\\\[0-9]+\\\\name=" "$MOONLIGHT_CONF")
}

# host_state <host> -> sets HOST_STATE, HOST_CURRENT (app id, "" when idle or
# unknown), HOST_NAME (what the host calls itself), HOST_RUNNING (cached name
# of the running app, "" when unknown). Returns 1 when serverinfo is unreachable.
host_state() {
    local info idx
    HOST_STATE=""; HOST_CURRENT=""; HOST_NAME=""; HOST_RUNNING=""; HOST_INDEX=""
    info="$(sunshine_serverinfo "$1")" || return 1
    [ -n "$info" ] || return 1
    HOST_STATE="$(xml_field "$info" state)"
    HOST_CURRENT="$(xml_field "$info" currentgame)"
    HOST_NAME="$(xml_field "$info" hostname)"
    idx="$(conf_host_index "$HOST_NAME")"
    [ -n "$idx" ] || idx="$(conf_host_index "$1")"
    HOST_INDEX="$idx"
    case "$HOST_CURRENT" in
        ""|0) HOST_CURRENT="" ;;
        *) HOST_RUNNING="$(conf_app_name "$idx" "$HOST_CURRENT" 2>/dev/null)" || HOST_RUNNING="" ;;
    esac
    return 0
}

# moonlight_precheck <host> <app> -> 0 to go ahead (host idle, or already
# running exactly <app>, which Moonlight resumes), 3 to refuse.
moonlight_precheck() {
    local host="$1" app="$2"
    if ! host_state "$host"; then
        echo "WARN: no serverinfo from $host:$SUNSHINE_PORT; cannot tell what it is running (streaming anyway)" >&2
        return 0
    fi
    if [ -z "$HOST_CURRENT" ]; then
        echo "streaming host: idle ($HOST_STATE); starting '$app'"
        return 0
    fi
    if [ -n "$HOST_RUNNING" ] && [ "$HOST_RUNNING" = "$app" ]; then
        echo "streaming host: already running '$app' (id $HOST_CURRENT); Moonlight will resume it"
        return 0
    fi
    {
        echo "launch.sh: REFUSED. The streaming host is ${HOST_STATE:-busy} with '${HOST_RUNNING:-<unknown name>}' (app id $HOST_CURRENT), not '$app'."
        echo "  \`moonlight stream\` would block forever on its invisible \"quit the running app?\" dialog."
        echo "  Either resume what is running:"
        if [ -n "$HOST_RUNNING" ]; then
            echo "      launch.sh moonlight stream $host '$HOST_RUNNING'"
        else
            echo "      launch.sh apps $host        # find the name of app id $HOST_CURRENT, then stream it"
        fi
        echo "  or end that session on the host first (a decision, never a default):"
        echo "      launch.sh moonlight --quit stream $host '$app'      # runs \`moonlight quit\` first"
        echo "      launch.sh moonlight quit $host                     # the same, by hand"
    } >&2
    return 3
}

# moonlight_headless <args...> -> a GUI-less Moonlight command (list/quit) with
# a cap, on the offscreen platform so no window lands in gamescope.
moonlight_headless() {
    QT_QPA_PLATFORM=offscreen timeout "${TV_SHELL_GS_MOONLIGHT_TIMEOUT:-30}" "$MOONLIGHT_BIN" "$@" 2>/dev/null \
        | grep -v -i -E 'ffmpeg|vaapi|Format 0x'
}

# tag_pid_then_base <pid> <appid> [gs_tag_pid opts...] -> base layer preference
# first (gamescope falls back to the shell until a window with <appid> exists,
# then switches the moment one is tagged), then tag every window of <pid>.
tag_pid_then_base() {
    local pid="$1" appid="$2"
    shift 2
    "$KIT/focus.sh" app "$appid,$SHELL_APPID"
    gs_tag_pid "$pid" "$appid" "$@"
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
        # --name is a lookup hint, --class a second predicate for toolkits
        # that set no _NET_WM_PID (Xt/Athena); both go to gs_tag_pid.
        NAME_OPTS=()
        while :; do
            case "${1:-}" in
                --name|--class) NAME_OPTS+=("$1" "$2"); shift 2 ;;
                *) break ;;
            esac
        done
        [ $# -ge 1 ] || { echo "launch.sh x11 <id> [--name <wm-name>] [--class <wm-class>] <cmd...>" >&2; exit 2; }
        export QT_QPA_PLATFORM=xcb
        export SDL_VIDEODRIVER=x11
        export ENABLE_GAMESCOPE_WSI=1
        nohup "$@" > "$LOG_DIR/x11-$APPID.log" 2>&1 &
        PID=$!
        echo "x11 app pid $PID (log $LOG_DIR/x11-$APPID.log)"
        tag_pid_then_base "$PID" "$APPID" --timeout 20 --log "$LOG_DIR/x11-$APPID.log" "${NAME_OPTS[@]}"
        ;;
    apps)
        HOST="${2:?launch.sh apps <host>}"
        if host_state "$HOST"; then
            if [ -n "$HOST_CURRENT" ]; then
                echo "streaming host '$HOST_NAME': $HOST_STATE, running '${HOST_RUNNING:-<name not cached>}' (app id $HOST_CURRENT)"
            else
                echo "streaming host '$HOST_NAME': ${HOST_STATE:-idle}, nothing running"
            fi
        else
            echo "streaming host: no serverinfo on port $SUNSHINE_PORT (down, asleep, or not Sunshine)"
            HOST_INDEX="$(conf_host_index "$HOST")"
        fi
        echo "== app names cached by Moonlight (quoted: a leading space is part of the name; copy it exactly):"
        if [ -n "$HOST_INDEX" ]; then
            while IFS= read -r n; do
                if [ -n "$HOST_RUNNING" ] && [ "$n" = "$HOST_RUNNING" ]; then
                    printf "    '%s'   <- running now\n" "$n"
                else
                    printf "    '%s'\n" "$n"
                fi
            done < <(conf_app_names "$HOST_INDEX")
        else
            echo "    (host not found in $MOONLIGHT_CONF; is it paired?)"
        fi
        echo "== moonlight list $HOST (live, ${TV_SHELL_GS_MOONLIGHT_TIMEOUT:-30} s cap):"
        moonlight_headless list "$HOST" | sed "s/^/    '/; s/\$/'/"
        ;;
    moonlight)
        shift
        # gamescope's WSI layer is what lets Moonlight present HDR; gamescope
        # sets ENABLE_GAMESCOPE_WSI for its own children but not for us,
        # arriving over SSH. Both paths below keep it.
        export ENABLE_GAMESCOPE_WSI=1
        export QT_WAYLAND_DISABLE_WINDOWDECORATION=1
        WAYLAND=""
        QUIT=""
        while :; do
            case "${1:-}" in
                --wayland) WAYLAND=1; shift ;;
                --quit) QUIT=1; shift ;;
                *) break ;;
            esac
        done
        # K7: `moonlight stream <host> <app>` while the host runs another app
        # opens a "quit the running app?" dialog inside Moonlight's unmapped
        # GUI and waits forever. Ask the host first. The app name is passed
        # through verbatim (K8: Sunshine's names may start with a space).
        if [ "${1:-}" = "stream" ] && [ -n "${2:-}" ] && [ -n "${3:-}" ]; then
            if [ -n "$QUIT" ]; then
                echo "--quit: ending whatever the streaming host is running (moonlight quit $2)"
                moonlight_headless quit "$2"
                for _ in 1 2 3 4 5 6 7 8 9 10; do
                    host_state "$2" && [ -z "$HOST_CURRENT" ] && break
                    sleep 1
                done
            fi
            moonlight_precheck "$2" "$3" || exit $?
        fi
        if [ -n "$WAYLAND" ]; then
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
        # Opt-in: the WSI layer only exposes HDR10 formats when the window can
        # bypass XWayland (matches its toplevel within 2 px). If Moonlight's
        # log says "hdr formats exposed to client: false" while the root atom
        # GAMESCOPE_HDR_OUTPUT_FEEDBACK is 1, force the bypass.
        if [ -n "${GAMESCOPE_WSI_FORCE_BYPASS:-}" ]; then
            export GAMESCOPE_WSI_FORCE_BYPASS
            echo "GAMESCOPE_WSI_FORCE_BYPASS=$GAMESCOPE_WSI_FORCE_BYPASS (XWayland bypass forced)"
        fi
        nohup "$MOONLIGHT_BIN" "$@" > "$LOG_DIR/moonlight.log" 2>&1 &
        PID=$!
        echo "moonlight (xcb) pid $PID (log $LOG_DIR/moonlight.log)"
        echo "HDR signature in the log: 'server hdr output enabled: true' + 'hdr formats exposed to client: true'"
        # K6: the stream is NOT the window named "Moonlight" (that is the Qt
        # main window, gone once the session starts). Tag every window of the
        # pid as it appears, and keep watching: the stream window is created
        # after the session handshake, 5-20 s in. The WSI log names its xid
        # the moment it exists; the name lookup covers the GUI (pairing, no
        # `stream` verb). In stream mode the watch ends once the stream window
        # ("<host> - Moonlight") is tagged.
        DONE_OPTS=(--expect 1)
        [ "${1:-}" = "stream" ] && DONE_OPTS=(--done-name '* - Moonlight')
        tag_pid_then_base "$PID" "$MOONLIGHT_APPID" --timeout 60 --class moonlight \
            --log "$LOG_DIR/moonlight.log" --name Moonlight "${DONE_OPTS[@]}"
        rc=$?
        "$KIT/focus.sh" list 2>/dev/null | grep -E 'FOCUSABLE_APPS|FOCUSED_APP'
        exit $rc
        ;;
    xmessage)
        shift
        nohup xmessage -center "${*:-hello from gamescope}" > "$LOG_DIR/xmessage.log" 2>&1 &
        PID=$!
        echo "xmessage pid $PID"
        # xmessage is an Xt client: no _NET_WM_PID, so match its WM_CLASS
        tag_pid_then_base "$PID" 9002 --timeout 10 --expect 1 --name xmessage --class xmessage
        ;;
    *)
        sed -n '2,26p' "$0"
        exit 2
        ;;
esac
