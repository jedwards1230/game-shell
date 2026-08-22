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
> Its pages are now capability-gated (below), the transport is behind a trait,
> and a second implementation — `HttpTransport`, for a **sidecar** node — has
> landed alongside its `[[panel.nodes]]` config (see [below](#the-sidecarremote-node-transport));
> what remains before a second node ships is the node switcher that actually
> serves one, designed in [MULTI_NODE_PANEL.md](MULTI_NODE_PANEL.md).

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

Tiers 1 and 2 are reached through traits, not concrete clients: `AppState`
holds `Arc<dyn NodeTransport>` (`transport.rs`) and `Arc<dyn DevBridge>`
(`bridge.rs`). That is the seam a node reached over HTTP instead of a Unix
socket plugs into — see [MULTI_NODE_PANEL.md](MULTI_NODE_PANEL.md) §2.

### The sidecar/remote-node transport

A **sidecar** node (`tv-shell-host`, e.g. desktop-2) has no local recovery
tier worth serving — see [MULTI_NODE_PANEL.md](MULTI_NODE_PANEL.md) §4 — so it
is reached remotely over HTTP by `HttpTransport` (`panel/src/http.rs`): a
second `NodeTransport` implementation beside `IpcTransport`, speaking the
sidecar's own bearer-auth routes rather than the daemon's IPC vocabulary.

| Command line | Request |
|---|---|
| `capabilities` | `GET /capabilities` |
| `library` | `GET /library` |
| `status` | `GET /status` |
| `open-bpm` | `POST /open-bpm` |
| `sleep` | `POST /sleep` |
| `launch <appid>` | `POST /launch` `{"appid":<u32>}` |
| `quit <appid>` | `POST /quit` `{"appid":<u32>}` |

Configured per node via `[[panel.nodes]]` in `config.toml` (see
`config/config.toml.example`):

```toml
[[panel.nodes]]
id = "desktop-2"
base_url = "http://192.168.8.153:47995"
sidecar_token_file = "~/.config/tv-shell/desktop-2-sidecar-token"
```

`sidecar_token_file`, not `token_file` — a panel may hold credentials only for
**sidecar** nodes it serves, never another shell node's own token. The token
gets the same hygiene as the panel's own (config-dir-confined, 0600,
non-empty); a resolution failure aborts panel startup rather than degrading
silently.

A non-2xx reply is `TransportError::Http { status, body }`, deliberately
**not** `is_unreachable()`: a 401/403 means the node is up but the credential
is wrong (`is_auth_failure()`), and a 404 means the node predates the route —
neither is "the node is down", and collapsing either into `Unreachable` sends
an operator to the wrong machine. Default per-request timeout is 3s, matching
`IpcTransport`; a route whose protocol-level wait can exceed that (`launch`
waits for Big Picture to come up on the sidecar) must pass its own budget via
`NodeTransport::command_timeout`.

**Landed, not yet served.** `HttpTransport` and `[[panel.nodes]]` resolve, but no
page constructs one from a live node — that is the node switcher,
[MULTI_NODE_PANEL.md](MULTI_NODE_PANEL.md) sequencing step 6.

## Pages

> **The [PANEL_IA.md](PANEL_IA.md) redesign is landing in phases.** Phase 1
> (#405) shipped the navigation shell — a left drawer for six subject groups
> with a horizontal sub-nav inside each — and moved every page onto its new
> path, with page *contents* unchanged. Phase 2 (#406) split the Processes
> page three ways under **System**: Services (unit control), Processes
> (read-only observation) and Updates (pacman). Settings, Media and Tools are
> still the pre-split grab-bags, and dissolve in phases 3-4. The table below is
> what is built today.

Every page keeps a forwarding address: the pre-IA path 303s to the new one
(`pages::redirects`), registered in the same capability block as its target so a
redirect can never outlive the page it points at. The two Dashboard htmx
partials moved without redirects — they are poll targets, not bookmarks.

| Page | Path | Contents |
|---|---|---|
| Dashboard | `/` (Overview) | unit status, build info, system/storage tiles, pad fleet, quick actions, an Updates tile (own slow poll — see [System updates](#system-updates-pacman) below) |
| Services | `/system/services` | the three tv-shell systemd **user** units (daemon/shell/panel) with a per-unit Restart (color-coded dot + status word, not just text). `POST /system/services/restart/{key}` matches `key` against that fixed three-key set and resolves it to a real unit name **server-side** — an arbitrary client-supplied unit name never reaches `systemctl`. The panel's own unit carries a distinct confirm saying the restart will disconnect the page you are looking at. Reading arbitrary units and the configurable `managed_units` restart allowlist are phase 5 (#409) |
| Processes | `/system/processes` | **read-only observation, no actions at all**: Hyprland active window/clients (styled table)/monitors via IPC, and a top-processes table (`ps`, CPU-sorted, no kill action in v1) |
| Updates | `/system/updates` | the pacman System Updates section — pending-package table, cache-bypassing Refresh, the background full-update job and its self-terminating status poll (see [System updates](#system-updates-pacman) below) |
| Appearance | `/shell/appearance` | the `Appearance` slice of `settings.json` — theme mode, the two auto-theme hours, reduce-motion, text scale — as typed fields over `get-config`/`set-config` (shallow merge; unmentioned keys are left untouched). Phase 4 (#408) folds the Media page's wallpaper picker in beside it |
| Widgets | `/shell/widgets` | per-widget enabled/order/size/prefs editors (`widgets.<id>` subtree) |
| Apps | `/shell/apps` | the `prewarmApps` list editor (one `StartupWMClass` per line; an emptied box clears the list to `[]`). Phase 4 folds the Media page's web-app registry in here |
| Advanced | `/shell/advanced` | the three escape hatches, quarantined behind one deliberate click: the daemon-owned keys (binding layers + the `webApps` registry, `docs/WEB_APPS.md`) **read-only** — `keyBindings` is editable via the Controllers page's bindings editor, the per-game/per-player layers are read-only there too; a **read-only** `config.toml` view (a general edit path is deferred — editing still requires a manual edit + daemon/panel restart via the Dev page; the one targeted exception anywhere is the CEC page's `[cec].osd_name` editor); and the raw-JSON hatch with its explicit shallow-merge/`null`-deletes warning, which can write *any* key including ones no typed form models (`widgets`, `cecDeviceNames`) and the daemon-owned layers. Client-side JSON-object validation for immediate feedback, with the server-side object check as the authoritative gate |
| Display &amp; Audio | `/devices/display-audio` | the `Display`, `Night Light`, `Power` and `Audio` slices of `settings.json` on one form — HDR, overscan, auto-dim, `wallpaperPath`, night-light schedule/temperature, sleep timer, wake-on-controller, default sink and card profile. Phase 4 (#408) adds the Tools page's power probes |
| Media | `/shell/media` | **Wallpapers**: upload images into `~/.config/tv-shell/wallpapers/` (the dir the shell's Settings ▸ Wallpaper page reads), preview them as a grid, pick the active one (persisted as `wallpaperPath` via `set-config`) or clear it, and delete — this is the only way to get an image onto the box without SSH. Upload is treated as an attack surface in its own right (the route is authenticated, but auth is opt-in and a loopback panel may run without it): extension allowlist, filename sanitization, a containment re-check against the wallpapers dir, a 32 MB cap, and magic-byte sniffing, with the read-back route sharing the same resolver so it can't become an arbitrary file read. **Web apps**: list/add/remove the daemon-owned registry (`webapp-list`/`-add`/`-remove`, #187 P1+P3) — the panel is the add surface because the couch UI has no on-screen keyboard (#20) |
| Tools | `/remote/tools` | IPC console grouped by domain — Navigation (intent/key), Apps (list/launch/recents), Bluetooth (power/scan/list/connect-disconnect-pair-trust), Network (status/wifi/throughput/ping), Power (can-suspend/battery), System (sys-status/sys-metrics/storage-status/build-info/controllerdb); plus a raw-line escape hatch with a warning on commands owned by another page's guarded flow. CEC and controller/pads/bindings commands live on their own Controllers/CEC pages (below) instead. |
| Controllers | `/devices/controllers` | Fleet table (`get-pads`, per-pad battery/rumble-status/bounded rumble test) with a lazy `list-input-devices` diagnostics panel; grab-management (`grab`/`release`/`handoff`) with explanations and confirms on the two that affect the live input path; a bindings editor (`get-bindings`/`set-binding` against the fixed action/button vocabulary, plus a `capture-next`/`capture-cancel` capture-and-apply flow); read-only per-game/per-player binding layers with a `set-active-game`/clear form (editing deferred — use the Advanced page's raw JSON hatch); the `Input` slice of `settings.json` (`controllerDebug`, `rumbleEnabled`), rendered only when the node declares `settings_store` because its save route lives in that block; controller-DB status/refresh |
| CEC | `/devices/cec` | Topology (`cec-scan`/`cec-device`, merged with the `cecDeviceNames` friendly-name overrides); switching (`cec-active-source` as the "switch input" primitive, per-device `cec-power-on`/`-off`, all confirmed); a health panel (`cec-health`/`cec-test`) classifying the daemon's transmit-wedge state, with an escalating "Recover CEC" ladder (test → restart daemon, reusing the Dev page's bridge-then-exec tier logic → link to a full reboot on Dev) that flags the recommended step for the current state; the `CEC` slice of `settings.json` (claim active source on startup/wake, auto-switch on device power-on, default input) so config and actions finally share a page, rendered only when the node declares `settings_store` because its save route lives in that block; and — distinct from all of the above — an Input-name editor for the OSD device name the daemon announces on the bus (`[cec].osd_name`, default = hostname), **the panel's one config.toml write**, done format-preservingly via `toml_edit` and applied by a daemon restart; a build/platform-gated daemon renders as an honest "not available" note, never a failure banner |
| Dev | `/dev/recovery` | restart daemon/restart shell (always available — unit restart is recovery) plus reboot/suspend behind `allow_dangerous` and deploy/build behind `allow_dangerous` **and** the node's `dev_deploy` capability, all with tier labels + confirms; screenshot viewer (provenance sha/branch/version/captured-at, proxied via `/dev/screenshot`, gated on the node's `screenshot` capability) |
| Logs | `/system/logs` | shell + daemon log tails (ANSI-stripped — including "bare" ESC-dropped residue like `[33m`/`[0m` — and wrapped rather than clipped), free-text filter plus one-click "Errors only"/"Hide icon noise" presets, and a Focus Shell/Focus Daemon toggle to expand one pane to full width (state lives on `#log-panels` itself, so it survives every htmx refresh of the panes inside it) |

### Scoped settings saves

`settings.json` is edited from five forms across five pages (Appearance, Apps,
Display & Audio, CEC, Controllers). They share one schema and one patch builder
in `pages::settings`, and the patch is **scoped to the groups the submitting
form actually rendered**.

That scoping is load-bearing, not tidiness. The builder writes every `Bool` in
scope as an explicit `true`/`false` — which is correct, because an unchecked box
is not absent, it is `false` — so an unscoped patch from one page would clear
the 10 checkboxes belonging to the other four. The mechanism:

- each form emits one `<input type="hidden" name="__group" value="…">` per
  `SettingField::group` it renders (Display & Audio emits four);
- the extractor is an ordered `Vec<(String, String)>` rather than a map, so the
  repeated companions survive;
- `build_patch` skips every schema entry whose group was not declared;
- **no `__group` is an error, not a default.** Falling back to "all groups"
  would be exactly the bug;
- a `__group` value unknown to the schema is an error, and so is one outside the
  route's own group list — that list is a server-side constant per page, so a
  hand-rolled POST cannot borrow one page's save route to write another's group.

`widgets` and `cecDeviceNames` are `FieldKind::Complex`: never rendered as a
typed field, never in a typed patch, editable only from Advanced's raw hatch.

### Navigation

Two levels, both rendered from the startup capability snapshot (`NAV` +
`Chrome` in `panel/src/capabilities.rs`), so neither can link to a page whose
routes were not registered — see [Capability gating](#capability-gating):

- a persistent **left drawer** of subject groups — Overview, System, Shell,
  Devices, Remote, Dev;
- a **horizontal sub-nav** at the top of the content view listing the registered
  pages of the active group.

Three rules make that honest rather than decorative: a group renders **iff at
least one of its pages is registered** (no empty group shells); its drawer link
targets its **first registered** page, not a fixed default (so a group whose
usual landing page is gated off still lands somewhere real); and a group with
**fewer than two** registered pages renders **no sub-nav bar at all** — which is
what gives Overview its bare content view without special-casing it.

Below 700px the drawer collapses to a horizontal strip above the content. No
JavaScript: the panel has no build step, and both strips scroll within
themselves so the page never scrolls sideways.

A small daemon-reachability dot lives in the **drawer footer** on every page
(`base.html` + `pages::nav`), polling a cheap, short-timeout `status` probe
every ~10s — green when the daemon answers, red when it doesn't, neutral until
the first poll lands. That dot is live reachability; the nav's *shape* is the
startup snapshot. They answer different questions on purpose.

## System updates (pacman)

`panel/src/updates.rs` owns pacman system-update state independently of the
daemon — the Dashboard Updates tile and the System ▸ Updates page
(`/system/updates`) both read it.

- **Read** (unprivileged): `checkupdates` (pacman-contrib) parsed into
  `{name, old_version, new_version}` rows. Exit code `2` ("no updates
  available") is an OK-empty result, not an error; exit `1` (or a spawn
  failure/timeout) surfaces as an honest error banner. Cached in `AppState`
  (`UpdatesState`) with a 5-minute TTL — `checkupdates` never runs on the
  Dashboard's fast 5s tile poll (the Updates tile polls on its own, much
  slower 300s interval instead); the Updates page's Refresh button
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
- The Updates page's job-status view polls itself
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
Updates page's failure banner shows the last non-empty log line inline
(`last_error_line` in `pages::updates`) rather than a bare "Update failed",
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
group/other-accessible; its contents must be non-empty and **cookie-value-safe**
(RFC 6265 `cookie-octet` — printable ASCII without space, `"`, `,`, `;` or `\`).
Every one of those violations **aborts startup** rather than silently degrading:
an empty file would 401 every request *and* every `/login` submission, and a
token carrying a `;` or a space would come back from the `Cookie:` header
truncated, breaking browser login forever while `Bearer` kept working. The
middleware also fails closed at runtime — auth on with no token rejects
everything.

### How the layer is attached — and the axum footgun it hides

The gate is `.route_layer(..)`, which wraps **only routes that match**. Two
consequences follow, and both serve traffic **unauthenticated** while looking
completely correct:

1. **A `.fallback(..)` catch-all is never wrapped.** It is not a matched route,
   so the layer does not run for it. An auth layer plus a fallback handler is an
   open handler.
2. **Any `.route(..)` registered *after* the `.route_layer(..)` call is never
   wrapped.** Ordering is load-bearing, and nothing about the builder chain
   signals that.

The same hole exists for `.merge(..)`, `.nest(..)`, `.nest_service(..)` and
`route_service`, which graft in handlers the layer never sees.

None of this is visible in review: the router compiles, the app boots, and every
existing test passes. So it is enforced by **test rather than convention** — see
`panel/src/tests.rs`, which asserts the last `.route(` appears before
`.route_layer(`, and rejects each of the five router forms the completeness gate
cannot parse. Adding a route with any other method form fails the suite loudly
instead of slipping through.

The same parser now also attributes every route to the **registration block** it
sits in, so the tier `main.rs` implements and the tier `route_table()` declares
cannot drift, and it panics on any block-opening construct it cannot attribute
(an unrecognized condition, a nested conditional, an unmodelled `app` binding) —
an unattributed route is an unchecked route. On top of that,
**every `post` registered unconditionally must appear in
`RECOVERY_TIER_MUTATING` with a written reason**; a new ungated mutating route
fails the suite rather than earning a review comment.

> This was a live defect, not a hypothetical. The first version of the gating
> test recognized only `get(`/`post(` and silently skipped anything else, so an
> ungated `PUT` served 200 unauthenticated with the whole suite green. A check
> that silently does nothing is indistinguishable from a check that passed —
> which is why the parser now panics on an unrecognized form rather than
> ignoring it.

### Capability gating

The panel asks the node `capabilities` (`docs/IPC_PROTOCOL.md`) **once at
startup**, before the router is built, and registers each route in one of four
tiers — `panel/src/capabilities.rs`:

| Tier | Registered when | Pages |
|---|---|---|
| **Recovery** | always | Overview, Services (+ unit restarts), Processes, Updates, Logs, Dev (+ unit restarts), login, assets |
| **Node** | the handshake succeeded | Tools console |
| **Capability** | the node declared that `Feature` | Appearance, Apps, Advanced, Display & Audio, **the whole Media page incl. the wallpaper files**, and the CEC/Input groups' save routes (`settings_store`), Widgets (`widgets`), web-app add/remove (`web_apps`), Controllers (`controllers`), CEC (`cec`), the Dev screenshot pair (`screenshot`) |
| **Danger** | `[panel].allow_dangerous`, intersected with a capability where a route is both | `/dev/deploy` + `/dev/build` also need `dev_deploy` |

**Two save routes sit in a block their page does not.** A registration block's
condition may name exactly one capability — `crate::tests`'s `main.rs` parser
accepts `allow_dangerous`, `caps.allows(Gate::X)`, or those two ANDed, and
nothing else — so `POST /devices/cec/config` and `POST
/devices/controllers/settings/save` are registered under `settings_store` while
the CEC and Controllers *pages* stay under `cec`/`controllers`. `set-config` is
the capability those two routes actually need. The harmless direction (a route
with no page in front of it) is already precedented by `/tools/raw`; the harmful
one — a page rendering a form that posts to an unregistered route — is closed by
rendering each of those forms only under `caps.allows(Gate::SettingsStore)`.

**A gated-off route 404s — it does not exist.** Non-registration, not a 403 from
a handler: honest, and it leaks nothing about what the node can do. Both nav
levels are built from the same `Gate` values, so a hidden page has no link
either — and a group whose pages all gated off has no drawer entry.

**Gate on what the node actually emits.** `daemon/src/ipc.rs::features()`
deliberately never emits `wallpapers`, `processes`, `system_updates`,
`steam_library` or `game_launch` — the daemon serves none of them. So Services,
Processes and Updates are **recovery** tier (the panel's own exec tier), not
capability tier. Same for `/system/logs`: `Feature::Logs` describes the
*daemon's* `GET /dev/logs`, while this page reads `journalctl` directly.

**Wallpaper upload now needs the daemon** (changed in PANEL_IA phase 1, #405).
The wallpaper routes are panel-local filesystem work and were recovery tier —
correctly *not* gated on `Feature::Wallpapers`, which the daemon never emits.
`settings_store` is a different claim: the daemon does emit it, and picking a
wallpaper always required it (`/media/wallpaper/select` writes `wallpaperPath`
through `set-config`). Gating the whole wallpaper surface on it is what lets the
Shell group vanish cleanly with the daemon down instead of rendering a one-page
shell. The accepted, deliberate cost: **with the daemon down you can no longer
upload a wallpaper** — the one path that put an image on the box without SSH.
Everything that recovers the daemon is untouched.

**Recovery mode leaves three groups: Overview, System, Dev.** PANEL_IA.md
originally said System and Dev; Overview stays because `/` is the landing page
and its tiles already have a daemon-down branch reading unit state from
`systemd`, so removing the group would leave `/` 404ing or force a conditional
root redirect. Shell, Devices and Remote do all disappear.

**Failed handshake ⇒ recovery mode, loudly.** The fallback is the EMPTY feature
set — recovery tier only. That is both fail-closed and exactly the
daemon-independent surface, so the panel keeps what still works and gains
nothing that would lie; it never fails open. Every page then renders a banner
saying so, and the resolved set is logged at `info!` beside the
`bind`/`auth`/`allow_dangerous` line:

```
tv-shell-panel: capabilities — handshake=ok, node_id="htpc-1", features=[cec,controllers,widgets,web_apps,settings_store,shell_lifecycle,screenshot,sleep,dev_deploy,logs]
```

(The order is `Feature`'s derived `Ord`, i.e. **declaration** order — not
alphabetical. `BTreeSet<Feature>` therefore serializes byte-stably but not
sorted by name.)

**A capability change needs a panel restart.** Registration is fixed at startup.
That is sound because the node's set is static too — `features()` derives it from
compile-time cfgs (cargo features, `target_os`) plus startup config
(`[http]`/`[mcp]` binds), and health is deliberately *not* in it, so a wedged CEC
adapter does not drop `cec` and nothing transient can flip a gate. The one-click
recovery is the Services page's own panel-restart button.

**Deployment dependency.** The panel now requires the deployed daemon to be at
or past the commit that added the `capabilities` IPC command. An older daemon
answers, but not with a capability set — the panel fails closed to five pages and
says so, naming version skew rather than telling the operator to wait for a
daemon that is already running. Deploy the daemon first, or deploy both together.

The handshake itself is bounded: 4 attempts on a 1.2s budget each, 1.5s apart
(~9.3s worst case), retried **only** while the node is unreachable — the
documented htpc-1 cold-boot race where the panel unit starts before the daemon's
socket exists. A node that *answers* with a refusal has answered; that fails fast.

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
`sudo -n pacman -Syu`. The line the gate draws:

> **Restarting a unit is recovery** — ungated, because it is the reason the
> panel exists. **Changing what code runs, powering the box, or running
> arbitrary commands is root-equivalent** — gated.

`[panel].allow_dangerous` defaults to **`false`**, and when false these routes
are **not registered at all** (404, not a 403 from a handler) and their buttons
are not rendered:

`POST /dev/deploy` · `/dev/build` · `/dev/reboot` · `/dev/suspend` ·
`/system/updates/apply` · `/tools/raw`

`GET /dev/recovery` stays available (observability), as does every
unit-restart route: `POST /system/services/restart/{key}`, `/devices/cec/recover/restart-daemon`,
`/dev/restart-daemon` and `/dev/restart-shell`. The last two used to be gated,
which bought nothing — they drive the *same* two systemd units that the ungated
`/system/services/restart/{key}` does, so the gate only hid one door to the same room.
`POST /tools/raw` is in the dangerous set because it drives the entire IPC
vocabulary, making it an arbitrary-command escape hatch. It carries **no**
capability gate on top — `allow_dangerous` is already an explicit opt-in to an
arbitrary-command surface, and gating it further would not remove a capability
lie (it reports the node's own error when the node is down). Note the scope
honestly: with the handshake failed, `/remote/tools` is gone too, so what survives is
reachable by `curl`, not from the UI.

`/dev/deploy` and `/dev/build` are the one intersection — they need
`allow_dangerous` **and** the node's `dev_deploy` capability, since they proxy
the daemon bridge. `GET /dev/screenshot` moved out of this list entirely: it is
now gated on `screenshot` (see [Capability gating](#capability-gating)).

## Danger tiers

Mutating buttons across the panel use one of two tiers, distinct from
`--error` (reserved for banners): `.warn-action` (amber-red) for
recoverable-but-disruptive actions — unit restarts, Controllers'
release/handoff, controllerdb refresh — and `.danger-severe` (deep red,
bold border) for actions that take the whole box down or overwrite the
running build — Dev's Reboot/Suspend/Deploy/Build, and the Updates
page's "Run full update". The Services page's own-unit (panel) Restart
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
