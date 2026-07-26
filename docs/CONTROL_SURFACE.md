# Network Control Surface (HTTP bridge + MCP server)

The daemon exposes its intent/key/screenshot/dev surface over the network two
ways. Both are **opt-in** (a single key each in `config.toml`), share **one bearer
token**, and are thin adapters over the same action logic in
`daemon/src/bridge_core.rs`.

| Adapter | Module | Opt-in (config.toml) | Endpoint |
|---------|--------|----------------------|----------|
| HTTP/1.1 bridge | `daemon/src/http.rs` | `[http] bind = "host:port"` | `http://<bind>/...` |
| MCP server (rmcp 1.7.0, streamable-HTTP) | `daemon/src/mcp.rs` | `[mcp] bind = "host:port"` | `http://<bind>/mcp` |

> The web control panel that consumes this surface is documented in [`PANEL.md`](PANEL.md).

Relationship to the Unix-socket IPC ([IPC_PROTOCOL.md](IPC_PROTOCOL.md)): the IPC
socket (`0o600`, owner-only) is the shell↔daemon contract. This control surface is
its **network-facing sibling** — same `Control::Intent` / `Control::Key` paths and
the `grim` screenshotter, reachable by off-box clients (an LLM agent, Home
Assistant) when explicitly bound. No `bind` set → no socket opened, zero exposure.

## Auth model

Both adapters share the **same** `[http]` keys in `config.toml`:

| Key | Default | Effect |
|-----|---------|--------|
| `[http] auth_enabled` | `true` | `false` disables auth (local-only dev) |
| `[http] token_file` | unset | path to a `0600` file holding the bearer token; every request needs `Authorization: Bearer <token>`. The token is **by reference only**, never inline |

- Constant-time compare (`bridge_core::ct_eq_str`).
- Auth enabled + no token → **fail closed** (all 401). An empty/missing token file is treated as no token.
- The daemon **refuses to start** (`DaemonConfig::validate`) on a **non-loopback**
  bind when dev tools are on AND auth is effectively off — that combo is an
  unauthenticated RCE surface. Set `[dev] allow_insecure_lan = true` to override
  the refusal (downgrades it to a loud warning) on a box that genuinely wants the
  unauthenticated LAN dev loop.

**Posture**: LAN-only, bind to a trusted interface, keep a token set, leave dev
tools off in production. A wildcard bind (`0.0.0.0`) widens reach — the token is
then the only gate.

## HTTP bridge endpoints (`http.rs`)

| Method | Path | Action |
|--------|------|--------|
| POST | `/intent/<name>` | dispatch intent (`<name>` percent-decoded; see vocab below) |
| POST | `/key/<name>` | synthesize nav key: `up\|down\|left\|right\|select\|back` |
| GET | `/screenshot[.png]` `[?flash=1]` | `grim -` PNG; `flash=1` paints a post-capture vignette. Capture provenance rides in `X-TvShell-{Sha,Branch,Version,Captured-At}` response headers (body stays pure PNG) |
| GET | `/status` | JSON `ShellStatus` — the shell's last-pushed state + staleness (see below). Distinct from `/dev/status` |
| POST | `/suspend` | suspend this machine via logind (`power-can-suspend` gate, then `power-suspend`) |
| GET | `/dev/status` | JSON `StatusInfo` blob |
| GET | `/dev/logs` `[?lines=N&filter=str]` | tail `/tmp/qs-log.txt` (lines default 100, max 1000) |
| POST | `/dev/deploy` `[?ref=git-ref]` | git fetch + checkout + reset (ref default `main`) |
| POST | `/dev/build` | run `scripts/build-daemon.sh` + install binary |
| POST | `/dev/restart-shell` | restart quickshell (single-instance; see note), return first WARN/ERROR |
| POST | `/dev/restart-daemon` | re-exec the daemon (picks up a new binary) |
| GET | `/metrics` | Prometheus/OpenMetrics exposition (**auth-exempt**; see Observability) |

