# IPC Protocol Specification

The input/backend daemon (`tv-shell-input`, Rust source in `daemon/`) communicates with clients over a Unix domain socket using a newline-delimited text protocol. The primary client is the QML shell; the `tv-shell-panel` web control panel (`panel/`, see [PANEL.md](PANEL.md)) is a second client, speaking the same protocol over the same socket.

## Socket Connection

| Property | Value |
|----------|-------|
| Path | `/run/user/$UID/tv-shell-input.sock` |
| Env override | `TV_SHELL_SOCK` |
| Type | `AF_UNIX`, `SOCK_STREAM` |
| Framing | Newline-delimited (`\n`) UTF-8 text |
| Permissions | `0o600` (owner only) |

The daemon removes any existing socket file on startup and creates a new one. Clients connect, send one command per line, and read the response. The `subscribe` command is the exception — it holds the connection open and streams events.

Commands and responses are **bare newline-delimited text**. A few commands carry a compact single-line JSON *body* (as a request argument and/or response): `get-bindings`, `get-pads`, `list-input-devices`, `list-apps`, `get-config`, `set-config`, `record-launch`, `get-recents`, `webapp-list`, `webapp-add`, `get-notifications`, `record-notification`, `set-notifications`, the shell's `shell-state` push, the Phase 3 query replies `bt-list`, `net-status`, `net-wifi-list`, `net-throughput`, `net-ping`, and `power-battery`, the Phase 4 query replies `hypr-active`, `hypr-clients`, `hypr-monitors`, and `sunshine-status`, and the CEC query replies `cec-scan`, `cec-health`, and `cec-test`. JSON only ever appears as such a body — never as the framing itself.

## Client-to-Daemon Commands

### `grab`

Acquire exclusive gamepad access via `EVIOCGRAB`. Clears held-key state, resets triggers, cancels any pending combo timer, and sets input mode to `controller`.

**Response:** `ok\n`

### `release`

Release exclusive gamepad access. Resets all stick state (releases held stick keys, cancels repeat tasks), clears held-key state, resets triggers, and cancels any pending combo timer.

**Response:** `ok\n`

### `handoff`

