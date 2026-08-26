# QA Screenshot Views

A living catalog of tv-shell views/overlays/states worth capturing in a visual-QA
screenshot batch. Keep this updated as views are added or changed.

## How to capture

See the `tv-shell-dev` skill ("Driving the UI for Screenshots"). In short there
are **two CLI channels** (see [IPC_PROTOCOL.md](IPC_PROTOCOL.md)):
- **Directional nav / select / back = real key events.** Either `wtype -k <Left|Right|Up|Down|Return|Escape>` (Wayland virtual keyboard) **or** the daemon's `key <name>` IPC (`key up|down|left|right|select|back` over the socket). Both reach the focused surface's `KeyNavigation`. **`Tab` toggles the nav drawer** when the shell window holds Wayland focus (K400 on the couch, `ShellLayout.Keys.onTabPressed → toggleMenu()`, idle only) — but `wtype -k Tab` from an external session may not reach the shell window if focus is elsewhere; prefer `intent menu` over the socket for reliable automation.
- **Drawer / settings / power / home = the `intent` control surface** (socket): `echo "intent menu" | nc -U /run/user/1000/tv-shell-input.sock` toggles the **left nav drawer**; `intent settings` / `intent power` / `intent home` open those. (At the TV the drawer also opens via gamepad **Home** tap or a bare **Super** press — Hyprland bind → `super-intent.sh` → `intent menu`. Super+Escape = escape; Super+Backspace = reset.) Deep-link targets also use this surface: `intent settings:<page>` opens a specific settings page in one command (e.g. `intent settings:bluetooth`); `intent overlay:volume` / `intent overlay:network` open the respective QAM popover; `intent app:<wmClass>` launches a local app by its StartupWMClass.
- Screenshots are 4K (~2000 tokens each) — shoot in **tiers** (below), not all at once.

## Home screen index map (QuickActions, top-right)

`0=Notifications, 1=Settings, 2=Widgets, 3=Theme toggle, 4=Network, 5=Volume, 6=Power`.
The Widgets glyph (⊞, index 2) opens the Widgets app — the **only** entry point for
it in the chrome now (the redundant nav-drawer Widgets row was removed). It's
glyph-only (no system icon theme on the target device). Left/Right move; Return activates; Down drops focus into the content regions below.
**Focus does not always start on this row** — with Continue/New content present it
starts on a card, so press **Up** first to reach the QuickActions row before
Left/Right.

**B (Back / Escape) on the home screen** resets focus to the default landing
position (top content row, first card). If already at the default position it is a
quiet no-op. B does **not** open Settings — use QuickActions idx 1 (→ Return) or
`intent settings` (socket) to reach Settings.

---

