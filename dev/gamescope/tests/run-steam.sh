#!/bin/bash
# Fixture test for the Steam verbs: class-family tagging (pid tree + WM_CLASS
# set), --keep-existing, the detached watcher, --watch-baselayer, and
# steamlink's not-installed path. Uses the same fake xprop as run.sh plus fake
# steam/flatpak under bin/.
#
#   run-steam.sh <path-to-dev/gamescope>
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# The kit under test defaults to the directory this fixture lives in; an explicit
# path still wins so a checkout can be tested against a deployed kit.
KIT="${1:-$(cd "$HERE/.." && pwd)}"
[ -x "$KIT/focus.sh" ] || { echo "error: no gamescope kit at $KIT" >&2; exit 2; }
export PATH="$HERE/bin:$PATH"
# Everything the run writes goes under one scratch dir: the fake X server state,
# a throwaway $HOME, and the kit's own client-log dir. Nothing is written into
# the checkout, and nothing collides with a real kit running on the same box.
WORK="$(mktemp -d "${TMPDIR:-/tmp}/tv-shell-gs-fixture.XXXXXX")"
export HOME="$WORK/home"; mkdir -p "$HOME"
export TV_SHELL_GS_LOG_DIR="$WORK/clients"; mkdir -p "$TV_SHELL_GS_LOG_DIR"
# --- cleanup ---------------------------------------------------------------
# Runs on a normal exit AND on INT/TERM, so an interrupted run leaves neither
# the scratch dir nor a stray `sleep 300` behind.
#
# Processes are killed BY PID, never by program name: a `pkill -f moonlight`
# would reach a real Moonlight, or a second fixture run on the same box. Three
# sources, because no single one sees everything:
#   1. our own live descendants — the shells and sleeps this script backgrounds;
#   2. pids recorded in $TV_SHELL_GS_TEST_PIDS — each fake client appends its own
#      pid on startup. The kit backgrounds them with `nohup` inside a command
#      substitution, so they are reparented to init and no tree walk finds them;
#   3. pids passed to track() explicitly, for the one client S2 deliberately
#      `setsid`s out of our process tree.
# track() writes to that same file rather than a shell array so it works from
# inside a command substitution, where an array assignment would be discarded.
export TV_SHELL_GS_TEST_PIDS="$WORK/pids"
: > "$TV_SHELL_GS_TEST_PIDS"
track() { printf '%s\n' "$1" >> "$TV_SHELL_GS_TEST_PIDS"; }

descendants() { # descendants <pid> -> every live process below it, deepest first
    local p
    for p in $(pgrep -P "$1" 2>/dev/null); do descendants "$p"; printf '%s ' "$p"; done
}

reap() { # end the fake clients this section started, and forget them
    local p
    while read -r p; do kill "$p" 2>/dev/null; done < "$TV_SHELL_GS_TEST_PIDS"
    : > "$TV_SHELL_GS_TEST_PIDS"
}

CLEANED=0
# shellcheck disable=SC2317,SC2329  # reached only through the traps below.
# (SC2317 up to shellcheck 0.9.x, SC2329 from 0.10 on — both must be named.)
cleanup() {
    [ "$CLEANED" = 0 ] || return 0   # idempotent: a signal fires this, then EXIT does
    CLEANED=1
    local pids p
    pids="$(descendants $$) $(tr '\n' ' ' < "$TV_SHELL_GS_TEST_PIDS" 2>/dev/null)"
    for p in $pids; do kill "$p" 2>/dev/null; done
    for p in $pids; do kill -0 "$p" 2>/dev/null && kill -9 "$p" 2>/dev/null; done
    rm -rf "$WORK"
    return 0
}
trap cleanup EXIT
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 143' TERM

