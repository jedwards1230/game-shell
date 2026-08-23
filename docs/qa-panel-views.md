# QA view catalog — web control panel

Every screen of `tv-shell-panel` and how to reach it, for manual or automated
verification.

This is the panel's counterpart to
[`qa-screenshot-views.md`](qa-screenshot-views.md), which covers the **QML TV
shell** — the on-TV home screen, QuickActions, the settings sidebar, gamepad
nav. The two surfaces share nothing: that one is driven over the IPC socket
with `intent`/key commands and captured with `grim`; this one is a web page
driven with HTTP and captured with a browser. Neither catalog covers the other.

> **Linked, not `@import`ed.** `CLAUDE.md` imports the shell catalog in full, so
> it loads into every session in this repo. This file is deliberately only
> linked, from [PANEL.md](PANEL.md) — most sessions never touch the panel, and
> doubling always-on context for them is a real cost.

## Reaching the panel at all

Two auth layers, and both stop headless tooling:

1. **Authentik forward-auth** sits on the public hostname
   `https://tv-shell.lilbro.cloud/`. A headless client gets a 302 to
   `auth.lilbro.cloud` it cannot pass. Use the LAN origin instead:
   **`http://192.168.8.50:8091`**.
2. **The panel's own token** — `[panel].token_file`, i.e.
   `~/.config/tv-shell/panel-token` on the device. This is **not** the daemon's
   `[http].token_file`; that one 401s here. Either send
   `Authorization: Bearer <panel-token>`, or `POST` the `token` field to
   `/login` for a session cookie (what a browser needs).

**The panel is a live control surface.** It restarts units, drives CEC (TV
power), and rewrites settings. Drive it by URL for QA; do not submit forms or
click destructive controls unless that specific behaviour is what you are
verifying.

## The 18 pages

`Gate` is the registration condition from `panel/src/capabilities.rs::NAV` —
authoritative there, mirrored here. A gated-off page is **not registered**: it
404s and vanishes from the sub-nav, rather than 403ing.

| Route | Group ▸ tab | Gate | Daemon down |
|---|---|---|---|
| `/` | Overview | `Recovery` | renders; unit/system tiles come from `systemctl`/`ps`, daemon-fed tiles degrade |
| `/system/services` | System ▸ Services | `Recovery` | **full function** — this is the page the panel exists for |
| `/system/processes` | System ▸ Processes | `Recovery` | top-processes table works (`ps`); Hyprland section shows its unavailable banner |
| `/system/updates` | System ▸ Updates | `Recovery` | full function (`checkupdates`, panel-local) |
| `/system/logs` | System ▸ Logs | `Recovery` | daemon pane empty, shell pane still read via `journalctl` |
| `/shell/appearance` | Shell ▸ Appearance | `SettingsStore` | **404** — group vanishes |
| `/shell/widgets` | Shell ▸ Widgets | `Widgets` | **404** |
| `/shell/apps` | Shell ▸ Apps | `SettingsStore` | **404** |
| `/shell/advanced` | Shell ▸ Advanced | `SettingsStore` | **404** |
| `/devices/controllers` | Devices ▸ Controllers | `Controllers` | **404** |
| `/devices/display-audio` | Devices ▸ Display & Audio | `SettingsStore` | **404** |
| `/devices/cec` | Devices ▸ CEC | `Cec` | **404** |
| `/devices/network` | Devices ▸ Network | `Node` | **404** |
| `/remote/navigation` | Remote ▸ Navigation | `Node` | **404** |
| `/remote/launcher` | Remote ▸ Launcher | `Node` | **404** |
| `/dev/recovery` | Dev ▸ Recovery | `Recovery` | renders; unit restarts work, bridge-backed actions degrade |
| `/dev/screenshot` | Dev ▸ Screenshot | `Screenshot` | **404** |
| `/dev/console` | Dev ▸ Console | `Node` | **404** |

### Separately reachable partials

These are polled or lazily swapped, so they break independently of the page
that hosts them and are worth probing directly:

| Partial | Trigger |
|---|---|
| `/nav/daemon-status` | every 10s, on every page — the drawer-footer dot |
| `/overview/tiles` | Overview, on load + poll |
| `/overview/services-tile` | Overview, own 30s poll |
| `/overview/updates-tile` | Overview, own slow poll |
| `/system/logs/view` | Logs, on refresh and on the "Errors only" preset |
| `/remote/launcher/list` | Launcher, on load and on submit |

## Cross-cutting states

These belong to no single page and are where regressions actually happen —
each one has bitten or nearly bitten at least once.

**Recovery mode** (handshake failed). Stop the daemon
(`systemctl --user stop tv-shell-input`) and restart the panel. The drawer must
collapse to exactly **Overview · System · Dev**; Shell, Devices and Remote
vanish entirely rather than rendering empty group shells; Dev keeps its group
but loses its sub-nav; the 12 gated pages 404 with an empty body, not a 500.
Restart the daemon afterwards. Pinned by
`capabilities.rs::recovery_mode_drawer_is_exactly_overview_system_and_dev`.

**REFUSED vs UNREACHABLE.** Identical gating, deliberately opposite operator
advice in the banner. Both are worth reading, since the whole point is that
they do not say the same thing.

**`allow_dangerous = false`** (the default, and what htpc-1 runs). Dev ▸ Console
renders an explanation and **no form**; Deploy, Build, Reboot, Suspend and the
full-update button are absent behind explanatory banners rather than present
and erroring.

**Empty `[panel].managed_units`.** System ▸ Services explains itself rather
than rendering an empty table, and still offers the read path — the inspector
works with no allowlist at all.

**System-scope restart with no sudoers line.** Fails **closed** with an
explicit refusal naming the unit and the missing NOPASSWD line — never a silent
no-op. On a node whose ansible run has applied
[`homelab-ansible#271`](https://github.com/jedwards1230/homelab-ansible/pull/271)
(htpc-1 has), the restart succeeds instead.

**Auth enabled.** `/login` renders the token form; an unauthenticated browser
navigation redirects there rather than 401ing blindly at the page.

**A group whose first page is gated off.** The drawer link must skip to the
first *registered* page in that group, not point at the gated one.

## Narrow viewport

The drawer wraps below ~700px — no JavaScript, no hamburger. At 380px it wraps
to two rows with all six groups and the daemon dot visible, and no page scrolls
horizontally at 380 or 700. The sub-nav still scrolls with a fade: a hidden tab
is a smaller loss than a hidden group.

## Capturing

Playwright against the LAN origin, logging in once at `/login`; the session
cookie then covers everything. Full-page captures at 1440×900 — most pages are
taller than the viewport, and the parts that regress (tables, the bottom of a
form) are usually below the fold.
