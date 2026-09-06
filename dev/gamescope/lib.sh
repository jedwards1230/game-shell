#!/bin/bash
# Shared helpers for the gamescope prototype kit. Sourced by client.sh and
# launch.sh, never executed:   . "$KIT/lib.sh"

# gs_resolve_qml6 -> prints the path of a Qt 6 `qml` runtime, or returns 1
# after printing what it tried on stderr.
#
# `command -v qml` is NOT good enough: on a box with qt5-declarative installed
# (Moonlight-qt and friends pull it in) /usr/bin/qml is the Qt 5.15 runtime,
# which rejects the versionless `import QtQuick` in proto-shell.qml with
# "Library import requires a version" and exits. gamescope then presents no
# frames and the TV stays black. So the Qt 6 locations come first, and a bare
# `qml` is only accepted when its --version says 6.
#
#   TV_SHELL_GS_QML   explicit runtime path, used verbatim when executable
gs_resolve_qml6() {
    local tried=() c
    if [ -n "${TV_SHELL_GS_QML:-}" ]; then
        if [ -x "$TV_SHELL_GS_QML" ]; then
            printf '%s\n' "$TV_SHELL_GS_QML"
            return 0
        fi
        tried+=("TV_SHELL_GS_QML=$TV_SHELL_GS_QML (not executable)")
    fi
    for c in qml6 /usr/lib/qt6/bin/qml /usr/lib64/qt6/bin/qml; do
        case "$c" in
            /*) if [ -x "$c" ]; then printf '%s\n' "$c"; return 0; fi ;;
            *) if command -v "$c" >/dev/null 2>&1; then command -v "$c"; return 0; fi ;;
        esac
        tried+=("$c (absent)")
    done
    if c="$(command -v qml 2>/dev/null)"; then
        local v
        v="$(gs_qml_version "$c")"
        case "$v" in
            6.*) printf '%s\n' "$c"; return 0 ;;
            "") tried+=("$c (--version unreadable)") ;;
            *) tried+=("$c (Qt $v, not 6)") ;;
        esac
    else
        tried+=("qml (absent)")
    fi
    printf 'no Qt 6 qml runtime found; tried: %s\n' "$(IFS='; '; echo "${tried[*]}")" >&2
    return 1
}

# gs_qml_version <qml-binary> -> "6.11.2" / "5.15.19" / "" when unreadable.
# The probe runs on the offscreen platform: the runtime instantiates a
# QGuiApplication even for --version and aborts without a display.
gs_qml_version() {
    QT_QPA_PLATFORM=offscreen timeout 5 "$1" --version 2>/dev/null \
        | sed -n 's/^Qml Runtime \([0-9][0-9.]*\).*/\1/p' | head -1
}

# gs_moonlight_x11_env -> exports the environment Moonlight needs to present
# INSIDE gamescope on the X11 (xcb) path, which is the path that survives. SDL
# on x11 too, so the stream window is an X11 window gamescope's SteamControlled
# policy can select; the WSI layer reads GAMESCOPE_HDR_OUTPUT_FEEDBACK off the
# X11 root for HDR, so nothing is lost versus Wayland on that front.
# ENABLE_GAMESCOPE_WSI is what lets Moonlight present HDR at all: gamescope
# sets it for its own children but not for a launch.sh arriving over SSH, so it
# is set here for both callers.
#
# GAMESCOPE_WSI_FORCE_BYPASS is passed through when the caller set it: the WSI
# layer only exposes HDR10 formats when the window can bypass XWayland (matches
# its toplevel within 2 px), and forcing the bypass is the opt-in escape hatch
# for a log that says "hdr formats exposed to client: false" while the root
# atom GAMESCOPE_HDR_OUTPUT_FEEDBACK is 1.
#
# Shared by launch.sh (the SSH-side `moonlight` verb) and client.sh (the
# session's moonlight primary child) so the two can never drift.
gs_moonlight_x11_env() {
    export ENABLE_GAMESCOPE_WSI=1
    export QT_WAYLAND_DISABLE_WINDOWDECORATION=1
    export QT_QPA_PLATFORM=xcb
    export SDL_VIDEODRIVER=x11
    if [ -n "${GAMESCOPE_WSI_FORCE_BYPASS:-}" ]; then
        export GAMESCOPE_WSI_FORCE_BYPASS
    fi
}

