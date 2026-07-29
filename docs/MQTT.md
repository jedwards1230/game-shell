# MQTT State & Command Surface

Both tv-shell processes publish to MQTT over **their own** broker connection, so
each carries its **own** Last Will and availability is a fact the broker asserts
rather than something a consumer probes for. Neither proxies for the other. Off
by default on both.

| Device | `device_id` | Binary | `status` payload |
|---|---|---|---|
| the TV client | `htpc-1` | `tv-shell-input` (`daemon/`) | `ShellSnapshot` |
| the gaming PC (dual-boot, ONE machine) | `desktop` | `tv-shell-host` (`host/`) | `StatusResponse` verbatim |

> **Status: both devices are live and their discovery entities exist in Home
> Assistant.** What is still deferred is the rest of the cutover — retiring the
> `rest:` poller entities, repointing the four automations (including the AVR
> safety-condition template) and updating the `dashboard-rooms` Office tiles.
>
> Because the entities are live, **changing the component set now churns real
> entities**: the discovery message is retained, so adding or removing one
> rewrites it and Home Assistant adds/removes the entity on the spot. Treat a
> `cmps` change as a deployment, not a refactor.

## Topics and identity — a contract

```text
tv-shell/<device_id>/state                        retained      device → broker
tv-shell/<device_id>/avail                        retained      LWT: "online" / "offline"
tv-shell/<device_id>/cmd/<name>                   not retained  broker → device
homeassistant/device/tv-shell-<device_id>/config  retained      discovery

HA device identifier : tv-shell-<device_id>
entity unique_id     : tv-shell-<device_id>-<entity_key>
```

Every builder lives in `protocol/src/mqtt.rs`, so the daemon and the sidecar
cannot drift. `DeviceId` restricts the id to `[A-Za-z0-9_-]`, ≤ 64 bytes — `/`,
`+`, `#` and `$` can never reach a topic.

These strings are a **contract**, because three of the four messages are
retained:

- changing `device_id` orphans the old Home Assistant device, and its retained
  state + discovery stay on the broker;
- changing the `unique_id` scheme registers duplicate `…_2` entities beside the
  originals;
- changing the namespace leaves retained ghosts that nothing publishes to.

**Remove a retained message by publishing an empty retained payload to its
topic** — never by deleting the entity in the Home Assistant UI, which leaves the
retained config to re-create it on the next restart.

## One envelope, two status shapes

One retained message on one topic per device, so a consumer never sees a torn
read across topics:

```jsonc
{
  "schema_version": 1,
  "published_at": 1785109000,  // unix seconds
  "seq": 4213,                 // monotonic per process, starts at 0
  "current_os": "linux",       // "linux" | "windows"
  "status": { /* ShellSnapshot or StatusResponse */ }
}
```

`status` differs by device on purpose: the sidecar already has a canonical
three-field `StatusResponse` (`version`, `running_appid`, `streaming`) that the
daemon parses over HTTP, while the daemon's own `GET /status` body is assembled
ad-hoc per request and therefore needed its own type.

### Why `published_at` and `seq` exist

A client can keep "publishing" into a **half-open socket** long after the broker
gave up on it and fired its Last Will. That happened on this broker for ~13.5
hours: every Home Assistant Zigbee entity read `unavailable` while the publisher's
own logs looked perfectly healthy. **Availability did not catch it** — it cannot
express *"connected, but nothing is arriving"*. A `published_at` that stops
advancing and a `seq` that stops incrementing can.

That only works if the publisher is guaranteed to keep publishing when nothing
has changed, which is why the cadence is emit-on-change **plus a ~30 s floor
heartbeat**. Both a `published_at` and a `seq` entity are exposed on purpose: if
one representation ever misbehaves, the other still exposes the wedge.

## Availability is per-entity, in three tiers

Both machines **sleep as their normal resting state**. A single shared
`availability` at the document root gated every entity on the LWT, so a sleeping
box read `unavailable` across the board — hiding a retained, perfectly good
last-known reading that the broker was still holding. Availability is therefore
stamped per-component by `base_discovery()`:

