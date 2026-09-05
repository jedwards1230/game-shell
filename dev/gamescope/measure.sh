#!/bin/bash
# Read the numbers that decide the gamescope prototype (see README.md), from the
# kernel and from gamescope itself, and print PASS / FAIL / UNKNOWN per criterion.
#
# Run on the box while the prototype session is up. DRM connector properties are
# readable by any user; the debugfs bit-depth file needs root, so run with sudo
# for the full picture:   sudo dev/gamescope/measure.sh
# Under sudo the gamescope-side reads (gamescopectl, xprop) are run as the
# session user (SUDO_USER, else the owner of the env file): root cannot open
# gamescope's Xwayland display or its Wayland socket.
#
# Bit depth: amdgpu's debugfs output_bpc prints a "Current:" line only when it
# can resolve the connector's active stream; on some kernels it prints just
# "Maximum:", and then NOTHING on the box reports the negotiated link depth
# (the connector's "max bpc" property is the cap the compositor requested, not
# what was negotiated). The verdict says so instead of guessing.
#
# Reads are scoped to ONE connector (first connected + enabled, or
# TV_SHELL_GS_CONNECTOR=card1-HDMI-A-1 to pick) and the CRTC driving it.
# Everything is read-only.
set -u

ENV_FILE="${TV_SHELL_GS_ENV_FILE:-/tmp/tv-shell-gamescope.env}"
STATS="${TV_SHELL_GS_STATS:-/tmp/tv-shell-gamescope-stats}"
CONNECTOR="${TV_SHELL_GS_CONNECTOR:-}"
WANT_W="${TV_SHELL_GS_WIDTH:-3840}"
WANT_H="${TV_SHELL_GS_HEIGHT:-2160}"
WANT_R="${TV_SHELL_GS_REFRESH:-120}"

pass() { printf 'PASS     %-28s %s\n' "$1" "$2"; }
fail() { printf 'FAIL     %-28s %s\n' "$1" "$2"; }
unknown() { printf 'UNKNOWN  %-28s %s\n' "$1" "$2"; }
section() { printf '\n== %s\n' "$1"; }

section "versions"
uname -r
gamescope --version 2>/dev/null | head -1 || echo "gamescope: not installed"
pgrep -a gamescope | head -3 || echo "gamescope: not running"

# --- which connector ---------------------------------------------------------
if [ -z "$CONNECTOR" ]; then
    for c in /sys/class/drm/card*-*; do
        [ -r "$c/status" ] || continue
        if [ "$(cat "$c/status")" = "connected" ] && [ "$(cat "$c/enabled" 2>/dev/null)" = "enabled" ]; then
            CONNECTOR="$(basename "$c")"
            break
        fi
    done
fi
[ -n "$CONNECTOR" ] || CONNECTOR="unknown"
CONN_NAME="${CONNECTOR#card*-}"
section "connector $CONNECTOR"

# --- bit depth (debugfs, root) -----------------------------------------------
BPC_FILE=""
for d in /sys/kernel/debug/dri/*/"$CONN_NAME"/output_bpc; do
    [ -r "$d" ] && BPC_FILE="$d" && break
done
CUR_BPC=""
DEBUGFS_MAX_BPC=""
if [ -n "$BPC_FILE" ]; then
    echo "$BPC_FILE:"; sed 's/^/  /' "$BPC_FILE"
    CUR_BPC="$(sed -n 's/^Current: *\([0-9]*\).*/\1/p' "$BPC_FILE")"
    DEBUGFS_MAX_BPC="$(sed -n 's/^Maximum: *\([0-9]*\).*/\1/p' "$BPC_FILE")"
    [ -n "$CUR_BPC" ] || echo "  (no Current: line: this kernel does not expose the negotiated link depth)"
elif [ "$(id -u)" = "0" ]; then
    echo "output_bpc: no readable debugfs file for $CONN_NAME"
else
    echo "output_bpc: not readable (debugfs needs root; run with sudo)"
fi

# --- DRM properties via modetest ---------------------------------------------
# -e is needed for the connector -> encoder -> CRTC walk below.
MODETEST_OUT=""
if command -v modetest >/dev/null 2>&1; then
    MODETEST_OUT="$(timeout 10 modetest -M amdgpu -c -e -p 2>/dev/null || true)"
