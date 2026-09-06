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
| `launch` | Scoped launching: `systemd-run --user --scope` into `app-steam-app<appid>-<pid>.scope`, the argv as a testable value, and reading a scope back out of a cgroup path. Preflight is fail-closed — there is no unscoped fallback — and a launch is **confirmed** (the launcher is still alive and `/proc/<pid>/cgroup` names the scope) before it reports success |
| `baselayer` | `show`/`home` as one write plus one bounded verify, `IntentGate` (which serializes a write and its verify against other intents), and `reconcile` as the read-only recovery path |
| `config` | `~/.config/tv-shell/core.toml` — a separate file from v1's `config.toml` (below), plus the socket path and the `[[app]]` class table. All-defaults on a missing file, `deny_unknown_fields` everywhere, `validate()` before any value is used |
| `boot` | Whether a fresh session gets its first app, keeping it alive across crashes, and the two observations that stop either one stealing a live session (below) |
| `protocol` | The IPC grammar, carried over from v1 unchanged in contract (§4): newline framing, 4096-byte lines, `ok` / `unknown` / `error:<msg>` / a bare JSON document |
| `ipc` | The Unix-socket server — `LinesCodec`, one task per connection, socket bound 0600 under a tightened umask. Compositor work sits behind a `Compositor` trait so the whole request/reply surface is testable with no X server |
| `compositor` | The seam between the two: IPC verbs → the §5 X primitives |

`units/` holds the v2 session units (§4's `tv-shell-session.target` shape, taken
from the ChimeraOS `gamescope-session` files rather than written from scratch).

> **They are installable now, and still untested on hardware.**
> `scripts/install-v2.sh` builds the core, lays a v2-only prefix
> (`/opt/tv-shell-v2` by default), installs these units with their
> `@TV_SHELL_V2_PREFIX@` token substituted, and writes a
> `tv-shell-v2.desktop` session entry — a third name, since v1 owns
> `tv-shell-wayland.desktop` and the Ansible measurement prototype owns
> `tv-shell-gamescope.desktop` (the name §4 wrote before that prototype existed).
> Nothing here has been booted: read the units as a proposal that can now be
> deployed, not as a verified configuration, and note that the §11 cutover
> criteria are all still unmet.
>
> Until this commit the units hard-coded `/opt/tv-shell` — **v1's** prefix — under
> comments claiming `scripts/install.sh` rewrote it, as it does for the v1 units.
> It did not; install.sh had no reference to `core/` at all. Eight tests in
> `config.rs` (`the_committed_units_name_no_absolute_install_path` and the seven
> that run the real installer into a scratch tree) now make that class of claim
> falsifiable.

Three things are worth knowing before reading them:

- Every v2 sidecar carries a **`v2-` infix** (`tv-shell-v2-panel`, not
  `tv-shell-panel` — §11 forbids a shared unit name and v1 already owns the
  plain one).
- gamescope's primary child is `units/tv-shell-gamescope-child.sh`, which
  publishes the compositor environment from **inside** gamescope's process tree
  and then sends `READY=1`. Its `sleep infinity` tail is an explicit placeholder
  for §4's real session child, not finished work.
