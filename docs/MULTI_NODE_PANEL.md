# Multi-Node Panel — one UI, N nodes, capability-gated

> Status: **design, not built.** Today exactly one node runs a panel (htpc-1,
> `tv-shell-panel` on `:8091`). The gaming PC runs `tv-shell-host`, which has no
> UI at all. This document defines the pattern for giving every node with an
> agent its own panel, and the security work that must land *before* the pattern
> is replicated rather than after.

## The problem

The panel is structurally single-node today, in four separate ways:

| Coupling | Where | Consequence |
|---|---|---|
| Transport is concrete | `AppState` holds `IpcClient` + `BridgeClient` + `Recovery` by type; pages call `state.ipc.*` directly | No seam to serve a node that speaks HTTP instead of a Unix socket |
| Unix socket is unconditional | `panel/` dials `AF_UNIX` with no `cfg` | The crate **does not build on Windows** (`CONTRIBUTING.md` says so explicitly) |
| Routes are flat and ungated | ~80 routes registered unconditionally in `main.rs` | Every page assumes CEC, controllers, widgets, a QML shell — none of which a sidecar node has |
| Platform ops are Linux-only | `exec.rs` (systemd), `updates.rs` (pacman) | A Windows node has no systemctl and no `checkupdates` |

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

### 2. A transport trait replaces the concrete clients

```rust
#[async_trait]
pub trait NodeTransport: Send + Sync {
    async fn capabilities(&self) -> Result<Capabilities, TransportError>;
    async fn command(&self, cmd: &Command) -> Result<Response, TransportError>;
    fn reachability(&self) -> Reachability;
}
```

Two implementations:

- `IpcTransport` — the existing `ipc.rs`, gated `#[cfg(unix)]`.
- `HttpTransport` — bearer-auth HTTP, wrapping the sidecar's routes.

Pages call `node.command(…)`. `AppState` holds `Box<dyn NodeTransport>` plus an
optional `PlatformOps`. With `IpcTransport` behind `cfg(unix)`, the panel
compiles on Windows against `HttpTransport` alone — add Windows to the panel's
CI matrix, mirroring what `host.yml` already does for three targets.

### 3. Platform ops behind a second trait

`exec.rs` and `updates.rs` are the remaining Linux-isms.

| Op | Linux | Windows |
|---|---|---|
| Service restart | `systemctl --user restart` | Task Scheduler (the sidecar already deploys as a scheduled task) |
| Logs | `journalctl` | Event Log / the task's log file |
| Updates | `checkupdates` + `pacman -Syu` | `winget upgrade` / Windows Update |

Each is a declared capability, so a node that can't do one simply doesn't render
that page — rather than rendering a page that errors on click.

### 4. One panel *per node*, federated by link — not one central panel

This is the load-bearing architectural call, and it goes against the instinct to
build a single pane of glass.

**Deploy the same binary to every node; each panel manages only its own node.**
Peers appear in the nav as a node-switcher rendered from a config list of URLs.
They are **links, not proxies**.

Why not a central panel:

- **The exec tier is inherently local.** The panel exists to be the recovery path
  when the daemon is wedged — `systemctl restart` on a hung unit. A central panel
  cannot do that remotely. It would be exactly useless in the one scenario the
  panel was built for.
- **A central panel is a credential aggregator.** It would hold every node's
  token, turning one compromise into all of them.
- **It contradicts the daemon's own boundary.** The daemon is deliberately "an
  HTTP *client*, not a process supervisor" of its sidecar (`CLAUDE.md`). A panel
  that supervises remote nodes re-introduces exactly the coupling the daemon
  refuses.

**Invariant: no panel ever holds a peer's credential, and no route forwards to a
peer.** Worth a test that asserts the config struct has no peer-token field.

### 5. Fix the version while you're here

`Capabilities.agent_version` must carry the real release version. Today
`daemon_version` and `host_version` come from `env!("CARGO_PKG_VERSION")`, every
crate is still `version = "0.1.0"`, and no release workflow writes the tag into
`Cargo.toml` — so both the HA entities and any capability handshake would report
`0.1.0` forever while `input-v0.2.2` / `host-v0.6.0` are deployed. Inject the tag
at build time or bump `Cargo.toml` in the release flow; a version that can't
distinguish two releases is worse than absent, because it invites trust.

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
5. **`PlatformOps` trait**; Windows implementations; panel added to the Windows
   CI leg.
6. **Deploy to desktop-2**, peer switcher, per-node Ansible vars and token.

Steps 1–4 are worth doing even if a second node never ships: they fix a live
credential-laundering issue and delete the "reserved for a later milestone"
comment that has been load-bearing for longer than intended.
