#!/usr/bin/env bash
# Install tv-shell on a Linux machine: build the input daemon, lay the install
# tree under a prefix, register the Wayland session, and scaffold the per-user
# config dir from the shipped *.example files.
#
# This is the generic, distribution-agnostic install path — the same steps the
# homelab Ansible role used to hand-roll, now owned by the repo so anyone can
# install tv-shell without that private tooling. It does NOT install system
# dependencies (Hyprland, Quickshell, Rust, build libs) — run scripts/install-deps.sh
# first, or install them via your package manager. See docs/INSTALL.md.
#
# Usage:
#   sudo ./scripts/install.sh [--prefix DIR] [--user NAME] [options]
#
#   --prefix DIR        Install root (default: /opt/tv-shell). The shell itself
#                       is prefix-agnostic and resolves this at runtime; /opt is
#                       only this installer's default.
#   --user NAME         User whose ~/.config gets scaffolded and who owns the
#                       prefix (default: $SUDO_USER, else the invoking user).
#   --session-dir DIR   Where to write the .desktop (default: /usr/share/wayland-sessions).
#   --session-exec CMD  Exec= line for the session .desktop (default:
#                       <prefix>/scripts/tv-shell-session.sh). Override when a
#                       deployment wraps the session (e.g. a site launcher that
#                       runs site-specific setup before the repo launcher).
#   --no-build          Skip building the daemon (reuse an existing binary if any).
#   --features LIST     Cargo features for the daemon (default: cec,mcp).
#   -h, --help          Show this help.
#
# Re-runnable: rebuilds the daemon, refreshes the install tree and session file,
# and never clobbers existing per-user config (only fills in missing files).
set -euo pipefail

PREFIX="/opt/tv-shell"
# Legacy prefix kept as a compat symlink one migration cycle (see step 2b) so an
# old game-shell-wayland.desktop / session path still resolves after the rename.
LEGACY_PREFIX="/opt/game-shell"
SESSION_DIR="/usr/share/wayland-sessions"
SESSION_EXEC=""
TARGET_USER="${SUDO_USER:-}"
DO_BUILD=1
FEATURES="cec,mcp"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

die() { echo "install: $*" >&2; exit 1; }
log() { echo "install: $*"; }

while [ $# -gt 0 ]; do
    case "$1" in
        --prefix)       PREFIX="${2:?--prefix needs a value}"; shift 2 ;;
        --user)         TARGET_USER="${2:?--user needs a value}"; shift 2 ;;
        --session-dir)  SESSION_DIR="${2:?--session-dir needs a value}"; shift 2 ;;
        --session-exec) SESSION_EXEC="${2:?--session-exec needs a value}"; shift 2 ;;
        --no-build)     DO_BUILD=0; shift ;;
        --features)     FEATURES="${2:?--features needs a value}"; shift 2 ;;
        -h|--help)      sed -n '2,33p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *)              die "unknown argument: $1 (try --help)" ;;
    esac
done

# Default the session Exec to the repo launcher under the resolved prefix.
SESSION_EXEC="${SESSION_EXEC:-$PREFIX/scripts/tv-shell-session.sh}"

# Resolve the target user: --user wins, else $SUDO_USER, else the invoking user.
# Guard the footgun where someone runs as plain root (e.g. `sudo -i`) with no
# --user — that would install everything root-owned and break the shell for
# normal users. We check the *resolved* user (not $SUDO_USER directly) so an
# explicit --user still works under `become`/sudo that doesn't export SUDO_USER.
TARGET_USER="${TARGET_USER:-$(id -un)}"
if [ "$(id -u)" -eq 0 ] && [ "$TARGET_USER" = "root" ]; then
    die "running as root with no target user — pass --user NAME (or run via sudo) so the install isn't root-owned"
fi

# Resolve the target user's home (works whether or not we're under sudo).
TARGET_HOME="$(getent passwd "$TARGET_USER" | cut -d: -f6)"
[ -n "$TARGET_HOME" ] || die "could not resolve home for user '$TARGET_USER' (does the user exist?)"
CONFIG_DIR="$TARGET_HOME/.config/tv-shell"
QS_LINK="$TARGET_HOME/.config/quickshell/tv-shell"
# Legacy Quickshell config link kept one cycle so an old `quickshell -c game-shell`
# (a not-yet-updated session/exec-once during a mid-migration git pull) still finds
# the shell tree.
QS_LINK_LEGACY="$TARGET_HOME/.config/quickshell/game-shell"

