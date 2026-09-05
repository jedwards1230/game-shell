#!/bin/bash
# Read the numbers that decide the gamescope prototype (see README.md), from the
# kernel and from gamescope itself, and print PASS / FAIL / UNKNOWN per criterion.
#
# Run on the box while the prototype session is up. DRM connector properties are
# readable by any user; the debugfs bit-depth file needs root, so run with sudo
# for the full picture:   sudo dev/gamescope/measure.sh
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
if [ -n "$BPC_FILE" ]; then
    echo "$BPC_FILE:"; sed 's/^/  /' "$BPC_FILE"
    CUR_BPC="$(sed -n 's/^Current: *\([0-9]*\).*/\1/p' "$BPC_FILE")"
else
    echo "output_bpc: not readable (run with sudo)"
    CUR_BPC=""
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
if [ -r "$ENV_FILE" ]; then
    # shellcheck source=/dev/null
    . "$ENV_FILE"
    if command -v gamescopectl >/dev/null 2>&1; then
        RUN_AS=""
        if [ "$(id -u)" = "0" ] && [ -n "${SUDO_USER:-}" ]; then RUN_AS="sudo -u $SUDO_USER env XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR GAMESCOPE_WAYLAND_DISPLAY=$GAMESCOPE_WAYLAND_DISPLAY"; fi
        for cmd in backend_info focus_info; do
            echo "-- gamescopectl $cmd"
            # shellcheck disable=SC2086 # RUN_AS is intentionally word-split
            timeout 5 $RUN_AS gamescopectl "$cmd" 2>&1 | sed 's/^/  /' | head -40
        done
    fi
    if [ -n "${DISPLAY:-}" ] && command -v xprop >/dev/null 2>&1; then
        echo "-- focus atoms"
        xprop -root GAMESCOPE_FOCUSED_APP GAMESCOPE_FOCUSED_WINDOW GAMESCOPE_FOCUSABLE_APPS 2>&1 | sed 's/^/  /'
    fi
else
    echo "$ENV_FILE missing: prototype session not running (or client.sh never started)"
fi
if [ -r "$STATS" ]; then
    echo "-- stats tail ($STATS)"
    tail -n 8 "$STATS" | sed 's/^/  /'
fi

# --- verdicts ----------------------------------------------------------------
section "verdicts"
if [ -n "$CUR_BPC" ]; then
    if [ "$CUR_BPC" -ge 10 ]; then pass "output bit depth" "Current: $CUR_BPC bpc"; else fail "output bit depth" "Current: $CUR_BPC bpc (need >= 10; gamescope never sets max bpc, see ValveSoftware/gamescope#2075)"; fi
elif [ -n "$MAX_BPC" ]; then
    if [ "$MAX_BPC" -ge 10 ]; then unknown "output bit depth" "max bpc property = $MAX_BPC but debugfs Current unreadable; re-run with sudo"; else fail "output bit depth" "max bpc property = $MAX_BPC"; fi
else
    unknown "output bit depth" "no modetest or debugfs data"
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

cat <<'EOF'

Not measurable by script; judge on the TV and in the stats file:
  - composite cost: with a single HDR client, does the stats file show a steady frame
    time at the refresh, or does it sit at 2x? (gamescope composites in Vulkan whenever
    it cannot direct-scanout; this kernel lacks CONFIG_AMD_PRIVATE_COLOR)
  - SDR black floor: the black end of the strip at the top of the prototype shell should
    be as black as the letterbox around a 16:9 HDR film, not grey
  - overlay: launch.sh overlay, then confirm the app underneath keeps animating
  - focus: launch.sh xmessage / focus.sh app 9001 should switch instantly, every time
EOF