export TV_SHELL_GS_POLL_SECS=0.2
export TV_SHELL_GS_WATCH_SECS=0.2
export TV_SHELL_GS_ENV_FILE=/nonexistent
export DISPLAY=:9
pass=0; fail=0
ok()   { pass=$((pass + 1)); echo "  ok   $*"; }
bad()  { fail=$((fail + 1)); echo "  FAIL $*"; }
# shellcheck disable=SC2001  # a per-line prefix is what sed is for
dump() { printf '%s\n' "$1" | sed 's/^/     | /'; }   # echo a captured run, indented
check() { local d="$1"; shift; if "$@" >/dev/null 2>&1; then ok "$d"; else bad "$d"; fi; }
not() { ! "$@"; }
fresh() {
    export FAKE_X="$WORK/state/$1"
    rm -rf "$FAKE_X"; mkdir -p "$FAKE_X/win" "$FAKE_X/tags"
    : > "$FAKE_X/calls.log"; : > "$FAKE_X/tag.log"; : > "$FAKE_X/root.log"
    unset FAKE_WSI_LOG FAKE_SERVERINFO FAKE_STEAMLINK
}
win() { local x="$1"; shift; printf '%s\n' "$@" > "$FAKE_X/win/$x"; }

echo "== S1. focus.sh tag-pid --family --class: launcher -> client -> streaming_client, three X clients"
fresh S1
bash -c 'echo $$ > "$FAKE_X/fam.launcher"; bash -c "echo \$\$ > \"$FAKE_X/fam.client\"; sleep 300 & echo \$! > \"$FAKE_X/fam.stream\"; wait" & wait' </dev/null >/dev/null 2>&1 &
sleep 0.5
L=$(cat "$FAKE_X/fam.launcher"); C=$(cat "$FAKE_X/fam.client"); S=$(cat "$FAKE_X/fam.stream")
win 0xa00010 appear=1 "name=Steam Big Picture Mode" class=steam "pid=$C"
win 0xa00011 appear=1 name=Steam class=steam "pid=$C"           # Steam tagged this one itself
echo 769 > "$FAKE_X/tags/0xa00011"
win 0xc00010 appear=2 name=steamwebhelper class=steamwebhelper  # no _NET_WM_PID: class only
win 0xe00010 appear=4 "name=Steam Remote Play" class=streaming_client "pid=$S"
win 0xe00011 appear=4 name=unrelated class=other "pid=$S"       # same family, wrong class, but the pid matches
win 0xf00010 appear=1 name=stranger class=steam pid=1            # right class, other pid: still tagged (class rule)
out="$(TV_SHELL_GS_XID_PROBE=4 "$KIT/focus.sh" tag-pid "$L" 9004 --family --keep-existing --class steam --class steamwebhelper --class streaming_client --name "Steam Big Picture Mode" --name steamwebhelper --name "Steam Remote Play" --timeout 4 2>&1)"; rc=$?
dump "$out"
check "exit 0" [ "$rc" = 0 ]
check "client window tagged (family pid)" grep -q "tag 0xa00010 STEAM_GAME=9004" "$FAKE_X/tag.log"
check "steamwebhelper tagged (class only)" grep -q "tag 0xc00010 STEAM_GAME=9004" "$FAKE_X/tag.log"
check "streaming_client tagged when it appeared (grandchild pid)" grep -q "poll=4 tag 0xe00010 STEAM_GAME=9004" "$FAKE_X/tag.log"
check "family pid with foreign class still tagged (pid rule)" grep -q "tag 0xe00011 STEAM_GAME=9004" "$FAKE_X/tag.log"
check "Steam's own 769 left alone" not grep -q "0xa00011" "$FAKE_X/tag.log"
check "...and reported" grep -q 'known 0xa00011 "Steam" (has STEAM_GAME=769, left alone)' <<< "$out"
check "each window tagged once" [ "$(grep -c 'tag 0x' "$FAKE_X/tag.log")" = 4 ]
check "a window with no discovery path stays untagged (documented limit)" not grep -q "0xf00010" "$FAKE_X/tag.log"
kill "$L" "$C" "$S" 2>/dev/null

