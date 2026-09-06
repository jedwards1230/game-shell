# gamescope prototype session

A one-week measurement, not a product. It boots [gamescope](https://github.com/ValveSoftware/gamescope)
on the DRM backend in place of Hyprland, with the tv-shell input daemon beside it and a
deliberately tiny prototype shell as its primary child, so that the questions below can be
answered with numbers before any v2 architecture is written around gamescope.

Nothing in this directory is used by the real session. It is opt-in, selected at the
display manager, and removable by deleting the directory and the session entry.

## Why gamescope is on the table

The kiosk invariant ("exactly one app fills the screen") has been re-fixed nine times on
Hyprland because the shell asks the compositor questions it answers about a different
object, and a window can decline focus. gamescope's focus is decided by its own policy and
cannot be refused, it has an interactive overlay plane designed for a QAM over a running
game, it never touches gamepads, and it runs headless for CI. What it has *not* proven on
this hardware is 10-bit HDR output at 4K120 and the cost of Vulkan compositing when the
kernel lacks `CONFIG_AMD_PRIVATE_COLOR`. Those two unknowns are cheap to measure and
decisive, hence this kit.

## Pass/fail criteria

| # | Question | How | Pass |
|---|---|---|---|
| 1 | Output is 10-bit HDR at 4K120 | `sudo measure.sh` | `output bit depth` PASS, `colorspace` BT2020, `HDR_OUTPUT_METADATA` PASS, `mode` PASS |
| 2 | VRR engages | `sudo measure.sh` | `VRR` PASS |
| 3 | A lone HDR stream is not double-composited | stats FIFO + eyes | frame time steady at the refresh with Moonlight HDR running; no judder |
| 4 | SDR black floor is right | eyes | the black end of the strip at the top of the prototype shell is OLED-black under `--hdr-enabled` |
| 5 | Focus is unrefusable and instant | `launch.sh xmessage`, `focus.sh app 9001`, repeat 20x | every switch lands, nothing tiles, no window "declines" |
| 6 | Overlay over a running app | `launch.sh overlay` | panel visible above the app, app keeps animating, keys go to the panel |
| 7 | Gamepad reaches the shell | press D-pad | `last key` counter climbs in the prototype shell (daemon uinput path works under gamescope) |
| 8 | Moonlight HDR via the WSI layer | `sudo measure.sh` (`HDR to clients`), then `launch.sh moonlight` | `GAMESCOPE_HDR_OUTPUT_FEEDBACK` is 1; stream is HDR on the TV, no fallback to SDR tonemap |
| 9 | Qt on gamescope is usable | eyes | prototype shell renders at full size, no decorations, keyboard focus YES |
| 10 | Steam Remote Play under gamescope | launch the local Steam client (`launch.sh steam --watch-baselayer`, Big Picture; then the standalone Steam Link client, `launch.sh steamlink`), tag it, base-layer it, start a stream from the streaming host on the pad; `focus.sh list` + `steam-tag.log` + `steam-baselayer.log` + stats FIFO + eyes | stream shown (its window tagged, `GAMESCOPE_FOCUSED_APP` on it); HDR on the TV, or SDR recorded as such (see the risks below); `fps=120` held; `GAMESCOPECTRL_BASELAYER_APPID` still reads what `focus.sh` wrote, or Steam's own write wins once and stays put, never flapping (no base-layer fight with a Steam client that writes that atom itself under `--steam`; `steam-baselayer.log` is the record); the pad reaches the game. Gates the supported flavour, see `docs/V2_DESIGN.md` §12 |

Decision rule agreed 2026-09-04: if 1 or 3 fails, gamescope is out for v2 and the
Hyprland-plugin path is next; Smithay stays the v3 target on HDR grounds.

**Streaming clients are peers.** Moonlight (criteria 3 and 8) and Steam Remote Play
(criterion 10) are two first-class, independently testable clients of the prototype, not
one path with a fallback. Each has its own `launch.sh` verb, app id (9003 / 9004, Steam Link
9005), log, and tagging rule, and a change to one must leave the other runnable: a Steam
finding never retires the Moonlight test, and vice versa.

Criterion 10 carries two known risks, both from the SteamOS 3.9 session
(`~/tv-shell-v2-research/steamos-39-gamescope-shell.md`, §focus and §Remote Play):
the SteamOS session exports `GAMESCOPE_DISPLAY_DISABLED=1` for `streaming_client` ("disable
gamescope path in streaming_client until buffer frozen issues are understood better"), so
Valve itself presents the Remote Play client through plain Xwayland rather than the
gamescope-direct path, and gamescope issue #2196 (Remote Play under `--steam` flickers,
input routes unpredictably, 3.16.23) is still open; and the full Steam client writes
`GAMESCOPECTRL_BASELAYER_APPID` itself when it starts or ends a stream (that is how SteamOS
switches between Steam and a game), so it may overwrite the kit's own list; a first
unscripted run of the full client under this prototype (2026-09-05) saw exactly that: Steam
detected gamescope, tagged its own windows 769, and rewrote `GAMESCOPECTRL_BASELAYER_APPID`
within about ten seconds. The measured run below settles it — Steam wins the base layer
outright and rewrites it per stream start and stop. That is why the kit's Steam list
carries 769 and never re-tags a window Steam tagged itself, and why `--watch-baselayer`
exists. `launch.sh steam`
sets `GAMESCOPE_DISPLAY_DISABLED=1` and `GAMESCOPE_ZENITY_DISABLE=1` the way SteamOS does
(the first keeps `streaming_client` on the X11 + layer path, the one HDR ships on; the
second turns a layer failure into a loud exit instead of an invisible dialog).

### Criterion 10, measured (2026-09-05, gamescope 3.16.23, this box)

Full Steam client under the prototype, `launch.sh steam --watch-baselayer` (which runs
`env -u WAYLAND_DISPLAY GAMESCOPE_DISPLAY_DISABLED=1 GAMESCOPE_ZENITY_DISABLE=1 steam
-bigpicture`), two real Remote Play streams started by hand on the pad from the streaming
host, one after the other. The session was still running **with `--expose-wayland` on**;
the input daemon and the CEC watchdog were stopped so the pad was free.

**The WSI hook fix works, and the per-app unset alone is enough.** Steam's
`logs/console-linux.txt` at both stream starts:

```
[Gamescope WSI] Executable name: streaming_client
[Gamescope WSI] Creating Gamescope surface: xid: 0x2e0002f
[Gamescope WSI] Made gamescope surface for xid: 0x2e0002f
[Gamescope WSI] Surface state:
  server hdr output enabled:     true
  hdr formats exposed to client: true
```

That is exactly the success signature predicted below, reached **without a session
restart**: `--expose-wayland` was still set, and unsetting `WAYLAND_DISPLAY` for Steam
alone was sufficient. `TV_SHELL_GS_EXPOSE_WAYLAND=0` is not required for Steam.

**Steam still requests SDR, and that is now a client-side gate, not a gap in this stack.**
The very next line, both times:

```
Creating swapchain ... format: VK_FORMAT_B8G8R8A8_UNORM - colorspace: VK_COLOR_SPACE_SRGB_NONLINEAR_KHR
```

8-bit sRGB, immediately after the layer told the client `hdr formats exposed to client:
true`. Previously one could argue HDR was never offered because the layer was not hooking;
it is now offered explicitly and **declined by the client**. Steam Remote Play is SDR on
Linux by Steam's own gate — not this configuration, not gamescope, not the pin. **Moonlight
remains the HDR path** (criterion 8). The display itself stayed HDR + VRR throughout
(`GAMESCOPE_HDR_OUTPUT_FEEDBACK=1`, `GAMESCOPE_VRR_FEEDBACK=1`), so gamescope was
tonemapping Steam's SDR into an HDR output.

**120 fps held with a live Remote Play stream as the base layer**: the stats FIFO read
`fps=120.000000` / `119.952019` / `120.048019` repeatedly with `focus=3321460`.

**Tagging: `--keep-existing` is the right rule, and Steam tags the stream window with the
*game's* app id, not its own.** From `steam-tag.log`:

```
known 0x1c00035 "Steam Big Picture Mode" (has STEAM_GAME=769, left alone)
known 'Steam' (already STEAM_GAME=9004)
known 0x2e0002f "Streaming Client" (has STEAM_GAME=252950, left alone)
```

252950 is the streamed game's app id. The kit deferred to Steam's own tag on all three.

**The base-layer fight is confirmed, and it is stronger than "may fight": Steam wins
outright.** The kit set `9004,769,9001`; Steam overwrote it within seconds and kept
rewriting it on every stream start and stop:

```
15:54:43 baselayer=[413091, 769]              focused=[769]
15:56:28 baselayer=[413091, 252950, 769]      focused=[252950]
15:57:12 baselayer=[413091, 769]              focused=[769]
15:57:24 baselayer=[413091, 3321460, 769]     focused=[3321460]
```

The kit's own 9004 was dropped from `GAMESCOPE_FOCUSABLE_APPS` entirely (it read
`3321460, 769, 9001`). So a shell must **reconcile after Steam's writes**, not expect to
hold that list. `413091` appears persistently as the first element and we do not know what
it is; it is recorded here unexplained rather than guessed at.

