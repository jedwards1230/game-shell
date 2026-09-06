#!/bin/bash
# Fixture test for the gamescope kit's pid->xid tagging (K6), the streaming-host
# pre-check (K7) and verbatim app names (K8). Fake xprop/curl/moonlight/qml6
# under bin/ replay the phase-3 window list: two windows for one pid, the
# stream window appearing on the third poll.
#
#   run.sh <path-to-dev/gamescope>
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
# Every app verb now launches its client inside a systemd scope, and
# `systemd-run --user` needs a session bus. Both variables point into the
# scratch dir: bin/systemd-run is a stub, but gs_scope_ready checks the
# environment before calling it, and a fixture must not inherit (or depend on)
# the developer's live user manager.
export XDG_RUNTIME_DIR="$WORK/run"; mkdir -p "$XDG_RUNTIME_DIR"
export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"
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
export TV_SHELL_GS_ENV_FILE=/nonexistent
export DISPLAY=:9
# Moonlight's config is read-only fixture data, but the fake `moonlight quit`
# rewrites the serverinfo it pairs with, so the conf is copied into the scratch
# dir rather than read out of the checkout.
cp "$HERE/Moonlight.conf" "$WORK/Moonlight.conf"
export TV_SHELL_GS_MOONLIGHT_CONF="$WORK/Moonlight.conf"
export TV_SHELL_GS_MOONLIGHT_TIMEOUT=5
pass=0; fail=0
ok()   { pass=$((pass + 1)); echo "  ok   $*"; }
bad()  { fail=$((fail + 1)); echo "  FAIL $*"; }
# shellcheck disable=SC2001  # a per-line prefix is what sed is for
dump() { printf '%s\n' "$1" | sed 's/^/     | /'; }   # echo a captured run, indented
check() { # check <desc> <cmd...>
    local d="$1"; shift
    if "$@" >/dev/null 2>&1; then ok "$d"; else bad "$d"; fi
}
fresh() { # fresh <name> -> new FAKE_X dir
    export FAKE_X="$WORK/state/$1"
    rm -rf "$FAKE_X"; mkdir -p "$FAKE_X/win" "$FAKE_X/tags"
    : > "$FAKE_X/calls.log"; : > "$FAKE_X/tag.log"; : > "$FAKE_X/root.log"
    unset FAKE_WSI_LOG FAKE_SERVERINFO
}
win() { # win <xid> key=value...
    local x="$1"; shift
    printf '%s\n' "$@" > "$FAKE_X/win/$x"
}
not() { ! "$@"; }
spawn_pid() { sleep 300 >/dev/null 2>&1 & track $!; echo $!; }

echo "== A. lead's spec: two windows, same pid, stream window on poll 3 (WSI log)"
fresh A; P=$(spawn_pid); export FAKE_WSI_LOG="$FAKE_X/wsi.log"; : > "$FAKE_WSI_LOG"
win 0x80002f appear=1 name=Moonlight class=moonlight "pid=$P"
win 0x800031 appear=3 "name=stream-host - Moonlight" class=moonlight "pid=$P" wsi=1
out="$(TV_SHELL_GS_XID_PROBE=0 "$KIT/focus.sh" tag-pid "$P" 9003 --class moonlight --log "$FAKE_WSI_LOG" --name Moonlight --done-name '* - Moonlight' --timeout 20 2>&1)"; rc=$?
dump "$out"
check "exit 0" [ "$rc" = 0 ]
check "GUI window tagged (by name)" grep -q "tag 0x80002f STEAM_GAME=9003" "$FAKE_X/tag.log"
check "stream window tagged" grep -q "poll=3 tag 0x800031 STEAM_GAME=9003" "$FAKE_X/tag.log"
check "stream window tagged once" [ "$(grep -c 'tag 0x800031' "$FAKE_X/tag.log")" = 1 ]
check "GUI window tagged once" [ "$(grep -c 'tag 0x80002f' "$FAKE_X/tag.log")" = 1 ]
check "watch stopped at the stream window (no more polls)" [ "$(cat "$FAKE_X/poll")" = 3 ]
check "printed the stream window with its name" grep -q 'tagged 0x800031 "stream-host - Moonlight" STEAM_GAME=9003' <<< "$out"
kill "$P" 2>/dev/null

