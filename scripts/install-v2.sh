#!/usr/bin/env bash
# Install the tv-shell **v2** (gamescope) session BESIDE an existing v1 install:
# build tv-shell-core, lay a v2-only prefix, install the three v2 systemd --user
# units with their prefix token substituted, and register a v2 session entry the
# display manager can offer next to v1's.
#
# WHY THIS IS A SEPARATE SCRIPT AND NOT A `--v2` MODE IN scripts/install.sh
#
#   docs/V2_DESIGN.md §11's governing rule is "beside, not instead, at every
#   shared layer", and the installer is a shared layer. A `--v2` flag would put
#   both prefixes, both unit sets and both session files behind one entry point
#   whose DEFAULT is v1 — so a forgotten flag installs over the running
#   appliance, and every step inside would need a branch to stay out of v1's way.
#   This script cannot make that mistake: it has no v1 prefix, no v1 unit name
#   and no v1 session file anywhere in it, and it refuses outright to install to
#   v1's prefix (see PREFIX validation below). The blast radius is a property of
#   the file, not of an argument.
#
#   It is also not much shared code: install.sh's bulk is v1-specific — the
#   Quickshell config symlinks, the game-shell legacy compat symlinks and
#   .desktop, the panel and daemon binaries, targets.json. v2 has none of those,
#   and has one thing v1 does not (the @TV_SHELL_V2_PREFIX@ token substitution).
#
# It does NOT install system dependencies (gamescope, Rust, an X server) — see
# docs/INSTALL.md and scripts/install-deps.sh.
#
# Usage:
#   sudo ./scripts/install-v2.sh [--prefix DIR] [--user NAME] [options]
#
#   --prefix DIR        v2 install root (default: /opt/tv-shell-v2). MUST NOT be
#                       v1's /opt/tv-shell, or anywhere under it — the script
#                       normalises the path and refuses. Relative paths are made
#                       absolute (the units need an absolute ExecStart).
#   --user NAME         User whose ~/.config gets the units and core.toml, and
#                       who owns the prefix (default: $SUDO_USER, else invoker).
#   --session-dir DIR   Where to write the session .desktop
#                       (default: /usr/share/wayland-sessions).
#   --session-exec CMD  Exec= for the session .desktop
#                       (default: <prefix>/bin/tv-shell-gamescope-session.sh).
#   --unit-dir DIR      systemd --user unit dir
#                       (default: <home>/.config/systemd/user).
#   --config-dir DIR    Per-user config dir (default: <home>/.config/tv-shell).
#   --no-build          Skip building tv-shell-core (reuse an existing binary).
#   -h, --help          Show this help.
#
# Re-runnable: rebuilds, refreshes the tree, the units and the session file, and
# never clobbers an existing core.toml.
set -euo pipefail

# v2's own prefix. NEVER v1's — §11 gives v2 its own install prefix so a v2
# deploy cannot replace the shell tree the couch is running.
PREFIX="/opt/tv-shell-v2"
# v1's, named here only so the guard below can refuse it.
V1_PREFIX="/opt/tv-shell"
# The literal the committed units carry in place of an absolute path. See the
# long comment on tv-shell-core.service's ExecStart for why it is a token.
PREFIX_TOKEN="@TV_SHELL_V2_PREFIX@"
# The third session file name: v1 owns tv-shell-wayland.desktop and the Ansible
# measurement prototype owns tv-shell-gamescope.desktop. See config/tv-shell-v2.desktop.
SESSION_FILE="tv-shell-v2.desktop"
SESSION_DIR="/usr/share/wayland-sessions"
SESSION_EXEC=""
UNIT_DIR=""
CONFIG_DIR=""
TARGET_USER="${SUDO_USER:-}"
USER_EXPLICIT=0
DO_BUILD=1

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
UNIT_SRC_DIR="$REPO_ROOT/core/units"

# The three units this installs, and the two scripts that go in <prefix>/bin.
UNITS=(
    tv-shell-gamescope.service
    tv-shell-core.service
    tv-shell-session.target
)
BIN_SCRIPTS=(
    tv-shell-gamescope-session.sh
    tv-shell-gamescope-child.sh
)

