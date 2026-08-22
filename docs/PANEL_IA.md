# Panel Information Architecture — Redesign

Status: **proposed**. This is the target structure for the panel's navigation and
page boundaries. [PANEL.md](PANEL.md) remains the living doc for what is
*currently* built; this document is what we are moving toward and why. When a
phase lands, its rows move into PANEL.md's Pages table and the corresponding
section here is struck through.

## The problem

The panel grew a page per *subsystem the daemon exposes*, not a page per *job the
operator is doing*. Ten flat top-nav tabs, and most of them accreted unrelated
sections until the page stopped having a single subject.

Concretely, measured against the deployed build (2026-08-22):

| Symptom | Where |
|---|---|
| One page, four unrelated jobs | **Processes** = unit control + system updates + Hyprland window state + top-processes table |
| One page, 3133px tall at 1440px wide | **Settings** = eight typed form groups + read-only daemon-owned keys + read-only `config.toml` dump + raw-JSON escape hatch |
| Same action on two pages | Unit restart on **Processes** *and* **Dev** |
| One subject split across two pages | CEC *config* on **Settings**, CEC *actions* on **CEC** |
| Page is a grab-bag by input method | **Media** = wallpapers + web apps, related only in that both need a keyboard the couch UI lacks |
| Overlaps two other pages | **Tools** power/network duplicates **Settings** groups and **Dashboard** tiles |

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
| **Overview** | *(none)* | Is everything healthy right now? |
| **System** | Services · Processes · Updates · Logs | The machine underneath the shell |
| **Shell** | Appearance · Widgets · Apps · Advanced | The TV UI's own configuration |
| **Devices** | Controllers · Display & Audio · CEC · Network | Hardware attached to the box |
| **Remote** | Navigation · Launcher | Driving the TV from here |
| **Dev** | Recovery · Screenshot · Console | Breaking glass |

The daemon-reachability dot moves from the top nav to the drawer footer. It is
live reachability and stays orthogonal to nav *shape*, which remains the startup
capability snapshot — see PANEL.md's [Capability gating](PANEL.md#capability-gating).

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
- **Dev** — owned by whoever is recovering the box. Unified danger surface.

**Overview has no sub-nav on purpose.** It is the only read-only group, it is the
landing page, and giving it tabs would imply there is somewhere else to go for
status. Every tile deep-links into the page that owns that thing.

## Page-by-page mapping

Every current section lands somewhere; nothing is dropped.

### Overview

Tiles only, all read-only, each linking to its owning page. Drops the current
Dashboard's quick-action links (they became ambiguous once actions moved).

Keeps: units, build, system, resources, temperatures, storage, controllers,
updates-available. Gains: an **sshd/system-services** tile from the new Services
page.

### System

| Page | From | Notes |
|---|---|---|
| **Services** | *new*, plus Processes' unit table | See [Services](#services-new) below |
| **Processes** | Processes (top-processes table, Hyprland active window/clients/monitors) | Becomes purely read-only observation |
| **Updates** | Processes (System Updates section) | Its own page — it has its own slow poll, its own background job, and the most dangerous button on the page it currently shares |
| **Logs** | Logs | Unchanged |

### Shell

| Page | From | Notes |
|---|---|---|
| **Appearance** | Settings (Appearance group) + Media (Wallpapers) | Theme/text-scale/reduce-motion sit next to the wallpaper picker that they visually combine with |
| **Widgets** | Widgets | Unchanged |
| **Apps** | Settings (`prewarmApps`) + Media (Web apps) | Both are "what can launch on this box" |
| **Advanced** | Settings (daemon-owned keys, read-only `config.toml`, raw-JSON hatch) | All three escape hatches quarantined behind one deliberate click |

Quarantining the escape hatches is the single biggest win here: the raw-JSON
textarea can write *any* key including the daemon-owned binding layers, and it
currently sits directly below ordinary typed toggles on the same scroll.

### Devices

| Page | From | Notes |
|---|---|---|
| **Controllers** | Controllers | Unchanged — already single-subject |
| **Display & Audio** | Settings (Display, Night Light, Power, Audio groups) | |
| **CEC** | CEC + Settings (CEC group) | Config and actions finally on one page |
| **Network** | Tools (Network, Bluetooth) | |