`launch.sh steamlink` exits 2 with its "not installed" message — Steam Link is genuinely
not installed on the box, so only the full-client path is exercised. Steam Link's own HDR
behaviour is unmeasured.

**Still unverified:** whether the pad reaches the streamed game. That needs a person at
the TV and is not claimed either way.

**The old failure signature, kept so a regression is recognisable.** Before the
`WAYLAND_DISPLAY` fix, the same run rendered **only with the WSI layer disabled**, as SDR
4K120 HEVC NV12 BT.709 through plain Xwayland; with the layer enabled the client showed a
black screen and its log had only the layer's unconditional `Forcing on
VK_EXT_swapchain_maintenance1` line, no `Creating Gamescope surface`, then the
"non-Gamescope swapchain" dialog. That was a hook miss caused by the `WAYLAND_DISPLAY`
mismatch pressure-vessel creates under an `--expose-wayland` session (Known gaps, below).
If those lines come back, the unset was lost, not gamescope regressed.

## Install and select

Deployment is site configuration (an Ansible toggle in the private homelab repo installs
`gamescope` and writes `/usr/share/wayland-sessions/tv-shell-gamescope.desktop` whose
`Exec` is `session.sh` here). Without that tooling, the manual equivalent is:

```sh
# as root, on the box
pacman -S gamescope xorg-xprop
printf '%s\n' '[Desktop Entry]' 'Type=Application' 'Name=TV Shell (gamescope prototype)' \
  'Exec=/opt/tv-shell/dev/gamescope/session.sh' 'DesktopNames=gamescope' \
  > /usr/share/wayland-sessions/tv-shell-gamescope.desktop