die() { echo "install-v2: $*" >&2; exit 1; }
log() { echo "install-v2: $*"; }

while [ $# -gt 0 ]; do
    case "$1" in
        --prefix)       PREFIX="${2:?--prefix needs a value}"; shift 2 ;;
        --user)         TARGET_USER="${2:?--user needs a value}"; USER_EXPLICIT=1; shift 2 ;;
        --session-dir)  SESSION_DIR="${2:?--session-dir needs a value}"; shift 2 ;;
        --session-exec) SESSION_EXEC="${2:?--session-exec needs a value}"; shift 2 ;;
        --unit-dir)     UNIT_DIR="${2:?--unit-dir needs a value}"; shift 2 ;;
        --config-dir)   CONFIG_DIR="${2:?--config-dir needs a value}"; shift 2 ;;
        --no-build)     DO_BUILD=0; shift ;;
        -h|--help)      sed -n '2,47p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *)              die "unknown argument: $1 (try --help)" ;;
    esac
done

[ -n "$PREFIX" ] || die "--prefix cannot be empty"

# NORMALISE BEFORE COMPARING. A trailing-slash strip is not normalisation, and
# the difference was a live hole: `--prefix /opt//tv-shell` compared unequal to
# `/opt/tv-shell`, sailed past the guard, and — as root on the appliance — would
# have installed straight into the prefix v1's running session boots from. So
# would `--prefix /opt/tv-shell/anything`, which is under v1's tree even though
# it is not equal to it. `realpath -m` collapses duplicate slashes, resolves
# `.`/`..` and symlinked components, and makes a relative prefix absolute (which
# the units need anyway — systemd rejects a relative ExecStart), all without
# requiring the path to exist yet.
#
# It is REQUIRED, not best-effort: a fallback that silently compares
# un-normalised strings is a guard that reports success without providing the
# protection, which is the failure class this whole design exists to remove.
command -v realpath >/dev/null 2>&1 \
    || die "realpath (coreutils) is required to normalise --prefix safely"
PREFIX="$(realpath -m -- "$PREFIX")" || die "could not normalise --prefix"
V1_PREFIX_CANON="$(realpath -m -- "$V1_PREFIX")" || die "could not normalise the v1 prefix"