echo "== B. phase 3 as observed: the 'Moonlight' window has no _NET_WM_PID, stream via WSI log"
fresh B; P=$(spawn_pid); export FAKE_WSI_LOG="$FAKE_X/wsi.log"; : > "$FAKE_WSI_LOG"
win 0x80002f appear=1 name=Moonlight class=moonlight
win 0x800031 appear=3 "name=stream-host - Moonlight" class=moonlight "pid=$P" wsi=1
out="$("$KIT/focus.sh" tag-pid "$P" 9003 --class moonlight --log "$FAKE_WSI_LOG" --name Moonlight --done-name '* - Moonlight' --timeout 20 2>&1)"; rc=$?
dump "$out"
check "exit 0" [ "$rc" = 0 ]
check "GUI window tagged by class" grep -q "tag 0x80002f STEAM_GAME=9003" "$FAKE_X/tag.log"
check "stream window tagged on poll 3" grep -q "poll=3 tag 0x800031" "$FAKE_X/tag.log"
kill "$P" 2>/dev/null

echo "== C. no WSI log: the stream window is found by neighbour probing from the tagged one"
fresh C; P=$(spawn_pid)
win 0x80002f appear=1 name=Moonlight class=moonlight "pid=$P"
win 0x800031 appear=3 "name=stream-host - Moonlight" class=moonlight "pid=$P"
win 0x800030 appear=2 name=leader-ish  # a window of the same client that matches nothing
out="$("$KIT/focus.sh" tag-pid "$P" 9003 --class moonlight --name Moonlight --done-name '* - Moonlight' --timeout 20 2>&1)"; rc=$?
dump "$out"
check "exit 0" [ "$rc" = 0 ]
check "stream window tagged on poll 3 via probe" grep -q "poll=3 tag 0x800031" "$FAKE_X/tag.log"
check "unrelated neighbour never tagged" not grep -q "0x800030" "$FAKE_X/tag.log"
check "probe bounded: < 200 xprop calls" [ "$(wc -l < "$FAKE_X/calls.log")" -lt 200 ]
kill "$P" 2>/dev/null

echo "== D. stale title from a previous instance is never tagged (client.sh case)"
fresh D; P=$(spawn_pid)
win 0x400011 appear=1 vanish=3 name=tv-shell-proto pid=111
win 0x600011 appear=2 name=tv-shell-proto "pid=$P"
out="$("$KIT/focus.sh" tag-pid "$P" 9001 --name tv-shell-proto --expect 1 --timeout 10 2>&1)"; rc=$?
dump "$out"
check "exit 0" [ "$rc" = 0 ]
check "new shell tagged" grep -q "tag 0x600011 STEAM_GAME=9001" "$FAKE_X/tag.log"
check "stale window never tagged" not grep -q "0x400011" "$FAKE_X/tag.log"
kill "$P" 2>/dev/null

echo "== E. pid gone before any window -> exit 1; re-run on tagged windows -> known, exit 0"
fresh E; P=$(spawn_pid); kill "$P"; wait "$P" 2>/dev/null
out="$("$KIT/focus.sh" tag-pid "$P" 9003 --timeout 3 2>&1)"; rc=$?
check "dead pid exits 1" [ "$rc" = 1 ]
check "says so" grep -q "exited before any window" <<< "$out"
P=$(spawn_pid)
win 0x800031 appear=1 "name=stream-host - Moonlight" class=moonlight "pid=$P"
echo 9003 > "$FAKE_X/tags/0x800031"
out="$("$KIT/focus.sh" tag-pid "$P" 9003 --expect 1 --timeout 5 2>&1)"; rc=$?
check "already-tagged window reported known, exit 0" [ "$rc" = 0 ]
check "no re-tag" [ ! -s "$FAKE_X/tag.log" ]
check "known line" grep -q 'known 0x800031' <<< "$out"
kill "$P" 2>/dev/null

echo "== F. launch.sh moonlight: host BUSY with a different app -> REFUSED, nothing spawned"
fresh F; export FAKE_SERVERINFO="$FAKE_X/serverinfo.xml"
printf '%s' '<root status_code="200"><hostname>stream-host</hostname><appversion>7.1.431.-1</appversion><PairStatus>0</PairStatus><currentgame>1068023197</currentgame><state>SUNSHINE_SERVER_BUSY</state></root>' > "$FAKE_SERVERINFO"
out="$("$KIT/launch.sh" moonlight stream stream-host ' Desktop' --hdr 2>&1)"; rc=$?
dump "$out"
check "exit 3" [ "$rc" = 3 ]
check "names the running app" grep -q "with ' Steam Big Picture' (app id 1068023197), not ' Desktop'" <<< "$out"
check "offers resume with the exact name" grep -q "stream stream-host ' Steam Big Picture'" <<< "$out"
check "offers --quit" grep -q -- "--quit stream stream-host ' Desktop'" <<< "$out"
check "moonlight never spawned" [ ! -e "$FAKE_X/moonlight.argv" ]
check "no scope created" [ ! -e "$FAKE_X/scopes.log" ]
check "no base-layer change" [ ! -s "$FAKE_X/root.log" ]