# Writing under /opt + /usr/share needs root; the per-user bits get chowned back.
if [ "$(id -u)" -ne 0 ]; then
    die "must run as root (writes to $PREFIX and $SESSION_DIR) — re-run with sudo"
fi

log "prefix=$PREFIX user=$TARGET_USER home=$TARGET_HOME features=$FEATURES"

# Validate the prefix is creatable AND writable up front — don't fail with a
# cryptic error only after a multi-minute daemon build (read-only FS, missing
# parent, no space, etc.).
mkdir -p "$PREFIX" || die "cannot create prefix directory: $PREFIX (check permissions / parent path)"
( touch "$PREFIX/.install-write-check" && rm -f "$PREFIX/.install-write-check" ) \
    || die "cannot write to $PREFIX (read-only mount, permissions, or no space?)"

# 1. Build the daemon (canonical script owns the feature flags / profile). The
#    workspace builds to the repo-root target/ (see scripts/build-daemon.sh).
DAEMON_BIN="$REPO_ROOT/target/release/tv-shell-input"
if [ "$DO_BUILD" -eq 1 ]; then
    log "building tv-shell-input (features=$FEATURES) ..."
    TV_SHELL_FEATURES="$FEATURES" "$REPO_ROOT/scripts/build-daemon.sh" || die "daemon build failed"
    [ -f "$DAEMON_BIN" ] || die "build finished but $DAEMON_BIN is missing"
    log "daemon build succeeded"
fi

# 1b. Build the web control panel (canonical script: scripts/build-panel.sh).
#     Pure Rust, no feature flags — see that script for why.
PANEL_BIN="$REPO_ROOT/target/release/tv-shell-panel"
if [ "$DO_BUILD" -eq 1 ]; then
    log "building tv-shell-panel ..."
    "$REPO_ROOT/scripts/build-panel.sh" || die "panel build failed"
    [ -f "$PANEL_BIN" ] || die "build finished but $PANEL_BIN is missing"
    log "panel build succeeded"
fi

# 2. Lay down the install tree under $PREFIX. When the repo already lives at the
#    prefix (the in-place / dev-bridge layout), skip copying source over itself.
install -d -m755 "$PREFIX/bin"
if [ "$REPO_ROOT" != "$PREFIX" ]; then
    log "copying install tree to $PREFIX ..."
    for d in shell config scripts; do
        install -d -m755 "$PREFIX/$d" || die "cannot create $PREFIX/$d"
        cp -a "$REPO_ROOT/$d/." "$PREFIX/$d/" || die "failed to copy $d/ to $PREFIX/$d (permissions or space?)"
    done
else
    log "repo is the prefix — installing in place, no copy"
fi
# Install the freshly built binary. With --no-build and no build artifact (e.g. a
# python-fallback deploy, or target/ was cleaned after a prior install), leave any
# already-installed binary in place rather than failing.
if [ -f "$DAEMON_BIN" ]; then
    install -m755 "$DAEMON_BIN" "$PREFIX/bin/tv-shell-input"
elif [ -x "$PREFIX/bin/tv-shell-input" ]; then
    log "no build artifact at $DAEMON_BIN — keeping the installed daemon binary"
else
    log "WARNING: no daemon binary built or installed (continuing — shell will use any fallback)"
fi
# Install the freshly built panel binary, with the same no-build/no-artifact
# tolerance as the daemon binary above.
if [ -f "$PANEL_BIN" ]; then
    install -m755 "$PANEL_BIN" "$PREFIX/bin/tv-shell-panel"
elif [ -x "$PREFIX/bin/tv-shell-panel" ]; then
    log "no build artifact at $PANEL_BIN — keeping the installed panel binary"
else
    log "WARNING: no panel binary built or installed (continuing — panel unit will fail to start until built)"
fi

