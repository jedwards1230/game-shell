# tv-shell — Product Requirements Document

> **Status:** source of truth for the intended end state · 2026-08-22 · repo: [`jedwards1230/tv-shell`](https://github.com/jedwards1230/tv-shell) (public, GPL-3.0)
>
> This document describes what tv-shell **is meant to be, fully realized**. It is not a task list — open work lives in GitHub issues, and §10 links the two. Where a claim is about today rather than the end state, it says so.

## 1. What it is

tv-shell is a **couch console**: a 10-foot, controller-driven shell that turns a small Linux box wired to a TV into an appliance. It boots straight into its own Wayland session, owns the gamepad exclusively, launches game streams (Moonlight/Sunshine), local apps and web apps full-screen, and controls the AV chain over HDMI-CEC. It is a Quickshell (QML) UI on Hyprland, backed by a Rust daemon that owns input, CEC, state and every machine-readable control surface.

It exists because the off-the-shelf option was tried and rejected. Phase 0 of the project deployed **Plasma Bigscreen**; after a day of testing it failed on three counts: KWin killed Moonlight's Wayland connection every 2–7 minutes (`wp_linux_drm_syncobj_surface_v1 error 3`), full Plasma was a heavyweight stack for a streaming box, and **three separate processes read the gamepad simultaneously with no exclusive grab**. tv-shell replaces that stack — Bigscreen, KWin, `plasma-bigscreen-inputhandler`, an `evtest`-based controller-wake daemon and a Moonlight watchdog all collapse into one compositor config, one QML shell and one Rust daemon.

A second, equally load-bearing design goal came later: **the shell must be drivable by a machine**. Every action a human can take from the couch is reachable over a Unix socket, an HTTP bridge, an MCP server, or MQTT — so an AI agent can deploy a branch, drive the UI, screenshot the result and verify it, and so Home Assistant can treat the box as a first-class device.

## 2. Problem

A dedicated TV box has requirements a desktop session structurally cannot meet:

- **Exclusive input.** A gamepad is the only input device. If more than one process reads it, buttons double-fire, the shell navigates behind a running game, and "back" is ambiguous. Nothing in a desktop stack arbitrates this.
- **One window, always full-screen.** Two visible windows on a TV is a bug, not a layout. Tiling, floating and window decoration are all wrong answers.
- **The screen is shared with an AV chain the computer doesn't own.** A receiver and a TV must be woken, switched and put back to sleep, over a bus (CEC) that is unreliable and partially one-directional.
- **There is no keyboard and no mouse.** Any flow that needs text entry (adding a Wi-Fi network, a web app URL, a stream target) has no on-screen affordance by default.
- **No one is sitting at the machine when it breaks.** A wedged compositor or a dead daemon on a headless TV box means a black screen and a trip to a keyboard. Recovery has to be possible from elsewhere on the LAN.
- **The maintainer is increasingly an AI agent.** A shell that can only be operated by a human at 10 feet cannot be iterated on, tested, or verified by an agent.

## 3. Users & core workflows

| User class | Who / what | Surface they use |
|---|---|---|
| **Couch user** | Someone on the sofa with a gamepad | The QML shell on the TV |
| **Operator** | The same person, at a laptop, when something is wrong or needs typing | `tv-shell-panel` web UI on the LAN |
| **AI coding agent** | Claude Code and similar, iterating on the shell itself | MCP server (`/mcp`), HTTP bridge, screenshots |
| **Home automation** | Home Assistant, via MQTT and the HTTP bridge | Retained state topics, command topics, `GET /status` |
| **Contributor** | Human or agent writing QML/Rust | Repo, CI gates, `docs/` |

**Core journeys**

1. **Wake and stream.** Press Home on a cold gamepad → the box wakes the AV chain, claims the display, shows the home screen → pick a stream target → Moonlight launches full-screen with the physical pads handed off to the game.
2. **Escape from anything.** Meta-hold returns to the shell from any running app or stream; a four-button combo force-quits; a three-button combo suspends a stream. These work in every state and survive the input grab being released.
3. **Launch a local or web app.** Home rail → a `.desktop` app or a registered web app (Chromium `--app`) launches full-screen, tracked by window class, resumable from the drawer.
4. **Change a setting from the couch.** Twelve settings pages reachable by D-pad, all persisted by the daemon into one `settings.json`.
5. **Recover from the LAN.** Daemon wedged or shell black → open the panel in a browser → restart a unit, read journal logs, redeploy a branch, apply system updates, upload a wallpaper. The panel is designed to still work when the daemon does not.
6. **Agent iterate loop.** Agent calls `dev_deploy` → `dev_build` → `restart_shell` → `take_screenshot` → reads the pixels → repeats. Observe, act, verify, without a human at the TV.
7. **Home Assistant integration.** The box appears as one HA device with retained state, per-entity availability tiers, and buttons that fire real intents.

## 4. Goals / Non-goals

**Goals**

1. A 10-foot UI where **every** interactive element is reachable by D-pad and activatable with A; B always goes back.
2. **Exclusive, arbitrated gamepad ownership** with an explicit presenter model — the shell, a keyboard-style app, a game with a virtual twin, or a raw handoff to a streaming client.
3. **One app visible, always full-screen**, enforced declaratively by the compositor and by a single actor in the daemon.
4. **Declared, never inferred, capability.** Nodes announce what they can do; clients build their surface from the answer instead of probing.
5. **Every couch action is machine-drivable** over socket, HTTP, MCP and MQTT, with the same action logic behind all four.
6. **An out-of-band recovery path** that does not depend on the thing being recovered.
7. **Secure by default on a LAN**: token-gated network surfaces, secrets by reference only, fail-closed on an insecure non-loopback bind.
8. **Site-neutral source**: no site's addresses, hostnames or device identities appear as literals in code. AV endpoints, node addresses and the panel's deployment target are all configuration.
9. **Self-sufficient AV lifecycle**: the box can wake, claim, and release the display chain on its own, without an external automation platform.
10. **Installable the standard way for its platform** — a package, not a clone-and-run script.
11. Signal emitted in standard formats (journald, Prometheus) so any collector can consume it.

**Non-goals**

| Not doing | Why |
|---|---|
| Being a desktop, or supporting multiple visible windows | The kiosk invariant is the product |
| QML build tooling — bundler, compiler, package manager | Files deploy as-is; hot-deploy by `git pull` is the dev loop |
| Embedding a web engine (QtWebEngine) | Quickshell ships none; Widevine + hardware decode come free from a system Chromium |
| Collecting or forwarding its own telemetry | The repo emits signal; collection is deployment-private |
| Configuring autologin / display-manager setup | Site-specific, deliberately left to the installer |
| **First-run onboarding** — a guided setup wizard | Explicitly out of scope; it serves a user the project does not have, and the hardware-verification bottleneck cannot sustain the support surface it implies |
| Carrying homelab-specific host identity, service names or addresses | Repeatedly rejected — the repo is public and site-neutral |
| A `net-wifi-connect` IPC command | Network reads are first-class; joining stays a shell-out |
| Waking a machine over MQTT | A command topic cannot be actioned by a machine that is off; that is WoL's job |
| A Windows build of the panel | Sidecar nodes are served remotely instead; nothing plans a non-Linux *shell* node |
| Splitting the QML shell and the daemon into separate repos | They are bound by a private versioned IPC protocol and version as a unit |
| Screenshots over MQTT | Retained PNGs bloat the broker; they stay on the HTTP bridge |
| A general 10-foot on-screen keyboard | Only the flows that strand a user mid-use get one (see §5) |

**What "distribution agnostic" does and does not mean.** It means **no site identity in source** and no dependency on any particular configuration-management tool. It does **not** mean OS-neutral: the panel's recovery tier is systemd-specific by design, and its system-update tier is pacman-specific. `CLAUDE.md`'s "no knowledge of specific infrastructure, deployment tools, or host management" overstates this and should be narrowed to match — the panel manages systemd units and applies `pacman -Syu` today. Packaging (§5) turns that coupling into a **declared platform target** rather than an unstated assumption.

## 5. Locked product decisions

| Decision | Choice | Why |
|---|---|---|
| Compositor | Hyprland, kiosk config, shell as a `wlr-layer-shell` surface | Best Quickshell support; QML IPC module; rejected gamescope (no layer-shell), Cage, labwc/Sway |
| UI toolkit | Quickshell (QML), no build step | Lightweight vs full Plasma; hot-deployable |
| Backend language | Rust, one workspace, four crates | Replaced an earlier Python evdev daemon |
| Shell layer | **Overlay**, unmapped when an app owns the screen | Hyprland renders fullscreen windows above the Top layer; on Top the shell mapped *under* the app while stealing exclusive keyboard focus |
| Screen ownership | **Declared, never inferred** — `shell-focus on\|off` pushed to the daemon, re-asserted on a ~3 s heartbeat | The compositor cannot answer "should the shell be visible" |
| Input arbitration | Presenter state machine: Shell / Keyboard / Game / Handoff; only Handoff drops `EVIOCGRAB` | Games need the real evdev node; a keyboard-style app needs neither a grab nor a virtual twin |
| Meta button | **Tap belongs to the app, hold belongs to us** (`[input].meta_hold_ms`, default 500) | Guide-press must reach games; the universal escape must always reach the shell |
| Meta keycode | Socket-only; deliberately **not** mapped to `KEY_HOMEPAGE` | That keycode leaks to focused apps |
| IPC framing | Bare newline-delimited UTF-8 text over `AF_UNIX`, mode `0600`; JSON only ever as a body | Trivial to drive from `nc`, `socat`, a shell script or an agent |
| IPC auth | Filesystem permission only | Owner-only socket is the shell↔daemon contract |
| Network auth | One bearer token for HTTP + MCP, a **separate** one for the panel, a **third** per sidecar; secrets by reference (`*_token_file`, 0600, confined to the config dir) | A token that works on one port must 401 on another |
| Insecure bind | Both binaries **refuse to start** on a non-loopback bind with dev tools on and auth off, unless `[dev].allow_insecure_lan` | One insecure-LAN opt-in per node to audit, not two |
| Capability gating | A gated-off panel route **is not registered — it 404s**, and the nav is built from the same gates | A hidden nav link with a live route behind it is not a gate |
| Failed handshake | Empty feature set ⇒ recovery mode, loudly. Never fail open | The panel exists to recover a broken node |
| Where the panel runs | **On** a shell node (the exec tier is local); a **sidecar** node is served remotely over HTTP | You cannot `systemctl restart` a hung unit from another machine |
| Config | Typed `config.toml`, `deny_unknown_fields`, no reload path | A typo aborts startup instead of silently disabling a feature |
| `settings.json` | The **daemon is the sole writer**; QML and panel go through `get-config`/`set-config` | One RMW path under a mutex; a JSON `null` deletes a key |
| Web apps | Chromium `--app` + generated `.desktop` + `StartupWMClass` | Widevine and hardware decode for free; reuses the existing app-discovery and window-matching path |
| Text entry today | The **panel** is the add surface for anything needing a keyboard | The couch UI has no on-screen keyboard yet |
| CEC scope in-repo | libcec statically linked behind `--features cec`; no site-specific helper scripts invoked | Keeps homelab identity out of a public repo |
| Site config vs source | **AV device addresses, node addresses and the panel's deployment target are configuration, never literals in code.** No hostname, IP or MAC of any deployment appears in a non-test source path | The repo is public; site identity has been rejected from it three separate times |
| AV lifecycle owner | **The daemon owns IP-based AV control** alongside CEC — receiver power/zone control and display wake, driven from typed config | The box must be self-sufficient for AV; an external automation platform is not a dependency |
| Panel topology | **A fleet console with a node switcher.** Every shell node also runs its own local panel as the recovery path of last resort | The exec tier is local; a remote console cannot restart a hung unit |
| Text entry | **A narrow on-screen keyboard** for flows that strand a user mid-use (Wi-Fi password, stream target) — not a general keyboard | A fresh install must be able to join a network without a second device; nothing more |
| Wedge recovery | **Sensor in the daemon, actuator outside** — export a frame-presentation counter; let external automation decide to act | An actuator that fires wrongly kills a live game; the daemon cannot see enough context |
| Doctrine | **The daemon reports; the caller decides** — no `busy` boolean, no auto-suspend on unknown | Policy belongs to the automation, not the device |
| Versioning | Per-artifact tag streams (`input-v*`, `host-v*`, `widget-<id>-v*`); the tag *is* the version, stamped into `Cargo.toml` at build | Shell and panel ship from git and carry no version |

## 6. Product surface

### 6.1 Processes and binaries

| Binary | Crate | Runs on | Role |
|---|---|---|---|
| `tv-shell-input` | `daemon/` | The TV box (Linux only) | Input grab, IPC, CEC, MQTT, HTTP bridge, MCP, D-Bus actors, metrics |
| `tv-shell-panel` | `panel/` | Beside the daemon (Linux/macOS) | LAN web control panel + recovery |
| `tv-shell-host` | `host/` | The gaming PC (Linux/macOS/Windows) | Steam enumerate/launch/quit/sleep sidecar |
| *(none — interpreted)* | `shell/` | The TV box | The QML UI, run by `quickshell -c tv-shell` |
| *(library)* | `protocol/` | — | Shared wire types: `Capabilities`, `Feature`, MQTT envelope, `/library` types, brand/env shims |

None of the binaries takes command-line flags. Configuration is `~/.config/tv-shell/config.toml` plus `TV_SHELL_*` environment variables (each with a legacy `GAME_SHELL_*` fallback); `RUST_LOG` is the one env var that stays an env var by design.

### 6.2 IPC — the shell↔daemon contract

Unix socket at `/run/user/$UID/tv-shell-input.sock` (override `TV_SHELL_SOCK`), `SOCK_STREAM`, mode `0600`, newline-delimited text. One command per line; `subscribe` holds the connection open and streams events. Replies are `ok`, `subscribed`, a compact JSON body, `unknown`, or `error:<detail>` — with `error:input-runtime-down` a distinct, actionable reply that is never conflated with `unknown`.

| Domain | Commands |
|---|---|
| Screen & presenter | `grab` · `release` · `handoff` · `shell-focus on\|off` · `overlay-focus on\|off` · `shell-state <json>` · `status` · `subscribe` |
| Controllers | `get-pads` · `list-input-devices` · `get-bindings` · `set-binding <action> <button>` · `capture-next` · `capture-cancel` · `set-active-game <id>` · `pad-battery <id>` · `pad-rumble-status <id>` · `rumble <id> <ms>` · `controllerdb-status` · `controllerdb-refresh` |
| Control surface | `intent <name>` (broadcast only, touches no device) · `key <name>` (synthesizes a real keystroke) |
| Apps & web apps | `list-apps` · `record-launch <json>` · `get-recents` · `webapp-list` · `webapp-add <json>` · `webapp-remove <id>` |
| Settings | `get-config` · `set-config <json>` |
| Notifications | `get-notifications` · `record-notification <json>` · `set-notifications <json>` |
| System | `sys-status` · `storage-status` · `sys-metrics` · `build-info` · `capabilities` |
| Bluetooth | `bt-power-status\|-on\|-off` · `bt-scan-on\|-off` · `bt-list` · `bt-connect\|-disconnect\|-pair\|-trust <mac>` |
| Network (read-only) | `net-status` · `net-wifi-list` · `net-wifi-rescan` · `net-throughput <iface>` · `net-ping <host> [count]` · `wol <host>` (stateless wake of a configured host; served directly from the dispatcher, like `sunshine-status`) |
| Power | `power-can-suspend` · `power-suspend` · `power-battery` |
| Compositor | `hypr-active` · `hypr-clients` · `hypr-monitors` |
| HDMI-CEC (`--features cec`) | `cec-scan` · `cec-device <addr>` · `cec-power-on\|-off <addr>` · `cec-active-source` · `cec-health` · `cec-test` |
| Streaming & media | `sunshine-status <host> <port>` · `plex-hubs` · `moonlight-forget <host>` |
| Steam (proxied to the sidecar) | `steam-library` · `steam-hosts` · `steam-set-host <name>` · `steam-launch <appid>` · `steam-quit <appid>` · `steam-bigpicture` · `steam-suspend` |

**Events** (after `subscribe`): controller lifecycle (`controller-wake`, `pad:connected\|disconnected\|index\|battery`), `intent:<name>`, combos (`combo:end-session`, `combo:force-quit`, `combo:suspend-stream`), `input-mode:controller\|mouse`, `bt:*`, `net:*`, `power:battery`, `hypr:*`, `cec:device\|power\|health`, `config:changed`, `health:<json>`.

**Intent vocabulary** (the single closed control language, shared by socket, HTTP, MCP and MQTT): `home`, `home-tap`, `home-hold`, `menu`, `settings`, `power`; deep links `settings:<page>`, `overlay:volume|network|session`, `app:<wmClass>`. The `overlay:` namespace is **closed and includes `session`** — every enumeration of it must list all three. `app:` accepts any leaf. `key <name>` accepts exactly `up`, `down`, `left`, `right`, `select`, `back`.

**Authoritative `settings:<page>` slugs.** The registry in `shell/settings/SettingsApp.qml` is the single source of truth, and every doc that enumerates slugs must match it: `audio`, `bluetooth`, `network`, `display`, `wallpaper`, `controllers`, `keybindings`, `avcontrol`, `webapps`, `accessibility`, `power`, `system`. Three further slugs are accepted by `ShellLayout.openSettings` but are **not** settings pages — `widgets` (a top-level surface) and `moonlight`/`streaming` (demoted, both land on Widgets ▸ Moonlight). There is no `appearance` page.

**Capability handshake.** `capabilities` returns `{node_id, kind, agent_version, platform, features}` where `kind` is `shell` or `sidecar` and `features` is a sorted set drawn from `cec`, `controllers`, `widgets`, `web_apps`, `settings_store`, `shell_lifecycle`, `screenshot`, `sleep`, `dev_deploy`, `logs`, `steam_library`, `game_launch`, `wallpapers`, `processes`, `system_updates`. Two rules govern it: **report what this build can do, never what is momentarily working** (a wedged CEC adapter does not drop `cec`), and **a proxied capability stays the remote node's to declare**. Unknown feature names round-trip verbatim rather than failing the parse.

### 6.3 Network control surface (opt-in)

Two thin adapters over the same action logic, both off unless bound, both sharing `[http].token_file` as `Authorization: Bearer`, constant-time compared.

**HTTP bridge** (`[http].bind`): `POST /intent/<name>` · `POST /key/<name>` · `GET /screenshot[.png][?flash=1]` · `GET /status` · `POST /suspend` · `GET /dev/status` · `GET /dev/logs?lines&filter` · `POST /dev/deploy?ref=` · `POST /dev/build` · `POST /dev/restart-shell` · `POST /dev/restart-daemon` · `GET /metrics` (**auth-exempt** — scrapers do not send tokens; aggregate counters and gauges only). Hardened with a 4 KiB header cap, 5 s header timeout, 128-connection cap and a 180 s budget for `/dev/*` subprocesses.

`GET /status` reports shell state, `media_playing`, **staleness** (`stale`, `age_seconds`, `stale_after_seconds`), `shell_running`, and CEC display-ownership with timestamps. Callers must gate on `stale` before acting, and `cec_display_ownership: unknown` never means "nobody is watching".

**MCP server** (`[mcp].bind`, streamable-HTTP at `/mcp`): 16 tools — `shell_action`/`intent`, `navigate`/`key`, `open_settings`, `open_overlay`, `launch_app`, `list_apps`, `get_ui_state`, `take_screenshot`, `get_status`, `get_logs`, `restart_shell`, plus `dev_deploy`, `dev_build`, `dev_restart_daemon` gated by `[mcp].dev` — and the resource `screenshot://current`. Deep links are rejected at the MCP layer in favor of the typed tools. `[mcp].allowed_hosts` narrows the Host header.

### 6.4 MQTT / Home Assistant

Four topics per device, three of them retained:

```
tv-shell/<device_id>/state                        retained    device → broker
tv-shell/<device_id>/avail                        retained    LWT: "online" | "offline"
tv-shell/<device_id>/cmd/<name>                   not retained  broker → device
homeassistant/device/tv-shell-<device_id>/config  retained    discovery
```

`device_id` is restricted to `[A-Za-z0-9_-]`, ≤64 bytes, so `/`, `+`, `#` and `$` can never reach a topic. The state payload is one envelope — `{schema_version, published_at, seq, current_os, status}` — carrying either the daemon's shell snapshot or the sidecar's canonical `{version, running_appid, streaming}`. Cadence is **emit-on-change plus a ~30 s floor heartbeat**, because availability cannot express "connected, but nothing is arriving". System metrics ride along on other publishes and never trigger one.

Commands: the daemon accepts `suspend`, `restart-shell`, **and any valid intent** (payload ignored); the sidecar accepts `sleep`, `quit`, `open-bpm`. The five published buttons are a convenience, not a boundary — **the security boundary is the broker ACL**. Home Assistant integration is one retained device-based discovery document with per-entity availability in three tiers: commands and liveness gated (go `unavailable` while the machine sleeps), facts ungated (last known value, timestamped by `published_at`), plus an ungated `connected` binary sensor reading the LWT.

### 6.5 Web control panel

Server-rendered HTML + HTMX (vendored, no CDN, no build step), bound `127.0.0.1:8091` by default. Its own systemd user unit, so it survives a wedged daemon. Four data tiers: the daemon's Unix socket (primary), the daemon's HTTP bridge (dev ops), an HTTP transport for remote sidecar nodes, and **direct exec** (`systemctl --user`, `journalctl`, `ps`, `checkupdates`, `pacman`) which needs no daemon at all.

Auth: browsers exchange the panel token for an `HttpOnly`/`SameSite=Strict` session cookie; scripts send a bearer. **The cookie value is the token** — there is no session store, so revocation means rotating the file and restarting. Exactly four routes are public: login (GET/POST) and the two static assets. A non-loopback bind requires a token or the panel refuses to start.

The UI is six subject groups behind a drawer — **Overview**, **System** (services, processes, updates, logs), **Shell** (appearance, widgets, apps, advanced), **Devices** (controllers, display & audio, CEC, network), **Remote** (navigation, launcher) and **Dev** (recovery, screenshot, console).

Routes register in four tiers. **Recovery** is always registered and is what survives a failed handshake — **Overview, System and Dev remain; Shell, Devices and Remote disappear entirely**, because those three depend on a node that is answering. **Node** requires a successful handshake, **Capability** requires the node to have declared the matching `Feature`, and **Danger** requires `[panel].allow_dangerous`, intersected with a capability where a route is both. The rule that separates recovery from danger: *restarting a unit is recovery; changing what code runs, powering the box, or running arbitrary commands is root-equivalent.*

Remote sidecar nodes are declared as `[[panel.nodes]]` with `id`, `base_url` and `sidecar_token_file` — never `token_file`, because a panel may hold credentials only for sidecar nodes it serves, never another shell node's own token.

### 6.6 Sidecar API (`tv-shell-host`)

Bearer-auth'd HTTP on port `47995` by default (chosen outside Sunshine's range): `GET /library` · `POST /launch {appid}` · `POST /open-bpm` · `POST /quit {appid}` · `POST /sleep` · `GET /status` · `GET /capabilities`, plus a deliberately public `GET /art/{appid}` because QML's `Image.source` cannot send an Authorization header. Refusals (a game is running, a stream is live, the app is not running) return HTTP 200 with `{ok:false, reason}` — a refusal is an answer, not an error. The sidecar **refuses to start** on a non-loopback bind with no token, with no escape-hatch flag; on a loopback bind it mints and logs a random one. It is an HTTP service the daemon is a *client* of — the daemon never spawns or supervises Steam.