Returns `200 ok`, `400` (`error:` reply), `401`, `404`, `405`, `409`
(`/suspend` refused), `500` (grim/dev failure), `503` (daemon unavailable).
Hardening: 4 KiB header cap, 5 s header timeout, 128 concurrent-connection cap
(→ 503), 180 s budget for `/dev/*` subprocesses (auth checked first).

> ⚠️ **`POST /suspend` widens what a leaked bearer token can do.** Until this
> route existed, the worst an attacker with the token could do was drive the UI
> and read the screen. Now the same token powers the machine down — a trivially
> repeatable denial of service against a box whose whole job is to be on when
> someone sits down. Treat the token as a device-control credential, keep
> `auth_enabled = true`, and prefer a loopback/Tailscale bind over `0.0.0.0`.
> Note this is the *same* token as `[mcp]` (`[http].token_file` is shared), so
> exposure of either surface exposes both.

### `GET /status` — shell state for automation

Reports what the shell last pushed over [`shell-state`](IPC_PROTOCOL.md), plus a
staleness verdict:

```json
{
  "shell_state": "streaming",
  "media_playing": true,
  "stale": false,
  "age_seconds": 2,
  "stale_after_seconds": 9,
  "shell_running": true
}
```

**The daemon reports; the caller decides.** There is deliberately no `busy`
boolean here — what counts as "too busy to suspend" is the consumer's policy, so
it can change without a daemon release. `shell_state` is the shell's own enum
string, republished verbatim.

