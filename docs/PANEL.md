# tv-shell-panel — Web Control Panel

`tv-shell-panel` is a LAN-only web control panel for tv-shell: server-rendered
HTML + HTMX over the daemon's existing control surface. It runs as its own
`systemd --user` unit beside the daemon, so it stays available to rebuild or
restart a wedged daemon — the recovery path that previously required remote
config management.

> Status: all four milestones (M1-M4) plus a final-polish pass are **merged to
> `main`** — every page is fully implemented, and the panel is deployed on
> htpc-1. This document is the panel's living doc.
>
> **Single-node today.** The panel dials the daemon's Unix-socket IPC
> unconditionally, so it serves exactly one node and does not build on Windows.
> Extending it to every node with an agent — capability-gated pages, a transport
> trait, and the security work that must land *first* — is designed in
> [MULTI_NODE_PANEL.md](MULTI_NODE_PANEL.md).

## Architecture

- **Crate**: `panel/` (workspace member) → binary `tv-shell-panel`.
- **Stack**: axum + askama templates + vendored `htmx.min.js` (no CDN; the panel
  must render when the network or the rest of the system is broken).
- **Bind**: `[panel]` section in `config.toml` (`enabled`, `bind`, default
  `127.0.0.1:8091`; `token_file` enables auth; `allow_dangerous`, default
  `false`). See [Authentication](#authentication) below — a non-loopback bind
  now **requires** a token, or the panel refuses to start.
- **Unit**: `config/tv-shell-panel.service`, installed by `scripts/install.sh`,
  started by the session script.

### Three data tiers

1. **Unix-socket IPC** (primary) — the daemon's newline-text protocol
   (`docs/IPC_PROTOCOL.md`): status, system info, storage, settings
   (`get-config`/`set-config` — the daemon remains sole writer of
   `settings.json`), widgets subtree, bluetooth, network, power, CEC,
   controllers/bindings, apps, intents/keys.
2. **Daemon HTTP bridge** (dev ops) — `/dev/deploy|build|restart-shell|
   restart-daemon|logs`, `/screenshot` (`docs/CONTROL_SURFACE.md`).
3. **Direct exec** (recovery + system) — `systemctl --user` restarts,
   `build-daemon.sh` when the daemon is down, `journalctl`, process list,
   reboot/suspend via logind. The UI labels which tier each action uses;
   destructive actions are confirmed and single-flight.

## Pages

| Page | Contents |
|---|---|
| Dashboard | unit status, build info, system/storage tiles, pad fleet, quick actions, an Updates tile (own slow poll — see [System updates](#system-updates-pacman) below) |
| Processes | tv-shell systemd user units (daemon/shell/panel) with per-unit restart (color-coded dot + status word, not just text); Hyprland active window/clients (styled table)/monitors via IPC; a read-only top-processes table (`ps`, CPU-sorted, no kill action in v1); a System Updates section (see below) |
| Settings | grouped typed forms over `settings.json` via `get-config`/`set-config` (shallow merge — unmentioned keys, notably the daemon-owned `keyBindings`/`perGameBindings`/`perPlayerBindings`/`webApps`, are left untouched); covers the QML-owned scalars plus `wallpaperPath` and a line-per-entry `prewarmApps` list editor; the daemon-owned keys (binding layers + the `webApps` registry, `docs/WEB_APPS.md`) are shown read-only here (`keyBindings` is also editable via the Controllers page's bindings editor; the per-game/per-player layers are read-only there too — full editors are deferred; web-app **management** now lives on the Media page); read-only `config.toml` view (a general edit path is deferred — editing still requires a manual edit + daemon/panel restart via the Dev page; the one targeted exception is the CEC page's `[cec].osd_name` input-name editor); raw JSON escape hatch with an explicit shallow-merge/`null`-deletes warning for keys not modeled as typed fields (e.g. `widgets`, `cecDeviceNames`) |
| Widgets | per-widget enabled/order/size/prefs editors (`widgets.<id>` subtree) |
| Media | **Wallpapers**: upload images into `~/.config/tv-shell/wallpapers/` (the dir the shell's Settings ▸ Wallpaper page reads), preview them as a grid, pick the active one (persisted as `wallpaperPath` via `set-config`) or clear it, and delete — this is the only way to get an image onto the box without SSH. Upload is treated as an attack surface in its own right (the route is authenticated, but auth is opt-in and a loopback panel may run without it): extension allowlist, filename sanitization, a containment re-check against the wallpapers dir, a 32 MB cap, and magic-byte sniffing, with the read-back route sharing the same resolver so it can't become an arbitrary file read. **Web apps**: list/add/remove the daemon-owned registry (`webapp-list`/`-add`/`-remove`, #187 P1+P3) — the panel is the add surface because the couch UI has no on-screen keyboard (#20) |
| Tools | IPC console grouped by domain — Navigation (intent/key), Apps (list/launch/recents), Bluetooth (power/scan/list/connect-disconnect-pair-trust), Network (status/wifi/throughput/ping), Power (can-suspend/battery), System (sys-status/sys-metrics/storage-status/build-info/controllerdb); plus a raw-line escape hatch with a warning on commands owned by another page's guarded flow. CEC and controller/pads/bindings commands live on their own Controllers/CEC pages (below) instead. |
| Controllers | Fleet table (`get-pads`, per-pad battery/rumble-status/bounded rumble test) with a lazy `list-input-devices` diagnostics panel; grab-management (`grab`/`release`/`handoff`) with explanations and confirms on the two that affect the live input path; a bindings editor (`get-bindings`/`set-binding` against the fixed action/button vocabulary, plus a `capture-next`/`capture-cancel` capture-and-apply flow); read-only per-game/per-player binding layers with a `set-active-game`/clear form (editing deferred — use the Settings raw JSON hatch); controller-DB status/refresh |
| CEC | Topology (`cec-scan`/`cec-device`, merged with the `cecDeviceNames` friendly-name overrides from Settings); switching (`cec-active-source` as the "switch input" primitive, per-device `cec-power-on`/`-off`, all confirmed); a health panel (`cec-health`/`cec-test`) classifying the daemon's transmit-wedge state, with an escalating "Recover CEC" ladder (test → restart daemon, reusing the Dev page's bridge-then-exec tier logic → link to a full reboot on Dev) that flags the recommended step for the current state; an Input-name editor for the OSD device name the daemon announces on the bus (`[cec].osd_name`, default = hostname) — **the panel's one config.toml write**, done format-preservingly via `toml_edit`, applied by a daemon restart; a build/platform-gated daemon renders as an honest "not available" note, never a failure banner |
| Dev | deploy/build/restart daemon/restart shell/reboot with tier labels + confirms; screenshot viewer (provenance sha/branch/version/captured-at, proxied via `/dev/screenshot`) |
| Logs | shell + daemon log tails (ANSI-stripped — including "bare" ESC-dropped residue like `[33m`/`[0m` — and wrapped rather than clipped), free-text filter plus one-click "Errors only"/"Hide icon noise" presets, and a Focus Shell/Focus Daemon toggle to expand one pane to full width (state lives on `#log-panels` itself, so it survives every htmx refresh of the panes inside it) |

A small daemon-reachability dot lives in the topnav on every page (`base.html`
+ `pages::nav`), polling a cheap, short-timeout `status` probe every ~10s —
green when the daemon answers, red when it doesn't, neutral until the first
poll lands.

## System updates (pacman)

`panel/src/updates.rs` owns pacman system-update state independently of the
daemon — the Dashboard Updates tile and the Processes page's "System
Updates" section both read it.

- **Read** (unprivileged): `checkupdates` (pacman-contrib) parsed into
  `{name, old_version, new_version}` rows. Exit code `2` ("no updates
  available") is an OK-empty result, not an error; exit `1` (or a spawn
  failure/timeout) surfaces as an honest error banner. Cached in `AppState`
  (`UpdatesState`) with a 5-minute TTL — `checkupdates` never runs on the
  Dashboard's fast 5s tile poll (the Updates tile polls on its own, much
  slower 300s interval instead); the Processes page's Refresh button
  bypasses the cache.
- **Reboot-needed detection**: compares `uname -r` against the installed
  kernel package's version. The kernel package is found by filtering
  `pacman -Qq` for `linux`/`linux-<flavor>` names (excluding
  headers/docs/firmware/tools suffixes) and, when several are installed
  (e.g. `linux` + `linux-lts`), matching the flavor suffix against the
  `uname -r` release string. An ambiguous or unparseable result degrades to
  `RebootStatus::Unknown` rather than guessing.
- **Apply** (privileged): `sudo -n pacman -Syu --noconfirm`. Runs as a
  single-flighted `tokio::spawn` background task tracked in `AppState`
  (`Idle` → `Running{started, log_tail}` → `Done{success, finished,
  log_tail}`) — the pacman process outlives any one HTTP request. Combined
  stdout+stderr streams into a live ~200-line tail as the process runs;
  `kill_on_drop` enforces a 30-minute timeout. A second "Run full update"
  click while one is already running is a no-op (the existing job's status
  is shown, not a new one started).
- The Processes page's job-status view polls itself
  (`hx-trigger="every 2s [this.dataset.running=='1']"`) only while
  `Running`; on `Done` it shows success/failure and, if the kernel package
  version no longer matches the running kernel, a reboot-needed banner
  linking to Dev → Reboot.
- No new `config.toml` keys — every threshold (cache TTL, apply timeout, log
  tail length) is a hardcoded constant in `updates.rs`.

### Deployment prerequisite: passwordless sudo for the apply path

**The panel's systemd-unit user needs a NOPASSWD sudoers rule scoped to
`pacman -Syu`** — `-n` ("never prompt") is what makes `sudo -n pacman -Syu
--noconfirm` safe to shell out to from an unattended background task in the
first place; without a real terminal to prompt at, a plain `sudo pacman -Syu`
would otherwise just hang until the 30-minute timeout killed it. htpc-1 (the
reference deploy host) grants this today; a fresh deploy host needs the
equivalent, e.g. a drop-in under `/etc/sudoers.d/`:

```
tv-shell ALL=(root) NOPASSWD: /usr/bin/pacman -Syu --noconfirm
```

(substitute the actual unit user and `pacman` path for the target host).

**Failure mode when the rule is absent or wrong**: `sudo -n` fails
immediately — no hang, no password prompt — printing something like `sudo: a
password is required` to stderr and exiting non-zero. The apply job captures
that exact line into its log tail, and the UI surfaces it directly: the
Processes page's failure banner shows the last non-empty log line inline
(`last_error_line` in `pages::processes`) rather than a bare "Update failed",
and the log-tail `<details>` auto-expands on a failed run instead of staying
collapsed — so the real cause is visible without an extra click. The
Dashboard Updates tile is unaffected either way, since it only reflects the
unprivileged `checkupdates` read.

## Authentication

Set `[panel].token_file` to a 0600 file under `~/.config/tv-shell/` and every
route is gated:

```bash
install -m600 /dev/null ~/.config/tv-shell/panel-token
openssl rand -hex 32 > ~/.config/tv-shell/panel-token
```

Use a **different** token from `[http].token_file` — that one is the daemon's,
and the panel already holds it to call the bridge.

**Two credentials, one secret.**

- **Browser** — `GET /login` renders a one-field form; a correct token is
  exchanged for a session cookie (`HttpOnly`, `SameSite=Strict`, `Path=/`, no
  `Max-Age` — it dies with the browser session).
- **Script** — `curl -H "Authorization: Bearer $(cat ~/.config/tv-shell/panel-token)" …`

Both are compared **constant-time** (`subtle::ConstantTimeEq`, the same
primitive as the daemon's `bridge_core::ct_eq_str`).

Two deliberate simplifications, stated so they are not mistaken for oversights:

- **The cookie value IS the token.** No session store, no session id — the panel
  has exactly one credential, so a session table would add state without adding
  a security property. It follows that revoking access means rotating the token
  file and restarting the unit.
- **`Secure` is deliberately omitted** from the cookie. The panel is served over
  plain HTTP on the LAN; `Secure` would make login impossible.

**Exempt routes — exactly four**, everything else (including
`GET /nav/daemon-status`) is gated:

| Route | Why |
|---|---|
| `GET /assets/htmx.min.js` | the login page can't be styled/scripted otherwise |
| `GET /assets/style.css` | same |
| `GET /login` | the form itself |
| `POST /login` | the submission |

**Response shape when unauthenticated** — an `HX-Request: true` swap gets a
plain-text `401` (never an HTML login page: htmx would splice it into whatever
target the caller declared, e.g. the nav status dot); a browser navigation
(`Accept: text/html`) gets a `303` to `/login`; anything else gets `401`.

**Token file hygiene** (mirrors the daemon): the path is tilde-expanded,
canonicalized, and must resolve **under the config dir**; the file must not be
group/other-accessible. A violation **aborts startup** rather than silently
degrading to "no auth". The middleware also fails closed at runtime — auth on
with no token rejects everything.

### Startup refusal

The panel refuses to start when it is bound to a **non-loopback** address with
auth effectively disabled (no `token_file`, or no token resolvable from it) —
the same shape as the daemon's refusal, and reusing the **same**
`[dev].allow_insecure_lan` flag so a node has one insecure-LAN opt-in to audit,
not two. With that flag set the refusal downgrades to a loud `error!`-level log
and the panel serves anyway. The check runs before the listener binds, so a
refused configuration never opens the port.

Leaving `token_file` unset is still supported and keeps the loopback dev
experience unchanged — it is only the non-loopback combination that is refused.

### Dangerous actions (`allow_dangerous`)

The panel is the most privileged surface on its node: it can overwrite the
running build, reboot, suspend, write `config.toml`, upload files and run
`sudo -n pacman -Syu`. `[panel].allow_dangerous` defaults to **`false`**, and
when false these routes are **not registered at all** (404, not a 403 from a
handler) and their buttons are not rendered:

`POST /dev/deploy` · `/dev/build` · `/dev/restart-daemon` · `/dev/restart-shell` ·
`/dev/reboot` · `/dev/suspend` · `/processes/updates/apply` · `/tools/raw`

`GET /dev` and `GET /dev/screenshot` stay available (observability), as do
`POST /processes/restart/{key}` and `POST /cec/recover/restart-daemon` —
restarting a wedged unit is *recovery*, which is the reason the panel exists.
`POST /tools/raw` is in the dangerous set because it drives the entire IPC
vocabulary, making it an arbitrary-command escape hatch.

## Danger tiers

Mutating buttons across the panel use one of two tiers, distinct from
`--error` (reserved for banners): `.warn-action` (amber-red) for
recoverable-but-disruptive actions — unit restarts, Controllers'
release/handoff, controllerdb refresh — and `.danger-severe` (deep red,
bold border) for actions that take the whole box down or overwrite the
running build — Dev's Reboot/Suspend/Deploy/Build, and the Updates
section's "Run full update". The Processes page's own-unit (panel) Restart
button carries a distinct confirm message noting the click will disconnect
the very page the operator is looking at.

## Running locally

Build with `scripts/build-panel.sh` (outputs to `target/release/tv-shell-panel`)
or `cargo run -p tv-shell-panel` for a dev loop. It reads `[panel]` from
`~/.config/tv-shell/config.toml` and serves on `127.0.0.1:8091` by default (see
`config/config.toml.example`). Installed systems run it as
`tv-shell-panel.service`, started by `scripts/tv-shell-session.sh`.

## Milestones

- [x] M1 — crate scaffold, IPC client, app shell/nav, Dashboard, Logs, Dev page
- [x] M2 — Settings + Widgets editors
  - [x] Settings editor
  - [x] Widgets editor
- [x] M3 — Tools console, Processes, screenshot viewer
  - [x] Tools console (Navigation/Apps/Bluetooth/Network/Power/System + raw escape hatch)
  - [x] Processes page (systemd units, Hyprland, top processes)
  - [x] Dev screenshot viewer (provenance headers, PNG proxy)
- [x] M4 — Controllers + CEC (switching, grab handling, wedge recovery)
  - [x] Controllers page (fleet/battery/rumble, grab management, bindings editor + capture, per-game/per-player read-only, controller DB)
  - [x] CEC page (topology, switching, health + escalating wedge recovery)
- [x] UI-polish pass (post-M4, live-audit fixups; branch `panel-staging-ui-polish`)
  - [x] Raw `<connection>:<grab>` IPC tokens humanized into plain language + a
        colored state dot (Dashboard tile, Controllers fleet), raw token kept
        as a muted suffix (`panel/src/humanize.rs`)
  - [x] Log panes ANSI-stripped server-side (`panel/src/text.rs`), wrapped
        instead of clipped, plus "Errors only"/"Hide icon noise" preset filters
  - [x] CEC recovery ladder recommends exactly one step, chosen from the
        health classification, instead of flagging every step
  - [x] CEC health panel and Controllers fleet section auto-refresh via htmx
        out-of-band swaps after any CEC action / grab / release / handoff
  - [x] Settings raw-JSON escape hatch pretty-prints on render (15-row
        textarea) and is compacted server-side before `set-config`
  - [x] Settings' persisted binding-override block relabeled to point at
        Controllers for the resolved view
  - [x] Dashboard tiles are whole-tile links to their natural page
  - [x] Global daemon-reachability dot in the topnav (`pages::nav`)
  - [x] `[profile.release]` (`strip = "debuginfo"`, `lto = "thin"`) added to
        the workspace root — trims every workspace binary, daemon/host included
- [x] Final-polish pass (post-UI-polish; branch `panel-staging-final-polish`)
  - [x] System updates (pacman): Dashboard tile + Processes page section,
        async background apply job, reboot-needed detection, and a
        NOPASSWD-sudo deployment prerequisite with an honest (not generic)
        failure banner when it's missing (see
        [System updates](#system-updates-pacman) above)
  - [x] Log pane focus/expand toggle on `/logs`
  - [x] `strip_ansi` also strips bare (ESC-dropped) CSI residue
  - [x] Loading feedback (`.htmx-request` opacity/spinner) and
        `hx-disabled-elt` double-fire protection on every mutating
        form/button
  - [x] Two-tier danger button styling + a distinct panel-restart confirm
        (see [Danger tiers](#danger-tiers) above)
  - [x] Dashboard/Processes unit tiles pair color with an explicit status
        word, never a bare dot
  - [x] Post-action verification on `/dev` — unit-state chips + nav dot
        refresh via htmx OOB swaps after deploy/build/restart
  - [x] Mobile nav affordance: topnav right-edge fade + active-link
        scroll-into-view
  - [x] Widgets reorder via ▲/▼ buttons instead of a free-text order input
  - [x] Tools page: outline vs filled buttons for read-only vs mutating
        commands, a bordered raw-console panel with a stronger confirm for
        guarded verbs
  - [x] CEC recovery ladder wrapped in its own alert-bordered panel
  - [x] Input-truncation fixes (flex `min-width:0`, wider free-text fields)
  - [x] Controllers bindings table reflows to stacked cards below 800px
  - [x] `--danger`/`--error` hue split, checkbox `accent-color`, global
        `:focus-visible` ring
  - [x] Processes page: Top Processes + Hyprland Clients as styled tables;
        `.degraded-msg`/`.stub-msg` collapsed into one class
