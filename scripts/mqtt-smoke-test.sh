#!/usr/bin/env bash
#
# End-to-end MQTT smoke test for tv-shell-host — the committed, repeatable
# version of the manual "does it actually publish?" check.
#
# It owns the broker's whole lifecycle: it brings mosquitto up from the same
# committed compose file the CI job and a developer use (dev/mqtt/compose.yaml),
# and tears it down from an EXIT trap. The mosquitto clients run INSIDE the
# container, so the only things this host needs are `docker` and `cargo` — no
# mosquitto-clients install, no port-forward gymnastics.
#
# What it proves, in order:
#   1. the built binary actually contains the MQTT code (so silence is
#      attributable — a build without MQTT publishes nothing, which reads
#      exactly like "MQTT is broken");
#   2. the capture pipeline works at all, via a canary publish (a subscriber
#      that never started also produces an empty capture file);
#   3. the three frozen topics are published;
#   4. all three are RETAINED — proved with a SECOND, fresh subscriber, because
#      MQTT 3.1.1 clears the RETAIN flag when forwarding to an ALREADY
#      SUBSCRIBED client. A live subscriber can never observe retention;
#   5. the process is alive, then really dies on `kill -9`;
#   6. the broker fires the retained Last Will `offline`.
#
# Every wait has a deadline and dumps the full capture on failure. Run it from
# anywhere:
#
#   ./scripts/mqtt-smoke-test.sh
#
# Override the broker's published port when 1883 is taken locally:
#
#   TV_SHELL_MQTT_PORT=18830 ./scripts/mqtt-smoke-test.sh
#
# The `#[ignore]`-gated Rust harness in host/tests/mqtt_broker.rs asserts the
# same contract in far more detail (document shape, envelope fields, heartbeat
# monotonicity). This script exists because it needs nothing but docker+cargo and
# it exercises the compose file itself.

set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

COMPOSE_FILE="dev/mqtt/compose.yaml"
BROKER_PORT="${TV_SHELL_MQTT_PORT:-1883}"
# Distinct high port: the host binds an axum listener and a bind failure EXITS
# the process, which would present as "MQTT never published".
HOST_HTTP_PORT="${TV_SHELL_SMOKE_HTTP_PORT:-47908}"
# Short knobs keep the run fast. The 30s/60s DEFAULTS are covered by the pure
# env-parsing unit tests in host/src/mqtt.rs; this script covers the mechanism.
HEARTBEAT_SECS=2
KEEPALIVE_SECS=5

# Unique per run so a retained message from an earlier run can never satisfy an
# assertion here (that would be a false PASS, which is worse than a failure).
DEVICE_ID="smoke-$$-$(date +%s)"

# The frozen contract, written out literally rather than computed from the Rust
# topic builders — a builder-vs-builder comparison passes even after the
# contract changes.
DISCOVERY_TOPIC="homeassistant/device/tv-shell-${DEVICE_ID}/config"
AVAIL_TOPIC="tv-shell/${DEVICE_ID}/avail"
STATE_TOPIC="tv-shell/${DEVICE_ID}/state"
CANARY_TOPIC="tv-shell/${DEVICE_ID}/canary"

BIN="target/debug/tv-shell-host"
WORKDIR="$(mktemp -d)"
LIVE="$WORKDIR/live.log"
RETAINED="$WORKDIR/retained.log"
HOST_PID=""
SUB_PID=""
BROKER_UP=0

say() { printf '\n== %s\n' "$*"; }
asserting() { printf '   asserting: %s\n' "$*"; }
ok() { printf '   ok: %s\n' "$*"; }

fail() {
  printf '\nFAILED: %s\n' "$*" >&2
  printf '\n--- live capture (format: "<retain> <topic> <payload>") ---\n' >&2
  if [ -s "$LIVE" ]; then cat "$LIVE" >&2; else printf '  (nothing at all)\n' >&2; fi
  if [ -s "$RETAINED" ]; then
    printf -- '--- retained-replay capture (format: "<retain> <topic>") ---\n' >&2
    cat "$RETAINED" >&2
  fi
  exit 1
}

cleanup() {
  local code=$?
  set +e
  if [ -n "$HOST_PID" ] && kill -0 "$HOST_PID" 2>/dev/null; then
    kill -9 "$HOST_PID"
    wait "$HOST_PID" 2>/dev/null
  fi
  if [ -n "$SUB_PID" ] && kill -0 "$SUB_PID" 2>/dev/null; then
    kill "$SUB_PID" 2>/dev/null
  fi
  if [ "$BROKER_UP" -eq 1 ]; then
    docker compose -f "$COMPOSE_FILE" down -v --remove-orphans >/dev/null 2>&1
  fi
  rm -rf "$WORKDIR"
  exit "$code"
}
trap cleanup EXIT

