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

Decision rule agreed 2026-09-04: if 1 or 3 fails, gamescope is out for v2 and the
Hyprland-plugin path is next; Smithay stays the v3 target on HDR grounds.

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

## Files

| File | Role |
|---|---|
| `session.sh` | The session. Starts `tv-shell-input.service`, creates the stats FIFO, execs gamescope with `--steam --expose-wayland --keep-alive -W/-H/-r --hdr-enabled --adaptive-sync`, logs to journal tag `tv-shell-gamescope` |
| `client.sh` | gamescope's primary child. Runs `proto-shell.qml` on X11 with a Qt 6 runtime, tags it `STEAM_GAME=9001`, makes it the base layer, writes `/tmp/tv-shell-gamescope.env` for the SSH-side tools, relaunches it on exit (with backoff once it crash-loops) |
| `lib.sh` | Sourced by `client.sh` and `launch.sh`: the Qt 6 `qml` resolver (`qml6`, `/usr/lib/qt6/bin/qml`, `/usr/lib64/qt6/bin/qml`, then a `qml` whose `--version` says 6; `TV_SHELL_GS_QML` overrides) |
| `proto-shell.qml` | The prototype shell: shows window size, keyboard focus, last key, a moving dot, and a black-to-white strip |
| `proto-overlay.qml` | Overlay test client, semi-transparent side panel |
| `focus.sh` | `list` / `tag` / `app` / `window` / `clear` over gamescope's root X11 atoms |
| `launch.sh` | `overlay` / `x11` / `moonlight` (X11, tagged `STEAM_GAME=9003`; `--wayland` for the xdg-shell experiment) / `xmessage` into the running session from SSH |
| `measure.sh` | Reads DRM connector properties, debugfs bit depth, the active mode, gamescope's own info (`gamescopectl`, `backend_info`, `help`) and the root feedback atoms, and prints verdicts. Every DRM read is scoped to one connector (the first connected + enabled one, or `TV_SHELL_GS_CONNECTOR=card1-HDMI-A-1` to choose on a two-output box) and to the CRTC driving it. Under `sudo` the gamescope-side reads run as the session user |

## Running a measurement

From another machine, with the prototype session up on the TV:

```sh
ssh box 'sudo /opt/tv-shell/dev/gamescope/measure.sh'          # criteria 1, 2, 8 (signal)
ssh box '/opt/tv-shell/dev/gamescope/launch.sh xmessage hi'     # criterion 5
ssh box '/opt/tv-shell/dev/gamescope/focus.sh app 9001'         # back to the shell
ssh box '/opt/tv-shell/dev/gamescope/launch.sh overlay'         # criterion 6
ssh box '/opt/tv-shell/dev/gamescope/launch.sh moonlight'       # criteria 3, 8 (X11, tagged 9003)
ssh box 'cat /tmp/tv-shell-gamescope-stats'                     # criterion 3 (a FIFO, see below)
```

First check the journal (`journalctl -t tv-shell-gamescope -b`) for the line
`qml runtime: ... (6.x.y)` from `client.sh`. A `FATAL: no Qt 6 qml runtime found` line
lists what was tried; until it is fixed gamescope presents no frames, the TV is black, and
every DRM verdict from `measure.sh` is an artifact of "no client ever rendered".

`measure.sh` needs `sudo` for the debugfs read, and under `sudo` runs the gamescope-side
reads (`gamescopectl`, `xprop`) as the session user itself. Its `HDR to clients` verdict
is criterion 8's signal: the WSI layer offers HDR swapchains only while the root atom
`GAMESCOPE_HDR_OUTPUT_FEEDBACK` is 1, whatever the connector is doing.

The stats path is a **FIFO**, created by `session.sh` and removed with the session.
gamescope only `open()`s an existing one (retrying every 10 s), so it attaches within
10 s of a reader appearing. One reader at a time: `cat` or `tail -f` it, and stop that
reader before starting another. Lines are `fps=<float>` and `focus=<appid>` at about 1 Hz.
`measure.sh` samples it for 4 s, which takes the stream over from any other reader for
that window.

