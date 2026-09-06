# gamescope kit fixture tests

Two offline suites that exercise `dev/gamescope/` — `focus.sh`, `launch.sh`,
`client.sh` and `session.sh` — against a **fake X display and fake clients**.
No gamescope, no Steam, no Moonlight, no network, no display server.

```bash
./dev/gamescope/tests/run.sh         # Moonlight + focus/tagging suite  (99 assertions)
./dev/gamescope/tests/run-steam.sh   # Steam / Steam Link / env suite   (72 assertions)
```

Each script takes an optional path to the kit under test; with no argument it
tests the kit in its own parent directory, so a clean checkout runs as-is. Both
print `passed=N failed=M` and exit non-zero if anything failed.

## How the fake display works

`bin/xprop` replays a scripted gamescope Xwayland session out of a state
directory (`$FAKE_X`): one file per window declaring the poll on which it
appears (and optionally vanishes), its `WM_NAME` / `WM_CLASS` / `_NET_WM_PID`,
and whether it logs a Gamescope WSI surface. Root-window reads of
`GAMESCOPE_FOCUSABLE_WINDOWS` advance the poll counter, so a test can say "the
stream window shows up on poll 3" and the kit's watch loop experiences exactly
that. `-set STEAM_GAME` appends to `tag.log`, which is what the assertions read.

`bin/steam` becomes a real process *family* (launcher → client →
streaming_client) so the `--family` pid-tree walk has something to walk;
`bin/moonlight`, `bin/flatpak`, `bin/curl`, `bin/qml6` and `bin/gamescope`
record their argv and environment and otherwise idle.

`bin/systemd-run` records the argv and the `--unit` name of every scope launch
(into `systemd-run.argv` and `scopes.log`) and then `exec`s the command, exactly
as the real `systemd-run --scope` does — so the pid the kit captured with `$!`
stays the app's own pid and every pid-keyed assertion downstream still holds.
It is what lets a fixture assert that each verb creates a scope whose unit name
matches gamescope's `app-steam-app%u-%d.scope` parser, with the verb's own app
id in it. Both suites also set `XDG_RUNTIME_DIR` and `DBUS_SESSION_BUS_ADDRESS`
into the scratch dir, because `gs_scope_ready` checks the environment before
calling `systemd-run` at all.

`bin/systemctl` exists so a fixture can assert whether `session.sh` **started**
`tv-shell-input.service` — the daemon that must not run in
`TV_SHELL_GS_CLIENT=moonlight` mode. Without it the real `systemctl` would talk
to the developer's live user manager, which is precisely the thing under test.
It records its argv and reports the unit inactive so the branch reaches `start`.

Everything a run writes — the fake X state, a throwaway `$HOME`, and the kit's
own client-log dir (via `TV_SHELL_GS_LOG_DIR`) — lives under one `mktemp -d`
scratch directory. Nothing is written into the checkout.

## Cleanup

An `EXIT`/`INT`/`TERM` trap removes the scratch dir and ends every process the
run started, so an interrupted run leaves nothing behind — a stray `sleep 300`
or a half-written state dir from a previous run is exactly how a fixture starts
passing for the wrong reason. The trap is idempotent: a signal fires it, then
`EXIT` fires it again and the second pass finds nothing.

Processes are ended **by pid, never by program name**. A `pkill -f moonlight`
would reach a real Moonlight, or a second fixture run on the same box. Pids come
from three places, because no one of them sees everything:

1. the run's own live descendants, walked with `pgrep -P`;
2. `$TV_SHELL_GS_TEST_PIDS`, a file each fake client appends its own pid to on
   startup — the kit backgrounds them with `nohup` inside a command
   substitution, so they are reparented to init and no tree walk finds them;
3. `track <pid>`, for the one client `S2` deliberately `setsid`s out of the tree.

For (2) to work each fake client must `exec` the process that outlives it,
rather than forking one: the registered pid has to *be* the long-lived process,
or killing it just orphans the `sleep` underneath.

The kit's own detached `focus.sh` watchers are the one thing not tracked — their
argv carries no scratch path and the fixture doesn't own their pids. They are
bounded by the `TV_SHELL_GS_WATCH_SECS` / `TV_SHELL_GS_STEAM_WATCH_SECS` values
each section sets (0.2–6s), so they exit on their own within seconds.

## What they cannot guard — read this before trusting a green run

**These fixtures cannot catch a change in gamescope's behaviour, and one of the
worst bugs the kit has had was exactly that.**

They run against a fake `xprop` replaying a scripted window list. That fake
answers however the fixture wrote it, so the suites verify *our logic* —
did the kit tag the window it should have, create the scope it should have,
refuse the launch it should have — and nothing about what the compositor on the
other side actually does with any of it.

On 2026-09-06 gamescope was pinned from 3.16.23 up to 3.16.28
(`jedwards1230/homelab-ansible#321`). Under 3.16.28 the kit's post-hoc window
tagging stopped working outright: `GAMESCOPE_FOCUSABLE_WINDOWS` came back
**empty** with Moonlight running and rendering, so there was no candidate window
to tag and nothing the shell could focus. **All 126 assertions passed that day**,
before, during and after, because a fake `xprop` cannot stop returning windows.

The rule that follows: **the live bench is the only gate on a gamescope version
bump.** A green fixture run says the kit still does what it was written to do;
only a run on the box says that is still the right thing to do. These suites
protect refactors, not upgrades.

## What they guard

`run.sh` covers scope launching (each verb's client goes into an
`app-steam-app<its own id>-<pid>.scope`; a missing session bus is a loud
refusal, never a silent unscoped launch; a refused Moonlight launch creates no
scope), pid→xid tagging as the repair path (windows sharing a pid, a pid-less
window found by class, neighbour probing with no WSI log, stale titles from a
previous instance), the Sunshine pre-flight (busy with another app → refused;
busy with the requested app → resume; unreachable or garbage serverinfo → warn
and stream anyway, never "idle"), verbatim app names including a leading space,
and `client.sh` scoping and tagging the shell by pid.

`run-steam.sh` covers class-family tagging, `--keep-existing` leaving Steam's own
`STEAM_GAME=769` alone, the detached post-launch watcher, `--watch-baselayer`,
Steam Link's not-installed path, and the **`env(1)` argument ordering** the Steam
verbs depend on: every `-u NAME` must precede any `NAME=VALUE`, or `env` takes
the `-u` as the command to run. `S7c`, `S8` and `S8c` pass those flags in both
orders; emitting them in flag order instead fails 8 assertions.