```

Then pick "TV Shell (gamescope prototype)" at the greeter, or, on an autologin kiosk that
never shows one, point the display manager's autologin session at `tv-shell-gamescope`
for the week. The default boot session is untouched; log out (or set it back) to return to it.

## Moonlight as the session's client (`TV_SHELL_GS_CLIENT=moonlight`)

The session's primary child is selectable:

| `TV_SHELL_GS_CLIENT` | Primary child |
|---|---|
| `proto` (default) | `proto-shell.qml`, the measurement rig. Launches nothing on purpose. |
| `moonlight` | Moonlight-qt's own GUI, tagged `STEAM_GAME=9003` and set as the base layer. |

The default stays `proto` so running the kit as a bench behaves exactly as it did.
`moonlight` turns the same session into an appliance: the box boots straight into a
pad-navigable grid of the streaming host's apps, in HDR, with no keyboard.

```sh
TV_SHELL_GS_CLIENT=moonlight session.sh
# or in the session .desktop:  Exec=/usr/bin/env TV_SHELL_GS_CLIENT=moonlight /opt/tv-shell/dev/gamescope/session.sh
```

`TV_SHELL_GS_MOONLIGHT_ARGS` (word-split) is appended to the launch, so
`stream <host> '<app>'` boots straight into a stream instead of the grid; empty (the
default) is the grid. `TV_SHELL_GS_MOONLIGHT` overrides the binary.

**This is an interim step, not the v2 shell.** It gives pad-navigable streaming on boot
and HDR, and nothing else: no Plex, no web apps, no settings, no local app launcher, no
shell of ours — Moonlight's UI is the whole UI. It exists because the v2 core does not
exist yet, and it is the smallest thing that makes the gamescope session usable from the
couch in the meantime.

**Do not "helpfully" re-enable the input daemon in this mode.** `session.sh` defaults
`TV_SHELL_GS_DAEMON=0` when the client is `moonlight`, and that default is the feature.
Under gamescope the daemon never yields the gamepad: its only focus source is Hyprland,
which is not running here, so it keeps its exclusive evdev grab (`EVIOCGRAB`) forever and
goes on translating the pad into arrow keys, and on the pinned build it presents no
virtual pad at all. With the daemon up, an app under gamescope gets **D-pad only, no face
buttons** — you can move around Moonlight's grid and never start anything. Moonlight
reads the pad directly and needs no daemon, so the measured fix is to not run one. An
explicit `TV_SHELL_GS_DAEMON=1` is still honoured (someone measuring the grab itself needs
it) and logs a `WARN` saying the pad will be D-pad only, so the symptom is never a mystery.

The consequence of dropping the daemon is that this mode has **no intent surface**: no
Super-key escape, no nav drawer, no `intent`-driven automation. Quitting Moonlight returns
you to Moonlight — `client.sh` relaunches its primary child, which for a streaming
appliance is the right behaviour (quitting the client puts you back on the grid, not on a
black screen). A Moonlight that cannot start at all hits the same fast-exit backoff the
prototype shell does — three exits inside 10 s stretches the retry to 60 s — so a broken
install cannot hot-spin.

This mode used to run a **day-long background window watch**, because Moonlight's stream
window is a *second* X11 window of the same pid that only exists once a stream starts —
whenever someone picks a game, minutes after launch — and a one-shot tag would catch the
grid and stop, leaving the stream window unselectable. Scope launching retires that
whole problem: the stream window belongs to the same process, in the same
`app-steam-app9003-<pid>.scope`, so gamescope resolves it with no tag and nothing to
arrive late. What runs now is one bounded repair pass over the first window, capped by
`TV_SHELL_GS_TAG_TIMEOUT` (30 s).

## Files

| File | Role |
|---|---|
| `session.sh` | The session. Starts `tv-shell-input.service` (**not** in `TV_SHELL_GS_CLIENT=moonlight` mode — see above), creates the stats FIFO, execs gamescope with `--steam --expose-wayland --keep-alive -W/-H/-r --hdr-enabled --adaptive-sync`, logs to journal tag `tv-shell-gamescope` |
| `client.sh` | gamescope's primary child. Runs `proto-shell.qml` on X11 with a Qt 6 runtime inside `app-steam-app9001-<pid>.scope` — or, with `TV_SHELL_GS_CLIENT=moonlight`, Moonlight inside `app-steam-app9003-<pid>.scope`. Either way it sets the base layer before launching, makes one bounded `STEAM_GAME` repair pass, writes `/tmp/tv-shell-gamescope.env` for the SSH-side tools, and relaunches the client on exit (with backoff once it crash-loops) |
| `lib.sh` | Sourced by `client.sh`, `focus.sh` and `launch.sh`: `gs_scope_ready`/`gs_scope_run`/`gs_scope_check`, the scope launcher gamescope identifies apps by (above), the Qt 6 `qml` resolver (`qml6`, `/usr/lib/qt6/bin/qml`, `/usr/lib64/qt6/bin/qml`, then a `qml` whose `--version` says 6; `TV_SHELL_GS_QML` overrides), `gs_tag_pid`, the tag-every-window-of-a-pid watcher (below; `--family` follows the pid tree, `--class` repeats, `--keep-existing` never overwrites a tag the client set itself), and `gs_watch_baselayer`, the base-layer atom logger, and `gs_moonlight_x11_env`, the one definition of the xcb/SDL/WSI environment Moonlight is launched with (shared by `launch.sh moonlight` and `client.sh`'s moonlight mode so they cannot drift) |
| `proto-shell.qml` | The prototype shell: shows window size, keyboard focus, last key, a moving dot, and a black-to-white strip |
| `proto-overlay.qml` | Overlay test client, semi-transparent side panel |
| `focus.sh` | `list` / `tag` / `tag-pid` / `watch-baselayer` / `app` / `window` / `clear` over gamescope's root X11 atoms. `tag-pid <pid> <id>` tags every window of a pid as it appears (`--timeout`, `--class`, `--family`, `--keep-existing`, `--log`, `--name`, `--expect`, `--done-name`); `watch-baselayer [secs]` logs every change of `GAMESCOPECTRL_BASELAYER_APPID` / `GAMESCOPE_FOCUSED_APP` with a timestamp |
| `launch.sh` | Every app verb launches inside its own `app-steam-app<id>-<pid>.scope`; the tagging each verb still does is repair. `overlay` (the one unscoped verb — an overlay is not an app) / `x11` / `apps <host>` / `moonlight [--quit]` (X11, app 9003; `--wayland` for the xdg-shell experiment) / `steam [--gamepadui] [--watch-baselayer]` (the full Steam client, its whole process family tagged `STEAM_GAME=9004` as windows appear, for 10 min in a detached watcher) / `steamlink` (the standalone Steam Link client, `STEAM_GAME=9005`) / `xmessage` into the running session from SSH |
| `measure.sh` | Reads DRM connector properties, debugfs bit depth, the active mode, gamescope's own info (`gamescopectl`, `backend_info`, `help`) and the root feedback atoms, and prints verdicts. Every DRM read is scoped to one connector (the first connected + enabled one, or `TV_SHELL_GS_CONNECTOR=card1-HDMI-A-1` to choose on a two-output box) and to the CRTC driving it. Under `sudo` the gamescope-side reads run as the session user |
| `tests/` | Offline fixture suites for the kit's shell logic — a fake X display (`tests/bin/xprop`) plus fake `steam`/`moonlight`/`flatpak`/`curl`/`qml6`/`gamescope`/`systemctl`/`systemd-run`. `tests/run.sh` (scope launching + Moonlight + tagging + client selection, 99 assertions) and `tests/run-steam.sh` (Steam/Steam Link/env, 72 assertions) run on any Linux box with no hardware and no network; CI runs both. **They cannot catch a change in gamescope's behaviour** — all 126 passed on the day a point release broke the kit completely; the live bench is the only gate on a version bump. See `tests/README.md` |

## Running a measurement

From another machine, with the prototype session up on the TV:

```sh
ssh box 'sudo /opt/tv-shell/dev/gamescope/measure.sh'          # criteria 1, 2, 8 (signal)
ssh box '/opt/tv-shell/dev/gamescope/launch.sh xmessage hi'     # criterion 5
ssh box '/opt/tv-shell/dev/gamescope/focus.sh app 9001'         # back to the shell
ssh box '/opt/tv-shell/dev/gamescope/launch.sh overlay'         # criterion 6
ssh box '/opt/tv-shell/dev/gamescope/launch.sh apps <host>'     # what the streaming host runs + exact app names
ssh box "/opt/tv-shell/dev/gamescope/launch.sh moonlight stream <host> ' Steam Big Picture' \
    --resolution 3840x2160 --fps 120 --hdr --display-mode fullscreen"   # criteria 3, 8
