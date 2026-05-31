# IPC Protocol Specification

The input/backend daemon (`game-shell-input`, Rust source in `rust/`) communicates with QML components over a Unix domain socket using a newline-delimited text protocol.

## Socket Connection

| Property | Value |
|----------|-------|
| Path | `/run/user/$UID/game-shell-input.sock` |
| Env override | `GAME_SHELL_SOCK` |
| Type | `AF_UNIX`, `SOCK_STREAM` |
| Framing | Newline-delimited (`\n`) UTF-8 text |
| Permissions | `0o600` (owner only) |

The daemon removes any existing socket file on startup and creates a new one. Clients connect, send one command per line, and read the response. The `subscribe` command is the exception — it holds the connection open and streams events.

Commands and responses are **bare newline-delimited text**. A few commands carry a compact single-line JSON *body* (as a request argument and/or response): `get-bindings`, `get-pads`, `list-input-devices`, `list-apps`, `get-config`, `set-config`, `record-launch`, `get-recents`, the Phase 3 query replies `bt-list`, `net-status`, `net-wifi-list`, and `power-battery`, and the Phase 4 query replies `hypr-active`, `hypr-clients`, and `sunshine-status`. JSON only ever appears as such a body — never as the framing itself.

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

| Condition | Response |
|-----------|----------|
| `<name>` in the closed vocabulary | `ok\n` (and an `intent:<name>` event is broadcast) |
| `<name>` outside the vocabulary | `error:unknown intent '<name>'\n` (no event) |
| Missing/empty `<name>` body | `error:usage: intent <name>\n` |

**Example (automation):**

```
echo "intent home" | nc -U "$GAME_SHELL_SOCK"
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
echo "rumble uniq:e4:17:d8:01:02:03 200" | nc -U "$GAME_SHELL_SOCK"
```

> **`rumbleEnabled` setting.** A QML-owned boolean in `settings.json`
> (default `true`) gating all daemon-fired rumble — both the `rumble` command and
> the connect pulse. Read by the daemon via `get-config`; toggled like any other
> QML-owned key via `set-config`.

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
echo "intent settings" | nc -U "$GAME_SHELL_SOCK"   # control surface
echo "key down"         | nc -U "$GAME_SHELL_SOCK"   # synthesized input
echo "key select"       | nc -U "$GAME_SHELL_SOCK"
```

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
| `controller-wake` | A gamepad was discovered and grabbed on (re)connect (fires per joining pad) |
| `controller-disconnected` | A gamepad stream errored during event read (USB disconnect; fires per leaving pad) |
| `pad:connected:<json>` | A pad joined the fleet and was assigned a player slot. Payload: compact `{id,index,name}` object (#101) |
| `pad:disconnected:<id>` | A pad left the fleet; its slot is freed for reuse. Payload: the pad's stable wire `id` |
| `pad:index:<json>` | A pad's player-indicator LED was lit to match its slot at assignment (#101 LED). Payload: compact `{id,index}` object. Emitted for pads with a controllable LED — via `EV_LED`, or via the `/sys/class/leds` xpad fallback (Xbox 360 pads expose their ring through sysfs, not `EV_LED`); a no-op for pads with neither |
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
  first, then falls back to the `/sys/class/leds` xpad node (Xbox 360 pads expose
  their player ring through sysfs, not `EV_LED`). No controllable LED → no
  event.
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

### Home Button

The gamepad Home button (`BTN_MODE`) is **always intercepted** by the daemon and
surfaced as a neutral [intent](#intents) on the broadcast stream — in **both**
the shell and game presenters (so the shell overlay can come up over a running
game). It is never mapped to a key and never forwarded to a game's virtual pad.

| Event | Trigger |
|-------|---------|
| `intent:home-tap` | `BTN_MODE` released before the 2-second hold threshold |
| `intent:home-hold` | `BTN_MODE` held for 2 seconds |

The legacy `home-press` / `combo:home-hold` events were removed in Phase 5 — QML
now consumes only the `intent:*` vocabulary.

### Intents

Every accepted [`intent <name>`](#intent-name) command re-broadcasts here as an
`intent:<name>` event. This is the global control-surface stream: keyboard
global-escape, automation, and (in later phases) the daemon's own gamepad logic
all surface through it, so QML consumes **one** vocabulary regardless of source.

| Event | Payload (`<name>`) |
|-------|--------------------|
| `intent:<name>` | One of `home`, `home-tap`, `home-hold`, `menu`, `settings`, `power` |

`intent:home` is the global return-to-shell escape; `intent:home-tap` /
`intent:home-hold` are the neutral gamepad Home signals (QML maps them by the
focus it owns). `intent:menu` toggles the navigation drawer. The daemon's own
gamepad Home handling publishes `intent:home-tap` / `intent:home-hold` directly,
so QML has exactly one shell-intent vocabulary regardless of source.

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
`intent:home-tap` (tap) / `intent:home-hold` (hold) on the socket — mapping it to a
key would leak `KEY_HOMEPAGE` to whatever app has keyboard focus. It is still a
valid *target* for `set-binding` (it is in the remappable set), but no action
defaults to it.

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

## Presenters: Shell vs Game

The daemon **keeps the physical `EVIOCGRAB` in both modes** (Phase 5) — no
controller input ever leaks to the compositor. The `grab` IPC selects the
**shell presenter**, `release` selects the **game presenter**; the two differ
only in where the pad's input goes.

| Feature | Shell presenter (`grab`) | Game presenter (`release`) |
|---------|--------------------------|----------------------------|
| Physical pad grabbed | Yes | Yes |
| Per-player clean virtual gamepad | — | One per pad (`game-shell-virtual-pad-<slot>`) |
| Button-to-keyboard mapping | Active | — (raw buttons forwarded to the virtual pad) |
| D-pad / left stick → arrow keys | Active | — (forwarded as raw axes) |
| Right stick → mouse cursor | Active | — (forwarded as raw axis) |
| LB/RB → mouse clicks | Active | — (forwarded as raw buttons) |
| Combo detection (end-session, force-quit, suspend) | Active | Active |
| Home → `intent:home-tap` / `intent:home-hold` | Active (intercepted) | Active (intercepted, never forwarded) |
| `buttons:` debug events | Active | — |
| Input mode tracking | Active | — |
| Capture mode | Active | — |
| Force-quit `Ctrl+Alt+Shift+Q` emission | No (not needed) | Yes |

On entering the shell presenter: each pad's clean virtual gamepad is torn down;
held keys/triggers reset and combos cancel on the underlying grab.
On entering the game presenter: each pad gets a clean virtual gamepad mirroring
its capabilities (keys, axes with calibration, `input_id`). The game reads the
virtual pad; the daemon still intercepts Home so the shell can come up over a
running game.

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

- **`BTN_MODE` is socket-only**: The Home button is not mapped to a keyboard key. It drives `intent:home-tap` (tap) and `intent:home-hold` (hold) subscriber events directly, intercepted in both presenters. Mapping it to `KEY_HOMEPAGE` was intentionally avoided because that keycode leaks to focused apps (browsers treat it as "go to home page"). `BTN_MODE` is still a valid `set-binding` *target*, but no action defaults to it.