### Remote

| Page | From | Notes |
|---|---|---|
| **Navigation** | Tools (Navigation: intents, settings deep-link, D-pad keys) | |
| **Launcher** | Tools (Apps: list/launch/recents) | |

### Dev

| Page | From | Notes |
|---|---|---|
| **Recovery** | Dev (restart daemon/shell, reboot, suspend, deploy, build) | Unit restart *links to* Services rather than duplicating it |
| **Screenshot** | Dev (screenshot viewer) | |
| **Console** | Tools (raw IPC line console) | Belongs with the other `allow_dangerous` surfaces, not under a general-purpose tab |

Moving the raw console here consolidates the danger surface: after this, every
`allow_dangerous`-gated control in the panel is in one group.

### Dissolved pages

**Settings**, **Media**, and **Tools** cease to exist. Each was a container for
"things that didn't fit elsewhere" rather than a subject.

## Services (new)

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

Today `POST /processes/restart/:key` matches `key` against a fixed three-key set
(`daemon`/`shell`/`panel`) and resolves it to a real unit name server-side —
`panel/src/pages/processes.rs` calls this out explicitly. That property must
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

Shipped by the `htpc_common` ansible role, generated from the same list that
renders `managed_units`, so the two cannot drift. A unit in `managed_units`
without a matching sudoers line fails closed with an honest "not permitted on
this node" — never a silent no-op.

`systemctl --user` restarts need no sudo and keep working with the daemon down,
which is what makes this page a recovery surface rather than a convenience.

### Danger tier

Restarting `sshd` from a remote browser can strand the box. `.danger-severe`
(PANEL.md's [Danger tiers](PANEL.md#danger-tiers)), with a confirm naming the
specific unit — and, for `sshd` and `NetworkManager`, noting that a failed
restart may end remote access entirely.

## Capability gating

The drawer is rendered from the same startup snapshot as today's top nav, so a
group whose pages all gated off must not render an empty shell. Rules:

1. A **page** gates exactly as now — not registered, 404, absent from sub-nav.
2. A **group** renders iff at least one of its pages registered.
3. A group's drawer link targets its first *registered* page, not a fixed default.

In recovery mode (handshake failed, recovery tier only) the drawer collapses to
**System** and **Dev** — which is the correct answer: those are exactly the
daemon-independent surfaces, and the operator lands on the two groups that can
still fix things.

## Phasing

Each phase ships independently and leaves the panel working.

| Phase | Scope | Depends on | Issue |
|---|---|---|---|
| **1** | Drawer + sub-nav chrome; group model in `capabilities.rs`; existing pages re-routed under new paths with redirects from old ones. No page content changes. | — | #405 |
| **2** | Split **Processes** → Services (shell only, three built-in units) + Processes + Updates. | 1 | #406 |
| **3** | Dissolve **Settings** → Shell/Appearance, Shell/Apps, Shell/Advanced, Devices/Display & Audio, Devices/CEC. | 1 | #407 |
| **4** | Dissolve **Media** and **Tools** → Shell/*, Devices/Network, Remote/*, Dev/Console. | 1, 3 | #408 |
| **5** | Services: read any unit; `managed_units` config; sudoers generation in the ansible role; danger-tier confirms. | 2 | #409 |
| **6** | Overview rebuilt as pure read-only tiles with deep links. | 2-5 | #410 |

Phase 1 is deliberately mechanical — it changes navigation without changing any
page's content, so it can be reviewed as a routing change and reverted cleanly if
the structure feels wrong in use.

## Open questions

- **Drawer on narrow viewports.** The panel is used from a phone often enough to
  matter. Collapse to a hamburger below ~700px, or an icon-only rail?
- **Sub-nav persistence.** Returning to a group — land on its first page every
  time, or remember the last page visited within it?
- **`managed_units` beyond restart.** Start/stop are strictly more dangerous than
  restart (stop has no automatic recovery). Restart-only until asked for.