echo "== S2. --family outlives a launcher that exits; ends when the whole family is gone"
fresh S2
bash -c 'echo $$ > "$FAKE_X/fam.launcher"; setsid bash -c "echo \$\$ > \"$FAKE_X/fam.client\"; sleep 300" & sleep 1.5' </dev/null >/dev/null 2>&1 &
sleep 0.5
L=$(cat "$FAKE_X/fam.launcher"); C=$(cat "$FAKE_X/fam.client")
track "$C"   # setsid put this one outside our process tree; the trap needs its pid
win 0xa00010 appear=3 name=Steam class=steam "pid=$C"
( sleep 2.5; kill "$C" 2>/dev/null ) &
out="$(TV_SHELL_GS_XID_PROBE=0 "$KIT/focus.sh" tag-pid "$L" 9004 --family --class steam --name Steam --timeout 20 2>&1)"; rc=$?
dump "$out"
check "launcher gone before the window appeared, watch went on" grep -q "tag 0xa00010 STEAM_GAME=9004" "$FAKE_X/tag.log"
check "ended when the client died, exit 0" [ "$rc" = 0 ]
check "says so" grep -q "exited; 1 window(s) had been tagged" <<< "$out"

echo "== S3. focus.sh watch-baselayer logs each change with a timestamp"
fresh S3
"$KIT/focus.sh" watch-baselayer 3 > "$FAKE_X/bl.log" 2>&1 &
W=$!
sleep 0.6; printf '%s\n' "9004, 769, 9001" > "$FAKE_X/baselayer"; echo 9001 > "$FAKE_X/focused"
sleep 0.6; printf '%s\n' "769, 9001" > "$FAKE_X/baselayer"; echo 769 > "$FAKE_X/focused"
wait "$W"
sed 's/^/     | /' "$FAKE_X/bl.log"
check "initial unset state logged" grep -q 'baselayer=\[unset\]' "$FAKE_X/bl.log"
check "kit's list logged" grep -q 'baselayer=\[9004, 769, 9001\] window=\[\] focused=\[9001\]' "$FAKE_X/bl.log"
check "Steam's rewrite logged" grep -q 'baselayer=\[769, 9001\] window=\[\] focused=\[769\]' "$FAKE_X/bl.log"
check "timestamps present" grep -q -E '^[0-9]{2}:[0-9]{2}:[0-9]{2}\.[0-9]{3} baselayer=' "$FAKE_X/bl.log"
check "ended on time" grep -q 'watch ended after 3s' "$FAKE_X/bl.log"

echo "== S4. launch.sh steam --watch-baselayer: -bigpicture default, base list, detached watchers"
fresh S4
win 0xa00010 appear=1 "name=Steam Big Picture Mode" class=steam "pid=@$FAKE_X/steam.client.pid"
# gamescope lists a STEAM_STREAMING_CLIENT window as focusable even untagged (focusable=1)
win 0xe00010 appear=5 "name=Steam Remote Play" class=streaming_client "pid=@$FAKE_X/steam.stream.pid" focusable=1
out="$(TV_SHELL_GS_XID_PROBE=0 TV_SHELL_GS_STEAM_WATCH_SECS=6 "$KIT/launch.sh" steam --watch-baselayer --extra-arg 2>&1)"; rc=$?
dump "$out"
check "exit 0" [ "$rc" = 0 ]
check "steam -bigpicture by default, extra args kept" [ "$(paste -sd' ' "$FAKE_X/steam.argv")" = "-bigpicture --extra-arg" ]
check "base list 9004,769,9001 set first" [ "$(head -1 "$FAKE_X/root.log")" = "set GAMESCOPECTRL_BASELAYER_APPID=9004,769,9001" ]
check "first window tagged in the foreground" grep -q "tag 0xa00010 STEAM_GAME=9004" "$FAKE_X/tag.log"
check "focus lines printed" grep -q "GAMESCOPECTRL_BASELAYER_APPID(CARDINAL) = 9004, 769, 9001" <<< "$out"
check "points at the tag log" grep -q "tail -f $TV_SHELL_GS_LOG_DIR/steam-tag.log" <<< "$out"
sleep 3
check "detached watcher tagged the Remote Play window later" grep -q "tag 0xe00010 STEAM_GAME=9004" "$FAKE_X/tag.log"
check "detached watcher log exists" grep -q "tagged 0xe00010" "$TV_SHELL_GS_LOG_DIR/steam-tag.log"
check "baselayer log running" grep -q 'baselayer=\[9004, 769, 9001\]' "$TV_SHELL_GS_LOG_DIR/steam-baselayer.log"
reap