## A. Home screen — states & rows
| # | View | How to reach | Notes |
|---|------|--------------|-------|
| A1 | Home, full (idle) | default after restart | hero clock/date + QuickActions, then the enabled standardized widgets: Now Playing, Plex (On Deck + Recently Added), Apps (segmented `Recent` / `All Apps` horizontal rail + "Open Library" chip). No standalone All Apps tile — the Library is reached via the Apps widget's "Open Library" chip. When Settings ▸ Wallpaper has a wallpaper set (#29), HomeScreen renders it full-bleed behind the scrolling content, with a scroll-driven blur (past the hero row) and a theme-tinted readability scrim so foreground text/cards stay legible in both light and dark mode |
| A2 | Now Playing — medium | MPRIS active, size = medium (default) | `MediaWidget` card: cover art + progress bar + transport row |
| A3 | Now Playing — small | MPRIS active, size = small (Widgets page ▸ Now Playing) | `NowPlayingStrip` slim strip; both sizes collapse when nothing plays |
| A4 | Plex Recently Added — dynamic chips | Plex healthy, ≥2 media categories present | `FilterChips` show All + only categories present (no Music pill on a music-less library); re-filter live on item `kind` |
| A4b | Widgets app — list (L0) + config (L1) | QuickActions ▸ Widgets (idx 2) or `intent settings:widgets` (socket, rerouted) — no longer a nav-drawer row | The **Widgets app** (`WidgetsApp`, `shell.widgets` module — peer of Home/Library/Settings, no longer a Settings sidebar page). Schema-driven from the per-widget manifests. **L0 (`WidgetList`)**: rows (Moonlight/Now Playing/Plex/Apps/Steam) sorted by saved order, each one focus stop — **A** opens that widget's config (drill into L1), **X** toggles enable/disable in place, **←/→** reorder the widget (persists `widgets.<id>.order`, plasma-bigscreen style). **L1 (`WidgetConfig`)**: the Enabled toggle + manifest controls (Size + prefs like Hide-from-Recent, and — Moonlight only — the full server-management surface inlined below Size). **B** steps back (config → list → Home). Hint bar reads `A: Configure   X: Enable/Disable   ←→: Reorder   B: Back`. The **Steam** row (id `steam`, ships disabled) shows the same poster library as Moonlight's medium/large view, but activation launches Steam LOCALLY instead of streaming it. |
| A6 | Empty states | no running/recents, Plex empty/off | Continue + New rails collapse; B still lands on a focusable row (or the QuickActions row when every widget is empty) — never strands |
| A9 | Long-name marquee | card with long title | `MarqueeText` scroll |
| A10 | Controller battery glyph | wireless pad connected reporting charge | 🔋+% beside QuickActions; ⚡ when charging; crimson ≤15%; hidden when only wired pads or none (#100) |
| A11 | Plex widget — On Deck + Recently Added | `[plex]` configured in config.toml and the Widgets page ▸ Plex enabled | two poster rows (`PlexWidget`), size-scaled (small/medium); On Deck shows a resume bar; Recently Added carries the dynamic chips (A4) |
| A11b | Plex server-down notice | as A11 but Plex unreachable (down / 5xx) | inline `ServiceStatusNotice`: "Plex unavailable" — both Plex rails collapse, focus chain still walks (service-health bus) |

## A12. Library — secondary browse surface
| # | View | How to reach | Notes |
|---|------|--------------|-------|
| A12 | Library — full | Apps widget ▸ "Open Library" chip → A | "Library" header + Moonlight section + Applications grid; B returns to Home with focus restored |
| A12a | Moonlight — servers | Library | server cards (`StreamCard`); servers-only (the apps-view toggle was removed) |
| A12c | Applications grid | local launchers present | full `AppDiscoveryManager.applications` as a vertical wrapping `NavigableGrid` (scrolls within the Library Flickable) |

## B. Context menus / popovers
| # | View | How to reach |
|---|------|--------------|
| B10 | App card context menu (`PopoverMenu`) | focus an app card → context key (Focus / Close) |
| B11 | Stream card context menu | focus a stream card → context key (Resume / Quit) |
| B12 | Nav-drawer row context menu | open the drawer (C12) → arrow to a running-app row → **X** | Resume / Quit App / **Mute App** (the third item's label reads "Unmute App" when that app is already user-muted). On a window that reports no class the mute item renders disabled with a hint saying why, rather than vanishing. |

## C. Overlays & dialogs
| # | View | How to reach | Capturability |
|---|------|--------------|---------------|
| C12 | Left nav drawer (`NavigationDrawer`) | `intent menu` (socket) — or gamepad Home / bare Super at the TV — or `Tab` when the shell window holds Wayland keyboard focus | socket-reachable; `wtype -k Tab` works only when shell window has focus (unreliable in automation) |
| C12a | Nav drawer — per-row audio indicators | as C12, with an app producing sound (e.g. a live stream) and/or an app the user has muted via B12 | Two **conditionally rendered** glyphs per running-app row, so a row with no audio story looks exactly as it did before they existed. 🔊 = **this app has a live playback stream** — the informative one, since it names what is making noise you cannot hear; it is NOT "is audible". 🔇 (crimson) = **the user muted this app by hand** — never the automatic workspace mute, which is true of nearly every app at any moment and would light up every row. Both survive a brief gap in the stream (a ~12s latch) so they do not blink on the 5s audio sweep. |
| C13 | Notification center | QuickActions idx 0 → Return | wtype |
| C14 | Notification center — empty | as above, no notifications | wtype |
| C15 | Notification toast (`NotificationToast`) | trigger a notification | transient; timing-sensitive |
| C16 | Power overlay (`PowerOverlay`) | QuickActions idx 6 → Return | wtype |
| C17 | Session conflict dialog (`SessionDialog`) | real stream conflict | needs live conflict / mock |
| C18 | Stream overlay (`StreamOverlay`) | launching / reconnecting / error | needs active/failing stream |
| C19 | Error log viewer (`ErrorLogViewer`) | notification center → error log | wtype |
| C20 | Volume QAM popover (`VolumeOverlay`) | home QuickActions idx 5 → Return; also reachable from the nav drawer; also `intent overlay:volume` (socket) | wtype |
| C21 | Network QAM popover (`NetworkOverlay`) | home QuickActions idx 4 → Return; also reachable from the nav drawer; also `intent overlay:network` (socket). On a **wired/ethernet** link it's status-only — the disconnect/disable toggle (and its confirm + divider + "A: Toggle" hint) are hidden so the couch can't strand a wired box; the toggle appears **only on Wi-Fi**. | wtype |

## D. Settings panel + pages + substates
| # | View | How to reach / notes |
|---|------|----------------------|
| D20 | Settings sidebar (panel open) | QuickActions idx 1 → Return |
| D21–31 | Pages: Audio, Bluetooth, Network, Display (+Appearance/theme), Wallpaper, Controllers, Key Bindings, AV Control, Web Apps, Accessibility, Power, System (+Storage). The **Web Apps** page (#187, P0) is a read-only stub: it lists the daemon-owned web-app registry (`SettingsStore.webApps`) or an empty state ("No web apps yet") when none exist — the add/remove flow lands in later phases. **Widgets is no longer a sidebar page** — it's the top-level Widgets app (A4b); `intent settings:widgets` reroutes there. **Moonlight server management is no longer a sidebar page** either — it's inlined on the Widgets ▸ Moonlight config page; `intent settings:moonlight` / `settings:streaming` reroute there too. | Down/Up move the sidebar **cursor only** — the content pane does **not** follow it. Press **Return** to load the focused page (focus stays on the sidebar). `Right` then enters the *loaded* page's controls; it does **not** switch pages. So per page: Down/Up → **Return** → screenshot. Each sidebar page is also directly reachable via `intent settings:<id>` (socket) — id slugs: `audio`, `bluetooth`, `network`, `display`, `wallpaper`, `controllers`, `keybindings`, `avcontrol`, `webapps`, `accessibility`, `power`, `system`. `widgets` reroutes to the top-level Widgets app; `streaming`/`moonlight` reroute to its Widgets ▸ Moonlight config page (none are sidebar pages). Theme mode selector (auto/light/dark) is part of the **Display** page. Free-space storage readout is part of the **System** page. Display page (#127): now reads monitors via daemon `hypr-monitors` IPC (replaces `hyprctl monitors -j` shell-out); shows live HDR status (read-only, driven by daemon `hdr` field), HDR toggle (persists + applies via `hyprctl keyword monitor` with/without `bitdepth,10,cm,hdr` suffix), separate Refresh Rate dropdown (filters `availableModes` to current resolution), Night Light toggle + color-temperature dropdown (applies via `hyprsunset`, requires hyprsunset), Overscan stepper (persists safe-area pct). **Wallpaper page (#29)**: a controller-navigable grid of a synthetic "None" tile + every image dropped into `~/.config/tv-shell/wallpapers/` (read-only `FolderListModel`); A selects (persists `SettingsStore.wallpaperPath` as a plain filesystem path), a ✓ marks the current selection; empty folder still shows the None tile plus a hint to drop images in. |
| — | Bluetooth — scanning + device list | substate |
| — | Network — Wi-Fi list / connect | substate |
| — | Network — gateway/DNS card + test-connection result | substate (net-status now carries `gateway`, `dns`, and per-connection `speed`; page shows a Gateway/DNS read-only card and a Test-connection action with OK/Failed inline result) |
| — | Controllers — pad connected vs none | substate |
| — | Key Bindings — capture mode ("press a button") | substate |
| — | AV Control — CEC device info populated | substate — reads `cec-scan` JSON from daemon + subscribes to `cec:device:*`/`cec:power:*` events (#16) |
| — | AV Control — Focus preference toggles | always-visible "Focus Preferences" section: "Focus TV on startup" (default Off) and "Focus TV on wake from sleep" (default On); render correctly even when CEC is unavailable |
| — | AV Control — CEC link status line (#19) | substate — a status line below the header distinguishes three states from the daemon's `cec-health` IPC (+ `cec:health:*` events): **OK** (`CEC link: OK`, green) when transmits succeed; **transmit failing / wedged** (`CEC transmit failing — the adapter may be wedged…`, ember/warning, wraps) when the adapter opens + receives but every transmit fails; and **unavailable** (line hidden — the "HDMI-CEC Not Available" card owns that state). A `checking…` neutral line shows before the first probe. A **Test CEC** button beside Refresh fires `cec-test` on demand and reports the result via the action-feedback line. |
| — | AV Control — CEC unavailable card, per reason (#22) | substate — the unavailable card (shown via `!cecAvailable`) now reads its title + body off the daemon health reply's `reason` field, so the three distinct unavailable causes no longer collapse into one misleading message: **`no_libcec`** → "HDMI-CEC Not Available" / "CEC requires the daemon built with libcec support." (neutral, the original copy); **`no_adapter`** → "No CEC Adapter" / "No CEC adapter detected — plug in the USB CEC adapter." (neutral); **`adapter_open_failed`** → "CEC Adapter Not Responding" / "CEC adapter detected but not responding — re-seat the USB adapter or power-cycle the AVR…" rendered as an **ember/warning** card (warning title + border) because it is actionable — the adapter is physically present but hardware-wedged. Footer hint mirrors the reason. Before the first `cec-health` reply (reason unknown) the generic neutral copy shows as a safe fallback. |
| — | Moonlight — add/edit server form | substate, reached via Widgets ▸ Moonlight ▸ Add Server (inline) |
| — | Display — each theme mode selected (auto/light/dark) | substate in Display page Appearance section |
| — | Display — live external reload | QA: edit `~/.config/tv-shell/settings.json` over SSH (e.g. flip `themeMode` `dark`→`light`) while the shell is open; confirm the theme switches without a Quickshell restart. The daemon broadcasts `config:changed` and `SettingsStore` re-fetches via `get-config`. No new screenshot view — the existing theme substates cover the visual. |
| — | Accessibility — Reduce Motion on/off; Text Size Default/Large/Larger | substate |
| — | Audio — default-sink persistence (by node.name, re-applied on boot), 5.1 speaker-test buttons (FL/FR/Center/LFE/RL/RR + All channels), sample-rate/format read-out | substate |
| — | Power — sleep-timer cycle (Off/5/10/15/30/60 min), wake-on-controller toggle (On/Off), End session button reachable via `intent settings:power` | substate — the auto-suspend idle timer lives at the shell root and fires regardless of which settings page is open |

> **#141**: All list-bearing settings pages (Network ×2, Bluetooth ×2, Moonlight, Display, Controllers) now share `SettingsList` for row-count sizing — the floating-gap regression class (#123/#139) is centralized. QA: verify lists pack directly under their headers with no gap in both dark and light mode. The Display page still uses `SettingsList` for the monitor list (#127 did not change that).

## E. Theme variants (multiplier)
Capture at least **home, a settings page, notification center, power overlay** in
both **light** and **dark** mode (toggle via QuickActions idx 3). Full rigor = every
view ×2.

## F. Input-mode variants
Same view in **controller mode** (crimson focus borders) and **mouse mode** (hover
highlights + cursor). Relevant to the #45 mouse-mode work.

## G. Transient / condition-dependent (flag, don't block a batch)
Launching state, streaming (LIVE badge), `appRunning` overlay drawer — only
capturable with a live stream/app.

---

## Capturability summary
- **Key-driven (nav/select/back via `wtype -k` or `key <name>`):** A1–A9, B10–B11, C13–C14, C16, C19, E, F; D20 and (once open) the D21–D31 page controls.
- **Socket deep-link (`intent` command):** D21–D31 settings pages (`intent settings:<page>`) and C20/C21 overlays (`intent overlay:volume` / `intent overlay:network`) are directly socket-reachable in one command without navigating through the sidebar or QuickActions.
- **Drawer (C12):** `intent menu` over the socket (or gamepad Home / bare Super at the TV) is the reliable path; `Tab` also works when the shell window holds Wayland keyboard focus, but is unreliable from an external automation session.
- **Needs a real condition:** C15 (toast timing), C17 (stream conflict), C18 (stream overlay), G (live stream/app).

## Suggested tiered batch
1. **Tier 1 — static views, dark mode:** A1–A9, B10–B11, D20–D31 + settings substates, C13/C14/C16/C19.
2. **Tier 2 — light mode** re-shoot of the core set (E).
3. **Tier 3 — input-mode** variants (F) where visually distinct.
4. **Tier 4 — manual/condition:** drawer (C12, `intent menu` over the socket or a TV press; or `Tab` with direct keyboard focus), then condition-dependent (C15/C17/C18/G).