### 6.7 Configuration

One typed `~/.config/tv-shell/config.toml` shared by daemon and panel, `deny_unknown_fields`, no reload path.

| Section | Keys (defaults) |
|---|---|
| `[http]` | `bind` (unset ⇒ off) · `auth_enabled` (`true`) · `token_file` |
| `[mcp]` | `bind` (unset ⇒ off) · `dev` (`false`) · `allowed_hosts` (`[]` ⇒ allow-all, token-gated) |
| `[panel]` | `enabled` (`true`) · `bind` (`127.0.0.1:8091`) · `token_file` · `allow_dangerous` (`false`) |
| `[[panel.nodes]]` | `id` · `base_url` · `sidecar_token_file` |
| `[cec]` | `lifecycle` (`false`) · `osd_name` (unset ⇒ hostname; ASCII, max 13) |
| `[input]` | `meta_hold_ms` (`500`) · `combo_guard_ms` (`120`) |
| `[input.contracts]` | `"<wm-class>" = "gamepad" \| "keyboard" \| "handoff"` |
| `[plex]` / `[steam]` | `url` · `token_file`; `[[steam.hosts]]` (`name`/`url`/`token_file`/`mac`) · `wake_active_host_on_start` (`false`) |
| `[mqtt]` | `broker` (unset ⇒ off; `mqtt://`/`mqtts://` only) · `device_id` (required with broker) · `username` + `password_file` (both or neither) · `ca_file` · `heartbeat_secs` (`30`) · `keepalive_secs` (`60`) |
| `[observability]` | `log_journal` (auto) · `metrics_textfile` (unset ⇒ writer off) · `metrics_interval` (`15`) |
| `[dev]` | `allow_insecure_lan` (`false`) — read by **both** binaries |