echo "== G. launch.sh moonlight: host already running the requested app -> resume, tag, base layer"
fresh G; export FAKE_SERVERINFO="$FAKE_X/serverinfo.xml"; export FAKE_WSI_LOG="$TV_SHELL_GS_LOG_DIR/moonlight.log"
printf '%s' '<root status_code="200"><hostname>stream-host</hostname><currentgame>1068023197</currentgame><state>SUNSHINE_SERVER_BUSY</state></root>' > "$FAKE_SERVERINFO"
win 0x80002f appear=1 name=Moonlight class=moonlight
win 0x800031 appear=3 "name=stream-host - Moonlight" class=moonlight "pid=@$FAKE_X/moonlight.pid" wsi=1
# the fake moonlight records its pid via the argv file's writer: use a wrapper
cat > "$FAKE_X/moonlight-wrap" <<'EOF'
#!/bin/bash
echo $$ > "$FAKE_X/moonlight.pid"
exec moonlight "$@"
EOF
chmod +x "$FAKE_X/moonlight-wrap"
out="$(TV_SHELL_GS_XID_PROBE=0 TV_SHELL_GS_MOONLIGHT="$FAKE_X/moonlight-wrap" "$KIT/launch.sh" moonlight stream stream-host ' Steam Big Picture' --resolution 3840x2160 --fps 120 --hdr 2>&1)"; rc=$?
dump "$out"
check "exit 0" [ "$rc" = 0 ]
check "says resume" grep -q "already running ' Steam Big Picture' (id 1068023197); Moonlight will resume it" <<< "$out"
check "app name passed verbatim with its leading space" [ "$(sed -n 3p "$FAKE_X/moonlight.argv")" = " Steam Big Picture" ]
check "moonlight args intact" grep -q -x -- '--hdr' "$FAKE_X/moonlight.argv"
check "base layer 9003,9001 set before tagging" [ "$(head -1 "$FAKE_X/root.log")" = "set GAMESCOPECTRL_BASELAYER_APPID=9003,9001" ]
check "stream window tagged" grep -q "tag 0x800031 STEAM_GAME=9003" "$FAKE_X/tag.log"
check "GUI window tagged" grep -q "tag 0x80002f STEAM_GAME=9003" "$FAKE_X/tag.log"
# The scope is the app id gamescope actually reads; the tags above are repair.
check "launched in one scope" [ "$(wc -l < "$FAKE_X/scopes.log")" = 1 ]
check "scope named for app 9003, in gamescope's sscanf format" grep -qE '^app-steam-app9003-[0-9]+$' "$FAKE_X/scopes.log"
check "scope's pid field is the pid the kit reported" grep -qE "^app-steam-app9003-$(grep -oE 'moonlight \(xcb\) pid [0-9]+' <<< "$out" | awk '{print $NF}')\$" "$FAKE_X/scopes.log"
# -e, because grep would read a pattern starting with `--` as an option.
check "systemd-run got --user --scope --collect" grep -qF -e $'--user\t--scope\t--collect' "$FAKE_X/systemd-run.argv"
reap

echo "== H. launch.sh moonlight --quit: ends the host's app first, then streams"
fresh H; export FAKE_SERVERINFO="$FAKE_X/serverinfo.xml"; export FAKE_WSI_LOG="$TV_SHELL_GS_LOG_DIR/moonlight.log"
printf '%s' '<root status_code="200"><hostname>stream-host</hostname><currentgame>1068023197</currentgame><state>SUNSHINE_SERVER_BUSY</state></root>' > "$FAKE_SERVERINFO"
win 0x800031 appear=1 "name=stream-host - Moonlight" class=moonlight wsi=1
out="$(TV_SHELL_GS_XID_PROBE=0 "$KIT/launch.sh" moonlight --quit stream stream-host ' Desktop' 2>&1)"; rc=$?
dump "$out"
check "exit 0" [ "$rc" = 0 ]
check "quit sent first" [ "$(head -1 "$FAKE_X/moonlight.log")" = "quit sent" ]
check "then streamed ' Desktop'" grep -q "streaming stream-host \[ Desktop\]" "$FAKE_X/moonlight.log"
check "host reported idle after quit" grep -q "streaming host: idle" <<< "$out"
reap

