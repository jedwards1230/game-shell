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
| 3 | A lone HDR stream is not double-composited | stats file + eyes | frame time steady at the refresh with Moonlight HDR running; no judder |
| 4 | SDR black floor is right | eyes | the black end of the strip at the top of the prototype shell is OLED-black under `--hdr-enabled` |
| 5 | Focus is unrefusable and instant | `launch.sh xmessage`, `focus.sh app 9001`, repeat 20x | every switch lands, nothing tiles, no window "declines" |
| 6 | Overlay over a running app | `launch.sh overlay` | panel visible above the app, app keeps animating, keys go to the panel |
| 7 | Gamepad reaches the shell | press D-pad | `last key` counter climbs in the prototype shell (daemon uinput path works under gamescope) |
| 8 | Moonlight HDR via the WSI layer | `launch.sh moonlight` | stream is HDR on the TV, no fallback to SDR tonemap |
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

Then pick "TV Shell (gamescope prototype)" at the greeter. The default boot session is
untouched; log out to return to it.

## Files

| File | Role |
|---|---|
| `session.sh` | The session. Starts `tv-shell-input.service`, execs gamescope with `--steam --expose-wayland --keep-alive -W/-H/-r --hdr-enabled --adaptive-sync`, logs to journal tag `tv-shell-gamescope` |
| `client.sh` | gamescope's primary child. Runs `proto-shell.qml` on X11, tags it `STEAM_GAME=9001`, makes it the base layer, writes `/tmp/tv-shell-gamescope.env` for the SSH-side tools |
| `proto-shell.qml` | The prototype shell: shows window size, keyboard focus, last key, a moving dot, and a black-to-white strip |
| `proto-overlay.qml` | Overlay test client, semi-transparent side panel |
| `focus.sh` | `list` / `tag` / `app` / `window` / `clear` over gamescope's root X11 atoms |
| `launch.sh` | `overlay` / `x11` / `moonlight` / `xmessage` into the running session from SSH |
| `measure.sh` | Reads DRM connector properties, debugfs bit depth, the active mode, gamescope's own info, and prints verdicts |

## Running a measurement

From another machine, with the prototype session up on the TV:

```sh
ssh box 'sudo /opt/tv-shell/dev/gamescope/measure.sh'          # criteria 1, 2
ssh box '/opt/tv-shell/dev/gamescope/launch.sh xmessage hi'     # criterion 5
ssh box '/opt/tv-shell/dev/gamescope/focus.sh app 9001'         # back to the shell
ssh box '/opt/tv-shell/dev/gamescope/launch.sh overlay'         # criterion 6
ssh box '/opt/tv-shell/dev/gamescope/launch.sh moonlight'       # criteria 3, 8
ssh box 'tail -f /tmp/tv-shell-gamescope-stats'                 # criterion 3
```

Tunables are environment variables read by `session.sh` (`TV_SHELL_GS_HDR=0` for an SDR
control run, `TV_SHELL_GS_SDR_NITS`, `TV_SHELL_GS_EXTRA` for any other gamescope flag).
Set them in the session entry's `Exec` line, e.g. `Exec=env TV_SHELL_GS_HDR=0 /opt/tv-shell/dev/gamescope/session.sh`.

## Known gaps, on purpose

- The prototype shell launches nothing. Everything is driven from SSH, because the point
  is to measure the compositor, not to port the shell.
- The input daemon's Hyprland actor finds no compositor and logs that it is deaf on
  every retry. Expected in this session.
- `--steam` puts gamescope in the SteamControlled focus strategy so the
  `GAMESCOPECTRL_BASELAYER_*` atoms work. That strategy only considers X11 windows with a
  game id (or Steam / a Remote Play client). Wayland-native clients (Moonlight, a
  Quickshell `FloatingWindow`) get an app id from their cgroup only, so they cannot be
  selected with `focus.sh`. That is one of the findings, not a bug in the kit.
- Quickshell's `PanelWindow` is a layer-shell surface; gamescope maps every layer surface
  to a non-interactive overlay. The real shell cannot run here unchanged.