# wait_for_line <grep-pattern> <timeout-seconds> <description>
#
# Polls the live capture. Fails early (rather than waiting out the full timeout)
# if the host process has died in the meantime — a dead host explains a missing
# publish, and must be reported as that rather than as a mystery timeout.
wait_for_line() {
  local pattern="$1" timeout="$2" desc="$3"
  local ticks=$((timeout * 4)) waited=0
  while [ "$waited" -lt "$ticks" ]; do
    if grep -q -- "$pattern" "$LIVE" 2>/dev/null; then
      ok "$desc"
      return 0
    fi
    if [ -n "$HOST_PID" ] && ! kill -0 "$HOST_PID" 2>/dev/null; then
      fail "the host process (pid $HOST_PID) exited while waiting for $desc — it published nothing after that, so this wait could never have succeeded. Its log is above."
    fi
    sleep 0.25
    waited=$((waited + 1))
  done
  fail "timed out after ${timeout}s waiting for $desc"
}

printf 'tv-shell MQTT smoke test\n'
printf '  repo root:  %s\n' "$ROOT"
printf '  device_id:  %s\n' "$DEVICE_ID"
printf '  broker:     mqtt://127.0.0.1:%s (from %s)\n' "$BROKER_PORT" "$COMPOSE_FILE"
printf '  host http:  127.0.0.1:%s\n' "$HOST_HTTP_PORT"

# ── 1. Build, and prove the binary has MQTT in it ────────────────────────────
say "Building the host sidecar"
cargo build -p tv-shell-host

[ -x "$BIN" ] || fail "$BIN is missing or not executable after a successful build"
asserting "the built binary carries the MQTT env surface (a build without MQTT support publishes nothing, and silence must be attributable before anything below rests on it)"
grep -aq 'TV_SHELL_MQTT_BROKER' "$BIN" ||
  fail "$BIN contains no 'TV_SHELL_MQTT_BROKER' string, so this build has no MQTT support. Every assertion below would be resting on silence from a binary that was never going to publish."
ok "$BIN has MQTT support"

# ── 2. Broker ────────────────────────────────────────────────────────────────
say "Starting mosquitto from $COMPOSE_FILE"
docker compose -f "$COMPOSE_FILE" up -d --wait
BROKER_UP=1
docker compose -f "$COMPOSE_FILE" ps

# ── 3. Live subscriber, inside the container ─────────────────────────────────
say "Subscribing (inside the container) BEFORE the host starts"
# -F '%r %t %p': %r is the RETAINED flag, %t the topic, %p the payload — see
# mosquitto_sub(1). mosquitto_sub flushes stdout per message, so the capture file
# stays live rather than sitting in a block buffer.
docker compose -f "$COMPOSE_FILE" exec -T mosquitto \
  mosquitto_sub -h 127.0.0.1 -p 1883 -q 1 -F '%r %t %p' \
  -t "tv-shell/${DEVICE_ID}/#" -t "$DISCOVERY_TOPIC" >"$LIVE" 2>&1 &
SUB_PID=$!

asserting "the capture pipeline works at all — a subscriber that never started also produces an empty file, which would read exactly like 'the host published nothing'"
canary_ok=0
for _ in $(seq 1 20); do
  docker compose -f "$COMPOSE_FILE" exec -T mosquitto \
    mosquitto_pub -h 127.0.0.1 -p 1883 -t "$CANARY_TOPIC" -m ready >/dev/null 2>&1 || true
  if grep -q -- " ${CANARY_TOPIC} " "$LIVE" 2>/dev/null; then
    canary_ok=1
    break
  fi
  sleep 0.5
done
[ "$canary_ok" -eq 1 ] ||
  fail "the canary publish on $CANARY_TOPIC never reached the capture file after 10s — the subscriber is not running or its output is not landing, so nothing below could be concluded from an empty capture"
ok "the canary round-tripped; the capture pipeline is live"

# ── 4. Host ──────────────────────────────────────────────────────────────────
say "Starting the host sidecar against the broker"
TV_SHELL_MQTT_BROKER="mqtt://127.0.0.1:${BROKER_PORT}" \
  TV_SHELL_MQTT_DEVICE_ID="$DEVICE_ID" \
  TV_SHELL_MQTT_HEARTBEAT_SECS="$HEARTBEAT_SECS" \
  TV_SHELL_MQTT_KEEPALIVE_SECS="$KEEPALIVE_SECS" \
  TV_SHELL_HOST_PORT="$HOST_HTTP_PORT" \
  TV_SHELL_HOST_BIND=127.0.0.1 \
  TV_SHELL_HOST_TOKEN=mqtt-smoke-test \
  RUST_LOG=info \
  "$BIN" &
