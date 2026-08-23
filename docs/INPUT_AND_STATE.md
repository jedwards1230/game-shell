# Input & State Reference

The canonical reference for the shell's **state machine** and **input semantics**:
in a given state and context, what does each button do, and what changes. The
top-level state machine lives in `shell/shell.qml` (`states:`), but its
transitions are scattered as `root.state = "…"` assignments, and button-by-context
behavior is split across three layers. This document is the single map.

> **Source of truth.** Everything here is derived from the QML — primarily
> `shell/shell.qml`, `shell/components/ShellLayout.qml`, and
> `shell/components/InputManager.qml` — plus the daemon contract in
> [`IPC_PROTOCOL.md`](IPC_PROTOCOL.md). When the code and this doc disagree, the
> code wins; fix the doc. Where the code is ambiguous it is flagged inline.

## Contents

1. [The two input channels](#1-the-two-input-channels)
2. [State machine](#2-state-machine)
3. [Focus model](#3-focus-model)
4. [Input-semantics matrix](#4-input-semantics-matrix)
5. [Back-button (B / Escape) precedence](#5-back-button-b--escape-precedence)
6. [The `intent` control surface](#6-the-intent-control-surface)
7. [Gotchas](#7-gotchas)

---

## 1. The two input channels

Navigation reaches the shell over **two distinct channels**. Keeping them
separate is the central design idea — the daemon owns no focus/state knowledge,
so anything that needs to know "what is focused right now" must arrive as a real
key event, not an intent.

| Channel | What it carries | How it arrives | Who decides the effect |
|---------|-----------------|----------------|------------------------|
| **A. Directional / select / back** (real key events) | Focus moves + confirm + cancel: `up/down/left/right`, `select` (A→`Enter`), `back` (B→`Esc`) | Gamepad d-pad/A/B synthesized to `KEY_*` by the daemon; the K400 keyboard via Wayland; `wtype -k`; the daemon's [`key <name>`](IPC_PROTOCOL.md#key-name) IPC; CEC remote (lifecycle mode) | The **focused** QML surface's `KeyNavigation` / `Keys.on*Pressed` |
| **B. High-level actions** (`intent:*` broadcast) | Coarse, focus-*independent* control: `menu/home/home-tap/home-hold/settings/power` + deep-links (`settings:<page>`, `overlay:<target>`, `app:<wmClass>`) | The daemon's [`intent <name>`](IPC_PROTOCOL.md#intent-name) command — issued by the gamepad Home neutral, the Hyprland `Super` binds (`scripts/super-intent.sh`), the LAN HTTP bridge (`POST /intent/*`), or automation | `InputManager.qml` signal handlers in `shell.qml`, gated by `root.state` |

The daemon **does not read the keyboard**. The K400 belongs to the compositor +
QML (Wayland focus / `Keys`). Hyprland `Super` binds inject intents via
`scripts/super-intent.sh`:

| Keyboard | Intent | Effect |
|----------|--------|--------|
| bare `Super` | `intent menu` | Toggle the nav drawer (home screen only) |
| `Super+Escape` | `intent home` | Global return-to-shell escape |
| `Super+Backspace` | `intent home-hold` | Reset to a clean home screen |
| `Tab` (K400, direct Wayland) | — | `ShellLayout.Keys.onTabPressed → toggleMenu()` (idle only) |

See [the IPC protocol](IPC_PROTOCOL.md) for the full wire contract; this doc does
not re-document command framing.

---

## 2. State machine

The top-level state is `root.state` (a string property on `ShellRoot` in
`shell.qml`), hosted on an internal `Item { id: stateMachine }` because
`ShellRoot` doesn't inherit `Item`. There are **five** states.

```
                   onStreamStarted
        ┌──────────────────────────────────────────┐
        │                                           ▼
   ┌─────────┐  streamRequested   ┌───────────┐  ┌───────────┐
   │  idle   │ ─────────────────▶ │ launching │  │ streaming │
   │ (shell) │                    └───────────┘  └─────┬─────┘
   └─────────┘                          │              │ onStreamCrashed
     ▲  ▲ ▲ │ appLaunched               │ onStreamStarted     │
     │  │ │ └──────────────────────┐    ▼              ▼
     │  │ │                        │  (streaming)  ┌──────────────┐
     │  │ │ returnToShell()        │               │ reconnecting │
     │  │ └────────────────────────┼───────────────┴──────┬───────┘
     │  │   (stream end/suspend/    │  onStreamFailed      │
     │  │    crash-give-up,         └──────────────────────┘
     │  │    app close, forceQuit,  reconnect succeeds → streaming
     │  │    intent:home)
     │  │
     │  └─────────── checkAndLaunchApp → onAppLaunched ──┐
     │                                                   ▼
     │            returnToShell()                  ┌────────────┐
     └────────────────────────────────────────────┤ appRunning │
                  (app close, intent:home,         └────────────┘
                   home-hold, forceQuit)
```

### States — what each owns/shows

| State | Shell window visible? | What it shows / owns |
|-------|----------------------|----------------------|
| `idle` | Yes (full) | The shell proper: HomeScreen, nav drawer, settings panel, all overlays. The auto-suspend idle timer arms only here. On entry, forces `overlayDrawerOpen=false` (with `restoreEntryValues:false`). |
| `launching` | **No** | Transient: a stream launch is in flight (`StreamManager.launch`). Shell window hidden; `StreamOverlay` shows progress. |
| `streaming` | **No** | A Moonlight stream owns the screen. Shell window hidden so Hyprland direct-scanout works. |
| `reconnecting` | **No** | Stream dropped; `StreamManager` is retrying (`StreamOverlay` shows reconnect status). |
| `appRunning` | **No**, *except* when `overlayDrawerOpen` | A local app owns the screen. The shell window is mapped **only** when the overlay drawer is toggled open (`visible: … || root.overlayDrawerOpen`); its `color` is `transparent`. |

`PanelWindow.visible` is precisely:

```qml
visible: (root.state !== "appRunning" && root.state !== "streaming"
          && root.state !== "reconnecting" && root.state !== "launching")
         || root.overlayDrawerOpen
```

i.e. the shell is on-screen in `idle`, or in `appRunning` while the overlay
drawer is open.

### Transition table (trigger → from → to)

All transitions are imperative `root.state = "…"` assignments wired from manager
signals in `shell.qml`. The only declarative `Transition` is `from:"*" to:"idle"`
(it carries the `overlayDrawerOpen=false` `PropertyChanges`, not a logic
transition).

| Trigger (signal / call) | From | To | Notes |
|-------------------------|------|----|-------|
| `ShellLayout.onStreamRequested` | `idle` | `launching` | Also `avController.forceWake()` + `streamManager.launch(target)` |
| `StreamManager.onStreamStarted` | `launching` | `streaming` | Confirmation rumble (120 ms) |
| `StreamManager.onStreamCrashed` | `streaming` | `reconnecting` | Stronger rumble (250 ms) |
| (reconnect succeeds → `onStreamStarted`) | `reconnecting` | `streaming` | |
| `StreamManager.onStreamFailed` | `reconnecting`/* | `idle` | Terminal failure; re-`grab()`s input; 250 ms rumble |
| `StreamManager.onStreamEnded` / `onStreamSuspended` | `streaming`/`reconnecting` | `idle` | via `returnToShell()` |
| `StreamManager.onSessionCheckCancelled` | (any) | `idle` | re-`grab()` + `focusHome()` |
| `AppLifecycleManager.onAppLaunched` | `idle` | `appRunning` | 120 ms rumble |
| `AppLifecycleManager.onAppClosed` | `appRunning` | `idle` | clears `runningAppClass`, `returnToShell()` |
| `intent:home` (`onIntentHome`) | `appRunning` (or any) | `idle` | Global escape — `returnToShell()`, always leaves the app |
| `intent:home-hold` (`onIntentHomeHold` → `resetToHome()`) | `appRunning` | `idle` | Over an app → return to shell |
| `forceQuit()` (`combo:force-quit`) | any | `idle` | Kills stream, closes app, dismisses every overlay, `focusHome()` |
| `endSession()` (`combo:end-session`) | any | (external) | Runs `/usr/local/bin/end-game-session` (leaves the GUI session) |

The three reset/return functions in `shell.qml`:

- **`returnToShell()`** — `state="idle"`, re-`grab()`, hide `StreamOverlay`, hide
  settings panel + power overlay, `focusHome()`.
- **`resetToHome()`** (intent:home-hold) — over an app → `returnToShell()`; on
  the home screen → dismiss **every** drawer/overlay (nav drawer, settings,
  notification center, power) and `focusHome()`.
- **`forceQuit()`** (the force-quit combo) — the nuclear option: kill stream,
  close app, dismiss everything (incl. `sessionDialog`), `focusHome()`.

---

## 3. Focus model

Focus is pure Wayland/QML — `WlrLayershell.keyboardFocus: Exclusive` on the
shell `PanelWindow` so non-Hyprland-bound keys (arrows, Enter, Esc) reach the
focused QML widget. When an app or stream owns the screen the shell window isn't
focused at all, so its `Keys` handlers can't fire over a running app — only the
`intent` channel reaches the shell in those states.

### FocusScope hierarchy

```
PanelWindow (Exclusive keyboard focus)
└── ShellLayout  (FocusScope, id: layout — root._layout)
    ├── ScreenManager     (non-visual — routes the Home/Library/Settings layer:
    │                      push("library"|"settings",…) / popToHome(). Reacts to
    │                      `closed` signals; never intercepts Escape. Visibility &
    │                      focus BINDINGS stay declarative on the surfaces below.)
    ├── HomeScreen        (visible & focus only in idle, no overlay/Library open)
    │   └── home widgets  (Now-Playing [strip|card], Plex [On Deck + Recently
    │       Added + chips], Apps [Recent | All Apps segments] — one ordered focus
    │       list; no standalone All Apps tile)
    ├── LibraryScreen     (secondary browse surface, z:30 — Moonlight rows +
    │                      Applications; opened from the Apps widget's Open Library chip)
    ├── SettingsApp     (Rectangle — NOT a FocusScope; sidebar + Loader page;
    │                      own module shell.settings, public open/openPage/close)
    ├── NavigationDrawer  (idle nav drawer, z:50)
    ├── NotificationCenter / ErrorLogViewer (z:60)
    ├── PowerOverlay      (z:60)
    ├── VolumeOverlay / NetworkOverlay (z:70 — anchored QAM popovers)
    ├── StreamOverlay / SessionDialog
    └── Overlay drawer    (appRunning + overlayDrawerOpen, z:50)
        └── NavigationDrawer (overlayMode:true)
```

### Who holds `activeFocus`

`HomeScreen`'s `focus:` binding is the single declarative gate — it claims focus
only when nothing else is open:

```qml
focus: root.shellState === "idle" && !libraryScreen.visible
       && !settingsApp.visible && !navDrawer.opened
       && !notificationCenter.opened && !powerOverlay.opened
       && !networkOverlay.opened && !volumeOverlay.opened
```

| Context (idle) | Focus owner |
|----------------|-------------|
| Home screen | A focus region inside `HomeScreen` (the home `focus:` binding routes Wayland focus into it) |
| Library open | `LibraryScreen` — its own region chain (Moonlight rows + Applications); B/Escape emits `closed`, `homeFocusTimer` restores Home |
| Settings panel open | `SettingsApp` — specifically `sidebarList`, until you `Right`/`Return` into a page |
| Nav drawer open | `NavigationDrawer` (`navList`, or `drawerActions` quick-actions row) |
| Notification center | `NotificationCenter` (modal — `event.accepted = true` on every key) |
| Power overlay | `PowerOverlay` (modal) |
| Volume / Network QAM popover | `VolumeOverlay` / `NetworkOverlay` (z:70, on top of the drawer if launched from it) |
| `appRunning` + overlay drawer open | the overlay `NavigationDrawer` |
| `appRunning` (drawer closed) / `streaming` / `reconnecting` / `launching` | **The app/stream** — the shell window is unfocused; only intents reach the shell |

### Focus conventions (the "defer focus" rule)

Quickshell evaluates declarative `focus:` bindings on its own schedule, so a
synchronous `forceActiveFocus()` can be immediately stolen back by a sibling's
`focus:` binding. The codebase uses two deferral conventions:

- **`homeFocusTimer`** (`ShellLayout`, 50 ms `Timer`) — the standard
  "return focus to Home" path. `focusHome()` just `restart()`s it. On fire it
  bails if any overlay/panel is still open, else `homeScreen.forceActiveFocus()`.
  Used after a drawer/overlay/panel closes.
- **`Qt.callLater`** — `HomeScreen.focusDefaultPosition()` and
  `ShellLayout.focusDefaultPosition()` defer one event-loop tick so layout +
  declarative focus bindings settle first; otherwise a row's `focus:` binding
  can steal focus after another row's `forceActiveFocus()`. This is the
  **"defer focus to after the FocusScope is realized"** rule.

`SettingsApp` is a plain `Rectangle`, not a `FocusScope` — so its helpers call
`sidebarList.forceActiveFocus()` directly and **avoid** a trailing
`root.forceActiveFocus()` that would steal focus back from the sidebar.

When a QAM popover (Volume/Network) closes, `_returnFocusAfterOverlay()` routes
focus back to the nav drawer's quick-actions row if the drawer is still open
underneath, else falls back to `homeFocusTimer`.

### Home-widget focus contract (one ordered list)

The home screen is a stack of **standardized widgets** (#249), each reading its
own `enabled` + `size` from `SettingsStore` (via `Theme`) and implementing one
duck-typed contract so `HomeScreen` drives focus from a single ordered list
rather than hardcoding each widget by name:

| Member | Meaning |
|--------|---------|
| `visible` | widget occupies layout space (off when disabled or empty) |
| `regionFocused` | this widget currently holds focus (`activeFocus`) |
| `focusFirstChild()` | focus its first *selectable* child; returns `false` when it has none (disabled, hidden, empty, or a filtered-empty row) |

`HomeScreen._contentRegions()` returns these in top→bottom order —
`[nowPlayingStrip, nowPlayingCard, plexWidget, appsRow]` — and
the three focus helpers iterate it. Now-Playing has two size renderers
(`small` = `NowPlayingStrip`, `medium` = `MediaWidget`); only the size-matching
one is visible, the other reports `focusFirstChild()===false`. The always-present
**QuickActions row** (`statusIcons`, the WidgetHost `topAnchor`) is the guaranteed
non-stranding fallback when every widget is empty — there is no terminal All Apps
tile and the WidgetHost `bottomAnchor` is null. The full browse catalog (Moonlight
servers / per-host app-view / the complete Applications **grid** — a vertical
wrapping `NavigableGrid`, not a rail) lives in `LibraryScreen`, which keeps
its own identical region chain and `focusDefaultPosition()`, reached via the Apps
widget's "Open Library" chip and dismissed with B. The three home focus helpers:

- **`focusDefaultPosition()`** (the B / "back to home" handler) — snap
  `scrollView.contentY = 0`, then focus the first region whose
  `focusFirstChild()` succeeds. A region that can't take focus is skipped, so B
  never strands focus on an invisible row.
- **`_focusFirstVisibleRow()`** — same walk, used as the QuickActions "Down"
  target and the post-popover-close landing.
- **`_reanchorFocusIfNeeded()`** (150 ms safety-net timer) — if HomeScreen holds
  focus but no region reports `regionFocused`, re-anchor via the same list.

Adding a new home widget is therefore "implement the contract + `enabled`/`size`,
insert it into `_contentRegions()`, wire its `previousRow`/`nextRow`, and add a
row + config sub-page to the Widgets settings list (`WidgetsSettings.qml`)" — no
edits to the focus helpers. Each region's
`previousRow`/`nextRow` points at its immediate neighbour; every up/down walker —
`NavigableRow`, `NavigableGrid`, `WakeCard`, and the `Widget` base itself — shares
ONE traversal helper (`shell/components/lib/focusChain.js`) that follows that chain
and skips any neighbour whose `canFocus` is false (falling back to `visible` when a
neighbour predates the contract), so a disabled widget or a filtered-empty row is
transparently stepped over. (The helper replaced four verbatim copies of the walk
plus a fifth, subtly divergent `visible`-only variant in `WakeCard`.) The **Plex** widget owns its own `plex-hubs` `ServiceMonitor` (On
Deck + Recently Added rows, the latter with dynamic category chips); when the
server is unreachable the rows collapse and an inline `ServiceStatusNotice`
renders in their place.

---

## 4. Input-semantics matrix

For each context, what **A / B / D-pad / Home / Menu** do and which layer handles
each. "A" = `select` (Enter), "B" = `back` (Escape). **Home** and **Menu** are
intents (channel B); the rest are real key events (channel A).

| Context | D-pad | A (select) | B (back) | Home tap | Menu (`Super`/Home-tap on home) | Handler layer |
|---------|-------|-----------|----------|----------|--------------------------------|---------------|
| **Home screen** (idle) | Move between cards / rows (`NavigableRow`, `KeyNavigation`) | Launch focused stream/app card; activate QuickActions glyph | Reset to default landing position (first card, top row); quiet no-op if already there. **Does NOT open Settings.** | `intent:home-tap` → `toggleMenu()` (nav drawer) | Open nav drawer | `HomeScreen` rows + `QuickActions` `onEscaped`/`focusDefaultPosition`; intents in `shell.qml` |
| **QuickActions row** (top-right) | Left/Right move glyph; Down drops into rows; Up reaches it | Activate glyph (Notifications/Settings/Theme/Network/Volume/Power) | `focusDefaultPosition()` — `escapeRequestsSettings:false` on this row so B does **not** open Settings (#156) | (as Home screen) | (as Home screen) | `QuickActions.qml` |
| **Settings panel** | Up/Down move sidebar cursor (page does **not** auto-load); Right enters loaded page controls | Sidebar: `Return` loads focused page (focus stays on sidebar). In a page: activate control | **Hierarchical:** in a page → back to sidebar; on sidebar → close panel, return Home (`page → B → sidebar → B → Home`). The **Widgets** page owns its own internal stack (list → per-widget config; the Moonlight config page hosts server management inline, no deeper level): it consumes B to pop one level, only bubbling to the panel at the list level (`config → B → list → B → sidebar`). | (no special — intents guarded to idle; panel is part of idle) | n/a | `SettingsApp.Keys.onEscapePressed` / `onLeftPressed`; `WidgetsSettings.Keys.onEscapePressed` for the internal stack |
| **Nav drawer** (idle) | Up/Down move nav list; Down past end → quick-actions row; Up returns to list | `Return` activates nav item (Home/Settings) or quick-action glyph | Close drawer (`Drawer.Keys.onEscapePressed → closed()`); literal `B` key also closes | n/a (drawer already open) | `toggleMenu()` closes it | `NavigationDrawer`/`Drawer` + `QuickActions` |
| **Notification center** | Up/Down select entry | `Return` on an error entry → open error log | Close (`opened=false`) | — | — | `NotificationCenter.Keys.onPressed` (modal, consumes all) |
| **Power overlay** | Left/Right select action | `Return` activate selected power action | `cancelled()` → close | — | — | `PowerOverlay.Keys.onPressed` (modal, consumes all) |
| **Volume QAM popover** | adjust / move | activate | close popover → `_returnFocusAfterOverlay()` | — | first `toggleMenu()` press dismisses it | `VolumeOverlay.Keys.onPressed` |
| **Network QAM popover** | move Wi-Fi list | connect / activate | close popover → `_returnFocusAfterOverlay()` | — | first `toggleMenu()` press dismisses it | `NetworkOverlay.Keys.onPressed` |
| **App card context menu** (`PopoverMenu`) | move items | `Return` → activate (Focus / Close) | `closed()` | — | — | `PopoverMenu.qml` |
| **`appRunning`** (overlay drawer closed) | — (app owns input) | — | — | `intent:home-tap` → **toggle overlay drawer** | no-op (deliberate; the chord does the work) | `shell.qml onIntentHomeTap` |
| **`appRunning`** (overlay drawer open) | move overlay nav drawer | activate nav item (returns to shell + opens target) | `Keys.onEscapePressed` → close overlay drawer | toggle overlay drawer closed | — | overlay drawer `Item` in `ShellLayout` |
| **`streaming` / `reconnecting` / `launching`** | — (stream owns screen) | — | — | `intent:home` (Super+Escape) leaves; gamepad neutrals routed by daemon | — | only the intent channel reaches the shell |

**Global gamepad combos** (daemon-detected, delivered as `combo:*` events, not
state-gated) fire in every state:

| Combo event | Effect |
|-------------|--------|
| `combo:force-quit` | `forceQuit()` — nuke stream/app, dismiss all, Home |
| `combo:end-session` | `endSession()` — exit the GUI session |
| `combo:suspend-stream` | suspend the stream (only acts in `streaming`/`reconnecting`) |

---

## 5. Back-button (B / Escape) precedence

The gamepad **B** button is synthesized to `KEY_ESC`, so every "back" rule is a
`Keys.onEscapePressed` (or `Qt.Key_Escape`) handler on the focused surface. There
is no central back handler — precedence is whoever currently holds focus. The
single ordered rule, per context:

1. **A modal/overlay is open** (notification center, power overlay, QAM popover,
   `PopoverMenu`, app-card context menu) — B **dismisses that overlay** and
   returns focus underneath (via `homeFocusTimer` / `_returnFocusAfterOverlay`).
   These surfaces consume the key (`event.accepted = true`), so it never bubbles.
2. **Nav drawer open** — B closes the drawer (`Drawer.onEscapePressed`).
   `QuickActions` inside the drawer sets `escapeRequestsSettings:false` and does
   **not** consume B, so it bubbles up to the `Drawer` close handler (#142).
3. **Settings panel open** — **hierarchical**: from inside a page, B returns
   focus to the sidebar; from the sidebar, B closes the panel and returns Home.
   (`page → B → sidebar → B → Home`.)
4. **`appRunning`, overlay drawer open** — B closes the overlay drawer
   (`Keys.onEscapePressed → overlayDrawerClosed()`).
5. **Home screen, nothing open** — B resets to the default landing position
   (top content row, first card) via `focusDefaultPosition()`; a quiet no-op if
   already there. **B never opens Settings** — the QuickActions status-icon row
   sets `escapeRequestsSettings:false` (#156). Use QuickActions idx 1 → Return,
   or `intent settings`, to reach Settings.

> **Design note (the recurring bug class).** Because precedence is encoded
> implicitly across `toggleMenu()`, per-component `onEscaped`, and
> `Keys.onEscapePressed`, the classic regressions are: an overlay's B not
> consuming the key (focus stolen by the home screen behind it), B-on-home
> opening Settings, and a deep-linked view not owning focus. Verify any
> input/nav change against this ordered list. Centralizing back precedence into
> one handler is a tracked follow-up.

---

## 6. The `intent` control surface

The `intent` vocabulary is the cross-layer control surface (channel B). It is
fully specified in [`IPC_PROTOCOL.md` § `intent <name>`](IPC_PROTOCOL.md#intent-name);
this section maps each intent to the shell-side state change in
`shell.qml`'s `InputManager` handlers and notes the state guard.

### Coarse intents

| Intent | `shell.qml` handler | Effect | State guard |
|--------|--------------------|--------|-------------|
| `home` | `onIntentHome` | `returnToShell()` — global escape, always leaves the running app | **none** (fires in any state) |
| `home-tap` | `onIntentHomeTap` | `appRunning` → toggle overlay drawer; `idle` → `toggleMenu()` (+ AV wake if enabled) | acts only in `appRunning` or `idle` |
| `home-hold` | `onIntentHomeHold` | `resetToHome()` — over app → return to shell; on home → dismiss everything + `focusHome()` | acts only in `appRunning` or `idle` |
| `menu` | `onIntentMenu` | `toggleMenu()` (nav drawer) | `state === "idle"` (deliberate no-op over a running app) |
| `settings` | `onIntentSettings` | Open settings panel + focus it | `state === "idle"` |
| `power` | `onIntentPower` | Open power overlay + focus it | `state === "idle"` |

### Deep-link intents (`<ns>:<leaf>`)

All deep-links are **guarded by `state === "idle"`**, matching the coarse
intents. Unknown leaves are a graceful no-op in QML (logged, no crash).

| Intent | Handler | Effect |
|--------|---------|--------|
| `settings:<page>` | `onIntentSettingsPage` | `openSettings(page)` → `SettingsApp.openPage` → `openSectionById` — page ids below |
| `overlay:volume` | `onIntentOverlay` | `volumeOverlay.openAt(null)` |
| `overlay:network` | `onIntentOverlay` | `networkOverlay.openAt(null)` |
| `overlay:session` | `onIntentOverlay` | Open the power/session drawer |
| `app:<wmClass>` | `onIntentApp` | Match `_applications[].wmClass`, then `checkAndLaunchApp` |

**Settings page ids** (order from `SettingsApp.sections`): `audio`, `bluetooth`,
`network`, `display`, `wallpaper`, `controllers`, `keybindings`, `avcontrol`,
`webapps`, `accessibility`, `power`, `system`.

Three further leaves ride the `settings:` namespace but are **not** settings
pages: `ShellLayout.openSettings` intercepts `widgets`, `moonlight` and
`streaming` before delegating, pushing the top-level Widgets surface instead
(`moonlight`/`streaming` scoped to the Moonlight widget's config). They never
reach `SettingsApp.openPage`.

`home-tap` / `home-hold` are the gamepad Home **neutrals** — the daemon emits
them with no focus knowledge; QML (which owns focus) decides what each means.
`home` is the keyboard/automation global escape and is the only intent **not**
state-guarded.

### Meta / Guide gesture map (daemon tap/hold)

The gamepad **Meta/Guide** button (`BTN_MODE`) is split by the daemon into a
**tap** and a **hold** at the `[input].meta_hold_ms` threshold (default 500 ms).
The button is **buffered while discriminating** — nothing is forwarded to a
focused app until the daemon knows which it is, so a partial press never leaks.
The semantic rule: **tap belongs to the app, hold belongs to us** (the reserved,
non-destructive shell escape). Which `intent` (if any) fires depends on the
daemon's routed presenter:

| Presenter (focused surface) | Meta **TAP** (< threshold) | Meta **HOLD** (≥ threshold) |
|-----------------------------|----------------------------|-----------------------------|
| **Shell** (home screen) | `intent:home-tap` → `toggleMenu()` (nav drawer) | `intent:home-hold` → `resetToHome()` idle-branch (dismiss + clean home) |
| **Keyboard** (a keyboard-contract app, e.g. Plex) | *nothing* — a keyboard app has no Guide concept; the escape is the HOLD | **`intent:home-tap`** → toggle the **controllable overlay drawer** over the app (engages overlay-focus; non-destructive, app keeps running) |
| **Game** (virtual-pad app / stream) | Guide press+release **replayed to the virtual pad** (the game / remote Steam sees a real Guide tap) | **`intent:home-tap`** → same controllable overlay-drawer escape |
| **Handoff** (unpinned; pad ungrabbed) | app reads the raw Guide directly | **`intent:home-tap`** best-effort (the daemon can't fully swallow an ungrabbed node — the app may see the press up to the threshold) |

So the **everyday escape from any app is Meta-HOLD** → the controllable overlay
drawer (`intent:home-tap` in an app presenter), which is deliberately
**non-destructive**: the app keeps running foreground and the drawer engages
overlay-focus so it is controllable regardless of who holds compositor toplevel
focus. The heavier full return-to-home (`resetToHome()`/`returnToShell()`) is
reached from that drawer's menu or via `Super+Backspace` (`intent:home-hold`) —
it is **not** bound to the routine Meta hold. `intent:home-hold` from the Meta
button now fires only on the Shell home. **This escape requires the shell to be in
`appRunning`** — `AppLifecycleManager` adopts a focused external app into
`appRunning` (via the `hypr:activewindow` stream, including a shell restart under
an already-running app) precisely so the hold-escape arms.

### Combo safety (buffered participants)

While a focused app owns the screen (Keyboard/Game presenter) the daemon buffers
safety-combo **participant** buttons (`{Back, Home, LB, RB, Start, B}`) instead of
streaming them to the app, so a **partial** combo chord never leaks in (the Plex
accidental-playback class). The buffer is **swallowed** if a combo completes or
**replayed** to the app in order if disqualified (a non-participant press, a
participant release with no match, or `[input].combo_guard_ms` — default 120 ms —
elapsing). Arming is per-presenter: **Keyboard** buffers from the first
participant (a media app must never see stray media keys); **Game** arms only once
a second participant is co-held (single-button gameplay stays latency-free). The
`combo:*` events themselves always fire off the physically-held buttons — only the
app-forwarding of participants is gated. Full wire detail: [IPC_PROTOCOL.md](IPC_PROTOCOL.md#combo-safety-buffered-participants).

---

## 7. Gotchas

- **No Quickshell IPC.** The shell exposes no `IpcHandler` / `qs ipc` surface.
  Drive it externally **only** via the daemon's Unix socket (`intent`/`key`) or
  the LAN HTTP bridge (`POST /intent/*`, `POST /key/*`). See
  [`IPC_PROTOCOL.md`](IPC_PROTOCOL.md).
- **Two channels, never mixed.** Directional nav/select/back are *real key
  events* (`key <name>` / `wtype -k`), never intents — a focus move has no
  state-dependent decision for the shell to make. The drawer/settings/power are
  *intents*, not keys. There is **no `Tab` drawer key over the socket** — `Tab`
  only works as a direct Wayland key from the K400 (`ShellLayout.onTabPressed`).
- **B is Escape.** The gamepad B button arrives as `KEY_ESC`; back-button logic
  is all in `Keys.onEscapePressed` / `Qt.Key_Escape` handlers (plus a literal
  `Qt.Key_B` fallback for a physical keyboard 'B' in `Drawer`/`SettingsApp`).
- **The shell is unfocused over an app/stream.** In `appRunning` (drawer closed),
  `streaming`, `reconnecting`, and `launching`, the shell `PanelWindow` is hidden
  and unfocused — its `Keys` handlers cannot fire. The **intent channel is the
  only way in**; that is why `intent:home` is not state-guarded.
- **Deferred focus.** Never assume a `forceActiveFocus()` sticks — a sibling
  `focus:` binding can steal it back synchronously. Use `homeFocusTimer`
  (return-to-Home) or `Qt.callLater` (default-position) as the existing code does.
- **`menu` is a deliberate no-op over a running app.** A bare `Super` press also
  precedes a `Super+<key>` chord, so `intent:menu` is gated to `idle` to avoid an
  overlay flash before the chord's `home`/`home-hold` does the real work.
- **`idle` force-closes the overlay drawer** with `restoreEntryValues:false` so
  launching the next app doesn't restore a stale `overlayDrawerOpen=true` over
  the fresh app.
