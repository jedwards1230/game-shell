# Multi-Node Panel — one UI, N nodes, capability-gated

> Status: **the pattern is built; the second node is not served yet.** Steps 1–4
> of the sequencing have landed and are deployed — the panel authenticates, fails
> closed on an insecure bind, and gates route *registration* on a capability
> handshake, which htpc-1's daemon (`input-v0.3.0`) and desktop-2's sidecar
> (`host-v0.7.0`) both answer in production.
>
> `HttpTransport` and the `[[panel.nodes]]` config that points one at a sidecar have
> since **landed** too (step 5) — desktop-2's sidecar still has no UI, because
> nothing yet constructs an `HttpTransport` from a resolved node and serves it;
> that is the node switcher (step 6), not the transport. §4 has since been
> amended — a sidecar is served **remotely** rather than running its own panel,
> which takes a Windows build off the path entirely.

## The problem

The panel is structurally single-node today, in four separate ways:

| Coupling | Where | Consequence |
|---|---|---|
| ~~Transport is concrete~~ — **fixed**, see §2 | `AppState` now holds `Arc<dyn NodeTransport>` + `Arc<dyn DevBridge>`; pages call `state.node.*` | — |
| Unix socket is unconditional | `panel/` dials `AF_UNIX` with no `cfg` | The crate **does not build on Windows** (`CONTRIBUTING.md` says so explicitly) |
| ~~Routes are flat and ungated~~ — **fixed**, see §1 | `build_router` registers each route in one of four tiers (recovery / node / capability / danger); only 22 stay unconditional, 7 of them `post` | — |
| Platform ops are Linux-only | `exec.rs` (systemd), `updates.rs` (pacman) | A Windows node has no systemctl and no `checkupdates` |

The two rows that remain are **only** blockers for putting a panel *on* a Windows
node. §4 stops requiring that, so neither is on the current path — the gap that
actually keeps desktop-2 UI-less is the missing `HttpTransport`, not either of
these.

`protocol/` already exists as the shared daemon↔host wire-type crate, which is
the natural home for the fix.

## The pattern

### 1. Capability is declared by the node, never inferred by the panel

Add to `protocol/`:

```rust
pub struct Capabilities {
    pub node_id: String,        // "htpc-1", "desktop-2"
    pub kind: NodeKind,         // Shell | Sidecar
    pub agent_version: String,  // the REAL release version — see §Version below
    pub platform: Platform,     // Linux | Windows | MacOS
    pub features: BTreeSet<Feature>,
}

pub enum Feature {
    // shell-node features
    Cec, Controllers, Widgets, Wallpapers, WebApps, SettingsStore,
    ShellLifecycle, Screenshot,
    // sidecar features
    SteamLibrary, GameLaunch, Sleep,
    // shared, platform-dependent
    DevDeploy, Logs, Processes, SystemUpdates,
}
```

Served as `capabilities` over IPC (shell node) and `GET /capabilities` (sidecar).
The panel builds its nav and registers its routes from this set — it never
sniffs, probes, or guesses.

This mirrors a principle the codebase already commits to elsewhere: *"Screen
ownership is declared, never inferred"* (`shell-focus`). Same reasoning, same
failure mode if violated — a probe answers a question adjacent to the one you
asked, and is confidently wrong.

**Gating is on the route, not just the nav.** A hidden nav link with a live route
behind it is not a gate. Registering an ungated mutating route should be a test
failure, not a review comment.

**Landed** in `panel/src/capabilities.rs`. Four tiers, one `Gate` value each,
resolved from a snapshot taken **once at startup**:

> Route paths below are post-IA (`docs/PANEL_IA.md`). The pre-IA paths this
> section used to name — `/settings/*`, `/media/*`, `/tools/*` — no longer
> exist; the page paths among them 303-redirect, while the old *action* paths
> (`/tools/raw`, `/media/wallpaper/*`, `/processes/updates/apply`,
> `/tools/sys/controllerdb-*`) were deleted outright and 404.