User preferences live separately in `settings.json`, written only by the daemon: theme and auto-theme schedule, accessibility (`reduceMotion`, `textScale`), display (`hdrEnabled`, `nightLight*`, `overscan`, wallpaper), power (`sleepTimerMinutes`, `wakeOnController`, `autoDim*`), audio (`defaultSink`), CEC focus behavior, `prewarmApps`, `webApps`, `widgets.<id>.*`, and the binding layers (`keyBindings`, per-player, per-game).

## 7. Architecture

```
                    ┌──────────────── the TV box ────────────────┐
  gamepads ─evdev──▶│                                            │
                    │  tv-shell-input (Rust)                     │
   Pulse-Eight ─────│   ├─ input thread: EVIOCGRAB, presenters,  │
   USB-CEC ─────────│   │   virtual kb/mouse/pads, combos        │
                    │   ├─ IPC: /run/user/$UID/…​.sock (0600)     │──▶ tv-shell-panel
                    │   ├─ HTTP bridge  :bind  (token)           │     (axum+htmx,
                    │   ├─ MCP /mcp     :bind  (token)  ◀────────┼──── AI agent
                    │   ├─ MQTT client  ───────────────▶ broker ─┼──▶ Home Assistant
                    │   ├─ /metrics (auth-exempt) ───────────────┼──▶ Prometheus
                    │   ├─ D-Bus actors: BlueZ, NM, logind/UPower│
                    │   ├─ Hyprland IPC actor (fullscreen enforcer)
                    │   └─ CEC actor + display-ownership tracker │
                    │                ▲          │                │
                    │      subscribe │          │ intents/state  │
                    │                │          ▼                │
                    │  Quickshell shell.qml (QML)                │
                    │   ├─ state: idle│launching│streaming│      │
                    │   │            reconnecting│appRunning     │
                    │   ├─ layer-shell Overlay, keyboardFocus    │
                    │   │   Exclusive; unmapped when an app owns │
                    │   │   the screen                           │
                    │   └─ screens: Home · Library · Settings(12)│
                    │       Widgets · drawers · overlays         │
                    │                                            │
                    │  Hyprland (kiosk): fullscreen-everything   │
                    │  windowrules; apps are ordinary toplevels  │
                    └────────────────────────────────────────────┘
                                     │ HTTP :47995 (bearer)
                                     ▼
                            tv-shell-host  ── Steam library / launch /
                            (the gaming PC)   quit / sleep / Big Picture
                                     │
                            Sunshine ─┴─▶ Moonlight (launched by the shell)
```

