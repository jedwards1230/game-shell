# tv-shell v2 — design

> **Status:** draft · 2026-09-05 · records the decisions taken on 2026-09-04 and 2026-09-05 and does not reopen them. It supersedes `docs/PRD.md` §5 "Compositor" and "Input arbitration" and §12 decision 5; every other PRD section stands. Open work is §13; the dated decision log is §14.
>
> Evidence base: the research reports listed in §15 and the live gamescope measurements of 2026-09-05 (jedwards1230/tv-shell#453, #454). Hosts are named by role only: the HTPC, the streaming host, the AVR, the TV. Two reviews (adversarial and architectural) were run over the first draft; their findings are folded in or listed in §13.

## 1. Goals and non-goals

**Goals**

1. The kiosk invariant — exactly one app fills the screen, backgrounded apps keep running and never share it — is enforced by a compositor primitive that no window can refuse and that cannot half-succeed.
2. Every claim the shell makes about the screen is verified against the compositor's own published state, and a claim that cannot be verified is a failure, never a silence.
3. 4K120 HDR10 with VRR on the TV, with a real 4K120 HDR Moonlight stream presenting at the refresh rate alone and with the shell drawn over it.
4. We do not write a compositor. As SteamOS does, we own what sits on top of gamescope: the shell, the pad daemon, AV control and the supervisor. One Rust core process owns policy; the shell UI is an ordinary client of the compositor with no privileged surface type.
5. In-repo supervision: a session target with a frame heartbeat, restart with backoff, and a couch-reachable and LAN-reachable escape hatch in every state.
6. The whole window model is testable in CI against a real headless compositor, and asserted in the field by the running core.
7. Moonlight and Steam Remote Play are both first-class, permanently supported, independently testable streaming clients (§12).
8. v1 keeps working on the couch until v2 replaces it; both are selectable sessions on the same install and neither can break the other.
9. IP is the AV authority; CEC is an observer. The box never claims or releases the display unless it owns it.

**Non-goals** (PRD §4 stands; these are the v2 additions)

| Not doing | Why |
|---|---|
| Layer-shell, tiling, or native-Wayland windows on the focus/HDR path | gamescope's focus control is X11-atom driven; its WSI layer exposes HDR only to X11 surfaces (§6) |
| Smithay, wlroots, or a Hyprland plugin as the compositor | No upstream HDR (Smithay, cage); ABI churn and the screencopy blind spot (Hyprland plugin). See `gamescope-eval.md` |
| A virtual-pad twin while a game is on screen | Games get the real evdev node (§7) |
| libcec / `cec-rs` in the core | Six contenders for one exclusive tty; the in-process design is the defect generator (§8) |
| Deciding the plugin mechanism now | Requirements only (§13 Q2) |
| SteamOS itself, an immutable root, nested per-app gamescope | Rejected 2026-08-29; unchanged. The Steam-as-shell fallback in §12 is a gamescope-session on our own OS, not this |

## 2. The vision that does not change

A 10-foot couch console: controller-first, B always goes back, every element D-pad reachable, sized for a 4K OLED at couch distance, HDR10 and VRR at 120 Hz. It launches Moonlight streams, local apps and web apps full-screen, owns the gamepad fleet exclusively, wakes and releases the AV chain on its own, and is machine-drivable over socket, HTTP, MCP and MQTT with one intent vocabulary. The kiosk invariant is the product. PRD §1–§4 and §6.8 describe the finished couch UI and are not restated.

## 3. Why v1 failed

v1 re-fixed the kiosk invariant nine times in four months on Hyprland, and the symptom is still present after the ninth fix (jedwards1230/tv-shell#455, open).

| # | Date | PR | What it changed | Why it was insufficient |
|---|---|---|---|---|
| 1 | 05-27 | #49 | Window matching + resume | Matching only; nothing enforced fullscreen |
| 2 | 06-07 | #196 | Hyprland Lua window rules | Rule syntax churn; seven follow-up passes |
| 3 | 07-01 | #293 | Kiosk fullscreen on open | Open-only; a later window still took the screen |
| 4 | 07-01 | #294 | Continuous enforcement | Two racing enforcers on one workspace |
| 5 | 07-04 | #307 | Daemon self-heal + `KIOSK_WINDOW_MODEL.md` | Promised "one workspace per app", never built |
| 6 | 07-04 | #308 | Declarative lockdown, single enforcer | "Kills the root cause"; falsified in 15 days by #347 |
| 7 | 07-20 | #348 | Assert fullscreen after focus lands | Focusing a tiled window under a fullscreen one changes focus, not the screen |
| 8 | 08-26 | #444 | One workspace per app class | Found the July doc's "single workspace" clause was enforced by no code; falsified in 3 days |
| 9 | 08-29 | #448 | Accept the bare socket2 address | `openwindow` parsing required a `0x` prefix the compositor never sends; park never ran once. Unit fixtures asserted the shape the code wanted |

Three failure classes recur, all at the compositor boundary (`config/hyprland.conf`, `daemon/src/hyprland.rs`, `AppLifecycleManager.qml`, `shell.qml`; 204 commits mention focus; `fix:` outnumbers `feat:` 1.66:1; `shell/` has a 53% rewrite ratio and has not converged):

- **Silent success.** `hyprctl dispatch` exits 0 when its selector matches nothing. `ok` was returned for a dropped launch (#376), an escape that could not leave fullscreen (#436), an unparked window (#448), a stopped heartbeat (#402), and a compositor wedged for nine days (#383). The 2026-09-04 postmortem found zero `parked` lines across a whole boot on a binary containing #448; the residual cause (event listener attached, nothing processed) is still unknown, so v2 must not depend on an event path alone (§10).
- **The compositor answers about a different object.** As a layer-shell surface the shell never appeared in `activewindow`, which named a backgrounded toplevel instead (fixed by declaring ownership, #352). A focus request is something a window can decline: Remote Play's `streaming_client` reported `acceptsInput: false` and a live game became unreachable.
- **Exclusive-device contention.** The CEC adapter, the pads and the GPU each had more than one owner; each time the arbitration mechanism became the failure.

The two durable v1 fixes (#352 "declare ownership, never infer it"; #444 "a switch is a compositor operation no window can refuse") both removed a layer at which success could be reported without being true. v2 applies that to the whole boundary: gamescope's base-layer policy is unrefusable, it publishes the result as root-window atoms, and the core reads them back before it believes anything. The new risk is the mirror image: a name or shape the core writes that the compositor never reads (the scope name in §5 is the worked example). Every such contract is pinned in CI against the compositor's own bytes.

## 4. Architecture

Compositor: **gamescope**, DRM backend, `--steam` (SteamControlled focus policy), pinned 3.16.28 or newer (3.16.23 lacks the August keyboard-focus reclaim fixes and `focus_info`). Decided 2026-09-05 on the measured numbers in §6. Nothing below is a compositor of our own: the unit shape is SteamOS's, and the ChimeraOS `gamescope-session` unit files and scripts (ready-fd handshake, environment dump, short-session tracker, session select) are the starting point for the supervisor rather than units written from scratch.

```
display manager (autologin, Relogin=true) ── selects one of:
  tv-shell-wayland.desktop   → v1 session (Hyprland + Quickshell + tv-shell-input)   [unchanged]
  tv-shell-gamescope.desktop → v2 session script: stop any stale target, reset-failed,
      mkfifo ready + stats, systemctl --user start --wait tv-shell-session.target
        ├─ tv-shell-gamescope.service   Type=notify, NotifyAccess=all, TimeoutStartSec=5,
        │     Before=graphical-session.target; -R <ready fifo> → env dumped to
        │     %t/tv-shell-gamescope-environment, then systemd-notify --ready
        │     ├─ Xwayland :0  ← shell, overlay/QAM, notifications (X11 clients, self-tagged)
        │     └─ Xwayland :1… ← one server per launched app (GAMESCOPE_CREATE_XWAYLAND_SERVER)
        ├─ tv-shell-core.service        Upholds=, After=graphical-session.target, EnvironmentFile=-%t/…
        ├─ tv-shell-shell.service       Upholds=, After=…  — the UI, an ordinary X11 client
        ├─ tv-shell-stats.service       Wants=            — sole reader of the stats FIFO (§9)
        ├─ tv-shell-panel.service       Wants=            — LAN recovery + observability (survives core death)
        └─ tv-shell-cec.service         Wants=            — kernel-CEC observer sidecar (optional)
      apps: app-steam-app<id>-<pid>.scope, one per launch, on their own Xwayland server
```

`BindsTo`/`Upholds`/`Wants` carry no ordering; ordering is `graphical-session.target`, which becomes active only after gamescope's `READY=1`, and that is sent only after the environment file exists. The session script's `--wait` is what makes "gamescope dies → the session exits" true.

| Component | Owns | Never does |
|---|---|---|
| gamescope | Output mode, HDR/VRR state, which app id is on screen, keyboard/mouse focus, overlay plane, screenshots | Touch gamepads; know apps beyond an id |
| core (`tv-shell-core`, the daemon's successor) | Base-layer list; app launch, scope, tag; pad grab and presenters; intents; IPC, HTTP, MCP, MQTT, metrics; AV control; HDR toggle; field assertions | Draw anything; hold a privileged surface; keep state on disk that the X server and systemd already hold |
| shell (X11 client, fixed app id) | Home, Library, Settings, Widgets; drawers and QAM as separate `STEAM_OVERLAY` toplevels; tags its own windows | Talk to hardware; ask "who is on screen" |
| per-player presenters (uinput) | One clean virtual pad per player, always present; only grab and routing toggle | Appear and vanish (that is a hotplug to every game) |
| stats relay | Tail gamescope's stats FIFO into a file/socket both core and panel read | |
| panel | Unit restart, journal, deploy, supervisor state, escape hatches | Hold another node's bridge token |
| CEC sidecar | Observe the bus, report `<Active Source>`, forward remote keys as intents | Claim the display; share the adapter |
| `tv-shell-host` (streaming host) | Steam library, launch, quit, sleep | Unchanged from v1 |

**Carried over unchanged in contract:** the Unix-socket IPC framing and reply grammar, the four-transport control surface over one `bridge_core`, the closed intent vocabulary, the settings store with one writer, the capability handshake, MQTT/HA topics, the `/metrics` catalogue, web apps, the Steam sidecar, `shell-state` (media state for the HA suspend rule, a separate channel from the frame heartbeat), and logind `Active` watching (grabs release when another session takes the seat).

**Changed or deleted** (the IPC table in `docs/IPC_PROTOCOL.md` gets a deleted/renamed/kept column when the core lands):

| Item | Fate |
|---|---|
| `daemon/src/workspaces.rs`, `hyprland.rs`, `session_env.rs`, the park/reconcile paths, `hypr-*` commands and `hypr:*` events | Deleted; `screen-state` (a read of the gamescope atoms) replaces `hypr-active`/`hypr-clients`/`hypr-monitors` |
| `display_mode.rs` (daemon and panel): `monitor=` rewriting with a revert timer | Mode is pinned in `[display]`; a resolution/refresh change is a gamescope restart under the same confirm-or-revert contract; VRR and HDR are root-atom writes |
| `shell-focus on|off`, `overlay-focus` | The compositor publishes ownership; the shell's declaration survives only as the grab trigger (§7) |
| `grab`/`release`/`handoff`, `[input.contracts]` | Presenters collapse to two (§7) |
| `resumeFocus.js`, `prewarm.js`/`appQuirks.js` launch paths, `HyprctlClients.qml`, every fullscreen assertion, the layer-shell `PanelWindow`, the QML duplicates of display-mode and CEC-wake | Deleted or rewritten against the core |
| `WorkspaceAudioMuter.qml` / `audioOwnership.js` | Re-keyed to the base window's app id; the pure attribution logic is kept |
| `config/hyprland.conf*`, `scripts/super-intent.sh`, `tv-shell-session.sh` | Stay, v1-only |
| Suspend | Already one `power-suspend` path; v2 adds the policy gate (display ownership + media) it lacks |
| `settings_consumer_table()` | Every key whose reader is deleted (`hdrEnabled`, `nightLight*`, `overscan`, the `cec*` focus keys, `prewarmApps`, `wakeOnController`) names its new consumer or is dropped in the same PR |

## 5. Focus and window model

The invariant becomes a data structure gamescope owns. Under `--steam`, every mapped X window resolves to an app id and the base layer is the first id in an ordered list.

| Atom (X root unless noted) | Writer | Meaning |
|---|---|---|
| cgroup scope `app-steam-app<id>-<launcherpid>.scope` | core, at launch (`systemd-run --user --scope`) | **Primary id.** gamescope's only cgroup parser is `sscanf("app-steam-app%u-%d.scope")` (`src/Utils/Process.cpp`), evaluated at window creation from the XRes client pid. The prefix is an upstream contract, not ours to rename; it resolves before any core action and survives a core restart |
| `STEAM_GAME` (per window) | core (repair) / shell (its own windows) | Override for a window whose scope did not resolve (pid namespace, a browser that handed off to a running instance); authoritative when present |
| `GAMESCOPECTRL_BASELAYER_APPID` | core | Ordered list; first id with a mapped window is on screen. "Show app" = `[<id>, <shell>]`; "home" = `[<shell>]`; an app exiting falls back to the shell with no compositor action |
| `GAMESCOPE_FOCUSED_WINDOW`, `GAMESCOPE_FOCUSED_APP`, `GAMESCOPE_FOCUSABLE_WINDOWS`/`_APPS` | gamescope | Published result. **The base window is `GAMESCOPE_FOCUSED_WINDOW`**; `GAMESCOPE_FOCUSED_APP` reads empty while an input-focus overlay is up (measured), so every rule below keys on the window's app id |
| `GAMESCOPE_CREATE_XWAYLAND_SERVER` / `_FEEDBACK` (`"<identifier> <server_id> <display>"`), `GAMESCOPE_DESTROY_XWAYLAND_SERVER`, `GAMESCOPE_XWAYLAND_SERVER_ID` (per server root) | core / gamescope | One Xwayland server per app; `GAMESCOPE_FOCUS_DISPLAY` says which server holds the keyboard |
| `STEAM_OVERLAY=1` + `STEAM_INPUT_FOCUS=1`, `STEAM_NOTIFICATION=1` (per window) | shell, on its own toplevels before map | Drawer/QAM over a running app takes keyboard and mouse without changing the base layer (measured at 120 fps over an SDR base); toasts |
| `STEAM_STREAMING_CLIENT` (per window) | Steam | Remote Play client; always a focus candidate under SteamControlled (§13 Q3) |

Rules the core enforces:

- **Scope first, tag as repair, never by name.** Moonlight's stream window is a second X window replacing its main one (kit defect K6); a name-keyed tag lands on a window about to die. The core derives pids with `XResQueryClientIds` as gamescope does (not the client-asserted `_NET_WM_PID`), watches `MapNotify` on every server, and writes `STEAM_GAME` only where the scope did not resolve.
- **Multi-process apps get a class-keyed fallback.** A second Chromium `--app` launch hands off to the running browser and exits, so neither scope nor pid names it; each web app runs with its own `--user-data-dir`/`--class`, and the launch table matches on pid or `StartupWMClass` (§13 Q4).
- **Shell windows self-tag.** The shell is a service, not a launched app, and its drawers share its pid, so the shell process sets `STEAM_GAME` on its base window and `STEAM_OVERLAY`/`STEAM_INPUT_FOCUS`/`STEAM_NOTIFICATION` on each overlay toplevel before mapping it. That needs `XChangeProperty` access from the shell runtime (§13 Q1) and makes drawer, QAM and toasts separate X toplevels.
- **One write, then verify.** A switch is one `GAMESCOPECTRL_BASELAYER_APPID` write followed by a read of `GAMESCOPE_FOCUSED_WINDOW`'s app id within a bounded window (measured 14–19 ms). A mismatch is an IPC error, a metric and a log line, never `ok`.
- **Transient unmaps are held, not followed.** An app whose last window unmaps for a moment (Moonlight main → stream window, a browser navigation) would drop the base layer to the shell, flip grab and audio, and flip back. The core pins `GAMESCOPECTRL_BASELAYER_WINDOW` across known transitions and applies a short hysteresis before treating a fallback as an exit. Exit fallback itself is unmeasured and is a bench row (§10).
- **Audio follows the base window.** "You hear the workspace on screen" becomes "you hear the base window's app id"; PipeWire nodes are still attributed by `application.process.binary`.
- **The shell's app id is private** (the kit used 9001). Under `--steam`, 769 is the Steam client's own id (`window_is_steam`: forced fullscreen sizing, `focus=steam` in the stats pipe) and is reserved for the Steam client when it runs as an app (§12); the shell sizes itself to the output instead of inheriting that path.

## 6. Display and HDR

Measured 2026-09-05 on gamescope 3.16.23, an AMD iGPU, through the AVR to the TV; the kit is `dev/gamescope/` and is the regression bench (§10). Every number below was taken with `--hdr-enabled` on the command line, a single Xwayland server, and an SDR prototype shell as the base layer; the cutover re-measures on the pinned build through the runtime atoms and the per-app-server topology.

| Criterion | Result | Method |
|---|---|---|
| 3840x2160 @ 120 Hz | PASS | `active CRTC mode: 3840x2160 120.00`; `GAMESCOPE_DISPLAY_REFRESH_RATE_FEEDBACK=120` |
| HDR10 signalling | PASS | `Colorspace = BT2020_RGB`, `HDR_OUTPUT_METADATA` blob with EOTF = PQ |
| VRR | PASS | `VRR_ENABLED=1`, `VRR Active: true`, `GAMESCOPE_VRR_FEEDBACK=1` |
| 4K120 HDR Moonlight stream alone / with shell switched over it / back | PASS | stats FIFO `fps=120.000000` throughout, never 60; HEVC Main10, VAAPI on X11 |
| HDR10 swapchain exposed to an X11 client | PASS | WSI: `server hdr output enabled: true`, `hdr formats exposed to client: true`; swapchain `A2B10G10R10_UNORM_PACK32 / HDR10_ST2084_EXT`; no forced bypass needed |
| Focus switch | PASS | 20 of 20, 14–19 ms |
| Overlay over a running app | PASS over an SDR base | app stays base window at 120 fps, focus returns on close. **Not yet measured over the HDR stream** |
| Output bit depth | UNMEASURABLE | debugfs prints `Maximum: 12` with no `Current:` on this kernel; `max bpc` is a requested cap (gamescope leaves 16). The TV's info panel is the reading of record |
| Per-app Xwayland servers, app-exit fallback, static shell under VRR | NOT EXERCISED | bench rows in §10 |
| SDR black floor, pad reaches shell, Qt usable | eyes-only, pending | |

Design rules that follow:

- **Mode is pinned in config** (`[display]` width/height/refresh → `-W -H -r`): the EDID preferred mode is 60 Hz.
- **HDR is a runtime switch, SteamOS-style.** The core sets `GAMESCOPE_DISPLAY_HDR_ENABLED` on the root and reads `GAMESCOPE_HDR_OUTPUT_FEEDBACK` (= EDID HDR10 && `hdr_enabled`) and `GAMESCOPE_DISPLAY_SUPPORTS_HDR`. **Never a bare `gamescopectl <convar>`**: on 3.16.x and master a value-less call resets the convar to its default and turns HDR off with no log line (the phase-2 "feedback 0" was self-inflicted this way).
- **The hotplug window.** An HDMI re-negotiation (~1 s) zeroes `GAMESCOPE_HDR_OUTPUT_FEEDBACK`, `GAMESCOPE_DISPLAY_SUPPORTS_HDR` and `GAMESCOPE_VRR_FEEDBACK`, then restores them; a Vulkan surface created inside it stays SDR for its life. The one observed instance coincided with the Moonlight launch to within a second, and its cause is open (the AVR, an audio-format renegotiation on stream start, the v1 CEC lifecycle; §13 Q9). Policy: gate an HDR-capable launch on the feedback atom reading 1 for a settle period; detect an SDR-stuck client from its own HDR feedback, not by scraping its log; the relaunch-once fallback is provisional until the cause is known, because if the launch causes the hotplug it would kill every first stream.
- **HDR clients are X11 clients** under the WSI layer (`ENABLE_GAMESCOPE_WSI=1`, `QT_QPA_PLATFORM=xcb`). The layer hard-codes `hdrOutput = false` for native-Wayland surfaces in every version and gamescope serves no colour-management protocol. The window must match its toplevel within 2 px, else `GAMESCOPE_WSI_FORCE_BYPASS=1`. Moonlight 6.1.0 on gamescope's native Wayland segfaults anyway (gamescope#2261 family).
- **`CONFIG_AMD_PRIVATE_COLOR` is optional.** It gates only the AMD plane colour pipeline (direct scanout with hardware TF/LUTs). Without it gamescope composites in Vulkan, and that cost was invisible at 4K120 on this GPU for a base-layer switch; the overlay-over-HDR composite is the unmeasured case.
- **SDR in HDR.** `--hdr-sdr-content-nits` sets the shell's white; the black floor is criterion 4 (eyes). gamescope#1887 (SDR oversaturated on an HDR output) is the known risk for the shell's own colours.
- **VRR default is an open question** (§13 Q11): the ops record has OLED near-black flicker and AVR OSD notes recommending VRR off.

## 7. Input

This supersedes PRD §5 "Input arbitration" (four presenters).

- **The core keeps `EVIOCGRAB` of the pad fleet**: DB-match-or-reject discovery, stable per-player slots, hot join/leave, rumble/battery/LED, per-player uinput presenters. gamescope never opens joystick nodes (libinput ignores the class; true in SteamOS too), but a pad's companion touchpad/motion nodes present as pointers gamescope will read; discovery claims or inhibits them (SteamOS ships `ds-inhibit` for this).
- **Two contracts, not four.** `gamepad` (the default, games and streams): the physical node is ungrabbed while the app is the base window, so the game sees the real pad and no virtual twin double-fires. `keyboard` (web apps, Plex): the grab stays and the core translates the pad to a uinput keyboard, since a browser reads no gamepad. `handoff` collapses into `gamepad`.
- **Grab follows visibility, devices do not.** The grab is armed when the shell is the base window or a `STEAM_INPUT_FOCUS` overlay is mapped, and dropped otherwise; the uinput presenters stay present throughout (create/destroy is a hotplug event every game and Moonlight forward to the streaming host, #402). Sequence: overlay maps → grab → mask held buttons → route to the shell key-map; overlay unmaps → unmask → ungrab. With the pad ungrabbed a Guide tap reaches the game before the hold threshold; accepted, as in v1's Handoff.
- **Escapes.** The Meta hold and the safety combos come from a passive, non-grabbing reader in the core, so they are unrefusable by the compositor but depend on the core being alive (`Upholds=` is the mitigation). The keyboard escapes (`Super`, `Super+Escape`, `Super+Backspace`) are `gamescope-action-binding` entries and survive a dead core.
- **Keyboard stays with the compositor.** gamescope routes keyboard focus to the base window or the overlay deterministically (`GAMESCOPE_FOCUS_DISPLAY`); the shell reads keys through Qt, and automation injects nav keys via a uinput keyboard (or libei through `gamescope-eis`).
- **Intents are unchanged**: `home`, `home-tap`, `home-hold`, `menu`, `settings`, `power`, the `settings:`/`overlay:`/`app:` deep links, `key up|down|left|right|select|back`.

## 8. AV control

The chain is HTPC HDMI → the AVR (video, plus a CEC-only leg to a USB CEC adapter) → the TV. Facts that fix the design: the AVR in standby powers down its CEC line and its NIC unless its menu enables network control in standby; the AVR's telnet port accepts one client; a TV cold-start needs WoL plus the TV's own wake-over-network setting and a webOS pairing key; a receiver ignores CEC from non-selected inputs; the adapter's tty has one owner and the loser fails silently.

| Concern | v2 owner | Mechanism |
|---|---|---|
| Wake / input select / standby of the AVR | core, IP | Denon/Marantz ASCII telnet (`PWON`, `SI<input>`, `Z2OFF`, `PWSTANDBY`); port `av_net.rs` from the closed, unmerged PR #191 (branch `feat/daemon-av-lifecycle`, 465 lines, 9 tests) to typed `config.toml` |
| TV cold wake / power off | core, IP | WoL magic packet sent twice, webOS for state and standby |
| Display ownership | core | The passive gate (`owns_display()` needs positive proof; `may_claim_active_source()` yields to a known other owner) is kept; its sensor moves from CEC callbacks to the AVR's `SI` push events, with the sidecar's `<Active Source>` as second witness |
| Theater sleep on idle | core | Only when the HTPC is the AVR's selected input; otherwise release nothing |
| CEC | sidecar, kernel driver | Observer only: bus scan, active-source events, remote keys as intents. Sole owner of the adapter. libcec and `cec-rs` leave the tree with the static-link CI leg |
| TV remote passthrough | goal, not constraint | No evidence of routine remote use; it was silently dead for weeks in v1 |

Site preconditions (deployment, not code; listed in §11 cutover): AVR network control in standby enabled and no other telnet client holding the port; TV wake-over-network on and a webOS key provisioned; the v1 modprobe blacklist of the kernel CEC driver reversed and the Plex `bwrap` hiding kept.

## 9. Supervision and recovery

This reverses PRD §12 decision 5: sensor and actuator both live in the repo. The sensor has to earn that: gamescope's stats FIFO emits one `fps=` line per 300 paints, so on a static base layer under VRR it is silent for as long as nothing draws. The heartbeat is therefore a **forced-paint probe**: once a second the core damages a 1 px core-owned window (or issues a debug repaint), then waits a bounded time for the next `fps=`/`focus=` line. The FIFO has one reader, gamescope never reopens it, and lines written with no reader are dropped, so a small relay unit tails it into a file both core and panel read and holds the fd across core restarts. Its false-positive rate is a cutover number (§11).

| Failure | Sensor | Response |
|---|---|---|
| gamescope dies | `BindsTo=` stops the session target | Session script's `--wait` returns; autologin restarts the session |
| Shell dies | `Upholds=` on the shell unit | Restart under the live compositor; the base-layer list re-resolves to the shell |
| Shell crash-loops | Short-session tracker (`ExecStartPre`/`ExecStopPost`; 3 exits under 60 s) | Backoff, then a deployment hook selects the v1 session (a root-owned display-manager file; §13 Q8) |
| Core dies | `Upholds=` | Restart. On start the core is stateless: it reads `GAMESCOPECTRL_BASELAYER_APPID` back as its last intent, enumerates servers by `GAMESCOPE_XWAYLAND_SERVER_ID`, rebuilds the launch table from `app-steam-app*.scope` units, and never writes "home" on boot (that would yank a live game) |
| Frames stop | Forced-paint heartbeat, `tv_shell_frames_presented_total` | Stall over N s with a mapped base window → restart the shell; persisting → restart the session target. Session restart stays manual until the false-positive rate is measured |
| Compositor wedged but alive | Same relay file, read by the panel | The panel, independent of the core, restarts units and shows supervisor state |
| Stuck in an app | Meta hold → `intent home` = one base-layer write | Unrefusable by the compositor; the "only recovery was `kill -KILL` from another machine" class (#436) closes |

Three requirements on the supervisor come out of the 2026-09-05 Steam measurements and the v1 ops record:

- **Invisible prompts are a stall class of their own.** The WSI-layer failure above asked its question through a `zenity` dialog. Under gamescope that is an untagged window nobody can see, and the client blocked for eight minutes at 0% CPU behind it — a hang with no log line and no visible cause. The supervisor must **tag or auto-dismiss unknown windows from a known process family** (so an unexpected dialog becomes visible or disappears rather than blocking), and set `GAMESCOPE_ZENITY_DISABLE=1` in the launch environment so a layer failure exits loudly instead of hanging black.
- **Only one supervisor may hold restart authority on the box.** The `htpc_common` Ansible role ships a **CEC watchdog** as a *system* unit that probes `cec-health`, reads a deliberately stopped `tv-shell-input` as a wedged adapter, and restarts it — re-grabbing the pad and silently undoing input fixes (jedwards1230/homelab-ansible#266). v2 introduces its own supervisor with restart authority, so at cutover the two would contend on one box. The watchdog must **stand down at cutover**, and its health probe must distinguish a stopped unit from a wedged adapter. This is unfiled on the tv-shell side.
- **Sunshine and Steam Remote Play must not target the same streaming host at once.** When both did, Sunshine held the host running its own "Steam Big Picture" app and Remote Play faithfully captured *that*, so the host's Big Picture appeared instead of the game — for an hour, looking exactly like a capture bug. The kit already refuses a Moonlight stream against a busy host and names the running app; nothing does so on the Steam side. The supervisor owns this **interlock** — check the host's session before starting either client, and refuse with the running app named — rather than leaving it to whoever is on the couch.

**Steam owns `GAMESCOPECTRL_BASELAYER_APPID` while it runs, and the core must be designed for that, not against it.** The measurement above is not drift the core can win: Steam rewrote the atom on every stream start and stop and dropped our id from `GAMESCOPE_FOCUSABLE_APPS`. The core therefore **reconciles after Steam's writes** — reading the list back as its last intent (as the table's "Core dies" row already requires) and re-asserting only when it has an intent of its own to express — and never expects to hold the atom while Steam runs. Steam is an active adversary on that atom, not a source of drift.

The panel becomes the recovery and observability surface for the supervisor (unit states, heartbeat, last assertion failures, base-layer list, Xwayland servers, escape hatches), with its recovery tier still independent of the core; its unit names become config so the v1 and v2 panels can coexist.

## 10. Verification

**CI, headless.** `gamescope --backend headless` in a container with a software Vulkan device (lavapipe), Xwayland, and the pinned gamescope built from tag; no seat, no `/dev/uinput`, so the core runs with input disabled. Scripted X11 clients (`xprop`, the kit's `focus.sh`/`launch.sh`) exercise the whole contract: scope resolution (an untagged client in an `app-steam-app*` scope must appear in `GAMESCOPE_FOCUSABLE_WINDOWS` with that id), tag repair, base-layer ordering, app-exit fallback and the hysteresis, overlay focus, per-app Xwayland create/destroy, and the core's atom round-trips. Fixtures are real compositor bytes, not hand-written shapes (the #448 lesson). Whether headless emits stats lines and honours `GAMESCOPE_CREATE_XWAYLAND_SERVER` is unverified (§13 Q10); until the job passes, verification is the live bench only and the doc says so.

**Field assertions** in the running core, **polled, not event-driven** (the v1 residual defect was an attached listener that processed nothing), exported as metrics and on `GET /status`, alerting on any non-zero:

| Assertion | Reads |
|---|---|
| No untagged core-launched toplevel | launch table vs `GAMESCOPE_FOCUSABLE_WINDOWS`, filtered as gamescope filters (override-redirect, 1x1 and skip-taskbar windows excluded) |
| Base window equals intent | `GAMESCOPE_FOCUSED_WINDOW`'s app id == first mapped id of the list the core last wrote |
| Map events seen vs windows seen | a dead listener is visible as a widening gap |
| Frame heartbeat advancing | forced-paint probe |
| HDR atom expected | `GAMESCOPE_HDR_OUTPUT_FEEDBACK` == configured, outside the hotplug settle |
| Grab state matches visibility | grab armed ⇔ shell or overlay is the focus window |
| Exactly one shell, one core | unit MainPIDs |

**The measurement kit is the regression bench.** `dev/gamescope/measure.sh`, `focus.sh`, `launch.sh` and the stats reader stay in the tree as the live 4K120 HDR check after every gamescope or kernel bump, with the §6 pass table plus new rows: `STEAM_OVERLAY` over a 4K120 HDR stream (`fps=` and Moonlight's dropped-frame counter), a static home screen under VRR for five minutes (heartbeat lines per minute), app-exit fallback timing (observed working under the prototype 2026-09-05: closing Moonlight returned cleanly to the prototype shell), a window on a secondary Xwayland server getting the HDR bypass, and keys routed across servers. Every deployable artifact reports a real version (shell and core included); `tv_shell_build_info` reads git live and is not a restart signal.

## 11. Deploy and migration

- **Beside, not instead, at every shared layer.** v2 has its own session entry, install prefix and git clone (a `/dev/deploy` of a v2 branch must not replace v1's `shell/`), its own config file (the v1 daemon's `config.toml` root is `deny_unknown_fields`, so a new v2 table would abort v1 at startup), its own core binary and unit, and a panel unit whose managed unit names are config. Only one session runs at a time, so the socket and ports do not collide. Cutover includes "deploy v2, select v1, confirm v1 boots".
- **Hot git deploy stays**: `/dev/deploy` → `/dev/build` → unit restart → screenshot. Screenshots move to gamescope's `gamescope_control.take_screenshot` (grim's protocol is not served), which makes the core a Wayland client of `GAMESCOPE_WAYLAND_DISPLAY`; the same client serves `gamescope-action-binding` (§7).
- **Fix the Ansible pin first** (jedwards1230/homelab-ansible#320): the role pins a pre-workspace-model daemon and downgrades the running one on any run. The v2 core gets its own release stream and pin. Packaging (#144, #147) remains the end state for install, upgrade and rollback.
- **gamescope pinned ≥ 3.16.28** by the deploy role; 3.16.23 is not representative (§13 Q5). The headless CI compositor pins the same version.
- **`--expose-wayland` breaks the WSI layer for every Steam-runtime app, and the fix is per-app.** gamescope hands children `WAYLAND_DISPLAY=gamescope-0`; pressure-vessel (the Steam runtime container) preserves only `wayland-*` socket names, so it rewrites that to `wayland-0` and rewrites `GAMESCOPE_WAYLAND_DISPLAY` to an absolute socket path. The layer's `isRunningUnderGamescope()` gate can then never match, so it does not load: no HDR for any Steam-runtime app, and with dialogs enabled a black screen. Measured 2026-09-05. **The per-app fix is sufficient and was proven** — launching Steam with `WAYLAND_DISPLAY` unset worked with `--expose-wayland` still on and no session restart — so v2 keeps the flag and unsets the variable in the launch environment for that app class. Do **not** chase this with gamescope master: master's bd52d6e sets `WAYLAND_DISPLAY=""`, which pressure-vessel turns back into `wayland-0`, recreating the breakage.
- **New config** (`config.toml.example` rows and `daemon_config.rs` structs land with the core): `[display]` width/height/refresh, HDR default, SDR nits, hotplug settle; `[session]` shell app id, Xwayland count; `[supervisor]` stall seconds, restart thresholds, short-session window/count; `[av]` AVR host/port, input code, zone-2 policy, TV MAC/broadcast, webOS host and key file.
- **Cutover criteria**: §6 table green on the pinned build, driven through the runtime atoms, including the eyes-only rows and the new bench rows; field assertions at zero and heartbeat false positives at zero for seven consecutive days of normal use; every PRD §3 journey reproduced; a Moonlight session, a Plex session and a web app each survive a shell restart underneath; the §8 site preconditions verified; v1 still boots after a v2 deploy.
- **Rollback** is selecting the v1 session at the display manager, by hand or by the short-session hook.

## 12. Streaming clients and app classes

**Two streaming clients are first-class, permanently supported, and independently testable.** The user streams over Moonlight today and expects to move to Steam Remote Play as its features catch up; Moonlight stays in the repo regardless. Both sit under one contract: a window resolved by scope or tagged by pid, one base-layer switch, HDR through the X11 WSI path, audio ownership by the base window, the `gamepad` input contract.

| | Moonlight | Steam Remote Play |
|---|---|---|
| Status under gamescope | **Proven 2026-09-05**: xcb, tag-by-pid on the stream window (its second X window, K6), HDR10 swapchain exposed to the client, 120 fps alone and with the shell over it | **Measured 2026-09-05**, Big Picture flavour: it runs and renders. Two real streams under the kit; the stream window is tagged (Steam tags it with the *game's* app id, and the kit defers via `--keep-existing`), 120 fps held, the pad reached the game. **SDR, not HDR** — Valve's client declines the HDR the compositor offers (below). Steam owns `GAMESCOPECTRL_BASELAYER_APPID` outright. Steam Link remains unmeasured (not installed on the box; only the kit's "not installed → exit 2" path is exercised) |
| Flavours | one | (a) the full Steam client in Big Picture as a tagged app, carrying the `streaming_client` window (`STEAM_STREAMING_CLIENT=1`, always a focus candidate under SteamControlled); (b) the standalone Steam Link client, a plain SDL app with no shell-role contention, HDR on Linux unverified |
| Known risks | check the host's `<currentgame>` before launching (a busy host shows an invisible dialog) | (1) SteamOS 3.9's session exports `GAMESCOPE_DISPLAY_DISABLED=1` for `streaming_client` "until buffer frozen issues are understood better", and gamescope#2196 (flicker, unpredictable input under `--steam`) is open. (2) The full Steam client under `-e` expects to be app 769 and writes `GAMESCOPECTRL_BASELAYER_APPID` itself when a stream starts, so it may fight our shell for the base layer (`steamos-39-gamescope-shell.md` §2, §6) — **measured 2026-09-05: it does, and it wins.** The kit wrote `9004,769,9001`; Steam replaced it within seconds and rewrote it on every stream start and stop (`[413091, 769]` → `[413091, 252950, 769]` → `[413091, 769]` → `[413091, 3321460, 769]`), dropping the kit's own id from `GAMESCOPE_FOCUSABLE_APPS` entirely. (3) **Remote Play is SDR by Valve's gate.** The WSI layer hooked `streaming_client` and logged `server hdr output enabled: true` / `hdr formats exposed to client: true`; the client created a `VK_FORMAT_B8G8R8A8_UNORM` / `VK_COLOR_SPACE_SRGB_NONLINEAR_KHR` swapchain on the next line. HDR was offered and declined — no compositor, flag, pin or kernel changes it. Moonlight remains the HDR path. (4) `--expose-wayland` silently disables the WSI layer for every Steam-runtime app (§11); run Steam with `WAYLAND_DISPLAY` unset. (5) Sunshine and Remote Play must not target the same streaming host at once (§9 interlock) |
| Gate | kit criteria 3 and 8 (pass) | **kit criterion 10**: stream shown, **HDR on the TV, or SDR recorded as such**, 120 fps, pad reaches the game. Two of the five conditions as first written are now disproven rather than merely unmet: HDR is unreachable on this client (a Valve-side gate, below), and "no base-layer fight" is dead — Steam wins that atom, so it is a design requirement on the core (§9), not a pass condition. It still decides which flavour is supported and is §13 Q3 |

Other app classes:

| Class | Launch | Id | Notes |
|---|---|---|---|
| Plex HTPC (native) | own server; `keyboard` contract | scope | Keep its CEC disabled; it runs under `bwrap`, which must keep the host pid namespace or scope and pid both fail |
| Chromium `--app` web apps | own server, own `--user-data-dir`/`--class`; `keyboard` contract | scope, class fallback | §13 Q4 |
| Home Assistant, music streaming | later, as plugins | | §13 Q2: requirements only |

Plugin requirements (mechanism undecided): a plugin declares an app class, a launch command, an id strategy (scope, pid, or class), an input contract (`gamepad` / `keyboard`), an HDR expectation, and optional home-widget manifests; it never writes compositor atoms itself.

### Fallback considered

If criterion 10 fails, or if a Steam-first future makes a custom shell not worth its cost, the fallback is a **Bazzite/ChimeraOS-style `gamescope-session` with Steam Big Picture as the shell**. It buys Remote Play, Steam Input, the overlay and HDR for free, with Moonlight and Plex as non-Steam shortcuts. It costs what this design exists to keep: our shell as a peer of apps, native Plex and web apps, and daemon-owned AV control, which would move into a sidecar or a Decky-style plugin. It is a real path, not a strawman, and criterion 10 is where it is chosen. **Criterion 10's Big Picture half passed on 2026-09-05**, so this fallback is not selected; it stays on the page as the named alternative should a Steam-first future make a custom shell not worth its cost. Either way the ChimeraOS session files are reused (§4).

## 13. Open questions

1. **Shell runtime: Quickshell on xcb, or a plain Qt Quick application?** Whichever it is must set X11 properties on its own windows before map (a C++ helper or plugin) and split drawer, QAM and toasts into separate toplevels; Quickshell's layer-shell window is unusable here and its xcb operation is unverified. The plain `qml` runtime (Qt 6) worked as the prototype shell at 4K120. Porting cost of `shell/` versus a rewrite is the trade.
2. **Plugin mechanism** for Home Assistant and music streaming: manifest-driven `.desktop` extensions, a core-side registry, or out-of-process plugins over the IPC.
3. **Steam Remote Play under gamescope — the Steam Link half (kit criterion 10).** Partly answered 2026-09-05: **Big Picture as a tagged app works**, in its SDR form. The stream window is tagged (with the game's app id, by Steam itself), 120 fps held, input reached the game, and the base-layer question is settled against us (§9). What is still open is the **standalone Steam Link client**, which is not installed on the box, so only the kit's "not installed → exit 2" path is exercised: whether it is worth supporting beside Big Picture, and whether its HDR on Linux is any better than Big Picture's — the latter is capped by a Valve-side gate on `streaming_client`, so the standalone client is the only remaining place an HDR Remote Play could come from. The consequence of the SDR result is the live question: Remote Play is an SDR path for as long as Valve's client declines the HDR swapchain, which is a product decision (Moonlight stays the HDR path) rather than a compositor one. gamescope#2196 and the v1 finding that Steam's capture-window selection is a Valve-side bug stay open. A failure of the remaining half narrows the supported flavour; it does not select the §12 fallback, since Big Picture passes.
4. **Chromium: Xwayland or native Wayland, and per-app profiles.** Native has no focus selector under SteamControlled; Xwayland is the safe path but hardware decode and Widevine under Xwayland on this GPU are unmeasured, and the per-app `--user-data-dir` split costs shared logins.
5. **gamescope pin and packaging.** Arch's package at the 3.16.28 tag, a built tag, or tracking master for content-driven HDR (commit 6513879, not in any tag).
6. **Kernel colour pipeline.** Stay on the stock kernel (composite path, measured fine for a switch) or ship one with `CONFIG_AMD_PRIVATE_COLOR` for direct scanout; decide after the overlay-over-HDR bench row and the TV panel's bit-depth reading.
7. **CEC driver for the sidecar.** Kernel `pulse8-cec` via a `cec-rs` replacement or a small `cecd`-style daemon; whether the adapter stays in the chain at all if the AVR's push events prove sufficient.
8. **Privilege model.** The panel's exec tier, the short-session fallback (a root-owned display-manager file), and the pacman path: sudoers allowlist (v1) or polkit + a system D-Bus helper (SteamOS shape). PRD §4 lists display-manager setup as a non-goal; the fallback hook either lives in deployment or amends that.
9. **Cause of the launch-coincident hotplug**: the AVR, an audio infoframe renegotiation at stream start, or the v1 CEC lifecycle. Discriminating runs: CEC off; Moonlight audio disabled. The relaunch-once policy waits on the answer.
10. **Headless CI feasibility** on a hosted runner: lavapipe acceptance, stats emission, `GAMESCOPE_CREATE_XWAYLAND_SERVER` under headless.
11. **VRR default**: on, off, or per-app, given the OLED near-black flicker and AVR OSD notes in the ops record.
12. **Core name and repo layout**: whether `daemon/` evolves in place or a new crate is created beside it while v1 is kept buildable; the same for a v2 `shell/`. **Answered 2026-09-06 for the core half**: a NEW crate, `core/` (`tv-shell-core`), beside `daemon/`, with v1 (`daemon/`, `host/`, `protocol/`, `panel/`) untouched and still building. Evolving `daemon/` in place was never viable at the config layer — its root is `deny_unknown_fields`, so a v2 table in `config.toml` aborts v1 at startup — so the core takes its own file, socket and units (§11). The **v2 `shell/` half is still open**, and waits on Q1's runtime decision.
13. **Screenshot fidelity under HDR** through `gamescope_control` (`screen_buffer` type) versus a WSI-side capture.

## 14. Decision log

| Date | Decision | Where |
|---|---|---|
| 2026-05-24 | Hyprland + Quickshell layer-shell; gamescope rejected for "no layer-shell" | PRD §5 |
| 2026-07-04 | Kiosk model on Hyprland: declarative isolation + daemon hardening; gamescope re-rejected | #307, #308 |
| 2026-08-29 | Nested per-app gamescope rejected; SteamOS itself rejected | "TV Shell vs SteamOS" artifact |
| 2026-09-04 | v2 beside v1; one Rust core, shell as ordinary client; in-repo supervisor with frame heartbeat (reverses PRD §12.5); grab only while shell visible; headless CI + field assertions; keep hot deploy, fix the Ansible pin; panel in scope; plugins later; IP is the AV authority with a kernel-CEC observer, libcec goes; theater sleep only when the HTPC owns the display; TV remote is a goal | memory note; #453; homelab-ansible#318 |
| 2026-09-04 | Compositor gated on a one-week gamescope measurement; fail rule: 10-bit HDR at 4K120 or composite cost | `dev/gamescope/README.md` |
| 2026-09-05 | Phases 1–3 pass; bit depth unmeasurable on this kernel; bare `gamescopectl <convar>` resets the convar; Moonlight must be xcb; tag by pid | #454, `gamescope-hdr-feedback.md` |
| 2026-09-05 | **gamescope is the v2 compositor**, pin ≥ 3.16.28; Hyprland stays v1-only; Smithay dropped; SteamOS unit shape and base-layer contract adopted; wiki writes held until this document exists | this document |
| 2026-09-05 | Moonlight and Steam Remote Play are both first-class and permanently supported; Remote Play under gamescope is kit criterion 10 and gates the flavour (Big Picture vs Steam Link); a Steam-as-shell `gamescope-session` is the named fallback; the shell's app id is private, 769 stays Steam's | this document, `dev/gamescope/README.md` |
| 2026-09-05 | **Kit criterion 10 measured.** Steam Remote Play runs under the prototype: Big Picture as a tagged app passes in its **SDR** form (120 fps, pad reaches the game, stream window tagged with the game's app id and deferred to). Remote Play is **SDR by a Valve-side gate** — the client declines the HDR swapchain the compositor offers — so Moonlight stays the HDR path. **Steam owns the base-layer atom** and rewrites it per stream start and stop; the core reconciles after Steam rather than contending. Steam Link is **unmeasured** (not installed). `--expose-wayland` disables the WSI layer for Steam-runtime apps; the per-app `WAYLAND_DISPLAY`-unset fix is proven and the flag stays. The decision rule (criteria 1 and 3) is untouched and still passes — gamescope remains the v2 compositor | this document, jedwards1230/tv-shell#458 |
| 2026-09-05 | Review findings folded in: `app-steam-app` scope prefix is the upstream contract and the primary id; shell self-tags; the core is stateless and reads the base-layer list back on restart; v1 and v2 share no config file, prefix or unit name; the heartbeat is a forced-paint probe with one FIFO reader; `GAMESCOPE_FOCUSED_WINDOW` not `_APP` is the truth under an overlay; presenters collapse to `gamepad`/`keyboard` with persistent uinput devices | this document |
| 2026-09-06 | **§13 Q12 answered for the core**: a new crate `tv-shell-core` in `core/`, beside `daemon/`, not an evolution of it — v1 (`daemon/`, `host/`, `protocol/`, `panel/`) is untouched and still builds, and the two share no config file (`core.toml`, since v1's root is `deny_unknown_fields`), socket (`tv-shell-core.sock`) or unit name. The crate lands with the typed atom layer, `ScreenState`, scope launching, base-layer show/home and the IPC server; input, CEC, the network surfaces, the heartbeat and the v2 `shell/` are not in it | this document, `core/README.md` |

## 15. References

Repository: `docs/PRD.md`, `docs/KIOSK_WINDOW_MODEL.md` (v1 model, historical), `docs/INPUT_AND_STATE.md`, `docs/IPC_PROTOCOL.md`, `docs/CONTROL_SURFACE.md`, `docs/OBSERVABILITY.md`, `docs/PANEL.md`, `docs/SYSTEMD_SETUP.md`, `dev/gamescope/README.md`; PRs jedwards1230/tv-shell#191, #352, #444, #448, #453, #454; issues #383, #402, #436, #455; jedwards1230/homelab-ansible#318, #320.

Research reports (outside the repo): `arch-map.md`, `gh-patterns.md`, `git-churn.md`, `ops-surface.md`, `history-sweep.md`, `htpc-stream-postmortem.html`, `gamescope-eval.md`, `cec-history.md`, `gamescope-live-measurements-2026-09-05.md`, `gamescope-hdr-feedback.md`, `steamos-39-gamescope-shell.md`, `steamos-history.md`, `tv-shell-vs-steamos-artifact.html`, `pr453-review-fixes.md`.

Upstream:

- gamescope, tags 3.16.23–3.16.28 and master: https://github.com/ValveSoftware/gamescope — `src/steamcompmgr.cpp` (focus policy, atoms, HDR feedback, stats cadence), `src/Backends/DRMBackend.cpp`, `src/Backends/HeadlessBackend.cpp`, `layer/VkLayer_FROG_gamescope_wsi.cpp`, `src/Utils/Process.cpp` (the `app-steam-app%u-%d.scope` parser), `src/convar.h` + `src/Apps/gamescopectl.cpp` (the convar reset), `protocol/gamescope-control.xml`, `protocol/gamescope-action-binding.xml`; issues #1887, #2075, #2196, #2261, #2051
- SteamOS session package `gamescope-3.16.26-2` (units, `gamescope-session`, `steam-launcher`, short-session tracker): https://github.com/Jovian-Experiments/PKGBUILDs-mirror/tree/jupiter-main/gamescope-3.16.26-2 ; SteamOS Manager: https://github.com/evlaV/steamos-manager ; ChimeraOS: https://github.com/ChimeraOS/gamescope-session
- Steam Input udev rules: https://github.com/ValveSoftware/steam-devices/blob/master/60-steam-input.rules ; InputPlumber: https://github.com/ShadowBlip/InputPlumber ; ds-inhibit: https://gitlab.com/evlaV/ds-inhibit
- Moonlight HDR decision: https://github.com/moonlight-stream/moonlight-qt/blob/master/app/streaming/video/ffmpeg-renderers/plvk.cpp
- AMD private colour properties: https://melissawen.github.io/blog/2025/05/19/drm-info-with-kms-color-api