| Tier | Registered when | Routes |
|---|---|---|
| Recovery | always | Overview + its tiles, System ▸ Processes / Updates / Logs, System ▸ Services incl. unit restarts, Dev ▸ Recovery, nav dot, login, assets, and the pre-IA redirects |
| Node | the handshake succeeded | Remote ▸ Navigation and Remote ▸ Launcher, Devices ▸ Network, and Dev ▸ Console's probe surface (minus `raw`) |
| Capability | the named `Feature` is declared | `cec` → `/devices/cec/*`; `controllers` → `/devices/controllers/*`; `widgets` → `/shell/widgets/*`; `settings_store` → `/shell/appearance/*`, `/shell/apps/save`, `/shell/advanced/*`, `/devices/display-audio/*`, `/devices/cec/config`, `/devices/controllers/settings`, and the wallpaper surface; `web_apps` → the web-app registry under `/shell/apps`; `screenshot` → `/dev/screenshot*` |
| Danger | `[panel].allow_dangerous`, **intersected** with a capability where a route is both | `/dev/deploy` + `/dev/build` also need `dev_deploy`; `/dev/reboot`, `/dev/suspend`, `/system/updates/apply`, `/dev/console/raw` are the panel's own exec tier and carry no capability gate |

There are **nine** `Gate` variants, not four tiers of hand-written conditions:
`Recovery`, `Node`, `Cec`, `Controllers`, `Widgets`, `SettingsStore`, `WebApps`,
`Screenshot`, `DevDeploy`. The `gates!` macro generates `Gate::ALL` from the
same variant list as the enum, so exhaustiveness is **structural** — a variant
cannot be added and left out.

**The gate must be checked against what the node actually emits.**
`daemon/src/ipc.rs::features()` deliberately never emits `wallpapers`,
`processes`, `system_updates`, `steam_library` or `game_launch` — so gating
System ▸ Processes or System ▸ Updates on the matching `Feature` would have
deleted those working pages from htpc-1. They are recovery tier because the
panel serves them itself, out of its own filesystem and exec tier.
`Feature::Logs` describes the *daemon's* `GET /dev/logs`, so the panel's
System ▸ Logs page — `journalctl` via direct exec — is recovery tier too.

**Wallpaper is the deliberate exception, and it moved.** It reads like it
belongs above (panel-local filesystem, no daemon needed), and before the IA
refresh it was recovery tier for exactly that reason. #412 moved the whole
wallpaper surface under `Gate::SettingsStore` so the Shell group can vanish
cleanly in recovery mode rather than leaving a one-page shell. The cost —
wallpaper upload is unavailable with the daemon down — was accepted knowingly;
see PANEL.md. Do not "fix" this back.

**A failed handshake falls back to the EMPTY set**, i.e. recovery tier only —
fail-closed, and identical to the daemon-independent set, so the panel keeps
precisely what still works and gains nothing that would lie. Never fail-open.
The degraded state is rendered as a banner, not left silent.

**Registration is static, so a capability change needs a panel restart.** Sound
because the node's set is itself static: compile-time cfgs plus startup config,
with health explicitly excluded (a wedged CEC adapter does not drop `cec`).

Enforced by test, not convention: `panel/src/tests.rs` parses `build_router`
and attributes every route to its registration block, then asserts that against
the hand-maintained `route_table()`; every unconditional `post` must appear in
`RECOVERY_TIER_MUTATING` (**5** entries, each **with a written reason**); and a
live-router test pins that htpc-1's declared set still registers exactly today's
**108** routes.

### 2. A transport trait replaces the concrete clients — **landed**

`panel/src/transport.rs`:

```rust
#[async_trait]
pub trait NodeTransport: Send + Sync {
    async fn capabilities(&self) -> Result<Capabilities, TransportError>;
    async fn command(&self, line: &str) -> Result<String, TransportError>;
    async fn command_timeout(&self, line: &str, t: Duration) -> Result<String, TransportError>;
    fn reachability(&self) -> Reachability;
}
```