**Control flow.** The shell never talks to hardware. It subscribes to the daemon's event bus, pushes declared screen ownership and shell state back down, and issues commands over the same socket. Every high-level action — from a gamepad, a keyboard `Super` bind, the HTTP bridge, MCP or MQTT — converges on one intent vocabulary and one broadcast bus, so an agent and a human take literally the same code path.

**Process model.** Three systemd `--user` units (`tv-shell-input`, `tv-shell-quickshell`, `tv-shell-panel`), none with an `[Install]` section: the session script is the single owner of the daemon and panel lifecycle, and Hyprland's `exec-once` starts the shell after importing the Wayland environment. The daemon runs its input subsystem on a dedicated OS thread with its own runtime, supervised and respawned on panic, separate from the multi-thread runtime serving IPC and the D-Bus actors.

## 8. Deployment & operations

**Install.** `sudo ./scripts/install-deps.sh` (Hyprland, Quickshell, Qt, Rust, `grim`, `socat`; `--with-apps` adds Chromium, Moonlight, Plex HTPC, Spotify) then `sudo ./scripts/install.sh` — which builds the daemon and panel, lays down an install tree under `--prefix` (default `/opt/tv-shell`), registers `/usr/share/wayland-sessions/tv-shell-wayland.desktop`, installs the three user units with `ExecStart` rewritten to the prefix, symlinks the Quickshell config, and seeds `~/.config/tv-shell` from the shipped examples without ever clobbering existing files. The shell resolves its install root at runtime, so any prefix works. The installer is re-runnable and is the upgrade path.