echo "== S5. launch.sh steam --gamepadui"
fresh S5
win 0xa00010 appear=1 name=Steam class=steam "pid=@$FAKE_X/steam.client.pid"
out="$(TV_SHELL_GS_XID_PROBE=0 TV_SHELL_GS_STEAM_WATCH_SECS=2 "$KIT/launch.sh" steam --gamepadui 2>&1)"; rc=$?
check "exit 0" [ "$rc" = 0 ]
check "steam -gamepadui" [ "$(head -1 "$FAKE_X/steam.argv")" = "-gamepadui" ]
check "no baselayer watcher without the flag" not grep -q "baselayer" <<< "$out"
reap

echo "== S6. launch.sh steamlink: not installed -> exit 2; flatpak present -> runs, tagged 9005"
fresh S6
out="$("$KIT/launch.sh" steamlink 2>&1)"; rc=$?
dump "$out"
check "exit 2" [ "$rc" = 2 ]
check "clear message" grep -q "Steam Link is not installed" <<< "$out"
export FAKE_STEAMLINK=1
export WAYLAND_DISPLAY=gamescope-0
win 0xb00010 appear=1 "name=Steam Link" class=steamlink
out="$(TV_SHELL_GS_XID_PROBE=0 TV_SHELL_GS_STEAM_WATCH_SECS=2 "$KIT/launch.sh" steamlink 2>&1)"; rc=$?
dump "$out"; sed 's/^/     env| /' "$FAKE_X/steamlink.env"
check "exit 0" [ "$rc" = 0 ]
check "flatpak run com.valvesoftware.SteamLink" [ "$(paste -sd' ' "$FAKE_X/steamlink.argv")" = "run com.valvesoftware.SteamLink" ]
check "tagged 9005" grep -q "tag 0xb00010 STEAM_GAME=9005" "$FAKE_X/tag.log"
check "base list 9005,9001" [ "$(head -1 "$FAKE_X/root.log")" = "set GAMESCOPECTRL_BASELAYER_APPID=9005,9001" ]
# steamlink shares the steam verb's env: it is a containerized streaming client too
check "steamlink runs with WAYLAND_DISPLAY unset as well" not grep -q '^WAYLAND_DISPLAY=' "$FAKE_X/steamlink.env"
check "steamlink gets the SteamOS streaming_client vars" grep -q -x 'GAMESCOPE_DISPLAY_DISABLED=1' "$FAKE_X/steamlink.env"
unset WAYLAND_DISPLAY
reap

