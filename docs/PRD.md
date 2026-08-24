# tv-shell — Product Requirements Document

> **Status:** source of truth for the intended end state · 2026-08-22 · repo: [`jedwards1230/tv-shell`](https://github.com/jedwards1230/tv-shell) (public, GPL-3.0)
>
> This document describes what tv-shell **is meant to be, fully realized**. It is not a task list — open work lives in GitHub issues, and §10 links the two. Where a claim is about today rather than the end state, it says so.
>
> ⚠️ **One decision in this document is deliberately open**: whether the fleet console may serve remote *shell* nodes, which would require it to hold their RCE-capable daemon bridge tokens. See **§12 decision 3**. Everything else here is settled. **Sidecars-only is the operative behavior until the owner decides — do not implement the remote-shell-node path.**

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
12. **Legible from ten feet by someone who needs it to be.** Reduced motion and text scaling are first-class settings the whole shell honors, not a page that stores two keys — which is what they are today (see §10). Colour is never the only carrier of state, and every affordance stays reachable by D-pad, which is the same constraint as goal 1.

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
| Multiple user accounts or switchable profiles † | One box, one couch, one session. The only per-person state that has ever mattered is controller bindings, and those already have per-player and per-game layers without a profile system. A profile switcher on a 10-foot UI costs a screen and buys nothing the binding layers do not |
| Localization / translated UI † | The strings are English, and one CEC constraint is hard: `osd_name` is ASCII, max 13 bytes (`daemon/src/daemon_config.rs`), so the device's own name on the bus cannot be localized anyway. Extraction and translation are a standing maintenance cost against a hardware-verification bottleneck that is already the project's throughput limit |

† These two were open — neither goal nor non-goal — until this revision. They are recorded here as the default answer, not a fork that was argued; overturning either is cheap and neither has code depending on it.

**What "distribution agnostic" does and does not mean.** It means **no site identity in source** and no dependency on any particular configuration-management tool. It does **not** mean OS-neutral: the panel's recovery tier is systemd-specific by design, and its system-update tier is pacman-specific. `CLAUDE.md` states the narrowed version (it previously claimed "no knowledge of specific infrastructure, deployment tools, or host management", which overstated it — the panel manages systemd units and applies `pacman -Syu` today). Packaging (§5) turns that coupling into a **declared platform target** rather than an unstated assumption.

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
| Config | Typed `config.toml`, no reload path; `deny_unknown_fields` on **the daemon's own sections only** | A typo in a daemon section aborts startup instead of silently disabling a feature. The `[panel]` sections are deliberately exempt (see §6.7) — that protection does not extend to them |
| `settings.json` | The **daemon is the sole writer**; QML and panel go through `get-config`/`set-config` | One RMW path under a mutex; a JSON `null` deletes a key |
| Web apps | Chromium `--app` + generated `.desktop` + `StartupWMClass` | Widevine and hardware decode for free; reuses the existing app-discovery and window-matching path |
| Text entry today | The **panel** is the add surface for anything needing a keyboard | The couch UI has no on-screen keyboard yet |
| CEC scope in-repo | libcec statically linked behind `--features cec`; no site-specific helper scripts invoked | Keeps homelab identity out of a public repo |
| Site config vs source | **AV device addresses, node addresses and the panel's deployment target are configuration, never literals in code.** No hostname, IP or MAC of any deployment appears in a non-test source path. Enforced on every PR by `scripts/check-site-identity.py`, which masks `#[cfg(test)]` items so fixtures stay legal | The repo is public; site identity has been rejected from it three separate times |
| AV lifecycle owner | **The daemon owns IP-based AV control** alongside CEC — receiver power/zone control and display wake, driven from typed config | The box must be self-sufficient for AV; an external automation platform is not a dependency |
| Panel topology | **A fleet console with a node switcher.** Every shell node also runs its own local panel as the recovery path of last resort. **Which node kinds it may serve is an open decision — see §12.3**; sidecars-only is operative today | The exec tier is local; a remote console cannot restart a hung unit |
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

**Authoritative `settings:<page>` slugs.** The registry in `shell/settings/SettingsApp.qml` is the single source of truth, and every doc **and every enumeration in code** must match it: `audio`, `bluetooth`, `network`, `display`, `wallpaper`, `controllers`, `keybindings`, `avcontrol`, `webapps`, `accessibility`, `power`, `system`. Three further slugs are accepted by `ShellLayout.openSettings` but are **not** settings pages — `widgets` (a top-level surface) and `moonlight`/`streaming` (demoted, both land on Widgets ▸ Moonlight). There is no `appearance` page.

Two enumerations have drifted from it, and the second is the one that matters. `docs/IPC_PROTOCOL.md` invents an `appearance` slug and omits `wallpaper`, `webapps` and `system` — a doc fix. `daemon/src/mcp.rs`'s `SettingsPage` enum, which is the **typed constraint on the MCP `open_settings` tool**, rejects `wallpaper` and `webapps` at schema-validation time and accepts `streaming`/`widgets`, which are not settings pages — so an agent gets a hard deserialization error on two pages a human can reach by D-pad. Both enumerations happen to carry twelve entries, which is why the drift reads as fine at a glance. Nothing spans QML and Rust to catch this class of divergence; the end state pins it with a test the way the panel's route table and `widgets-index.json` are pinned (jedwards1230/tv-shell#423).

**Capability handshake.** `capabilities` returns `{node_id, kind, agent_version, platform, features}` where `kind` is `shell` or `sidecar` and `features` is a `BTreeSet` — ordered by the `Feature` enum's **declaration** order, not alphabetically (`docs/PANEL.md` spells out the trap: it serializes byte-stably but not sorted by name) — drawn from `cec`, `controllers`, `widgets`, `web_apps`, `settings_store`, `shell_lifecycle`, `screenshot`, `sleep`, `dev_deploy`, `logs`, `steam_library`, `game_launch`, `wallpapers`, `processes`, `system_updates`. Two rules govern it: **report what this build can do, never what is momentarily working** (a wedged CEC adapter does not drop `cec`), and **a proxied capability stays the remote node's to declare**. Unknown feature names round-trip verbatim rather than failing the parse.

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

One typed `~/.config/tv-shell/config.toml` shared by daemon and panel; no reload path.

**`deny_unknown_fields` is a daemon property, not a product-wide one.** The daemon sets it on its own 26 sections and on the top-level table, so a typo there aborts startup — the intended behavior. It is deliberately absent in two places, and the consequence is worth stating plainly rather than leaving it implied by the locked rule:

- The daemon's `PanelConfig` omits it on purpose, "so the panel can add its own keys later without forcing a matching daemon-struct change" (`daemon/src/daemon_config.rs`). The daemon only needs `[panel]` to not abort its top-level parse.
- The panel parses the whole file **permissively** — it declares only the sections it needs and lets serde ignore everything else (`panel/src/config.rs`), because the file is shared with the daemon and the panel must tolerate sections it knows nothing about.

So a typo inside `[panel]` or `[[panel.nodes]]` — `allow_dangrous = true` — aborts nothing and **silently leaves the feature off**, which is exactly the failure the daemon's rule is there to prevent. This is an accepted cost of one shared config file, not an oversight; the mitigation is that the panel logs its resolved `bind`/`auth`/`allow_dangerous` line at startup, so the effective values are visible even when the written ones were not read.

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

### 6.8 The couch UI, finished

Every other part of §6 describes an end state. This section exists because the shell did not have one: the panel's end state is a fleet console, and the shell's was a list of open issues. What follows is the finished product, not the current build — §10's "Shell end state" row tracks the distance.

**One session, resumable.** The shell's five states — `idle`, `launching`, `streaming`, `reconnecting`, `appRunning` — describe what the box is doing, but nothing describes what the *person* is doing. At end state there is one notion of "what's going on right now" that survives a shell restart, a daemon restart and a suspend: what is running or streaming, where the pad was, what the last thing on screen was. The shell restarting during a stream is a shell problem, and the person on the couch should not be able to tell. That is the same guarantee jedwards1230/tv-shell#75 asks for, generalized beyond streaming — and it is what makes the box an appliance rather than a computer that happens to boot into a launcher.

**Home is a rail, and the rail is the product.** Box art rather than text (jedwards1230/tv-shell#114), rows the owner can add, remove and reorder (jedwards1230/tv-shell#19), and a hint bar that is a structured `{button, action}` model every surface feeds rather than a hand-written string per screen (jedwards1230/tv-shell#377). The four screens stay as they are — Home, Library, Settings, Widgets — because the kiosk invariant means depth is the enemy; richness belongs in the rail, not in more screens.

**The overlay is how you get anywhere without leaving.** The two drawers (nav, notifications) and four overlays (volume, network, power, session QAM) already work over a running app; at end state they work identically over a live stream, which is the all-or-nothing piece of jedwards1230/tv-shell#75. The rule is that no shell surface ever requires killing what you were doing to reach it.

**Nothing is stranded.** The narrow OSK (jedwards1230/tv-shell#20) covers joining a Wi-Fi network and entering a stream target — the two flows where a fresh box has no second device and no way forward. That is the whole scope; everything else about text entry stays on the panel, per §5.

**The screen goes quiet on its own.** An idle box on a TV should dim and then release the display rather than burn a static rail into an OLED for six hours. The hook exists as a comment in `ShellLayout.qml` (left by closed jedwards1230/tv-shell#156) and the mechanism it needs — giving the display back — is jedwards1230/tv-shell#372. There is no issue for the screensaver itself yet.

**Legible.** Reduced motion and text scaling are honored by every surface, not stored by a settings page and read by nothing (§10, and goal 12 in §4).

**Not in it:** more screens, a desktop metaphor, multiple visible windows, a general keyboard, profiles. §4's non-goals are the boundary and this section does not widen them.

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

**Upgrade and rollback (end state — not today).** Re-running the installer is the whole upgrade story today: it rebuilds from the working tree, overwrites the install tree in place, and keeps no previous version. There is nothing to roll back to, and a bad build on a headless TV box is recovered by the panel's `/dev/deploy?ref=` to a known-good ref plus a rebuild — which only works if the daemon still answers. That is the same single-point problem the panel exists to solve, unsolved one layer down.

The end state follows from the packaging goal (§4 goal 10, jedwards1230/tv-shell#144) rather than adding a mechanism beside it: the platform's package manager owns install, upgrade and downgrade, so rollback is `pacman -U` of the previous package and the box keeps whatever cache the platform already keeps. Three properties matter and none is free today:

- **The previous version stays on disk.** Rollback must not require a network or a build.
- **A downgrade is one operator action from the panel**, in the danger tier beside the update tier that already applies `pacman -Syu` — the same surface an operator is already using when an upgrade goes wrong.
- **Config survives both directions.** `config.toml` is `deny_unknown_fields` on the daemon's sections, so a downgrade past a new key aborts the daemon at startup rather than ignoring it. Either new keys are additive-and-optional across a supported version window, or the downgrade path has to strip them — this needs deciding before a package ships, not after.

Until packaging lands, the honest statement is the first paragraph: re-run the installer, and recovery is `/dev/deploy` to a good ref.

**Session.** Display manager → `tv-shell-session.sh` → exports `TV_SHELL_*`, starts the daemon and panel units → `exec Hyprland` → `exec-once` imports `WAYLAND_DISPLAY`/`HYPRLAND_INSTANCE_SIGNATURE`/`XDG_RUNTIME_DIR` into the user manager and starts the Quickshell unit. An EXIT trap stops everything. Autologin is deliberately the installer's business, not the repo's.

**Sidecar.** `tv-shell-host` ships as a per-platform release binary (`host-v*` with checksums) and is expected to be placed by whatever configuration management the site already uses — as a Windows scheduled task at logon, a Linux user unit, or a macOS LaunchAgent. macOS is a CI target only: it can open a launch URL but can never see or stop a running game, so it does not claim `game_launch`.

**Agent/dev loop.** Push a branch → `/dev/deploy?ref=` → `/dev/build` → `/dev/restart-daemon` or `/dev/restart-shell` → `/screenshot`. These routes are RCE-by-design and always registered when the bridge is bound; on the panel they sit behind both `allow_dangerous` and the `dev_deploy` capability. **Deploy the daemon before the panel, or both together** — the panel requires a daemon that answers `capabilities`.

**Credential rotation.** A node carries up to three independent tokens — the daemon's `[http].token_file` (shared by the HTTP bridge and MCP), the panel's `[panel].token_file`, and one `sidecar_token_file` per remote node served. **No binary has a reload path**, so rotation is a restart, and rotation is therefore outage-adjacent rather than a config edit. The order is: write the new token file (mode 0600, inside the config dir) → restart the holder → restart the consumer. Rotating the daemon's bridge token invalidates any fleet console holding it; rotating a sidecar token must be done on both ends together. The panel's cookie *is* its token, so rotating it logs every browser out by construction.

**Observability.** Logs go to journald when available (structured fields, syslog priority mapping) and stdout otherwise, never neither; `RUST_LOG` behaves identically on both paths. Metrics are namespaced `tv_shell_*` and rendered once, shared between a node_exporter textfile writer and the auth-exempt `GET /metrics`. The catalogue is deliberately counter-heavy — `input_events_total`, `intents_emitted_total`, `transitions_total`, `pad_joins/leaves_total`, `shell_restarts_total`, `input_runtime_up`, `input_runtime_restarts_total`, `grab_invariant_violations_total`, `deploy/build/restart_* _total`, `quickshell_multi_instance_total`, `quickshell_warnings_total`, `build_info` — with CPU/memory/load/temperature gauges as a convenience that a real node_exporter should supersede. Collection and forwarding are out of scope on purpose.

**System updates.** The panel reads pending updates via `checkupdates` with a TTL cache, detects a needed reboot by comparing the running kernel to the installed package, and applies with a single-flighted `sudo -n pacman -Syu --noconfirm` streaming a live tail. This requires a narrow NOPASSWD sudoers rule for the unit's user; with no rule it fails closed with an explicit refusal naming what is missing.

## 9. Quality bar

"Working" means all of the following, and CI enforces the mechanical half.

- **Rust**: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo build --release`, `cargo test` — per crate; plus a `--features cec` leg (in a glibc-new-enough container) that asserts via `ldd` that **no system libcec is linked**, and `scripts/assert-pure-rust-tls.sh`, which fails the build if any C-backed TLS or crypto crate enters the dependency graph. The invariant is rustls + `ring`, no cmake, no system TLS.
- **Cross-platform**: `host` and `protocol` build, lint and test on Linux, macOS and Windows.
- **QML**: `qmlformat -i` over every `.qml` (auto-committed on PRs, hard-fail on `main`) and `qmllint -D Quick` over the whole shell. Logic that can be tested headlessly is extracted into `.js` modules and covered by `qmltestrunner` under `QT_QPA_PLATFORM=offscreen` against a synthesized module of real components plus hand-written stubs.
- **MQTT**: contract tests against a real broker, `#[ignore]`-gated behind `TV_SHELL_TEST_BROKER` so the default suite stays offline — and they **panic rather than pass** if run `--ignored` without a broker.
- **Structural invariants pinned by test, not convention**: the panel's route table is asserted against a textual parse of the router (an unattributed route fails the suite), every mutating recovery-tier route carries a written justification, nav items must agree with the routes they link to, and the auth layer must wrap every registered route.
- **Widget catalog**: `widgets-index.json` must not drift from the QML manifest singleton.
- **`CI / ci-gate` is the intended single required check** — one aggregating job so path filters can skip untouched areas and a skipped area counts as success (`.github/workflows/ci.yml` states this design). ⚠️ **It is not configured as one.** `main`'s ruleset carries `deletion`, `non_fast_forward` and `pull_request` and **no `required_status_checks` rule**; there is no classic branch protection either. A red `ci-gate` cannot block a merge today. What the ruleset *does* enforce is a PR (`required_approving_review_count: 0`) and `required_review_thread_resolution: true` — so the "all threads resolved" gate below is real. Wiring `CI / ci-gate` as the required check is end-state work, not a description of today.
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
| Settings controls actually wired | Every rendered control has a consumer | partial — a pattern, not two loose ends. `cecAutoSwitchOnPowerOn` has a reader nothing calls (`daemon/src/config.rs`'s `cec_auto_switch_on_power_on` is called only by its own tests); `cecDefaultInput` has **no consumer** — it is read, but only to render its own "Default ✓" badge in `AVControlSettings.qml`; `hdrEnabled` and `nightLightEnabled` are read only by `DisplaySettings.qml` and appear nowhere in `daemon/src`. The audit that finds the rest, and the test that pins it, are jedwards1230/tv-shell#416 | jedwards1230/tv-shell#16, jedwards1230/tv-shell#415, jedwards1230/tv-shell#416 |
| MQTT / Home Assistant | Full state + command surface, HA discovery | shipped | — |
| MCP + HTTP bridge | Agent can deploy, drive, screenshot, verify | shipped | — |
| Current MCP spec | Track the 2026-07-28 spec | not started — blocked on an upstream stable release and an MSRV bump | jedwards1230/tv-shell#379 |
| Screenshot fidelity | A capture under fullscreen HDR is current and 10-bit | partial — triggers done, capture engine returns stale/flattened frames | jedwards1230/tv-shell#284 |
| Web control panel | Capability-gated, recovery-first operator surface | shipped | — |
| Panel information architecture | Six grouped areas behind a drawer, dangerous actions in one place | shipped — jedwards1230/tv-shell#412 merged 2026-08-22, all six phases. Residual: the Services allowlist needs a sudoers rule per node, which is deployment work with no issue of its own | — (jedwards1230/tv-shell#409, the phase-5 issue, is closed) |
| Fleet console | One panel serves N nodes behind a node switcher; every shell node keeps a local recovery panel. Sidecars only until §12.3 is decided | partial — `NodeTransport`, `HttpTransport` and `[[panel.nodes]]` landed (MULTI_NODE_PANEL.md steps 1–5); nothing constructs a transport from a live node and there is no node switcher | jedwards1230/tv-shell#422 (MULTI_NODE_PANEL.md step 6) |
| On-screen keyboard | A narrow OSK for stranding flows only (Wi-Fi password, stream target) | not started | jedwards1230/tv-shell#20 |
| Web apps | Add from the couch or the panel; presets and icons | partial — panel add flow shipped; presets, icons, on-TV flow deferred | jedwards1230/tv-shell#187 |
| Packaging | Installable the standard way for the platform | not started — `packaging/` is empty | jedwards1230/tv-shell#144, jedwards1230/tv-shell#147 |
| Wedge detection | A frame-presentation counter on `/metrics`; healing is the environment's job | not started | jedwards1230/tv-shell#383 |
| Home rail richness | Box art, configurable rows, structured hint bar | not started / partial | jedwards1230/tv-shell#114, jedwards1230/tv-shell#19, jedwards1230/tv-shell#377 |
| Agent-reachable settings pages | Every settings page an agent can name is one the shell actually has | broken — `daemon/src/mcp.rs`'s `SettingsPage` enum has drifted from the QML registry: it rejects `wallpaper` and `webapps` at schema validation, and accepts `streaming`/`widgets`, which are not settings pages | jedwards1230/tv-shell#423 |
| Site-neutral source | No deployment hostname, IP or MAC in a non-test source path | done — source swept and a CI gate added; `CI / ci-gate` fails on a reintroduction | jedwards1230/tv-shell#417 |
| Version reporting | Every artifact answers "what is deployed" with a real version | partial — daemon and sidecar do; panel and protocol are pinned `0.0.0` and report it forever | jedwards1230/tv-shell#418 |
| CI covers what ships | Every feature combination a deploy or release builds is compiled by CI | partial — `cec` has a leg, `mcp` has none, though every deploy and release build enables it | jedwards1230/tv-shell#414 |
| Merge gates enforced, not just intended | The required check, and the branch rules, match what §9 claims | not started — `main` has no `required_status_checks` rule; thread resolution and PR-required are enforced | — |
| Shell end state | The finished couch UI of §6.8 — one resumable session, overlays over anything, nothing stranded, the screen goes quiet on its own | partial — see §6.8; the rows above track its named pieces. The two with **no issue at all** are the resumable session model and the screensaver (its hook is a bare comment in `ShellLayout.qml`, left behind by closed jedwards1230/tv-shell#156) | §6.8; jedwards1230/tv-shell#75 and jedwards1230/tv-shell#372 are its nearest existing pieces |
| Upgrade & rollback | A deployed box updates and rolls back without a laptop | not started — the re-runnable installer is the only upgrade path; no rollback | jedwards1230/tv-shell#144 |

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
- **The panel does not build on Windows.** The blockers are `tokio::net::UnixStream` in `panel/src/ipc.rs` and two `libc` calls — `getuid()` in `config.rs` and `gethostname()` in `pages/cec.rs` — which fail at name resolution off unix because `libc` is declared under `[target.'cfg(unix)'.dependencies]`. Capability gating does not rescue this, because gating is a runtime decision and **every page module compiles regardless of what any node declares**. (`systemctl`/`journalctl`/`pacman` are *not* compile blockers — they are `Command::new` string literals that compile anywhere and fail at runtime.) Serving sidecars remotely removes the reason to care, and nothing plans a non-Linux shell node.
- **The site-neutrality rule in §5 is now enforced by a check, not by vigilance — and the check has known limits.** Vigilance failed three times (two PRs closed over it, then 23 occurrences back in `main` across ten non-test files in three crates, one of them a runtime error string in `panel/src/config.rs`'s `validate_base_url`). `scripts/check-site-identity.py` closes that loop: it masks `#[cfg(test)]` items so fixtures stay legal, flags private IPv4 and MAC literals everywhere, and flags `<word>-<digits>` host-shaped identifiers across the Rust crates, the QML shell and the shipped config examples. What it does **not** catch: a host name that is a single bare word (`plexbox`), a two-letter-prefixed one (`pi-1` — the prefix must be 3+ characters, because a 2-letter rule trips on `substr(s, 1, ci-1)`), a public IP, or anything in `docs/`, which is deliberately allowed to describe a real deployment. The allowlist of non-host tokens in the script is the maintenance surface; an entry there that looks like a machine name is the smell (jedwards1230/tv-shell#417).
- **A fleet console would concentrate device-control credentials — and that is why the question is still open.** Serving a *remote shell node* means holding that node's daemon bridge token, the same credential that reaches `/dev/deploy` and MCP. One console holding several of them is a single point whose compromise is remote code execution on every node in the set. The existing rule — a panel holds credentials only for the sidecar nodes it serves — keeps that from arising, and it stands unamended. **Deferred; see §12 decision 3.**
- **Hardware-bound verification is a throughput limit, not a scheduling detail.** The three hardest open problems (multi-pad handoff, CEC wedge recovery, render-wedge detection) all require a physical, singly-available TV to verify.
- **Version reporting is uneven, and the weak half reports a *wrong* version rather than none.** The daemon and sidecar carry real released versions, stamped into `Cargo.toml` from the tag at build time by the release workflow. The shell ships from git and reports nothing. `panel` and `protocol` are pinned `version = "0.0.0"`, so every capability handshake and Home Assistant entity that reads them will report `0.0.0` forever — which is worse than absent, because a version field that cannot distinguish two releases still invites trust. "What is deployed" is therefore answered by `build-info`/`tv_shell_build_info` (sha + branch), not a version number (jedwards1230/tv-shell#418).

## 12. Decision record

The five forks that shaped this document, and how they were settled. Four are closed. **The fifth — decision 3 — is settled except for one deliberately deferred question**, flagged at the top of this document and repeated in §5, §10 and §11: which node kinds the fleet console may serve. Sidecars-only is operative until the owner decides.

**1. Who owns AV control beyond CEC? → The daemon does.** CEC cannot wake a receiver in standby or reliably cold-wake a TV. The alternative — publishing state over MQTT and letting Home Assistant drive the receiver and the wake — was rejected: **tv-shell must be self-sufficient for AV lifecycle**, not dependent on an external automation platform. The existing unmerged implementation is a port, not a rewrite; it must land driven by typed `config.toml`, with every address supplied as configuration. Consequence: the daemon carries protocol code for AV control, and the §5 site-config rule is what keeps that from becoming site identity in source.

**2. Who is this for? → Package it; skip onboarding.** Packaging is an end-state goal and the single biggest gap between the stated goals and reality. First-run onboarding is an **explicit non-goal**. Consequence: the product is installable by someone who already runs Hyprland, and makes no promise to a user who does not.

**3. Is the panel a fleet console or a per-node tool? → A fleet console.** The node switcher is end-state, not a nicety. `MULTI_NODE_PANEL.md` marks exactly one question open — §4's "where the remote panel process runs" — and that one is answered below. The question this document *adds* and leaves open is a different one, about node kinds rather than deployment.

*Settled:*

- **Where it runs.** The fleet console is a second `tv-shell-panel` instance on a Linux shell node, bound to its own port with its own token file. It does not replace the per-node panel: **every shell node keeps a local panel on the default bind**, because the exec tier — restarting a hung unit — is inherently local and is the reason the panel exists. The fleet instance is the convenience surface; the local instance is the recovery surface of last resort.
- **How it is secured.** All node transports are LAN-scoped HTTP with bearer auth and a per-node credential; a non-loopback bind without a token refuses to start. Reach beyond the LAN is the environment's job — a VPN or an authenticating reverse proxy — and never the panel's. A node's *panel* token is never shared with another node's panel, under either option below.

> ### ⚠️ OPEN — what node kinds may the fleet console serve?
>
> **Status: deferred, owner decision pending. Do not implement the remote-shell-node path until this is settled.** This is the one deliberate exception to this document's no-open-forks rule.
>
> **Option A — sidecars only.** The console serves sidecar nodes over `HttpTransport` with a per-node `sidecar_token_file`, and nothing else. The existing locked credential rule — *a panel may hold credentials only for the sidecar nodes it serves, never another shell node's own token* (§6.5) — stays **fully intact, unamended**. A remote shell node is reached by opening its own local panel.
>
> **Option B — remote shell nodes too.** The console additionally serves remote shell nodes in a degraded tier: everything reachable through that node's daemon (state, settings, controllers, CEC, intents, dev bridge) but not its local exec tier. Capability gating already produces that shape with no new mechanism. **This requires amending the credential rule** to permit holding a remote shell node's **daemon bridge token**, with the amendment reading: *a fleet console must be deployed on a host no less trusted than the most privileged node it serves.*
>
> **What is at stake.** The bridge token reaches `/dev/deploy`, `/dev/build` and MCP — all RCE-by-design. Option B concentrates one such credential per served node in a single process, so compromising the console is remote code execution on every node in its set. Option A keeps each node's blast radius to itself at the cost of a node switcher that cannot reach the node kind you most often want to look at.
>
> **Operative until decided: Option A.** It is what the current rule already permits and it needs no amendment.

**4. Is an on-screen keyboard part of the end state? → Yes, narrowly.** Scoped to flows that strand a user **mid-use** with no second device — joining a Wi-Fi network, entering a stream target. Not a general 10-foot keyboard, and explicitly not scoped to first-run setup, which is a non-goal per decision 2. The panel remains the comfortable surface for bulk text entry.

**5. Does the product self-heal? → It reports; the environment acts.** The daemon exports a frame-presentation counter on `/metrics` so a wedged render loop becomes visible; the actuator that kills and restarts the graphical stack lives outside the daemon. This is the same split the product already made everywhere else, and it keeps a game-killing action behind a policy layer that can see more context than the daemon can. Consequence: unattended recovery depends on external automation being configured — the one place the product deliberately does not stand alone.