**Session.** Display manager → `tv-shell-session.sh` → exports `TV_SHELL_*`, starts the daemon and panel units → `exec Hyprland` → `exec-once` imports `WAYLAND_DISPLAY`/`HYPRLAND_INSTANCE_SIGNATURE`/`XDG_RUNTIME_DIR` into the user manager and starts the Quickshell unit. An EXIT trap stops everything. Autologin is deliberately the installer's business, not the repo's.

**Sidecar.** `tv-shell-host` ships as a per-platform release binary (`host-v*` with checksums) and is expected to be placed by whatever configuration management the site already uses — as a Windows scheduled task at logon, a Linux user unit, or a macOS LaunchAgent. macOS is a CI target only: it can open a launch URL but can never see or stop a running game, so it does not claim `game_launch`.

**Agent/dev loop.** Push a branch → `/dev/deploy?ref=` → `/dev/build` → `/dev/restart-daemon` or `/dev/restart-shell` → `/screenshot`. These routes are RCE-by-design and always registered when the bridge is bound; on the panel they sit behind both `allow_dangerous` and the `dev_deploy` capability. **Deploy the daemon before the panel, or both together** — the panel requires a daemon that answers `capabilities`.

**Credential rotation.** A node carries up to three independent tokens — the daemon's `[http].token_file` (shared by the HTTP bridge and MCP), the panel's `[panel].token_file`, and one `sidecar_token_file` per remote node served. **No binary has a reload path**, so rotation is a restart, and rotation is therefore outage-adjacent rather than a config edit. The order is: write the new token file (mode 0600, inside the config dir) → restart the holder → restart the consumer. Rotating the daemon's bridge token invalidates any fleet console holding it; rotating a sidecar token must be done on both ends together. The panel's cookie *is* its token, so rotating it logs every browser out by construction.