# ---------------------------------------------------------------------------
# Launching an app inside its own systemd scope.
#
# gamescope's PRIMARY app identifier is the cgroup scope the client process
# sits in. Its only cgroup parser is
#
#     sscanf(cgroup, "app-steam-app%u-%d.scope", &appid, &pid)
#
# (`src/Utils/Process.cpp`), evaluated at window creation from the pid the X
# server reports for the client (XRes), not from anything the window carries.
# The `app-steam-app` prefix is an upstream contract — Steam's own name for a
# launched app's scope — and is not ours to rename. `docs/V2_DESIGN.md` §5
# states the same rule: **scope first, tag as repair, never by name.**
#
# This matters because post-hoc tagging alone STOPPED WORKING. Measured on the
# bench 2026-09-06, gamescope 3.16.28 (pinned up from 3.16.23 that day,
# jedwards1230/homelab-ansible#321):
#
#   launch                                    GAMESCOPE_FOCUSABLE_WINDOWS
#   ---------------------------------------   ---------------------------
#   plain launch, post-hoc tag attempted      (empty)
#   plain launch, control                     (empty)
#   inside app-steam-app9003-2970.scope       8388625, 9003, 2998
#
# The scoped launch worked with NO tagging at all — `STEAM_GAME` was never
# set — and the display went to `fps=120.000000 / focus=9003`. The unscoped
# ones produced no focus candidate, which is also a chicken-and-egg for the
# kit: `gs_tag_pid` DISCOVERS candidate windows through
# `GAMESCOPE_FOCUSABLE_WINDOWS`, so with that atom empty it can never find a
# window to tag. Tagging remains as the documented repair path for a window
# whose scope did not resolve (a pid namespace — Plex under `bwrap` — or a
# browser that handed off to an already-running instance), never as the
# primary mechanism.

# gs_scope_ready -> 0 when a scope can be created, 1 with a clear error on
# stderr otherwise. `systemd-run --user` talks to the caller's session bus, so
# XDG_RUNTIME_DIR and DBUS_SESSION_BUS_ADDRESS must both be present — over SSH
# neither usually is. DBUS_SESSION_BUS_ADDRESS is derived from the runtime dir
# only when that socket actually exists; there is deliberately NO fallback to
# an unscoped launch, because an unscoped launch is the broken case above and
# would fail silently at the far end.
#
# Call this in the FOREGROUND before backgrounding gs_scope_run: the derived
# DBUS_SESSION_BUS_ADDRESS export has to survive into the child, and an error
# has to reach the operator instead of a subshell that vanishes.
gs_scope_ready() {
    if ! command -v systemd-run >/dev/null 2>&1; then
        echo "gs_scope_run: no systemd-run on PATH; gamescope identifies an app by its cgroup scope, so a scope-less launch cannot be focused" >&2
        return 1
    fi
    if [ -z "${XDG_RUNTIME_DIR:-}" ]; then
        echo "gs_scope_run: XDG_RUNTIME_DIR is unset, so 'systemd-run --user' has no session bus to talk to" >&2
        echo "  source the session env file first (/tmp/tv-shell-gamescope.env), which carries both it and DBUS_SESSION_BUS_ADDRESS" >&2
        return 1
    fi
    if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
        if [ -S "$XDG_RUNTIME_DIR/bus" ]; then
            export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"
        else
            echo "gs_scope_run: DBUS_SESSION_BUS_ADDRESS is unset and $XDG_RUNTIME_DIR/bus is not a socket; 'systemd-run --user' has no session bus" >&2
            return 1
        fi
    fi
    return 0
}