The generic and derived helpers (`command_json::<T>`, `get_config`,
`set_config`) live on a blanket-implemented `NodeTransportExt`, defined purely
in terms of `command` — the base trait has to stay object-safe because
`AppState` holds `Arc<dyn NodeTransport>`. The dev-ops tier gets a parallel
`DevBridge` trait (`bridge.rs`); it stays separate because it is a fixed set of
named HTTP operations, not a `command(line)` surface.

The command is a **`&str` line, not a typed `Command`/`Response` pair**, as this
section originally sketched. No such wire vocabulary exists to reuse — the
daemon's `Command` enum is private to `daemon/src/protocol.rs` and `protocol/`
models none of it — so introducing one would be new surface plus a rewrite of
every call site's error handling, not a refactor. The panel keeps speaking the
line protocol of `docs/IPC_PROTOCOL.md`.

Implementations:

- `IpcTransport` — the existing `ipc.rs`, gated `#[cfg(unix)]`. **Landed.**
- `HttpTransport` — bearer-auth HTTP wrapping the sidecar's routes. **Landed**
  (`panel/src/http.rs`); its consumer is §4's remote-panel case, not a Windows
  build — nothing constructs one from a live node yet (step 6).

#### What actually blocks a Windows build

Worth stating precisely, because the imprecise version ("`exec.rs` shells to
`systemctl`") reads as a compile blocker and is not one. **Capability gating is a
runtime decision; compilation is not** — every page module is compiled into the
binary regardless of what any node declares, so `pages/cec.rs` blocks a Windows
build even on a node that would never render the CEC page.

The complete blocker list, verified against the tree:

| Blocker | Where |
|---|---|
| `tokio::net::UnixStream` | `ipc.rs:25`, `ipc.rs:49` |
| `libc::getuid()` | `config.rs:1069` |
| `libc::gethostname()` | `pages/cec.rs:705` |
| `tokio::net::UnixListener` | `ipc.rs` + `tests.rs` — **test-only** |

`libc` is declared under `[target.'cfg(unix)'.dependencies]`, so the two `libc`
call sites fail at *name resolution* off unix, not merely at link time.
(`config.rs`'s `std::os::unix::fs::PermissionsExt` uses are already `cfg(unix)`-
gated and are not blockers.)

**`systemctl` / `journalctl` / `pacman` / `checkupdates` are NOT compile
blockers.** They are `Command::new("…")` string literals in `exec.rs` and
`updates.rs`; `std::process::Command` compiles on Windows and those calls simply
fail at runtime. That distinction matters for scoping: porting them is a
`PlatformOps` behavior question (§3), not a prerequisite for the crate to build.

`CONTRIBUTING.md`'s "does not build on Windows" remains true and the panel's CI
stays Linux/macOS — but note that **§4 removes the reason to change that**: a
sidecar node is served by a Linux-built panel over HTTP, so no Windows panel
build is on the path.

### 3. Platform ops behind a second trait

`exec.rs` and `updates.rs` are the remaining Linux-isms.

| Op | Linux | Windows |
|---|---|---|
| Service restart | `systemctl --user restart` | Task Scheduler (the sidecar already deploys as a scheduled task) |
| Logs | `journalctl` | Event Log / the task's log file |
| Updates | `checkupdates` + `pacman -Syu` | `winget upgrade` / Windows Update |

Each is a declared capability, so a node that can't do one simply doesn't render
that page — rather than rendering a page that errors on click.

> **Deprioritized by §4.** The Windows column existed to support a panel deployed
> *onto* a sidecar. Serving sidecars remotely removes that need, so no `PlatformOps`
> port is on the current path. The trait is still the right shape if a **shell**
> node ever runs a non-Linux OS; nothing plans that. Note also that none of these
> are compile blockers (§2) — they are runtime shell-outs, so a Windows build
> would produce pages that compile and fail on click, which is precisely what a
> declared capability is meant to prevent.

### 4. Local panel for a shell node; remote panel for a sidecar node

This is the load-bearing architectural call, and the rule is **per node kind**,
not uniform. An earlier revision of this document said "one panel per node,
federated by link" flatly. That is right for a shell node and wrong for a
sidecar, for a reason worth keeping rather than deleting.

**Shell node** (`kind: shell` — htpc-1): the panel runs **on** it.

- **The exec tier is inherently local.** The panel exists to be the recovery path
  when the daemon is wedged — `systemctl restart` on a hung unit. A remote panel
  cannot do that. It would be exactly useless in the one scenario the panel was
  built for.
- **It contradicts the daemon's own boundary.** The daemon is deliberately "an
  HTTP *client*, not a process supervisor" of its sidecar (`CLAUDE.md`). A panel
  that supervises a remote shell node re-introduces that coupling.

**Sidecar node** (`kind: sidecar` — desktop-2): served **remotely over HTTP** by
a Linux-built panel, via `HttpTransport`.

Both arguments above evaporate here, which is why the rule splits:

- **There is no local tier worth recovering.** `tv-shell-host` is a single HTTP
  service — no Unix socket, no CEC adapter, no compositor, no QML shell. Its
  entire recovery story is "restart the scheduled task", which the config
  manager already owns and does better than a web button.
- **The supervision objection doesn't apply.** Reading a sidecar's
  `GET /capabilities` and rendering its Steam library is being an HTTP *client* —
  precisely the relationship the daemon already has with it.
- **It removes the Windows build from the path entirely.** A panel deployed
  *onto* a sidecar would need the §2 blockers fixed **plus** a whole `PlatformOps`
  port (Task Scheduler, Event Log, winget). Served remotely, it needs neither:
  CI already builds the panel on Linux.

#### The cost, stated rather than waved away

A panel serving a sidecar **does hold that sidecar's bearer token**. The
credential-aggregator objection is reduced, not eliminated, so bound it:

- A panel may hold credentials **only for sidecar nodes it serves**. It never
  holds another **shell** node's token — those panels are peers reachable by
  link, and a link carries no credential.
- **No route proxies to a peer panel.** The node switcher renders `<a href>`, not
  a reverse proxy. A test asserting the config struct has no peer-*panel* token
  field still holds; the new `[[panel.nodes]]` entries are sidecar tokens, which are a
  different thing and should be named so they cannot be confused.
- Blast radius is therefore "every sidecar this panel serves", not "every node in
  the fleet" — and a sidecar token buys Steam launch/quit/sleep, not root on a
  shell node.

#### Open: where the remote panel process runs

Serving desktop-2 remotely raises a question the per-node rule never had to
answer — **which machine runs that panel**. Candidates: a second unit on htpc-1
bound to another port, a separate Linux host, or the cluster. The deciding factor
is which box is acceptable as the holder of desktop-2's token, and whether a
sleeping htpc-1 taking the sidecar's UI down with it is acceptable. Unresolved
here on purpose; it is a deployment decision, not a design one.

### 5. Versioning: what each crate reports, and why the panel reports nothing

**Resolved — the panel is deliberately versionless.** An earlier draft of this
section claimed "every crate is still `version = "0.1.0"`" and asked for the tag
to be injected at build time. Both halves were wrong, and the record is corrected
here.

What is actually true:

- **`daemon` and `host` carry real released versions.** Their release workflows
  (`release-input.yml`, `release-host.yml`) stamp the computed tag into
  `Cargo.toml` before `cargo build`, so `env!("CARGO_PKG_VERSION")` is accurate
  *in a release build*. `Capabilities.agent_version` is set from it at
  `daemon/src/ipc.rs:552` and `host/src/main.rs:571`.
- **`panel` and `protocol` are pinned `0.0.0` on purpose.** Neither is
  distributed as a release artifact; the panel is compiled on-box by
  `scripts/install.sh` / `/dev/build`, and `protocol` is an internal path-only
  crate. A `panel-v*` tag stream would stamp a number into a release artifact
  nobody installs, while the deployed panel kept reporting the manifest value —
  fixed in appearance, unchanged in fact.
- **The panel never reports a version at all.** `panel/` contains no
  `CARGO_PKG_VERSION` reference; the panel only ever *consumes* `agent_version`
  from the nodes it talks to. So its `0.0.0` is not a value anyone sees — there is
  no panel-side handshake for it to leak into.

So, to answer "what is deployed":

| Question | Look at |
|---|---|
| Which panel build is running? | `build-info`'s **sha + branch** — the only thing that identifies a source-built binary |
| Which daemon/host is a node running? | that node's **`agent_version`** in `Capabilities` |

**Caveat on `agent_version`, worth knowing before you trust it.** Because the
release workflows stamp `Cargo.toml` *without committing it*, any **source-built**
deploy reports the manifest's checked-in value, which is the *previous* release.
A box built from `main` can therefore advertise a **lower** version than one
running the release artifact, while actually being newer. The label answers
"which release artifact is this", not "which code is this" — for the latter, use
sha + branch.

---

## Security considerations

The current single-node posture has issues that are *tolerable at N=1 and not at
N>1*. Replicating the pattern multiplies them, so these are ordered by what must
land before a second node ships.

### Must fix before replicating

**S1 — The panel has no authentication, on a wildcard bind.**
`[panel].token_file` is parsed and deliberately unused; v1 is "LAN-only, no
auth". The code default is loopback with an explicit *"the panel has NO auth in
v1, so firewall it yourself if you widen the bind"* warning — and htpc-1's
Ansible widens it to `0.0.0.0:8091` anyway, for a cold-boot NetworkManager race.
The safe default is already being overridden in the only deployment that exists.

Implement the reserved token: a form login setting an `HttpOnly` +
`SameSite=Strict` cookie for humans, plus `Authorization: Bearer` for scripted
access, both validated against the same token file with a **constant-time**
comparison.

Scale of the exposure at the time this was written: **88 routes registered, 68 of
them `post`**, every one reachable unauthenticated on the LAN. **Fixed** — the
token, the login form and the `route_layer` gate landed; only the four documented
exemptions serve anonymously (`docs/PANEL.md` § Authentication). Capability
gating (§1) narrowed it further: 22 routes are now unconditional, 7 of them
`post`.

**S2 — The panel is a confused deputy for the daemon's token.**
The panel reads `[http].token_file` and attaches it to every bridge call. An
unauthenticated panel in front of an authenticated daemon means the daemon's auth
is currently worth nothing: anything that reaches `:8091` gets the daemon's
authenticated surface laundered for free. S1 fixes this, but note the ordering —
enabling `[http].auth_enabled` (done 2026-07-26) did not achieve what it looks
like it achieved while the panel sits in front of it.

**S3 — Adopt the daemon's own startup refusal.**
The daemon refuses to start on (non-loopback bind + dev tools + auth off) unless
`[dev].allow_insecure_lan = true`. The panel — which is *more* privileged — has
no such check. It should refuse the identical combination, reusing the same flag
so there is one opt-in to audit, not two.

**S4 — The sidecar mints a weak token when its env var is missing.**
`generate_token()` mixes `SystemTime` nanos with the pid through a splitmix
scramble and is self-described as "not cryptographically strong". If
`TV_SHELL_HOST_TOKEN` fails to resolve, `:47995` — which accepts `/launch`,
`/quit`, `/sleep` — is guarded by a secret derived from boot time and a pid, a
small search space for anyone who can observe uptime. Use a CSPRNG, or better:
**fail closed** — refuse to start on a non-loopback bind with no explicit token.
Minting a weak secret to stay up is the wrong trade, and it gets worse once every
node runs this binary.

### Design constraints for the pattern

**S5 — The panel is the most privileged surface on its node.**
It can overwrite the running build (`/dev/deploy`, `/dev/build`), reboot, suspend,
restart units, write `config.toml`, upload files, and run `sudo -n pacman -Syu`
under a NOPASSWD rule — which makes it *root-equivalent on that node*. Ship
`[panel].allow_dangerous = false` by default so a fresh node gets the read-only
and recovery surface without deploy/build/reboot until explicitly enabled. Keep
the sudoers rule scoped to the exact command (it already is:
`/usr/bin/pacman -Syu --noconfirm`) and re-verify no wildcard creeps in per node.

**S6 — Token sprawl and rotation.**
The HTTP token already lives in three places with two encodings (1Password,
Ansible vault, and Home Assistant *with* the `Bearer ` prefix). A panel token per
node multiplies that. Use one token per node per surface, all sourced from
1Password under a documented name, and write the rotation runbook — because
**neither binary has a reload path**. Rotation means a restart, and restarting the
daemon hands the CEC adapter to whatever grabs it next. Rotation here is
outage-adjacent, not a config edit.

**S7 — The MQTT command topic is the real control boundary, per node.**
Already documented in `MQTT.md`, and it becomes per-node policy under this
pattern: the published button list is *not* the boundary — anything that can
publish to `tv-shell/<device_id>/cmd/+` drives the entire intent vocabulary,
including `app:<wmClass>` launches. Every new node needs its own MQTT user with
an ACL scoped to its own `device_id`. (desktop-2 currently publishes as
`device_id = "desktop"`, a name inherited from when it was one boot of a
dual-boot box; the ACL should be checked against the name actually in use.)

**S8 — `/art/{appid}` is intentionally unauthenticated — keep its invariant.**
The justification is sound (QML's `Image.source` cannot send an `Authorization`
header, cover art isn't sensitive) and the `u32` path type prevents traversal
before any filesystem access. That typing is load-bearing, not incidental: it is
the only thing standing between an unauthenticated route and an arbitrary file
read. Pin it with a test. Note it does leak the installed-game list to anyone on
the LAN willing to enumerate appids — acceptable, but it should be a decision on
the record rather than a side effect.

**S9 — One filesystem-write resolver, reused.**
The wallpaper upload is well defended: extension allowlist, filename
sanitization, a containment re-check against the target dir, a 32 MB cap,
magic-byte sniffing, and a read-back route sharing the same resolver so it cannot
become an arbitrary file read. Under this pattern that resolver must be the
*only* write path — any node-specific upload reuses it rather than
re-implementing the checks.

---

## Suggested sequencing

1. **Security floor** — S1, S2, S3, S4. Nothing else ships first; these are the
   items that get worse with each node added.
2. **`Capabilities` in `protocol/`** + both `capabilities` endpoints, carrying a
   real version (§5).
3. **`NodeTransport` trait**, `IpcTransport` behind `cfg(unix)`, pages migrated
   off `state.ipc`. No behavior change on htpc-1 — this step should be invisible.
4. **Capability-gated routes + nav**, with the ungated-mutating-route test.
   **Landed** — `panel/src/capabilities.rs`; see §1.
5. **`HttpTransport`** + an HTTP-status shape for `TransportError` (a 401/403/404
   must collapse into neither `Command` — the dashboard would claim reachable and
   render `unwrap_or_default()` garbage — nor `Unreachable`, which would make an
   auth misconfig indistinguishable from a down node). **Landed**
   (`panel/src/http.rs`, `panel/src/transport.rs`).
6. **Serve desktop-2 remotely**: a `[[panel.nodes]]` config entry and its sidecar
   token are **landed** too (`panel/src/config.rs`). What remains is the node
   switcher that actually constructs an `HttpTransport` from a resolved entry
   and renders it — this is what actually gives desktop-2 a UI. Settle the
   "where does it run" question in §4 first.

**`PlatformOps` and a Windows panel build are no longer on this path.** They were
step 5 when the plan assumed a panel deployed onto every node; §4 removes that
requirement for sidecars. Revisit only if a *shell* node ever runs Windows, which
nothing plans today.

Steps 1–4 are worth doing even if a second node never ships: they fix a live
credential-laundering issue and delete the "reserved for a later milestone"
comment that has been load-bearing for longer than intended. **Steps 1–4 have
landed** (`#389`, `#391`, `#393`, `#395`), and both nodes now answer a
capabilities handshake in production.
