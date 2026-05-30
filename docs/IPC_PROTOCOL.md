# IPC Protocol Specification

The gamepad input daemon (`input/gamepad-input.py`) communicates with QML components over a Unix domain socket using a newline-delimited text protocol.

## Socket Connection

| Property | Value |
|----------|-------|
| Path | `/run/user/$UID/game-shell-input.sock` |
| Env override | `GAME_SHELL_SOCK` |
| Type | `AF_UNIX`, `SOCK_STREAM` |
| Framing | Newline-delimited (`\n`) UTF-8 text |
| Permissions | `0o600` (owner only) |

The daemon removes any existing socket file on startup and creates a new one. Clients connect, send one command per line, and read the response. The `subscribe` command is the exception — it holds the connection open and streams events.

Commands and responses are **bare newline-delimited text**. A few commands carry a compact single-line JSON *body* (as a request argument and/or response): `get-bindings`, `list-apps`, `get-config`, `set-config`, `record-launch`, `get-recents`, the Phase 3 query replies `bt-list`, `net-status`, `net-wifi-list`, and `power-battery`, and the Phase 4 query replies `hypr-active`, `hypr-clients`, and `sunshine-status`. JSON only ever appears as such a body — never as the framing itself.

## Client-to-Daemon Commands

### `grab`

Acquire exclusive gamepad access via `EVIOCGRAB`. Clears held-key state, resets triggers, cancels any pending combo timer, and sets input mode to `controller`.

**Response:** `ok\n`

### `release`

Release exclusive gamepad access. Resets all stick state (releases held stick keys, cancels repeat tasks), clears held-key state, resets triggers, and cancels any pending combo timer.

**Response:** `ok\n`

### `status`

Query current connection and grab state.

**Response:** `<connection>:<grab>\n`

| Field | Values |
|-------|--------|
| connection | `connected` or `disconnected` |
| grab | `grabbed` or `released` |

Example: `connected:grabbed\n`

### `subscribe`

Register as an event subscriber. The daemon sends `subscribed\n`, then streams events (one per line) for the lifetime of the connection. The connection stays open — the server reads until EOF, then removes the subscriber.

