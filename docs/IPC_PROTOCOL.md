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

Example: `{"select":"BTN_SOUTH","back":"BTN_EAST","altSelect":"BTN_NORTH","confirm":"BTN_START","drawer":"BTN_MODE"}\n`

### `set-binding <action> <button_name>`

Remap a button for the given action. Rebuilds the internal button map and persists to settings.

**Response:**

| Condition | Response |
|-----------|----------|
| Success | `ok\n` |
| Wrong number of args | `error:usage: set-binding <action> <button_name>\n` |
| Unknown action | `error:unknown action '<action>'\n` |
| Invalid or non-remappable button | `error:invalid button '<button_name>'\n` |

Valid actions: `select`, `back`, `altSelect`, `confirm`, `drawer`

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
| `buttons:<held>` | Space-and-plus-separated list of currently held inputs |

Sent on every button down/up, trigger threshold crossing, and stick axis change. The `<held>` portion is empty when nothing is held (i.e., `buttons:\n`). Includes button friendly names, D-pad directions, stick directions, and trigger state.

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

## Default Button Mappings

| Action | Default Button | Keyboard Output | Remappable |
|--------|---------------|-----------------|------------|
| `select` | `BTN_SOUTH` (A) | `KEY_ENTER` | Yes |
| `back` | `BTN_EAST` (B) | `KEY_ESC` | Yes |
| `altSelect` | `BTN_NORTH` (Y) | `KEY_TAB` | Yes |
| `confirm` | `BTN_START` (Start) | `KEY_ENTER` | Yes |
| `drawer` | `BTN_MODE` (Home) | `KEY_HOMEPAGE` | Yes |

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
{"keyBindings":{"select":"BTN_SOUTH","back":"BTN_EAST","altSelect":"BTN_NORTH","confirm":"BTN_START","drawer":"BTN_MODE"}}
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

The daemon scans `/dev/input/event*` devices matching a vendor/product ID pair, filtering for devices with `EV_KEY` capability containing `BTN_SOUTH`.

| Parameter | Default | Env Override |
|-----------|---------|--------------|
| Vendor ID | `0x045e` (Microsoft) | `GAMEPAD_VENDOR` |
| Product ID | `0x028e` (Xbox 360 Controller) | `GAMEPAD_PRODUCT` |

On disconnect (`OSError` during event read), the daemon sends `controller-disconnected` to subscribers, resets stick state, and retries discovery every 1 second. On initial startup with no gamepad present, it retries every 2 seconds.

## Known Issues

- **`KeyBindingsSettings.qml` hardcodes socket path**: Uses `/run/user/1000/game-shell-input.sock` via `socat` instead of respecting `GAME_SHELL_SOCK`. Other QML components use Python one-liners that read the env var.
- **`BTN_MODE` dual purpose**: The Home button serves both as a remappable action (`drawer`) and as the trigger for home-press/home-hold detection. These are independent code paths — the key mapping emits `KEY_HOMEPAGE` via uinput, while the hold/press detection fires subscriber events. Remapping `drawer` to a different button moves the keyboard emission but does not move the home-hold/press detection.