ssh box 'cat /tmp/tv-shell-gamescope-stats'                     # criterion 3 (a FIFO, see below)
ssh box '/opt/tv-shell/dev/gamescope/launch.sh steam --watch-baselayer'   # criterion 10: then start a
                                                                #   Remote Play stream on the pad
ssh box 'tail -f /tmp/tv-shell-gamescope-clients/steam-tag.log'          #   windows tagged as they appear
ssh box 'tail -f /tmp/tv-shell-gamescope-clients/steam-baselayer.log'    #   who writes the base layer
ssh box '/opt/tv-shell/dev/gamescope/launch.sh steamlink'       # criterion 10 with the standalone client
```

First check the journal (`journalctl -t tv-shell-gamescope -b`) for the line
`qml runtime: ... (6.x.y)` from `client.sh`. A `FATAL: no Qt 6 qml runtime found` line
lists what was tried; until it is fixed gamescope presents no frames, the TV is black, and
every DRM verdict from `measure.sh` is an artifact of "no client ever rendered".

`measure.sh` needs `sudo` for the debugfs read, and under `sudo` runs the gamescope-side
reads (`gamescopectl`, `xprop`) as the session user itself. Its `HDR to clients` verdict
is criterion 8's signal: the WSI layer offers HDR swapchains only while the root atom
`GAMESCOPE_HDR_OUTPUT_FEEDBACK` is 1. On 3.16.x that atom is simply
`(EDID says HDR10) && hdr_enabled`, written before any client presents, and the connector
goes to PQ/BT2020 in the same loop iteration, so atom=1 and connector-in-HDR are one boolean.

**Rule: never run `gamescopectl <convar>` without a value.** On gamescope 3.16.x and master
a bare `gamescopectl hdr_enabled` is not a read, it **resets the convar to false** (the
server always passes two arguments and an empty bool parses as false), turning HDR off
for the whole session with no log line. The same holds for every convar (`vrr_enabled`,
`composite_force`, ...). Commands are fine (`gamescopectl` bare, `help`, `backend_info`,
which is all the kit runs). Read convar state from the root atoms instead
(`xprop -root GAMESCOPE_DISPLAY_HDR_ENABLED GAMESCOPE_HDR_OUTPUT_FEEDBACK GAMESCOPE_DISPLAY_SUPPORTS_HDR`)
and set one only with an explicit value. Recovery after an accidental reset:
`gamescopectl hdr_enabled 1`, which `measure.sh` prints whenever `HDR to clients` is FAIL
with `GAMESCOPE_DISPLAY_SUPPORTS_HDR=1`.

The stats path is a **FIFO**, created by `session.sh` and removed with the session.
gamescope only `open()`s an existing one (retrying every 10 s), so it attaches within
10 s of a reader appearing. One reader at a time: `cat` or `tail -f` it, and stop that
reader before starting another. Lines are `fps=<float>` and `focus=<appid>` at about 1 Hz.
`measure.sh` samples it for 4 s, which takes the stream over from any other reader for
that window.

## How an app is identified: its cgroup scope

**Every app the kit launches runs inside its own systemd scope,
`app-steam-app<id>-<pid>.scope`, and that is how gamescope knows which app it
is.** gamescope's only cgroup parser is
`sscanf(cgroup, "app-steam-app%u-%d.scope", &appid, &pid)` in
`src/Utils/Process.cpp`, evaluated at window creation from the pid the X server
reports for the client (XRes) — not from anything the window carries. The
`app-steam-app` prefix is Steam's own name for a launched app's scope and is an
upstream contract, not ours to rename. `docs/V2_DESIGN.md` §5 states the rule
the same way: **scope first, tag as repair, never by name.**

This is not a refinement. Post-hoc `STEAM_GAME` tagging on its own **stopped
working** when gamescope was pinned from 3.16.23 to 3.16.28
(`jedwards1230/homelab-ansible#321`). Measured on the bench, 2026-09-06:

