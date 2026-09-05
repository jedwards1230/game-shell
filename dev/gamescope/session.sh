#!/bin/bash
# tv-shell gamescope PROTOTYPE session.
#
# A selectable Wayland session (see README.md) that boots gamescope on the DRM
# backend instead of Hyprland, with the tv-shell input daemon running beside it,
# and runs a deliberately tiny "prototype shell" as gamescope's primary child.
# It exists to MEASURE, not to be used: the questions it answers are listed in
# README.md ("Pass/fail criteria"). Nothing here touches the real tv-shell
# session, its units, or its config.
#
# Tunables (all optional, environment):
#   TV_SHELL_DIR            install root                      (default /opt/tv-shell)
#   TV_SHELL_GS_WIDTH/HEIGHT/REFRESH  output mode requested   (3840 / 2160 / 120)
#   TV_SHELL_GS_HDR         1 = --hdr-enabled                 (1)
#   TV_SHELL_GS_VRR         1 = --adaptive-sync               (1)
#   TV_SHELL_GS_SDR_NITS    --hdr-sdr-content-nits            (200)
#   TV_SHELL_GS_DAEMON      1 = start tv-shell-input.service  (1)
#   TV_SHELL_GS_EXTRA       extra gamescope args, word-split  ("")
#
# Logs: journal tag `tv-shell-gamescope` plus /tmp/tv-shell-gamescope.log.
# Stats: /tmp/tv-shell-gamescope-stats (gamescope --stats-path).
set -u

SHELL_DIR="${TV_SHELL_DIR:-/opt/tv-shell}"
KIT="$SHELL_DIR/dev/gamescope"
export TV_SHELL_DIR="$SHELL_DIR"
TV_SHELL_SOCK="/run/user/$(id -u)/tv-shell-input.sock"
export TV_SHELL_SOCK
export XDG_CURRENT_DESKTOP=gamescope
export PATH="$SHELL_DIR/scripts:$PATH"

LOG=/tmp/tv-shell-gamescope.log
STATS=/tmp/tv-shell-gamescope-stats
ENV_FILE=/tmp/tv-shell-gamescope.env

W="${TV_SHELL_GS_WIDTH:-3840}"
H="${TV_SHELL_GS_HEIGHT:-2160}"
R="${TV_SHELL_GS_REFRESH:-120}"
HDR="${TV_SHELL_GS_HDR:-1}"
VRR="${TV_SHELL_GS_VRR:-1}"
NITS="${TV_SHELL_GS_SDR_NITS:-200}"
DAEMON="${TV_SHELL_GS_DAEMON:-1}"

log() { printf 'tv-shell-gamescope: %s\n' "$*"; }

# Everything this script and gamescope print goes to the journal AND a file,
# so a session that dies before the shell appears still leaves evidence. The
# file is the floor: without systemd-cat (non-systemd launch) it is the only log.
if command -v systemd-cat >/dev/null 2>&1; then
    exec > >(tee -a "$LOG" | systemd-cat -t tv-shell-gamescope) 2>&1
else
    exec > >(tee -a "$LOG") 2>&1
fi

rm -f "$ENV_FILE" "$STATS"
log "starting: ${W}x${H}@${R} hdr=$HDR vrr=$VRR sdr_nits=$NITS daemon=$DAEMON"

STARTED_DAEMON=0
if [ "$DAEMON" = "1" ] && command -v systemctl >/dev/null 2>&1 \
    && systemctl --user show-environment >/dev/null 2>&1; then
    # Same unit the real session uses, so gamepad -> uinput nav keys work in the
    # prototype shell. The daemon's Hyprland actor will find no compositor and
    # log "event listener is DEAF" on every retry; that is expected here and is
    # one of the things a v2 core would stop doing.
    systemctl --user reset-failed tv-shell-input.service >/dev/null 2>&1 || true
    if systemctl --user is-active --quiet tv-shell-input.service; then
        log "tv-shell-input.service already active; leaving it"
    elif systemctl --user start tv-shell-input.service; then
        STARTED_DAEMON=1
        log "started tv-shell-input.service"
    else
        log "WARN: tv-shell-input.service failed to start; continuing without gamepad"
    fi
fi

# shellcheck disable=SC2329 # invoked via the EXIT trap below
cleanup() {
    log "session ending"
    if [ "$STARTED_DAEMON" = "1" ]; then
        systemctl --user stop tv-shell-input.service >/dev/null 2>&1 || true
    fi
    rm -f "$ENV_FILE"
}
trap cleanup EXIT

ARGS=(
    --backend drm
    # -e/--steam selects the SteamControlled focus strategy. Without it the
    # GAMESCOPECTRL_BASELAYER_* root atoms are ignored and there is no external
    # focus control at all, which is the property this prototype exists to test.
    --steam
    # xdg-shell for Wayland-native clients (Moonlight, Quickshell FloatingWindow).
    --expose-wayland
    # gamescope normally exits with its primary child; the shell must survive a
    # child crash and the supervisor must own restarts.
    --keep-alive
    # Output mode is matched EXACTLY against the EDID mode list; a mismatch falls
    # back to the first listed mode, which measure.sh will catch.
    -W "$W" -H "$H" -r "$R"
    # Client (nested) size equals the output so nothing is scaled.
    -w "$W" -h "$H"
    --stats-path "$STATS"
    --hide-cursor-delay 3000
)
if [ "$HDR" = "1" ]; then
    ARGS+=(--hdr-enabled --hdr-sdr-content-nits "$NITS")
fi
if [ "$VRR" = "1" ]; then
    ARGS+=(--adaptive-sync)
fi
if [ -n "${TV_SHELL_GS_EXTRA:-}" ]; then
    # shellcheck disable=SC2206 # word-splitting is the documented contract
    ARGS+=(${TV_SHELL_GS_EXTRA})
fi

log "exec: gamescope ${ARGS[*]} -- $KIT/client.sh"
export TV_SHELL_GS_ENV_FILE="$ENV_FILE"
gamescope "${ARGS[@]}" -- "$KIT/client.sh"
rc=$?
log "gamescope exited rc=$rc"
exit "$rc"
