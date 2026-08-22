# Web Apps (PWAs)

Let the user add **web apps** (YouTube, Plex, Spotify, …) from the **web control
panel's Apps page** (`/shell/apps`), persist them, and have them appear on the home
rail and launch like any other local app — the same UX Plasma Bigscreen ships.
The couch UI's **Settings ▸ Web Apps** page lists the registry read-only; adding
lives on the panel because the TV has no on-screen keyboard yet (#20).

## Mechanism

A web app is just an XDG **`.desktop` file** in `~/.local/share/applications/`.
The daemon already scans that directory (`list-apps`, via the
`freedesktop-desktop-entry` crate), so a web app flows into the home
**Applications** row and `intent app:<wmClass>` **for free** — no new launch
plumbing. Each generated entry carries:

- `Exec=` a **Chromium `--app=<URL>`** launcher, with `--class=<stable-id>`,
  `--ozone-platform=wayland`, and a dedicated per-app `--user-data-dir`.
- `StartupWMClass=<stable-id>` matching `--class`, so the daemon's existing
  window matching (`app:<wmClass>` intent + window lifecycle) works unchanged.
- `X-TvShell-WebApp=true` — a marker key so we can list/edit/remove only our
  own entries and never touch foreign `.desktop` files. (The pre-rebrand
  `X-GameShell-WebApp=true` spelling is still accepted when detecting
  ownership, mirroring the header compatibility in `panel/src/bridge.rs`.)

### Why Chromium `--app`, not a QtWebEngine viewer

- Quickshell ships no WebEngine.
- Netflix / Plex need **Widevine**; Chromium provides it (plus hardware decode).
- Reusing the OS browser keeps logins and DRM working with zero extra code.

## Per-service guidance

| Service | Recommended surface | Why |
|---------|---------------------|-----|
| YouTube | **Web app** (Chromium `--app`) | No good native HTPC client; TV web UI is fine. |
| Netflix | **Web app** (Widevine) | Web is the only Linux path; needs Widevine. |
| Plex    | **Native (Flatpak)** | Dedicated 10-foot HTPC client; better than web. |
| Spotify | **Native (Flatpak)** | Mature native client; Connect + gapless. |

Web apps complement native apps — they don't replace the better native client
where one exists.

## Registry schema

The registry is a JSON array of entries. Each entry:

```json
{ "id": "youtube", "name": "YouTube", "url": "https://youtube.com/tv", "wmClass": "tvshell-youtube" }
```

- `id` — stable slug (also the registry key / user-data-dir suffix).
- `name` — display label on the rail and the Settings list.
- `url` — launched via Chromium `--app=<url>`.
- `wmClass` — the `--class` / `StartupWMClass` value; the window-matching key.

**Ownership: the daemon is the sole writer** (mirroring `settings.json`). QML
reads the registry via `SettingsStore.webApps` (a read-only mirror, `noSave` in
the settings schema) and never formats or writes it — the daemon owns
`.desktop` generation and the registry key (see Phases).

## Phases

- **P0 — Foundation (shipped):** this doc + `Web Apps` settings page (read-only
  stub listing registry entries) + `webApps` registry schema in `SettingsStore`
  (daemon-owned mirror) + qmldir / SettingsApp registration. No daemon/IPC yet.
- **P1 — Registry IPC (shipped):** daemon `webapp-list` / `webapp-add` /
  `webapp-remove` (see [IPC_PROTOCOL.md](IPC_PROTOCOL.md)). The daemon is the
  sole writer of both the registry and the generated entries.
- **P3 — Launcher generation (shipped, with P1):** Chromium `--app` launcher +
  `.desktop` writer with stable `--class` / `StartupWMClass` and per-app
  `--user-data-dir`. Shipped together with P1 on purpose: a registry whose
  entries launch nothing would be worse than the read-only P0 stub.
- **P2 — Add flow UI (shipped, on the panel):** the **web control panel's Apps
  page** (`/shell/apps`) is the add/remove surface — name + URL entry, the registry
  table, and removal. This is why P2 no longer blocks on the on-screen keyboard
  (#20): the panel has a real keyboard, and the couch UI stays read-only.
  Still deferred: curated presets (YouTube / Plex / Netflix / Spotify) for
  one-tap add, and icon fetch/fallback.
- **P4 — Polish (deferred):** edit-in-place, icon caching, preset refinement,
  optional per-app user-agent, and an on-TV add flow once #20 lands.

## Notes

- No credentials are committed — logins live in each web app's own
  `--user-data-dir` on the device.
- `intent settings:webapps` deep-links to the Settings page (generic
  `openSectionById` routing; no special-casing).
- Web apps require a Chromium-family browser on `PATH` (`chromium`,
  `chromium-browser`, `google-chrome[-stable]`, `brave-browser`, or
  `microsoft-edge-stable`). Without one, `webapp-add` fails loudly rather than
  writing a launcher that cannot run.
- Removing a web app keeps its `--user-data-dir` profile, so re-adding the same
  app restores its logins.

Tracking: [#187](https://github.com/jedwards1230/tv-shell/issues/187).