| Tier | Entities | Gated? | While the machine sleeps |
|---|---|---|---|
| commands | every `button` | **yes** | `unavailable` — pressing it cannot work |
| liveness | `status_stale`, `age_seconds` | **yes** | `unavailable` — a frozen value would lie |
| facts | everything else | no | last known value, as of `published_at` |

The liveness tier is the subtle one. `status.stale` and `status.age_seconds` are
computed **by the publisher**, not by a Home Assistant template that re-renders
on a clock tick — so the last message a sleeping machine sent says "not stale, a
few seconds old", and keeps saying it for as long as the machine stays asleep.
`unavailable` is the honest rendering of "I cannot tell you how stale this is".

Two things make the ungated tier safe rather than merely quieter:

- `published_at` carries `device_class: timestamp`, which Home Assistant renders
  as relative time — so a tile reads "2 hours ago" on its face, and freshness is
  visible without inferring it from a missing entity.
- a **`connected`** binary sensor (`device_class: connectivity`) reads the LWT
  topic raw, so the online/offline fact the root availability used to encode is
  still a first-class entity. It is deliberately **not** gated: an
  availability-gated connectivity sensor is `unavailable` at the exact moment it
  has something to report.

Getting a tier wrong is silent in both directions — an ungated liveness sensor
reports stale data as fresh, and a gated connectivity sensor says nothing when it
matters. Each tier is pinned by name in `availability_is_gated_per_tier`.

## The dual-boot rule

The gaming PC is **one physical machine** that dual-boots CachyOS and Windows.
One `device_id`, one MQTT username, one Home Assistant device, plus a
`current_os` sensor that flips.

Both boots publish an **identical discovery component set**. That is structural
rather than a convention: `host_discovery()` takes `device_id` alone — no OS
argument, no `cfg!` — so there is nothing a boot could vary. Entities that only
apply to one boot simply report `unknown`/`unavailable` on the other.

**No software version appears in the retained discovery document.** The two boots
install independently, so a version there would rewrite the retained config on
every OS switch — the exact churn the identical component set exists to prevent.
The running version is a `*_version` diagnostic **entity** read from the state
payload instead, which is the right home for something that changes per boot.

## Commands

Not retained, and the payload is ignored (a Home Assistant button sends an
arbitrary press payload). The two binaries accept **different** names.

| Binary | Accepted on `cmd/<name>` |
|---|---|
| `tv-shell-input` (daemon) | `suspend`, `restart-shell`, **and any valid shell intent** |
| `tv-shell-host` (sidecar) | `sleep`, `quit`, `open-bpm` |

The daemon does not hard-code its intent list: anything
`bridge_core::is_valid_intent` accepts is dispatched, so the MQTT and IPC
surfaces can never disagree about what an intent is. That currently covers
`home`, `home-tap`, `home-hold`, `menu`, `settings`, `power`, plus the
`settings:<slug>` / `app:<wmClass>` / `overlay:{volume,network,session}`
deep-link namespaces (see [`CONTROL_SURFACE.md`](CONTROL_SURFACE.md)).

> [!IMPORTANT]
> **The accepted command surface is much wider than the published buttons, and
> the buttons are not the boundary.**
>
> The discovery document publishes buttons for only five names — `suspend`,
> `home`, `menu`, `settings`, `restart-shell` — because adding a button rewrites
> a retained message. **Every other intent above is still accepted** by anything
> that can publish to `tv-shell/<device_id>/cmd/+`, including `power` and the
> `app:<wmClass>` launcher.
>
> So the security boundary is the **broker ACL**, not the button list: whatever
> can write to that topic can drive the whole intent vocabulary. Scope the
> per-client ACL to exactly the devices it should control, and do not reason
> about exposure from what Home Assistant happens to render.

`suspend` runs the same two-step logind gate as `POST /suspend`, and the
sidecar's `sleep` runs the same running-game/Sunshine refusal gate as
`POST /sleep` — the code is shared, not duplicated, so the two transports cannot
drift on the check that stops a suspend mid-game.

