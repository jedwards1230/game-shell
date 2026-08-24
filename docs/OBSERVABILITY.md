# Observability

The `tv-shell-input` daemon emits observability in Linux-native, standard,
self-describing formats so **any** consumer can collect it their way. This repo
emits the signal; **collection and forwarding are intentionally out of scope**
(they are deployment-private — point your own node_exporter / Prometheus /
journald pipeline at the contract below).

There are two signals:

1. **Logs** → the systemd **journal** (structured, syslog-priority-mapped), with
   a plain-stdout fallback.
2. **Metrics** → a Prometheus/OpenMetrics **textfile** for the node_exporter
   textfile collector (primary), plus a portable HTTP **`/metrics`** scrape
   endpoint (alternative).

---

## Logs

`init_tracing()` chooses a logging backend at startup:

- **systemd journal** (via `tracing-journald`) when a journal is available —
  structured fields + syslog priority mapping (so `journalctl -p` works).
- **stdout** (the original compact `fmt` layer) otherwise, and always on
  non-Linux.

Selection is automatic but overridable via `[observability]` in
`~/.config/tv-shell/config.toml`:

| Config key (`[observability]`) | Values | Effect |
|---|---|---|
| `log_journal` | `true` | Force the journald layer on. |
| | `false` | Force stdout (no journald). |
| | _omitted_ (default) | **Auto**: use journald when `JOURNAL_STREAM` is set (i.e. launched under a systemd unit) and the journal socket is reachable; otherwise stdout. |

The log level/targets remain an env var (NOT a config key), so the standard
`RUST_LOG=… tv-shell-input` workflow is unchanged:

| Env var | Values | Effect |
|---|---|---|
| `RUST_LOG` | e.g. `info`, `tv_shell_input=debug` | Standard `EnvFilter` syntax. Honoured identically on **both** paths. Default `info`. |

If the journald layer is requested but the journal socket cannot be opened, the
daemon logs a one-line notice to stderr and falls back to stdout — it is never
left without logging.

### Reading logs

When run under the user service (the common deployment):

```bash
journalctl --user -u tv-shell-input            # all logs
journalctl --user -u tv-shell-input -f         # follow
journalctl --user -u tv-shell-input -p warning # warnings and above
journalctl --user -u tv-shell-input -o json    # structured fields
```

Raise verbosity by setting the `RUST_LOG` env var (it stays an env var, not a
config key), e.g. `RUST_LOG=tv_shell_input=debug tv-shell-input`. The
`publish` chokepoint at `debug` is a full event tracer (intents, combos,
`pad:*`, input-mode, controller-wake).

**CEC display-ownership history** rides these logs. Every observed change of who
owns the display is logged at `info` with `previous`/`current`/`source` fields —
the cache published on `GET /status` keeps only the *current* owner and its
change time, so the journal is the replayable history:

```bash
journalctl --user -u tv-shell-input --grep 'display ownership'
```