# 2b. Back-compat: a $LEGACY_PREFIX symlink -> the new default prefix so any
#     lingering game-shell-wayland.desktop / old $GAME_SHELL_DIR path still
#     resolves for one migration cycle. Only when installing to the default
#     prefix, and NEVER clobber a real directory (an old in-place install) — in
#     that case just log so the operator can migrate it deliberately.
if [ "$PREFIX" = "/opt/tv-shell" ]; then
    if [ ! -e "$LEGACY_PREFIX" ] || [ -L "$LEGACY_PREFIX" ]; then
        if ln -sfn "$PREFIX" "$LEGACY_PREFIX"; then
            log "compat symlink $LEGACY_PREFIX -> $PREFIX"
        else
            log "note: could not create compat symlink $LEGACY_PREFIX (continuing)"
        fi
    else
        log "note: $LEGACY_PREFIX exists as a real directory — skipping compat symlink (remove it to enable)"
    fi
fi

# 3. Register the Wayland session, rewriting Exec to the resolved prefix. Write
#    BOTH the new tv-shell-wayland.desktop and the legacy game-shell-wayland.desktop
#    (identical content, both pointing at the NEW session exec) for one migration
#    cycle so a display manager still lists a working entry under either name. The
#    old file is left in place, not deleted.
install -d -m755 "$SESSION_DIR"
write_session_file() { # write_session_file <path>
    log "writing session file $1"
    cat > "$1" <<EOF
[Desktop Entry]
Type=Application
Name=TV Shell (Wayland)
Comment=Quickshell game streaming launcher on Hyprland
Exec=$SESSION_EXEC
DesktopNames=Hyprland
EOF
}
write_session_file "$SESSION_DIR/tv-shell-wayland.desktop"
write_session_file "$SESSION_DIR/game-shell-wayland.desktop"

# 3b. Install the systemd --user units, rewriting ExecStart to the resolved prefix
#     where needed (mirrors the session .desktop Exec rewrite above). Three units:
#       - tv-shell-input.service   (daemon) — ExecStart rewritten to the prefix.
#       - tv-shell-quickshell.service (UI)  — installed verbatim; `quickshell`
#         resolves from PATH, so no prefix rewrite. It enforces a single Quickshell
#         instance (#254) and is started by Hyprland's exec-once.
#       - tv-shell-panel.service (web control panel) — ExecStart rewritten to the
#         prefix, same treatment as the daemon unit. Started by the session script.
#     No unit is enabled (no [Install]) — the session/compositor start them —
#     so installing is just file placement + a daemon-reload.
UNIT_SRC="$REPO_ROOT/config/tv-shell-input.service"
QS_UNIT_SRC="$REPO_ROOT/config/tv-shell-quickshell.service"
PANEL_UNIT_SRC="$REPO_ROOT/config/tv-shell-panel.service"
if [ -f "$UNIT_SRC" ]; then
    UNIT_DIR="$TARGET_HOME/.config/systemd/user"
    UNIT_FILE="$UNIT_DIR/tv-shell-input.service"
    log "installing systemd --user unit -> $UNIT_FILE"
    install -d -m755 "$UNIT_DIR"
    # Rewrite the committed default ExecStart (/opt/tv-shell/...) to the resolved
    # prefix's binary. Keep the rest of the unit verbatim. Use awk with the prefix
    # passed as a variable (not sed) so a prefix containing `#` (sed delimiter) or
    # `&` (sed replacement backreference) can't corrupt the unit.
    awk -v prefix="$PREFIX" \
        '/^ExecStart=/ { print "ExecStart=" prefix "/bin/tv-shell-input"; next } { print }' \
        "$UNIT_SRC" > "$UNIT_FILE" \
        || die "failed to write $UNIT_FILE"
    # Quickshell UI unit — copied verbatim (no ExecStart rewrite needed).
    if [ -f "$QS_UNIT_SRC" ]; then
        log "installing systemd --user unit -> $UNIT_DIR/tv-shell-quickshell.service"
        install -m644 "$QS_UNIT_SRC" "$UNIT_DIR/tv-shell-quickshell.service" \
            || die "failed to write $UNIT_DIR/tv-shell-quickshell.service"
    else
        log "WARNING: $QS_UNIT_SRC missing — Quickshell will run via the exec-once fallback (bare process)"
    fi
    # Panel unit — same ExecStart-rewrite treatment as the daemon unit above.
    if [ -f "$PANEL_UNIT_SRC" ]; then
        log "installing systemd --user unit -> $UNIT_DIR/tv-shell-panel.service"
        awk -v prefix="$PREFIX" \
            '/^ExecStart=/ { print "ExecStart=" prefix "/bin/tv-shell-panel"; next } { print }' \
            "$PANEL_UNIT_SRC" > "$UNIT_DIR/tv-shell-panel.service" \
            || die "failed to write $UNIT_DIR/tv-shell-panel.service"
    else
        log "WARNING: $PANEL_UNIT_SRC missing — the web control panel will not be available"
    fi
    chown -R "$TARGET_USER" "$TARGET_HOME/.config/systemd" \
        || die "failed to chown $TARGET_HOME/.config/systemd to $TARGET_USER"
    # daemon-reload so a re-run picks up unit edits. Best-effort: the target
    # user's systemd manager / bus may not be reachable from this (root) install
    # context — a fresh box, a container, or an install before first login. The
    # session script reloads-by-starting anyway, so never hard-fail here.
    if command -v systemctl >/dev/null 2>&1; then
        TARGET_UID="$(id -u "$TARGET_USER" 2>/dev/null || true)"
        if [ -n "$TARGET_UID" ] && [ -S "/run/user/$TARGET_UID/bus" ]; then
            if sudo -u "$TARGET_USER" \
                XDG_RUNTIME_DIR="/run/user/$TARGET_UID" \
                DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$TARGET_UID/bus" \
                systemctl --user daemon-reload >/dev/null 2>&1; then
                log "ran systemctl --user daemon-reload for $TARGET_USER"
            else
                log "note: systemctl --user daemon-reload skipped (user manager not reachable now — picked up on next login/start)"
            fi
        else
            log "note: no user bus for $TARGET_USER yet — systemd will load the unit on next login"
        fi
    fi