# gs_scope_unit <outvar> <appid> <launcher-pid> -> assigns the unit name,
# WITHOUT the `.scope` suffix, which is the form `systemd-run --unit` takes.
# It assigns rather than prints so the one place the name is formatted can be
# called without a command substitution: `$(...)` forks a subshell, and
# $BASHPID inside that subshell is the SUBSHELL's pid, not the pid that is
# about to become the app.
gs_scope_unit() {
    printf -v "${1:?gs_scope_unit: outvar}" 'app-steam-app%s-%s' \
        "${2:?gs_scope_unit: appid}" "${3:?gs_scope_unit: launcher pid}"
}

# gs_scope_run <appid> <cmd...> -> EXEC <cmd> inside a transient
# `app-steam-app<appid>-<pid>.scope`, so gamescope resolves every window the
# command (or any child of it — a process family inherits the cgroup) creates
# to <appid> with no tagging at all.
#
# It execs on purpose. `systemd-run --scope` also execs, so the pid the caller
# captures with `$!` is the app's own pid the whole way down (verified): so
# `gs_tag_pid`'s pid matching, `--family` tree walks and a supervisor's `wait`
# all keep working unchanged. Backgrounding and redirection are the caller's
# job:
#
#     gs_scope_ready || exit 2
#     gs_scope_run 9003 nohup moonlight "$@" > "$LOG" 2>&1 &
#     PID=$!
#
# The unit is named after $BASHPID — the pid of the backgrounded subshell,
# which is the pid the app itself ends up with — so it is unique per launch and
# a supervisor relaunching its child can never collide with a scope whose
# processes have not been reaped yet. `--collect` also removes a scope that
# ended up failed, which nothing else would.
gs_scope_run() {
    local appid="${1:?gs_scope_run: appid}" unit
    shift
    [ $# -ge 1 ] || { echo "gs_scope_run: no command" >&2; return 2; }
    gs_scope_ready || return 1
    gs_scope_unit unit "$appid" "$BASHPID"
    exec systemd-run --user --scope --collect --quiet --unit="$unit" -- "$@"
}

# gs_scope_of <pid> -> the `app-steam-app<id>-<pid>.scope` unit the pid is in,
# or "" when it is in none (an unscoped launch, or a kernel too old for
# cgroup v2). Informational: it is what gamescope reads, so it is the one
# honest post-launch confirmation the kit can make without asking gamescope.
gs_scope_of() {
    sed -n 's,.*/\(app-steam-app[0-9]*-[0-9]*\.scope\)$,\1,p' "/proc/$1/cgroup" 2>/dev/null | head -1
}

# gs_scope_check <pid> <appid> -> report the scope <pid> actually ended up in.
# Returns 1 (and warns) only when the process is still alive and is NOT in a
# scope for <appid>: a launcher that has already exited (Steam's does) tells us
# nothing either way, and saying so is more honest than a warning that reads
# like a failure.
gs_scope_check() {
    local got
    got="$(gs_scope_of "$1")"
    case "$got" in
        app-steam-app"$2"-*)
            echo "scope: $got — gamescope resolves this app's windows to $2 with no tagging"
            return 0
            ;;
    esac
    if ! kill -0 "$1" 2>/dev/null; then
        echo "scope: pid $1 already exited; its scope holds whatever it spawned"
        return 0
    fi
    if [ -z "$got" ]; then
        echo "WARN: pid $1 is in no app-steam-app*.scope; gamescope cannot identify it by cgroup and the STEAM_GAME repair below is all there is" >&2
    else
        echo "WARN: pid $1 is in $got, which is not app $2" >&2
    fi
    return 1
}