fi
# modetest lists EVERY object: each connector's props, then each CRTC's. On a
# box with two outputs an unscoped scan reports whichever comes first, so
# every read is scoped to one object block: a
# "Connectors:" / "CRTCs:" section, then the object row whose column <col>
# equals <key> ("402  401  connected  HDMI-A-1 ..." / "376  1338  (0,0)
# (3840x2160)"). An empty key means the whole section, which the verdicts then
# cannot attribute. Props are "\t<id> <name>:" followed by indented "flags:" /
# "enums:" / "value:" lines; a blob prop's value: line is EMPTY, with the bytes
# (if any) on the lines after it.
#
# obj_line <section> <col> <key> <prop> <field> -> that field's text. For a
# blob prop: the first 16 bytes as hex, or "0" when no blob is attached.
obj_line() {
    printf '%s\n' "$MODETEST_OUT" | awk -v sec="$1" -v col="$2" -v key="$3" -v want="$4" -v field="$5" '
        /^[A-Z][A-Za-z ]*:$/ {insec = ($0 == sec ":"); inobj = 0; grab = 0}
        insec && /^[0-9]+\t/ {inobj = (key == "" || $col == key); grab = 0}
        !inobj {next}
        $0 ~ "^\t[0-9]+ " want ":" {grab = 1; next}
        grab && $0 ~ ("^[ \t]*" field ":") {
            v = $0; sub(/^[ \t]*[a-z]+: */, "", v)
            if (v != "") {print v; exit}
            blob = 1; next
        }
        blob {print (/^\t\t\t[0-9a-f]+$/ ? $1 : "0"); exit}
        grab && /^\t[0-9]+ / {grab = 0}
    '
}
CONN_KEY="$CONN_NAME"
[ "$CONN_NAME" != "unknown" ] || CONN_KEY=""
# VRR_ENABLED and the active mode are CRTC properties, not connector ones. The
# connector row's column 2 is its encoder id; that encoder row's column 2 is
# the CRTC it drives (0 = none). Unresolved -> every CRTC, same as above.
CRTC_ID="$(printf '%s\n' "$MODETEST_OUT" | awk -v name="$CONN_KEY" '
    /^[A-Z][A-Za-z ]*:$/ {sec = $0}
    sec == "Encoders:" && /^[0-9]+\t/ {crtc[$1] = $2}
    sec == "Connectors:" && /^[0-9]+\t/ && $4 == name {enc = $2}
    END {if (enc in crtc && crtc[enc] != "0") print crtc[enc]}
')"
conn_value() { obj_line Connectors 4 "$CONN_KEY" "$1" value; }
crtc_value() { obj_line CRTCs 1 "$CRTC_ID" "$1" value; }
MAX_BPC="$(conn_value "max bpc")"
COLORSPACE_IDX="$(conn_value "Colorspace")"
COLORSPACE_NAME="$(obj_line Connectors 4 "$CONN_KEY" "Colorspace" enums)"
HDR_BLOB="$(conn_value "HDR_OUTPUT_METADATA")"
VRR_PROP="$(crtc_value "VRR_ENABLED")"
echo "crtc:                     ${CRTC_ID:-unresolved (reading every CRTC)}"
echo "max bpc value:            ${MAX_BPC:-?}"
echo "Colorspace value:         ${COLORSPACE_IDX:-?}   (${COLORSPACE_NAME:-enum list unavailable})"
echo "HDR_OUTPUT_METADATA blob: ${HDR_BLOB:-?}"
echo "VRR_ENABLED:              ${VRR_PROP:-?}"

# --- current mode ------------------------------------------------------------
# Each CRTC row is followed by its mode as "  #0 3840x2160 120.00 ..."; an idle
# CRTC prints "  #0  -nan 0 0 ..." with no WxH, which the $2 test skips.
CUR_MODE="$(printf '%s\n' "$MODETEST_OUT" | awk -v key="$CRTC_ID" '
    /^[A-Z][A-Za-z ]*:$/ {insec = ($0 == "CRTCs:"); inobj = 0}
    insec && /^[0-9]+\t/ {inobj = (key == "" || $1 == key)}
    inobj && /^ *#[0-9]+ / && $2 ~ /x/ {print $2, $3; exit}
')"
echo "active CRTC mode:         ${CUR_MODE:-?}"