Hand the physical pads to a Moonlight stream (#221). Switches the fleet to the **Handoff** presenter: drops any virtual twin and **releases** the physical `EVIOCGRAB` so SDL/Moonlight reads the real evdev node directly (a true handoff — no virtual pad). The daemon keeps reading events so the session stays active and the gamepad safety combos (force-quit / suspend / end-session) still arm, but it does **not** intercept Home — the Guide button flows straight through to the game (remote Steam sees it).

This is the presenter the shell enters when a stream launches; `grab` is the inverse (re-grab + Shell presenter). Contrast with `release`, which keeps the grab and routes through a virtual twin.

> **Re-grab is the caller's responsibility, and is guaranteed on every exit path.** Between `handoff` and Moonlight actually opening the evdev node the pads are ungrabbed, so a stuck/failed launch would leave them readable by the compositor. The shell bounds this window: `StreamManager.qml` re-emits `grab` on **every** terminal path — clean exit, suspend, force-quit, 5-attempt crash give-up, and a 30 s `launchTimeout` that force-kills a hung launch and re-grabs. During the window itself the daemon is still in the Handoff presenter reading the ungrabbed nodes, so the force-quit safety combo remains armed; and the kiosk Hyprland binds no gamepad input, so leaked events are inert. The 30 s bound is deliberately generous (network stream handshake) rather than tightened to a few seconds, which would false-trip slow launches.

**Response:** `ok\n`

### `shell-focus on|off`

Declare whether the **shell owns (or shares) the screen**. This is the shell's authoritative statement of screen ownership, and the daemon cannot derive it for itself.

**Why it must be told:** the shell is a **wlr-layer-shell surface**, not an xdg toplevel. Hyprland's `activewindow` only ever tracks toplevels, so the shell never appears in it (`hyprctl layers` is where it shows up). Taking exclusive keyboard focus does not clear the active *toplevel* either. The consequence is that while the shell is on screen, `activewindow` keeps naming whatever app was focused last — typically a backgrounded, still-fullscreen Steam. Anything that asks the compositor "what is the user looking at?" gets a stale answer.

Two subsystems used to ask exactly that, and both misbehaved:

- **Follow-focus** resolved the stale class and flipped the presenter to `Game`, handing the pad to a virtual gamepad while the user was on the home screen. Symptom: the controller appears connected but does nothing in the shell — or, in the mirror case (daemon in `Shell` while an app holds focus), pad motion is converted to nav **keys** and injected into that app, so it navigates by itself with a held direction reading as "stuck scrolling".
- **The kiosk fullscreen backstop** resolved the same stale window and issued `dispatch focuswindow` on it, actively yanking the screen to a backgrounded app. Symptom: launch Steam, and a prewarmed Plex comes to the front instead.

While `on`: `route_presenter()` dispatches pad events to the **shell** handler regardless of the base presenter, `should_grab()` forces the grab even over `Handoff`, follow-focus is **suppressed entirely** (`focus_presenter_target()` returns `None`), and the Hyprland actor's fullscreen backstop stands down. Toggling `off` restores the base presenter's routing and grab **exactly** and re-enables both. `sh.presenter` is never touched — it remembers the base routing to restore.

**Re-assert it freely.** The handler is idempotent, and `shell/shell.qml` re-sends the current value on a ~3s heartbeat (plus on every change). Every edge-triggered signal here has a way to be missed — an app that closes to background without a close event (Steam), a launch that times out, a daemon restart mid-session — and a missed edge otherwise wedges the pad against the wrong surface indefinitely. The heartbeat bounds any desync to one tick. The daemon defaults to `true` at startup (the shell boots owning the screen), so a restart lands in the safe state until the next heartbeat corrects it.

Derived in `shell/shell.qml` from `shellOwnsScreen` — the same predicate that drives the `PanelWindow`'s `visible` **and** `WlrLayershell.keyboardFocus`, so what is drawn, what holds the keyboard, and what the daemon believes cannot diverge.

**Response:** `ok\n` (`error:usage: shell-focus on|off` on a bad body)

### `shell-state <json>`

Push the shell's **own** state so the daemon can report it to remote consumers. Body is a compact single-line JSON object:

```
shell-state {"state":"streaming","media_playing":true}
```

- `state` — the shell's state-machine value, one of `idle` / `launching` / `streaming` / `reconnecting` / `appRunning`. The daemon stores and republishes it **verbatim**; it never maps, normalises, or interprets it.
- `media_playing` — whether any MPRIS player is playing, sourced from the same predicate `IdleInhibitController` uses, so the two cannot disagree about what "media playing" means.

**Why the daemon must be told.** This is QML-internal state. `bridge_core::UiState` answers a *compositor* question (which toplevel holds focus) and is explicitly not this — the shell is a layer surface, so no compositor query can recover what the shell itself is doing. Same reasoning as [`shell-focus`](#shell-focus-onoff), which is why this rides the same channel and the same heartbeat.

**The daemon computes no policy from it.** It caches the value with a receipt timestamp and republishes it, plus a staleness verdict, on the bridge's [`GET /status`](CONTROL_SURFACE.md). Deciding whether a given state means "busy" belongs to the consumer (e.g. a home-automation rule), so that rule can change without a daemon release.

**Re-assert it freely.** `shell/shell.qml` pushes on every change *and* re-sends the current value on the same ~3s heartbeat as `shell-focus`. The heartbeat is what makes staleness meaningful: without a liveness signal the daemon cannot tell "the shell is idle" from "the shell is dead", and reporting a stale `idle` as idle is exactly what would let a consumer suspend a box that is really mid-stream behind a wedged shell. A push older than `3 ×` the heartbeat (9 s) is flagged `stale`.

Stateless and cross-platform — it never reaches the Linux-only input runtime.

**Response:** `ok\n`. A missing body replies `error:usage: shell-state <json-object with state+media_playing>`; malformed JSON replies `error:invalid JSON: <detail>`. Neither panics.

### `overlay-focus on|off`

Route pad input to the **shell** while a modal shell overlay (the overlay nav drawer or the Session QAM) is open **over a running app**. Without this, the base presenter keeps driving the app — Game forwards to the virtual pad, Handoff leaves the node ungrabbed for SDL/Steam — so pressing **A** on the drawer leaks into the app instead of activating the drawer item.

While `on`, `route_presenter()` folds the flag in so pad events dispatch to the **shell** handler (buttons→keyboard: A→Enter, dpad→arrows) regardless of the base presenter, and `should_grab()` forces a grab even over Handoff (so Steam/SDL stop reading the raw node). Toggling `off` restores the base presenter's routing and grab **exactly**. The gamepad safety combos (force-quit / suspend / end-session) are unaffected in either state.

The shell drives this off the "shell surface mapped over the app" condition in `shell/shell.qml` (the same boolean that maps the `PanelWindow` in `appRunning`), so it turns `on` when the overlay opens and `off` on every close path (B, item select, app exit, force-quit, `intent:home`, QAM close) — it can't get stuck on. Pairs with the surface's `WlrLayershell.keyboardFocus` flipping to `Exclusive` so the daemon-emitted keys actually land on the drawer, not the app.

**Response:** `ok\n` (`error:usage: overlay-focus on|off` on a missing/invalid argument).

### `status`

Query current connection and grab state.

**Response:** `<connection>:<grab>\n`

| Field | Values |
|-------|--------|
| connection | `connected` or `disconnected` |
| grab | `grabbed` or `released` |

Example: `connected:grabbed\n`

> **Fleet aggregate (Phase 4).** With multi-pad support the daemon tracks a
> *fleet* of pads. `status` is the fleet aggregate: `connected` if **any** pad is
> present, `grabbed` if **any** pad is grabbed. For a single connected pad this
> is byte-identical to the pre-fleet reply (`connected:grabbed` /
> `disconnected:released`). Use `get-pads` for per-pad detail.

### `get-pads`

Return the connected gamepad fleet as a compact JSON array, one object per pad in
ascending player-slot order. Each pad has a stable player `index` (#101) that
survives another pad reconnecting (P1 stays slot 0 across a P2 unplug/replug).

**Response:** Single-line JSON array of `{id,index,name,grabbed}` objects.

| Field | Meaning |
|-------|---------|
| `id` | Stable wire id (from evdev `uniq`/`phys`, else `vp:vendor:product:path`) — follows a physical pad across reconnects |
| `index` | Player slot (0 = P1, 1 = P2, …); lowest free slot reused on reconnect |
| `name` | Device display name |
| `grabbed` | Whether the daemon currently holds the exclusive grab |

Example: `[{"id":"uniq:e4:17:...","index":0,"name":"Xbox Wireless Controller","grabbed":true}]\n`

Empty fleet → `[]\n`.

### `list-input-devices`

Enumerate **every controller-like input device** on the host — anything that
advertises `BTN_SOUTH` **or** carries a `js*` handler — as a compact JSON array.
A **diagnostics enumerator** (it replaces `ControllerSettings`' old
`/proc/bus/input/devices` python reader), distinct from `get-pads`: it lists
ungrabbed and virtual devices too, not just the grabbed fleet. The runtime marks
`grabbed=true` only for devices whose devnode path the fleet currently owns.

**Response:** Single-line JSON array of device objects, one per device, in
ascending devnode-path order:

```json
[{"name":"Xbox 360 Controller","path":"/dev/input/event18","vendor":"045e","product":"028e","phys":"usb-0000:00:14.0-1/input0","handlers":["event18","js0"],"grabbed":true}]
```

| Field | Type | Notes |
|-------|------|-------|
| `name` | string | Device display name (evdev name) |
| `path` | string | The `/dev/input/eventN` devnode |
| `vendor` | string | 4-hex-digit lowercase vendor id (e.g. `"045e"`) |
| `product` | string | 4-hex-digit lowercase product id (e.g. `"028e"`) |
| `phys` | string | evdev physical path (`""` if none) |
| `handlers` | array | Handler names from `/proc/bus/input/devices` (e.g. `["event18","js0"]`); falls back to just the event-node name when `/proc` is unavailable |
| `grabbed` | bool | True only for devices the fleet currently owns |

An empty result is `[]`.

### `subscribe`

Register as an event subscriber. The daemon sends `subscribed\n`, then streams events (one per line) for the lifetime of the connection. The connection stays open — the server reads until EOF, then removes the subscriber.

**Response:** `subscribed\n` followed by a stream of events (see [Daemon-to-Subscriber Events](#daemon-to-subscriber-events)).

### `get-bindings`

Return current button-to-action mappings as compact JSON.

**Response:** Single-line JSON object mapping action names to evdev button code names.

Example: `{"select":"BTN_SOUTH","back":"BTN_EAST","altSelect":"BTN_NORTH","menu":"BTN_START"}\n`

### `set-binding <action> <button_name>`

Remap a button for the given action. Rebuilds the internal button map and persists to settings.

**Response:**

| Condition | Response |
|-----------|----------|
| Success | `ok\n` |
| Wrong number of args | `error:usage: set-binding <action> <button_name>\n` |
| Unknown action | `error:unknown action '<action>'\n` |
| Invalid or non-remappable button | `error:invalid button '<button_name>'\n` |

Valid actions: `select`, `back`, `altSelect`, `menu`, `altAction`

### `set-active-game <id>`

Signal the currently foregrounded app/game to the daemon. The daemon uses this
to activate the matching per-game binding override layer from
`settings.json`'s `perGameBindings`. Sending bare `set-active-game` (no body)
clears the active game, reverting to the player/global binding layers only.
In-memory only — resets on daemon restart.

**Response:**

| Condition | Response |
|-----------|----------|
| Game set | `ok\n` |
| Game cleared (bare command) | `ok\n` |

**Notes:**
- Touches no device; pure in-memory state change.
- The per-game override layer is read from `perGameBindings` in `settings.json`
  (see [Settings Persistence](#settings-persistence)). An unrecognized game id
  silently uses only the player/global layers.
- Resolution order: game override → player override → global → default.
- The shell sends this command when an app or game is foregrounded (e.g. after
  `record-launch`), and clears it when returning to the shell.

### `capture-next`

Wait for the next remappable button press on the gamepad (10-second timeout). If a capture is already pending, the previous one is cancelled. Non-remappable button presses during capture are silently ignored.

**Response:**

| Condition | Response |
|-----------|----------|
| Button pressed | `captured:<button_name>\n` (e.g., `captured:BTN_SOUTH\n`) |
| Timeout (10s) | `timeout\n` |
| Cancelled | `cancelled\n` |

### `capture-cancel`

Cancel a pending `capture-next` without waiting for timeout.

**Response:** `ok\n`

### `list-apps`

Scan installed XDG `.desktop` entries and return the launchable applications.
Stateless — served directly by the daemon's IPC layer (no input-runtime
round-trip) via the cross-platform `freedesktop-desktop-entry` parser.

Scans `/usr/share/applications` then `~/.local/share/applications` (in that
order). Skips entries with `NoDisplay=true`, `Hidden=true`, `Type != Application`,
or an empty `Name`. De-duplicates by `Name` (first occurrence wins, in
directory-then-filename order) and sorts the result by `name` case-insensitively.
The `Exec` field has the freedesktop field codes `%u %U %f %F %i %c %k` stripped
and is trimmed.

**Response:** A compact single-line JSON **array** of app objects:

```json
[{"name":"Firefox","exec":"firefox","icon":"firefox","comment":"Browse the web","wmClass":"firefox"}]
```

Each object has `name`, `exec`, `icon`, `comment`, `wmClass` (all strings;
missing optional fields are `""`). An empty result is `[]`.

### `webapp-list`

Return the web-app registry (#187). Stateless. Web apps are Chromium `--app`
launchers the daemon generates as XDG desktop entries — see
[WEB_APPS.md](WEB_APPS.md).

**Response:** A compact single-line JSON **array**:

```json
[{"id":"youtube","name":"YouTube","url":"https://youtube.com/tv","wmClass":"tvshell-youtube"}]
```

An empty or unreadable registry is `[]`.

### `webapp-add <json-object>`

Register a web app and generate its launcher. Body is a compact JSON object with
`name` and `url`; the daemon allocates `id` (a slug of the name, de-duplicated)
and `wmClass` (`tvshell-<id>`) — clients never choose them.

The daemon writes `~/.local/share/applications/tv-shell-webapp-<id>.desktop`
with a Chromium `--app=<url>` `Exec`, a matching `--class`/`StartupWMClass`, a
per-app `--user-data-dir`, `--ozone-platform=wayland`, and the ownership marker
`X-TvShell-WebApp=true`. The entry therefore appears in `list-apps`, on the home
Applications row, and via `intent app:<wmClass>` with no extra plumbing. It then
persists the registry into the `webApps` key of settings.json (the daemon
remains the sole settings writer).

**Validation** (all rejected with `error:webapp-add failed: …`): a URL that
isn't `http`/`https`, an empty name, control characters/newlines in either
field, and URLs containing whitespace, quotes, backslashes, backticks, `$` or
`;`. A `.desktop` file that exists but lacks our ownership marker is never
overwritten. If no Chromium-family browser is found on `PATH`, the add fails
rather than writing a launcher that can't run.

**Response:** the created entry as a compact JSON object, e.g.
`{"id":"youtube","name":"YouTube","url":"https://youtube.com/tv","wmClass":"tvshell-youtube"}`

**Usage error:** `error:usage: webapp-add <json-object with name+url>`

### `webapp-remove <id>`

Remove a web app from the registry and delete its generated `.desktop`. A file
lacking our ownership marker is refused rather than deleted; an already-missing
file is not an error. The app's Chromium profile directory is deliberately
**kept**, so re-adding the same app restores its logins.

**Response:** `ok\n`, or `error:webapp-remove failed: no web app with id "…"`.

**Usage error:** `error:usage: webapp-remove <id>`

### `get-config`

Return the full settings document (`~/.config/tv-shell/settings.json`).
Stateless. A missing or unparseable file yields `{}`.

**Response:** The settings document as a compact single-line JSON **object**:

```json
{"themeMode":"dark","streamingViewMode":"servers","keyBindings":{"select":"BTN_SOUTH"}}
```

### `set-config <json-object>`

Merge a compact single-line JSON object of settings updates into
`settings.json` (read-modify-write). The daemon is the sole writer of
`settings.json`; foreign keys not present in the body are preserved untouched
(notably the daemon-owned `keyBindings`). A key whose value is JSON `null` is
**removed** from the document (used to drop the legacy `moonlightViewMode` key).
Stateless. Written single-line compact JSON.

**Response:**

| Condition | Response |
|-----------|----------|
| Success | The new document as compact single-line JSON (same shape as `get-config`) |
| Missing body | `error:usage: set-config <json-object>\n` |
| Body isn't valid JSON | `error:invalid JSON: <detail>\n` |
| Body is valid JSON but not an object | `error:set-config body must be a JSON object\n` |
| Write failed | `error:set-config failed: <detail>\n` |

Example request: `set-config {"themeMode":"dark","controllerDebug":false,"moonlightViewMode":null}\n`

After a successful merge, the daemon refreshes its cached input-runtime settings
(currently `rumbleEnabled`) so a rumble toggle takes effect immediately without a
daemon restart.

**QML-owned settings keys** (written by QML via `set-config`, read by the daemon
at relevant lifecycle points):

| Key | Type | Default | Read by daemon |
|-----|------|---------|----------------|
| `themeMode` | string | `"dark"` | No |
| `streamingViewMode` | string | `"servers"` | No |
| `controllerDebug` | bool | `false` | No |
| `widgets` | object | `{}` | No |
| `rumbleEnabled` | bool | `true` | On every rumble event |
| `reduceMotion` | bool | `false` | No |
| `textScale` | number | `1.0` | No |
| `hdrEnabled` | bool | `true` | No |
| `nightLightEnabled` | bool | `false` | No |
| `nightLightTemp` | number | `4500` | No |
| `overscan` | number | `0` | No |
| `sleepTimerMinutes` | number | `0` | No |
| `wakeOnController` | bool | `true` | No |
| `autoDimEnabled` | bool | `false` | No |
| `autoDimDelayMinutes` | number | `2` | No |
| `defaultSink` | string | `""` | No |
| `cecFocusOnStartup` | bool | `false` | At CEC startup (within `[cec] lifecycle`) |
| `cecFocusOnWake` | bool | `true` | At CEC resume from sleep (within `[cec] lifecycle`) |
| `prewarmApps` | array | `[]` | No |

The `prewarmApps` key is an array of app keys — apps to silently prewarm (launch
unfocused in the background) at shell login (#238). An entry is normally a
`StartupWMClass`; for desktop entries that declare none (e.g. Steam) the key is
the `Exec` basename instead. It is opaque to the daemon; the QML shell owns both
the resolution and the launch.

The `widgets` key is the **namespaced per-widget config** the home-widget
framework owns (still opaque to the daemon). Shape:

```json
{"widgets":{"moonlight":{"enabled":true,"order":0,"size":"large","prefs":{}},
            "nowplaying":{"enabled":true,"order":1,"size":"medium","prefs":{"hideFromRecent":true}},
            "plex":{"enabled":true,"order":2,"size":"medium","prefs":{"hideFromRecent":true}},
            "recent":{"enabled":true,"order":3,"size":"medium","prefs":{}}}}
```

`enabled` and `order` are framework keys for every widget; `size` and the
`prefs.*` entries are declared by each widget's manifest
(`shell/widgets/lib/WidgetManifests.qml`). On load, `SettingsStore`
one-shot-migrates the legacy flat `widget<Name>Enabled/Size/HideFromRecent`
keys into this subtree (`widgetSpotify*` → `nowplaying`), preserving existing
values; the daemon's shallow merge leaves the old flat keys on disk untouched
for one release. `set-config` always sends the complete `widgets` object, so the
merge replaces that subtree wholesale.

### `record-launch <json-object>`

Record an app launch into the recents file
(`~/.local/share/tv-shell/recents.json`). The body is a compact single-line
JSON object `{"name":...,"exec":...,"comment":...}` (all optional, default
`""`). The daemon prepends a `{name,exec,comment,time}` entry (where `time` is
unix seconds set by the daemon), removing any existing entry with the same
`name` (most-recent-wins), and caps the file at 20 entries. Stateless. Written
single-line compact JSON.

**Response:**

| Condition | Response |
|-----------|----------|
| Success | `ok\n` |
| Missing body | `error:usage: record-launch <json-object>\n` |
| Body isn't valid JSON | `error:invalid JSON: <detail>\n` |
| Write failed | `error:record-launch failed: <detail>\n` |

Example request: `record-launch {"name":"Firefox","exec":"firefox","comment":"Browse the web"}\n`

### `get-recents`

Return the recently launched apps, newest first. Stateless. A missing or
unparseable file yields `[]`. Returns at most 15 entries.

**Response:** A compact single-line JSON **array** of recents objects:

```json
[{"name":"Firefox","exec":"firefox","comment":"Browse the web","time":1716950400.0}]
```

---

## Notification History Commands (#71)

### `get-notifications`

Return the stored notification history, newest first. Stateless. A missing or
unparseable file yields `[]`. Returns at most 100 entries.

**Response:** A compact single-line JSON **array** of notification objects:

```json
[{"id":5,"title":"Stream started","message":"","level":"info","source":"stream","icon":"📡","time":1716950400.0}]
```

| Field | Type | Notes |
|-------|------|-------|
| `id` | number | Notification id (monotonically increasing within a session) |
| `title` | string | Notification title |
| `message` | string | Body text (may be `""`) |
| `level` | string | `info` / `warning` / `error` |
| `source` | string | Source tag (e.g. `system`, `stream`, `controller`, `network`, `av`) |
| `icon` | string | Icon character or name (may be `""`) |
| `time` | number | Unix seconds (float) when the notification was created |

### `record-notification <json-object>`

Append a notification to the history file
(`~/.local/share/tv-shell/notifications.json`). The body is a compact
single-line JSON object `{"id":N,"title":...,"message":...,"level":...,"source":...,"icon":...}`.
The daemon prepends the entry (newest first), stamps `time` with the current
wall-clock unix seconds, and caps the file at 100 entries. **No de-duplication**
— every notification is a distinct log event (unlike `record-launch` which
deduplicates by name). Stateless. Written single-line compact JSON.

**Response:**

| Condition | Response |
|-----------|----------|
| Success | `ok\n` |
| Missing body | `error:usage: record-notification <json-object>\n` |
| Body isn't valid JSON | `error:invalid JSON: <detail>\n` |
| Write failed | `error:record-notification failed: <detail>\n` |

Example request: `record-notification {"id":5,"title":"Stream started","message":"","level":"info","source":"stream","icon":""}\n`

### `set-notifications <json-array>`

Overwrite the notifications file entirely with the given array. Used by the QML
`NotificationManager` when the user clears history or removes a single
notification — the caller sends the full updated in-memory list. Caps at 100
entries. Stateless. Written single-line compact JSON.

**Response:**

| Condition | Response |
|-----------|----------|
| Success | `ok\n` |
| Missing body | `error:usage: set-notifications <json-array>\n` |
| Body isn't valid JSON | `error:invalid JSON: <detail>\n` |
| Write failed | `error:set-notifications failed: <detail>\n` |

Example request: `set-notifications []\n` (clears history)

---

## Controller DB Commands (#159)

### `controllerdb-status`

Return the current state of the bundled SDL `GameControllerDB` (the known-controller
lookup table used for pad discovery). Stateless — answered directly by the IPC layer.

**Response:** Compact single-line JSON object.

| Field | Type | Meaning |
|-------|------|---------|
| `source` | string | Which source was used: `"bundled_baseline"` (only shipped DB), `"upstream_cache"` (cache overrides baseline), or `"env_override"` (env var overrides all — last source wins, not a union) |
| `entryCount` | number | Total number of known (vendor, product) pairs after merging all sources |
| `lastDownloaded` | number | Unix timestamp (seconds) of the last successful upstream fetch, or `0` if never fetched |
| `upstreamUrl` | string | The URL used for upstream fetches |
| `error` | string? | Last fetch error, omitted when the most recent fetch succeeded |

Example:
```json
{"source":"upstream_cache","entryCount":3712,"lastDownloaded":1749200000,"upstreamUrl":"https://raw.githubusercontent.com/mdqinc/SDL_GameControllerDB/master/gamecontrollerdb.txt"}
```

The **Controllers** settings page (`ControllerSettings.qml`) renders this status
(entry count, last-download timestamp, source) and exposes a **Refresh DB**
button that issues `controllerdb-refresh` and updates the status line in place.

### `controllerdb-refresh`

Fetch the upstream SDL `GameControllerDB`, persist it to
`~/.local/share/tv-shell/gamecontrollerdb.txt`, and hot-swap the live db in
the input runtime (new controllers are identified without a daemon restart — the
IPC layer sends `Control::ControllerDbRefreshed` to the runtime after a
successful fetch). **Linux-only** (`reqwest` HTTPS); on non-Linux the fetch
always fails gracefully.

**Response:** Same JSON shape as `controllerdb-status`, reflecting the post-refresh state.

| Condition | Response |
|-----------|----------|
| Success | `controllerdb-status` JSON with updated `entryCount`/`lastDownloaded` |
| Fetch error | `controllerdb-status` JSON with `error` field set, `entryCount`/`source` unchanged |

---

## Per-Pad Battery and Rumble Status (#160)

### `pad-battery <id>`

Query the current battery state of the pad whose stable wire id is `<id>`. For
wired pads (no battery sysfs entry), `present` is `false`. An unknown id replies
`error:pad not found '<id>'` (not a `present:false` object).

**Response:** Compact single-line JSON object.

| Field | Type | Condition | Meaning |
|-------|------|-----------|---------|
| `id` | string | always | The requested wire id |
| `present` | bool | always | `true` if a battery reading is available |
| `level` | number | `present=true` | Charge percentage 0–100 |
| `charging` | bool | `present=true` | `true` when charging |

Examples:
```
pad-battery uniq:e4:17:d8:ab:cd:ef
{"id":"uniq:e4:17:d8:ab:cd:ef","present":true,"level":82,"charging":false}

pad-battery uniq:unknown
error:pad not found 'uniq:unknown'
```

| Condition | Response |
|-----------|----------|
| Pad found, has battery | JSON with `present:true`, `level`, `charging` |
| Pad found, wired/no battery | JSON with `present:false` |
| Pad not found | `error:pad not found '<id>'` |
| Missing/empty `<id>` | `error:usage: pad-battery <id>` |

### `pad-rumble-status <id>`

Query whether the pad whose stable wire id is `<id>` supports rumble and whether
it is currently enabled (the `rumbleEnabled` setting in `settings.json`).

**Response:** Compact single-line JSON object.

| Field | Type | Meaning |
|-------|------|---------|
| `id` | string | The requested wire id |
| `supported` | bool | `true` if the pad has an `EV_FF`/`FF_RUMBLE` effect uploaded |
| `enabled` | bool | `true` if the `rumbleEnabled` setting is on (affects all pads) |

Example:
```
pad-rumble-status uniq:e4:17:d8:ab:cd:ef
{"id":"uniq:e4:17:d8:ab:cd:ef","supported":true,"enabled":true}
```

| Condition | Response |
|-----------|----------|
| Pad found | JSON with `supported` and `enabled` |
| Pad not found | `error:pad not found '<id>'` |
| Missing/empty `<id>` | `error:usage: pad-rumble-status <id>` |

---

## System Status Commands (#164)

These commands back the System/About settings page. Both are **stateless** (read
from procfs/sysfs/`/etc` at call time) and cross-platform (on non-Linux hosts the
reads degrade gracefully to `"Unknown"`).

### `sys-status`

Return OS name, kernel version, hostname and uptime as a compact JSON object.

**Response:** Compact single-line JSON object.

| Field | Type | Source |
|-------|------|--------|
| `os` | string | `NAME=` from `/etc/os-release` |
| `kernel` | string | `/proc/sys/kernel/osrelease` (`uname -r`) |
| `hostname` | string | `/proc/sys/kernel/hostname` |
| `uptime` | string | `/proc/uptime`, formatted as `Xd Xh Xm Xs` |

Example:
```json
{"os":"Arch Linux","kernel":"6.9.3-arch1-1","hostname":"my-streaming-box","uptime":"2d 14h 32m 10s"}
```

### `storage-status`

Return a JSON array of real filesystem mounts with raw-byte sizes. Pseudo-
filesystems (`proc`, `sysfs`, `devtmpfs`, `cgroup`, etc.) and system paths
(`/proc`, `/sys`, `/run/user/*`) are excluded automatically. Duplicate devices
(bind mounts) are de-duplicated by device path.

**Response:** Compact single-line JSON array.

| Field | Type | Meaning |
|-------|------|---------|
| `mount` | string | Mount point path |
| `size` | number | Total bytes |
| `used` | number | Used bytes |
| `avail` | number | Available (free) bytes |
| `pct` | number | Usage percentage 0–100 |

Example:
```json
[{"mount":"/","size":500107862016,"used":125026959360,"avail":375080902656,"pct":25},{"mount":"/home","size":1000204886016,"used":400000000000,"avail":600204886016,"pct":40}]
```

Empty array `[]` if no real filesystems are found (e.g. non-Linux host).

### `build-info`

Return the currently-deployed build identity as a compact JSON object. Backs the
System settings page's version readout. Read **live** (async `git` at
`install_root()`) so a `/dev/deploy` HEAD swap under the running daemon is
reflected without a restart — the same source as the `/metrics` `build_info`
gauge and the `X-TvShell-*` response headers.

**Request:** `build-info` (bare, no body).

**Response:** Compact single-line JSON object.

| Field | Type | Source |
|-------|------|--------|
| `version` | string | `CARGO_PKG_VERSION` (compile-time) |
| `sha` | string | short git SHA at `install_root()` (`"unknown"` off a repo) |
| `branch` | string | branch name, or `"HEAD"` on a detached checkout (`"unknown"` off a repo) |

Example:
```json
{"version":"0.1.0","sha":"a1b2c3d","branch":"main"}
```

### `sys-metrics`

Return live hardware telemetry as a compact JSON object (#235). Backs the System
settings page's live readout and is the same source as the `tv_shell_sys_*`
Prometheus gauges. Stateless (reads procfs/sysfs at call time) and cross-platform
(non-Linux hosts degrade to zeros / an empty `temps` array).

**Request:** `sys-metrics` (bare, no body).

**Response:** Compact single-line JSON object.

| Field | Type | Meaning |
|-------|------|---------|
| `cpuPct` | number | Aggregate CPU utilization 0–100 |
| `memUsed` | number | Used memory, bytes |
| `memTotal` | number | Total memory, bytes |
| `memPct` | number | Memory usage percentage 0–100 |
| `load1` | number | 1-minute load average |
| `temps` | array | Temperature sensors: `[{label, celsius}, …]` (empty when none are readable) |

Example:
```json
{"cpuPct":12.5,"memUsed":8123456789,"memTotal":33554432000,"memPct":24,"load1":0.42,"temps":[{"label":"Package id 0","celsius":41.0}]}
```

---

## Phase 3 Commands (D-Bus backbone)

Phase 3 adds D-Bus integrations to the daemon for Bluetooth (`bluer`/BlueZ),
Wi-Fi **reads** (`zbus`/NetworkManager), and power/idle (`zbus`/logind + UPower).
These commands replace the QML shell-outs that *read* system state.

These integrations are **Linux-only**. On a non-Linux build (or any host where
the D-Bus backbone failed to start), every Phase 3 command except the MAC-usage
error replies:

```
error:unsupported on this platform
```

Commands and replies follow the same conventions as Phase 1/2: bare
newline-delimited text, with a few replies carrying a compact single-line JSON
body. The streamed Phase 3 events are documented under
[Phase 3 Events](#phase-3-events).

> **Scope (unchanged by Phase 3):** Wi-Fi **join** stays an `nmcli device wifi
> connect` shell-out (the D-Bus `AddAndActivateConnection` variant map is not
> implemented), audio stays `wpctl`, and one-shot compositor actions stay
> `hyprctl dispatch`. Only system-state *reads* (plus Bluetooth/power *actions*)
> moved onto D-Bus.

### Bluetooth (`bluer` / BlueZ)

#### `bt-power-status`

Query the default adapter's power state.

**Response:**

| Condition | Response |
|-----------|----------|
| Adapter powered on | `bt:on\n` |
| Adapter powered off | `bt:off\n` |
| No adapter / read error | `error:<detail>\n` |
| Non-Linux build | `error:unsupported on this platform\n` |

#### `bt-power-on` / `bt-power-off`

Power the default adapter on / off.

**Response:** `ok\n` on success, `error:<detail>\n` on failure.

#### `bt-scan-on` / `bt-scan-off`

Start / stop device discovery. While scanning, discovered/updated devices are
streamed to subscribers as `bt:device:<json>` events and removals as
`bt:device-removed:<mac>`; scan start/stop also emits `bt:scanning:on` /
`bt:scanning:off`.

**Response:** `ok\n` on success, `error:<detail>\n` on failure.

#### `bt-list`

List known Bluetooth devices.

**Response:** A compact single-line JSON **array** of device objects:

```json
[{"mac":"AA:BB:CC:DD:EE:FF","name":"Xbox Wireless Controller","paired":true,"connected":true,"trusted":true,"rssi":-52}]
```

| Field | Type | Notes |
|-------|------|-------|
| `mac` | string | Device address |
| `name` | string \| null | BlueZ remote name, falling back to a non-empty alias, else `null` |
| `paired` | bool | |
| `connected` | bool | |
| `trusted` | bool | |
| `rssi` | number \| null | Signal strength when available, else `null` |

An empty result is `[]`. On a non-Linux build: `error:unsupported on this platform\n`.

#### `bt-connect <mac>` / `bt-disconnect <mac>` / `bt-pair <mac>` / `bt-trust <mac>`

Act on a device by MAC address. `bt-pair` uses the BlueZ default agent
(just-works). The MAC is a single whitespace-trimmed token after the command
word.

**Response:**

| Condition | Response |
|-----------|----------|
| Success | `ok\n` |
| Failure (unknown device, BlueZ error) | `error:<detail>\n` |
| Missing MAC argument | `error:usage: bt-connect <mac>\n` (word matches the command issued: `bt-connect` / `bt-disconnect` / `bt-pair` / `bt-trust`) |
| Non-Linux build | `error:unsupported on this platform\n` |

The MAC-usage error is produced by the cross-platform parser, so it is returned
on every platform (not gated behind the D-Bus backbone).

### Network READ (`zbus` / NetworkManager)

#### `net-status`

Current connectivity and primary/active connection state.

**Response:** A compact single-line JSON **object**:

```json
{"connectivity":"full","primaryType":"802-3-ethernet","hasWifi":true,"ipv4":"eth0: 192.168.1.50","gateway":"192.168.1.1","dns":["192.168.1.1","8.8.8.8"],"activeConnections":[{"name":"Wired connection 1","type":"802-3-ethernet","device":"eth0","speed":1000}]}
```

| Field | Type | Notes |
|-------|------|-------|
| `connectivity` | string | `none` / `portal` / `limited` / `full` / `unknown` (NM connectivity code 1/2/3/4, else `unknown`) |
| `primaryType` | string | Connection type of NM's primary connection (`""` if none) |
| `hasWifi` | bool | True if any NM device is a Wi-Fi device (`DeviceType == 2`) |
| `ipv4` | string | Best-effort non-loopback IPv4 addresses as `"<iface>: <ip>"` lines (newline-joined, up to 3; `""` if none). Read via an `ip -4 -o addr` shell-out — explicitly allowed, since only `nmcli` *reads* must move to D-Bus |
| `gateway` | string | IPv4 gateway address from the primary connection's IP4Config (`""` when none/unknown); best-effort |
| `dns` | string array | DNS server addresses from the primary connection's IP4Config (`[]` when none/unknown); best-effort. Prefer `NameserverData` (NM ≥ 1.6); fall back to legacy packed-u32 `Nameservers` property |
| `activeConnections` | array | `{name, type, device, speed}` objects; `device` is the first interface name; `speed` is link speed in Mb/s from `Device.Wired` (0 for non-wired devices, virtual devices, or when the link speed is not yet known — render only when > 0) |

If NetworkManager is unreachable, a best-effort object is returned with
`connectivity:"unknown"`, empty strings, `hasWifi:false`, `gateway:""`,
`dns:[]`, and `activeConnections:[]` (the command does not error). On a
non-Linux build: `error:unsupported on this platform\n`.

#### `net-wifi-list`

List visible Wi-Fi access points (deduplicated by SSID, strongest signal wins,
sorted by signal descending).

**Response:** A compact single-line JSON **array** of AP objects:

```json
[{"ssid":"home-network","signal":82,"security":"WPA2","inUse":true}]
```

| Field | Type | Notes |
|-------|------|-------|
| `ssid` | string | Access-point SSID (UTF-8-lossy of the raw SSID bytes); hidden/empty SSIDs are skipped |
| `signal` | number | Signal strength percentage (0–100) |
| `security` | string | Coarse label derived from the AP flag triple: `Open` / `WEP` / `WPA` / `WPA2` / `WPA3` / `WPA-Enterprise` / `OWE` |
| `inUse` | bool | True for the AP the device is currently associated with |

An empty / unavailable result is `[]`. On a non-Linux build:
`error:unsupported on this platform\n`.

> **Read-only:** there is intentionally no `net-wifi-connect` command. Joining a
> network stays an `nmcli device wifi connect` shell-out in the QML.

#### `net-wifi-rescan`

Trigger a Wi-Fi rescan (NetworkManager `RequestScan`). Fresh results show up via
`net-wifi-list` and `net:wifi` events.

**Response:** `ok\n` on success, `error:<detail>\n` on failure. Non-Linux:
`error:unsupported on this platform\n`.

### Network reads (stateless: sysfs + `ping`)

These two reads do **not** go through the NetworkManager D-Bus actor — they are
stateless and served directly from the dispatcher (like `sunshine-status` /
`wol`). They exist so the QML shell stops shelling out for per-interface
throughput and connectivity checks.

#### `net-throughput <iface>`

Read an interface's **cumulative** rx/tx byte counters from
`/sys/class/net/<iface>/statistics/{rx,tx}_bytes`. These are raw counters, **not
a rate** — the caller samples on an interval and computes the delta itself.

The `<iface>` is the single token after the command word (the device name the
caller gets from `net-status`'s primary connection). It is validated (no path
separators / `..`) before it touches the sysfs path.

**Response:** A compact single-line JSON **object**:

```json
{"iface":"eth0","rxBytes":12345678,"txBytes":2345678}
```

| Field | Type | Notes |
|-------|------|-------|
| `iface` | string | Echo of the requested interface |
| `rxBytes` | number | Cumulative received bytes since boot |
| `txBytes` | number | Cumulative transmitted bytes since boot |

**Fail-soft:** an unknown/unreadable interface, an invalid name, or a non-Linux
build returns the same shape with zeroed counters and an `error` field rather
than a protocol error, so the UI can render a dash without special-casing:

```json
{"iface":"eth0","rxBytes":0,"txBytes":0,"error":"interface not found"}
```

A missing `<iface>` body is `error:usage: net-throughput <iface>\n`.

#### `net-ping <host> [count]`

Run a bounded `ping` and report reachability + average RTT. `count` defaults to
`1` and is clamped to `1..=10`; each packet uses a 2 s timeout. `<host>` is
passed as a single argv (no shell), so it cannot inject.

**Response:** A compact single-line JSON **object**:

```json
{"host":"1.1.1.1","reachable":true,"rttMs":14.5}
```

| Field | Type | Notes |
|-------|------|-------|
| `host` | string | Echo of the requested host |
| `reachable` | bool | True when `ping` exited 0 (at least one reply) |
| `rttMs` | number \| null | Average round-trip ms when reachable; `null` otherwise |

**Fail-soft:** an unreachable host, 100% packet loss, an unknown host, or a
missing `ping` binary all return `reachable:false` with `rttMs:null` — never a
protocol error:

```json
{"host":"1.1.1.1","reachable":false,"rttMs":null}
```

A missing `<host>` body is `error:usage: net-ping <host> [count]\n`.

### Power / idle (`zbus` / logind + UPower)

#### `power-can-suspend`

Whether the system can suspend (logind `CanSuspend`).

**Response:**

| Condition | Response |
|-----------|----------|
| Suspend allowed | `yes\n` |
| Suspend not allowed / query failed | `no\n` |
| Non-Linux build | `error:unsupported on this platform\n` |

#### `power-suspend`

Suspend the system (logind `Suspend(false)`).

**Response:** `ok\n` on success, `error:<detail>\n` on failure. Non-Linux:
`error:unsupported on this platform\n`.

#### `power-battery`

Battery state. The deploy host is typically a desktop, so "no battery" is the normal case
and is reported gracefully (never an error) whenever UPower / a battery device
is absent.

**Response:** A compact single-line JSON **object**.

No battery present:

```json
{"present":false}
```

Battery present:

```json
{"present":true,"percentage":74,"state":"discharging","onBattery":true,"icon":"battery-good-symbolic"}
```

| Field | Type | Notes |
|-------|------|-------|
| `present` | bool | `false` ⇒ the object has no other keys |
| `percentage` | number | Charge percentage, rounded to a whole number |
| `state` | string | `unknown` / `charging` / `discharging` / `empty` / `full` / `pending-charge` / `pending-discharge` (UPower state 0–6) |
| `onBattery` | bool | True when running on battery (line power absent) |
| `icon` | string | UPower icon name (e.g. `battery-good-symbolic`) |

On a non-Linux build: `error:unsupported on this platform\n`.

### `intent <name>`

Inject a shell *intent* into the broadcast bus — the daemon's first-class,
headless **control surface**. Any accepted `intent <name>` re-broadcasts to all
`subscribe` clients as an `intent:<name>` event (see
[Intents](#intents)). The daemon does not know or care who issued it, so the
keyboard global-escape (Hyprland `Super` bind), screenshot/automation, and the
daemon's own gamepad logic all ride the **identical** path.

`<name>` is a single whitespace-trimmed token validated against a **closed
vocabulary**. Unknown names are rejected; the command **touches no device** (it
is a pure broadcast).

| Intent | Meaning |
|--------|---------|
| `home` | Global return-to-shell escape (keyboard `Super`, automation). Always leaves the running app. Distinct from the gamepad neutrals below. |
| `home-tap` | Gamepad Home neutral — a short Home press. QML routes it (typically to `menu` when the shell is focused). |
| `home-hold` | Gamepad Home neutral — a long Home press. QML routes it (typically to the return-to-shell / reset path). |
| `menu` | Toggle the navigation drawer. |
| `settings` | Open settings. |
| `power` | Open the power menu. |

`home` is the global escape; `home-tap`/`home-hold` are the **neutral** gamepad
Home signals (QML, which owns focus, decides what each means). This split keeps
the daemon free of any focus/state knowledge.

> **Directional nav / select / back are NOT intents.** They are *keyboard-layer*
> concerns served by real key events — the gamepad d-pad/A/B (which the daemon
> synthesizes to `KEY_*`), `wtype -k`, or the [`key <name>`](#key-name) command —
> and handled by each surface's `KeyNavigation`/`Keys`. Earlier revisions listed
> `nav-up/down/left/right`, `select`, `back` here, but nothing produced or
> consumed them (a focus move has no state-dependent decision for QML to make);
> they were removed in favor of `key <name>`. The intent surface is only the
> high-level, focus-*independent* actions above.

#### Deep-link targets

In addition to the coarse vocabulary, `<name>` may be a namespaced deep-link
target in the form `<ns>:<leaf>`. Deep-links ride the existing `Intent(String)`
wire path — no new variants.

| Target | Effect |
|--------|--------|
| `settings:<page>` | Open the Settings panel on the named page. `<page>` is one of the section id slugs: `audio`, `bluetooth`, `network`, `display`, `controllers`, `keybindings`, `avcontrol`, `appearance`, `accessibility`, `power`, plus the provider id (e.g. `streaming` or the active provider's own id) when a streaming provider is configured. |
| `overlay:volume` | Open the volume QAM popover from idle. |
| `overlay:network` | Open the network QAM popover from idle. |
| `overlay:session` | Open the power/session drawer from idle. |
| `app:<id>` | Launch the local app whose `wmClass` (StartupWMClass) is `<id>`. |

**Validation boundary:** the daemon validates the namespace and structural shape.
The `overlay:` namespace is **closed** — only `volume`, `network`, and `session` are valid
leaves; anything else is rejected. The `settings:` and `app:` namespaces accept
any non-empty leaf (the page/app registries live in QML, not the daemon). An
empty leaf (`settings:`, `overlay:`, `app:`) or unknown namespace (`foo:bar`)
returns `error:unknown intent '<name>'` and no event is broadcast. A typo'd
settings page or absent app is accepted by the daemon (`ok` + broadcast) but is
a **graceful no-op in QML** (logged, no crash). Deep-link targets are extensible
by namespace.

**Example (open Bluetooth settings directly):**

```
echo "intent settings:bluetooth" | nc -U "$TV_SHELL_SOCK"
```

| Condition | Response |
|-----------|----------|
| `<name>` in the closed coarse vocabulary OR a valid deep-link target | `ok\n` (and an `intent:<name>` event is broadcast) |
| `<name>` outside the vocabulary / invalid deep-link | `error:unknown intent '<name>'\n` (no event) |
| Missing/empty `<name>` body | `error:usage: intent <name>\n` |

**Example (automation):**

```
echo "intent home" | nc -U "$TV_SHELL_SOCK"
```

### `rumble <id> <ms>`

Fire a rumble (haptic `FF_RUMBLE`) effect on the pad whose stable wire `id` (the
`get-pads` / `pad:*` id) is `<id>`, for `<ms>` milliseconds. Part of the fleet
*outputs* ride-along (#99). The shell fires this on meaningful events (e.g. a
launch confirmation); the daemon itself also pulses a connecting pad.

`<id>` is a single whitespace-trimmed token (the wire id, which may itself
contain `:`); `<ms>` is a non-negative integer.

This is a **best-effort, cap-gated no-op**:

- a no-op (still `ok`) if no pad has that wire id,
- a no-op if the pad has no force feedback (`EV_FF` / `FF_RUMBLE`),
- a no-op if the persisted **`rumbleEnabled`** setting is `false` (default
  `true`).

| Condition | Response |
|-----------|----------|
| Accepted (fired, or a cap-gated no-op) | `ok\n` |
| Missing/incomplete `<id> <ms>`, or non-integer `<ms>` | `error:usage: rumble <id> <ms>\n` |

**Example (automation):**

```
echo "rumble uniq:e4:17:d8:01:02:03 200" | nc -U "$TV_SHELL_SOCK"
```

> **`rumbleEnabled` setting.** A QML-owned boolean in `settings.json`
> (default `true`) gating all daemon-fired rumble — both the `rumble` command and
> the connect pulse. The daemon caches this flag at startup and refreshes it on a
> successful `set-config`, so a toggle takes effect immediately with no per-rumble
> disk read.

### `key <name>`

Synthesize a single keystroke (press + release) on the daemon's **shared virtual
keyboard** — the headless counterpart to a gamepad d-pad/A/B tap or a `wtype -k`.
It reaches whatever surface currently holds Wayland focus, exactly like the
gamepad's own nav (the daemon already maps the d-pad to these same `KEY_*`
codes). This is the **socket-driven navigation surface** for automation and
screenshot tours, so a script can drive the entire UI over one connection.

Unlike [`intent`](#intent-name), `key` **does touch the device** — that is the
whole point. The two surfaces stay distinct: `intent` is pure-broadcast *control*
(state actions QML interprets), `key` is *synthesized input* (focus moves QML
never sees as intents).

`<name>` is a single whitespace-trimmed token in a **closed vocabulary**:

| Name | Key | Use |
|------|-----|-----|
| `up` / `down` / `left` / `right` | `KEY_UP` / `KEY_DOWN` / `KEY_LEFT` / `KEY_RIGHT` | Move focus (`KeyNavigation`). |
| `select` | `KEY_ENTER` | Confirm / activate the focused element (A). |
| `back` | `KEY_ESC` | Cancel / go back (B). |

| Condition | Response |
|-----------|----------|
| `<name>` in the closed vocabulary | `ok\n` (keystroke emitted) |
| `<name>` outside the vocabulary | `error:unknown key '<name>'\n` (nothing emitted) |
| Missing/empty `<name>` body | `error:usage: key <name>\n` |

**Example (automation — open Settings from home, then walk down the sidebar):**

```
echo "intent settings" | nc -U "$TV_SHELL_SOCK"   # control surface
echo "key down"         | nc -U "$TV_SHELL_SOCK"   # synthesized input
echo "key select"       | nc -U "$TV_SHELL_SOCK"
```

## Phase 4 Commands (Hyprland + Sunshine)

Phase 4 adds two subsystems: a Hyprland compositor actor (direct IPC sockets, no
crate — see `hyprland.rs`) and a Sunshine session detector (`reqwest` over the
host's self-signed HTTPS endpoint).

The **Hyprland** commands replace the `hyprctl clients -j` read in
`components/HyprctlClients.qml` and feed `components/AppLifecycleManager.qml`'s
window-event watching. They are **Linux-only** (the Hyprland IPC socket): on a
non-Linux build (or any host where the Hyprland actor failed to start), they
reply `error:unsupported on this platform\n`. User-triggered, one-shot
compositor *actions* (`hyprctl dispatch exec/closewindow`) stay shell-outs in
the QML. The one exception: on every `openwindow` event the actor itself
dispatches `focuswindow address:<addr>` + `fullscreen 0 set` to force the new
window to take over the screen, class-agnostic — see `force_fullscreen` in
`hyprland.rs`. That's a blanket kiosk policy, not a per-app QML decision, so it
lives in the actor that already owns the event.

The **Sunshine** command (`sunshine-status`) is **stateless and cross-platform**
— served directly by the daemon's IPC layer (no actor round-trip, like
`list-apps`), since `reqwest` runs everywhere. It replaces the inline Sunshine
HTTP polls in `components/StreamManager.qml` and `StreamCard.qml`. The streamed
Phase 4 events are documented under
[Phase 4 Events](#phase-4-events).

### Hyprland (direct IPC sockets)

#### `hypr-active`

Query the active window. Sends `j/activewindow` to Hyprland's request socket
(`.socket.sock`) — no `hyprctl` shell-out.

**Response:** A compact single-line JSON **object** describing the active window,
or `{}` when no window is focused (or on any IPC failure, e.g. the Hyprland
socket is absent):

```json
{"class":"firefox","title":"Mozilla Firefox","address":"0x55a1b2c3d4e5","fullscreen":false}
```

| Field | Type | Notes |
|-------|------|-------|
| `class` | string | Active window's class (empty string allowed) |
| `title` | string | Active window's title |
| `address` | string | Hyprland window address (e.g. `0x…`) |
| `fullscreen` | bool | Whether the active window is fullscreen. Lets QML read the state on the initial query, before any live `hypr:fullscreen:<0\|1>` event arrives. Derived from Hyprland's `fullscreen` field (a bool or an integer fullscreen-mode); `true` for a bool `true` or any nonzero mode, `false` for `false`/`0`/absent. |

On a non-Linux build: `error:unsupported on this platform\n`.

#### `hypr-clients`

List all Hyprland clients, mirroring what `hyprctl clients -j` gave the QML.
Sends `j/clients` to Hyprland's request socket (no `hyprctl` shell-out).

**Response:** A compact single-line JSON **array** of client objects:

```json
[{"class":"firefox","title":"Mozilla Firefox","address":"0x55a1b2c3d4e5","workspace":"1"}]
```

| Field | Type | Notes |
|-------|------|-------|
| `class` | string | Window class |
| `title` | string | Window title |
| `address` | string | Hyprland window address |
| `workspace` | string | Workspace name (matches the QML's `workspace.name` read) |

An empty result (or any IPC failure) is `[]`. On a non-Linux build:
`error:unsupported on this platform\n`.

#### `hypr-monitors`

List all Hyprland monitors. Sends `j/monitors` to Hyprland's request socket.

**Response:** A compact single-line JSON **array** of monitor objects:

```json
[{"name":"DP-1","description":"LG OLED","width":3840,"height":2160,"refreshRate":120.0,"scale":1.0,"x":0,"y":0,"activeWorkspace":"1","dpmsStatus":true,"vrr":true,"availableModes":["3840x2160@120.00000"],"currentFormat":"XRGB2101010","hdr":true}]
```

| Field | Type | Notes |
|-------|------|-------|
| `name` | string | Monitor name (e.g. `DP-1`) |
| `description` | string | Human-readable description |
| `width` | number | Current resolution width in pixels |
| `height` | number | Current resolution height in pixels |
| `refreshRate` | number | Current refresh rate in Hz |
| `scale` | number | DPI scale factor |
| `x` | number | X position in the global compositor layout |
| `y` | number | Y position in the global compositor layout |
| `activeWorkspace` | string | Name of the active workspace on this monitor |
| `dpmsStatus` | bool | `true` = display powered on |
| `vrr` | bool | Variable refresh rate enabled |
| `availableModes` | array | Mode strings in `WxH@Hz` format |
| `currentFormat` | string | Current pixel format (e.g. `XRGB2101010`, `XRGB8888`) |
| `hdr` | bool | **Derived**: `true` when `currentFormat` contains `"2101010"` (10-bit packed formats indicate HDR/wide-gamut path). Hyprland exposes no explicit HDR flag; 10-bit `currentFormat` is the proxy. |

An empty result (or any IPC failure) is `[]`. On a non-Linux build:
`error:unsupported on this platform\n`.

### Sunshine session detection (`reqwest`)

> **Security note (TLS):** `sunshine-status` talks to Sunshine over HTTPS with
> `danger_accept_invalid_certs` — Sunshine ships a self-signed cert that can't be
> verified, so the channel is **encrypted but not authenticated** (no protection
> against an active MITM). It's a read-only `/serverinfo` probe carrying no
> credentials, but only use it against hosts on a **trusted LAN**.

#### `sunshine-status <host> <port>`

Pre-flight check the QML shell runs before launching a Moonlight stream: is the
host up, are we paired, and is another app already streaming? The `<host>` and
`<port>` are two whitespace-trimmed tokens after the command word; `<port>` is
the host's HTTPS port (Sunshine's self-signed `/serverinfo` endpoint, e.g.
`47990`). The fetch accepts the self-signed cert (rustls
`danger_accept_invalid_certs`).

Stateless and cross-platform — served directly by the daemon's IPC layer (no
actor round-trip), so it works on every platform, including a non-Linux build.

**Response:** A compact single-line JSON **object**:

```json
{"status":"ok","online":true,"paired":true,"currentApp":"881448767","httpsPort":47990}
```

| Field | Type | Notes |
|-------|------|-------|
| `status` | string | Shared service-health vocabulary (see [Service health](#service-health)). `ok` ⇔ `online:true`; `unreachable` otherwise (the serverinfo port is unauthenticated, so failures are never `error`) |
| `online` | bool | The host responded with a parseable `/serverinfo` document |
| `paired` | bool | `<PairStatus>1</PairStatus>` — this client is paired with the host |
| `currentApp` | string | Id of the app currently streaming, or `""` when idle. Busy only when `<state>` ends in `SERVER_BUSY` **and** `<currentgame>` is a non-zero id |
| `httpsPort` | number | `<HttpsPort>` from the document (`0` when absent/unparseable) |

A host that is unreachable / times out / returns a non-2xx or non-serverinfo
body degrades to the offline object (the command does not error):

```json
{"status":"unreachable","online":false,"paired":false,"currentApp":"","httpsPort":0}
```

| Condition | Response |
|-----------|----------|
| Success / host unreachable | The JSON object above |
| Missing or incomplete `<host> <port>` body | `error:usage: sunshine-status <host> <port>\n` |

The response *parser* is a pure, unit-tested function (parses Sunshine's
`/serverinfo` XML into the object above).

### Plex hubs (home-screen Plex widget)

#### `plex-hubs`

Fetch the Plex server's **On Deck** (continue-watching / up-next) and **Recently
Added** hubs for the home-screen Plex widget. Bare command — no body. Stateless
and cross-platform (`reqwest`), served like `sunshine-status`.

Config comes from the **`[plex]` section** of `~/.config/tv-shell/config.toml`:

- `url` — server base, e.g. `https://plex.example.com`
- `token_file` — path to a `0600` file (under `~/.config/tv-shell/`) holding an
  `X-Plex-Token`. Token-file only; inline tokens are not supported.

The token never leaves the daemon: the reply embeds ready-to-load, tokenized
poster URLs (`/photo/:/transcode?…&X-Plex-Token=…`) so the QML `Image` loads art
directly without ever seeing the credential.

Reply (compact single-line JSON):

```json
{"status":"ok",
 "onDeck":[{"title":"Dexter","subtitle":"S6 · E2 · Once Upon a Time...","kind":"episode","art":"https://…","progress":0.0}],
 "recentlyAdded":[{"title":"Skyfall","subtitle":"2012","kind":"movie","art":"https://…","progress":0.05}]}
```

| Field | Meaning |
|-------|---------|
| `status` | Shared service-health vocabulary (see [Service health](#service-health)): `disabled` (env unset — widget collapses), `ok`, `unreachable` (server down / 5xx — widget shows a notice), or `error` (auth/4xx). A lightweight `/identity` probe decides reachability **before** the hubs are fetched, so a down server is distinct from an empty library |
| `onDeck` / `recentlyAdded` | up to 16 / 24 items; each `{title,subtitle,kind,art,progress}`. Empty unless `status` is `ok` |
| `progress` | 0..1 resume position (On Deck partially-watched items); 0 otherwise |

### Steam library / launch (home-screen Steam widget)

These commands back the home-screen Steam widget and the Steam-card launch
choreography. All are **stateless and cross-platform** — the daemon is an HTTP
**client** proxying to the `tv-shell-host` sidecar running on the gaming PC (see
[HOST_SETUP.md](HOST_SETUP.md)); it never spawns or supervises Steam. Config
comes from the **`[steam]` section** of `config.toml` — either a single sidecar
(`url` + `token_file`) or several named **`[[steam.hosts]]`** entries, of which
exactly one is **active** at a time (selected via `steam-set-host`, persisted as
the daemon-owned `steamServer` key in settings.json, default = the first entry).
The daemon↔sidecar JSON contract is single-sourced in the `tv-shell-protocol`
crate.

#### `steam-library`

Fetch the ACTIVE host's Steam library for the widget's poster grid. Bare
command — no body. Proxies the sidecar's library endpoint.

**Reply (compact single-line JSON):**
`{"status":…,"recentlyPlayed":[…],"allGames":[…],"host":…}`.
`status` uses the shared service-health vocabulary (see
[Service health](#service-health)); an unconfigured `[steam]` returns
`{"status":"disabled",…}` and the widget collapses. Each game entry carries its
appid, title, and (tokenized) poster art URL. `host` is the active sidecar's
URL name-part (IP/hostname, or null when unconfigured) — present on down/empty
replies too, so the widget's Wake card targets the server actually being
checked rather than a separately-configured streaming target.

#### `steam-hosts`

List the configured sidecars and which one is active. Bare command — no body;
a cheap config read (no network).

**Reply (compact single-line JSON):**
`{"status":"ok","active":<name>,"hosts":[{"name":…,"host":…}]}`, or
`{"status":"disabled","active":null,"hosts":[]}` when none are configured.
Each entry's `host` is the URL's name-part only — the full URL and token never
leave the daemon.

#### `steam-set-host <name>`

Select which configured sidecar the daemon actively checks. The body is the
host's configured name, **verbatim** (names may contain spaces). Persists to
settings.json (`steamServer`); the next `steam-library` poll and health probe
use the new host.

| Condition | Response |
|-----------|----------|
| Selection persisted | `ok\n` |
| `<name>` not a configured host | `error:unknown steam host "<name>"\n` |
| settings.json write failed | `error:steam-set-host failed: <reason>\n` |
| Missing / empty `<name>` body | `error:usage: steam-set-host <name>\n` |

#### `steam-launch <appid>`

Navigate the host's Steam Big Picture to a game's page (proxies `POST /launch` to
the sidecar). The body is a **single numeric appid** token. Does **not** directly
`rungameid`-launch — the shell pairs it with a Moonlight stream client-side.

| Condition | Response |
|-----------|----------|
| Proxied OK | `ok\n` |
| Sidecar unconfigured/unreachable/error | `error:<reason>\n` |
| Missing / non-numeric `<appid>` body | `error:usage: steam-launch <appid>\n` |

#### `steam-bigpicture`

Reset the host's Steam Big Picture to its **home** screen (proxies `POST /open-bpm`
to the sidecar — no body). Bare command, mirroring `steam-launch` but landing on
the BPM home instead of a game page.

**Reply (compact single-line JSON status):** `{"status":"ok"}` / `{"status":"error",…}`.

#### `steam-quit <appid>`

Gracefully terminate a running Steam game on the host (proxies `POST /quit` with
`{appid}` to the sidecar). The body is a **single numeric appid** token, mirroring
`steam-launch`.

The host decides whether there is anything to quit and may **refuse** — it answers
HTTP 200 with `{"ok":false,"appid":…,"reason":"not running"}` when no live process
for that appid exists (see [HOST_SETUP.md](HOST_SETUP.md)). A refusal is a normal
answer, not a failure, so it is surfaced rather than flattened into `"ok"` — the
same three-outcome shape as `steam-suspend`:

**Reply (compact single-line JSON status):**

| Outcome | Reply |
|---------|-------|
| quit sent | `{"status":"ok"}` |
| refused by the host (nothing running) | `{"status":"error","reason":"<the host's reason>","refused":true}` |
| unreachable / bad token / decode failure | `{"status":"error","reason":"steam-quit failed: …","refused":false}` |
| steam unconfigured | `{"status":"disabled","reason":"steam not configured"}` |

The host's `reason` is passed through **verbatim** (it is written to be shown to a
person); `refused` lets a consumer branch without string-matching it. `"ok"` means
a matching game process was found and signalled — before this the daemon discarded
the host's body, so "nothing was running" was indistinguishable from a successful
quit.

A missing / non-numeric `<appid>` body returns `error:usage: steam-quit <appid>\n`.

#### `steam-suspend`

Put the **Steam host machine** to sleep (proxies `POST /sleep` to the sidecar — no
body). Bare command, like `steam-bigpicture`, so there is no usage error.

Not to be confused with **`power-suspend`**, which suspends the **local TV box**
via logind. This one suspends the *remote gaming PC*.

The host decides whether sleeping is safe and may **refuse** — it answers HTTP 200
with `{"ok":false,"reason":…}` while a game is running or a Sunshine session is
live/resumable (see [HOST_SETUP.md](HOST_SETUP.md)). A refusal is a normal answer,
not a failure, so it is surfaced rather than flattened:

**Reply (compact single-line JSON status):**

| Outcome | Reply |
|---------|-------|
| accepted | `{"status":"ok"}` |
| refused by the host | `{"status":"error","reason":"<the host's reason>","refused":true}` |
| unreachable / bad token / decode failure | `{"status":"error","reason":"steam-suspend failed: …","refused":false}` |
| steam unconfigured | `{"status":"disabled","reason":"steam not configured"}` |

The host's `reason` is passed through **verbatim** (it is written to be shown to a
person); `refused` lets a consumer branch without string-matching it. `"ok"` means
the suspend was accepted and dispatched, not that the machine is already asleep.

### Moonlight local-config "forget" (creds-free unpair)

#### `moonlight-forget <host>`

Remove a host from Moonlight's local config so THIS client is no longer paired
with it (the Moonlight settings "Unpair" row action). Unlike a Sunshine-side
unpair this needs **no credentials** — it only edits a local file the user owns.
After forgetting, the host's status flips to "not paired" and the **Pair** action
returns; re-pairing re-establishes it.

`<host>` is the single host token — the IP/hostname string the shell uses, e.g.
`192.0.2.1`. The daemon reads Moonlight's config
(`${XDG_CONFIG_HOME:-$HOME/.config}/Moonlight Game Streaming Project/Moonlight.conf`,
a QSettings INI) and, within its `[hosts]` array, finds the index whose
`hostname` / `localaddress` / `manualaddress` / `remoteaddress` equals `<host>`,
removes that index's lines, **renumbers** the remaining hosts contiguously
`1..k`, and updates the section `size=k`. All other content (other sections,
other hosts, the `srvcert` `@ByteArray(...)` blobs) is preserved verbatim — the
edit is line-based, not a QSettings round-trip.

Idempotent: a host that isn't found, or a missing conf, returns `ok` (nothing to
forget). Stateless and cross-platform — it's just file editing (no actor, no
feature gate). The core rewrite (`forget_host`) is a pure, unit-tested function;
the handler does read → `forget_host` → write off the reactor.

> Moonlight is invoked once per command (`moonlight stream/list/pair …`), not held
> as a persistent process, so editing the conf between invocations is safe — no
> live process's in-memory QSettings can clobber the edit.

**Response:**

| Condition | Response |
|-----------|----------|
| Host removed (or already absent / no conf) | `ok\n` |
| File read/write error | `error:<reason>\n` |
| Missing `<host>` body | `error:usage: moonlight-forget <host>\n` |


## HDMI-CEC Commands (#94, #16)

Persistent HDMI-CEC control via `cec-rs` / libcec. The daemon owns **one
in-process libcec connection** for its lifetime (a single-owner async actor), so
every command reuses that connection instead of spawning a subprocess — this is
the reliability fix behind #16's intermittent device detection.

**Feature-gated and Linux-only.** libcec is a Linux/udev C library that
`libcec-sys` links at build time, so the actor compiles only under
`#[cfg(all(target_os = "linux", feature = "cec"))]`. The daemon must be built
`cargo build --release --features cec` (on a host with libcec available) for
these commands to do anything. On a **default build** (no `cec` feature), a
**non-Linux build**, or any host where libcec fails to open at runtime, every
CEC command except `cec-*-usage` replies `error:unsupported on this platform\n`
(feature/platform off) or `error:libcec unavailable\n` (adapter absent/asleep).
The `cec-device`/`cec-power-on`/`cec-power-off` missing-address usage error is
cross-platform (`error:usage: <cmd> <addr>\n`).

> **Dropped fallback chain.** The daemon is now the *single* CEC owner. The
> former `living-room-cec` → `cec-ctl` → `cec-client` fallback chain that
> `AVControlSettings.qml` shelled out to is **removed**: a persistent in-process
> libcec connection is more reliable than per-call subprocesses (the subprocess
> lifecycle *was* the flakiness). `living-room-cec` remains available as an
> operator escape hatch but is no longer invoked by the daemon or the shell.

> **Device-info scope.** cec-rs 12.0.1 wraps no per-device metadata query (OSD
> name, physical address, vendor id, device type are commented-out TODOs in the
> crate). Device objects therefore carry only `logicalAddress` + `powerStatus`;
> the bus is enumerated by probing the 16 logical addresses
> (`get_device_power_status`, where Unknown ≡ absent). The QML derives friendly
> device names from the logical address.

### `cec-scan`

Scan the CEC bus and return all visible devices.

**Response:** a compact single-line JSON **array** of device objects:

```json
[{"logicalAddress":0,"powerStatus":"on"},{"logicalAddress":5,"powerStatus":"standby"}]
```

| Field | Type | Notes |
|-------|------|-------|
| `logicalAddress` | number | CEC logical address (0–15; TV=0, AudioSystem=5) |
| `powerStatus` | string | `on` / `standby` / `waking` / `sleeping` / `unknown` |

An empty result is `[]`. Feature/platform off: `error:unsupported on this platform\n`. libcec absent: `error:libcec unavailable\n`.

### `cec-device <addr>`

Return the device object for a single logical address. `<addr>` is a decimal
logical address (0–15).

| Condition | Response |
|-----------|----------|
| Device present | Compact JSON device object (same shape as a `cec-scan` element) |
| Device absent / `<addr>` not on bus | `error:no device at address <addr>\n` |
| `<addr>` out of range | `error:invalid address <addr>\n` |
| Missing `<addr>` argument | `error:usage: cec-device <addr>\n` |
| Feature/platform off | `error:unsupported on this platform\n` |

### `cec-power-on <addr>`

Send a CEC power-on (Image View On) command to the device at logical address
`<addr>`.

| Condition | Response |
|-----------|----------|
| Success | `ok\n` |
| Failure | `error:<detail>\n` |
| `<addr>` out of range | `error:invalid address <addr>\n` |
| Missing `<addr>` argument | `error:usage: cec-power-on <addr>\n` |
| Feature/platform off | `error:unsupported on this platform\n` |

### `cec-power-off <addr>`

Send a CEC standby command to the device at logical address `<addr>`.

| Condition | Response |
|-----------|----------|
| Success | `ok\n` |
| Failure | `error:<detail>\n` |
| `<addr>` out of range | `error:invalid address <addr>\n` |
| Missing `<addr>` argument | `error:usage: cec-power-off <addr>\n` |
| Feature/platform off | `error:unsupported on this platform\n` |

### `cec-active-source`

Announce this adapter as the CEC active source (switches all displays to this
input). This **is** the "Switch Input" primitive — there is no separate
`cec-switch-input` command.

**Response:** `ok\n` on success, `error:<detail>\n` on failure. Feature/platform
off: `error:unsupported on this platform\n`.

### `cec-health` (#19)

Return the current CEC **transmit-wedge health** — whether the adapter's last
transmit succeeded, returned `TransmitFailed`, or hasn't been attempted. The
Pulse-Eight USB adapter periodically enters a "transmit wedge": libcec opens and
can RECEIVE, but every TRANSMIT fails. This read surfaces that to the AV Control
page's status line. **Read-only** — it reports the last-known state and never
drives the bus.

**Response:** A compact single-line JSON object:

```json
{"transmit":"ok","reason":null,"since":1719500000000,"lastError":null}
```

| Field | Type | Meaning |
|-------|------|---------|
| `transmit` | string | `"ok"` (last transmit succeeded), `"failing"` (last transmit returned `TransmitFailed`), `"unknown"` (none attempted yet / indeterminate), or `"unavailable"` (the adapter isn't open — see `reason`) |
| `reason` | string \| null | `null` whenever the adapter is **open** (`transmit` ∈ {ok,failing,unknown}); otherwise the unavailable cause: `"no_libcec"` (daemon built without `--features cec`, or non-Linux), `"no_adapter"` (libcec found ZERO adapters — no hardware), or `"adapter_open_failed"` (an adapter IS present but the libcec open handshake failed — the hardware "wedge"; the actionable truth is "adapter detected but not responding — re-seat it") |
| `since` | number | Epoch **milliseconds** (UTC) of the last state CHANGE (`0` for the static `no_libcec` reply) |
| `lastError` | string \| null | The last transmit error string while `failing`, else `null` (always `null` when `unavailable`) |

The health state is also refreshed automatically as a side effect of `cec-scan`
(so the QML 30s scan poll keeps the line fresh), and a `cec:health:<json>` event
is broadcast whenever the state changes — including once when the libcec open
handshake fails, carrying `transmit:"unavailable"` + the `reason`.

For `cec-health` and `cec-test` the unavailable cases now return this **structured
JSON** (`{"transmit":"unavailable","reason":…}`) instead of the old bare
`error:libcec unavailable\n` / `error:unsupported on this platform\n` — so the AV
Control page can show an accurate per-cause message. The other CEC commands
(`cec-scan` / `cec-device` / `cec-power-on` / `cec-power-off` /
`cec-active-source`) still reply the bare `error:unsupported on this platform\n`
(feature/platform off) or `error:libcec unavailable\n` (adapter absent/wedged).

### `cec-test` (#19)

Run an explicit on-demand, **side-effect-free** CEC poll probe (a `<Polling
Message>` to the TV at logical addr 0 — no power/input change), update the
transmit-health, broadcast `cec:health` if it changed, and reply with the same
JSON object as `cec-health`. This is the "Test CEC" button's backend.

**Response:** The health JSON object (same shape as `cec-health`, including the
`reason` field). When the adapter isn't open it returns the same structured
`{"transmit":"unavailable","reason":…}` object as `cec-health` (`no_libcec` when
the feature/platform is off, `no_adapter` / `adapter_open_failed` when the open
handshake failed) — not a bare `error:` line.

### Session-lifecycle CEC (`[cec] lifecycle`)

Beyond the manual `cec-*` commands above, the daemon can drive the AV on session
lifecycle transitions. This is **separate** from the manual commands — it is an
internal behavior with no IPC verb, gated entirely by a config flag.

| Key | Purpose |
|-----|---------|
| `[cec] lifecycle` | Enable daemon-owned CEC lifecycle (a TOML bool, default `false`). **Unset/`false` → disabled (the default).** Set it in `~/.config/tv-shell/config.toml` on the deploy host. |

When **enabled** (and the daemon is built `--features cec` on a host with a
working libcec adapter), the daemon:

- **Wakes on start:** when the libcec connection opens, powers on the **AVR
  (logical address 5)** then the **TV (logical address 0)**, waits briefly for
  the display to leave standby, then claims active source (switches the TV to
  this input).
- **Wakes on resume:** on logind `PrepareForSleep(false)` (system resumed from
  suspend), runs the same wake sequence.
- **Standby on suspend:** on logind `PrepareForSleep(true)` (system about to
  suspend), sends CEC standby to the **TV (0)** then the **AVR (5)**.
- **Standby on session end:** on a real shutdown (SIGTERM/SIGINT from the
  session wrapper), sends the same standby before exiting. A re-exec restart
  (`/dev/restart-daemon`) is **skipped** so the AV stays awake across it.

- **Remote input -> navigation:** registers a libcec key-press callback so
  TV/AVR **remote buttons** arriving on the CEC bus are injected as the SAME
  synthesized keyboard nav events the gamepad d-pad produces (via the
  `Control::Key` path → `config::key_for_action`). This replaces the retired
  kernel `pulse8-cec` evdev "Pulse-Eight CEC Adapter" input device. The callback
  fires on libcec's own thread and only forwards the keypress over a channel; a
  dedicated forwarder thread debounces it (acting on the **initial press only**,
  i.e. libcec `duration == 0`; the release event with a non-zero held duration
  is ignored) and maps the CEC user-control code to a nav action:

  | CEC user-control code | nav action | emitted key |
  |-----------------------|------------|-------------|
  | `UP`     | `up`     | `KEY_UP`    |
  | `DOWN`   | `down`   | `KEY_DOWN`  |
  | `LEFT`   | `left`   | `KEY_LEFT`  |
  | `RIGHT`  | `right`  | `KEY_RIGHT` |
  | `SELECT` | `select` | `KEY_ENTER` |
  | `EXIT`   | `back`   | `KEY_ESC`   |

  All other codes (menus, media transport, number/colour keys) are ignored —
  no new nav vocabulary is introduced. Gated by the **same**
  `[cec] lifecycle` flag as the wake/standby behavior, so a default
  build, a host without the flag, or dev/CI never inject keys.

> **Display-ownership gate (safety invariant, not a setting).** The wake and
> standby paths above never transmit for a display this box does not own.
> Ownership is tracked **passively** from received bus traffic — the daemon
> registers a command-received callback and remembers who last broadcast
> `<Active Source>` (`<Inactive Source>` clears it) — so the gate keeps working
> on an adapter whose transmits all fail, and costs the suspend path nothing.
>
> - **Standby** runs only on positive proof the last claim was ours. Another
>   device's claim, or no claim observed at all (a daemon started mid-session, or
>   a connection reopened after a transmit failure — both reset to "unknown"),
>   skips it entirely, so suspending this box can't power off a TV someone is
>   watching on another input.
> - **The wake active-source claim** is skipped only when a different *known*
>   device holds active source (requiring "we already hold it" would make the
>   claim a permanent no-op). The AVR/TV **power-on still runs** — waking an idle
>   display is what the user asked for; stealing a live input is the harm.
>
> A skip replies `ok`, performs zero transmits, is excluded from `cec-health`,
> and logs an `info` line naming the observed owner and our own address. Manual
> `cec-active-source` / `cec-power-off` are deliberate user actions and stay
> unconditional (a successful manual claim does record us as the owner).
>
> Limitation: the cache reflects only traffic observed since the connection
> opened, so a freshly-started daemon is deliberately conservative — it will not
> standby a display until it has seen a claim.

When **disabled** (the default) the CEC actor still serves the manual `cec-*`
commands, but performs **none** of the above — it never auto-drives the bus on
start, suspend/resume, or shutdown. This keeps CI, dev boxes, and any host
without the flag from ever powering a TV/AVR on or off. (On a default build with
no `cec` feature, the lifecycle wiring is compiled out entirely.)

> **Address mapping:** AVR = CEC logical address **5** (Audiosystem), TV = CEC
> logical address **0** (Tv).


## LAN HTTP Control Bridge (#151)

An optional, LAN-bound HTTP/1.1 listener that maps `POST /intent/<target>`,
`POST /key/<name>`, and `GET /screenshot` onto the daemon's existing intent/key
broadcast paths and the `grim` screenshotter, so Home Assistant `rest_command` /
curl / scripts can drive the shell without needing a Unix socket client.

### Opt-in via `config.toml`

| Key (`[http]`) | Purpose |
|----------------|---------|
| `bind` | `host:port` address to bind (e.g. `192.168.1.50:8731` or `0.0.0.0:8731`). When **omitted** (the default), no TCP socket is opened and no control surface is exposed. |
| `token_file` | Path to a `0600` file holding the bearer token (the token is **by reference only**, never inline). When auth is enabled (the default), every request must carry `Authorization: Bearer <token>` (constant-time match); requests without a valid token receive 401. |
| `auth_enabled` | Auth toggle (a TOML bool, default `true`). Set to `false` to skip auth entirely for local-only dev. When auth is enabled but `token_file` resolves no token, **all requests are rejected with 401** (secure by default — you cannot authenticate without a token). |

> **Security note**: bind to a trusted LAN interface (e.g. `192.168.1.x:8731`),
> not a public one. The bridge is a control surface — a mis-bound listener would
> expose shell control to the public internet. Pair with `token_file` for
> defence-in-depth even on a LAN. The daemon **refuses to start** on a non-loopback
> bind with dev tools on and auth effectively off unless `[dev] allow_insecure_lan
> = true`.

Default port suggestion: **8731**. The bind address must include the port.

### Per-box opt-in via `config.toml`

The daemon reads an optional machine-local typed config at startup, so per-box
overrides survive git deploys without touching tracked files:

```
~/.config/tv-shell/config.toml
```

Example to opt a box into the LAN HTTP bridge:

```toml
# ~/.config/tv-shell/config.toml
[http]
# Bind the HTTP bridge to the LAN interface on this box.
bind = "192.168.1.50:8731"
# A 0600 file holding the bearer token (required when auth is enabled, the default).
token_file = "~/.config/tv-shell/http-token"
# Uncomment to disable auth entirely for local-only dev:
# auth_enabled = false
```

### Routes

| Method | Path | Action |
|--------|------|--------|
| `POST` | `/intent/<target>` | Forward `<target>` to the [`intent <name>`](#intent-name) surface. `<target>` is the full remainder after `/intent/`, percent-decoded (`settings%3Abluetooth` → `settings:bluetooth`). The daemon's existing vocabulary gate applies — unknown intents return 400. |
| `POST` | `/key/<name>` | Forward `<name>` to the [`key <name>`](#key-name) surface (synthesize a keystroke). |
| `GET` | `/screenshot` or `/screenshot.png` | Capture the current screen via `grim -` and return the PNG bytes with `Content-Type: image/png`. Auth applies (the screenshot exposes screen content). Returns 500 if `grim` fails or is not installed. |
| Any other method | Any path | 405 |
| POST | Any other path (outside the documented set — see note) | 404 |

> **Also registered when the bridge is bound** (documented in their own sections,
> not repeated in this table): the dev-control routes `GET /dev/status`,
> `GET /dev/logs`, and `POST /dev/{deploy,build,restart-shell,restart-daemon}`
> (see [Dev-control surface](#dev-control-surface-dev-167)), plus the auth-exempt
> `GET /metrics` (see [OBSERVABILITY.md](OBSERVABILITY.md)). The "any other path →
> 404 / any other method → 405" fall-through applies only to requests **outside**
> that full documented route set.

`<target>` is the **same string** the Unix-socket `intent` command accepts,
including deep-link namespaces (`settings:<page>`, `overlay:volume`,
`overlay:network`, `app:<wmClass>`). Unknown vocabulary → 400 (the daemon's
`is_known_intent` gate is the single source of truth; the HTTP layer does not
re-validate).

### HTTP status mapping

| Status | Meaning |
|--------|---------|
| 200 | Request accepted (`ok`, or PNG body for `/screenshot`) |
| 400 | Unknown intent or key (daemon returned `error:*`) |
| 401 | Missing or invalid `Authorization: Bearer <token>`, or auth enabled with no token configured |
| 404 | Unknown POST route |
| 405 | Wrong method for the requested path |
| 500 | `grim` failed or is not installed (`/screenshot` only) |
| 503 | Daemon unavailable (control channel closed) |

### Home Assistant `rest_command` example

```yaml
# configuration.yaml
rest_command:
  tv_shell_intent:
    url: "http://192.168.1.50:8731/intent/{{ intent }}"
    method: POST
    headers:
      Authorization: "Bearer {{ token }}"
  tv_shell_key:
    url: "http://192.168.1.50:8731/key/{{ key }}"
    method: POST
    headers:
      Authorization: "Bearer mysecret"
  tv_shell_screenshot:
    url: "http://192.168.1.50:8731/screenshot"
    method: GET
    headers:
      Authorization: "Bearer mysecret"
```

Usage in an automation:

```yaml
action: rest_command.tv_shell_intent
data:
  intent: "settings:bluetooth"
  token: "mysecret"
```

### curl examples

```bash
# Open Bluetooth settings (no auth)
curl -X POST http://192.168.1.50:8731/intent/settings:bluetooth

# Same with bearer token
curl -X POST http://192.168.1.50:8731/intent/settings:bluetooth \
     -H "Authorization: Bearer mysecret"

# Colon percent-encoded (HA encodes `:` as `%3A`)
curl -X POST http://192.168.1.50:8731/intent/settings%3Abluetooth

# Synthesize a key press
curl -X POST http://192.168.1.50:8731/key/select

# Capture a screenshot (returns image/png)
curl -H "Authorization: Bearer mysecret" \
     http://192.168.1.50:8731/screenshot > screenshot.png

# Screenshot without auth ([http] auth_enabled = false)
curl http://192.168.1.50:8731/screenshot > screenshot.png
```

### Relation to the Unix socket intent surface

The HTTP bridge builds on the deep-link vocabulary introduced in issue #150.
`<target>` is the same string the socket `intent` command accepts. See
[`intent <name>`](#intent-name) for the full vocabulary and deep-link namespace
documentation.

### Dev-control surface (`/dev/*`) (#167)

An additional set of routes for iterating on the shell from a remote machine — deploy
a new git ref, build the daemon, restart quickshell, tail logs, and query daemon state,
all over HTTP. These routes require a freshly-built binary (issues #167 + #165).

All `/dev/*` routes are gated by the same `[http] auth_enabled` bearer auth as the
rest of the bridge, and are LAN-only by design. `/dev/deploy` and `/dev/build`
execute arbitrary code on the host and are **RCE-by-design** — acceptable for a
trusted LAN box, but never expose the bridge to a public interface. (This is
exactly the combination `DaemonConfig::validate` refuses on a non-loopback bind
with auth off, absent `[dev] allow_insecure_lan = true`.)

The daemon reads its options from a typed `~/.config/tv-shell/config.toml` at
startup, and separately self-discovers session env (#165): it resolves
`WAYLAND_DISPLAY` and `HYPRLAND_INSTANCE_SIGNATURE` from `$XDG_RUNTIME_DIR` when
they are not inherited. This means the bridge binds and subprocesses (`grim`,
`quickshell`) work correctly even when the daemon is launched before
`exec Hyprland` in the session script.

#### Route table

| Method | Path | Query params | Description |
|--------|------|--------------|-------------|
| `GET` | `/dev/status` | — | JSON status blob for the running daemon. |
| `GET` | `/dev/logs` | `lines=N`, `filter=F` | Tail `/tmp/qs-log.txt`. Graceful 200 when the file is absent. |
| `POST` | `/dev/restart-shell` | — | Kill and relaunch quickshell detached via `setsid`. Returns WARN/ERROR tail. |
| `POST` | `/dev/build` | — | `cargo build --release` in `daemon/` + install binary to `bin/`. ~15 s; connection timeout is 180 s. |
| `POST` | `/dev/deploy` | `ref=<ref>` | `git fetch origin --prune` + `checkout -f <ref>` + `reset --hard origin/<ref>`. Default ref: `main`. |
| `POST` | `/dev/restart-daemon` | — | Self re-exec: replies `ok, re-execing`, then calls `execv` of the installed binary. Same PID; pads re-grabbed; bridge rebinds in ~3 s. |

#### `GET /dev/status`

Returns a JSON object with a snapshot of the running daemon's state. No query params.

**Response** (`Content-Type: application/json`):

```json
{
  "sha": "a1b2c3d",
  "daemon_pid": 12345,
  "version": "0.1.0",
  "shell_running": true,
  "wayland_display": "wayland-1",
  "hypr_sig_present": true
}
```

| Field | Type | Notes |
|-------|------|-------|
| `sha` | string | Short git SHA of HEAD in the install root (`unknown` when git is unavailable) |
| `daemon_pid` | number | PID of the running daemon process |
| `version` | string | Cargo package version baked in at build time |
| `shell_running` | bool | True when a `quickshell` process is currently running (`pgrep -x quickshell`) |
| `wayland_display` | string \| null | Resolved Wayland display socket name; `null` when not found |
| `hypr_sig_present` | bool | True when `HYPRLAND_INSTANCE_SIGNATURE` (or its `$XDG_RUNTIME_DIR/hypr/` equivalent) is resolvable |

#### `GET /dev/logs[?lines=N&filter=F]`

Tail `/tmp/qs-log.txt` — the quickshell log file created (or appended to) by
`/dev/restart-shell`. **The file only exists after a `/dev/restart-shell`** call; the
boot launch of quickshell is not redirected there. Returns a graceful 200 with an
advisory message when the file is absent.

| Query param | Default | Description |
|-------------|---------|-------------|
| `lines` | `100` | Maximum number of lines to return (counting from the end). |
| `filter` | _(none)_ | Case-insensitive substring filter applied to each line before tail truncation. |

**Response:** Plain text; the last `lines` lines of the file (after optional filter),
one line per line. 200 even when the file is absent:

```
(no /tmp/qs-log.txt yet — POST /dev/restart-shell to capture quickshell logs)
```

#### `POST /dev/restart-shell`

Sends `pkill -x quickshell` (ignoring failures if quickshell is not running), then
spawns a new `quickshell -c tv-shell` process detached via `setsid` with stdout and
stderr redirected to `/tmp/qs-log.txt`. Session env (`WAYLAND_DISPLAY`,
`HYPRLAND_INSTANCE_SIGNATURE`, `XDG_RUNTIME_DIR`) is injected into the child via
`session_env`. The handler waits 3 seconds for initial log output, then returns the
WARN/ERROR tail (up to 30 lines, excluding noisy `COULD NOT LOAD ICON` lines).

**Response:** Plain text — WARN/ERROR lines from the first 3 s of the log, or:

```
started (no WARN/ERROR in first 3s)
```

#### `POST /dev/build`

Runs `cargo build --release` inside the `daemon/` subdirectory of the install root
(resolved via `session_env::install_root()` — from `current_exe` / `$TV_SHELL_DIR`;
`/opt/tv-shell` is only a last-ditch default). On success, installs the built binary to `bin/tv-shell-input`
with `install -m755`. Returns the last 12 lines of cargo stderr plus `ok` on success,
or a 500 with the cargo/install error on failure. Typical build time is ~15 s on a
warm cache; the connection timeout is 180 s to cover cold builds.

**Response:** Plain text — last 12 lines of `cargo build` stderr, then `ok`.

500 body on failure: `cargo build failed: <tail>` or `install failed: <detail>`.

#### `POST /dev/deploy[?ref=<ref>]`

Fetches the latest refs from `origin` (`git fetch origin --prune`), checks out the
given ref (`git checkout -f <ref>`), and — when a corresponding `origin/<ref>` tracking
branch exists — resets hard to it (`git reset --hard origin/<ref>`). Returns the short
SHA of the resulting HEAD.

| Query param | Default | Description |
|-------------|---------|-------------|
| `ref` | `main` | Branch name, tag, or commit SHA to check out. |

**Response** (200):

```
deployed main @ a1b2c3d
```

500 on any git failure with the failed command's output in the body.

#### `POST /dev/restart-daemon`

Triggers a **self re-exec** of the daemon: the response `ok, re-execing` is written
and flushed before the re-exec, then `execv` replaces the current process image with
the installed binary (same PID). The daemon re-grabs input pads and the HTTP bridge
rebinds to the same address within ~3 s. Use this after `/dev/build` to hot-swap the
running binary without a full process restart.

**Response** (200, sent before re-exec):

```
ok, re-execing
```

The TCP connection closes immediately after the response; the bridge is briefly
unreachable during the ~3 s re-exec window.

#### HTTP status codes for `/dev/*` routes

The standard HTTP status codes from the [HTTP status mapping](#http-status-mapping)
table apply. Additional codes specific to dev routes:

| Status | Condition |
|--------|-----------|
| 200 | Operation succeeded |
| 405 | Correct `/dev/*` path but wrong HTTP method (e.g. `GET /dev/build`) |
| 404 | Unknown `/dev/*` path |
| 500 | Subprocess failed (`cargo build`, `git`, `install`, log open); body contains the error detail |

#### curl examples

```bash
# Check daemon status
curl -H "Authorization: Bearer mysecret" http://192.168.1.50:8731/dev/status

# Tail the last 50 lines of the quickshell log, filtered to errors
curl -H "Authorization: Bearer mysecret"      "http://192.168.1.50:8731/dev/logs?lines=50&filter=error"

# Restart quickshell and see initial WARN/ERROR output
curl -X POST -H "Authorization: Bearer mysecret"      http://192.168.1.50:8731/dev/restart-shell

# Build the daemon (~15 s)
curl -X POST -H "Authorization: Bearer mysecret"      http://192.168.1.50:8731/dev/build

# Deploy the main branch
curl -X POST -H "Authorization: Bearer mysecret"      http://192.168.1.50:8731/dev/deploy

# Deploy a specific branch
curl -X POST -H "Authorization: Bearer mysecret"      "http://192.168.1.50:8731/dev/deploy?ref=feat/my-branch"

# Hot-swap the binary after a build (daemon re-execs; bridge back in ~3 s)
curl -X POST -H "Authorization: Bearer mysecret"      http://192.168.1.50:8731/dev/restart-daemon
```

### Unrecognized Commands

Any command not listed above receives:

**Response:** `unknown\n`

### Input runtime down

Commands served by the **input runtime** (`grab`, `release`, `handoff`, `status`,
`get-bindings`, `set-binding`, `capture-next`, `capture-cancel`, `get-pads`,
`list-input-devices`, `intent`, `rumble`, `key`, `set-active-game`,
`overlay-focus`, `pad-battery`, `pad-rumble-status`, `controllerdb-refresh`)
round-trip a dedicated control channel and await the runtime's reply. The input
runtime is **supervised**: on a panic it is respawned in-process a few times, and
if it exhausts its retry budget it stays down while the rest of the daemon keeps
running (so `/metrics` and the Phase 3/4 D-Bus surfaces still answer).

While the input runtime is down (control channel send fails or the reply is
dropped), those commands receive:

**Response:** `error:input-runtime-down\n`

This is a distinct, actionable reply — a dead backend is **not** reported as
`unknown` (which is reserved for genuinely unrecognized commands / client typos).
Watch `tv_shell_input_runtime_up` (0 = down) and
`tv_shell_input_runtime_restarts_total` in the metrics for the same signal.

## Daemon-to-Subscriber Events

Subscribers (registered via `subscribe`) receive these events as newline-terminated strings.

### Controller Lifecycle

| Event | Trigger |
|-------|---------|
| `controller-wake` | A gamepad was discovered and grabbed on (re)connect (fires per joining pad) |
| `controller-disconnected` | A gamepad stream errored during event read (USB disconnect; fires per leaving pad) |
| `pad:connected:<json>` | A pad joined the fleet and was assigned a player slot. Payload: compact `{id,index,name}` object (#101) |
| `pad:disconnected:<id>` | A pad left the fleet; its slot is freed for reuse. Payload: the pad's stable wire `id` |
| `pad:index:<json>` | A pad's player-indicator LED was lit to match its slot at assignment (#101 LED). Payload: compact `{id,index}` object. Emitted for pads with a controllable LED — via `EV_LED`, or via the sysfs `/sys/class/leds` fallback (xpad/Sony driver families); a no-op for pads with neither |
| `pad:battery:<json>` | A pad's battery level/charging state changed (#100). Payload: compact `{id,level,charging}` object (`level` 0–100, `charging` bool). Only emitted for pads that report a battery (wireless); wired pads emit none |

`controller-wake` / `controller-disconnected` are the legacy single-pad signals
and still fire for every join/leave (so existing QML wake handling is unchanged).
The `pad:*` events carry the fleet-aware per-pad detail (stable id + player
index). Example: `pad:connected:{"id":"uniq:e4:17:...","index":0,"name":"Xbox Wireless Controller"}\n`.

**Fleet outputs (ride-along, #99/#100/#101).** On top of join/leave, the daemon
drives three per-pad *outputs*, each cap-gated and degrading to a clean no-op
when the pad lacks the hardware:

- **LED (#101):** at slot assignment the daemon lights the pad's player LED and
  emits `pad:index:{id,index}`. It tries `EV_LED` (LED code == player slot)
  first, then falls back to the `/sys/class/leds` sysfs tree. The sysfs fallback
  correlates the correct node per physical pad (longest shared canonical-path
  prefix with the pad's own sysfs device path; ties broken by sorted name) so
  two identical pads each light their own ring with no cross-talk. Two driver
  conventions are supported: xpad (Xbox 360/One) pads write `6 + slot` to the
  node's `brightness` attribute; Sony DualSense/DualShock4 pads drive either
  `*:white:player-N` per-slot LED-class entries (write `1` to the matching
  `player-<slot+1>` sibling, `0` to the others) or an `*:rgb:indicator`
  lightbar node (per-slot solid colour via `multi_intensity`). No LED command
  is exposed over IPC — the indicator is driven internally on pad join and the
  result is published as `pad:index:*`. Pads with neither EV_LED nor a usable
  sysfs leds node are a clean no-op: no event is emitted. The Sony lightbar
  path requires on-device verification on the deploy host with a DualSense.
- **Battery (#100):** the daemon polls the pad's `power_supply` sysfs and emits
  `pad:battery:{id,level,charging}` on change (and once at connect). Wired pads
  report no battery → no event.
- **Rumble (#99):** a short haptic pulse on connect, plus the `rumble <id> <ms>`
  command. Gated by `FF_RUMBLE` support and the `rumbleEnabled` setting; there is
  no rumble *event* (it's an output, not a notification).

Example wire lines:

```
pad:index:{"id":"uniq:e4:17:...","index":0}
pad:battery:{"id":"uniq:e4:17:...","level":80,"charging":false}
```

### Home Button (Meta / Guide) — tap vs hold

The gamepad Home/Guide button (`BTN_MODE`) is **intercepted** by the daemon in
every grabbed presenter (Shell/Keyboard/Game) and **never forwarded live** while
the daemon discriminates tap vs hold — so no partial Meta press leaks to a focused
app (no media-key / Guide leak window). The threshold is the `[input].meta_hold_ms`
config knob (**default 500 ms**, was a hard-coded 2 s).

- **TAP** — `BTN_MODE` released before `meta_hold_ms`. TAP belongs to the *app*:
  - **Shell** (home screen) → publishes `intent:home-tap` (opens the drawer; there
    is no app to forward to).
  - **Game** → the buffered Guide is delivered to the app as a real tap: `BTN_MODE`
    press+release is forwarded to the virtual pad (the game / remote Steam sees a
    Guide tap).
  - **Keyboard** (a keyboard-contract app like Plex) → delivers **nothing** (a
    keyboard-contract app has no Guide concept; the escape here is the HOLD).
- **HOLD** — `BTN_MODE` held past `meta_hold_ms`. HOLD belongs to *us* (the
  reserved shell escape) and is fully swallowed from the app:
  - **Shell** → publishes `intent:home-hold` (the idle reset-to-clean-home).
  - **Keyboard / Game** → publishes **`intent:home-tap`**, which while an app is
    running toggles the shell's *controllable overlay drawer* + overlay-focus — a
    non-destructive everyday escape that works regardless of who holds compositor
    toplevel focus. (The heavier `home-hold` full return-to-home is only the Shell
    home's hold.)

| Event | Trigger |
|-------|---------|
| `intent:home-tap` | `BTN_MODE` released before `meta_hold_ms` on the Shell home; **or** `BTN_MODE` held past `meta_hold_ms` in an app presenter (Keyboard/Game/unpinned Handoff) — the app-escape |
| `intent:home-hold` | `BTN_MODE` held past `meta_hold_ms` on the Shell home only — the idle reset |

The fleet-level dedup latch still applies to the hold-fire, so two pads holding
Meta at once publish the escape intent once. **Net semantic change:** the Meta TAP
now belongs to the app (Game gets a real Guide tap; it used to always open our
drawer), and the HOLD is the reliable shell escape. The legacy `home-press` /
`combo:home-hold` events were removed in Phase 5 — QML consumes only the `intent:*`
vocabulary (`onIntentHomeTap` / `onIntentHomeHold`).

**Unpinned Handoff caveat.** A contract-driven (unpinned) Handoff leaves the pad
ungrabbed, so the daemon cannot swallow Meta — the app reads the raw press for up
to `meta_hold_ms`. The daemon still detects a HOLD and publishes `intent:home-tap`
(the escape), best-effort. A **pinned** Handoff (the Moonlight `handoff` IPC) is
untouched: Home flows straight through so remote Steam sees Guide.

### Combo safety: buffered participants

When a focused app owns the screen (**Keyboard** or **Game** presenter) the daemon
buffers safety-combo *participant* buttons (`{Back, Home, LB, RB, Start, B}`)
instead of forwarding them, so a **partial** combo chord never leaks into the app
as a stray media key (the accidental-playback bug in Plex). A buffered sequence is
**swallowed** if a combo completes, or **replayed** to the app (in order) if
disqualified — a non-participant press, a participant released without a combo, or
the settle window (`[input].combo_guard_ms`, **default 120 ms**) elapsing.
Per-presenter arming is asymmetric by design: **Keyboard** buffers from the *first*
participant (a media app is not latency-critical; fully prevents leaks), **Game**
buffers only once a *second* participant is simultaneously held (single-button
gameplay stays latency-free; the first participant of a pair may forward before
arming — a game tolerates one benign leaked button). The combo *events* themselves
(below) are unaffected — they always fire off the physically-held buttons; only
the app-forwarding of participants is gated.

### Intents

Every accepted [`intent <name>`](#intent-name) command re-broadcasts here as an
`intent:<name>` event. This is the global control-surface stream: keyboard
global-escape, automation, and the daemon's own gamepad logic all surface through
it, so QML consumes **one** vocabulary regardless of source.

| Event | Payload (`<name>`) |
|-------|--------------------|
| `intent:<name>` | A coarse intent (`home`, `home-tap`, `home-hold`, `menu`, `settings`, `power`) OR a deep-link target (`settings:<page>`, `overlay:volume`, `overlay:network`, `app:<id>`) |

`intent:home` is the global return-to-shell escape; `intent:home-tap` /
`intent:home-hold` are the neutral gamepad Home signals (QML maps them by the
focus it owns). `intent:menu` toggles the navigation drawer. The daemon's own
gamepad Home handling publishes `intent:home-tap` / `intent:home-hold` directly,
so QML has exactly one shell-intent vocabulary regardless of source.

Deep-link targets are wire-compatible with the existing event: a
`intent settings:bluetooth` command broadcasts `intent:settings:bluetooth` —
the payload after the first `intent:` prefix is the full name including
any namespace colon.

### Combo Events

| Event | Trigger | Details |
|-------|---------|---------|
| `combo:end-session` | `BTN_MODE` + `BTN_EAST` (Home+B) held for 3 seconds | Timed combo |
| `combo:force-quit` | `BTN_SELECT` + `BTN_MODE` + `BTN_TL` + `BTN_TR` (Back+Home+LB+RB) all held | Instant, no hold timer. In the **game presenter** (an app/stream owns the screen) also emits `Ctrl+Alt+Shift+Q` via uinput to quit Moonlight; the shell presenter has no app to quit |
| `combo:suspend-stream` | `BTN_START` + `BTN_TL` + `BTN_TR` (Start+LB+RB) all held, and **not** the force-quit combo | Instant, no hold timer. Gamepad-only safety combo; QML routes it to a stream suspend |

All three combos are **gamepad-button** based (matched against the fleet's held
buttons), never keyboard chords — they survived the Phase 2 keyboard-snoop
deletion unchanged. Any pad in the fleet can fire them (shared/deduped at the
fleet level).

### Input Mode

| Event | Trigger |
|-------|---------|
| `input-mode:controller` | D-pad, left stick outside deadzone, or face/start/select/home button press |
| `input-mode:mouse` | Right stick outside deadzone, or LB/RB press |

Mode events are only sent on transitions (not re-sent if already in that mode).

### Debug Display

| Event | Format |
|-------|--------|
| `buttons:<held>` | Space-and-plus-separated list of currently held controller inputs |

`buttons:` is sent on every controller button down/up, trigger threshold crossing, and stick axis change. The `<held>` portion is empty when nothing is held (i.e., `buttons:\n`). Includes button friendly names, D-pad directions, stick directions, and trigger state.

> **Keyboard keys are NOT a daemon event.** The daemon no longer snoops the
> keyboard (Phase 2 — the keyboard belongs to the compositor + QML). The shell's
> debug overlay reads held keyboard keys directly from Wayland `Keys` in QML;
> there is no `keys:` event and no `kbd-log` command.

Example: `buttons:Home + B + LT + L→ + R↑\n`

Button display names used in the `buttons:` event:

| Code | Display |
|------|---------|
| `BTN_SOUTH` | A |
| `BTN_EAST` | B |
| `BTN_NORTH` | Y |
| `BTN_WEST` | X |
| `BTN_TL` | LB |
| `BTN_TR` | RB |
| `BTN_TL2` | LT |
| `BTN_TR2` | RT |
| `BTN_SELECT` | Back |
| `BTN_START` | Start |
| `BTN_MODE` | Home |
| `BTN_THUMBL` | L3 |
| `BTN_THUMBR` | R3 |

Stick and D-pad directions: `L↑`, `L↓`, `L←`, `L→`, `R↑`, `R↓`, `R←`, `R→`, `D-Up`, `D-Down`, `D-Left`, `D-Right`. Triggers show as `LT`/`RT` when the analog value exceeds 100 (digital threshold).

### Phase 3 Events

Streamed to `subscribe` clients by the D-Bus backbone (Linux-only; never emitted
on a non-Linux build). Each follows the bare-text `name:payload` convention; the
three `<json>` payloads are compact single-line JSON sharing the shape of their
corresponding query reply.

| Event | Trigger | Payload |
|-------|---------|---------|
| `bt:powered:on` / `bt:powered:off` | Adapter power state changed | `on` / `off` |
| `bt:device:<json>` | A device was discovered or updated during scan | Compact JSON object, same shape as a `bt-list` element (`{mac,name,paired,connected,trusted,rssi}`) |
| `bt:device-removed:<mac>` | A device dropped out of discovery | The device MAC (e.g. `bt:device-removed:AA:BB:CC:DD:EE:FF`) |
| `bt:scanning:on` / `bt:scanning:off` | Discovery (scan) started / stopped | `on` / `off` |
| `net:connectivity:<state>` | NetworkManager connectivity changed | One of `none` / `portal` / `limited` / `full` / `unknown` |
| `net:wifi:<json>` | Wi-Fi / primary state changed | Compact JSON object, same shape as a `net-status` body |
| `net:primary:<id>` | Primary connection changed | Its id/name (may be empty: `net:primary:`) |
| `power:battery:<json>` | Battery state changed | Compact JSON object, same shape as a `power-battery` body. Only emitted when a real battery is present — a desktop with no battery emits none |

Example wire lines:

```
bt:powered:on
bt:device:{"mac":"AA:BB:CC:DD:EE:FF","name":"Xbox Wireless Controller","paired":true,"connected":false,"trusted":true,"rssi":-60}
bt:device-removed:AA:BB:CC:DD:EE:FF
bt:scanning:off
net:connectivity:full
net:wifi:{"connectivity":"full","primaryType":"802-11-wireless","hasWifi":true,"ipv4":"wlan0: 192.168.1.50","activeConnections":[]}
net:primary:Wired connection 1
power:battery:{"present":true,"percentage":74,"state":"discharging","onBattery":true,"icon":"battery-good-symbolic"}
```

### Phase 4 Events

Streamed to `subscribe` clients by the Hyprland actor (Linux-only; never emitted
on a non-Linux build). Each follows the bare-text `name:payload` convention.
`AppLifecycleManager.qml` watches these to track window open/close/focus and
fullscreen transitions.

| Event | Trigger | Payload |
|-------|---------|---------|
| `hypr:activewindow:<class>` | The active window changed | The new active window's class. An empty class is allowed (e.g. `hypr:activewindow:` when no window is focused) |
| `hypr:fullscreen:<0|1>` | The active window's fullscreen state changed | `1` when fullscreen, `0` otherwise |
| `hypr:openwindow:<json>` | A new window was mapped (Hyprland `openwindow` event). Payload is a compact JSON object `{"address":"0x..","class":"..","title":"..","workspace":".."}`. Titles may contain commas — JSON encoding avoids comma-splitting issues. Consumed by `AppLifecycleManager.qml` for deterministic launch confirmation (#203) | Compact JSON object |
| `hypr:closewindow:<address>` | A window was closed (Hyprland `closewindow` event). Payload is the window's Hyprland address. Consumed by `AppLifecycleManager.qml` for immediate `appClosed` detection (#203) | Window address string (e.g. `0x55a1b2c3d4e5`) |

`hypr:openwindow` and `hypr:closewindow` supplement the existing `hypr:activewindow`
event and the `windowPoller` in `AppLifecycleManager`. The poller remains the source of
truth for `runningWindows` and app-closed detection; the new events enable deterministic,
immediate response when a launched window actually maps, without waiting for the next
2-second poll tick.

`hypr:openwindow` payload fields:

| Field | Type | Notes |
|-------|------|-------|
| `address` | string | Hyprland window address (e.g. `0x55a1b2c3d4e5`) |
| `class` | string | Window class |
| `title` | string | Window title (may contain commas — safe because JSON-encoded) |
| `workspace` | string | Workspace name where the window was mapped |

Example wire lines:

```
hypr:activewindow:firefox
hypr:activewindow:
hypr:fullscreen:1
hypr:fullscreen:0
hypr:openwindow:{"address":"0x55a1b2c3d4e5","class":"firefox","title":"Mozilla Firefox","workspace":"1"}
hypr:closewindow:0x55a1b2c3d4e5
```

### HDMI-CEC Events (#94, #16)

Streamed to `subscribe` clients by the CEC actor. **Feature-gated and
Linux-only** (`all(target_os = "linux", feature = "cec")`) — never emitted on a
default build, a non-Linux build, or when libcec is absent. Each follows the
bare-text `name:payload` convention; the payload is a compact single-line JSON
object. `AVControlSettings.qml` merges these into its device list so rows update
live without re-polling.

| Event | Trigger | Payload |
|-------|---------|---------|
| `cec:device:<json>` | A CEC device was discovered or updated (emitted per device after a `cec-scan` and after a `cec-device`) | `{"logicalAddress":N,"powerStatus":"<word>"}` (same shape as a `cec-scan` element) |
| `cec:power:<json>` | A CEC device's power status changed (emitted after `cec-power-on` / `cec-power-off`) | `{"addr":"N","power":"<word>"}` — `addr` is the wire string the command received; `<word>` is `on`/`standby`/`waking`/`sleeping`/`unknown` |
| `cec:health:<json>` (#19) | The CEC transmit-wedge health state CHANGED (broadcast only on a real transition — from a transmit op, the `cec-scan` side-effect refresh, or `cec-test` — not on every probe), **plus** once when the libcec open handshake fails (carrying `transmit:"unavailable"` + the `reason`) | `{"transmit":"ok"\|"failing"\|"unknown"\|"unavailable","reason":<null\|"no_libcec"\|"no_adapter"\|"adapter_open_failed">,"since":N,"lastError":"…"\|null}` (same shape as the `cec-health` reply; `reason` is `null` while the adapter is open) |

Example wire lines:

```
cec:device:{"logicalAddress":0,"powerStatus":"on"}
cec:power:{"addr":"5","power":"on"}
cec:health:{"transmit":"failing","reason":null,"since":1719500000000,"lastError":"active-source failed: TransmitFailed"}
cec:health:{"transmit":"unavailable","reason":"adapter_open_failed","since":1719500000000,"lastError":null}
```

### Config Live-Reload

Emitted by the file-watch actor (Linux-only, inotify-based) when
`settings.json` is modified by an **external** writer (SSH, Ansible, a web UI,
or any tool other than the daemon itself). The daemon suppresses its own
`set-config` and `set-binding` writes via a self-write generation guard
(`config::note_self_write()` + `config::self_write_gen()`), so this event fires
only for foreign edits.

Carries **no payload** — subscribers re-fetch the full settings document via the
existing `get-config` command (one re-read path, no inline JSON concerns).

| Event | Trigger | Payload |
|-------|---------|---------|
| `config:changed` | `settings.json` was modified by an external writer | _(none)_ |
| `health:<json>` | A remote service's reachability changed (or its initial state at poll startup) | `{"service":<name>,"status":<status>}` |

Example wire lines:

```
config:changed
health:{"service":"plex","status":"unreachable"}
```

See [Service health](#service-health) for the `status` vocabulary and the poller
that emits `health:*`.

> **QML wiring**: `SettingsStore.qml` subscribes to this event via its
> `configWatch` `SocketClient` (subscribe mode). On receipt it calls
> `store.load()` (the same `get-config` path used at startup), re-applying all
> QML-owned keys (`themeMode`, `streamingViewMode`, `controllerDebug`,
> `rumbleEnabled`, `reduceMotion`, `textScale`) and `keyBindings` live.

## Service health

A shared vocabulary for "is this remote service reachable?", used by both the
per-request replies (`plex-hubs`, `sunshine-status`) and the `health:<json>`
broadcast events. It exists so widgets can tell a **down server** apart from
**no data** — a failed fetch is no longer indistinguishable from an empty result.

| `status` | Meaning | Widget UX |
|----------|---------|-----------|
| `disabled` | Not configured (no URL/token) | Collapse silently |
| `ok` | Reachable and serving | Show data |
| `unreachable` | Configured but not answering — transport error, timeout, or 5xx (e.g. a reverse proxy's `503 no available server` when the backend is down) | Show a "server unavailable" notice |
| `error` | Reached but rejected — auth failure / 4xx | Show a "check configuration" notice |

Mapping (daemon `service_health::classify_code`): `2xx`/`3xx` → `ok`; `5xx` →
`unreachable`; transport failure → `unreachable`; other `4xx` → `error`.

The daemon's **health poller** (`service_health::run`) probes always-on services
(currently Plex, via a lightweight `GET /identity`) every 60s and broadcasts a
`health:<json>` event **on change**. Per-instance services (Sunshine, per stream
card) are polled on demand and carry the same `status` in their reply rather than
through the poller. On the QML side the `ServiceMonitor` lib component consumes
either source and exposes `status`/`ok`/`degraded`/`disabled`; `ServiceStatusNotice`
renders the degraded states. Because the poller emits on change, a late subscriber
can miss the current state — so `ServiceMonitor` also re-polls the per-request
command each interval as a self-healing floor, with the broadcast as the fast path.

## Default Button Mappings

| Action | Default Button | Keyboard Output | Remappable |
|--------|---------------|-----------------|------------|
| `select` | `BTN_SOUTH` (A) | `KEY_ENTER` | Yes |
| `back` | `BTN_EAST` (B) | `KEY_ESC` | Yes |
| `altSelect` | `BTN_NORTH` (Y) | `KEY_TAB` | Yes |
| `menu` | `BTN_START` (Start) | `KEY_TAB` | Yes |

`menu` emits the same `KEY_TAB` as `altSelect` — that is the nav drawer's
existing keyboard gesture (`ShellLayout.qml`'s `Keys.onTabPressed`), so Start
toggles the drawer open/closed as a dedicated menu button rather than
duplicating `select`'s `KEY_ENTER`.

`BTN_MODE` (Home/Guide) is **not** a mapped action. It is handled directly via the
[tap-vs-hold split](#home-button-meta--guide--tap-vs-hold) — a tap belongs to the
app (Shell drawer / Game Guide replay), a hold is the reserved shell escape —
never mapped to a key (mapping it would leak `KEY_HOMEPAGE` to whatever app has
keyboard focus). It is still a valid *target* for `set-binding` (it is in the
remappable set), but no action defaults to it.

### Remappable Buttons

Any of these evdev buttons can be assigned to an action via `set-binding`:

`BTN_SOUTH`, `BTN_EAST`, `BTN_NORTH`, `BTN_WEST`, `BTN_TL`, `BTN_TR`, `BTN_SELECT`, `BTN_START`, `BTN_MODE`, `BTN_THUMBL`, `BTN_THUMBR`

## Analog Stick Behavior

### Left Stick (Navigation)

Converted to arrow key presses with deadzone filtering and key repeat.

| Parameter | Value |
|-----------|-------|
| Deadzone | 30% of half-range from center |
| Initial repeat delay | 300 ms |
| Repeat interval | 150 ms |

The center and threshold are calibrated from the device's `absinfo` on connect, not hardcoded. Each axis (`ABS_X`, `ABS_Y`) is handled independently. Crossing a threshold emits a key-down; returning inside the deadzone emits a key-up and cancels repeat.

### Right Stick (Mouse Cursor)

Emits relative mouse movement via a virtual mouse device at ~60 Hz.

| Parameter | Value |
|-----------|-------|
| Poll rate | 16 ms (~60 Hz) |
| Speed range | 2 to 25 pixels/poll |
| Velocity curve | Quadratic (`mag^2`) |

Velocity formula: `speed = 2 + 23 * (normalized_deflection ^ 2)`, where `normalized_deflection` is `(abs(offset) - threshold) / (half_range - threshold)` clamped to `[0, 1]`.

LB and RB emit mouse left-click and right-click respectively (both in grabbed and ungrabbed modes).

### Triggers (Digital Threshold)

Left and right triggers (`ABS_Z`, `ABS_RZ`) are treated as digital: held when analog value > 100, released otherwise. They appear in the `buttons:` debug event as `LT`/`RT` but do not emit any keyboard or mouse events.

## Presenters: Shell vs Keyboard vs Game vs Handoff

The daemon keeps the physical `EVIOCGRAB` in the **Shell**, **Keyboard**, and
**Game** presenters — no controller input leaks to the compositor in those modes.
The **Handoff** presenter (#221) is the exception: it **releases** the grab so
SDL/Moonlight reads the real evdev node directly. `grab` selects the shell
presenter, `release` the game presenter, and `handoff` the handoff presenter
explicitly; the **Keyboard** presenter is selected only by follow-focus (a
focused app whose input contract is `keyboard`, see below). They differ in where
the pad's input goes and whether the physical grab is held.

| Feature | Shell presenter (`grab`) | Keyboard presenter (contract) | Game presenter (`release`) | Handoff presenter (`handoff`) |
|---------|--------------------------|-------------------------------|----------------------------|-------------------------------|
| Physical pad grabbed | Yes | Yes | Yes | **No** (ungrabbed → SDL reads it directly) |
| Per-player clean virtual gamepad | — | **—** (no virtual pad) | One per pad (`tv-shell-virtual-pad-<slot>`) | — (game reads the real node) |
| Button-to-keyboard mapping | Active | Active (lands on the focused app) | — (raw buttons forwarded to the virtual pad) | — |
| D-pad / left stick → arrow keys | Active | Active | — (forwarded as raw axes) | — |
| Right stick → mouse cursor | Active | Active | — (forwarded as raw axis) | — |
| LB/RB → mouse clicks | Active | Active | — (forwarded as raw buttons) | — |
| Combo detection (end-session, force-quit, suspend) | Active | Active | Active | Active |
| Meta/Home tap vs hold (see [Home Button](#home-button-meta--guide--tap-vs-hold)) | tap→`intent:home-tap`, hold→`intent:home-hold` (intercepted, never leaked) | tap→nothing, hold→`intent:home-tap` (intercepted, never leaked) | tap→Guide replayed to vpad, hold→`intent:home-tap` (never leaked live) | **Unpinned**: hold→`intent:home-tap` best-effort (app sees raw press ≤ threshold). **Pinned**: forwarded — remote Steam sees Guide |
| `buttons:` debug events | Active | Active | — | — |
| Input mode tracking | Active | Active | — | — |
| Capture mode | Active | Active | — | — |
| Force-quit `Ctrl+Alt+Shift+Q` emission | No (not needed) | No (local app) | Yes | Yes |

The **Keyboard** presenter is mechanically the Shell presenter — the same
button→keyboard/mouse mapping on the shared virtual devices — but the focused app
holds Wayland focus, so the emitted keys land on *it*, not the shell. It holds
**no** virtual pad, which is the point: a key-driven HTPC app like Plex becomes
d-pad drivable, and — the bug this fixes — Steam has no always-alive virtual pad
to take an exclusive `EVIOCGRAB` on out from under the focused app. `status`
reports it as `grabbed` (like Shell: the daemon owns and translates the pad, it is
not "released" to a game).

On entering the shell/keyboard presenter: each pad's clean virtual gamepad is torn
down; held keys/triggers reset and combos cancel on the underlying grab.
On entering the game presenter: each pad gets a clean virtual gamepad mirroring
its capabilities (keys, axes with calibration, `input_id`). The game reads the
virtual pad; the daemon still intercepts Home so the shell can come up over a
running game.
On entering the handoff presenter: each pad's virtual gamepad is torn down and the
physical `EVIOCGRAB` is released, so SDL/Moonlight reads the real evdev node. The
**Home/Guide trade-off**: because the grab is dropped and Home is not intercepted,
remote Steam (Big Picture) sees the Guide button — but a local Home-tap can no
longer raise the shell overlay over the stream. The gamepad force-quit combo
(Back+Home+LB+RB) and the keyboard remain the escape hatches.

### Follow-focus: presenter tracks the focused window's input contract

`grab`/`release`/`handoff` are the *explicit* presenter switches the shell
requests. On top of those, the Hyprland actor's `activewindow` event watcher
drives the presenter *continuously*, mirroring the kiosk fullscreen enforcement
pattern (`hyprland.rs`'s `enforce_active_fullscreen`). The focused window's
**input contract** *is* the presenter it selects:

- **empty class** (no toplevel focused — the shell's own layer-shell surface
  never appears in `activewindow`) → **Shell**.
- **`gamepad` contract** → **Game** (a clean per-player virtual pad). This is the
  built-in default for any unlisted class, so a controller keeps working as a real
  gamepad in an app launched **out-of-band** of the shell's own flow (e.g. a Steam
  Remote Play `streaming_client` window opened from Big Picture).
- **`keyboard` contract** → **Keyboard** (d-pad drives the focused app, no virtual
  pad). The built-in default for `tv.plex.Plex`.
- **`handoff` contract** → **Handoff** (ungrab so the app reads the raw node).

Contracts are keyed by window class and configured under `[input.contracts]` in
`config.toml` (window-class → `gamepad`|`keyboard`|`handoff`); user entries
override the built-in defaults (`tv.plex.Plex` = keyboard, `steam` = gamepad,
everything else = gamepad). Read once at startup — a change needs a daemon
restart. See `config/config.toml.example`.

**Handoff pinning.** A Moonlight stream is entered via the explicit `handoff` IPC
and is **pinned**: follow-focus never overrides it while the streamed window holds
focus for the whole stream, and only the shell's explicit `grab` (on stream exit)
ends it. A Handoff reached instead via a `handoff` *contract* is **not** pinned —
it follows focus like any other presenter, so it can never strand the pads
ungrabbed.

**Settle debounce.** Follow-focus is debounced by a short settle timer
(`FOCUS_SETTLE_MS`, 300 ms): a focus change arms the timer and the transition
applies only once focus has held that long. Hyprland churns focus during an app
launch/close (empty↔toplevel flaps), and applying each flap would tear down and
rebuild the virtual pad repeatedly — which Steam re-enumerates as a controller
reconnect on every flap (audible/visible, worse than the original bug). The
debounce collapses a flap burst into a single net transition. Only follow-focus is
debounced; explicit `grab`/`release`/`handoff`/`overlay-focus` IPC applies
instantly.

Internally, `activewindow` changes reach the input runtime over a **coalescing
`tokio::sync::watch` channel** (latest-wins), not the control channel — focus is
*state*, so only the newest class matters and a momentarily-busy input loop can
never drop or back up a focus change (it retains just the latest).

## Settings Persistence

Key bindings are persisted to `~/.config/tv-shell/settings.json` under the `keyBindings` key. The daemon reads on startup and writes on each `set-binding` command (read-modify-write, compact JSON).

```json
{"keyBindings":{"select":"BTN_SOUTH","back":"BTN_EAST","altSelect":"BTN_NORTH","menu":"BTN_START"}}
```

Values are evdev code names (e.g., `BTN_SOUTH`). On load, if a value is an array, the last element is used. Unknown actions and non-remappable buttons are silently skipped.

### Per-player and per-game override layers (#104)

Two optional keys in `settings.json` provide additive override layers on top of
the global `keyBindings`. Absent keys mean today's behavior — no migration needed.

**`perPlayerBindings`** — object keyed by player slot (`"0"`, `"1"`, `"2"`, `"3"`).
Each value is an `{action: button_name}` object (same shape as `keyBindings`).
Overrides the global binding for that slot only.

```json
{
  "perPlayerBindings": {
    "0": {"select": "BTN_NORTH"},
    "1": {"back": "BTN_WEST"}
  }
}
```

**`perGameBindings`** — object keyed by arbitrary game-id strings (non-empty).
Each value is an `{action: button_name}` object.
The active game id is set at runtime via `set-active-game <id>`.

```json
{
  "perGameBindings": {
    "steam_12345": {"select": "BTN_SOUTH", "menu": "BTN_EAST"}
  }
}
```

**Resolution order** (first matching layer wins for each action):
1. **Game override** — `perGameBindings[active_game]` (if a game is active)
2. **Player override** — `perPlayerBindings[slot]` (for that pad's slot)
3. **Global** — `keyBindings`
4. **Default** — built-in defaults (`select=BTN_SOUTH`, etc.)

These keys are daemon-owned (the daemon is the sole writer for `keyBindings`;
QML/external tools write `perPlayerBindings`/`perGameBindings`). The daemon
re-reads both keys live on every `set-config` change (or external file edit
that triggers `config:changed`). The `active_game` state is in-memory only
and is never written to `settings.json`.

## Virtual Input Devices

The daemon creates two uinput devices on startup:

### `tv-shell-virtual-kb`

Emits keyboard events for mapped buttons, arrow keys (D-pad and left stick), and the force-quit key combo.

Registered capabilities: all mapped keyboard codes (deduplicated, from button bindings) plus `KEY_UP`, `KEY_DOWN`, `KEY_LEFT`, `KEY_RIGHT`, `KEY_LEFTCTRL`, `KEY_LEFTALT`, `KEY_LEFTSHIFT`, `KEY_Q`.

### `tv-shell-virtual-mouse`

Emits relative mouse movement (right stick) and mouse button clicks (LB/RB).

Registered capabilities: `REL_X`, `REL_Y`, `REL_WHEEL`, `REL_HWHEEL`, `BTN_LEFT`, `BTN_RIGHT`, `BTN_MIDDLE`.

## Gamepad Discovery

The daemon scans `/dev/input/event*` for devices with `EV_KEY` capability containing
`BTN_SOUTH`. The Rust daemon selects an **arbitrary** known controller by computing the
SDL joystick GUID from the device's `input_id` and checking it against a bundled
`SDL_GameControllerDB` (extendable via `TV_SHELL_GAMECONTROLLERDB`); if no DB match is
found it falls back to the first `BTN_SOUTH` device. Setting **both** env overrides below
pins discovery to an exact vendor/product (legacy behavior).

| Parameter | Env Override | Notes |
|-----------|--------------|-------|
| Vendor ID | `GAMEPAD_VENDOR` | e.g. `0x045e` (Microsoft) |
| Product ID | `GAMEPAD_PRODUCT` | e.g. `0x028e` (Xbox 360 Controller) |

On disconnect (`OSError` during event read), the daemon sends `controller-disconnected` to subscribers, resets stick state, and retries discovery every 1 second. On initial startup with no gamepad present, it retries every 2 seconds.

## Known Issues

- **`BTN_MODE` is socket-only**: The Home/Guide button is not mapped to a keyboard key. It drives `intent:home-tap` / `intent:home-hold` subscriber events directly via the [tap-vs-hold split](#home-button-meta--guide--tap-vs-hold) (threshold = `[input].meta_hold_ms`, default 500 ms), never forwarded live to a focused app. Mapping it to `KEY_HOMEPAGE` was intentionally avoided because that keycode leaks to focused apps (browsers treat it as "go to home page"). `BTN_MODE` is still a valid `set-binding` *target*, but no action defaults to it.