A run with no such lines and `cec_display_owner_tracking: true` on
`GET /status` means the bus genuinely never broadcast `<Active Source>` — the
one reading that distinguishes "correctly reports unknown" from "the receive
callback never fires". See
[CONTROL_SURFACE.md](CONTROL_SURFACE.md#cec-display-ownership-cec_).

---

## The Quickshell log (`/tmp/qs-log.txt`)

The QML shell is a separate process from the daemon, and its log is a separate
signal. `tv-shell-quickshell.service` tees Quickshell's merged stdout/stderr to
**both** journald (tagged `tv-shell-quickshell`) and `/tmp/qs-log.txt`, which
`/dev/logs` tails and `/dev/restart-shell` reads its warning tail from. The file
is truncated on every shell start.

```bash
journalctl --user -t tv-shell-quickshell -f    # follow
tail -f /tmp/qs-log.txt                        # same stream, restart-truncated
```

### Signal-to-noise is a maintained invariant, not an accident

**A healthy shell start emits a handful of WARN lines, not hundreds.** If you
see a warning class repeating in the thousands, that is a bug in this repo —
treat it as one. Do not filter it at the reader.

That rule is load-bearing because we already broke it once. A measured baseline
before the fix: **1,108 WARN lines in the first 69 seconds** of a shell start
(and 10,498 over a 23-hour session). Zero ERROR lines. Every class was benign in
isolation, which is exactly why it went unnoticed — and it made `/dev/logs` and
the `/dev/restart-shell` tail useless for real debugging.

The four root causes, all fixed, are worth knowing because each is a trap that
is easy to reintroduce:

**1. Degenerate screen geometry collapsed every derived size (~795 lines).**
`Units.qml` derives `gridUnit` — and therefore every spacing, radius and icon
size in the shell — from `Quickshell.screens[0].height`. That list goes
transiently **empty** during startup, DPMS, mode-set and CEC/TV power events,
and a `ShellScreen` can briefly report a height of ~0. The old code fell back to
a literal `2160` on an empty list and did no flooring, so those two states
produced `iconSizeXL` values of `240` (a size this 1080p-logical device never
renders) and `0` (which the icon provider clamped to 1px × DPR 2 → the
`QSize(2, 2)` requests). Every settle pass re-requested every icon at a bogus
size, and the same degenerate size fed to `QSvgIconEngine` produced the
`qt.svg.draw: The requested buffer size is too big` lines.

> **Invariant:** `Units` holds the last *valid* screen height and exposes a
> `screenReady` flag. Nothing derived from screen geometry may be requested
> before a real height has been adopted. If you add a size that feeds an
> `Image`'s `sourceSize`, it must not be computable from an empty screen list.

**2. Icons were resolved against the wrong theme (~960 lines).**
Qt selects a platform theme from `QT_QPA_PLATFORMTHEME`; on this host it was
unset and `XDG_CURRENT_DESKTOP` is `Hyprland`, so `QIcon` fell back to bare
`hicolor` — which contains **none** of the standard freedesktop names that
`.desktop` entries use. The icons were never missing from the system: `breeze`,
`breeze-dark` and the `Adwaita` chain are all installed and carry nearly all of
them.

`tv-shell-quickshell.service` now sets `Environment=QT_QPA_PLATFORMTHEME=kde`
(plasma-integration's platform theme, already installed), which resolves to
`breeze-dark` with a `breeze` fallback and adds the XDG icon search paths. It
does **not** require `XDG_CURRENT_DESKTOP=KDE` — leave that as `Hyprland`, since
changing it also steers xdg-desktop-portal backend selection.

Note the amplification: only **12 distinct icon names** produced all ~960 lines.
`AppIcon.qml` sets `cache: false` (load-bearing for the stale-neighbour-texture
bug on delegate recycling — do not change it), so every re-source re-resolves
and re-warns. `AppIcon` now keeps a session-scoped negative memo
(`lib/iconMemo.js`): a name that has failed once at a valid size is never
requested again, and the existing letter-initial fallback renders instead.

> **Known gap:** `image://icon/` can return a *Ready* magenta placeholder rather
> than an `Image.Error` for some missing names, so the memo only catches names
> that genuinely error. `Quickshell.iconPath(name, true)` is *not* a fix — that
> overload does not return `""` for theme-missing names on this Quickshell
> build (tried and reverted).

**3. A poll reset the whole Steam model every 10 s (24 lines, plus real churn).**
`SteamLibraryView` reassigned `recentItems`/`allItems` on every successful
`ServiceMonitor` reply. Those are fresh lists each time, so the property
identity changed even when the payload was byte-identical, the `ListView` model
reset, and **every delegate was destroyed and rebuilt** — re-walking each
`SteamCard` art candidate chain from index 0 and re-emitting its 404s. The
assignments are now gated on a content signature, the same way
`AppLifecycleManager` gates `runningWindows`.

> **Invariant:** if you assign a list property from a poll reply, gate it on a
> content signature covering exactly the fields the delegate binds. A field the
> delegate binds but the signature omits means a real update is silently
> dropped — a far worse bug than the noise.

**4. A binding loop in `AppsWidget` (8 lines).** `_activeModel` had two cycles:
a writable `_segment` imperatively coerced from `onModelChanged`, and a
cross-component path where `HomeScreen`'s hint bar read back *upstream* to
`_recentModel`. Both are broken by deriving rather than writing. Qt abandons a
re-entered binding update, so the visible symptom was a **stale** `_activeModel`
— the warning was the only evidence.

> **Invariant:** never write a dependency of a derived property from a handler
> on another of its dependencies, and never let a downstream consumer read back
> upstream. `tests/qml/tst_appssegment.qml` pins this with
> `failOnWarning: /Binding loop/`.

### Why there is no reader-side filter

`/dev/restart-shell`'s warning tail used to drop every `COULD NOT LOAD ICON`
line unconditionally, because ~960 of them buried everything else. That filter
is **gone on purpose**: now that the source is fixed, filtering there would only
hide a regression. The panel's *opt-in* "Hide icon noise" toggle
(`panel/src/pages/logs.rs`) is a different thing — a user-chosen view, not a
forced one — and can be retired once the fix has held for a while.

If you find yourself wanting to add a filter to read the log, fix the emitter
instead.

---

## Metrics

All metrics are namespaced `tv_shell_` and carry `# HELP`/`# TYPE` lines. The
exposition text is rendered **once** by `metrics::render` and shared between the
textfile writer and `/metrics`, so the two never drift.

> **Resource gauges are a convenience.** `tv_shell_cpu_percent`,
> `tv_shell_mem_*`, `tv_shell_load1`, and `tv_shell_temperature_celsius`
> are reused from the daemon's existing sys-metrics reader. If a **node_exporter
> is present on the host, prefer its** `node_cpu_*` / `node_memory_*` /
> `node_hwmon_temp_*` — they are more complete and authoritative. The genuinely
> valuable, daemon-specific signal is the **counters** below, which node_exporter
> cannot provide.

### Metric catalogue

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `tv_shell_build_info` | gauge | `sha`, `branch`, `version` | Currently deployed revision. Standard info-metric: value is always `1`, identity is in the labels. Resolved **live on each render** from the same provenance as the `/screenshot` `X-TvShell-*` headers and `/dev/status`, so a `/dev/deploy` HEAD swap shows up on the next render. |
| `tv_shell_input_events_total` | counter | — | Raw gamepad evdev events read and processed by the input runtime (hot path). |
| `tv_shell_intents_emitted_total` | counter | — | Shell intents broadcast (`intent:<name>`) — IPC, HTTP `/intent/*`, MCP `send_intent`, and gamepad Home-tap/Home-hold all funnel through one chokepoint. |
| `tv_shell_transitions_total` | counter | — | Shell↔game presenter transitions (`grab`/`release`/`handoff`). |
| `tv_shell_pad_joins_total` | counter | — | Gamepads that joined the fleet (hot-join or initial enumeration). |
| `tv_shell_pad_leaves_total` | counter | — | Gamepads that left the fleet (disconnect). |
| `tv_shell_shell_restarts_total` | counter | — | Daemon starts observed this boot session (the daemon re-execs on `/dev/restart-daemon` and is otherwise supervised, so this is the input-daemon restart count). |
| `tv_shell_input_runtime_up` | gauge | — | Input-runtime liveness: `1` while the supervised input event loop is running, `0` during a respawn gap or after it has panicked past its retry budget (the daemon stays alive; IPC input commands then reply `error:input-runtime-down`). Always emitted. |
| `tv_shell_input_runtime_restarts_total` | counter | — | **In-process** input-runtime respawns after a caught panic — the supervisor rebuilds the input event loop (fresh fleet → released grabs) without re-execing the daemon. Distinct from `tv_shell_shell_restarts_total` (whole-process starts); a rising value flags a recurring panic in the input path. |
| `tv_shell_grab_invariant_violations_total` | counter | — | Detected grab-state drift: a pad's physical `EVIOCGRAB` disagreed with the presenter policy (`should_grab`) after a transition. Should stay `0`; nonzero means the daemon's grab bookkeeping and the kernel diverged. |
| `tv_shell_deploy_total` | counter | `outcome` (`ok`\|`error`) | `POST /dev/deploy` attempts via the HTTP bridge, split by success/failure. |
| `tv_shell_build_total` | counter | — | `POST /dev/build` attempts via the HTTP bridge. |
| `tv_shell_restart_shell_total` | counter | — | `POST /dev/restart-shell` attempts via the HTTP bridge. |
| `tv_shell_restart_daemon_total` | counter | — | `POST /dev/restart-daemon` (re-exec) requests via the HTTP bridge. Counted before the process image is replaced; the re-exec'd process starts its own counters at zero. |
| `tv_shell_cpu_percent` | gauge | — | Aggregate CPU utilisation 0..=100. _Convenience — prefer node_exporter._ |
| `tv_shell_mem_used_bytes` | gauge | — | Used memory in bytes. _Convenience._ |
| `tv_shell_mem_total_bytes` | gauge | — | Total memory in bytes. _Convenience._ |
| `tv_shell_load1` | gauge | — | 1-minute load average. _Convenience._ |
| `tv_shell_temperature_celsius` | gauge | `sensor` | Per-sensor hardware temperature (e.g. `sensor="CPU Tctl"`). _Convenience._ |

### Option A — node_exporter textfile collector (primary)

A background task periodically renders the exposition text and writes it
**atomically** (temp file + `rename(2)`, as the textfile collector requires) to a
`.prom` file.

| Config key (`[observability]`) | Default | Effect |
|---|---|---|
| `metrics_textfile` | _omitted_ → **writer disabled** | Absolute path to the `.prom` file to write (e.g. `/var/lib/node_exporter/textfile/tv-shell.prom`). |
| `metrics_interval` | `15` | Render/write interval in seconds. `0` falls back to the default. |

When `metrics_textfile` is omitted, **no file is written** — the textfile path is
opt-in. The `/metrics` HTTP route is unaffected by this setting.

Point node_exporter's textfile collector at the file's **directory** (see
[`examples/README.md`](../examples/README.md)).

### Option B — scrape `/metrics` (portable alternative)

When the HTTP bridge is bound (`[http].bind` in config.toml), it serves:

```
GET /metrics  →  200, Content-Type: text/plain; version=0.0.4; charset=utf-8
```

This route **bypasses the bearer-token auth** (scrapers don't send tokens) and
exposes only aggregate counters + resource gauges (no screen content, no
control). It is always available and cheap. See
[`examples/prometheus-scrape.yaml`](../examples/prometheus-scrape.yaml).

```bash
curl -s http://<host>:<port>/metrics
```

---

## Configuration summary

Everything below is `[observability]` in `~/.config/tv-shell/config.toml`,
except `RUST_LOG` which stays a standard env var:

| Setting | Default | Purpose |
|---|---|---|
| `[observability].log_journal` | auto | `true`/`false` to force journald on/off; omitted = auto-detect. |
| `RUST_LOG` (env) | `info` | `EnvFilter` log level/targets (both logging paths). |
| `[observability].metrics_textfile` | omitted (disabled) | Path to the `.prom` textfile-collector output. |
| `[observability].metrics_interval` | `15` | Textfile render/write interval (seconds). |

See [`config/config.toml.example`](../config/config.toml.example) for
copy-runnable defaults and [`examples/`](../examples/) for a starter Grafana
dashboard and a Prometheus scrape snippet.