| launch | `GAMESCOPE_FOCUSABLE_WINDOWS` |
|---|---|
| the session's own Moonlight, post-hoc tag attempted | *empty* |
| control: plain launch, no scope | *empty* |
| inside `app-steam-app9003-2970.scope` | `8388625, 9003, 2998` |

The scoped launch worked with **no tagging at all** — `STEAM_GAME` was never
set — and the display went to `fps=120.000000 / focus=9003`. cgroups confirmed
it: the working pid sat in the scope, the two failing ones in a plain
`session-N.scope`. The unscoped case is also a chicken-and-egg for the kit
itself, because `gs_tag_pid` *discovers* candidate windows through that same
empty atom.

`lib.sh` owns the mechanism: `gs_scope_ready` checks that `systemd-run` and a
session bus (`XDG_RUNTIME_DIR` + `DBUS_SESSION_BUS_ADDRESS`, both carried in
`/tmp/tv-shell-gamescope.env` for SSH callers) are there and **refuses loudly**
rather than falling back to an unscoped launch; `gs_scope_run <appid> <cmd...>`
execs the command under `systemd-run --user --scope --collect
--unit=app-steam-app<appid>-<pid>`. Because both `systemd-run --scope` and
`gs_scope_run` exec, the pid a caller captures with `$!` is the app's own pid
the whole way down, so pid-keyed tagging, `--family` tree walks and the
supervisor's `wait` are unchanged. A cgroup is inherited, so a process family
(Steam → steamwebhelper → the `streaming_client` a Remote Play stream spawns)
and a client's later windows are all identified without a single tag.

Since the app id is fixed by the scope name **at launch**, the base-layer
preference is now written *before* the process exists: there is no window in
which the app is on screen under the wrong base layer, and nothing racing a tag
that may never land.

The one deliberate exception is `launch.sh overlay`: an overlay is not an app,
it is a layer over one, and giving it an app id would make it a focus
candidate. It keeps `STEAM_OVERLAY` + `STEAM_INPUT_FOCUS`.

`launch.sh moonlight` runs Moonlight on X11 (`QT_QPA_PLATFORM=xcb`, `SDL_VIDEODRIVER=x11`,
`ENABLE_GAMESCOPE_WSI=1`) in `app-steam-app9003-<pid>.scope`, sets the base-layer
preference to `9003,9001` before launching, and then makes one **repair** pass tagging
the windows of Moonlight's pid `STEAM_GAME=9003`; `focus.sh app 9001` brings the shell
back. The repair pass is by pid, not by title, because **the stream is not the window
named "Moonlight"**: that is the Qt main window, which `moonlight stream` unmaps once the
session starts. The stream window is a second X window (`WM_NAME "<host> - Moonlight"`,
`WM_CLASS "moonlight"`, `_NET_WM_PID` = Moonlight's pid) created after the session
handshake, 5-20 s in — and it belongs to the same process in the same scope, so
gamescope resolves it to 9003 the moment it exists, tagged or not. That is what retired
`client.sh`'s day-long background tag watcher: with the scope doing the identifying,
a watcher waiting all day for a window that needs no tag is a process that looks busy
and is not. `gs_tag_pid` (lib.sh) re-scans once a second for up to 60 s and stops once a
window named `* - Moonlight` is tagged; each window is tagged once and printed as
`tagged 0x... "<name>" STEAM_GAME=9003 (t+12s)`. Its remaining job is the
`STEAM_GAME` override for a window whose scope did **not** resolve — a pid namespace
(Plex under `bwrap`), or a browser that handed off to an already-running instance — and
the kit's window-by-window report of what actually appeared.

There is no window-enumeration call to lean on: xprop is the only X client here and
gamescope publishes no `_NET_CLIENT_LIST` (`GAMESCOPE_FOCUSABLE_WINDOWS` lists only
windows that already carry a game id). So the watcher collects candidate xids from the
WSI layer's own log lines (`Creating Gamescope surface: xid: 0x...`, written the moment
Moonlight creates the surface), from `GAMESCOPE_FOCUSABLE_WINDOWS` (re-runs), from
`xprop -name` hints, and from the next `TV_SHELL_GS_XID_PROBE` (32) resource ids above
each window it already knows, since an X client allocates ids sequentially. Every
candidate is kept only when its `_NET_WM_PID` is the pid or its `WM_CLASS` carries
`moonlight`. The same helper tags the prototype shell in `client.sh` (title as a hint,
pid as the rule, so a relaunched shell is never confused with a window of the instance
being torn down) and is exposed as `focus.sh tag-pid <pid> <id>` for anything else.