**Always gate on `stale` before acting.** `shell_state` is the *last known*
value, not a live one. A stale `"idle"` means "the shell stopped reporting", not
"the box is idle" — acting on it is how you suspend a machine that is actually
mid-stream behind a wedged shell. `stale` goes `true` once the last push is
`stale_after_seconds` (3× the shell's ~3 s heartbeat) old, and is `true` from
startup until the first push ever lands (`shell_state: null`,
`age_seconds: null`).

`shell_running` is an independent `pgrep -x quickshell` check sharing one
implementation with `/dev/status`, so the two can never disagree. Together the
two fields separate "shell is gone" (`shell_running: false`) from "shell is
alive but silent" (`shell_running: true`, `stale: true`) — different faults with
different fixes.

### `POST /suspend` — put this machine to sleep

No body. Runs the existing `power-can-suspend` gate first and only then
`power-suspend` (logind `Suspend(false)`) — the same D-Bus actor the IPC command
uses, so there is exactly one suspend path in the daemon.

| Result | Status | Body |
|--------|--------|------|
| logind accepted | `200` | `ok` |
| this machine reports it cannot suspend | `409` | `suspend refused: this system reports it cannot suspend` |
| the suspend call failed | `500` | `suspend failed: <reason>` |

A **refusal is not a failure** — `power-can-suspend` deliberately degrades a bus
error to `no`, and a non-Linux build answers `unsupported on this platform`;
both land on `409` so a caller can distinguish "won't" from "broken". `200 ok`
means *accepted and dispatched*, not "already asleep": the response is flushed
before the kernel can freeze the process.

This route does **not** consult `/status`. Whether the box is too busy to
suspend is the caller's decision — read `/status`, apply your own rule, then
call `/suspend`.

> The HTTP `/dev/*` routes are **always registered** when the bridge is bound —
> they are not behind a separate dev flag (unlike MCP). Gate them by not binding
> the HTTP bridge in production, or bind it to loopback.

### `restart-shell` single-instance semantics (#254)

Both `POST /dev/restart-shell` and the MCP `restart_shell` tool are serialized by
a process-wide lock and prefer the systemd unit, so a restart can never stack a
second Quickshell on the same output:

- **Serialized, reject-not-queue.** The handler holds a process-wide async lock
  across the whole kill→spawn→verify sequence. A second call that arrives while a
  restart is in flight does **not** queue (which would trigger a redundant
  kill/spawn immediately after) — it returns `restart already in progress` (HTTP
  `200`) and no-ops. This closes the race where two overlapping HTTP/MCP calls
  each killed and respawned, leaving 2+ instances.
- **Prefers the systemd unit.** When `tv-shell-quickshell.service` is active
  (`systemctl --user is-active`), the restart runs `systemctl --user restart
  tv-shell-quickshell.service` — systemd stops the old instance before starting
  the new one. Otherwise it falls back to the serialized `pkill -x quickshell` +
  detached `setsid quickshell` spawn (a fresh/dev install with no unit, or a
  session with no user manager).
- **Post-restart verification.** After the settle window it counts **live**
  quickshell processes (`ps -eo stat=,comm=`, excluding `Z`/`<defunct>` zombies —
  a plain `pgrep -xc` would miscount the fallback path's transient defunct
  children); if it ever sees more than one live shell it logs an `error!` and bumps
  `tv_shell_quickshell_multi_instance_total` (should always stay 0). See
  [SYSTEMD_SETUP.md](SYSTEMD_SETUP.md) for the unit.

## Observability (`/metrics`)

`GET /metrics` returns the daemon's Prometheus/OpenMetrics exposition text
(`Content-Type: text/plain; version=0.0.4; charset=utf-8`). Unlike every other route it
**bypasses the bearer-token auth** — scrapers don't send tokens, and it exposes
only aggregate counters (`tv_shell_*_total`) and convenience resource gauges
(no screen content, no control). It is always available when the bridge is bound.

This is the *portable* metrics path; the *primary* path is the node_exporter
textfile collector (`[observability].metrics_textfile` in config.toml). Logs go
to the systemd journal (`journalctl --user -u tv-shell-input`). The full emit
contract — config keys, the complete metric catalogue with types, and both
collection options — is in [`OBSERVABILITY.md`](OBSERVABILITY.md).

## MCP tools (`mcp.rs`)

16 tools over streamable-HTTP at `/mcp`. The 3 dev tools are gated by
`[mcp] dev = true` in `config.toml` — when off they return a clear error
instead of acting (registered unconditionally; rmcp can't yet register
conditionally, `mcp.rs:669`).

| Tool | Params | Annotation | Maps to |
|------|--------|------------|---------|
| `shell_action` | `name` (bare verb only) | write | bare intent from closed vocab |
| `intent` | `name` (bare verb only) | write | **alias of `shell_action`** — same handler/schema; named to match the HTTP/IPC `intent` verb |
| `navigate` | `key` (up/down/left/right/select/back) | write | `Control::Key` |
| `key` | `key` (up/down/left/right/select/back) | write | **alias of `navigate`** — same handler/schema; named to match the HTTP/IPC `key` verb |
| `open_settings` | `page` (`SettingsPage` enum) | write | `settings:<page>` |
| `open_overlay` | `target` (volume/network/session) | write | `overlay:<target>` |
| `launch_app` | `wm_class` (StartupWMClass) | write | `app:<wm_class>` |
| `list_apps` | — | read-only | XDG `.desktop` scan → `[{name,wm_class,comment}]` |
| `get_ui_state` | — | read-only | Hyprland active window + quickshell focus |
| `take_screenshot` | `flash` (bool) | read-only | `grim` → PNG content + a trailing JSON text block `{captured_at,sha,branch,version}` |
| `get_status` | — | read-only | typed `StatusInfo` JSON (output schema) |
| `get_logs` | `lines` (≤1000), `filter` | read-only | tail `/tmp/qs-log.txt` |
| `restart_shell` | — | destructive | restart quickshell (single-instance; serialized, prefers systemd unit — see note) |
| `dev_deploy` 🔒 | `git_ref` (default `main`) | destructive | git fetch/checkout/reset |
| `dev_build` 🔒 | — | destructive | build + install binary (~15–60 s) |
| `dev_restart_daemon` 🔒 | — | destructive | re-exec daemon (connection drops) |

🔒 = requires `[mcp] dev = true`.

**MCP resource — `screenshot://current`:** the live display as a PNG, exposed via
`resources/list` + `resources/read` (capabilities advertise `resources`). It is
**additive, not a replacement** for the `take_screenshot` tool: the tool is the
model-driven primitive the autonomous observe→act→verify loop calls; the resource
is the host/user-driven path for attaching the current screen as context from an
MCP client's resource picker. A `resources/read` is side-effect-free (flash is
hard-wired off — only the tool flashes) and lazy (nothing is captured until a
client reads). It returns two content blocks: the PNG `blob` (`image/png`) and the
same `{captured_at,sha,branch,version}` provenance as a JSON text block. Unknown
URIs return a JSON-RPC `resource_not_found` (-32002).