echo "== I. launch.sh moonlight: serverinfo unreachable -> warn, stream anyway"
fresh I; export FAKE_WSI_LOG="$TV_SHELL_GS_LOG_DIR/moonlight.log"
win 0x800031 appear=1 "name=stream-host - Moonlight" class=moonlight wsi=1
out="$(TV_SHELL_GS_XID_PROBE=0 "$KIT/launch.sh" moonlight stream stream-host ' Desktop' 2>&1)"; rc=$?
check "exit 0" [ "$rc" = 0 ]
check "warned" grep -q "WARN: no usable serverinfo" <<< "$out"
check "streamed" [ -e "$FAKE_X/moonlight.argv" ]
reap

echo "== J. launch.sh apps: quoted names, running marker, live list"
fresh J; export FAKE_SERVERINFO="$FAKE_X/serverinfo.xml"
printf '%s' '<root status_code="200"><hostname>stream-host</hostname><currentgame>1068023197</currentgame><state>SUNSHINE_SERVER_BUSY</state></root>' > "$FAKE_SERVERINFO"
out="$("$KIT/launch.sh" apps stream-host 2>&1)"; rc=$?
dump "$out"
check "exit 0" [ "$rc" = 0 ]
check "state line" grep -q "streaming host 'stream-host': SUNSHINE_SERVER_BUSY, running ' Steam Big Picture' (app id 1068023197)" <<< "$out"
check "cached names quoted with the space" grep -q -F "    ' Desktop'" <<< "$out"
check "running marker" grep -q -F "    ' Steam Big Picture'   <- running now" <<< "$out"
check "live list quoted" grep -q -F "    'Factorio'" <<< "$out"
check "other host's apps not mixed in" [ "$(grep -c -F "' Desktop'" <<< "$out")" = 2 ]

echo "== K. client.sh tags the shell by pid (title is only a hint)"
fresh K; export TV_SHELL_GS_ENV_FILE="$FAKE_X/env"
win 0x400011 appear=2 name=tv-shell-proto "pid=@$FAKE_X/shell.pid"
out="$(timeout 6 "$KIT/client.sh" 2>&1)"
dump "$out"
check "shell tagged by pid" grep -q "tagged 'tv-shell-proto' (pid $(cat "$FAKE_X/shell.pid")) as app 9001 (base layer 9001, set at launch)" <<< "$out"
check "STEAM_GAME set on its window" grep -q "tag 0x400011 STEAM_GAME=9001" "$FAKE_X/tag.log"
check "base layer 9001" grep -q "set GAMESCOPECTRL_BASELAYER_APPID=9001" "$FAKE_X/root.log"
check "shell launched in a scope named for app 9001" grep -qE '^app-steam-app9001-[0-9]+$' "$FAKE_X/scopes.log"
# The measurement rig's default must not move when a second client mode exists.
check "default primary child is proto" grep -q "primary child: proto" <<< "$out"
check "no moonlight spawned by default" [ ! -e "$FAKE_X/moonlight.argv" ]
reap

echo "== L. garbage serverinfo (HTML error page) -> treated as unreachable, not idle"
fresh L; export FAKE_SERVERINFO="$FAKE_X/serverinfo.xml"; export FAKE_WSI_LOG="$TV_SHELL_GS_LOG_DIR/moonlight.log"
printf '%s' '<html><head><title>502 Bad Gateway</title></head><body>proxy error</body></html>' > "$FAKE_SERVERINFO"
win 0x800031 appear=1 "name=stream-host - Moonlight" class=moonlight wsi=1
out="$(TV_SHELL_GS_XID_PROBE=0 "$KIT/launch.sh" moonlight stream stream-host ' Desktop' 2>&1)"; rc=$?
dump "$out"
check "exit 0" [ "$rc" = 0 ]
check "warned, not 'idle'" grep -q "WARN: no usable serverinfo" <<< "$out"
check "never claimed idle" not grep -q "streaming host: idle" <<< "$out"
reap