echo "== S7. launch.sh steam env (K9): WAYLAND_DISPLAY unset, SteamOS streaming_client vars set; --no-wsi / --wayland-display"
fresh S7
export WAYLAND_DISPLAY=gamescope-0
win 0xa00010 appear=1 name=Steam class=steam "pid=@$FAKE_X/steam.client.pid"
out="$(TV_SHELL_GS_XID_PROBE=0 TV_SHELL_GS_STEAM_WATCH_SECS=2 "$KIT/launch.sh" steam 2>&1)"; rc=$?
dump "$out"; sed 's/^/     env| /' "$FAKE_X/steam.env"
check "exit 0" [ "$rc" = 0 ]
check "WAYLAND_DISPLAY absent in Steam's env" not grep -q '^WAYLAND_DISPLAY=' "$FAKE_X/steam.env"
check "GAMESCOPE_DISPLAY_DISABLED=1" grep -q -x 'GAMESCOPE_DISPLAY_DISABLED=1' "$FAKE_X/steam.env"
check "GAMESCOPE_ZENITY_DISABLE=1" grep -q -x 'GAMESCOPE_ZENITY_DISABLE=1' "$FAKE_X/steam.env"
check "layer still enabled by default" grep -q -x 'ENABLE_GAMESCOPE_WSI=1' "$FAKE_X/steam.env"
check "launch line shows the env" grep -q "env -u WAYLAND_DISPLAY GAMESCOPE_DISPLAY_DISABLED=1 GAMESCOPE_ZENITY_DISABLE=1 steam -bigpicture" <<< "$out"
reap
fresh S7b; export WAYLAND_DISPLAY=gamescope-0
win 0xa00010 appear=1 name=Steam class=steam "pid=@$FAKE_X/steam.client.pid"
out="$(TV_SHELL_GS_XID_PROBE=0 TV_SHELL_GS_STEAM_WATCH_SECS=2 "$KIT/launch.sh" steam --no-wsi --wayland-display 2>&1)"; rc=$?
sed 's/^/     env| /' "$FAKE_X/steam.env"
check "--wayland-display keeps the variable (A/B shape)" grep -q -x 'WAYLAND_DISPLAY=gamescope-0' "$FAKE_X/steam.env"
check "--no-wsi disables the layer" grep -q -x 'ENABLE_GAMESCOPE_WSI=0' "$FAKE_X/steam.env"
check "--no-wsi sets DISABLE_GAMESCOPE_WSI=1" grep -q -x 'DISABLE_GAMESCOPE_WSI=1' "$FAKE_X/steam.env"
check "no -u left in the launch line when the display is kept" grep -q "env GAMESCOPE_DISPLAY_DISABLED=1 GAMESCOPE_ZENITY_DISABLE=1 ENABLE_GAMESCOPE_WSI=0 DISABLE_GAMESCOPE_WSI=1 steam" <<< "$out"
unset WAYLAND_DISPLAY
reap
# same two flags in the opposite order: env(1) needs every -u BEFORE any
# NAME=VALUE, so the verb builds the list from flags, never in flag order.
fresh S7c; export WAYLAND_DISPLAY=gamescope-0
win 0xa00010 appear=1 name=Steam class=steam "pid=@$FAKE_X/steam.client.pid"
out="$(TV_SHELL_GS_XID_PROBE=0 TV_SHELL_GS_STEAM_WATCH_SECS=2 "$KIT/launch.sh" steam --no-wsi 2>&1)"; rc=$?
dump "$out"; sed 's/^/     env| /' "$FAKE_X/steam.env"
check "exit 0 (env accepted the argument order)" [ "$rc" = 0 ]
check "unset comes first, assignments after" grep -q "env -u WAYLAND_DISPLAY GAMESCOPE_DISPLAY_DISABLED=1 GAMESCOPE_ZENITY_DISABLE=1 ENABLE_GAMESCOPE_WSI=0 DISABLE_GAMESCOPE_WSI=1 steam" <<< "$out"
check "child really ran (argv recorded), so env did not swallow the command" [ "$(head -1 "$FAKE_X/steam.argv")" = "-bigpicture" ]
check "both effects landed" grep -q -x 'ENABLE_GAMESCOPE_WSI=0' "$FAKE_X/steam.env"
check "...and the display is gone" not grep -q '^WAYLAND_DISPLAY=' "$FAKE_X/steam.env"
unset WAYLAND_DISPLAY
reap