# ---------------------------------------------------------------------------
# Tagging windows by pid — the REPAIR path (see gs_scope_run above).
#
# gamescope's SteamControlled focus policy only considers X11 windows that
# carry STEAM_GAME. Tagging "the window named X" (`xprop -name`) reaches ONE
# window, and for Moonlight it is the wrong one: its Qt main window, which
# `moonlight stream` unmaps once the session starts. The stream itself is a
# second X window (WM_NAME "<host> - Moonlight", WM_CLASS "moonlight",
# _NET_WM_PID = Moonlight's pid) that appears 5-20 s after launch, once the
# handshake is done. So the kit tags by pid: every window whose _NET_WM_PID is
# the client's pid (or whose WM_CLASS is a given class) gets STEAM_GAME, and
# the scan repeats once a second so windows created later are tagged as they
# appear.
#
# xprop is the only X client the kit relies on, and gamescope publishes no
# _NET_CLIENT_LIST (it is not among the atoms steamcompmgr.cpp sets), so no
# single call enumerates every window. Candidate xids come from four cheap
# sources instead:
#   1. root _NET_CLIENT_LIST, when a window manager offers one (not gamescope)
#   2. root GAMESCOPE_FOCUSABLE_WINDOWS (xid, appid, pid) triplets: windows
#      gamescope already knows, so a re-run finds what was tagged before
#   3. xids gamescope's Vulkan WSI layer logs into the client's own log
#      ("Creating Gamescope surface: xid: 0x..."), passed as --log <file>
#   4. neighbours: an X client allocates resource ids sequentially, so its
#      later windows sit just above its earlier ones; every known window of
#      the client seeds a probe of the next TV_SHELL_GS_XID_PROBE ids (default
#      32, 0 disables) on each poll
# plus WM_NAME lookups (--name <wm-name>), checked and tagged through
# `xprop -name` because xprop cannot print the xid of a window it found by
# name. Every candidate is kept only when its _NET_WM_PID is the pid (or its
# WM_CLASS carries --class), so a stale window with the right title but the
# wrong pid is never tagged.

# gs_win_props <xid> -> prints the raw xprop lines for _NET_WM_PID, WM_CLASS,
# WM_NAME and STEAM_GAME, or returns 1 when <xid> is not a window.
gs_win_props() {
    local out
    out="$(xprop -id "$1" _NET_WM_PID WM_CLASS WM_NAME STEAM_GAME 2>/dev/null)" || return 1
    [ -n "$out" ] || return 1
    printf '%s\n' "$out"
}

# gs_props_field <props> pid|class|name|game -> the value, or "" when absent.
gs_props_field() {
    case "$2" in
        pid) printf '%s\n' "$1" | sed -n 's/^_NET_WM_PID(CARDINAL) = \([0-9]*\).*/\1/p' | head -1 ;;
        class) printf '%s\n' "$1" | sed -n 's/^WM_CLASS([^)]*) = \(.*\)$/\1/p' | head -1 ;;
        name) printf '%s\n' "$1" | sed -n 's/^WM_NAME([^)]*) = "\(.*\)"$/\1/p' | head -1 ;;
        game) printf '%s\n' "$1" | sed -n 's/^STEAM_GAME(CARDINAL) = \([0-9]*\).*/\1/p' | head -1 ;;
    esac
}

# gs_props_match <props> "<pid>..." "<class>..." -> 0 when the window's
# _NET_WM_PID is one of the pids, or its WM_CLASS carries one of the classes
# (both lists space-separated, either may be empty).
gs_props_match() {
    local wpid wclass p c
    wpid="$(gs_props_field "$1" pid)"
    if [ -n "$wpid" ]; then
        for p in $2; do [ "$wpid" = "$p" ] && return 0; done
    fi
    if [ -n "$3" ]; then
        wclass="$(gs_props_field "$1" class)"
        for c in $3; do
            case "$wclass" in *"\"$c\""*) return 0 ;; esac
        done
    fi
    return 1
}

