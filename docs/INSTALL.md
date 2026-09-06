# Installing tv-shell

A standalone install on any Linux box with Hyprland + Quickshell. The repo ships
the installer and config examples, so you don't need any external deployment
tooling. For the on-device dev/iteration loop instead, see the Development
section in [CLAUDE.md](../CLAUDE.md).

## What gets installed

- The Rust input daemon (`tv-shell-input`) → `<prefix>/bin/`
- The web control panel (`tv-shell-panel`) → `<prefix>/bin/` (see [PANEL.md](PANEL.md))
- The QML shell + Hyprland config + scripts → `<prefix>/` (default prefix `/opt/tv-shell`)
- A Wayland session entry → `/usr/share/wayland-sessions/tv-shell-wayland.desktop`
- `systemd --user` units for the daemon and panel → `~/.config/systemd/user/tv-shell-input.service` + `tv-shell-panel.service` (see [SYSTEMD_SETUP.md](SYSTEMD_SETUP.md))
- A Quickshell config symlink → `~/.config/quickshell/tv-shell`
- A per-user config dir seeded from examples → `~/.config/tv-shell/`

The shell is **prefix-agnostic** and resolves its install root at runtime, so any
`--prefix` works; `/opt/tv-shell` is just the installer's default.

## 1. Dependencies

```bash
sudo ./scripts/install-deps.sh              # runtime deps only
sudo ./scripts/install-deps.sh --with-apps  # + the optional launchable apps
```

Installs what your package manager can provide (Hyprland, Qt6/Wayland, Rust,
`libudev`+`pkg-config`, `grim`, `socat`) and prints what you must add by hand —
typically **Quickshell** (AUR / source) and **Moonlight Qt** (the streaming
client). The `cec` daemon feature static-links its own libcec, so no system
`libcec`/`libcec-dev` is needed.