echo "== M. a window reached by name AND by xid is counted once: --expect 2 waits for the second window"
fresh M; P=$(spawn_pid)
win 0x80002f appear=1 name=Moonlight class=moonlight "pid=$P"
win 0x800031 appear=4 "name=stream-host - Moonlight" class=moonlight "pid=$P"
out="$("$KIT/focus.sh" tag-pid "$P" 9003 --class moonlight --name Moonlight --expect 2 --timeout 20 2>&1)"; rc=$?
dump "$out"
check "exit 0" [ "$rc" = 0 ]
check "second window tagged (watch did not stop early)" grep -q "tag 0x800031 STEAM_GAME=9003" "$FAKE_X/tag.log"
check "first window tagged once" [ "$(grep -c 'tag 0x80002f' "$FAKE_X/tag.log")" = 1 ]
kill "$P" 2>/dev/null

echo "== N. client.sh TV_SHELL_GS_CLIENT=moonlight: Moonlight is the primary child"
fresh N; export TV_SHELL_GS_ENV_FILE="$FAKE_X/env"
export FAKE_WSI_LOG="$TV_SHELL_GS_LOG_DIR/moonlight.log"
# The fake moonlight cannot write its own pid where the window fixture can see
# it (it is exec'd), so a wrapper records it first — as section G does.
cat > "$FAKE_X/moonlight-wrap" <<'EOF'
#!/bin/bash
echo $$ > "$FAKE_X/moonlight.pid"
exec moonlight "$@"
EOF
chmod +x "$FAKE_X/moonlight-wrap"
win 0x800040 appear=2 name=Moonlight class=moonlight "pid=@$FAKE_X/moonlight.pid"
# The stream window: a SECOND window of the same pid, minutes later in real
# life. It used to be the reason the moonlight watch ran for the life of the
# client. It no longer needs a tag at all — it is a window of a process inside
# the app's scope, which is what gamescope reads — so the repair pass stops at
# the first window and this one is deliberately never reached.
win 0x800041 appear=6 "name=stream-host - Moonlight" class=moonlight "pid=@$FAKE_X/moonlight.pid" wsi=1
out="$(TV_SHELL_GS_CLIENT=moonlight TV_SHELL_GS_MOONLIGHT="$FAKE_X/moonlight-wrap" \
    TV_SHELL_GS_TAG_TIMEOUT=5 TV_SHELL_GS_XID_PROBE=0 timeout 7 "$KIT/client.sh" 2>&1)"
dump "$out"
check "logs the primary child" grep -q "primary child: moonlight" <<< "$out"
check "moonlight spawned" [ -e "$FAKE_X/moonlight.argv" ]
check "no proto shell spawned" [ ! -e "$FAKE_X/shell.pid" ]
check "no qml runtime resolved" not grep -q "qml runtime:" <<< "$out"
check "base layer 9003" grep -q "set GAMESCOPECTRL_BASELAYER_APPID=9003" "$FAKE_X/root.log"
check "launched in a scope named for app 9003" grep -qE '^app-steam-app9003-[0-9]+$' "$FAKE_X/scopes.log"
check "GUI window tagged 9003 (repair pass)" grep -q "tag 0x800040 STEAM_GAME=9003" "$FAKE_X/tag.log"
check "repair pass stopped at the first window" [ "$(wc -l < "$FAKE_X/tag.log")" = 1 ]
check "no day-long watcher announced" not grep -q "watching its windows for" <<< "$out"
check "nothing tagged as the proto shell" not grep -q "STEAM_GAME=9001" "$FAKE_X/tag.log"
reap

echo "== Q. no session bus -> a clear refusal, not a silent unscoped launch"
fresh Q; export TV_SHELL_GS_ENV_FILE="$FAKE_X/env"
out="$(env -u DBUS_SESSION_BUS_ADDRESS -u XDG_RUNTIME_DIR PATH="$PATH" HOME="$HOME" \
    FAKE_X="$FAKE_X" TV_SHELL_GS_ENV_FILE="$FAKE_X/env" TV_SHELL_GS_LOG_DIR="$TV_SHELL_GS_LOG_DIR" \
    DISPLAY=:9 timeout 6 "$KIT/client.sh" 2>&1)"; rc=$?