**Response:** `subscribed\n` followed by a stream of events (see [Daemon-to-Subscriber Events](#daemon-to-subscriber-events)).

### `get-bindings`

Return current button-to-action mappings as compact JSON.

**Response:** Single-line JSON object mapping action names to evdev button code names.

Example: `{"select":"BTN_SOUTH","back":"BTN_EAST","altSelect":"BTN_NORTH","confirm":"BTN_START"}\n`

### `set-binding <action> <button_name>`

Remap a button for the given action. Rebuilds the internal button map and persists to settings.

**Response:**

| Condition | Response |
|-----------|----------|
| Success | `ok\n` |
| Wrong number of args | `error:usage: set-binding <action> <button_name>\n` |
| Unknown action | `error:unknown action '<action>'\n` |
| Invalid or non-remappable button | `error:invalid button '<button_name>'\n` |

Valid actions: `select`, `back`, `altSelect`, `confirm`

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

### `get-config`

Return the full settings document (`~/.config/game-shell/settings.json`).
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

### `record-launch <json-object>`

Record an app launch into the recents file
(`~/.local/share/game-shell/recents.json`). The body is a compact single-line
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
{"connectivity":"full","primaryType":"802-3-ethernet","hasWifi":true,"ipv4":"eth0: 192.168.8.50","activeConnections":[{"name":"Wired connection 1","type":"802-3-ethernet","device":"eth0"}]}
```

| Field | Type | Notes |
|-------|------|-------|
| `connectivity` | string | `none` / `portal` / `limited` / `full` / `unknown` (NM connectivity code 1/2/3/4, else `unknown`) |
| `primaryType` | string | Connection type of NM's primary connection (`""` if none) |
| `hasWifi` | bool | True if any NM device is a Wi-Fi device (`DeviceType == 2`) |
| `ipv4` | string | Best-effort non-loopback IPv4 addresses as `"<iface>: <ip>"` lines (newline-joined, up to 3; `""` if none). Read via an `ip -4 -o addr` shell-out — explicitly allowed, since only `nmcli` *reads* must move to D-Bus |
| `activeConnections` | array | `{name, type, device}` objects; `device` is the first interface name |

If NetworkManager is unreachable, a best-effort object is returned with
`connectivity:"unknown"`, empty strings, `hasWifi:false`, and
`activeConnections:[]` (the command does not error). On a non-Linux build:
`error:unsupported on this platform\n`.

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

Battery state. game-client-1 is a desktop, so "no battery" is the normal case
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

### `kbd-log on` / `kbd-log off`

Toggle daemon-side logging of keyboard key events. When enabled, every initial keydown (auto-repeats excluded) emits a log line like:

```
game-shell-input: kbd-key code=125 raw=KEY_LEFTMETA display='Meta' source=mapped
```

`source` is one of:

| Value | Meaning |
|---|---|
| `mapped` | Friendly name came from the explicit `KEY_DISPLAY_NAMES` table |
| `fallback` | Raw kernel name found, display computed by stripping `KEY_` prefix and titlecasing |
| `unknown` | The evdev tables don't recognize this code at all |

This lets us build a history of pressed keys over time and find unmapped or mis-mapped keys to fold back into `KEY_DISPLAY_NAMES`. Off by default — the shell's debug overlay flips it on when it opens and off when it closes, so normal use doesn't accumulate keystroke history.

**Response:** `ok\n`

## Phase 4 Commands (Hyprland + Sunshine)

Phase 4 adds two subsystems: a Hyprland compositor actor (`hyprland` crate, async
event listener + data getters) and a Sunshine session detector (`reqwest` over
the host's self-signed HTTPS endpoint).

The **Hyprland** commands replace the `hyprctl clients -j` read in
`components/HyprctlClients.qml` and feed `components/AppLifecycleManager.qml`'s
window-event watching. They are **Linux-only** (the Hyprland IPC socket): on a
non-Linux build (or any host where the Hyprland actor failed to start), they
reply `error:unsupported on this platform\n`. One-shot compositor *actions*
(`hyprctl dispatch exec/closewindow/focuswindow/fullscreen`) stay shell-outs in
the QML.

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
{"class":"firefox","title":"Mozilla Firefox","address":"0x55a1b2c3d4e5"}
```

| Field | Type | Notes |
|-------|------|-------|
| `class` | string | Active window's class (empty string allowed) |
| `title` | string | Active window's title |
| `address` | string | Hyprland window address (e.g. `0x…`) |

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

### Sunshine session detection (`reqwest`)

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
{"online":true,"paired":true,"currentApp":"881448767","httpsPort":47990}
```

| Field | Type | Notes |
|-------|------|-------|
| `online` | bool | The host responded with a parseable `/serverinfo` document |
| `paired` | bool | `<PairStatus>1</PairStatus>` — this client is paired with the host |
| `currentApp` | string | Id of the app currently streaming, or `""` when idle. Busy only when `<state>` ends in `SERVER_BUSY` **and** `<currentgame>` is a non-zero id |
| `httpsPort` | number | `<HttpsPort>` from the document (`0` when absent/unparseable) |

A host that is unreachable / times out / returns a non-2xx or non-serverinfo
body degrades to the offline object (the command does not error):

```json
{"online":false,"paired":false,"currentApp":"","httpsPort":0}
```

| Condition | Response |
|-----------|----------|
| Success / host unreachable | The JSON object above |
| Missing or incomplete `<host> <port>` body | `error:usage: sunshine-status <host> <port>\n` |

The response *parser* is a pure, unit-tested function (parses Sunshine's
`/serverinfo` XML into the object above).

### Unrecognized Commands

Any command not listed above receives:

**Response:** `unknown\n`

## Daemon-to-Subscriber Events

Subscribers (registered via `subscribe`) receive these events as newline-terminated strings.

### Controller Lifecycle

| Event | Trigger |
|-------|---------|
| `controller-wake` | Gamepad discovered and grabbed on (re)connect |
| `controller-disconnected` | Gamepad `OSError` during event read (USB disconnect) |

### Home Button

| Event | Trigger |
|-------|---------|
| `home-press` | `BTN_MODE` released before the 2-second hold threshold |
| `combo:home-hold` | `BTN_MODE` held for 2 seconds |

### Combo Events

| Event | Trigger | Details |
|-------|---------|---------|
| `combo:end-session` | `BTN_MODE` + `BTN_EAST` (Home+B) held for 3 seconds | Timed combo |
| `combo:force-quit` | `BTN_SELECT` + `BTN_MODE` + `BTN_TL` + `BTN_TR` (Back+Home+LB+RB) all held | Instant, no hold timer. When ungrabbed, also emits `Ctrl+Alt+Shift+Q` via uinput to quit Moonlight |

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
| `keys:<held>` | Space-and-plus-separated list of currently held keyboard keys |

`buttons:` is sent on every controller button down/up, trigger threshold crossing, and stick axis change. The `<held>` portion is empty when nothing is held (i.e., `buttons:\n`). Includes button friendly names, D-pad directions, stick directions, and trigger state.

`keys:` is sent on every keyboard key down/up from any discovered evdev keyboard device. The daemon reads these devices without `EVIOCGRAB` — keys still reach focused windows normally; this is a read-only snoop for the debug overlay. Names use a friendly display map (e.g. `KEY_LEFTMETA` → `Meta`, `KEY_UP` → `↑`) with a fallback that strips the `KEY_` prefix and titlecases the rest. Empty held set emits `keys:\n`.

Example: `buttons:Home + B + LT + L→ + R↑\n`
Example: `keys:Ctrl + Shift + A\n`

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
net:wifi:{"connectivity":"full","primaryType":"802-11-wireless","hasWifi":true,"ipv4":"wlan0: 192.168.8.50","activeConnections":[]}
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

Example wire lines:

```
hypr:activewindow:firefox
hypr:activewindow:
hypr:fullscreen:1
hypr:fullscreen:0
```

## Default Button Mappings

| Action | Default Button | Keyboard Output | Remappable |
|--------|---------------|-----------------|------------|
| `select` | `BTN_SOUTH` (A) | `KEY_ENTER` | Yes |
| `back` | `BTN_EAST` (B) | `KEY_ESC` | Yes |
| `altSelect` | `BTN_NORTH` (Y) | `KEY_TAB` | Yes |
| `confirm` | `BTN_START` (Start) | `KEY_ENTER` | Yes |

`BTN_MODE` (Home) is **not** a mapped action. It is handled directly to broadcast
`home-press` (tap) / `combo:home-hold` (hold) on the socket — mapping it to a key
would leak `KEY_HOMEPAGE` to whatever app has keyboard focus. It is still a valid
*target* for `set-binding` (it is in the remappable set), but no action defaults to it.

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

## Grabbed vs Ungrabbed Behavior

| Feature | Grabbed | Ungrabbed |
|---------|---------|-----------|
| Button-to-keyboard mapping | Active | Inactive |
| D-pad → arrow keys | Active | Inactive |
| Left stick → arrow keys | Active | Inactive |
| Right stick → mouse cursor | Active | Active |
| LB/RB → mouse clicks | Active | Active |
| Combo detection (end-session, force-quit) | Active | Active |
| Home button hold detection | Active | Inactive |
| `home-press` event | Active | Inactive |
| `buttons:` debug events | Active | Active |
| Input mode tracking | Active | Active |
| Capture mode | Active | Inactive |
| Force-quit `Ctrl+Alt+Shift+Q` emission | No (not needed) | Yes |

On grab: clears held keys, resets triggers, cancels combos, sets input mode to `controller`.
On release: resets all stick state (releases held keys, cancels repeat tasks), clears held keys, resets triggers, cancels combos.

## Settings Persistence

Keybindings are persisted to `~/.config/game-shell/settings.json` under the `keyBindings` key. The daemon reads on startup and writes on each `set-binding` command (read-modify-write, compact JSON).

```json
{"keyBindings":{"select":"BTN_SOUTH","back":"BTN_EAST","altSelect":"BTN_NORTH","confirm":"BTN_START"}}
```

Values are evdev code names (e.g., `BTN_SOUTH`). On load, if a value is an array, the last element is used. Unknown actions and non-remappable buttons are silently skipped.

## Virtual Input Devices

The daemon creates two uinput devices on startup:

### `game-shell-virtual-kb`

Emits keyboard events for mapped buttons, arrow keys (D-pad and left stick), and the force-quit key combo.

Registered capabilities: all mapped keyboard codes (deduplicated, from button bindings) plus `KEY_UP`, `KEY_DOWN`, `KEY_LEFT`, `KEY_RIGHT`, `KEY_LEFTCTRL`, `KEY_LEFTALT`, `KEY_LEFTSHIFT`, `KEY_Q`.

### `game-shell-virtual-mouse`

Emits relative mouse movement (right stick) and mouse button clicks (LB/RB).

Registered capabilities: `REL_X`, `REL_Y`, `REL_WHEEL`, `REL_HWHEEL`, `BTN_LEFT`, `BTN_RIGHT`, `BTN_MIDDLE`.

## Gamepad Discovery

The daemon scans `/dev/input/event*` for devices with `EV_KEY` capability containing
`BTN_SOUTH`. The Rust daemon selects an **arbitrary** known controller by computing the
SDL joystick GUID from the device's `input_id` and checking it against a bundled
`SDL_GameControllerDB` (extendable via `GAME_SHELL_GAMECONTROLLERDB`); if no DB match is
found it falls back to the first `BTN_SOUTH` device. Setting **both** env overrides below
pins discovery to an exact vendor/product (legacy behavior).

| Parameter | Env Override | Notes |
|-----------|--------------|-------|
| Vendor ID | `GAMEPAD_VENDOR` | e.g. `0x045e` (Microsoft) |
| Product ID | `GAMEPAD_PRODUCT` | e.g. `0x028e` (Xbox 360 Controller) |

On disconnect (`OSError` during event read), the daemon sends `controller-disconnected` to subscribers, resets stick state, and retries discovery every 1 second. On initial startup with no gamepad present, it retries every 2 seconds.

## Known Issues

- **`BTN_MODE` is socket-only**: The Home button is not mapped to a keyboard key. It drives `home-press` (tap) and `combo:home-hold` (hold) subscriber events directly. Mapping it to `KEY_HOMEPAGE` was intentionally avoided because that keycode leaks to focused apps (browsers treat it as "go to home page"). `BTN_MODE` is still a valid `set-binding` *target*, but no action defaults to it.