# THE ONE REFUSAL THIS SCRIPT MAKES. §11: v2 has its own install prefix, and a v2
# install must be incapable of disturbing v1. Installing to v1's prefix — or
# ANYWHERE UNDER IT — would put the core binary and the v2 session script inside
# the tree v1's session runs from, and the next v1 install.sh run would fight it.
case "$PREFIX" in
    "$V1_PREFIX_CANON" | "$V1_PREFIX_CANON"/*)
        die "refusing to install v2 into v1's prefix ($V1_PREFIX_CANON) or under it — v2 needs its own (§11). Pass --prefix elsewhere."
        ;;
esac

SESSION_EXEC="${SESSION_EXEC:-$PREFIX/bin/tv-shell-gamescope-session.sh}"

# Resolve the target user. Same footgun as install.sh: a plain-root run with no
# --user would install everything root-owned. Guard only the IMPLICIT case, so an
# explicit `--user root` (a root-only container, CI) still works.
TARGET_USER="${TARGET_USER:-$(id -un)}"
if [ "$(id -u)" -eq 0 ] && [ "$TARGET_USER" = "root" ] && [ "$USER_EXPLICIT" -eq 0 ]; then
    die "running as root with no target user — pass --user NAME (or run via sudo) so the install isn't root-owned"
fi

TARGET_HOME="$(getent passwd "$TARGET_USER" | cut -d: -f6)"
[ -n "$TARGET_HOME" ] || die "could not resolve home for user '$TARGET_USER' (does the user exist?)"
UNIT_DIR="${UNIT_DIR:-$TARGET_HOME/.config/systemd/user}"
CONFIG_DIR="${CONFIG_DIR:-$TARGET_HOME/.config/tv-shell}"

# Writability is checked up front for every destination — not a blanket `must be
# root`, because the defaults are what need root and a caller pointing all three
# somewhere writable (a staging dir, the install test) legitimately does not.
# Failing here beats failing after a multi-minute cargo build.
#
# `install -d -m755`, NOT `mkdir -p`: mkdir applies the CALLER's umask, and a
# caller with a tight one (the core's own IPC bind sets 0o177 while it creates
# the socket) would leave a prefix directory with no execute bit — after which
# every write into it fails with a permission error that looks like a
# filesystem problem rather than an inherited umask. An explicit mode is
# umask-immune, which is also why the file installs below use `install -m`.
need_writable() { # need_writable <dir> <what>
    install -d -m755 "$1" || die "cannot create $2 directory: $1 (permissions? try sudo)"
    ( touch "$1/.install-v2-write-check" && rm -f "$1/.install-v2-write-check" ) \
        || die "cannot write to $2 directory: $1 (read-only mount, permissions, or no space? try sudo)"
}
need_writable "$PREFIX" "prefix"
need_writable "$SESSION_DIR" "session"
need_writable "$UNIT_DIR" "unit"
need_writable "$CONFIG_DIR" "config"

log "prefix=$PREFIX user=$TARGET_USER units=$UNIT_DIR session=$SESSION_DIR/$SESSION_FILE"

# 1. Build the core. Workspace-scoped, so the binary lands in the repo-root
#    target/ (the same place scripts/build-daemon.sh puts the daemon).
CORE_BIN="$REPO_ROOT/target/release/tv-shell-core"
if [ "$DO_BUILD" -eq 1 ]; then
    log "building tv-shell-core ..."
    ( cd "$REPO_ROOT" && cargo build --release -p tv-shell-core ) || die "core build failed"
    [ -f "$CORE_BIN" ] || die "build finished but $CORE_BIN is missing"
    log "core build succeeded"
fi

# 2. <prefix>/bin: the core binary plus the two session scripts. The session
#    script derives the prefix from its own directory, so these two MUST land
#    beside the binary — that adjacency is the substitute for a rewrite there.
install -d -m755 "$PREFIX/bin"
if [ -f "$CORE_BIN" ]; then
    install -m755 "$CORE_BIN" "$PREFIX/bin/tv-shell-core"
elif [ -x "$PREFIX/bin/tv-shell-core" ]; then
    log "no build artifact at $CORE_BIN — keeping the installed core binary"
else
    log "WARNING: no core binary built or installed — the session will fail at 'write-session-env'"
fi
for s in "${BIN_SCRIPTS[@]}"; do
    [ -f "$UNIT_SRC_DIR/$s" ] || die "missing $UNIT_SRC_DIR/$s"
    install -m755 "$UNIT_SRC_DIR/$s" "$PREFIX/bin/$s"
done

# 3. The systemd --user units, with @TV_SHELL_V2_PREFIX@ substituted.
#
#    Literal replacement, not sed and not awk's gsub(): a prefix containing `&`
#    is a backreference to both of those, and `#`/`/` are delimiters. This walks
#    the string with index()/substr() so the prefix is only ever data.
subst_prefix() { # subst_prefix <src> <dst>
    awk -v prefix="$PREFIX" -v tok="$PREFIX_TOKEN" '
        {
            out = ""; rest = $0
            while ((i = index(rest, tok)) > 0) {
                out = out substr(rest, 1, i - 1) prefix
                rest = substr(rest, i + length(tok))
            }
            print out rest
        }' "$1" > "$2" || die "failed to write $2"
}

for u in "${UNITS[@]}"; do
    [ -f "$UNIT_SRC_DIR/$u" ] || die "missing $UNIT_SRC_DIR/$u"
    log "installing systemd --user unit -> $UNIT_DIR/$u"
    subst_prefix "$UNIT_SRC_DIR/$u" "$UNIT_DIR/$u"
    chmod 644 "$UNIT_DIR/$u"
    # THE INSTALLER ASSERTS ITS OWN POSTCONDITION. A leftover token would exec a
    # path that does not exist; a v1 path would exec v1's tree out of a v2 unit,
    # which is the failure §11 exists to prevent and the one that would look like
    # it worked. Both are fatal here rather than at 2 a.m. on the couch.
    ! grep -q "$PREFIX_TOKEN" "$UNIT_DIR/$u" \
        || die "$UNIT_DIR/$u still carries $PREFIX_TOKEN after substitution"
    ! grep -q "$V1_PREFIX/" "$UNIT_DIR/$u" \
        || die "$UNIT_DIR/$u names a path under v1's prefix ($V1_PREFIX/)"
done

# 4. The v2 session entry. A DIFFERENT file name from both v1's
#    (tv-shell-wayland.desktop) and the Ansible measurement prototype's
#    (tv-shell-gamescope.desktop) — see config/tv-shell-v2.desktop.
log "writing session file $SESSION_DIR/$SESSION_FILE"
cat > "$SESSION_DIR/$SESSION_FILE" <<EOF
[Desktop Entry]
Type=Application
Name=TV Shell v2 (gamescope)
Comment=gamescope session with the tv-shell v2 core (base-layer policy, scoped launching)
Exec=$SESSION_EXEC
DesktopNames=gamescope
EOF
chmod 644 "$SESSION_DIR/$SESSION_FILE"

# 5. Seed core.toml — v2's own config file. NOT config.toml: v1's root is
#    deny_unknown_fields, so a shared file would abort the v1 daemon at startup
#    (§11). Never clobber an operator's edits.
CORE_EXAMPLE="$REPO_ROOT/config/core.toml.example"
if [ -f "$CORE_EXAMPLE" ] && [ ! -e "$CONFIG_DIR/core.toml" ]; then
    install -m644 "$CORE_EXAMPLE" "$CONFIG_DIR/core.toml"
    log "seeded $CONFIG_DIR/core.toml (from core.toml.example)"
fi

# 6. Hand the tree back to the target user (root runs only).
if [ "$(id -u)" -eq 0 ]; then
    chown -R "$TARGET_USER" "$PREFIX" \
        || die "failed to chown $PREFIX to $TARGET_USER"
    chown -R "$TARGET_USER" "$UNIT_DIR" "$CONFIG_DIR" \
        || die "failed to chown the per-user dirs to $TARGET_USER"
fi

# 7. daemon-reload, best-effort: the user manager may not be reachable from a
#    root install context (fresh box, container, pre-first-login). Same tolerance
#    as install.sh — the session script starts the target anyway.
if command -v systemctl >/dev/null 2>&1; then
    TARGET_UID="$(id -u "$TARGET_USER" 2>/dev/null || true)"
    if [ -n "$TARGET_UID" ] && [ -S "/run/user/$TARGET_UID/bus" ]; then
        # `sudo -n`: an install is not an interactive session, and a sudo that
        # PROMPTS here would hang the whole run on a password nobody is watching
        # for. When we already are the target user, skip the hop entirely.
        reload=(env "XDG_RUNTIME_DIR=/run/user/$TARGET_UID" \
                "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$TARGET_UID/bus" \
                systemctl --user daemon-reload)
        [ "$TARGET_USER" = "$(id -un)" ] || reload=(sudo -n -u "$TARGET_USER" "${reload[@]}")
        if "${reload[@]}" >/dev/null 2>&1; then
            log "ran systemctl --user daemon-reload for $TARGET_USER"
        else
            log "note: daemon-reload skipped (user manager not reachable now)"
        fi
    else
        log "note: no user bus for $TARGET_USER yet — systemd loads the units on next login"
    fi
fi

log "done. v1 is untouched: its prefix ($V1_PREFIX), units and session file were not written."
log "Select 'TV Shell v2 (gamescope)' at the display manager; 'TV Shell (Wayland)' is still the v1 rollback (§11)."
log "Edit $CONFIG_DIR/core.toml to taste (see config/core.toml.example)."
