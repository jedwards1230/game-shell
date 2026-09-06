#!/bin/bash
# tv-shell v2 session script — launched by the display manager via
# tv-shell-v2.desktop (installed by scripts/install-v2.sh; a third name, since v1
# owns tv-shell-wayland.desktop and the Ansible measurement prototype owns
# tv-shell-gamescope.desktop).
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
# THE PREFIX IS RESOLVED AT RUNTIME, NOT REWRITTEN AT INSTALL.
#
# This used to be a hard-coded /opt/tv-shell path with a comment claiming
# scripts/install.sh rewrote it. It did not — install.sh had no reference to
# core/ at all — so the shipped script pointed at v1's prefix, which is both the
# hardcode CLAUDE.md forbids and a path that would have run v1's tree.
#
# scripts/install-v2.sh installs this script and the core binary side by side in
# <prefix>/bin/, so the script's own directory IS the prefix's bin dir. Deriving
# it needs no substitution and cannot go stale: unlike the units (systemd cannot
# resolve a path at runtime, so those carry a @TV_SHELL_V2_PREFIX@ token the
# installer substitutes), a script can just look at itself.
SESSION_BIN_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
CORE_BIN="${TV_SHELL_CORE_BIN:-$SESSION_BIN_DIR/tv-shell-core}"

# The v1 units this session must exclude. Held in one place because the stop,
# the mask and the unmask all have to agree.
V1_UNITS=(
    tv-shell-input.service
    tv-shell-quickshell.service
    # v1's panel too: left running it keeps serving on the LAN while dialling a
    # socket with no daemon behind it, so the recovery surface looks broken
    # rather than saying "v2 is running".
    tv-shell-panel.service
)

log() { printf 'tv-shell-session: %s\n' "$*" >&2; }

# EXPORTING THESE IS NOT ENOUGH, AND USED TO BE ALL THAT HAPPENED.
#
# This script starts nothing directly — everything is a unit — and units inherit
# the USER MANAGER's environment, not this shell's. So a bare `export` here was
# dead code: anything keying on XDG_CURRENT_DESKTOP=gamescope silently took its
# non-gamescope path. `import-environment` pushes them into the user manager (so
# units see them) and `dbus-update-activation-environment` into the D-Bus
# activation environment (so D-Bus-activated services do too), which is what
# ChimeraOS's session script does for the same reason.
export XDG_CURRENT_DESKTOP=gamescope
export XDG_SESSION_TYPE=wayland
systemctl --user import-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE \
    >/dev/null 2>&1 || log "WARN: could not import the session environment"
if command -v dbus-update-activation-environment >/dev/null 2>&1; then
    dbus-update-activation-environment --systemd XDG_CURRENT_DESKTOP XDG_SESSION_TYPE \
        >/dev/null 2>&1 || log "WARN: could not update the D-Bus activation environment"
fi

# IDEMPOTENT: it can run from the trap AND again on the way out, and both
# orderings have to be safe. Every command here is already a no-op when its
# effect is absent.
cleanup() {
    log "session ending; stopping the target and cleaning up"
    systemctl --user stop "$TARGET" >/dev/null 2>&1 || true
    # Give v1 back. A runtime mask cannot outlive the user manager, so this is
    # belt-and-braces rather than the only thing standing between a bad exit and
    # an unstartable v1.
    systemctl --user unmask --runtime "${V1_UNITS[@]}" >/dev/null 2>&1 || true
    rm -f "$STATS_FIFO" "$ENV_FILE" "$MODE_FILE"
}

# EXIT ALONE WAS NOT ENOUGH, AND THE GAP COST A TRIP TO THE TELEVISION.
#
# This script blocks in a foreground `systemctl start --wait`. On the display
# manager's SIGTERM, bash with no TERM handler dies without running an EXIT trap
# — so cleanup never ran, the target was never stopped, and gamescope kept DRM
# master. The greeter could not get the seat back, which means §11's "select the
# v1 session at the display manager" rollback was unreachable, killed by the very
# signal meant to trigger it.
trap cleanup EXIT INT TERM HUP

# 1. Stale state from a previous session.
#
# `stop` is best-effort: there may be nothing to stop, which is the normal case
# and not an error. `reset-failed` matters more than it looks — a unit left in
# `failed` with its start-limit hit will refuse to start again, and the symptom
# is a session that dies instantly with no obvious cause.
# `reset-failed` is SCOPED to the units this session owns. Unscoped, it clears
# failed state for every user unit on the box, v1's included — which both hides
# v1 failures an operator was about to look at and is half of why a start limiter
# on these units could never accumulate.
log "clearing any stale session state"
systemctl --user stop "$TARGET" >/dev/null 2>&1 || true
systemctl --user reset-failed \
    "$TARGET" tv-shell-gamescope.service tv-shell-core.service \
    'tv-shell-v2-*.service' >/dev/null 2>&1 || true

# 1b. v1 exclusion, ONE-DIRECTIONALLY.
#
# The units used to carry `Conflicts=tv-shell-input.service`, which is
# bidirectional: the Ansible CEC watchdog restarting tv-shell-input on a bad
# `cec-health` reading (§9, jedwards1230/homelab-ansible#266) would have issued a
# start job that STOPPED the v2 session target — a black screen mid-game caused
# by a watchdog acting on a misreading. Stopping v1 from here is the same
# exclusion without the return path.
#
# `mask --runtime` is what makes it hold: a masked unit CANNOT be started, so the
# watchdog's restart fails loudly and harmlessly instead of tearing down a live
# session. Runtime masks live under /run and vanish with the user manager, so a
# v2 session that dies without running its trap can never leave v1 unstartable.
#
# This is a mitigation. §9 requires the watchdog itself to stand down at cutover,
# and that work is still unfiled on the Ansible side.
log "stopping and runtime-masking the v1 units"
systemctl --user stop "${V1_UNITS[@]}" >/dev/null 2>&1 || true
systemctl --user mask --runtime "${V1_UNITS[@]}" >/dev/null 2>&1 \
    || log "WARN: could not mask the v1 units; a stray start could disrupt this session"

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
