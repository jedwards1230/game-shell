# Panel Information Architecture — Redesign

Status: **landed** (all six phases, #405–#410, including phase 5's ansible-side
sudoers generation — see [Phasing](#phasing)). This
document is now a **record of what was built and why**, not a proposal.
[PANEL.md](PANEL.md) remains the living doc for what the panel *does*; this one
keeps the reasoning, and — deliberately — the places where building it proved
the plan wrong. Those corrections are left visible in situ (struck-through
claim, then what actually shipped) rather than tidied away, because the
reasoning that was wrong is the part worth keeping. Three are load-bearing:

- **Recovery mode leaves three groups, not two.** The drawer collapses to
  **Overview + System + Dev**; this document and #405 originally said System
  and Dev. See [Capability gating](#capability-gating).
- **Five of the six `allow_dangerous` controls are in Dev, not all six.**
  `POST /system/updates/apply` is the exception and stays under System ▸
  Updates. See [Dev](#dev).
- **`scope = "system"` restarts work on htpc-1 and fail closed elsewhere.** The
  `htpc_common` sudoers generation shipped in
  [`jedwards1230/homelab-ansible#271`](https://github.com/jedwards1230/homelab-ansible/pull/271)
  and is deployed there; a node whose role run has not applied it still fails
  closed, by design. See [Privilege](#privilege) and [Phasing](#phasing).

## The problem

The panel grew a page per *subsystem the daemon exposes*, not a page per *job the
operator is doing*. Ten flat top-nav tabs, and most of them accreted unrelated
sections until the page stopped having a single subject.

Concretely, measured against the build deployed when this was written
(2026-08-22, before phase 1). Every row is struck because every row is fixed:

| Symptom | Where |
|---|---|
| ~~One page, four unrelated jobs~~ | ~~**Processes** = unit control + system updates + Hyprland window state + top-processes table~~ — split three ways in phase 2 |
| ~~One page, 3133px tall at 1440px wide~~ | ~~**Settings** = eight typed form groups + read-only daemon-owned keys + read-only `config.toml` dump + raw-JSON escape hatch~~ — dissolved in phase 3, escape hatches quarantined on Shell ▸ Advanced |
| ~~Same action on two pages~~ | ~~Unit restart on **Processes** *and* **Dev**~~ — one restart table, on System ▸ Services, since phase 2; Dev ▸ Recovery keeps only the daemon/shell restarts it needs to recover with |
| ~~One subject split across two pages~~ | ~~CEC *config* on **Settings**, CEC *actions* on **CEC**~~ — one page since phase 3 |
| ~~Page is a grab-bag by input method~~ | ~~**Media** = wallpapers + web apps, related only in that both need a keyboard the couch UI lacks~~ — dissolved in phase 4 |
| ~~Overlaps two other pages~~ | ~~**Tools** power/network duplicates **Settings** groups and **Dashboard** tiles~~ — dissolved in phase 4; the duplicated `sys-*` probes were deleted rather than moved |

The common failure is that "which page is this on?" has no principled answer, so
it must be memorized. Two levels of navigation fix that: the drawer answers *what
am I working on*, the sub-nav answers *which aspect*.

## Structure

A persistent **left drawer** for the primary group, and a **horizontal sub-nav**
within the content view for pages inside that group. Six groups, three to four
pages each — small enough that neither level needs scrolling at 1440px.

```
┌────────────┬──────────────────────────────────────────────┐
│ tv-shell   │  Services · Processes · Updates · Logs       │
│            ├──────────────────────────────────────────────┤
│ Overview   │                                              │
│ System   ◄ │   (page content)                             │
│ Shell      │                                              │
│ Devices    │                                              │
│ Remote     │                                              │
│ Dev        │                                              │
│            │                                              │
│ ● daemon   │                                              │
└────────────┴──────────────────────────────────────────────┘
```

| Group | Pages | Subject |
|---|---|---|
| **Overview** | *(none)* | Is everything healthy right now — and where do I go to fix what isn't? |
| **System** | Services · Processes · Updates · Logs | The machine underneath the shell |
| **Shell** | Appearance · Widgets · Apps · Advanced | The TV UI's own configuration |
| **Devices** | Controllers · Display & Audio · CEC · Network | Hardware attached to the box |
| **Remote** | Navigation · Launcher | Driving the TV from here |
| **Dev** | Recovery · Screenshot · Console | Breaking glass |

The daemon-reachability dot moved from the top nav to the drawer footer in
phase 1. It is live reachability and stays orthogonal to nav *shape*, which
remains the startup capability snapshot — see PANEL.md's [Capability gating](PANEL.md#capability-gating).

### Why these six

The grouping is by **who owns the thing being changed**, which is the one
distinction that already exists in the codebase and is therefore stable:

- **System** — owned by the OS. `systemctl`, `pacman`, `ps`, `journalctl`. The
  panel's own exec tier, and the reason it survives a dead daemon.
- **Shell** — owned by `settings.json`, written through the daemon's
  `get-config`/`set-config`.
- **Devices** — owned by hardware plus the daemon subsystems that drive it
  (pads, CEC bus, display/audio sinks, NetworkManager/bluez).
- **Remote** — owned by the running shell: transient commands that change what
  is on screen right now and persist nothing.
- **Dev** — owned by whoever is recovering the box. Danger surface, with the
  one documented exception under [Dev](#dev).

**Overview has no sub-nav on purpose.** It is the only read-only group, it is the
landing page, and giving it tabs would imply there is somewhere else to go for
status. Every tile deep-links into the page that owns that thing. It needed no
special case in the end: rule 4 under [Capability gating](#capability-gating)
drops the sub-nav bar for any group with fewer than two registered pages, and
Overview has one.

## Page-by-page mapping

Every section of the old panel landed somewhere. Six routes were deleted
outright — all six were duplicates of something that already existed
elsewhere (see [Dissolved pages](#dissolved-pages)); nothing else was dropped.

### Overview

✅ **Landed in phase 6** at `/` (also `/overview`). Tiles only, all read-only,
each a whole-tile link to its owning page. The quick-action links are gone —
they became ambiguous once actions moved into groups — and so is every other
mutating control: no form, no button, no `hx-post` on the page or in any of its
partials, on either the reachable or the daemon-down branch. That absence is
pinned by `tests::overview_renders_no_mutating_control` rather than asserted
here, because the failure mode is additive.

Kept: units, build, system, resources, temperatures, storage, controllers,
updates-available. Gained: the **system-services** tile — `[panel].managed_units`
at a glance, since "is sshd up" is the question that motivated the Services page
and belongs on the health screen. With nothing configured (the default, and
every node's state today) it says so and names the config key rather than
rendering a blank card or hiding itself.

| Tile | Owns its subject |
|---|---|
| Input daemon, Controllers | `/devices/controllers` |
| Build | `/dev/recovery` |
| System, Resources, Temperatures, Storage | `/system/processes` |
| Units, System services | `/system/services` |
| Updates | `/system/updates` |

Three poll targets, **one** grid. `/overview/tiles` keeps its 5s cadence and
`/overview/updates-tile` its 300s one; the system-services tile got its own
30s target rather than joining the fast poll, because it costs one `systemctl
show` per configured unit and `managed_units` is operator-set and unbounded —
on the 5s poll the panel's steady-state subprocess rate would be decided by a
config file. The grid itself is declared once on the page and each fragment
swaps bare tiles into a `display: contents` slot inside it, so three cadences
produce one grid instead of three stacked ones (which is what used to strand
the Updates tile alone on a row of its own).

The daemon-down branch survives and gained the services tile: the IPC tiles
collapse to a full-width banner, unit state still comes straight from
`systemd`, and the system-services tile is exec-only so it is unaffected —
which is precisely when you want to know whether `sshd` is up.

### System

| Page | From | Notes |
|---|---|---|
| **Services** | *new*, plus Processes' unit table | ✅ landed at `/system/services` — the three built-in units in phase 2, arbitrary-unit reads and the `managed_units` restart allowlist in phase 5. See [Services](#services-new) below; the sudoers half of phase 5 is still open |
| **Processes** | Processes (top-processes table, Hyprland active window/clients/monitors) | ✅ landed in phase 2 — purely read-only observation, no action affordance at all |
| **Updates** | Processes (System Updates section) | ✅ landed in phase 2 at `/system/updates` — it has its own slow poll, its own background job, and the most dangerous button on the page it used to share |
| **Logs** | Logs | ✅ landed at `/system/logs` in phase 1 — a path move only; contents unchanged |

### Shell

| Page | From | Notes |
|---|---|---|
| **Appearance** | Settings (Appearance group) + Media (Wallpapers) | ✅ landed — the settings half in phase 3, the wallpaper picker in phase 4. `wallpaperPath` moved into the `Appearance` schema group with it and is no longer rendered as a typed path field: the grid is its editor |
| **Widgets** | Widgets | ✅ landed at `/shell/widgets` in phase 1 — a path move only; contents unchanged |
| **Apps** | Settings (`prewarmApps`) + Media (Web apps) | ✅ landed — `prewarmApps` in phase 3, the web-app registry in phase 4. Both are "what can launch on this box" |
| **Advanced** | Settings (daemon-owned keys, read-only `config.toml`, raw-JSON hatch) | ✅ landed in phase 3 — all three escape hatches quarantined behind one deliberate click |

Quarantining the escape hatches is the single biggest win here: the raw-JSON
textarea can write *any* key including the daemon-owned binding layers, and it
used to sit directly below ordinary typed toggles on the same scroll.

Splitting one form into five made the save patch's scope load-bearing: the
builder writes every checkbox in scope as an explicit `true`/`false`, so an
unscoped save from one page would have cleared the other four pages' toggles.
Each form now declares the schema groups it owns and the patch is restricted to
them, fail-closed when nothing is declared — see PANEL.md's
[Scoped settings saves](PANEL.md#scoped-settings-saves).

### Devices

| Page | From | Notes |
|---|---|---|
| **Controllers** | Controllers | ✅ landed at `/devices/controllers` in phase 1 — already single-subject; phase 3 added Settings' `Input` group (`controllerDebug`, `rumbleEnabled`), which is the same subject |
| **Display & Audio** | Settings (Display, Night Light, Power, Audio groups) | ✅ landed in phase 3 at `/devices/display-audio`; phase 4 added Tools' two power probes beside the `Power` group; a **Display mode** section (resolution / refresh / VRR) was added later — the page's first controls that are Hyprland compositor state rather than a `settings.json` slice, with their own IPC path and a confirm-or-revert timer ([PANEL.md § Display mode](PANEL.md#display-mode-resolution-refresh-vrr)) |
| **CEC** | CEC + Settings (CEC group) | ✅ landed in phase 3 — config and actions on one page. The `settings.json` group stays visibly distinct from the `[cec].osd_name` `config.toml` editor beneath it |
| **Network** | Tools (Network, Bluetooth) | ✅ landed in phase 4 at `/devices/network`. Node tier: these commands map to no declared `Feature` |

### Remote

| Page | From | Notes |
|---|---|---|
| **Navigation** | Tools (Navigation: intents, settings deep-link, D-pad keys) | ✅ landed in phase 4 at `/remote/navigation` |
| **Launcher** | Tools (Apps: list/launch/recents) | ✅ landed in phase 4 at `/remote/launcher` |

### Dev

| Page | From | Notes |
|---|---|---|
| **Recovery** | Dev (restart daemon/shell, reboot, suspend, deploy, build) | ✅ landed at `/dev/recovery` in phase 1. The general unit restart *links to* System ▸ Services rather than duplicating it; the daemon/shell restarts stay here because they are the recovery path itself |
| **Screenshot** | Dev (screenshot viewer) | ✅ landed in phase 4 at `/dev/screenshot`. The PNG proxy that used to own that path is now `/dev/screenshot/image` |
| **Console** | Tools (raw IPC line console) | ✅ landed in phase 4 at `/dev/console`. Belongs with the other `allow_dangerous` surfaces, not under a general-purpose tab |

~~Moving the raw console here consolidates the danger surface: after this, every
`allow_dangerous`-gated control in the panel is in one group.~~ **Corrected in
phase 4:** five of the six are. `POST /system/updates/apply` is the exception —
it is the button under System ▸ Updates' pending-package table, sharing that
page's background job and status poll, and separating it from the list it
applies would be a worse page than the tidier danger surface is worth. The
claim the code supports, and the one
`tests::the_dangerous_set_is_the_dev_group_plus_the_updates_apply` enforces, is
**"every `allow_dangerous` control is in the Dev group except the pacman
apply"** — a *second* exception fails the suite. See PANEL.md's
[Dangerous actions](PANEL.md#dangerous-actions-allow_dangerous).

### Dissolved pages

**Settings**, **Media**, and **Tools** no longer exist. Each was a container for
"things that didn't fit elsewhere" rather than a subject. All three are gone:
Settings in phase 3, Media and Tools in phase 4. The old paths keep forwarding
addresses — `/settings` and `/media` → `/shell/appearance`, `/tools` →
`/remote/navigation` — but the old *action* paths (`/media/wallpaper/*`,
`/tools/*`) were deleted, not redirected: they were htmx targets, never
bookmarks.

Two modules outlived their pages, because each grab-bag did have real shared
machinery even though it had no subject: `pages::settings` (the schema, the form
renderer and the scoped patch builder the five settings forms share) and
`pages::ipc_console` (the result partial, reply pretty-printer and argument
validators the four pages the console dissolved into share). Neither is a page.

Six routes were deleted outright rather than moved: Tools ▸ System's four
`sys-status`/`sys-metrics`/`storage-status`/`build-info` probes, whose content
is already on the Overview tiles, and its two `controllerdb-*` buttons, which
were exact duplicates of the Controllers page's own.

## Services (new)

> ✅ **Landed in phase 5**, both halves. The panel side shipped in #412; the
> sudoers generation under [Privilege](#privilege) shipped in
> [`jedwards1230/homelab-ansible#271`](https://github.com/jedwards1230/homelab-ansible/pull/271)
> and is applied to htpc-1. A node whose role run has not applied it fails
> closed, which is the intended behaviour rather than a gap — see PANEL.md's
> [Restartable units](PANEL.md#restartable-units-panelmanaged_units).

The gap that prompted this: there is no way to see whether `sshd` is running, let
alone restart it, without SSHing in — which is precisely what you cannot do when
`sshd` is the thing that is down.

### Scope

- **Read: any unit**, system or user. Status, enabled-state, active-since,
  and the failure reason when failed.
- **Restart: allowlist only.** A configured set of unit names. Arbitrary
  client-supplied unit names must never reach `systemctl`.

The read/write asymmetry is deliberate. Reading unit status is inert; restarting
`sshd` on a headless box is not, and restarting an arbitrary unit is a general
privilege-escalation primitive.

### Preserving the no-arbitrary-unit property

`POST /system/services/restart/{key}` matches `key` against a fixed three-key set
(`daemon`/`shell`/`panel`) and resolves it to a real unit name server-side —
`panel/src/pages/services.rs` calls this out explicitly. That property must
survive: the allowlist is an **index into a server-side table**, not a unit name
passed through.

```toml
[panel]
# Units the Services page may restart. Read is unrestricted; restart is not.
# Each entry is resolved server-side; the client only ever sends an index/key.
managed_units = [
  { key = "sshd",    unit = "sshd.service",           scope = "system" },
  { key = "network", unit = "NetworkManager.service", scope = "system" },
  { key = "bluetooth", unit = "bluetooth.service",    scope = "system" },
]
```

The three tv-shell units stay built in, so a config typo cannot cost you the
recovery path.

### Privilege

System-scope restarts need root; the panel runs as `systemd --user`. Reuse the
mechanism already deployed for updates — the `sudo -n` NOPASSWD path documented in
PANEL.md's [Deployment prerequisite](PANEL.md#deployment-prerequisite-passwordless-sudo-for-the-apply-path)
— but with a **narrow sudoers entry per allowlisted unit**, not blanket
`systemctl`:

```
tv-shell ALL=(root) NOPASSWD: /usr/bin/systemctl restart sshd.service, \
                              /usr/bin/systemctl restart NetworkManager.service
```

To be shipped by the `htpc_common` ansible role, generated from the same list
that renders `managed_units`, so the two cannot drift. **That generation has not
landed** (it was out of scope for the panel PR), so today *every* system-scope
entry is a unit without a matching sudoers line. Which is exactly the case the
panel has to get right, and does: it fails closed with an honest "NOT PERMITTED
on this node", naming the unit and the missing line — never a silent no-op,
never a misleading success.

`systemctl --user` restarts need no sudo and keep working with the daemon down,
which is what makes this page a recovery surface rather than a convenience.

### Danger tier

Restarting `sshd` from a remote browser can strand the box. `.danger-severe`
(PANEL.md's [Danger tiers](PANEL.md#danger-tiers)), with a confirm naming the
specific unit — and, for `sshd` and `NetworkManager`, noting that a failed
restart may end remote access entirely.

## Capability gating

The drawer renders from the same startup capability snapshot the old top nav
used, so a group whose pages all gated off must not render an empty shell.
Rules, all four built:

1. A **page** gates exactly as it did before — not registered, 404, absent from sub-nav.
2. A **group** renders iff at least one of its pages registered.
3. A group's drawer link targets its first *registered* page, not a fixed default.
4. A group with fewer than two registered pages renders **no sub-nav bar** —
   which is what gives Overview no tabs, without special-casing it.

~~In recovery mode (handshake failed, recovery tier only) the drawer collapses to
**System** and **Dev**.~~ **Corrected in phase 1:** the drawer collapses to
**Overview + System + Dev**. Overview is recovery tier — `/` is the landing page
and its tiles already have a daemon-down branch that reads unit state straight
from `systemd`, so deleting the group would leave `/` 404ing or force a
conditional root redirect, which is strictly worse than a three-group drawer.
Shell, Devices and Remote do all vanish, per rule 2. See PANEL.md's
[Capability gating](PANEL.md#capability-gating).

## Phasing

Each phase shipped independently and left the panel working. All six have
landed; the one piece of phase 5 that did not is called out below and is
tracked on #409, not on this table.

| Phase | Scope | Depends on | Issue |
|---|---|---|---|
| **1** ✅ landed | Drawer + sub-nav chrome; group model in `capabilities.rs`; existing pages re-routed under new paths with redirects from old ones. No page content changes. | — | #405 |
| **2** ✅ landed | Split **Processes** → Services (shell only, three built-in units) + Processes + Updates. | 1 | #406 |
| **3** ✅ landed | Dissolve **Settings** → Shell/Appearance, Shell/Apps, Shell/Advanced, Devices/Display & Audio, Devices/CEC — plus the `Input` group onto Devices/Controllers. Saves are now scoped to the groups the submitting form rendered. | 1 | #407 |
| **4** ✅ landed | Dissolve **Media** and **Tools** → Shell/Appearance + Shell/Apps, Devices/Network, Devices/Display & Audio, Remote/Navigation + Remote/Launcher, Dev/Screenshot + Dev/Console. Six duplicate routes deleted rather than moved. | 1, 3 | #408 |
| **5** ✅ landed | Services: read any unit; `managed_units` config; danger-tier confirms. Panel half in #412; ansible-side sudoers generation in [homelab-ansible#271](https://github.com/jedwards1230/homelab-ansible/pull/271), applied to htpc-1. | 2 | #409 |
| **6** ✅ landed | Overview rebuilt as pure read-only tiles with deep links. Gained a system-services tile on its own 30s poll; the three poll targets now fill one grid. | 2-5 | #410 |

Phase 1 was deliberately mechanical — it changed navigation without changing any
page's content, so it could be reviewed as a routing change and reverted cleanly if
the structure felt wrong in use.

**Phase 5's split delivery.** The panel half is done: any unit is readable in
either scope, `[panel].managed_units` is parsed and validated at load (a bad
`key`/`unit`/`scope`, or a key colliding with a built-in, aborts startup),
restart is allowlist-only and resolved server-side, and the confirms are
scope-derived. The **ansible half was explicitly out of scope for that PR** and
shipped separately in
[`jedwards1230/homelab-ansible#271`](https://github.com/jedwards1230/homelab-ansible/pull/271):
`tv_shell_panel_managed_units` renders both the `managed_units` table and
`/etc/sudoers.d/tv-shell-panel` from one list, so config and privilege cannot
drift. It is applied to htpc-1 (sshd, NetworkManager, bluetooth).

**On a node whose role run has not applied those lines, a `scope = "system"`
restart fails closed** — `sudo -n` refuses and the page reports "NOT PERMITTED
on this node", naming the unit and the missing sudoers line. That is the
designed behaviour, not an outstanding gap. Reading those units works, and
`scope = "user"` restarts work. Phase 5 is closed; the lines are generated from
the same list that renders
`managed_units` so the two cannot drift.

## Open questions

- ~~**Drawer on narrow viewports.**~~ **Settled in phase 1:** below 700px the
  drawer collapses to a horizontal strip above the content — no JavaScript, and
  both strips scroll within themselves so the page never scrolls sideways. A
  hamburger would need a build step the panel does not have.
- **Sub-nav persistence.** Returning to a group — land on its first page every
  time, or remember the last page visited within it?
- **`managed_units` beyond restart.** Start/stop are strictly more dangerous than
  restart (stop has no automatic recovery). Restart-only until asked for.