### What is deliberately NOT on MQTT

- **Wake.** Home Assistant's `wake_on_lan` already emits a directed subnet
  broadcast from a `hostNetwork` pod. A command topic cannot be actioned by a
  machine that is off — the entity would be unavailable exactly when it is
  needed.
- **Screenshots.** Retained PNGs would bloat the broker and its persistence
  file. They stay on the HTTP bridge behind a Home Assistant `generic` camera.

**The existing htpc-1 → sidecar HTTP path remains.** MQTT is additive; the QML
shell's Steam widget depends on those routes.

## Configuration

- **Daemon** — the `[mqtt]` table in `config.toml`. Keys, defaults and the
  security rules are documented inline in
  [`config/config.toml.example`](../config/config.toml.example).
- **Sidecar** — environment variables only. The sidecar has no config file, and
  `brand::config_dir()` resolves to a CWD-relative path on Windows (neither
  `XDG_CONFIG_HOME` nor `HOME` is normally set there), so it never calls it. The
  variables are listed in [`HOST_SETUP.md`](HOST_SETUP.md#mqtt-optional).

The sidecar's trust model is asymmetric between its two boots, and worth stating
plainly: the Linux `host.env` is `0600`, while the Windows per-user
`win_environment` variables are ACL-protected and readable by any process running
as that user. That is **the same trust model as the bearer token already deployed
there — no regression** — but it is not parity with Linux.

Neither binary has a reload path. Any change, credential rotation included, needs
a restart — and restarting the daemon hands the CEC adapter to whatever grabs it
next, so rotating the MQTT password is outage-adjacent rather than a config edit.

## Failure behaviour

A misconfigured MQTT setup logs at `error` naming the offending field and is
**skipped**. The daemon starts normally (shell, CEC and the input fleet are
unaffected) and the sidecar starts normally with **every HTTP route still
serving**. So the symptom of an MQTT typo is *"there is no device in Home
Assistant"* — never a dead TV and never a dead Steam row. Check the process's
startup log first.

One exception on the daemon, and it is a different mechanism: `config.toml` is
parsed with `deny_unknown_fields`, so a misspelled **key** under `[mqtt]` fails
the TOML parse and aborts startup. `panel/src/config.rs` parses the same file
leniently and keeps running, which makes that present as "the daemon is broken"
rather than "the config has a typo".

## Operating notes

- **Reconnect**: exponential backoff from 1 s, capped at 60 s. Keepalive
  defaults to 60 s — generous because reconnect churn on a broker that Zigbee and
  Z-Wave also ride on is a live risk, and the Windows sidecar's scheduled task
  has a `PT5M` watchdog plus a session-unlock trigger.
- **Everything is republished on every reconnect** — discovery, the `online`
  availability message and the command subscription — because the broker may have
  expired the session. A state publish is forced immediately after.
- The daemon publishes nothing before its first ConnAck, so a state message can
  never reach the broker ahead of the discovery document that gives it meaning.
- **The daemon's system metrics do not trigger a publish.** CPU%, memory, uptime
  and the clock-derived ages move every tick and would publish once a second onto
  a broker home automation depends on. They ride along on whatever publish happens
  for a real reason; `published_at` still advances every heartbeat.

## Testing

Every *decision* was factored into a pure function, so the 26 unit tests
(12 in `daemon/src/mqtt.rs`, 14 in `host/src/mqtt.rs`) cover config/URL parsing,
redaction, the `should_publish` truth table and `seq` monotonicity with no
broker. What that leaves uncovered is the **I/O glue** — and since three of the
four messages are retained, a regression there orphans a Home Assistant device
rather than raising an error.

`host/tests/mqtt_broker.rs` covers it against a real broker: the three topic
names, their retain flags, the discovery document's shape (per-component
`availability` in the list form, no root `availability` at all, no
`availability_topic` anywhere, no `sw`, `state_topic` absent on buttons, the
`connected` sensor reading the LWT topic ungated), the state envelope, the floor
heartbeat advancing `seq` +
`published_at`, the Last Will firing on an **ungraceful** kill, and discovery
being republished after a reconnect.