echo "== S8. launch.sh x11 --no-wsi --no-wayland-display"
fresh S8
export WAYLAND_DISPLAY=gamescope-0
win 0xa00010 appear=1 name=Steam class=steam "pid=@$FAKE_X/steam.client.pid"
out="$(TV_SHELL_GS_XID_PROBE=0 "$KIT/launch.sh" x11 9004 --name Steam --class steam --no-wsi --no-wayland-display steam -bigpicture 2>&1)"; rc=$?
dump "$out"; sed 's/^/     env| /' "$FAKE_X/steam.env"
check "exit 0" [ "$rc" = 0 ]
check "WAYLAND_DISPLAY absent" not grep -q '^WAYLAND_DISPLAY=' "$FAKE_X/steam.env"
check "layer disabled" grep -q -x 'ENABLE_GAMESCOPE_WSI=0' "$FAKE_X/steam.env"
check "argv intact" [ "$(head -1 "$FAKE_X/steam.argv")" = "-bigpicture" ]
check "tagged 9004" grep -q "tag 0xa00010 STEAM_GAME=9004" "$FAKE_X/tag.log"
unset WAYLAND_DISPLAY
reap
fresh S8b
win 0xa00010 appear=1 name=Steam class=steam "pid=@$FAKE_X/steam.client.pid"
out="$(TV_SHELL_GS_XID_PROBE=0 WAYLAND_DISPLAY=gamescope-0 "$KIT/launch.sh" x11 9004 --name Steam steam 2>&1)"; rc=$?
check "x11 without the flags keeps WAYLAND_DISPLAY and the layer" grep -q -x 'WAYLAND_DISPLAY=gamescope-0' "$FAKE_X/steam.env"
check "..." grep -q -x 'ENABLE_GAMESCOPE_WSI=1' "$FAKE_X/steam.env"
reap
# x11 collects the flags AS THEY COME, so this is the order-independence test:
# --no-wsi first (an assignment) then --no-wayland-display (an unset). env(1)
# would treat the -u as the command name if they were emitted in flag order.
echo "== S8c. launch.sh x11: assignment flag before unset flag still yields a valid env"
fresh S8c
export WAYLAND_DISPLAY=gamescope-0
win 0xa00010 appear=1 name=Steam class=steam "pid=@$FAKE_X/steam.client.pid"
out="$(TV_SHELL_GS_XID_PROBE=0 "$KIT/launch.sh" x11 9004 --no-wsi --no-wayland-display --name Steam --class steam steam -bigpicture 2>&1)"; rc=$?
dump "$out"; sed 's/^/     env| /' "$FAKE_X/steam.env"
check "exit 0" [ "$rc" = 0 ]
check "child ran with its own argv (env did not eat the command)" [ "$(head -1 "$FAKE_X/steam.argv")" = "-bigpicture" ]
check "unset applied" not grep -q '^WAYLAND_DISPLAY=' "$FAKE_X/steam.env"
check "assignment applied" grep -q -x 'ENABLE_GAMESCOPE_WSI=0' "$FAKE_X/steam.env"
check "still tagged" grep -q "tag 0xa00010 STEAM_GAME=9004" "$FAKE_X/tag.log"
unset WAYLAND_DISPLAY
reap

echo "== S9. session.sh TV_SHELL_GS_EXPOSE_WAYLAND (K10)"
fresh S9
out="$(TV_SHELL_GS_DAEMON=0 TV_SHELL_GS_STATS="$FAKE_X/stats" TV_SHELL_GS_ENV_FILE="$FAKE_X/env" TV_SHELL_GS_LOG="$FAKE_X/session.log" "$KIT/session.sh" 2>&1)"
check "default: --expose-wayland passed" grep -q -x -- '--expose-wayland' "$FAKE_X/gamescope.argv"
check "logs expose_wayland=1" grep -q 'expose_wayland=1' "$FAKE_X/session.log"
rm -f "$FAKE_X/gamescope.argv"
out="$(TV_SHELL_GS_EXPOSE_WAYLAND=0 TV_SHELL_GS_DAEMON=0 TV_SHELL_GS_STATS="$FAKE_X/stats" TV_SHELL_GS_ENV_FILE="$FAKE_X/env" TV_SHELL_GS_LOG="$FAKE_X/session.log" "$KIT/session.sh" 2>&1)"
check "=0: flag dropped" not grep -q -x -- '--expose-wayland' "$FAKE_X/gamescope.argv"
check "=0: --steam still passed" grep -q -x -- '--steam' "$FAKE_X/gamescope.argv"
check "logs expose_wayland=0" grep -q 'expose_wayland=0' "$FAKE_X/session.log"

echo
echo "passed=$pass failed=$fail"
[ "$fail" = 0 ]
