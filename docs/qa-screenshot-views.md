# QA Screenshot Views

A living catalog of game-shell views/overlays/states worth capturing in a visual-QA
screenshot batch. Keep this updated as views are added or changed.

## How to capture

See the `game-shell-dev` skill ("Driving the UI for Screenshots"). In short there
are **two CLI channels** (see [IPC_PROTOCOL.md](IPC_PROTOCOL.md)):
- **Directional nav / select / back = real key events.** Either `wtype -k <Left|Right|Up|Down|Return|Escape>` (Wayland virtual keyboard) **or** the daemon's `key <name>` IPC (`key up|down|left|right|select|back` over the socket). Both reach the focused surface's `KeyNavigation`. There is **no `Tab` drawer key** — `wtype -k Tab` does nothing useful.
- **Drawer / settings / power / home = the `intent` control surface** (socket): `echo "intent menu" | nc -U /run/user/1000/game-shell-input.sock` toggles the **left nav drawer**; `intent settings` / `intent power` / `intent home` open those. (At the TV the drawer also opens via gamepad **Home** tap or a bare **Super** press — Hyprland bind → `super-intent.sh` → `intent menu`. Super+Escape = escape; Super+Backspace = reset.)
- Screenshots are 4K (~2000 tokens each) — shoot in **tiers** (below), not all at once.

## Home screen index map (QuickActions, top-right)

`0=Notifications, 1=Settings, 2=Theme toggle, 3=Network, 4=Volume, 5=Power`.
Left/Right move; Return activates; Down drops focus into the app rows.
`streamingViewMode` ∈ `"servers"` | `"apps"` toggles two different home layouts.

---

## A. Home screen — states & rows
| # | View | How to reach | Notes |
|---|------|--------------|-------|
| A1 | Home, full (idle) | default after restart | hero clock/date + QuickActions + rows |
| A2 | Running row | apps running | `runningWindows.length > 0` |
| A3 | Recent row | after launching apps | `RecentsTracker.recentApps` |
| A4 | Moonlight row — "servers" view | `streamingViewMode = servers` | server cards |
| A5 | App rows — "apps" view | `streamingViewMode = apps` | per-host "Moonlight — \<host\>" rows |
| A6 | Applications row | local launchers present | `appsRow` |
| A7 | Empty states | no running / recents / targets | verify layout holds when rows hide |
| A8 | App-view substates | apps view, host discovering/offline | "Discovering apps…" / "Offline or no apps found" |
| A9 | Long-name marquee | card with long title | `MarqueeText` scroll |

## B. Context menus / popovers
| # | View | How to reach |
|---|------|--------------|
| B10 | App card context menu (`PopoverMenu`) | focus an app card → context key (Focus / Close) |
| B11 | Stream card context menu | focus a stream card → context key (Resume / Quit) |

## C. Overlays & dialogs
| # | View | How to reach | Capturability |
|---|------|--------------|---------------|
| C12 | Left nav drawer (`NavigationDrawer`) | `intent menu` (socket) — or gamepad Home / bare Super at the TV | socket-reachable (NOT `wtype` — no Tab handler) |
| C13 | Notification center | QuickActions idx 0 → Return | wtype |
| C14 | Notification center — empty | as above, no notifications | wtype |
| C15 | Notification toast (`NotificationToast`) | trigger a notification | transient; timing-sensitive |
| C16 | Power overlay (`PowerOverlay`) | QuickActions idx 5 → Return | wtype |
| C17 | Session conflict dialog (`SessionDialog`) | real stream conflict | needs live conflict / mock |
| C18 | Stream overlay (`StreamOverlay`) | launching / reconnecting / error | needs active/failing stream |
| C19 | Error log viewer (`ErrorLogViewer`) | notification center → error log | wtype |

## D. Settings panel + pages + substates
| # | View | How to reach / notes |
|---|------|----------------------|
| D20 | Settings sidebar (panel open) | QuickActions idx 1 → Return |
| D21–30 | Pages: Audio, Bluetooth, Network, Display, Controllers, Key Bindings, AV Control, Moonlight, Appearance, Power | Down/Up sidebar, Right enters content |
| — | Bluetooth — scanning + device list | substate |
| — | Network — Wi-Fi list / connect | substate |
| — | Controllers — pad connected vs none | substate |
| — | Key Bindings — capture mode ("press a button") | substate |
| — | AV Control — CEC device info populated | substate |
| — | Moonlight — add/edit server form + servers/apps toggle | substate |
| — | Appearance — each theme mode selected (auto/light/dark) | substate |

## E. Theme variants (multiplier)
Capture at least **home, a settings page, notification center, power overlay** in
both **light** and **dark** mode (toggle via QuickActions idx 2). Full rigor = every
view ×2.

## F. Input-mode variants
Same view in **controller mode** (crimson focus borders) and **mouse mode** (hover
highlights + cursor). Relevant to the #45 mouse-mode work.

## G. Transient / condition-dependent (flag, don't block a batch)
Launching state, streaming (LIVE badge), `appRunning` overlay drawer — only
capturable with a live stream/app.

---

## Capturability summary
- **Key-driven (nav/select/back via `wtype -k` or `key <name>`):** A1–A9, B10–B11, C13–C14, C16, C19, all of D, E, F.
- **Drawer (C12):** `intent menu` over the socket (or gamepad Home / bare Super at the TV) — **not** `wtype` (no Tab handler).
- **Needs a real condition:** C15 (toast timing), C17 (stream conflict), C18 (stream overlay), G (live stream/app).

## Suggested tiered batch
1. **Tier 1 — static views, dark mode:** A1–A9, B10–B11, D20–D30 + settings substates, C13/C14/C16/C19.
2. **Tier 2 — light mode** re-shoot of the core set (E).
3. **Tier 3 — input-mode** variants (F) where visually distinct.
4. **Tier 4 — manual/condition:** drawer (C12, `intent menu` over the socket or a TV press), then condition-dependent (C15/C17/C18/G).