**Observability.** Logs go to journald when available (structured fields, syslog priority mapping) and stdout otherwise, never neither; `RUST_LOG` behaves identically on both paths. Metrics are namespaced `tv_shell_*` and rendered once, shared between a node_exporter textfile writer and the auth-exempt `GET /metrics`. The catalogue is deliberately counter-heavy — `input_events_total`, `intents_emitted_total`, `transitions_total`, `pad_joins/leaves_total`, `shell_restarts_total`, `input_runtime_up`, `input_runtime_restarts_total`, `grab_invariant_violations_total`, `deploy/build/restart_* _total`, `quickshell_multi_instance_total`, `build_info` — with CPU/memory/load/temperature gauges as a convenience that a real node_exporter should supersede. Collection and forwarding are out of scope on purpose.

**System updates.** The panel reads pending updates via `checkupdates` with a TTL cache, detects a needed reboot by comparing the running kernel to the installed package, and applies with a single-flighted `sudo -n pacman -Syu --noconfirm` streaming a live tail. This requires a narrow NOPASSWD sudoers rule for the unit's user; with no rule it fails closed with an explicit refusal naming what is missing.

## 9. Quality bar

"Working" means all of the following, and CI enforces the mechanical half.

- **Rust**: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo build --release`, `cargo test` — per crate; plus a `--features cec` leg (in a glibc-new-enough container) that asserts via `ldd` that **no system libcec is linked**, and `scripts/assert-pure-rust-tls.sh`, which fails the build if any C-backed TLS or crypto crate enters the dependency graph. The invariant is rustls + `ring`, no cmake, no system TLS.
- **Cross-platform**: `host` and `protocol` build, lint and test on Linux, macOS and Windows.
- **QML**: `qmlformat -i` over every `.qml` (auto-committed on PRs, hard-fail on `main`) and `qmllint -D Quick` over the whole shell. Logic that can be tested headlessly is extracted into `.js` modules and covered by `qmltestrunner` under `QT_QPA_PLATFORM=offscreen` against a synthesized module of real components plus hand-written stubs.
- **MQTT**: contract tests against a real broker, `#[ignore]`-gated behind `TV_SHELL_TEST_BROKER` so the default suite stays offline — and they **panic rather than pass** if run `--ignored` without a broker.
- **Structural invariants pinned by test, not convention**: the panel's route table is asserted against a textual parse of the router (an unattributed route fails the suite), every mutating recovery-tier route carries a written justification, nav items must agree with the routes they link to, and the auth layer must wrap every registered route.
- **Widget catalog**: `widgets-index.json` must not drift from the QML manifest singleton.
- **The single required check is `CI / ci-gate`**; path filters skip untouched areas and a skipped area counts as success.
- **Human/agent gates**: docs update in the same PR as the change (a new IPC command belongs in `IPC_PROTOCOL.md`; a new config key in `config.toml.example`); conventional commits; branch off `main`, never commit to it; all review threads resolved before merge.
- **Product acceptance** (not automatable): every interactive element reachable by D-pad; B always goes back; exactly one app window visible; the escape from any app works in every state; the panel still works with the daemon stopped; a QA screenshot batch over the catalogued view tiers shows no regression.