`launch.sh moonlight` runs Moonlight on X11 (`QT_QPA_PLATFORM=xcb`, `SDL_VIDEODRIVER=x11`,
`ENABLE_GAMESCOPE_WSI=1`), tags its window `STEAM_GAME=9003` and makes it the base layer
over the shell, so `focus.sh app 9001` brings the shell back. That is the path that
survives here: `launch.sh moonlight --wayland` keeps the native xdg-shell experiment,
which Moonlight-qt 6.1.0 did not survive (below) and which has no focus selector.

Tunables are environment variables read by `session.sh` (`TV_SHELL_GS_HDR=0` for an SDR
control run, `TV_SHELL_GS_SDR_NITS`, `TV_SHELL_GS_EXTRA` for any other gamescope flag).
Set them in the session entry's `Exec` line, e.g. `Exec=env TV_SHELL_GS_HDR=0 /opt/tv-shell/dev/gamescope/session.sh`.

### First live results (2026-09-05)

One target box, gamescope 3.16.23, a 7.2-series kernel, an AMD GPU through an AVR to the
TV. The base layer was the kit's own prototype shell (SDR, X11). Verdicts:

| # | Criterion | Result |
|---|---|---|
| 1 | 10-bit HDR at 4K120 | PASS on what is measurable: colorspace `BT2020_RGB`, `HDR_OUTPUT_METADATA` blob with EOTF = PQ, mode `3840x2160 120.00`. **Bit depth is unmeasurable on this kernel**: debugfs `output_bpc` prints only `Maximum: 12` (no `Current:`), gamescope sets `max bpc` to 16 (a requested cap), `gamescopectl` has no output-format command and the gamescope log never states the scanout depth. Only the TV's info panel can answer it |
| 2 | VRR engages | PASS: `VRR_ENABLED=1`, `backend_info` `VRR Active: true`, `GAMESCOPE_VRR_FEEDBACK=1`, `GAMESCOPE_DISPLAY_REFRESH_RATE_FEEDBACK=120` |
| 3 | Lone HDR stream not double-composited | Held 120 fps in SDR: the stats stream read `fps=120.000000` (with a few `119.95`) over 45 s with the shell alone and with the overlay on top, never 60. Not yet measured with an HDR stream, since 8 blocks it |
| 5 | Focus unrefusable + instant | PASS: 20 of 20 switches landed, 14 to 20 ms each; `GAMESCOPECTRL_BASELAYER_APPID` honoured every time |
| 6 | Overlay over a running app | PASS (scriptable half): overlay tagged in 0.5 s, the app stayed the base window, 120 fps held, focus returned to the shell on close. "Keys go to the panel" needs a person at the TV |
| 8 | Moonlight HDR via WSI | **FAIL**: `GAMESCOPE_HDR_OUTPUT_FEEDBACK=0` while `GAMESCOPE_DISPLAY_SUPPORTS_HDR=1` and the connector is in PQ/BT2020. Moonlight's WSI block logged `server hdr output enabled: false` and found no HDR-capable Vulkan device. Why the atom stays 0 with `--hdr-enabled` on 3.16.23 (EDID through the AVR? content-driven gating?) is the open question |
| 4, 7, 9 | black floor / pad / Qt usable | need a person at the TV; 9 partially yes (the Qt 6 shell maps, holds focus, presents at 120 Hz) |

The kit defects that run exposed (a Qt 5 `qml` winning the resolver, the never-created
stats FIFO, the focus-stomping relaunch loop, the `measure.sh` root and bit-depth misreads,
Moonlight on native Wayland crashing) are fixed in this version.

## Known gaps, on purpose

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
- HDR reaching clients depends on `GAMESCOPE_HDR_OUTPUT_FEEDBACK`, which gamescope owns.
  The kit reports it; it cannot force it.
- The `qml` runtime must be Qt 6. A Qt 5 `qml` on `PATH` is skipped, and if no Qt 6 one
  is found `client.sh` logs what it tried and exits, leaving gamescope up with nothing to
  present.
