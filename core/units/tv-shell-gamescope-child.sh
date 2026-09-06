#!/bin/bash
# gamescope's primary child for the tv-shell v2 session.
#
# UNTESTED ON HARDWARE — see the header in tv-shell-session.target.
#
# WHY THIS SCRIPT EXISTS AT ALL: THE ENVIRONMENT HAS TO BE DUMPED FROM INSIDE.
#
#   gamescope sets WAYLAND_DISPLAY / DISPLAY / GAMESCOPE_WAYLAND_DISPLAY for the
#   children it spawns — not for its own parent, and not for a systemd
#   `ExecStartPost=` shell, which is a sibling process with the unit's ambient
#   environment and nothing of gamescope's. Dumping from there publishes an
#   EMPTY file, `EnvironmentFile=-` accepts an empty file silently, and the core
#   then connects to whatever ambient $DISPLAY happens to exist. That is the
#   "infer, don't declare" failure class V2_DESIGN §3 exists to kill: every unit
#   in the session would be talking to a server nobody declared.
#
#   This script runs as gamescope's child, so the environment it reads IS the
#   one gamescope hands its children. It publishes that, and only then does it
#   announce readiness — so `READY=1` means "the environment file exists and is
#   complete", which is the precondition every unit ordered after
#   graphical-session.target relies on.
#
# THE `sleep infinity` AT THE END IS A PLACEHOLDER, AND IS NOT FINISHED WORK.
#
#   gamescope exits when its primary child exits, so it must have one that
#   stays alive. §4's topology wants the real session child in this slot, and
#   WHICH process that should be is not settled in this PR (the shell runtime is
#   §13 Q1, still open). `sleep infinity` holds the slot honestly: it keeps
#   gamescope alive and does nothing else. Replace it — do not build on it.
set -euo pipefail

: "${TV_SHELL_ENV_FILE:?TV_SHELL_ENV_FILE must be set by the unit}"

# Publish the compositor environment ATOMICALLY: write a temp file in the same
# directory, then rename. A reader (`EnvironmentFile=` in another unit) can then
# only ever see the whole file or no file, never a half-written one — and a
# half-written one is unfalsifiable, because a truncated `KEY=` line parses.
env_dir="$(dirname -- "$TV_SHELL_ENV_FILE")"
tmp="$(mktemp "$env_dir/.tv-shell-gamescope-environment.XXXXXX")"
trap 'rm -f -- "$tmp"' EXIT

# REQUIRED vs OPTIONAL, and why the distinction has to exist here.
#
# This loop used to write whichever of the four happened to be set and then
# announce readiness unconditionally — so a file missing DISPLAY passed as
# "complete", the core's EnvironmentFile picked up nothing, and the core
# connected to whatever ambient $DISPLAY existed. That is exactly the
# silent-success class this script's own header (above) says it exists to kill,
# reproduced inside the script.
#
# DISPLAY and WAYLAND_DISPLAY are the two the session cannot work without: they
# ARE the compositor, and a session whose units are talking to a different one is
# worse than a session that failed to start. XAUTHORITY and
# GAMESCOPE_WAYLAND_DISPLAY are genuinely optional (Xwayland here needs no
# cookie; the Wayland socket name matters only to clients that use it).
for var in DISPLAY WAYLAND_DISPLAY; do
    if [ -z "${!var:-}" ]; then
        printf 'tv-shell-gamescope-child: %s is unset inside gamescope, so the\n' "$var" >&2
        printf '  session environment would be incomplete and every unit ordered after\n' >&2
        printf '  the compositor would connect to an ambient server nobody declared.\n' >&2
        printf '  Refusing to publish it or to send READY=1.\n' >&2
        exit 1
    fi
    printf '%s=%s\n' "$var" "${!var}" >>"$tmp"
done
for var in GAMESCOPE_WAYLAND_DISPLAY XAUTHORITY; do
    # Only if set: an empty `XAUTHORITY=` line is worse than an absent one, since
    # it would override an inherited value with nothing.
    if [ -n "${!var:-}" ]; then
        printf '%s=%s\n' "$var" "${!var}" >>"$tmp"
    fi
done

mv -f -- "$tmp" "$TV_SHELL_ENV_FILE"
trap - EXIT

# NotifyAccess=all on the unit is what lets this — a child, not the main
# process — send the notification. It goes out only after the rename above, so
# READY=1 cannot be observed before the file is complete.
#
# `--pid=$$` is not decoration. systemd validates a notification by looking up
# the SENDER's cgroup via /proc/<pid>, and plain `systemd-notify` sends as its
# own short-lived process — which can exit before systemd gets there, dropping
# the notification. The unit would then sit until TimeoutStartSec and BindsTo=
# would end the session: an intermittent failure to boot with nothing in the
# journal to explain it. Attributing it to this script's pid gives systemd a
# process that is still alive, and stays valid across the `exec` below.
systemd-notify --ready --pid=$$

# The placeholder primary child. See the header.
exec sleep infinity