Two things the live run showed about the atoms themselves. `xprop -root _NET_CLIENT_LIST`
answers `no such atom` under gamescope's XWM (the helper tolerates that; it is why the
candidate sources above exist), so a leftover-window check after a client exits has to go
through `xprop -name <title>` or `focus.sh list`, never a client list. And after the stream
quits, `GAMESCOPECTRL_BASELAYER_APPID` still reads `9003, 9001` until `focus.sh app 9001`
clears it; that is cosmetic, since with 9003 gone the shell is already the effective base
layer, but read `GAMESCOPE_FOCUSED_APP`, not the preference, to know what is on screen.

**Ask the streaming host before streaming.** `moonlight stream <host> <app>` while
Sunshine is already running a *different* app pops a "quit the running app?" dialog
inside Moonlight's unmapped GUI and waits forever (the first phase-3 attempt sat for 74 s
with no session lines). `launch.sh moonlight stream ...` therefore reads
`http://<host>:47989/serverinfo` first (`<state>`, `<currentgame>`; port via
`TV_SHELL_GS_SUNSHINE_PORT`), maps the running app id to its name through Moonlight's
own cache (`~/.config/Moonlight Game Streaming Project/Moonlight.conf`), and then:
idle → streams; already running exactly the requested app → streams (Sunshine resumes
it, nothing on the host changes); running something else → **refuses** (exit 3) and
prints the two ways out, resume what is running, or `launch.sh moonlight --quit stream
...` / `launch.sh moonlight quit <host>`, which ends the session on the host. That is
the operator's decision, never the kit's: nothing here quits a running app unless
`--quit` is on the command line. An unreachable serverinfo is a warning, not a refusal
(Moonlight fails fast in that case, it does not hang).

**Sunshine app names may start with a space** (`" Desktop"`, `" Steam Big Picture"` on
the streaming host measured here). The name is passed to Moonlight verbatim, so quote it
with the space: `launch.sh moonlight stream <host> ' Steam Big Picture'`. `launch.sh
apps <host>` prints the host's state, the running app, and every cached name in quotes
(`'  Desktop'`, with `<- running now` on the current one), plus the live `moonlight list
<host>` output the same way, so the exact string can be copied.

