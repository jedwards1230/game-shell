# tv-shell-core

The v2 core: gamescope base-layer policy, scope launching, and the screen-state
read that replaces v1's Hyprland queries. Design of record is
[`docs/V2_DESIGN.md`](../docs/V2_DESIGN.md); this file is the crate's own map and
its relationship to `daemon/`.

## Modules

| Module | Owns |
|---|---|
| `atoms` | The typed X root-atom layer — gamescope's published state (§5). The **only** place in the crate that speaks X: everything above it sees typed values, never `u32` blobs and never atom names. A missing atom is `Ok(None)`, never an error; every property is a 32-bit id array (`CARDINAL` or `WINDOW` — the width is the invariant, the per-atom type is not measured yet) and an unexpected shape or a truncated reply is a typed error, not a coerced value; names are interned once at connect so a rename upstream fails at startup, not at the first switch |
| `screen` | `ScreenState` — one snapshot of what is on screen, replacing `hypr-active`/`hypr-clients`/`hypr-monitors`. Read in one round trip; `_APP` is not exposed as an app id at all (below) |
| `launch` | Scoped launching: `systemd-run --user --scope` into `app-steam-app<appid>-<pid>.scope`, the argv as a testable value, and reading a scope back out of a cgroup path. Preflight is fail-closed — there is no unscoped fallback |
| `baselayer` | `show`/`home` as one write plus one bounded verify, and `reconcile` as the read-only recovery path |
| `config` | `~/.config/tv-shell/core.toml` — a separate file from v1's `config.toml` (below), plus the socket path. All-defaults on a missing file, `deny_unknown_fields` everywhere, `validate()` before any value is used |
| `protocol` | The IPC grammar, carried over from v1 unchanged in contract (§4): newline framing, 4096-byte lines, `ok` / `unknown` / `error:<msg>` / a bare JSON document |
| `ipc` | The Unix-socket server — `LinesCodec`, one task per connection, socket bound 0600 under a tightened umask. Compositor work sits behind a `Compositor` trait so the whole request/reply surface is testable with no X server |
| `compositor` | The seam between the two: IPC verbs → the §5 X primitives |

`units/` holds the v2 session units (§4's `tv-shell-session.target` shape, taken
from the ChimeraOS `gamescope-session` files rather than written from scratch).
They are **untested on hardware** — nothing has booted this session yet, and the
cutover criteria in §11 are all unmet. Two things about them are worth knowing
before reading: every v2 sidecar carries a **`v2-` infix** (`tv-shell-v2-panel`,
not `tv-shell-panel` — §11 forbids a shared unit name and v1 already owns the
plain one), and gamescope's primary child is
`units/tv-shell-gamescope-child.sh`, which publishes the compositor environment
from **inside** gamescope's process tree and then sends `READY=1`. Its
`sleep infinity` tail is an explicit placeholder for §4's real session child,
not finished work.

## Why a new crate, not an evolution of `daemon/`

§11's rule is "beside, not instead, at every shared layer", and this is the layer
where it bites hardest. **v1's `DaemonConfig` root carries
`#[serde(deny_unknown_fields)]`**, so putting a `[display]` or `[session]` table
into `config.toml` would make the v1 daemon abort at startup — and the symptom
would read as "v1 is broken", not "someone added a v2 table". v1 must keep
booting on the couch for as long as v2 is being built beside it (§1 goal 8:
both are selectable sessions on the same install and neither can break the
other), so v2 gets its own config file (`core.toml`), its own socket
(`tv-shell-core.sock`, never `tv-shell-input.sock`), its own units and its own
install prefix. Only one session runs at a time, so nothing collides.

`daemon/`, `host/`, `protocol/` and `panel/` are untouched and still build. This
crate depends on none of them — not even `tv-shell-protocol`, whose
`brand::config_dir` carries a legacy `game-shell` read-fallback that a new file
has no use for.

It is a lib plus a thin bin for the same reason the daemon is: `pub` items in a
library are public API and are never "dead", so `clippy -D warnings` stays clean
even where a module is not yet wired into `main`.

## The §5 rules this code enforces

These are the invariants the crate exists to make unbreakable, each one a v1
failure inverted:

- **The base window is `GAMESCOPE_FOCUSED_WINDOW`, never `_APP`.** `_APP` was
  measured to read empty while an input-focus overlay (a drawer, the QAM) is
  mapped, so a rule keyed on it decides "nothing is running" exactly when the
  user has opened the menu over a live game — v1's "the compositor answers about
  a different object" class. `ScreenState` therefore does not expose `_APP` as an
  `AppId` at all; it survives only as `focused_app_atom_diagnostic()`, for logs
  and metrics. The wrong choice is not reachable, because the wrong one is not
  `AppId`-shaped in the public API.
- **Scope first, tag as repair, never by name.** gamescope's only cgroup parser
  is `sscanf("app-steam-app%u-%d.scope")`, evaluated at window creation from the
  `XResQueryClientIds` pid — an upstream contract, not a prefix we may rename. A
  name-keyed tag lands on a window about to die (Moonlight's stream window
  replaces its main one), so `STEAM_GAME` is written **only** where the scope did
  not resolve, and `AppIdSource` records which happened so the field assertions
  can see a repair as a repair.
- **One write, then verify; a mismatch is never `ok`.** A switch is one
  `GAMESCOPECTRL_BASELAYER_APPID` write followed by a read of the focused
  window's app id inside a bounded window (measured 14–19 ms over 20 switches;
  the 250 ms default is the failure bound, not the expected time). v1 returned
  `ok` for a dropped launch, an escape that could not leave fullscreen, an
  unparked window, a stopped heartbeat and a compositor wedged for nine days.
  Here the only route to `Ok` is the compositor having published the intended id.
  The verify is **polled, not event-driven**: v1's residual defect was an
  attached listener that processed nothing, and a poll cannot fail that way.
- **Steam owns the base-layer atom, and the core reconciles after it.** Measured
  2026-09-05: while the Steam client runs it rewrites
  `GAMESCOPECTRL_BASELAYER_APPID` on every stream start and stop and drops our id
  from `GAMESCOPE_FOCUSABLE_APPS` entirely. That is an adversary, not drift, so
  this crate never busy-loops to hold the atom. `reconcile` reads the list back
  as the core's last intent — which is also how a restarted core recovers its
  state with nothing on disk — and deliberately does not re-assert. A write
  happens only when the core has an intent of its own to express. In particular
  the core never writes "home" on boot; that would yank a live game.
- **The shell's app id is private and may not be 769.** Under `--steam`, 769 is
  the Steam client's own id (`window_is_steam`: forced fullscreen sizing,
  `focus=steam` in the stats pipe) and is reserved for it. `CoreConfig::validate`
  refuses to start on `shell_app_id = 769` rather than letting the shell inherit
  that path and collide with the real client.

## Build, test & lint

```bash
cargo fmt --check -p tv-shell-core
cargo clippy -p tv-shell-core --all-targets -- -D warnings
cargo build --release -p tv-shell-core
cargo test -p tv-shell-core
```

Pure Rust — `x11rb` is used with `default-features = false` and without
`allow-unsafe-code`, so a build needs no libxcb/libX11 and no system C libraries
at all.

The X-backed integration test is `#[ignore]`d behind `TV_SHELL_TEST_XVFB`, the
same opt-in shape as the host crate's MQTT broker tests, so `cargo test` stays
offline and needs no display:

```bash
Xvfb :99 -screen 0 1920x1080x24 &
TV_SHELL_TEST_XVFB=:99 cargo test -p tv-shell-core --test atoms_xvfb -- --ignored
```

## Not yet here

Each of these is a follow-up, and none of it is implemented in this crate today:

- uinput / input presenters and the pad grab (§7)
- CEC — which leaves the core entirely in v2 and becomes an observer sidecar (§8)
- the QML shell (§13 Q1 is still open on its runtime) and any panel changes
- the HTTP bridge, MCP server, MQTT publisher and `/metrics` (§4 carries their
  contracts over; the code has not moved)
- the forced-paint heartbeat and the rest of the supervisor (§9) — the
  `[supervisor]` config keys exist so the file the units ship against is
  complete, but nothing acts on them yet
- per-app Xwayland server creation (`GAMESCOPE_CREATE_XWAYLAND_SERVER`, §5)