# gs_pid_family <pid> -> <pid> and every live descendant, one per line
# (breadth-first over `pgrep -P`). Steam is a family: the launcher script, the
# client, steamwebhelper, and the separate streaming_client that a Remote Play
# stream spawns, each with its own X connection and windows.
gs_pid_family() {
    local queue=("$1") p k
    printf '%s\n' "$1"
    while [ ${#queue[@]} -gt 0 ]; do
        p="${queue[0]}"; queue=("${queue[@]:1}")
        for k in $(pgrep -P "$p" 2>/dev/null); do
            printf '%s\n' "$k"
            queue+=("$k")
        done
    done
}

# gs_root_candidates "<pid>..." -> hex xids from _NET_CLIENT_LIST (all of
# them) and from the GAMESCOPE_FOCUSABLE_WINDOWS triplets whose pid is one of
# the given pids.
gs_root_candidates() {
    local out line vals i n arr=()
    out="$(xprop -root _NET_CLIENT_LIST GAMESCOPE_FOCUSABLE_WINDOWS 2>/dev/null)" || return 0
    line="$(printf '%s\n' "$out" | sed -n 's/^_NET_CLIENT_LIST(WINDOW): window id # //p')"
    if [ -n "$line" ]; then
        printf '%s\n' "$line" | tr ',' '\n' | tr -d ' ' | grep -E '^0x[0-9a-fA-F]+$' || true
    fi
    vals="$(printf '%s\n' "$out" | sed -n 's/^GAMESCOPE_FOCUSABLE_WINDOWS(CARDINAL) = //p' | tr -d ' ')"
    [ -n "$vals" ] || return 0
    IFS=',' read -r -a arr <<< "$vals"
    n=${#arr[@]}
    i=0
    local p
    while [ $((i + 2)) -lt "$n" ]; do
        for p in $1; do
            if [ "${arr[$((i + 2))]}" = "$p" ]; then
                printf '0x%x\n' "${arr[$i]}"
                break
            fi
        done
        i=$((i + 3))
    done
}

# gs_log_candidates <file>... -> hex xids the gamescope WSI layer logged.
gs_log_candidates() {
    local f
    for f in "$@"; do
        [ -r "$f" ] || continue
        grep -o 'Creating Gamescope surface: xid: 0x[0-9a-fA-F]*' "$f" 2>/dev/null | awk '{ print $NF }'
    done
}

# gs_tag_pid <pid> <appid> [options] -> tags every X window of <pid> with
# STEAM_GAME=<appid>, re-scanning every TV_SHELL_GS_POLL_SECS (1) seconds until
# --timeout (60 s), --expect windows are tagged, or a tagged window's WM_NAME
# matches --done-name. Prints one line per window as it is tagged. A window
# that already carries STEAM_GAME=<appid> is reported as "known" (and satisfies
# --done-name) but is never counted: only tags made by this run count toward
# --expect, so a window reached by both a name lookup and an xid cannot be
# counted twice. Returns 0 when at least one window was tagged or known, 1
# otherwise, and 1 when <pid> exits before any window of it is found.
#
#   --timeout <s>       give up after this long (default 60)
#   --class <wm-class>  also accept windows whose WM_CLASS carries this (repeatable)
#   --family            <pid> means <pid> and every descendant, re-read each poll
#                       (a Steam family: steam, steamwebhelper, streaming_client);
#                       the watch ends only when the whole family is gone
#   --keep-existing     a window that already carries a DIFFERENT STEAM_GAME is
#                       reported and left alone instead of re-tagged (Steam tags
#                       its own windows; overwriting that is the fight we measure)
#   --log <file>        harvest xids from this WSI-layer log (repeatable)
#   --name <wm-name>    also look a window up by WM_NAME (repeatable)
#   --expect <n>        stop once <n> windows are tagged
#   --done-name <glob>  stop once a tagged window's WM_NAME matches <glob>
gs_tag_pid() {
    local pid="${1:?gs_tag_pid: pid}" appid="${2:?gs_tag_pid: appid}"
    shift 2
    local timeout=60 classes="" family="" keep="" expect=0 done_glob="" logs=() names=()
    while [ $# -gt 0 ]; do
        case "$1" in
            --timeout) timeout="$2"; shift 2 ;;
            --class) classes="${classes:+$classes }$2"; shift 2 ;;
            --family) family=1; shift ;;
            --keep-existing) keep=1; shift ;;
            --log) logs+=("$2"); shift 2 ;;
            --name) names+=("$2"); shift 2 ;;
            --expect) expect="$2"; shift 2 ;;
            --done-name) done_glob="$2"; shift 2 ;;
            *) echo "gs_tag_pid: unknown option '$1'" >&2; return 2 ;;
        esac
    done
    local poll="${TV_SHELL_GS_POLL_SECS:-1}" probe="${TV_SHELL_GS_XID_PROBE:-32}"
    local -A seen=() maxid=() name_done=()
    local start=$SECONDS tagged=0 known=0 finished="" c props wname base cands n wgame
    local pids="$pid" alive p

    # gs_tag_pid_refresh_family -> pids = the root pid plus every descendant,
    # plus earlier members that are still alive even if the root has gone
    # (a launcher that exits after spawning the client).
    gs_tag_pid_refresh_family() {
        local next="" seenp=" "
        for p in $(gs_pid_family "$pid") $pids; do
            case "$seenp" in *" $p "*) continue ;; esac
            kill -0 "$p" 2>/dev/null || continue
            seenp="$seenp$p "
            next="${next:+$next }$p"
        done
        pids="$next"
    }

    # gs_tag_pid_note <wm-name> tagged|known -> counts a hit and applies the
    # stop conditions (--done-name on either kind, --expect on new tags only).
    gs_tag_pid_note() {
        if [ "$2" = tagged ]; then tagged=$((tagged + 1)); else known=$((known + 1)); fi
        # shellcheck disable=SC2254  # a glob is what --done-name takes
        case "$1" in $done_glob) [ -n "$done_glob" ] && finished=1 ;; esac
        [ "$expect" -gt 0 ] && [ "$tagged" -ge "$expect" ] && finished=1
        return 0
    }

    # gs_tag_pid_consider <xid> -> checks one xid once, tags it when it matches.
    gs_tag_pid_consider() {
        local xid="$1" xprops wgame wn
        [ -z "${seen[$xid]:-}" ] || return 0
        xprops="$(gs_win_props "$xid")" || return 0
        seen[$xid]=window
        base=$(( xid & ~0x1FFFFF ))
        if [ -z "${maxid[$base]:-}" ] || [ $((xid)) -gt "${maxid[$base]}" ]; then
            maxid[$base]=$((xid))
        fi
        gs_props_match "$xprops" "$pids" "$classes" || return 0
        wn="$(gs_props_field "$xprops" name)"
        wgame="$(gs_props_field "$xprops" game)"
        if [ "$wgame" = "$appid" ]; then
            seen[$xid]=tagged
            echo "known $xid \"$wn\" (already STEAM_GAME=$appid)"
            gs_tag_pid_note "$wn" known
        elif [ -n "$keep" ] && [ -n "$wgame" ]; then
            seen[$xid]=tagged
            echo "known $xid \"$wn\" (has STEAM_GAME=$wgame, left alone)"
            gs_tag_pid_note "$wn" known
        else
            xprop -id "$xid" -f STEAM_GAME 32c -set STEAM_GAME "$appid" 2>/dev/null || return 0
            seen[$xid]=tagged
            echo "tagged $xid \"$wn\" STEAM_GAME=$appid (t+$((SECONDS - start))s)"
            gs_tag_pid_note "$wn" tagged
        fi
        return 0
    }

    while :; do
        alive=""
        if [ -n "$family" ]; then
            gs_tag_pid_refresh_family
            [ -n "$pids" ] && alive=1
        else
            kill -0 "$pid" 2>/dev/null && alive=1
        fi
        if [ -z "$alive" ]; then
            if [ $((tagged + known)) -eq 0 ]; then
                echo "gs_tag_pid: pid $pid exited before any window of it appeared" >&2
                return 1
            fi
            echo "gs_tag_pid: pid $pid exited; $tagged window(s) had been tagged, $known already tagged"
            return 0
        fi
        cands=()
        while IFS= read -r c; do
            [ -n "$c" ] && cands+=("$c")
        done < <(gs_root_candidates "$pids"; gs_log_candidates "${logs[@]}")
        for c in "${cands[@]}"; do gs_tag_pid_consider "$c"; done
        for base in "${!maxid[@]}"; do
            n=1
            while [ "$n" -le "$probe" ]; do
                # maxid is re-read on every step, so a hit extends the window
                c="$(printf '0x%x' $((maxid[$base] + n)))"
                [ -n "${seen[$c]:-}" ] || gs_tag_pid_consider "$c"
                n=$((n + 1))
            done
        done
        for wname in "${names[@]}"; do
            [ -z "${name_done[$wname]:-}" ] || continue
            props="$(xprop -name "$wname" _NET_WM_PID WM_CLASS WM_NAME STEAM_GAME 2>/dev/null)" || continue
            gs_props_match "$props" "$pids" "$classes" || continue
            name_done[$wname]=1
            wgame="$(gs_props_field "$props" game)"
            if [ "$wgame" = "$appid" ]; then
                echo "known '$wname' (already STEAM_GAME=$appid)"
                gs_tag_pid_note "$wname" known
            elif [ -n "$keep" ] && [ -n "$wgame" ]; then
                echo "known '$wname' (has STEAM_GAME=$wgame, left alone)"
                gs_tag_pid_note "$wname" known
            else
                xprop -name "$wname" -f STEAM_GAME 32c -set STEAM_GAME "$appid" 2>/dev/null || continue
                echo "tagged '$wname' STEAM_GAME=$appid (t+$((SECONDS - start))s)"
                gs_tag_pid_note "$wname" tagged
            fi
        done
        [ -n "$finished" ] && return 0
        if [ $((SECONDS - start)) -ge "$timeout" ]; then
            if [ $((tagged + known)) -eq 0 ]; then
                echo "gs_tag_pid: no window of pid $pid appeared within ${timeout}s; nothing tagged" >&2
                return 1
            fi
            echo "gs_tag_pid: watch ended after ${timeout}s; $tagged window(s) tagged, $known already tagged"
            return 0
        fi
        sleep "$poll"
    done
}