- **v1 exclusion is one-directional, and deliberately not `Conflicts=`.**
  `Conflicts=` is bidirectional, so the Ansible CEC watchdog restarting
  `tv-shell-input` on a bad `cec-health` reading (§9,
  jedwards1230/homelab-ansible#266) would have issued a start job that stopped
  the v2 session target — a black screen mid-game, caused by a watchdog acting on
  a misreading, which is the exact incident §9 was written about. The session
  script stops and `mask --runtime`s the v1 units instead, so a stray start fails
  loudly rather than tearing down a live session, and a runtime mask cannot
  outlive the user manager. **That is a mitigation: §9 still requires the
  watchdog to stand down at cutover, and that is unfiled on the Ansible side.**

### The rule the ExecStart is written under

The measurement kit made a flag configurable **because the answer was still
open**. A unit that hard-codes one answer without saying it is answering anything
closes a design question in silence — and that had happened four times
(`--adaptive-sync` against §13 Q11, `--hdr-enabled` and its nits against §6's
eyes-only criterion, the pinned mode). So every flag in
`tv-shell-gamescope.service`'s `ExecStart` is either config-driven as the kit
gates it, or names the question it answers and the decision that closed it
(`--expose-wayland` → §11). Two tests enforce it: one asserts the unit still
carries every flag the measured session carries, the other that no
kit-configurable flag is hard-coded.

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
- **A switch and a cold app start are two different waits.** §5's 14–19 ms is for
  windows that are already mapped; a launching app takes seconds to map its
  first, and during that time the base layer is correct and the compositor is
  fine. Verifying both against one bound made `show <id>` right after
  `launch <id>` fail on every *working* launch — which trains a caller to ignore
  the one error that must never be ignored. So there are two bounds and two
  variants: `NotObserved` (mapped, still not on screen — the compositor
  boundary misbehaving, caught inside 250 ms of the window appearing) and
  `NeverMapped` (the app never came up). Neither is ever `ok`.
- **A spawn is not a launch.** `Command::spawn` returns `Ok` the instant
  `systemd-run` is forked, so a misspelled binary, a vanished session bus or a
  `--unit=` collision all used to reply with a JSON *success* naming a dead pid
  and a scope that never existed. A launch now confirms itself before reporting:
  the launcher has not exited, and `/proc/<pid>/cgroup` names the scope we asked
  for — which is exactly the string gamescope parses. A launch that cannot be
  confirmed is an `error:`, and the reason says whether the process died or
  merely landed outside its scope (where gamescope could never focus it).
- **A launch environment belongs to the app CLASS, and one of its operations is a removal.** Measured 2026-09-06: a bare `/usr/bin/moonlight` in the v2 session inherits `WAYLAND_DISPLAY=gamescope-0`, selects native Wayland — which §6 records Moonlight 6.1.0 segfaulting on — and never maps a window, so the base layer is right and the screen is black. The core said so exactly ("app 9003 never mapped a window … the base layer was set, so this is the app failing to start"), which is the correct failure; needing a human to remember `env -u WAYLAND_DISPLAY QT_QPA_PLATFORM=xcb …` is not. `[[app]]` carries both halves, and `env_unset` is first-class because **no value substitutes for absence**: `WAYLAND_DISPLAY=""` is not unset, and pressure-vessel rewrites an empty one back to `wayland-0` (§11). `launch <appid>` with no command is the class form; an explicit command for a known id still takes the class environment, because the environment is a property of the class and not of the argv.
- **A boot launch is "this compositor has never been used", never "the core started".** The core unit is `Restart=always`, so a restart under a live game is the designed recovery path — and relaunching there would yank the screen from something being played. `boot::decide` therefore fires only when the startup reconcile shows an EMPTY base layer *and* nothing on screen; a populated list, an app on screen, or a failed read are all "in use" and the core does nothing. Two consequences that are deliberate: a restart never resurrects an app the user quit (after a return to the shell the base layer holds the shell id), and an unreadable X state fails closed, because a failed read is no evidence rather than weak evidence. It runs after the socket is listening, so a slow app never delays the control surface §9 exists to keep reachable.
- **Durability is a supervisor, and "the app exited" must stay distinguishable from "the core restarted".** A boot client that launches once is durable-once: this hardware has wedged and killed streaming clients repeatedly, and the television then stays black until somebody reboots it. So the boot app is supervised — relaunched with the prototype's measured fast-exit backoff (`dev/gamescope/client.sh`: 3 exits inside 10 s stretches the retry from 2 s to 60 s, logged at WARN with the count so a backoff is never silent). The two events are kept apart **structurally, not by checking flags in the right order**: `supervise` takes a `Supervised` token, and the only thing that constructs one is a confirmed launch by this core. An exit therefore arrives on a channel we hold *because we started the process*; a restart is a fresh `decide` against the running world. There is no path that turns an observation into a relaunch. Two further guards: a relaunch is refused outright if anything else is on screen (the user quit, started something else, and the old process only then died), and a CLEAN exit ends supervision under the default `on-failure` policy, which assumes a shell to land on so that relaunching over a deliberate quit does not fight the user. **That assumption is not true yet** — §13 Q1 is open and the gamescope child is still `exec sleep infinity`, so today a clean quit lands on an empty compositor, i.e. a black television. Until a shell exists a deployment should set `boot_relaunch = "always"`; the default stays `on-failure` because it is right for the design and because a default that changed meaning between releases would silently change behaviour for anyone upgrading across the boundary. The `UserQuit` log line names the consequence and the override, and a test asserts it does not claim the missing shell.
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

## Install

```bash
sudo ./scripts/install-v2.sh --user "$(id -un)"
```

That builds `tv-shell-core`, installs it plus the two session scripts to
`/opt/tv-shell-v2/bin/`, writes the three units into `~/.config/systemd/user/`
with `@TV_SHELL_V2_PREFIX@` substituted, writes
`/usr/share/wayland-sessions/tv-shell-v2.desktop`, and seeds
`~/.config/tv-shell/core.toml` from the example. Then select **TV Shell v2
(gamescope)** at the display manager; **TV Shell (Wayland)** is still there and
is §11's rollback. `--prefix`, `--session-dir`, `--unit-dir` and `--config-dir`
move any of those; `--no-build` reuses an existing binary. It is re-runnable and
never overwrites an existing `core.toml`.

It is a **separate script from `scripts/install.sh`, not a `--v2` flag on it**,
for the reason in its header: §11's "beside, not instead" applies to the
installer too, and a flag whose default is v1 is one forgotten argument away from
installing over the running appliance. This script names no v1 prefix, no v1 unit
and no v1 session file, and refuses a `--prefix` at or under `/opt/tv-shell`.

### Who owns the session `.desktop`

By default the installer writes
`/usr/share/wayland-sessions/tv-shell-v2.desktop`, so a **standalone** install is
selectable at the display manager with no second tool. On an **Ansible-managed**
host it is run with **`--no-session`** and Ansible owns that path — one writer,
decided rather than left to whoever ran last:

- Precedent: the gamescope prototype's `.desktop` is written by
  `homelab-ansible`'s `roles/htpc_common/tasks/gamescope-prototype.yaml`, and
  that is the session htpc-1 boots today.
- Only Ansible can produce the full `Exec=`: it renders the session env as a
  `/usr/bin/env` prefix, which is the only way to set environment for a
  greeter-launched or autologin session — there is no shell in between. The file
  the installer writes cannot carry that, so it is strictly less capable there.
- Only Ansible's toggle **removes** the entry. An installer-written file would
  survive that toggle and leave a selectable session pointing at a tree someone
  believed they had disabled.
- Host configuration on this box goes through Ansible, not hand-install.

`--no-session` **suppresses** the write and does not even create the directory —
"do not write it" and "create it and write nothing into it" are different
promises. `--session-dir` *redirects*, which is a different thing and does not
solve two writers. A test asserts the flag suppresses exactly that file and
nothing else (the units, the launcher and `core.toml` still install, or an
Ansible-managed host would end up with a session entry pointing at nothing).

**The refusal normalises first.** It used to be an exact string compare against a
trailing-slash strip, and `--prefix /opt//tv-shell` walked straight past it — the
one failure on this path that is silent and destructive rather than loud, since
as root on the appliance it installs into v1's live tree instead of erroring. So
does anything *under* the prefix. `realpath -m` collapses the duplicate slashes,
resolves `.`/`..` and symlinks, and makes a relative prefix absolute (which the
units need anyway); the guard then refuses the prefix or any descendant, and the
test tables every spelling plus one that must still be **accepted**, so the guard
cannot pass by refusing everything.

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

The X-backed integration tests are `#[ignore]`d behind `TV_SHELL_TEST_XVFB`, the
same opt-in shape as the host crate's MQTT broker tests, so `cargo test` stays
offline and needs no display:

```bash
Xvfb :99 -screen 0 1920x1080x24 &
TV_SHELL_TEST_XVFB=:99 cargo test -p tv-shell-core --test atoms_xvfb -- --ignored
```

**They share one X server and run serialised.** Four of them write ROOT-window
properties and `screen_state_assembles_from_real_server_bytes` reads whole-root
state, so run in parallel they race each other — and that is not theoretical: CI
went intermittently red with `failed to read whole buffer` from
`AtomConn::connect`, a *connection* error rather than an assertion, i.e. nine
simultaneous handshakes against one Xvfb. `connect()` therefore hands back an
`XSession` holding a process-wide lock, so a test added later is serialised
without having to know it needs to be, and CI passes `--test-threads=1` as well
to say the same thing at the call site. **Neither is tuning; do not remove
either.** The fix is deliberately not a retry or a sleep: making a
shared-mutable-state test pass by timing is the failure mode this crate's whole
test strategy is written against.

**CI runs them** (`rust.yml`'s `core` job installs `xvfb` + `x11-utils`, waits
for the server to accept a real connection via `xdpyinfo` — the socket file
appears before Xvfb is listening, so the old path check let cargo start too
early — and fails naming *Xvfb* if it never comes up, rather than letting a dead
server surface as a confusing client-side error inside a test). It did not run
them at all before, and no developer box here has Xvfb either, so those nine
tests executed in neither place — compiled everywhere, run nowhere. The same job
now also shellchecks `core/units/*.sh` and `scripts/install-v2.sh`, which
`gamescope.yml` does not cover (it lints `dev/gamescope/` only).

### Rule-defending tests, and how to check they still defend anything

Several tests in this crate exist to make a stated rule falsifiable rather than
to cover a line. The way to check one is still doing its job is to **break the
rule in the source and confirm the suite goes red**, then revert:

- Delete the `env_remove` loop from `launch::prepare_command`.
  `the_class_environment_reaches_the_spawned_command` **and**
  `wayland_display_is_absent_from_the_real_child_environment` must fail — the
  second on a real child's own environment, not on a recorded intention.
- Make `boot::decide` ignore `observed`, or treat a `None` observation as fresh.
  `a_restart_under_a_live_app_never_launches` / `an_unreadable_session_does_not_launch`
  must fail. This is the one whose regression costs a television.
- Drop the `return` from `boot::run`'s launch-failure arm.
  `a_failed_boot_launch_never_shows` must fail.
- Make `compositor::resolve_launch`'s `(None, true)` arm return an empty argv
  instead of an error. `an_unknown_id_with_no_command_is_a_clean_error` must fail.
- Make an explicit command for a KNOWN class drop the class environment.
  `an_explicit_command_for_a_known_class_keeps_its_environment` must fail.
- Delete the `boot_app` arm of `CoreConfig::validate`.
  `a_boot_app_with_no_class_is_refused` must fail.
- Delete the `on_screen` guard at the top of `boot::after_exit`, or move it below
  the policy match. `something_else_on_screen_always_wins_over_a_relaunch` and
  `the_supervisor_yields_to_another_app` must fail.
- Drop the `else { Self(0) }` reset in `FastExits::record`.
  `repeated_fast_exits_back_off_and_a_long_run_clears_them` must fail.
- Make `ExitKind::of` treat a signalled exit (`code() == None`) as `Clean`.
  `a_signalled_exit_is_a_failure_not_a_clean_one` must fail.
- Delete the `BackOff` arm of `after_exit`.
  `a_crash_loop_reaches_the_backoff_delay` must fail.
- Make `after_exit` ignore `ExitKind` under `on-failure`.
  `a_crash_relaunches_and_a_quit_does_not` must fail — **in under a second**, not
  by hanging: the runner tests deliberately carry a give-up bound so a loop that
  should stop cannot spin instead of failing.
- Turn `write_and_verify`'s `Err(SwitchError::NotObserved { .. })` into an `Ok`.
  `baselayer::tests::a_switch_that_never_takes_is_never_ok` must fail.
- Make `IntentGate::run` call `f()` without taking the lock.
  `the_intent_gate_admits_one_intent_at_a_time` must fail.
- Change `finish`'s `confirmed?` to `confirmed.unwrap_or(0)`.
  `an_unconfirmed_launch_never_becomes_a_launched_payload` must fail.
- Make `launch_reply`'s `None` arm spawn anyway.
  `a_launch_without_a_verified_scope_environment_is_refused` must fail.
- Hard-code the mode back into the gamescope unit's `ExecStart`.
  `the_display_keys_reach_the_unit_that_claims_to_read_them` must fail.
- Put `/opt/tv-shell/bin/tv-shell-core` back in the core unit's `ExecStart`.
  `the_committed_units_name_no_absolute_install_path` must fail (and so must the
  three install tests, because the installer refuses to write it).
- Replace `subst_prefix` in `scripts/install-v2.sh` with a plain copy.
  `the_installed_units_carry_no_token_and_no_v1_path` must fail.
- Set the installer's `SESSION_FILE` to `tv-shell-gamescope.desktop`.
  `the_v2_session_entry_collides_with_neither_v1_nor_the_prototype` must fail.
- Add `tv-shell-panel.service` to the installer's `UNITS`.
  `the_v2_unit_names_collide_with_none_of_v1s` must fail.
- Widen the installer's prefix guard back to an exact string compare (drop the
  `realpath -m` normalisation and the `"$V1_PREFIX_CANON"/*` arm).
  `the_installer_refuses_v1s_prefix_however_it_is_spelled` must fail — on
  `/opt//tv-shell` first, which is the spelling that actually slipped through.