## 10. End state vs today

| Capability | End-state intent | Status | Tracking |
|---|---|---|---|
| Kiosk shell, controller nav, 12 settings pages | Every element D-pad reachable, B always back | shipped | — |
| Exclusive input + presenter model | Four presenters, per-game/per-player binding layers | shipped | — |
| Multi-pad through a stream | 2–4 pads survive handoff and hot-plug | partial — mechanism built, unverified on hardware | jedwards1230/tv-shell#221 |
| Moonlight streaming | Launch, auto-reconnect, pre-flight pairing gate | shipped | — |
| Console overlay over a live stream | QAM and drawer usable *on top of* a running stream, surviving a shell restart | not started (deliberately all-or-nothing) | jedwards1230/tv-shell#75 |
| HDMI-CEC control | Wake, claim source, standby, health reporting | shipped | — |
| CEC adapter self-heal | A wedged adapter recovers without a host reboot | not started | jedwards1230/tv-shell#251 |
| Display release / ownership handoff | The box can give the display back, enabling ownership-aware idle | not started | jedwards1230/tv-shell#372 |
| AV lifecycle beyond CEC | Receiver zone-off and cold TV wake handled by the daemon over IP, from typed config | partial — a complete implementation exists unmerged and needs porting to `config.toml` | jedwards1230/tv-shell#186 |
| AV control settings actually wired | Every rendered control has a consumer | partial — `cecAutoSwitchOnPowerOn` has a reader nothing calls; `cecDefaultInput` has no reader at all | jedwards1230/tv-shell#16, jedwards1230/tv-shell#415, jedwards1230/tv-shell#416 |
| MQTT / Home Assistant | Full state + command surface, HA discovery | shipped | — |
| MCP + HTTP bridge | Agent can deploy, drive, screenshot, verify | shipped | — |
| Current MCP spec | Track the 2026-07-28 spec | not started — blocked on an upstream stable release and an MSRV bump | jedwards1230/tv-shell#379 |
| Screenshot fidelity | A capture under fullscreen HDR is current and 10-bit | partial — triggers done, capture engine returns stale/flattened frames | jedwards1230/tv-shell#284 |
| Web control panel | Capability-gated, recovery-first operator surface | shipped | — |
| Panel information architecture | Six grouped areas behind a drawer, dangerous actions in one place | shipped — jedwards1230/tv-shell#412 merged 2026-08-22; the Services allowlist needs a sudoers rule per node | jedwards1230/tv-shell#409 |
| Fleet console | One panel serves N nodes behind a node switcher; every shell node keeps a local recovery panel | partial — transport and node config landed, nothing serves a second node | jedwards1230/tv-shell#409 and MULTI_NODE_PANEL.md step 6 |
| On-screen keyboard | A narrow OSK for stranding flows only (Wi-Fi password, stream target) | not started | jedwards1230/tv-shell#20 |
| Web apps | Add from the couch or the panel; presets and icons | partial — panel add flow shipped; presets, icons, on-TV flow deferred | jedwards1230/tv-shell#187 |
| Packaging | Installable the standard way for the platform | not started — `packaging/` is empty | jedwards1230/tv-shell#144, jedwards1230/tv-shell#147 |
| Wedge detection | A frame-presentation counter on `/metrics`; healing is the environment's job | not started | jedwards1230/tv-shell#383 |
| Home rail richness | Box art, configurable rows, structured hint bar | not started / partial | jedwards1230/tv-shell#114, jedwards1230/tv-shell#19, jedwards1230/tv-shell#377 |

## 11. Risks & accepted limits

**Hardware and AV.** These are properties of the room, not bugs to fix:

- A receiver's CEC processor is typically **off in standby** — it cannot be woken over CEC at all. Waking the chain needs an out-of-band path — which is why the daemon owns IP-based AV control (§5) rather than leaving the gap to CEC.
- TVs commonly accept a CEC standby **only from the current active source**, and cold-wake over CEC is unreliable; Wake-on-LAN is the dependable wake for a TV that supports it.
- Other HDMI sources reassert active-source seconds after any bus activity, so claiming the display is not a one-shot operation — it has to be defended.
- The USB-CEC adapter can enter a **transmit-dead state** whose only known fix today is a host reboot; the daemon reports this honestly through `cec-health` rather than pretending, but cannot yet recover it.
- A 2.4 GHz gamepad dongle generally does **not** implement USB remote-wake, so a gamepad press cannot resume a suspended host. Any "press Home to wake everything" flow depends on the host staying awake, or on a wake path that does not run on the host.
- Some pads present as a generic Xbox-compatible VID:PID, so device identity must come from descriptor strings, not IDs. Composite receivers can create a phantom gamepad node with no controller paired.
- HDMI bandwidth on open AMD drivers can silently downgrade a requested 10-bit mode to subsampled 8-bit, and the compositor reports the *requested* mode. The sink is the only honest source.
- Direct scanout means the compositor may stop recompositing, so a screen-copy capture can return a stale frame — which poisons any capture-based watchdog. Capture-hash probes are explicitly not a valid liveness signal.