# --- gamescope's own view ----------------------------------------------------
section "gamescope"
HDR_FEEDBACK=""
SUPPORTS_HDR=""
if [ -r "$ENV_FILE" ]; then
    # Source the env file BEFORE any gamescope-side read: DISPLAY, the Wayland
    # socket and XDG_RUNTIME_DIR all come from it.
    # shellcheck source=/dev/null
    . "$ENV_FILE"
    # Under sudo, run every gamescope-side read as the session user: root
    # cannot open gamescope's Xwayland display (XAUTHORITY is empty in the
    # env file, the server only admits its own uid) nor its Wayland socket.
    RUN_AS_USER=""
    if [ "$(id -u)" = "0" ]; then
        RUN_AS_USER="${SUDO_USER:-$(stat -c %U "$ENV_FILE" 2>/dev/null)}"
        [ "$RUN_AS_USER" != "root" ] || RUN_AS_USER=""
    fi
    as_user() { # as_user <cmd...>: as the session user when root; 5 s cap
        if [ -n "$RUN_AS_USER" ]; then
            timeout 5 sudo -u "$RUN_AS_USER" env "DISPLAY=${DISPLAY:-}" "XAUTHORITY=${XAUTHORITY:-}" \
                "XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-}" \
                "GAMESCOPE_WAYLAND_DISPLAY=${GAMESCOPE_WAYLAND_DISPLAY:-}" "$@"
        else
            timeout 5 "$@"
        fi
    }
    [ -z "$RUN_AS_USER" ] || echo "(gamescope-side reads run as $RUN_AS_USER)"
    if command -v gamescopectl >/dev/null 2>&1; then
        # Bare gamescopectl prints the display gamescope drives (connector,
        # make/model, valid refresh rates); `help` lists the commands and
        # convars this build actually has, so a missing one is visible here
        # rather than guessed at (focus_info does not exist in 3.16.x).
        echo "-- gamescopectl"
        as_user gamescopectl 2>&1 | sed 's/^/  /' | head -40
        echo "-- gamescopectl backend_info"
        as_user gamescopectl backend_info 2>&1 | sed 's/^/  /' | head -40
        echo "-- gamescopectl help (available commands and convars)"
        as_user gamescopectl help 2>&1 | sed 's/^/  /'
    fi
    if [ -n "${DISPLAY:-}" ] && command -v xprop >/dev/null 2>&1; then
        echo "-- focus + feedback atoms"
        ATOMS="$(as_user xprop -root GAMESCOPE_FOCUSED_APP GAMESCOPE_FOCUSED_WINDOW GAMESCOPE_FOCUSABLE_APPS \
            GAMESCOPE_HDR_OUTPUT_FEEDBACK GAMESCOPE_DISPLAY_SUPPORTS_HDR \
            GAMESCOPE_VRR_FEEDBACK GAMESCOPE_VRR_ENABLED 2>&1)"
        printf '%s\n' "$ATOMS" | sed 's/^/  /'
        # "NAME(CARDINAL) = 1" -> "1"; a "not found." line yields nothing.
        atom() { printf '%s\n' "$ATOMS" | sed -n "s/^$1(CARDINAL) = *\([0-9]*\).*/\1/p" | head -1; }
        HDR_FEEDBACK="$(atom GAMESCOPE_HDR_OUTPUT_FEEDBACK)"
        SUPPORTS_HDR="$(atom GAMESCOPE_DISPLAY_SUPPORTS_HDR)"
    fi
else
    echo "$ENV_FILE missing: prototype session not running (or client.sh never started)"
fi
if [ -p "$STATS" ]; then
    # A FIFO: reading it takes the stream over from any other reader for the
    # sample window, and a plain tail would never see EOF.
    echo "-- stats sample ($STATS, 4 s; empty until gamescope attaches to a reader)"
    timeout 4 head -n 6 "$STATS" 2>/dev/null | sed 's/^/  /'
elif [ -e "$STATS" ]; then
    echo "-- $STATS exists but is not a FIFO; session.sh predates the mkfifo fix?"
fi

# --- verdicts ----------------------------------------------------------------
section "verdicts"
# What IS known: debugfs Maximum (the sink's ceiling) and the connector's
# "max bpc" property (the cap the compositor requested). Neither is the
# negotiated link depth; only a "Current:" line is.
BPC_KNOWN="debugfs Maximum: ${DEBUGFS_MAX_BPC:-n/a}; max bpc property = ${MAX_BPC:-n/a} (requested cap, not negotiated depth)"
if [ -n "$CUR_BPC" ]; then
    if [ "$CUR_BPC" -ge 10 ]; then pass "output bit depth" "Current: $CUR_BPC bpc ($BPC_KNOWN)"; else fail "output bit depth" "Current: $CUR_BPC bpc (need >= 10; gamescope never sets max bpc, see ValveSoftware/gamescope#2075)"; fi
