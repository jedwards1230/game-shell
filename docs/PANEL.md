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

> **The [PANEL_IA.md](PANEL_IA.md) redesign has landed.** Phase 1
> (#405) shipped the navigation shell — a left drawer for six subject groups
> with a horizontal sub-nav inside each — and moved every page onto its new
> path, with page *contents* unchanged. Phase 2 (#406) split the Processes
> page three ways under **System**: Services, Processes and Updates. Phase 3
> (#407) dissolved **Settings**, and phase 4 (#408) dissolved the last two
> grab-bags, **Media** and **Tools** — so every page below has one subject and
> every group is at its final page set. Phase 5 (#409) gave Services its
> read/restart asymmetry: read any unit, restart only an allowlist (see
> [Restartable units](#restartable-units-panelmanaged_units)) — its
> ansible-side sudoers half is still open. Phase 6 (#410) rebuilt the landing
> page as **Overview**: read-only tiles with deep links, no mutating control
> anywhere on it. The table below is what is built today.

Every page keeps a forwarding address: the pre-IA path 303s to the new one
(`pages::redirects`), registered in the same capability block as its target so a
redirect can never outlive the page it points at. Overview's htmx tile
partials moved without redirects — they are poll targets, not bookmarks.

| Page | Path | Contents |
|---|---|---|
| Overview | `/` (also `/overview`) | **the panel's one purely read-only surface** — tiles only, no form, no button, no `hx-post` anywhere on the page or in its partials (`tests::overview_renders_no_mutating_control`), because every action now lives in the group that owns its subject. Ten tiles, each a whole-tile link to that owner: Input daemon and Controllers → `/devices/controllers`, Build → `/dev/recovery`, System / Resources / Temperatures / Storage → `/system/processes`, Units and System services → `/system/services`, Updates → `/system/updates`. The **System services** tile is `[panel].managed_units` at a glance (`sshd` and friends — see [Restartable units](#restartable-units-panelmanaged_units)); with none configured, which is every node's state today, it says so and names the config key rather than rendering a blank card. Three htmx poll targets fill **one** `.tile-grid` declared on the page — `/overview/tiles` (5s: IPC reads + the three built-in units), `/overview/services-tile` (30s: one `systemctl show` per configured unit, and that list is operator-set and unbounded), `/overview/updates-tile` (300s: `checkupdates` is expensive, see [System updates](#system-updates-pacman) below) — each swapping bare tiles into a `display: contents` slot so three cadences still produce one grid. With the daemon down, the IPC tiles collapse to a full-width banner plus the unit state read straight from `systemd`; the System services tile is exec-only and is unaffected, which is why Overview stays in the recovery-mode drawer |
| Services | `/system/services` | **read any unit, restart an allowlist.** Two restart tables — the three built-in tv-shell **user** units (daemon/shell/panel) and whatever `[panel].managed_units` names — each row showing a color-coded dot + status word, enabled-state, active-since and, when failed, systemd's reason. `POST /system/services/restart/{key}` resolves `key` against that server-side table and refuses an unknown one before any exec; an arbitrary client-supplied unit name never reaches a mutating `systemctl` on any path (the exec API takes a `config::RestartTarget`, which only the table constructs). Below them, an **Inspect any unit** form: any unit, either scope, validated by `config::UnitName::parse` and passed only to `systemctl show`. See [Restartable units](#restartable-units-panelmanaged_units). The panel's own unit carries a distinct confirm saying the restart will disconnect the page you are looking at |
| Processes | `/system/processes` | **read-only observation, no actions at all**: Hyprland active window/clients (styled table)/monitors via IPC, and a top-processes table (`ps`, CPU-sorted, no kill action in v1) |
| Updates | `/system/updates` | the pacman System Updates section — pending-package table, cache-bypassing Refresh, the background full-update job and its self-terminating status poll (see [System updates](#system-updates-pacman) below) |
| Appearance | `/shell/appearance` | the `Appearance` slice of `settings.json` — theme mode, the two auto-theme hours, reduce-motion, text scale — as typed fields over `get-config`/`set-config` (shallow merge; unmentioned keys are left untouched), **plus the wallpaper picker** (phase 4): upload images into `~/.config/tv-shell/wallpapers/` (the dir the shell's Settings ▸ Wallpaper page reads), preview them as a grid, pick the active one or clear it, and delete — the only way to get an image onto the box without SSH. Upload is treated as an attack surface in its own right (the route is authenticated, but auth is opt-in and a loopback panel may run without it): extension allowlist, filename sanitization, a containment re-check against the wallpapers dir, a 32 MB cap, and magic-byte sniffing, with the read-back route sharing the same resolver so it can't become an arbitrary file read. `wallpaperPath` moved into this schema group with the picker and is **not** rendered as a typed field — the grid is its editor (`settings::CUSTOM_EDITOR_KEYS`) |
| Widgets | `/shell/widgets` | per-widget enabled/order/size/prefs editors (`widgets.<id>` subtree) |
| Apps | `/shell/apps` | what can launch on this box: the `prewarmApps` list editor (one `StartupWMClass` per line; an emptied box clears the list to `[]`), and — since phase 4 — the daemon-owned **web-app registry** (`webapp-list`/`-add`/`-remove`, #187 P1+P3). The panel is the add surface because the couch UI has no on-screen keyboard (#20); the daemon validates, allocates the id/`wmClass` and writes the `.desktop`, so the panel only relays. Removing one keeps its Chromium profile, so re-adding restores logins. The two registry routes are gated on `web_apps` while the page is `settings_store`, so the add/remove forms render only when the node declared both |
| Advanced | `/shell/advanced` | the three escape hatches, quarantined behind one deliberate click: the daemon-owned keys (binding layers + the `webApps` registry, `docs/WEB_APPS.md`) **read-only** — `keyBindings` is editable via the Controllers page's bindings editor, the per-game/per-player layers are read-only there too; a **read-only** `config.toml` view (a general edit path is deferred — editing still requires a manual edit + daemon/panel restart via the Dev page; the one targeted exception anywhere is the CEC page's `[cec].osd_name` editor); and the raw-JSON hatch with its explicit shallow-merge/`null`-deletes warning, which can write *any* key including ones no typed form models (`widgets`, `cecDeviceNames`) and the daemon-owned layers. Client-side JSON-object validation for immediate feedback, with the server-side object check as the authoritative gate |
| Display &amp; Audio | `/devices/display-audio` | the `Display`, `Night Light`, `Power` and `Audio` slices of `settings.json` on one form — HDR, overscan, auto-dim, night-light schedule/temperature, sleep timer, wake-on-controller, default sink and card profile — plus the two live power probes (`power-can-suspend`, `power-battery`) beside the `Power` group they report on. Those two are **node** tier while the page is `settings_store`, so the buttons render only when the handshake succeeded. `wallpaperPath` left this page in phase 4 (see Appearance) |
| Controllers | `/devices/controllers` | Fleet table (`get-pads`, per-pad battery/rumble-status/bounded rumble test) with a lazy `list-input-devices` diagnostics panel; grab-management (`grab`/`release`/`handoff`) with explanations and confirms on the two that affect the live input path; a bindings editor (`get-bindings`/`set-binding` against the fixed action/button vocabulary, plus a `capture-next`/`capture-cancel` capture-and-apply flow); read-only per-game/per-player binding layers with a `set-active-game`/clear form (editing deferred — use the Advanced page's raw JSON hatch); the `Input` slice of `settings.json` (`controllerDebug`, `rumbleEnabled`), rendered only when the node declares `settings_store` because its save route lives in that block; controller-DB status/refresh |
| CEC | `/devices/cec` | Topology (`cec-scan`/`cec-device`, merged with the `cecDeviceNames` friendly-name overrides); switching (`cec-active-source` as the "switch input" primitive, per-device `cec-power-on`/`-off`, all confirmed); a health panel (`cec-health`/`cec-test`) classifying the daemon's transmit-wedge state, with an escalating "Recover CEC" ladder (test → restart daemon, reusing the Dev page's bridge-then-exec tier logic → link to a full reboot on Dev) that flags the recommended step for the current state; the `CEC` slice of `settings.json` (claim active source on startup/wake, auto-switch on device power-on, default input) so config and actions finally share a page, rendered only when the node declares `settings_store` because its save route lives in that block; and — distinct from all of the above — an Input-name editor for the OSD device name the daemon announces on the bus (`[cec].osd_name`, default = hostname), **the panel's one config.toml write**, done format-preservingly via `toml_edit` and applied by a daemon restart; a build/platform-gated daemon renders as an honest "not available" note, never a failure banner |
| Network | `/devices/network` | the box's two radios, from the dissolved Tools page: NetworkManager (link status, Wi-Fi list/rescan, per-interface throughput, ping) and bluez (adapter power, discovery, the known-device list with per-device connect/disconnect/pair/trust). Pairing a gamepad here is the keyboard-free alternative to the couch UI's own Bluetooth page. Every argument that becomes part of an IPC command line goes through the shared validators in `pages::ipc_console`: a ping host and a `wm_class` must be single tokens, an interface name additionally carries no `/` or `..` (it reaches a sysfs path daemon-side), and a ping count must be an integer in `1..=10` |
| Navigation | `/remote/navigation` | driving the running shell from here, from the dissolved Tools page: free-text and quick-button intents, the three overlay quick actions, a settings deep-link picker over the documented slug vocabulary, and the six-name D-pad key vocabulary (validated server-side too, not just by the fixed button values). Nothing here persists — these change what is on screen right now |
| Launcher | `/remote/launcher` | `list-apps` rendered with a per-app Launch button (`intent app:<wmClass>`), plus `get-recents`. What *can* launch is Shell ▸ Apps; this is what to launch now |
| Recovery | `/dev/recovery` | restart daemon/restart shell (always available — unit restart is recovery) plus reboot/suspend behind `allow_dangerous` and deploy/build behind `allow_dangerous` **and** the node's `dev_deploy` capability, all with tier labels + confirms. Every action's response carries out-of-band unit chips + a nav-dot refresh, so the operator sees the unit actually came back |
| Screenshot | `/dev/screenshot` | the screenshot viewer, its own page since phase 4 — the one read-only surface on a page otherwise made of destructive buttons, and legible at full width. `POST /dev/screenshot/capture` confirms the bridge answers and reads the provenance line (sha/branch/version/captured-at); only then is an `<img>` emitted, pointing at `GET /dev/screenshot/image` — the PNG proxy, renamed from `/dev/screenshot` to free that path for the page. Gated on the node's `screenshot` capability; a node that declares it while this panel has no HTTP bridge configured is told so up front rather than on click |
| Console | `/dev/console` | the raw IPC line console from the dissolved Tools page: sends any single command from the daemon's vocabulary and shows the raw reply, with a warning banner on the verbs owned by another page's guarded flow (`set-config`, `set-binding`, `grab`, `release`, `handoff`) and a sharpened client-side confirm for the same list. The **page** is node tier; `POST /dev/console/raw` is in the `allow_dangerous` set, so with that off (the default, and htpc-1's setting) the page renders an explanatory banner and **no form** — never a button that 404s |
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

`wallpaperPath` is a third kind of exception, added in phase 4. It is an
ordinary `FieldKind::Str` in the `Appearance` group, but it is listed in
`settings::CUSTOM_EDITOR_KEYS` and therefore **not rendered as a typed input** —
the wallpaper grid on the same page is its editor, and a raw path field beside
that grid would be a second, worse one that bypasses the picker's containment
checks. Omitting a field from the form omits it from the patch, which is safe
here *only because it is not a `Bool`*: non-`Bool` kinds are written only when
present, so the daemon's shallow merge leaves the current selection alone. A
`Bool` handled this way would be written `false` on every save.

### Navigation

Two levels, both rendered from the startup capability snapshot (`NAV` +
`Chrome` in `panel/src/capabilities.rs`), so neither can link to a page whose
routes were not registered — see [Capability gating](#capability-gating):

- a persistent **left drawer** of six subject groups — Overview, System, Shell
  (Appearance · Widgets · Apps · Advanced), Devices (Controllers · Display &
  Audio · CEC · Network), Remote (Navigation · Launcher), Dev (Recovery ·
  Screenshot · Console);
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
daemon — Overview's Updates tile and the System ▸ Updates page
(`/system/updates`) both read it.

- **Read** (unprivileged): `checkupdates` (pacman-contrib) parsed into
  `{name, old_version, new_version}` rows. Exit code `2` ("no updates
  available") is an OK-empty result, not an error; exit `1` (or a spawn
  failure/timeout) surfaces as an honest error banner. Cached in `AppState`
  (`UpdatesState`) with a 5-minute TTL — `checkupdates` never runs on
  Overview's fast 5s tile poll (the Updates tile polls on its own, much
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
Overview's Updates tile is unaffected either way, since it only reflects the
unprivileged `checkupdates` read.

## Restartable units (`[panel].managed_units`)

System ▸ Services is deliberately **asymmetric**: reading a unit's status is
inert, so it is unrestricted; restarting one is not, so it is allowlisted.

```toml
[panel]
managed_units = [
  { key = "sshd",      unit = "sshd.service",           scope = "system" },
  { key = "network",   unit = "NetworkManager.service", scope = "system" },
  { key = "bluetooth", unit = "bluetooth.service",      scope = "system" },
]
```

**The allowlist is an index into a server-side table, not a unit name passed
through.** The browser only ever sends `key`;
`POST /system/services/restart/{key}` resolves it via
`AppConfig::restart_target` and refuses an unknown key before any exec. The
resolved value is a `config::RestartTarget`, whose fields are private and whose
only constructors resolve a key against that table — and
`Recovery::restart` accepts nothing else. So there is no signature a
client-supplied unit name could arrive through, and two tests keep it that way:
`the_only_mutating_systemctl_argv_is_a_restart_target` reads `exec.rs` and
requires every argv element of a mutating `systemctl` to be a string literal or
`target.unit().as_str()`, and
`restart_target_is_only_constructible_from_the_server_side_table` pins the
constructor set.

**The three tv-shell units stay built in** (`daemon`, `shell`, `panel`), so a
config typo cannot cost the recovery path. A `managed_units` entry whose key
collides with a built-in is a **startup error**, not a silent shadow — as is an
empty or duplicated key, a `unit` that is not a plausible systemd unit name, or
a `scope` that is not exactly `system` or `user`. The list is a privilege
boundary; a quietly-dropped entry would be discovered mid-incident.

The read side takes any unit in either scope. An operator-typed name goes
through `config::UnitName::parse` — non-empty, ≤255 bytes, ASCII
`[A-Za-z0-9._@:-]` only, no leading `-` (which `systemctl` would read as an
option: the one real injection this interface has), no `..`, and a known unit
suffix if it has one — and only the parsed value reaches `systemctl show`.
Escaped names (`dev-disk-by\x2duuid-….device`) carry a backslash and are
therefore not addressable; mount and device units are not what this surface is
for.

### Deployment prerequisite: a per-unit sudoers line for `scope = "system"`

The panel runs as `systemd --user`, so a system-scope restart needs root. It
reuses the same `sudo -n` NOPASSWD mechanism as the [pacman apply
path](#deployment-prerequisite-passwordless-sudo-for-the-apply-path), but with
a **narrow entry per allowlisted unit** rather than blanket `systemctl`:

```
tv-shell ALL=(root) NOPASSWD: /usr/bin/systemctl restart sshd.service, \
                              /usr/bin/systemctl restart NetworkManager.service
```

The panel's argv is exactly `sudo -n systemctl restart <unit>` — no `--`
separator and no extra flags, because sudoers matches on the whole command
line and anything else would stop matching the rule. That is safe because
`<unit>` came out of the validated table, not off the wire.

`scope = "user"` units go through `systemctl --user` and **never** through
`sudo`. That is what keeps Services working with the daemon down, i.e. what
makes it a recovery surface rather than a convenience.

**Failure mode when the rule is absent, and it is the current state of every
node**: `sudo -n` exits non-zero immediately printing something like `sudo: a
password is required`. `exec::classify_sudo_failure` tells that apart from the
restart itself failing and returns `ExecError::NotPermitted`, which the page
renders as an explicit refusal — `NOT PERMITTED on this node: <unit> was not
restarted, and nothing was run`, followed by the exact sudoers line that is
missing. Never a silent no-op, never a misleading success. A genuine restart
failure (`Job for sshd.service failed…`) is *not* reported as a permission
problem — the two send an operator to different places.

> **The ansible side has not landed.** The `htpc_common` role in
> `jedwards1230/homelab-ansible` is where these sudoers lines will be
> generated, from the same list that renders `managed_units` so the two cannot
> drift. Until it does, **every `scope = "system"` restart fails closed on every
> node, htpc-1 included.** Listing a system unit in `managed_units` today makes
> it readable and visible on the page; it does not make it restartable.
> `scope = "user"` entries work now.

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
| **Recovery** | always | Overview, Services (+ unit restarts), Processes, Updates, Logs, Dev ▸ Recovery (+ unit restarts), login, assets |
| **Node** | the handshake succeeded | Devices ▸ Network, Remote ▸ Navigation, Remote ▸ Launcher, Dev ▸ Console (the *page*), and the two Display & Audio power probes |
| **Capability** | the node declared that `Feature` | Appearance **incl. the wallpaper files**, Apps, Advanced, Display & Audio, and the CEC/Input groups' save routes (`settings_store`), Widgets (`widgets`), web-app add/remove (`web_apps`), Controllers (`controllers`), CEC (`cec`), the three Dev ▸ Screenshot routes (`screenshot`) |
| **Danger** | `[panel].allow_dangerous`, intersected with a capability where a route is both | `/dev/deploy` + `/dev/build` also need `dev_deploy` |

**Some routes sit in a block their page does not.** A registration block's
condition may name exactly one capability — `crate::tests`'s `main.rs` parser
accepts `allow_dangerous`, `caps.allows(Gate::X)`, or those two ANDed, and
nothing else — so a page needing two capabilities has its routes split across
two blocks, in whichever direction fits:

| Route | Its block | Its page's block |
|---|---|---|
| `POST /devices/cec/config` | `settings_store` | `cec` |
| `POST /devices/controllers/settings/save` | `settings_store` | `controllers` |
| `POST /shell/apps/webapp/{add,remove}` | `web_apps` | `settings_store` |
| `POST /devices/display-audio/power/{can-suspend,battery}` | node | `settings_store` |
| `POST /dev/console/raw` | `allow_dangerous` | node |

Each route sits in the block naming the capability it *actually* needs. The
harmless direction — a route with no page in front of it — is precedented by
`/dev/console/raw`. The harmful one, a page rendering a control that posts to an
unregistered route, is closed by rendering each of those controls only under the
gate its route sits behind, and `no_page_renders_an_unregistered_target_*`
fetches every page under three capability sets and checks exactly that.

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
wallpaper always required it (select writes `wallpaperPath` through
`set-config`). Gating the whole wallpaper surface on it is what lets the
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
answers, but not with a capability set — the panel fails closed to the six
recovery-tier pages (Overview, Services, Processes, Updates, Logs, Dev ▸
Recovery) and says so, naming version skew rather than telling the operator to
wait for a daemon that is already running. Deploy the daemon first, or deploy both together.

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
`/dev/console/raw` · `/system/updates/apply`

**Almost all of them are in the Dev group — but not quite all.** Phase 4 (#408)
moved the raw IPC console to Dev ▸ Console to consolidate the danger surface,
and PANEL_IA.md's phrasing was "after this, every `allow_dangerous`-gated
control in the panel is in one group". That is true of five of the six.
`POST /system/updates/apply` is the exception and stays where it is: it is the
button at the bottom of System ▸ Updates' pending-package table, sharing that
page's background job and its self-terminating status poll, and moving the
button away from the list it applies and the log tail it produces would be a
worse page for a marginal gain in tidiness. So the accurate claim is **"every
`allow_dangerous` control lives in the Dev group except the pacman apply"**, and
`tests::the_dangerous_set_is_the_dev_group_plus_the_updates_apply` enforces
exactly that — a *second* exception fails the suite rather than quietly
widening the sentence.

`GET /dev/recovery` stays available (observability), as does every
unit-restart route: `POST /system/services/restart/{key}`, `/devices/cec/recover/restart-daemon`,
`/dev/restart-daemon` and `/dev/restart-shell`. The last two used to be gated,
which bought nothing — they drive the *same* two systemd units that the ungated
`/system/services/restart/{key}` does, so the gate only hid one door to the same room.
`POST /dev/console/raw` is in the dangerous set because it drives the entire IPC
vocabulary, making it an arbitrary-command escape hatch. It carries **no**
capability gate on top — `allow_dangerous` is already an explicit opt-in to an
arbitrary-command surface, and gating it further would not remove a capability
lie (it reports the node's own error when the node is down). Note the scope
honestly: with the handshake failed, `/dev/console` is gone too, so what
survives is reachable by `curl`, not from the UI. The inverse case — the page
registered while the route is not, which is htpc-1's actual state — renders an
explanatory banner and no form at all.

`/dev/deploy` and `/dev/build` are the one intersection — they need
`allow_dangerous` **and** the node's `dev_deploy` capability, since they proxy
the daemon bridge. The screenshot routes moved out of this list entirely: they
are gated on `screenshot` (see [Capability gating](#capability-gating)).

## Danger tiers

Mutating buttons across the panel use one of two tiers, distinct from
`--error` (reserved for banners): `.warn-action` (amber-red) for
recoverable-but-disruptive actions — `--user` unit restarts, Controllers'
release/handoff, controllerdb refresh — and `.danger-severe` (deep red,
bold border) for actions that take the whole box down or overwrite the
running build — Dev's Reboot/Suspend/Deploy/Build, the Updates page's
"Run full update", and **every system-scope restart on the Services page**.

On Services the tier is derived from the unit's scope rather than from which
table it is in, which is the honest cut: a `--user` restart needs no elevation
and is itself the recovery path, while a `scope = "system"` restart is elevated
and can take a service the whole box depends on with it. So the three built-in
tv-shell units stay tier 1 and a `managed_units` entry is severe iff it is
system-scope.

Every Services confirm names the specific unit, and three cases say more:

- **the panel's own unit** — the click disconnects the very page being looked
  at, so the confirm says so rather than reusing the generic wording;
- **remote-access-critical units** — a failed restart may end remote access
  entirely and leave the box needing physical attention;
- everything else gets `Restart <unit> now?`.

"Remote-access-critical" is `RestartTarget::is_remote_access_critical`, a small
explicit set (`sshd`, `ssh`, `dropbear`, `NetworkManager`, `systemd-networkd`,
`networking`, `iwd`, `wpa_supplicant`, `dhcpcd`, `tailscaled`) matched on the
lowercased unit stem, and never for a user-scope unit. It is a list rather than
a derivation because systemd exposes no property that honestly answers "is this
how I am connected right now" — `WantedBy=network.target` is true of plenty of
units whose failure costs nothing. The membership criterion is narrow and
checkable by eye: **system-scope units that either serve the remote login
session or own the network link it runs over.**

## QA

Every page, its gate, what it does with the daemon down, and the cross-cutting
states worth checking (recovery mode, `allow_dangerous = false`, an empty
allowlist, narrow viewport): **[qa-panel-views.md](qa-panel-views.md)**.

Linked rather than `@import`ed on purpose — `CLAUDE.md` imports the *shell*
catalog in full, and most sessions never touch the panel.

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
        colored state dot (Overview tile, Controllers fleet), raw token kept
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