HOST_PID=$!
printf '   host pid %s\n' "$HOST_PID"

say "Checking the three frozen topics"
asserting "the host publishes to exactly these literal topics"
printf '     %s\n     %s\n     %s\n' "$DISCOVERY_TOPIC" "$AVAIL_TOPIC" "$STATE_TOPIC"
wait_for_line " ${DISCOVERY_TOPIC} " 40 "a publish on $DISCOVERY_TOPIC"
wait_for_line " ${AVAIL_TOPIC} online" 40 "the 'online' birth message on $AVAIL_TOPIC"
wait_for_line " ${STATE_TOPIC} " 40 "a publish on $STATE_TOPIC"

asserting "a LIVE forward carries retain=0 — MQTT 3.1.1 clears RETAIN when forwarding to an already-subscribed client, so retention is proved separately below"
for topic in "$DISCOVERY_TOPIC" "$AVAIL_TOPIC" "$STATE_TOPIC"; do
  grep -q -- "^0 ${topic} " "$LIVE" ||
    fail "no live (retain=0) delivery of $topic in the capture. If the topic IS there with a different leading field, the -F format specifier is wrong: %r is the retained flag in mosquitto_sub(1)."
done
ok "all three arrived as live forwards with retain=0"

# ── 5. Retained flags, via a FRESH subscriber ────────────────────────────────
say "Retained-flag check with a FRESH subscriber"
asserting "a subscriber connecting AFTER the publishes receives all three as retained replays (retain=1) — this is the only way retention is observable"
docker compose -f "$COMPOSE_FILE" exec -T mosquitto \
  mosquitto_sub -h 127.0.0.1 -p 1883 -F '%r %t' -C 3 -W 15 \
  -t "$DISCOVERY_TOPIC" -t "$AVAIL_TOPIC" -t "$STATE_TOPIC" >"$RETAINED" 2>&1 || true

replays="$(grep -c '' "$RETAINED" 2>/dev/null || true)"
[ "${replays:-0}" -eq 3 ] ||
  fail "expected exactly 3 retained replays to the fresh subscriber, got ${replays:-0}. Fewer means at least one of the three is not retained on the broker, so a consumer that subscribes later (Home Assistant restarting, say) would see nothing at all."

while read -r flag topic; do
  case "$flag" in
  1) : ;;
  0) fail "$topic was replayed to a FRESH subscriber with retain=0, so it is NOT retained on the broker" ;;
  *) fail "unexpected leading field '$flag' (topic '$topic') in the retained capture — is %r still the retained flag in mosquitto_sub(1)?" ;;
  esac
done <"$RETAINED"

for topic in "$DISCOVERY_TOPIC" "$AVAIL_TOPIC" "$STATE_TOPIC"; do
  grep -q -- "^1 ${topic}$" "$RETAINED" ||
    fail "no retained replay of $topic to the fresh subscriber"
done
ok "discovery, availability and state are all retained"

# ── 6. Ungraceful kill ───────────────────────────────────────────────────────
say "Ungraceful kill and Last Will"
asserting "the host process is actually alive before the kill — a kill that kills nothing exits 0 and leaves us waiting out a Last Will that had no reason to fire"
kill -0 "$HOST_PID" 2>/dev/null ||
  fail "the host process (pid $HOST_PID) had ALREADY exited before the kill. It crashed; check its log above. Killing nothing and then waiting for a Last Will would have proved nothing."
ok "pid $HOST_PID is alive"

kill -9 "$HOST_PID"
# Reap it before polling: a killed background job stays a ZOMBIE until waited on,
# and `kill -0` SUCCEEDS on a zombie — polling without reaping would report the
# process as alive forever.
wait "$HOST_PID" 2>/dev/null || true

asserting "the process is really gone (poll until kill -0 fails)"
gone=0
for _ in $(seq 1 40); do
  if ! kill -0 "$HOST_PID" 2>/dev/null; then
    gone=1
    break
  fi
  sleep 0.25
done
[ "$gone" -eq 1 ] ||
  fail "the host process (pid $HOST_PID) is STILL alive 10s after kill -9"
ok "the host process is gone"
# Clear it so wait_for_line below does not fail on the (now expected) dead host.
HOST_PID=""

asserting "the broker publishes the retained Last Will 'offline' on $AVAIL_TOPIC (it waits ~1.5x keepalive = ~$((KEEPALIVE_SECS * 3 / 2))s after the socket dies)"
wait_for_line " ${AVAIL_TOPIC} offline" 45 "the Last Will 'offline' on $AVAIL_TOPIC"

say "PASSED"
printf '  device_id %s: three frozen topics published, all three retained, Last Will fired on an ungraceful kill.\n' "$DEVICE_ID"