The tests are `#[ignore]`-gated behind `TV_SHELL_TEST_BROKER`, so `cargo test`
stays fast and offline by default. They are deliberately **not**
`cfg(target_os)`-gated — they stay compiled and clippy-clean on all three of
`host.yml`'s targets.

```bash
docker compose -f dev/mqtt/compose.yaml up -d --wait
TV_SHELL_TEST_BROKER=mqtt://127.0.0.1:1883 \
  cargo test -p tv-shell-host --test mqtt_broker -- --ignored --test-threads=1 --nocapture
docker compose -f dev/mqtt/compose.yaml down -v

./scripts/mqtt-smoke-test.sh   # owns its own broker lifecycle
```

CI runs both in `.github/workflows/mqtt.yml`, against that same compose file.

Two failure modes this harness is built to *not* reproduce, because both
produced convincing false results when the surface was tested by hand:

- **A binary without the MQTT code publishes nothing**, which reads identically
  to "MQTT is broken". Both the job and the smoke script assert the built binary
  actually carries the MQTT env surface before drawing any conclusion from
  silence.
- **`pkill -f <pattern>` that matches nothing exits 0**, so the Last-Will check
  waited on a will that had no reason to fire. The harness proves the process was
  alive, then proves it is dead, before asserting on the consequence of its death.

Generally: a check that silently does nothing looks exactly like a check that
passed. Each assertion proves the precondition it depends on.

## Home Assistant discovery notes

Researched for the deferred cutover; none of it is testable from Rust.

- Device-based discovery is ONE retained message at
  `homeassistant/device/tv-shell-<device_id>/config` defining every entity under
  `cmps`.
- Inside `cmps` the platform key is **`p`**, and `unique_id` is required per
  component.
- `dev` and `o` are both **mandatory**, and `o.name` is required.
- Remove a component by publishing an empty retained payload — never via the UI.
- The supported *shared root* options are a specific set (`state_topic`,
  `command_topic`, `qos`, `encoding`, `availability`), and **unknown root keys are
  allowed but ignored** — so a misspelled shared option fails silently. That is
  how a root `availability_topic` slipped through initially.

**On availability**: `availability` (the list form) is the spelling Home
Assistant documents, so it is the unambiguously supported one and therefore the
safer encoding — which is why it is used. Whether the `availability_topic`
spelling also happens to work is **not established**: it cannot be settled
without a live broker and a live Home Assistant, neither of which was reachable
when this was written. Do not read this as "the other form is broken". The stakes
are why it is spelled out at all: a silently ignored availability block registers
every entity correctly and then leaves them permanently "available" while the
Last Will fires into the void.

Note that although `availability` *is* a legal shared root option, this document
deliberately does not use it there — it is set per-component instead, so each
entity can opt in or out (see [the tiers above](#availability-is-per-entity-in-three-tiers)).
That also sidesteps having to establish how a per-component `availability`
interacts with a root one: nothing here relies on override-or-merge semantics,
which is the same reasoning that keeps `state_topic` per-component.

Jinja rules, all checked against the live Home Assistant template engine:

| template | renders | why it matters |
|---|---|---|
| `{{ <unix_seconds> \| timestamp_utc }}` | `2026-07-26T23:36:40+00:00` | carries the UTC offset, so it is valid for `device_class: timestamp` |
| `{{ true }}` | `True` | a bare bool matches neither `payload_on` nor `payload_off` — hence the `{% if %}ON{% else %}OFF{% endif %}` form |
| `{{ 0 \| default('unknown', true) }}` | `unknown` | `default(.., true)` treats a real `0` as falsy (0% CPU, CEC address 0 = the TV) — hence the explicit `is not none` form |
| `{{ x if x is not none else none }}` | `None` | Home Assistant's documented sentinel for "unknown" on an MQTT sensor |