**Software and process.**

- **`/dev/deploy` and `/dev/build` are RCE-by-design.** A leaked bearer token is a device-control credential and, because the same token serves the HTTP bridge and MCP, exposure of either surface exposes both. `POST /suspend` widens the blast radius further. Non-loopback binds fail closed, which is the mitigation.
- **The MQTT security boundary is the broker ACL**, not the published button list — anything that can publish to `cmd/+` drives the entire intent vocabulary.
- **No binary has a config reload path.** Rotating any credential is a restart, which on the TV box is outage-adjacent.
- **A wedged compositor is currently invisible.** Qt timers keep firing while nothing is presented; every existing check can pass through a multi-day black screen. This is the single most severe known failure mode and it has neither a sensor nor an actuator today.
- **`--features mcp` is compiled by the deploy and release builds but never by CI.** `cec` has a dedicated CI leg; `mcp` has none, so a break in the agent control surface — the one carrying the RCE-by-design dev tools — passes PR CI green and fails only at release or on the device (jedwards1230/tv-shell#414).
- **The panel does not build on Windows** because every page module compiles regardless of capability gating; serving sidecars remotely removes the reason to care, and nothing plans a non-Linux shell node.
- **Site identity is already leaking into source, and both the AV and fleet-console decisions push harder on it.** A deployment hostname appears in a non-test doc comment in `panel/src/capabilities.rs` and in `panel/src/updates.rs`'s module doc, and the update path hard-codes `checkupdates`/`pacman`. This is the exact class of thing two PRs were closed over, now sitting in `main`. The locked rule in §5 is the mitigation: addresses and identities are configuration, and the fix is a cleanup plus a gate, not vigilance (jedwards1230/tv-shell#417).
- **A fleet console concentrates device-control credentials.** Serving a *remote shell node* means holding that node's daemon bridge token — the same token that reaches `/dev/deploy`. A fleet console is therefore only as safe as the least-trusted node it serves is allowed to be, and must be deployed on a host no less trusted than the most privileged node in its set.
- **Hardware-bound verification is a throughput limit, not a scheduling detail.** The three hardest open problems (multi-pad handoff, CEC wedge recovery, render-wedge detection) all require a physical, singly-available TV to verify.
- **Version reporting is uneven**: the daemon and sidecar carry real released versions; the shell and panel ship from git and report none, so "what is deployed" is answered by `build-info`/`tv_shell_build_info` (sha + branch) rather than a version number (jedwards1230/tv-shell#418).

## 12. Decision record

The five forks that shaped this document, and how they were settled. None remain open.

**1. Who owns AV control beyond CEC? → The daemon does.** CEC cannot wake a receiver in standby or reliably cold-wake a TV. The alternative — publishing state over MQTT and letting Home Assistant drive the receiver and the wake — was rejected: **tv-shell must be self-sufficient for AV lifecycle**, not dependent on an external automation platform. The existing unmerged implementation is a port, not a rewrite; it must land driven by typed `config.toml`, with every address supplied as configuration. Consequence: the daemon carries protocol code for AV control, and the §5 site-config rule is what keeps that from becoming site identity in source.

**2. Who is this for? → Package it; skip onboarding.** Packaging is an end-state goal and the single biggest gap between the stated goals and reality. First-run onboarding is an **explicit non-goal**. Consequence: the product is installable by someone who already runs Hyprland, and makes no promise to a user who does not.

**3. Is the panel a fleet console or a per-node tool? → A fleet console.** The node switcher is end-state, not a nicety, and the deployment question `MULTI_NODE_PANEL.md` left open is answered here:

- **Where it runs.** The fleet console is a second `tv-shell-panel` instance on a Linux shell node, bound to its own port with its own token file. It does not replace the per-node panel: **every shell node keeps a local panel on the default bind**, because the exec tier — restarting a hung unit — is inherently local and is the reason the panel exists. The fleet instance is the convenience surface; the local instance is the recovery surface of last resort.
- **What it serves.** Sidecar nodes are served in full over `HttpTransport` with a per-node `sidecar_token_file`. A remote **shell** node is served in a **degraded tier**: everything reachable through that node's daemon (state, settings, controllers, CEC, intents, dev bridge) but *not* its local exec tier. Capability gating already produces exactly this shape with no new mechanism — an unreachable tier is simply a set of routes that never register.
- **How it is secured.** All node transports are LAN-scoped HTTP with bearer auth and a per-node credential; a non-loopback bind without a token refuses to start. Reach beyond the LAN is the environment's job — a VPN or an authenticating reverse proxy — and never the panel's. The escalation is stated plainly: serving a remote shell node means holding its **daemon bridge token**, which reaches `/dev/deploy`, so a fleet console must be deployed on a host no less trusted than the most privileged node it serves, and a node's *panel* token is still never shared.

**4. Is an on-screen keyboard part of the end state? → Yes, narrowly.** Scoped to flows that strand a user **mid-use** with no second device — joining a Wi-Fi network, entering a stream target. Not a general 10-foot keyboard, and explicitly not scoped to first-run setup, which is a non-goal per decision 2. The panel remains the comfortable surface for bulk text entry.

**5. Does the product self-heal? → It reports; the environment acts.** The daemon exports a frame-presentation counter on `/metrics` so a wedged render loop becomes visible; the actuator that kills and restarts the graphical stack lives outside the daemon. This is the same split the product already made everywhere else, and it keeps a game-killing action behind a policy layer that can see more context than the daemon can. Consequence: unattended recovery depends on external automation being configured — the one place the product deliberately does not stand alone.
