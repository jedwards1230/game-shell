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