**Two tiers.** The bare run installs only the **runtime deps** — what the shell
can't run without. `--with-apps` additionally installs the **optional launchable
apps** the home-screen app-launcher surfaces — `chromium`, **Moonlight**, **Plex
HTPC**, **Spotify**, and **VacuumTube** (YouTube's 10-foot "leanback" TV UI).
These are home-screen entries, not dependencies; the shell runs fine without
them. The GUI apps come from Flathub (so `flatpak` + the Flathub remote are set
up if missing). VacuumTube also gets a Wayland override
(`ELECTRON_OZONE_PLATFORM_HINT=auto`) so it renders native-Wayland and can do
HDR — enable HDR inside VacuumTube (`wayland_hdr`) on an HDR-capable display.

## 2. Build + install

```bash
sudo ./scripts/install.sh                 # default prefix /opt/tv-shell
sudo ./scripts/install.sh --prefix ~/.local/share/tv-shell --user "$USER"
```

Re-runnable: it rebuilds the daemon and panel, refreshes the tree and session file, and
never overwrites your existing `~/.config/tv-shell` files. Useful flags:
`--no-build` (reuse an existing binary), `--features` (daemon Cargo features,
default `cec,mcp`), `--session-dir`, `--user`. See `./scripts/install.sh --help`.

### The v2 (gamescope) session — `scripts/install-v2.sh`

The v2 session installs **beside** this one, never over it
([`V2_DESIGN.md`](V2_DESIGN.md) §11): its own prefix, units, config file and
session entry, so v1 stays selectable as the rollback.

```bash
sudo ./scripts/install-v2.sh --user "$USER"   # default prefix /opt/tv-shell-v2
```

It builds `tv-shell-core`, installs it and the two session scripts to
`<prefix>/bin/`, writes the three v2 systemd `--user` units with their install
prefix substituted, writes `/usr/share/wayland-sessions/tv-shell-v2.desktop`, and
seeds `~/.config/tv-shell/core.toml` (v2's config file — **not** `config.toml`,
which is v1's and would abort the v1 daemon if it carried v2 tables). It refuses
a `--prefix` at or under `/opt/tv-shell` (normalised first, so `/opt//tv-shell`
and `/opt/tv-shell/anything` are refused too) so it cannot land in v1's tree.
Flags: `--prefix`,
`--user`, `--session-dir`, `--unit-dir`, `--config-dir`, `--no-build`,
`--no-session`; see `./scripts/install-v2.sh --help`.

**On a configuration-managed host, pass `--no-session`.** The installer writes
the session `.desktop` by default so a standalone install is selectable straight
away, but if your configuration management also writes that path you get two
writers of one file and it flips per run. `--no-session` suppresses the write
entirely (it does not even create the directory), leaving that file to its owner
— which is the right side of the split when only the config-management copy can
carry a `/usr/bin/env` prefix in `Exec=` for session environment, and only its
toggle can remove the entry. Everything else still installs, so the launcher its
`Exec=` points at is in place.

At the display manager you then get **TV Shell v2 (gamescope)** alongside **TV
Shell (Wayland)**. v2 is unproven on hardware — see `core/README.md`.

## 3. Configure

Everything machine-specific lives under `~/.config/tv-shell/` and is seeded
from the `config/*.example` files on first install.

### Streaming targets — `targets.json`

Single-line JSON array of Moonlight/Sunshine hosts. Fields:
`name`, `host`, `app`, `resolution`, `fps` (number), `codec`, `hdr` (bool), and
optional `sunshineUser`/`sunshinePass`/`sunshinePort` for pre-flight session
detection. Field semantics are documented in `config/targets.yaml.example`; the
copy-runnable starting point is `config/targets.json.example`. You can also
manage targets in-UI (Settings ▸ Widgets ▸ Moonlight). **Never pretty-print this
file** — the shell parses it line-by-line.

### Daemon config — `config.toml` (optional)

Per-machine daemon options, read directly by the daemon at startup: the LAN HTTP
bridge / MCP server binds, the auth toggle, CEC lifecycle, and the optional Plex /
Steam home-screen widgets. Every key is documented inline in
`config/config.toml.example`. The shared bearer token is **never inline** — it
lives in a separate `0600` file that `[http] token_file` points at (e.g.
`openssl rand -hex 32 > ~/.config/tv-shell/http-token`). Leave the file
untouched and the shell still boots fully.

> **Startup safety check:** the daemon **refuses to start** if a control surface
> is bound to a non-loopback address with dev tools on and auth effectively off
> (an unauthenticated RCE surface). The panel applies the identical refusal to
> its own `[panel] bind` when `[panel] token_file` is unset, reusing the same
> flag. Set `[dev] allow_insecure_lan = true` to override that on a box that
> genuinely wants the unauthenticated LAN dev loop.

#### Migrating an existing deploy (daemon.env → config.toml)

This release replaced the old `daemon.env` env file with `config.toml`. The
session script no longer sources `daemon.env`, so a box that relied on it for the
LAN bridge / MCP server **will silently lose those settings** (the surface just
won't come up) — and a box that ran a non-loopback bind with dev tools + no auth
will now hit the startup-refusal above. On such a box the deploy is no longer just
"restart the shell"; do it in this order so the daemon doesn't refuse to start:

1. Run the installer/migration so the new tree + `config.toml.example` are in place.
2. **Write `~/.config/tv-shell/config.toml`** translating the box's old
   `daemon.env` (and, if it ran LAN + dev + no-auth, include `[dev]
   allow_insecure_lan = true` — without it the daemon refuses to start). The old
   `TV_SHELL_*` env vars map to typed keys: HTTP/MCP → `[http]`/`[mcp]`, CEC →
   `[cec].lifecycle`, Plex/Steam → `[plex]`/`[steam]`, and observability
   (`TV_SHELL_LOG_JOURNAL` / `TV_SHELL_METRICS_TEXTFILE` /
   `TV_SHELL_METRICS_INTERVAL`) → `[observability]`. Only `RUST_LOG` stays an
   env var.
3. **Then** restart the daemon/shell.

For the canonical agent-native dev box (LAN bind + dev tools + auth off on a
trusted, firewalled single-user LAN), the equivalent `config.toml` is:

```toml
[http]
bind = "0.0.0.0:8089"
auth_enabled = false

[mcp]
bind = "0.0.0.0:8090"
dev = true

[dev]
# Required: without this the daemon refuses to start (LAN bind + dev + no auth).
allow_insecure_lan = true
```

To **lock such a box down** instead of preserving the insecure loop, drop the
`[dev]` block, set `[http] auth_enabled = true`, and point `[http] token_file` at
a `0600` token file (`openssl rand -hex 32 > ~/.config/tv-shell/http-token`).

### Display tuning — `hyprland-local.conf` (optional)

`config/hyprland.conf` sources `~/.config/tv-shell/hyprland-local.conf` if
present (ignored if absent). Drop your `monitor=` line and any HDR/VRR quirks
there — `config/hyprland.conf.example` is a worked 4K@120 10-bit HDR example.

## 4. Log in

Pick **TV Shell (Wayland)** in your display manager and log in. For a kiosk box
you'll usually also want autologin into this session — that's display-manager
specific (SDDM `autologin`, plasmalogin, GDM) and intentionally left to you.

## Optional: LAN control surface

Setting `[http] bind` / `[mcp] bind` in `config.toml` exposes the daemon's control
surface (screenshots, intents, MCP tools) over the network. See
[CONTROL_SURFACE.md](CONTROL_SURFACE.md). The web control panel (`tv-shell-panel`,
[PANEL.md](PANEL.md)) is separate — it's installed and enabled by default,
listening on `127.0.0.1:8091`. Widening `[panel] bind` beyond loopback now
**requires authentication**: set `[panel] token_file` to a 0600 token file inside
the config dir, or the panel refuses to start (the same refusal the daemon
applies, reusing the same `[dev] allow_insecure_lan` opt-in). The panel's
root-equivalent actions (deploy/build/reboot/suspend, `pacman -Syu`, the raw
IPC console at `/dev/console/raw`) are additionally gated behind `[panel] allow_dangerous = true`,
which is **off** by default. Firewall any of these ports you expose beyond
loopback yourself.

## Verify

After logging in, the daemon runs as a `systemd --user` unit — check it and its
logs:

```bash
systemctl --user status tv-shell-input         # active (running)
journalctl --user -u tv-shell-input -f         # daemon logs
journalctl --user -t tv-shell-quickshell -f    # quickshell output
```

If the LAN bridge is bound, `curl http://<host>:8089/metrics` returns Prometheus
metrics — `tv_shell_build_info` shows the deployed revision. Details in
[SYSTEMD_SETUP.md](SYSTEMD_SETUP.md) and [OBSERVABILITY.md](OBSERVABILITY.md).
