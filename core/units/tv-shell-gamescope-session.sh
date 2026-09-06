#!/bin/bash
# tv-shell v2 session script — launched by the display manager via
# tv-shell-gamescope.desktop.
#
# UNTESTED ON HARDWARE. This follows docs/V2_DESIGN.md §4 and the shape of the
# ChimeraOS/SteamOS `gamescope-session` script it is derived from, but it has
# not been booted. Review it as a proposal.
#
# It does four things, in order, and then blocks:
#
#   1. Stop any stale copy of the session target and clear failed state, so a
#      previous session that ended badly cannot leave units in a state that
#      makes this one fail for reasons that have nothing to do with this boot.
#   2. Create the stats FIFO BEFORE the target starts, so gamescope's
#      `--stats-path` finds it already present rather than racing its creation,
#      and render the output mode from core.toml into the env file the gamescope
#      unit reads.
#   3. `systemctl --user start --wait tv-shell-session.target`. The `--wait` is
#      what makes "gamescope dies → the session exits" true: the target is
#      BindsTo= the compositor, so the compositor's death stops the target,
#      which returns this script, which ends the session and hands control back
#      to the display manager (autologin Relogin=true restarts it).
#   4. Clean up on the way out.
#
# It deliberately starts NOTHING directly. Every process is a unit under the
# target, so journald capture, cgroup accounting, restart policy and the §9
# escape hatches all come for free and there is exactly one supervisor.
set -euo pipefail

TARGET=tv-shell-session.target
RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
STATS_FIFO="$RUNTIME_DIR/tv-shell-gamescope-stats"
ENV_FILE="$RUNTIME_DIR/tv-shell-gamescope-environment"
MODE_FILE="$RUNTIME_DIR/tv-shell-gamescope-mode"
# Rewrite this path to the resolved install prefix, as scripts/install.sh does
# for the v1 units.
CORE_BIN="${TV_SHELL_CORE_BIN:-/opt/tv-shell/bin/tv-shell-core}"

export XDG_CURRENT_DESKTOP=gamescope
export XDG_SESSION_TYPE=wayland

log() { printf 'tv-shell-session: %s\n' "$*" >&2; }

cleanup() {
    log "session target returned; stopping and cleaning up"
    systemctl --user stop "$TARGET" >/dev/null 2>&1 || true
    rm -f "$STATS_FIFO" "$ENV_FILE" "$MODE_FILE"
}
trap cleanup EXIT

# 1. Stale state from a previous session.
#
# `stop` is best-effort: there may be nothing to stop, which is the normal case
# and not an error. `reset-failed` matters more than it looks — a unit left in
# `failed` with its start-limit hit will refuse to start again, and the symptom
# is a session that dies instantly with no obvious cause.
log "clearing any stale session state"
systemctl --user stop "$TARGET" >/dev/null 2>&1 || true
systemctl --user reset-failed >/dev/null 2>&1 || true

# 2. The stats FIFO, created before anything can open it.
#
# gamescope's `--stats-path` open()s an EXISTING FIFO and never creates one; a
# missing file just makes it retry every 10 s and the stats never appear
# (dev/gamescope/session.sh:86-89, the invocation the week-long live measurement
# ran). A leftover regular file — from a crashed session that wrote where a FIFO
# should be — would silently swallow the stats stream instead. So remove and
# recreate unconditionally.
#
# There is no ready FIFO: the READY=1 handshake is the child script's job, since
# it has to happen AFTER the environment is published. See the comment on
# tv-shell-gamescope.service's ExecStart.
rm -f "$STATS_FIFO"
mkfifo -m 0600 "$STATS_FIFO"

# The environment file is written by gamescope's child script. Remove any
# stale copy so a unit reading it can never pick up a previous session's
# DISPLAY — which would connect it to an X server that no longer exists.
rm -f "$ENV_FILE"

# 2b. The output mode, rendered from ~/.config/tv-shell/core.toml.
#
# THIS IS THE LINK THAT MAKES [display] A REAL SETTING. The gamescope unit used
# to hard-code -W 3840 -H 2160 -r 120 while core.toml documented those keys as
# "read by the unit's ExecStart"; they were not, so setting refresh = 60 was
# accepted by validate() and changed nothing. `write-session-env` renders the
# config into KEY=value lines, the unit reads them with a REQUIRED
# EnvironmentFile= and substitutes them.
#
# Not `|| true`: the unit's EnvironmentFile has no leading `-`, so a missing or
# stale file fails the compositor's start loudly rather than booting it at some
# other mode. Failing here says WHY (a bad core.toml names its own bad key);
# failing there would only say the file is missing. `set -e` carries it out.
rm -f "$MODE_FILE"
log "rendering the output mode into $MODE_FILE"
"$CORE_BIN" write-session-env "$MODE_FILE"

# 3. Start and block.
#
# `--wait` returns when the target stops, for any reason. Note that a failed
# start also returns here, which is what we want: the display manager gets
# control back and the operator can select the v1 session (§11 rollback).
log "starting $TARGET"
systemctl --user start --wait "$TARGET"

log "$TARGET stopped"