**Tool design:**
- `shell_action` accepts only bare verbs from the closed vocabulary (`home`,
  `home-tap`, `home-hold`, `menu`, `settings`, `power`). Deep-links are rejected
  at the MCP layer — use `open_settings` / `open_overlay` / `launch_app` instead.
- `open_settings.page` is a typed `SettingsPage` enum (not a free string).
- `get_ui_state` reports compositor-level window focus (class + title + whether
  quickshell is focused) — NOT QML-internal state. Use `take_screenshot` for
  in-shell view state.
- `take_screenshot` returns capture provenance alongside the frame (HTTP: `X-TvShell-*`
  headers; MCP: a trailing JSON text block) so a caller can tell *which* deployed
  checkout produced the image — latest `main`, a feature branch, or another agent's
  work. It's read live per capture (via `bridge_core::capture_meta`), because a
  `dev_deploy` mutates HEAD under the long-lived daemon without a restart.
- `list_apps` makes `launch_app` discoverable without guessing `wm_class` values.
- `intent` / `key` are **additive aliases** of `shell_action` / `navigate` (same
  handlers, schemas, and annotations) so the MCP verb names match the `intent` /
  `key` verbs the HTTP bridge and Unix-socket IPC already use. `shell_action` /
  `navigate` are kept unchanged — they are federated to downstream MCP consumers
  (as `game-shell-*`-prefixed tools), so renaming them would break saved
  allowlists; the aliases are added alongside, never in place of them.

`[mcp] allowed_hosts = ["host[:port]", …]` sets the rmcp Host
allowlist (loopback always allowed; a concrete bind IP is auto-added; a wildcard
bind with no override disables Host matching and relies on the token, `mcp.rs:887`).

## Intent vocabulary

Bare: `home`, `home-tap`, `home-hold`, `menu`, `settings`, `power`. Deep-links:
`settings:<page-slug>`, `overlay:<volume|network|session>`, `app:<StartupWMClass>`.
Page slugs: `audio`, `bluetooth`, `network`, `display`, `controllers`,
`keybindings`, `avcontrol`, `widgets`, `accessibility`, `power`, `system`. Validated by
`bridge_core::is_valid_intent` (full vocab in `protocol.rs`; unknown `settings:`
slugs are a graceful QML no-op).

> **Reroute:** `settings:moonlight` and `settings:streaming` are **not** sidebar
> pages — Moonlight server management is demoted under Widgets. Both slugs open
> the **Settings ▸ Widgets ▸ Moonlight** config page directly (server management
> is inlined on it), via the QML `SettingsApp.openSectionById` mapping them to the
> `widgets` section with a pending deep-target. Agents driving the UI should
> expect the Widgets page (not a "Moonlight" sidebar entry) with the
> server-management surface in view.

## `StatusInfo` fields

`sha`, `daemon_pid`, `version`, `shell_running` (`pgrep -x quickshell`),
`wayland_display` (nullable), `hypr_sig_present` (`bridge_core.rs:256`).
