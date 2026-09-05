#!/bin/bash
# Drive gamescope's focus policy from outside, the way Steam does: root-window
# X11 atoms on gamescope's Xwayland display. Only works when gamescope runs with
# --steam (SteamControlled strategy), which session.sh passes.
#
#   focus.sh list                 show focusable apps/windows and the current focus
#   focus.sh tag <wm-name> <id>   set STEAM_GAME=<id> on the ONE window titled <wm-name>
#   focus.sh tag-pid <pid> <id> [opts]
#                                 set STEAM_GAME=<id> on EVERY window of <pid>, and keep
#                                 watching for new ones (default 60 s; see lib.sh
#                                 gs_tag_pid for --timeout/--class/--log/--name/
#                                 --expect/--done-name). This is what a multi-window
#                                 client such as Moonlight needs: its stream window
#                                 is not the window named "Moonlight"
#   focus.sh app <id>[,<id>...]   base layer = first running app in this priority list
#   focus.sh window <xid>         base layer = this exact X window
#   focus.sh clear                remove the base-layer overrides
#
# Run from inside the session, or from SSH after `source /tmp/tv-shell-gamescope.env`.
set -eu

KIT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=dev/gamescope/lib.sh
. "$KIT/lib.sh"
ENV_FILE="${TV_SHELL_GS_ENV_FILE:-/tmp/tv-shell-gamescope.env}"
if [ -z "${DISPLAY:-}" ] && [ -r "$ENV_FILE" ]; then
    # shellcheck source=/dev/null
    . "$ENV_FILE"
fi
if [ -z "${DISPLAY:-}" ]; then
    echo "focus.sh: no DISPLAY and no $ENV_FILE; is the gamescope session running?" >&2
    exit 2
fi
command -v xprop >/dev/null 2>&1 || { echo "focus.sh: xprop not installed (xorg-xprop)" >&2; exit 2; }

usage() { sed -n '2,19p' "$0"; exit 2; }

case "${1:-}" in
    list)
        xprop -root GAMESCOPE_FOCUSABLE_APPS GAMESCOPE_FOCUSABLE_WINDOWS \
            GAMESCOPE_FOCUSED_APP GAMESCOPE_FOCUSED_WINDOW \
            GAMESCOPECTRL_BASELAYER_APPID GAMESCOPECTRL_BASELAYER_WINDOW 2>&1
        ;;
    tag)
        [ $# -eq 3 ] || usage
        xprop -name "$2" -f STEAM_GAME 32c -set STEAM_GAME "$3"
        ;;
    tag-pid)
        [ $# -ge 3 ] || usage
        PID="$2"; APPID="$3"; shift 3
        gs_tag_pid "$PID" "$APPID" "$@"
        ;;
    app)
        [ $# -eq 2 ] || usage
        xprop -root -f GAMESCOPECTRL_BASELAYER_APPID 32c -set GAMESCOPECTRL_BASELAYER_APPID "$2"
        ;;
    window)
        [ $# -eq 2 ] || usage
        xprop -root -f GAMESCOPECTRL_BASELAYER_WINDOW 32c -set GAMESCOPECTRL_BASELAYER_WINDOW "$2"
        ;;
    clear)
        xprop -root -remove GAMESCOPECTRL_BASELAYER_APPID || true
        xprop -root -remove GAMESCOPECTRL_BASELAYER_WINDOW || true
        ;;
    *)
        usage
        ;;
esac