- Shift the `sed` range behind the installer's `--help` by one line.
  `the_installers_help_prints_the_flags_and_no_code` must fail.
- Make the installer's `--no-session` a no-op (parse it and drop it).
  `no_session_suppresses_the_session_file_and_installs_everything_else` must fail.

That last one is the shape worth noticing: the config's consumer test used to
check only that a key was *classified*, so a key labelled "read by the unit's
ExecStart" passed while the unit read nothing. A named consumer has to exist.

## Not yet here

Each of these is a follow-up, and none of it is implemented in this crate today:

- **§9's stall detection and short-session rollback.** The boot supervisor keeps
  the app alive; it does not watch FRAMES. A client that is running but painting
  nothing is still invisible to everything here, and `[supervisor]`'s keys remain
  unconsumed.
- **§12's other app-class fields**: an id strategy (scope / pid / class), an
  input contract (`gamepad` / `keyboard`) and an HDR expectation. `[[app]]`
  models the id, the command and the environment only — the id strategy is fixed
  at "scope, tag as repair" and is not selectable, §7's input layer does not
  exist, and the HDR settle key is itself unconsumed. Three more keys whose
  stated consumer does not exist is the #416 class; they land with their readers.
- uinput / input presenters and the pad grab (§7)
- CEC — which leaves the core entirely in v2 and becomes an observer sidecar (§8)
- the QML shell (§13 Q1 is still open on its runtime) and any panel changes
- the HTTP bridge, MCP server, MQTT publisher and `/metrics` (§4 carries their
  contracts over; the code has not moved)