dump "$out"
check "exit 2" [ "$rc" = 2 ]
check "names the missing variable" grep -q "XDG_RUNTIME_DIR is unset" <<< "$out"
check "says why it matters" grep -q "cannot create a systemd scope" <<< "$out"
check "nothing spawned" [ ! -e "$FAKE_X/shell.pid" ]
check "no scope created" [ ! -e "$FAKE_X/scopes.log" ]

echo "== R. every launched verb gets a scope whose app id is the verb's app id"
fresh R
win 0x900011 appear=1 name=xmessage class=xmessage
out="$("$KIT/launch.sh" xmessage hello 2>&1)"; rc=$?
dump "$out"
check "exit 0" [ "$rc" = 0 ]
check "xmessage scoped as app 9002" grep -qE '^app-steam-app9002-[0-9]+$' "$FAKE_X/scopes.log"
reap
fresh R2
win 0x910011 appear=1 name=probe class=probe
out="$("$KIT/launch.sh" x11 9007 --name probe --class probe sleep 30 2>&1)"; rc=$?
dump "$out"
check "exit 0" [ "$rc" = 0 ]
check "x11 verb scoped as the app id it was given" grep -qE '^app-steam-app9007-[0-9]+$' "$FAKE_X/scopes.log"
check "the scope's app id is not a hardcoded one" not grep -qE '^app-steam-app900[1-5]-' "$FAKE_X/scopes.log"
reap

echo "== O. client.sh refuses an unknown TV_SHELL_GS_CLIENT instead of guessing"
fresh O; export TV_SHELL_GS_ENV_FILE="$FAKE_X/env"
out="$(TV_SHELL_GS_CLIENT=bigpicture timeout 6 "$KIT/client.sh" 2>&1)"; rc=$?
dump "$out"
check "exit 2" [ "$rc" = 2 ]
check "names the valid values" grep -q "is not one of: proto, moonlight" <<< "$out"
check "nothing spawned" [ ! -e "$FAKE_X/shell.pid" ]
check "no base-layer change" [ ! -s "$FAKE_X/root.log" ]

echo "== P. session.sh: the input daemon default follows the primary child (the pad grab)"
fresh P
SESS_ENV=(TV_SHELL_GS_STATS="$FAKE_X/stats" TV_SHELL_GS_ENV_FILE="$FAKE_X/env")
out="$(env "${SESS_ENV[@]}" TV_SHELL_GS_CLIENT=moonlight TV_SHELL_GS_LOG="$FAKE_X/s-moon.log" "$KIT/session.sh" 2>&1)"
dump "$out"
check "moonlight: daemon off by default" grep -q 'client=moonlight .* daemon=0 ' "$FAKE_X/s-moon.log"
check "moonlight: tv-shell-input.service never started" not grep -q 'start tv-shell-input.service' "$FAKE_X/systemctl.log"
check "moonlight: no pad-grab warning when it is off" not grep -q 'WARN: TV_SHELL_GS_DAEMON=1' "$FAKE_X/s-moon.log"
check "client handed to gamescope's child" [ "$(cat "$FAKE_X/gamescope.client")" = moonlight ]
rm -f "$FAKE_X/systemctl.log"
out="$(env "${SESS_ENV[@]}" TV_SHELL_GS_LOG="$FAKE_X/s-proto.log" "$KIT/session.sh" 2>&1)"
check "proto (default): daemon on by default" grep -q 'client=proto .* daemon=1 ' "$FAKE_X/s-proto.log"
check "proto: tv-shell-input.service started" grep -q 'start tv-shell-input.service' "$FAKE_X/systemctl.log"
check "proto handed to gamescope's child" [ "$(cat "$FAKE_X/gamescope.client")" = proto ]
rm -f "$FAKE_X/systemctl.log"
out="$(env "${SESS_ENV[@]}" TV_SHELL_GS_CLIENT=moonlight TV_SHELL_GS_DAEMON=1 TV_SHELL_GS_LOG="$FAKE_X/s-both.log" "$KIT/session.sh" 2>&1)"
check "moonlight + explicit DAEMON=1 is still honoured" grep -q 'client=moonlight .* daemon=1 ' "$FAKE_X/s-both.log"
check "and it says the pad will be D-pad only" grep -q 'WARN: TV_SHELL_GS_DAEMON=1 with client=moonlight' "$FAKE_X/s-both.log"
check "moonlight + explicit DAEMON=1 starts it" grep -q 'start tv-shell-input.service' "$FAKE_X/systemctl.log"

echo
echo "passed=$pass failed=$fail"
[ "$fail" = 0 ]