elif [ -n "$MAX_BPC" ] && [ "$MAX_BPC" -lt 10 ]; then
    fail "output bit depth" "max bpc property = $MAX_BPC caps the link below 10"
elif [ -z "$BPC_FILE" ] && [ "$(id -u)" != "0" ]; then
    unknown "output bit depth" "$BPC_KNOWN; debugfs needs root, re-run with sudo"
else
    unknown "output bit depth" "$BPC_KNOWN; this kernel does not expose the negotiated link depth; judge by the TV's info panel"
fi

case "${COLORSPACE_NAME:-}" in
    *BT2020*)
        # Enum index -> name: pick the enum whose "=N" matches the current value.
        CS_CUR="$(printf '%s\n' "$COLORSPACE_NAME" | tr ' ' '\n' | awk -F= -v v="$COLORSPACE_IDX" '$2==v{print $1}')"
        case "$CS_CUR" in
            *BT2020*) pass "colorspace" "$CS_CUR" ;;
            "") unknown "colorspace" "value $COLORSPACE_IDX not resolved against enum list" ;;
            *) fail "colorspace" "$CS_CUR (HDR output should be BT2020_RGB)" ;;
        esac ;;
    *) unknown "colorspace" "enum list unavailable" ;;
esac

if [ -n "$HDR_BLOB" ] && [ "$HDR_BLOB" != "0" ]; then pass "HDR_OUTPUT_METADATA" "blob $HDR_BLOB"; elif [ -n "$HDR_BLOB" ]; then fail "HDR_OUTPUT_METADATA" "no blob: output is SDR"; else unknown "HDR_OUTPUT_METADATA" "no data"; fi

if [ -n "$CUR_MODE" ]; then
    MODE_WH="${CUR_MODE%% *}"; MODE_R="${CUR_MODE##* }"; MODE_R="${MODE_R%%.*}"
    if [ "$MODE_WH" = "${WANT_W}x${WANT_H}" ] && [ "$MODE_R" = "$WANT_R" ]; then pass "mode" "$CUR_MODE"; else fail "mode" "$CUR_MODE (wanted ${WANT_W}x${WANT_H} @ $WANT_R; pass -W/-H/-r exactly as the EDID lists them)"; fi
else
    unknown "mode" "could not read the active CRTC mode"
fi

case "${VRR_PROP:-}" in
    1) pass "VRR" "VRR_ENABLED=1" ;;
    0) fail "VRR" "VRR_ENABLED=0 (session started with --adaptive-sync?)" ;;
    *) unknown "VRR" "no VRR_ENABLED property read" ;;
esac

# Criterion 8's signal. The WSI layer only offers HDR swapchains to a client
# when GAMESCOPE_HDR_OUTPUT_FEEDBACK is 1 on the X11 root (the Wayland path is
# hardcoded off upstream), so a PQ/BT2020 connector with this atom at 0 still
# means every client, Moonlight included, gets SDR.
case "${HDR_FEEDBACK:-}:${SUPPORTS_HDR:-}" in
    1:*) pass "HDR to clients" "GAMESCOPE_HDR_OUTPUT_FEEDBACK=1" ;;
    0:1) fail "HDR to clients" "GAMESCOPE_HDR_OUTPUT_FEEDBACK=0 while GAMESCOPE_DISPLAY_SUPPORTS_HDR=1: gamescope drives the display in HDR but offers clients SDR only" ;;
    0:*) unknown "HDR to clients" "GAMESCOPE_HDR_OUTPUT_FEEDBACK=0 and GAMESCOPE_DISPLAY_SUPPORTS_HDR=${SUPPORTS_HDR:-unset}: display not reported HDR-capable" ;;
    *) unknown "HDR to clients" "feedback atoms not read (no session, no xprop, or root without a session user)" ;;
esac

cat <<'EOF'

Not measurable by script; judge on the TV and in the stats FIFO:
  - composite cost: with a single HDR client, does the stats FIFO show a steady frame
    time at the refresh, or does it sit at 2x? (gamescope composites in Vulkan whenever
    it cannot direct-scanout; this kernel lacks CONFIG_AMD_PRIVATE_COLOR)
  - SDR black floor: the black end of the strip at the top of the prototype shell should
    be as black as the letterbox around a 16:9 HDR film, not grey
  - overlay: launch.sh overlay, then confirm the app underneath keeps animating
  - focus: launch.sh xmessage / focus.sh app 9001 should switch instantly, every time
  - bit depth when the kernel hides it: the TV's own info panel is the only reading
EOF