# gs_watch_baselayer [secs] -> logs every change of GAMESCOPECTRL_BASELAYER_APPID
# and GAMESCOPE_FOCUSED_APP with a timestamp, one line per change, until
# <secs> elapse (default 600; 0 = forever). Exists because the full Steam
# client writes the base-layer atom itself when it starts a stream (the
# SteamOS mechanism), so whether it fights the kit's own `focus.sh app` is a
# thing to record, not assume. Polls every TV_SHELL_GS_WATCH_SECS (0.5).
gs_watch_baselayer() {
    local secs="${1:-600}" every="${TV_SHELL_GS_WATCH_SECS:-0.5}" start=$SECONDS
    local out cur prev=""
    echo "$(date '+%H:%M:%S.%N' | cut -c1-12) watching GAMESCOPECTRL_BASELAYER_APPID / GAMESCOPE_FOCUSED_APP for ${secs}s"
    while :; do
        out="$(xprop -root GAMESCOPECTRL_BASELAYER_APPID GAMESCOPECTRL_BASELAYER_WINDOW GAMESCOPE_FOCUSED_APP 2>/dev/null)"
        cur="$(printf '%s\n' "$out" | sed -n 's/^GAMESCOPECTRL_BASELAYER_APPID(CARDINAL) = //p' | head -1)"
        cur="baselayer=[${cur:-unset}] window=[$(printf '%s\n' "$out" | sed -n 's/^GAMESCOPECTRL_BASELAYER_WINDOW(CARDINAL) = //p' | head -1)] focused=[$(printf '%s\n' "$out" | sed -n 's/^GAMESCOPE_FOCUSED_APP(CARDINAL) = //p' | head -1)]"
        if [ "$cur" != "$prev" ]; then
            echo "$(date '+%H:%M:%S.%N' | cut -c1-12) $cur"
            prev="$cur"
        fi
        if [ "$secs" -gt 0 ] && [ $((SECONDS - start)) -ge "$secs" ]; then
            echo "$(date '+%H:%M:%S.%N' | cut -c1-12) watch ended after ${secs}s"
            return 0
        fi
        sleep "$every"
    done
}
