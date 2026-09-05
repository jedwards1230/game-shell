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

# ---------------------------------------------------------------------------
# Tagging windows by pid.
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