else
    log "WARNING: $UNIT_SRC missing — daemon will run via the session script fallback (bare process)"
fi

# 4. Per-user setup: Quickshell config symlinks + config dir seeded from examples.
#    Create BOTH the new (tv-shell) and legacy (game-shell) config-name symlinks so
#    either `quickshell -c <name>` resolves during the migration cycle.
log "linking Quickshell config -> $PREFIX/shell"
install -d -m755 "$(dirname "$QS_LINK")"
ln -sfn "$PREFIX/shell" "$QS_LINK" || die "failed to create symlink $QS_LINK"
[ -d "$QS_LINK" ] || die "symlink $QS_LINK is broken — target $PREFIX/shell does not exist"
ln -sfn "$PREFIX/shell" "$QS_LINK_LEGACY" \
    || log "note: could not create legacy Quickshell symlink $QS_LINK_LEGACY (continuing)"

install -d -m755 "$CONFIG_DIR"
# Seed real config from *.example without ever clobbering a user's edits.
seed() { # seed <example-name> <dest-name>
    local src="$REPO_ROOT/config/$1" dst="$CONFIG_DIR/$2"
    if [ -f "$src" ] && [ ! -e "$dst" ]; then
        cp "$src" "$dst"; log "seeded $dst (from $1)"
    fi
}
seed config.toml.example  config.toml
seed targets.json.example targets.json
# config.toml holds no secret inline (the bearer token lives in a separate 0600
# file referenced by [http].token_file), so it needs no special mode. If an
# operator still has a token file beside it, leave their permissions alone.

# 5. Hand the prefix + per-user files back to the target user. Failure here is
#    fatal — silently leaving the tree root-owned would block the user (and the
#    dev bridge) from editing or rebuilding later.
chown -R "$TARGET_USER" "$PREFIX" \
    || die "failed to chown $PREFIX to $TARGET_USER (check permissions / filesystem type)"
chown -h "$TARGET_USER" "$QS_LINK" "$QS_LINK_LEGACY" 2>/dev/null || true
chown -R "$TARGET_USER" "$CONFIG_DIR" "$(dirname "$QS_LINK")" \
    || die "failed to chown config dirs to $TARGET_USER (does $TARGET_USER own ~/.config?)"

log "done. Select 'TV Shell (Wayland)' in your display manager, then log in."
log "Edit $CONFIG_DIR/config.toml and targets.json to taste (see docs/INSTALL.md)."