`launch.sh steam` runs the full Steam client on X11 (`steam -bigpicture`; `--gamepadui`
for `steam -gamepadui`), sets the base-layer preference `9004,769,9001` first, waits for the
first window of the family, prints the focus atoms, and then leaves a detached watcher
running for `TV_SHELL_GS_STEAM_WATCH_SECS` (600) s (`steam-tag.log`) because the Remote
Play stream is started by hand on the pad. Steam is a process **family**, not one pid:
the launcher script, the client, `steamwebhelper`, and the separate `streaming_client`
that a stream spawns, each with its own X connection and windows. The watcher therefore
follows the pid tree (`--family`, re-read every poll, and the watch outlives a launcher
that exits) and the `WM_CLASS` set `steam` / `steamwebhelper` / `streaming_client`, and
gamescope itself lists the Remote Play window in `GAMESCOPE_FOCUSABLE_WINDOWS` even
untagged (`STEAM_STREAMING_CLIENT=1` makes it a focus candidate under `--steam`), which is
one of the candidate sources. Two Steam-specific rules: a window the client tagged itself
(`STEAM_GAME=769` for Steam, or the streamed app's id) is reported and **left alone**
(`--keep-existing`), never overwritten, and 769 sits in the base list so those windows are
selectable either way; and `--watch-baselayer` logs every change of
`GAMESCOPECTRL_BASELAYER_APPID` / `GAMESCOPE_FOCUSED_APP` with a timestamp
(`steam-baselayer.log`, `focus.sh watch-baselayer` by hand), because the full client writes
that atom itself when it starts a stream (the SteamOS mechanism). It does fight the kit's
list, and wins: the 2026-09-05 run above has the log. `launch.sh steamlink` does the same for the
standalone Steam Link client (`flatpak run com.valvesoftware.SteamLink` when installed,
else a `steamlink` on `PATH`, else a clear "not installed"), app id 9005, base list
`9005,9001`.

X11 is the path that survives here, and the **only** HDR path: gamescope's WSI layer
hardcodes `hdrOutput = false` for native-Wayland surfaces (3.16.23
`layer/VkLayer_FROG_gamescope_wsi.cpp:758`, master `:834`) and gamescope's own Wayland
server offers clients no colour-management protocol, so a Wayland-native Moonlight can
never get HDR under gamescope today. `launch.sh moonlight
--wayland` keeps the xdg-shell experiment for decode/latency comparisons only; Moonlight-qt
6.1.0 did not survive it (below) and it has no focus selector. The one thing to compare
between the two for criterion 3 is hardware decode: Moonlight warns that XWayland "will
probably break hardware decoding", so if the X11 stream falls back to software decode or
judders, `--wayland` is the control run.

The X11 layer exposes HDR10 formats only when the window can bypass XWayland (it must match
its toplevel within 2 px). Success signature in `moonlight.log`: `server hdr output
enabled: true` and `hdr formats exposed to client: true`. If the first is `true` and the
second stays `false`, run `GAMESCOPE_WSI_FORCE_BYPASS=1 launch.sh moonlight` (the variable
is passed through as-is). If the first is `false`, the root atom is 0: see the rule above.

Tunables are environment variables read by `session.sh` (`TV_SHELL_GS_HDR=0` for an SDR
control run, `TV_SHELL_GS_SDR_NITS`, `TV_SHELL_GS_EXTRA` for any other gamescope flag).
Set them in the session entry's `Exec` line, e.g. `Exec=env TV_SHELL_GS_HDR=0 /opt/tv-shell/dev/gamescope/session.sh`.

### First live results (2026-09-05)

One target box, gamescope 3.16.23, a 7.2-series kernel, an AMD GPU through an AVR to the
TV. The base layer was the kit's own prototype shell (SDR, X11). Verdicts of the first
pass (the phase-3 re-run with the fixed kit follows):

| # | Criterion | Result |
|---|---|---|
| 1 | 10-bit HDR at 4K120 | PASS on what is measurable: colorspace `BT2020_RGB`, `HDR_OUTPUT_METADATA` blob with EOTF = PQ, mode `3840x2160 120.00`. **Bit depth is unmeasurable on this kernel**: debugfs `output_bpc` prints only `Maximum: 12` (no `Current:`), gamescope sets `max bpc` to 16 (a requested cap), `gamescopectl` has no output-format command and the gamescope log never states the scanout depth. Only the TV's info panel can answer it |
| 2 | VRR engages | PASS: `VRR_ENABLED=1`, `backend_info` `VRR Active: true`, `GAMESCOPE_VRR_FEEDBACK=1`, `GAMESCOPE_DISPLAY_REFRESH_RATE_FEEDBACK=120` |
| 3 | Lone HDR stream not double-composited | UNTESTED with an HDR stream (8 was never reached). SDR baseline: the stats stream read `fps=120.000000` (with a few `119.95`) over 45 s with the shell alone and with the overlay on top, never 60. Re-run with an xcb Moonlight HDR stream once 8 passes |
| 5 | Focus unrefusable + instant | PASS: 20 of 20 switches landed, 14 to 20 ms each; `GAMESCOPECTRL_BASELAYER_APPID` honoured every time |
| 6 | Overlay over a running app | PASS (scriptable half): overlay tagged in 0.5 s, the app stayed the base window, 120 fps held, focus returned to the shell on close. "Keys go to the panel" needs a person at the TV |
| 8 | Moonlight HDR via WSI | **UNTESTED**: the reading `GAMESCOPE_HDR_OUTPUT_FEEDBACK=0` with `GAMESCOPE_DISPLAY_SUPPORTS_HDR=1` was self-inflicted. An ad-hoc probe ran a bare `gamescopectl hdr_enabled` at 10:24, which on this build resets `hdr_enabled` to false; every atom read and the xcb Moonlight run came after it, and the connector (last measured in PQ/BT2020 at 10:19) was never re-read. The native-Wayland run at 10:21 could not have shown HDR either (WSI layer, see above). Re-run with the rule above: xcb Moonlight, no bare convar queries, `measure.sh` `HDR to clients` PASS first |
| 4, 7, 9 | black floor / pad / Qt usable | need a person at the TV; 9 partially yes (the Qt 6 shell maps, holds focus, presents at 120 Hz) |

The kit defects that run exposed (a Qt 5 `qml` winning the resolver, the never-created
stats FIFO, the focus-stomping relaunch loop, the `measure.sh` root and bit-depth misreads,
Moonlight on native Wayland crashing) were fixed before the next pass.

#### 2026-09-05 phase 3 (fixed kit)

Same box, rebooted into the fixed kit (the kit's own Qt 6 shell as base layer, app 9001).
Every scriptable criterion passed; **the decision rule (1 and 3) is met**, so gamescope
stays on the table for v2.

| # | Criterion | Result |
|---|---|---|
| 1 | 10-bit HDR at 4K120 | PASS: colorspace `BT2020_RGB`, `HDR_OUTPUT_METADATA` blob with EOTF = PQ, mode `3840x2160 120.00`, `GAMESCOPE_HDR_OUTPUT_FEEDBACK=1` with the connector in BT2020 in the same second. Bit depth: still UNKNOWN to the kernel (debugfs has no `Current:` line, `max bpc` 16 is the requested cap); read off the TV's info panel, which is the reading of record |
| 2 | VRR engages | PASS: `VRR_ENABLED=1`, `backend_info` `VRR Active: true`, `GAMESCOPE_VRR_FEEDBACK=1`, at boot, under `measure.sh`, under the stream and after it |
| 3 | Lone HDR stream not double-composited | PASS: with a 4K120 HEVC Main10 HDR Moonlight stream as the base layer the stats FIFO read `fps=120.000000` (a few `119.95`) for 30 s with `focus=9003`; with the SDR shell over the running stream, 120 for 30 s; back to the stream, 120 for 10 s. Never 60, never a doubled frame time. Judder remains a person-at-the-TV call |
| 5 | Focus unrefusable + instant | PASS: 5 cycles x 4 switches, 20 of 20 landed, 14 to 19 ms each, no supervisor interference |
| 6 | Overlay over a running app | PASS (scriptable half): overlay tagged in 0.5 s, `GAMESCOPE_FOCUSED_APP` empty while it owns input, base layer untouched, 120 fps held, focus back to the shell on close |
| 8 | Moonlight HDR via WSI | PASS: stream `3840x2160x120` HEVC Main10 (`hdrMode=1`), VAAPI on x11, Mailbox present; the WSI layer on the stream window logged `server hdr output enabled: true` / `hdr formats exposed to client: true`, the swapchain was recreated as `VK_FORMAT_A2B10G10R10_UNORM_PACK32` with `VK_COLOR_SPACE_HDR10_ST2084_EXT`, and `VkHdrMetadataEXT` carried BT.2020 primaries with a 1670-nit mastering peak. No `GAMESCOPE_WSI_FORCE_BYPASS` needed. Shown as base layer (`FOCUSED_APP=9003`) once the stream window was tagged |
| 4, 7, 9 | black floor / pad / Qt usable | need a person at the TV; 9 partially yes (the Qt 6 shell maps, holds focus, presents at 120 Hz) |

One thing to know for the week: an **HDMI hotplug** (the AVR/TV re-negotiating, seen
once, one second long, coinciding with a Moonlight launch) makes gamescope drop and
re-select the connector, and for that second `GAMESCOPE_HDR_OUTPUT_FEEDBACK`,
`GAMESCOPE_DISPLAY_SUPPORTS_HDR` and `GAMESCOPE_VRR_FEEDBACK` all read 0. They come back
to 1 by themselves, but any client that creates its Vulkan surface inside that second
sees `server hdr output enabled: false` and gets SDR for the lifetime of that surface.
A Moonlight run that logs `false` right after a hotplug is an artifact: quit and
relaunch it.

The three kit defects that pass exposed were: the stream window never tagged (it is not
the window named "Moonlight"), `moonlight stream` hanging on the host's "quit the running
app?" dialog, and the leading space in Sunshine's app names. All three are fixed above,
and the fixes were re-run on the same box with no manual step: `launch.sh moonlight
stream …` tagged the stream window 0x800031 at t+3 s, `GAMESCOPE_FOCUSABLE_APPS` read
`9003, 9001` and `GAMESCOPE_FOCUSED_APP` 9003 at t+6 s, the TV switched by itself, the WSI
signature and the HDR10 swapchain were as above, 120 fps held as base layer and with the
shell over it; the busy-host refusal exited 3 within a second naming the running app; and
`launch.sh apps` showed the leading space in the quoted names.

## Known gaps, on purpose

- Criterion 10 has been run on the box with two real Remote Play streams (2026-09-05,
  above): the WSI hook fix is confirmed, Steam declines HDR itself, 120 fps held, and
  Steam wins the base layer outright. Two parts remain open — whether the pad reaches the
  streamed game (needs a person at the TV) and the Steam Link client (not installed here).
  Beyond that run the Steam verbs are exercised only in the fixture (a fake process family
  + fake `xprop`).
- **`--expose-wayland` silently disables the WSI layer for every Steam-runtime app.** With
  it, gamescope hands its children `WAYLAND_DISPLAY=gamescope-0`; inside the Steam
  container pressure-vessel rewrites that to `wayland-0` (it only preserves `wayland-*`
  names) and `GAMESCOPE_WAYLAND_DISPLAY` to `/run/pressure-vessel/gamescope-socket`; the
  layer's `isRunningUnderGamescope()` compares the two, they never match, the layer
  registers no instance, every surface hook passes through, and the client dies (or, with
  the dialogs on, hangs black) on "Creating swapchain for non-Gamescope swapchain". Any
  pressure-vessel / Steam Linux Runtime app under such a session has no HDR. SteamOS never
  passes `--expose-wayland`. `TV_SHELL_GS_EXPOSE_WAYLAND=0` drops it; `launch.sh steam`
  also unsets the variable for Steam itself, and the 2026-09-05 run **proved that per-app
  unset sufficient on its own** — the layer hooked `streaming_client` with
  `--expose-wayland` still on and no session restart. The session-level flag is therefore
  optional for Steam. The trap itself stands for any other pressure-vessel / Steam Linux
  Runtime app launched under such a session without the unset. Moonlight is unaffected
  either way; it runs on the host, where both variables read `gamescope-0`.
- **Do not chase this with gamescope master.** Nothing in the layer changed between 3.16.23
  and 3.16.28 on this path (four unrelated commits), and master's bd52d6e sets
  `WAYLAND_DISPLAY=""` for children, which pressure-vessel turns into `wayland-0` again,
  re-creating the same breakage for every Steam Linux Runtime game. 3.16.28 is the safe pin.

- The prototype shell launches nothing. Everything is driven from SSH, because the point
  is to measure the compositor, not to port the shell.
- The input daemon's Hyprland actor finds no compositor and logs that it is deaf on
  every retry. Expected in this session.
- `--steam` puts gamescope in the SteamControlled focus strategy so the
  `GAMESCOPECTRL_BASELAYER_*` atoms work. That strategy only considers X11 windows with a
  game id (or Steam / a Remote Play client). Wayland-native clients (Moonlight with
  `--wayland`, a Quickshell `FloatingWindow`) get an app id from their cgroup only, so they
  cannot be selected with `focus.sh`. That is one of the findings, not a bug in the kit.
- Quickshell's `PanelWindow` is a layer-shell surface; gamescope maps every layer surface
  to a non-interactive overlay. The real shell cannot run here unchanged.
- The negotiated output bit depth is not readable on every kernel (see above). `measure.sh`
  reports what it can (debugfs Maximum, the `max bpc` cap) and says UNKNOWN rather than
  guessing; the TV's info panel is the reading of record there.
- HDR reaching clients depends on `GAMESCOPE_HDR_OUTPUT_FEEDBACK`, which gamescope owns
  (`hdr_enabled` && EDID HDR10). The kit reports it and never touches convars; a bare
  `gamescopectl <convar>` from any other tool resets that convar (see the rule above), and
  `gamescopectl hdr_enabled 1` is the recovery.
- Native-Wayland clients never get HDR under gamescope (the WSI layer hardcodes it off for
  Wayland surfaces, on 3.16.23 and on master). xcb is the only HDR path for Moonlight.
- The `qml` runtime must be Qt 6. A Qt 5 `qml` on `PATH` is skipped, and if no Qt 6 one
  is found `client.sh` logs what it tried and exits, leaving gamescope up with nothing to
  present.