- the forced-paint heartbeat and the rest of the supervisor (§9) — the
  `[supervisor]` config keys exist so the file the units ship against is
  complete, but nothing acts on them yet
- **§9's crash-loop rollback.** No short-session tracker, no `ExecStartPre`/
  `ExecStopPost`, no counter, no deployment hook to select the v1 session.
  `[supervisor].restart_threshold` and `restart_window_secs` are its
  configuration and nothing reads them. The gamescope unit used to carry
  `StartLimitIntervalSec`/`StartLimitBurst` with a comment claiming this
  protection; the limiter was inert (`Restart=no` means there is never a second
  attempt, and the session script's `reset-failed` clears the counter on every
  relogin) and has been removed rather than left as a comment that stops the next
  person looking. **Rollback is manual today**: select the v1 session at the
  display manager.
- **§5's transient-unmap hysteresis.** `GAMESCOPECTRL_BASELAYER_WINDOW` is read
  and never written, and nothing holds the base layer across a known transition
  (Moonlight main → stream window, a browser navigation) or applies hysteresis
  before treating a fallback to the shell as an exit.
- ~~**Installation.**~~ Done: `scripts/install-v2.sh` + `config/tv-shell-v2.desktop`
  (see the box above). What is still missing on that path is a **release
  stream** — §11 gives the core its own `core-v*` tags and its own Ansible pin,
  and neither exists, so a deploy today is "run the installer from a checkout".
- **A recovery surface.** §9 makes the panel the thing you reach for when the
  session is wedged. `tv-shell-v2-panel.service` does not exist, and when it does
  it belongs to `default.target` — not to the session target, where it would die
  with the thing it exists to recover.
- per-app Xwayland server creation (`GAMESCOPE_CREATE_XWAYLAND_SERVER`, §5)
