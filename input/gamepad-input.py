#!/usr/bin/env python3
"""Game Shell input daemon.

Grabs a gamepad exclusively via EVIOCGRAB and emits keyboard events via uinput.
Listens on a unix socket for grab/release/subscribe commands from the shell.

IPC protocol: see docs/IPC_PROTOCOL.md
Commands: grab, release, status, subscribe, get-bindings, set-binding, capture-next, capture-cancel
Events (to subscribers): controller-wake, controller-disconnected, home-press, combo:*, input-mode:*, buttons:*
"""

import asyncio
import json
import logging
import os
import signal
import sys
from pathlib import Path

import evdev
from evdev import InputDevice, UInput, ecodes, categorize

log = logging.getLogger("game-shell-input")

VENDOR_ID = int(os.environ.get("GAMEPAD_VENDOR", "0x045e"), 0)
PRODUCT_ID = int(os.environ.get("GAMEPAD_PRODUCT", "0x028e"), 0)

SOCK_PATH = os.environ.get(
    "GAME_SHELL_SOCK",
    f"/run/user/{os.getuid()}/game-shell-input.sock",
)

DEFAULT_BINDINGS = {
    "select": (ecodes.BTN_SOUTH, ecodes.KEY_ENTER),
    "back": (ecodes.BTN_EAST, ecodes.KEY_ESC),
    "altSelect": (ecodes.BTN_NORTH, ecodes.KEY_TAB),
    "confirm": (ecodes.BTN_START, ecodes.KEY_ENTER),
    "drawer": (ecodes.BTN_MODE, ecodes.KEY_HOMEPAGE),
}

REMAPPABLE_BUTTONS = {
    ecodes.BTN_SOUTH, ecodes.BTN_EAST, ecodes.BTN_NORTH, ecodes.BTN_WEST,
    ecodes.BTN_TL, ecodes.BTN_TR, ecodes.BTN_SELECT, ecodes.BTN_START,
    ecodes.BTN_MODE, ecodes.BTN_THUMBL, ecodes.BTN_THUMBR,
}

SETTINGS_PATH = Path(os.path.expanduser("~/.config/game-shell/settings.json"))


def _button_name_to_code(name: str) -> int | None:
    """Convert an evdev code name like 'BTN_SOUTH' to its integer code."""
    return getattr(ecodes, name, None)


def _button_code_to_name(code: int) -> str:
    """Convert an evdev button code to its name like 'BTN_SOUTH'."""
    names = ecodes.bytype.get(ecodes.EV_KEY, {}).get(code)
    if names:
        return names[0] if isinstance(names, (list, tuple)) else names
    return f"0x{code:x}"


COMBO_KEYS = {ecodes.BTN_MODE, ecodes.BTN_EAST}
COMBO_HOLD_SECS = 3.0

# Force-quit combo: Back + Home + LB + RB (instant, no hold required)
QUIT_COMBO_KEYS = {ecodes.BTN_SELECT, ecodes.BTN_MODE, ecodes.BTN_TL, ecodes.BTN_TR}

BUTTON_NAMES = {
    ecodes.BTN_SOUTH: "A",
    ecodes.BTN_EAST: "B",
    ecodes.BTN_NORTH: "Y",
    ecodes.BTN_WEST: "X",
    ecodes.BTN_TL: "LB",
    ecodes.BTN_TR: "RB",
    ecodes.BTN_TL2: "LT",
    ecodes.BTN_TR2: "RT",
    ecodes.BTN_SELECT: "Back",
    ecodes.BTN_START: "Start",
    ecodes.BTN_MODE: "Home",
    ecodes.BTN_THUMBL: "L3",
    ecodes.BTN_THUMBR: "R3",
}

DPAD_NAMES = {
    ecodes.KEY_UP: "D-Up",
    ecodes.KEY_DOWN: "D-Down",
    ecodes.KEY_LEFT: "D-Left",
    ecodes.KEY_RIGHT: "D-Right",
}


# Home button hold detection
HOME_HOLD_KEYS = {ecodes.BTN_MODE}
HOME_HOLD_SECS = 2.0

# Left analog stick configuration
STICK_DEADZONE = 0.30  # 30% of half-range from center before triggering

# Repeat timing (seconds)
STICK_INITIAL_DELAY = 0.300  # 300ms before repeat starts
STICK_REPEAT_INTERVAL = 0.150  # 150ms between repeats

# Right-stick mouse cursor
MOUSE_SPEED_MIN = 2
MOUSE_SPEED_MAX = 25
MOUSE_POLL_MS = 16  # ~60Hz


def find_gamepad() -> InputDevice | None:
    for path in sorted(evdev.list_devices()):
        dev = InputDevice(path)
        if dev.info.vendor == VENDOR_ID and dev.info.product == PRODUCT_ID:
            caps = dev.capabilities(verbose=False)
            if ecodes.EV_KEY in caps and ecodes.BTN_SOUTH in caps[ecodes.EV_KEY]:
                return dev
    return None


class InputDaemon:
    def __init__(self):
        self.gamepad: InputDevice | None = None
        self.uinput: UInput | None = None
        self.grabbed = False
        self.subscribers: list[asyncio.StreamWriter] = []
        self.held_keys: set[int] = set()
        self.combo_task: asyncio.Task | None = None
        self.running = True

        # Build button_map from defaults, then override from config
        self.button_map: dict[int, int] = {}
        self._bindings: dict[str, tuple[int, int]] = dict(DEFAULT_BINDINGS)
        self._rebuild_button_map()
        self._load_bindings()

        # Capture state for keybinding reassignment
        self._capture_future: asyncio.Future | None = None

        # Left stick state: track which direction is currently "pressed"
        # and repeat tasks for each axis
        self.stick_x_key: int | None = None  # Currently emitted key for X axis
        self.stick_y_key: int | None = None  # Currently emitted key for Y axis
        self.stick_x_repeat: asyncio.Task | None = None
        self.stick_y_repeat: asyncio.Task | None = None

        # Right stick state (debug overlay + mouse cursor)
        self.rstick_x_dir: str | None = None
        self.rstick_y_dir: str | None = None
        self.rstick_raw_x: int = 0
        self.rstick_raw_y: int = 0
        self.rstick_half_range_x: int = 1
        self.rstick_half_range_y: int = 1
        self.mouse_uinput: UInput | None = None
        self._mouse_task: asyncio.Task | None = None

        # Home button hold detection
        self._home_hold_task: asyncio.Task | None = None

        # Trigger state
        self.left_trigger_held = False
        self.right_trigger_held = False

        # Input mode: "controller" (D-pad/left stick/face buttons) or "mouse" (right stick/LB/RB)
        self._input_mode: str = "controller"

        # Stick calibration — computed from device absinfo on connect
        self.stick_center_x: int = 0
        self.stick_center_y: int = 0
        self.stick_threshold_x: int = 0
        self.stick_threshold_y: int = 0
        self.rstick_center_x: int = 0
        self.rstick_center_y: int = 0
        self.rstick_threshold_x: int = 0
        self.rstick_threshold_y: int = 0

    async def _set_input_mode(self, mode: str):
        if mode == self._input_mode:
            return
        self._input_mode = mode
        await self._notify_subscribers(f"input-mode:{mode}")

    async def start(self):
        # Deduplicate mapped keys (e.g., BTN_SOUTH and BTN_START both map to KEY_ENTER)
        # sorted() ensures deterministic uinput capability registration order
        mapped_keys = sorted(set(self.button_map.values()))
        self.uinput = UInput(
            {ecodes.EV_KEY: mapped_keys + [
                ecodes.KEY_UP, ecodes.KEY_DOWN, ecodes.KEY_LEFT, ecodes.KEY_RIGHT,
                ecodes.KEY_LEFTCTRL, ecodes.KEY_LEFTALT, ecodes.KEY_LEFTSHIFT,
                ecodes.KEY_Q,
            ]},
            name="game-shell-virtual-kb",
        )
        log.info("uinput device created: %s", self.uinput.device.path)

        self.mouse_uinput = UInput(
            {
                ecodes.EV_REL: [ecodes.REL_X, ecodes.REL_Y, ecodes.REL_WHEEL, ecodes.REL_HWHEEL],
                ecodes.EV_KEY: [ecodes.BTN_LEFT, ecodes.BTN_RIGHT, ecodes.BTN_MIDDLE],
            },
            name="game-shell-virtual-mouse",
        )
        log.info("mouse uinput device created: %s", self.mouse_uinput.device.path)

        asyncio.create_task(self._serve_socket())
        asyncio.create_task(self._device_loop())

    async def _device_loop(self):
        """Find gamepad, grab it, read events. Reconnect on disconnect."""
        while self.running:
            self.gamepad = find_gamepad()
            if not self.gamepad:
                log.info("Gamepad not found (vendor=%04x product=%04x), retrying...",
                         VENDOR_ID, PRODUCT_ID)
                await asyncio.sleep(2)
                continue

            log.info("Found gamepad: %s at %s", self.gamepad.name, self.gamepad.path)
            self._calibrate_stick()
            await self._grab()
            await self._notify_subscribers("controller-wake")

            try:
                async for event in self.gamepad.async_read_loop():
                    if not self.grabbed:
                        await self._handle_event_ungrabbed(event)
                        continue
                    await self._handle_event(event)
            except OSError:
                log.warning("Gamepad disconnected, will reconnect...")
                await self._notify_subscribers("controller-disconnected")
                self._reset_stick_state()
                self.gamepad = None
                self.grabbed = False
                await asyncio.sleep(1)

    async def _handle_event(self, event):
        if event.type == ecodes.EV_KEY:
            key_event = categorize(event)

            # Capture mode: resolve pending capture on key down, only for remappable buttons
            if self._capture_future and not self._capture_future.done() and key_event.keystate == 1:
                if event.code in REMAPPABLE_BUTTONS:
                    self._capture_future.set_result(event.code)
                    return
                # Non-remappable button pressed during capture — ignore it
                return

            # Track held state for combo detection
            if key_event.keystate == 1:  # down
                self.held_keys.add(event.code)
                self._check_combo_start()
                self._check_quit_combo()
                asyncio.ensure_future(self._notify_held_buttons())
                # Input mode: LB/RB → mouse, other remappable buttons → controller
                if event.code in (ecodes.BTN_TL, ecodes.BTN_TR):
                    asyncio.ensure_future(self._set_input_mode("mouse"))
                elif event.code in REMAPPABLE_BUTTONS or event.code == ecodes.BTN_SELECT:
                    asyncio.ensure_future(self._set_input_mode("controller"))
            elif key_event.keystate == 0:  # up
                self.held_keys.discard(event.code)
                self._cancel_combo()
                asyncio.ensure_future(self._notify_held_buttons())

            # Home button hold detection
            if event.code == ecodes.BTN_MODE:
                if key_event.keystate == 1:  # down
                    self._start_home_hold()
                elif key_event.keystate == 0:  # up
                    if self._home_hold_task and not self._home_hold_task.done():
                        self._home_hold_task.cancel()
                        self._home_hold_task = None
                        asyncio.ensure_future(self._notify_subscribers("home-press"))
                    self._home_hold_task = None

            # LB/RB → mouse left/right click
            if event.code == ecodes.BTN_TL and self.mouse_uinput:
                self.mouse_uinput.write(ecodes.EV_KEY, ecodes.BTN_LEFT, key_event.keystate)
                self.mouse_uinput.syn()
            elif event.code == ecodes.BTN_TR and self.mouse_uinput:
                self.mouse_uinput.write(ecodes.EV_KEY, ecodes.BTN_RIGHT, key_event.keystate)
                self.mouse_uinput.syn()

            # Map to keyboard
            mapped = self.button_map.get(event.code)
            if mapped and self.uinput:
                self.uinput.write(ecodes.EV_KEY, mapped, key_event.keystate)
                self.uinput.syn()

        elif event.type == ecodes.EV_ABS:
            # D-pad hat → arrow keys
            if event.code == ecodes.ABS_HAT0X:
                if event.value == -1:
                    self._emit_key(ecodes.KEY_LEFT, 1)
                    self.held_keys.add(ecodes.KEY_LEFT)
                elif event.value == 1:
                    self._emit_key(ecodes.KEY_RIGHT, 1)
                    self.held_keys.add(ecodes.KEY_RIGHT)
                else:
                    self._emit_key(ecodes.KEY_LEFT, 0)
                    self._emit_key(ecodes.KEY_RIGHT, 0)
                    self.held_keys.discard(ecodes.KEY_LEFT)
                    self.held_keys.discard(ecodes.KEY_RIGHT)
                if event.value != 0:
                    asyncio.ensure_future(self._set_input_mode("controller"))
                asyncio.ensure_future(self._notify_held_buttons())
            elif event.code == ecodes.ABS_HAT0Y:
                if event.value == -1:
                    self._emit_key(ecodes.KEY_UP, 1)
                    self.held_keys.add(ecodes.KEY_UP)
                elif event.value == 1:
                    self._emit_key(ecodes.KEY_DOWN, 1)
                    self.held_keys.add(ecodes.KEY_DOWN)
                else:
                    self._emit_key(ecodes.KEY_UP, 0)
                    self._emit_key(ecodes.KEY_DOWN, 0)
                    self.held_keys.discard(ecodes.KEY_UP)
                    self.held_keys.discard(ecodes.KEY_DOWN)
                if event.value != 0:
                    asyncio.ensure_future(self._set_input_mode("controller"))
                asyncio.ensure_future(self._notify_held_buttons())

            # Left analog stick → arrow keys (with deadzone + repeat)
            elif event.code == ecodes.ABS_X:
                self._handle_stick_axis(
                    event.value, "x",
                    ecodes.KEY_LEFT, ecodes.KEY_RIGHT,
                )
            elif event.code == ecodes.ABS_Y:
                self._handle_stick_axis(
                    event.value, "y",
                    ecodes.KEY_UP, ecodes.KEY_DOWN,
                )

            # Right analog stick (debug overlay only, no key emission)
            elif event.code == ecodes.ABS_RX:
                self._handle_rstick_axis(event.value, "x")
            elif event.code == ecodes.ABS_RY:
                self._handle_rstick_axis(event.value, "y")

            # Triggers (analog axes treated as digital)
            elif event.code == ecodes.ABS_Z:  # Left trigger
                was = self.left_trigger_held
                self.left_trigger_held = event.value > 100
                if was != self.left_trigger_held:
                    asyncio.ensure_future(self._notify_held_buttons())
            elif event.code == ecodes.ABS_RZ:  # Right trigger
                was = self.right_trigger_held
                self.right_trigger_held = event.value > 100
                if was != self.right_trigger_held:
                    asyncio.ensure_future(self._notify_held_buttons())

    async def _handle_event_ungrabbed(self, event):
        """Lightweight handler for ungrabbed mode: combo detection + subscriber broadcast only."""
        if event.type == ecodes.EV_ABS:
            # Right stick mouse cursor works in ungrabbed mode too
            if event.code == ecodes.ABS_RX:
                self._handle_rstick_axis(event.value, "x")
            elif event.code == ecodes.ABS_RY:
                self._handle_rstick_axis(event.value, "y")
            return

        if event.type != ecodes.EV_KEY:
            return

        key_event = categorize(event)

        # LB/RB → mouse clicks in ungrabbed mode
        if event.code == ecodes.BTN_TL and self.mouse_uinput:
            self.mouse_uinput.write(ecodes.EV_KEY, ecodes.BTN_LEFT, key_event.keystate)
            self.mouse_uinput.syn()
        elif event.code == ecodes.BTN_TR and self.mouse_uinput:
            self.mouse_uinput.write(ecodes.EV_KEY, ecodes.BTN_RIGHT, key_event.keystate)
            self.mouse_uinput.syn()

        if key_event.keystate == 1:  # down
            self.held_keys.add(event.code)
            self._check_combo_start()
            self._check_quit_combo()
            asyncio.ensure_future(self._notify_held_buttons())
            # Input mode: LB/RB → mouse, other remappable buttons → controller
            if event.code in (ecodes.BTN_TL, ecodes.BTN_TR):
                asyncio.ensure_future(self._set_input_mode("mouse"))
            elif event.code in REMAPPABLE_BUTTONS or event.code == ecodes.BTN_SELECT:
                asyncio.ensure_future(self._set_input_mode("controller"))
        elif key_event.keystate == 0:  # up
            self.held_keys.discard(event.code)
            self._cancel_combo()
            asyncio.ensure_future(self._notify_held_buttons())

    def _send_moonlight_quit(self):
        """Emit Ctrl+Alt+Shift+Q via uinput to cleanly exit Moonlight."""
        if not self.uinput:
            return
        keys = [ecodes.KEY_LEFTCTRL, ecodes.KEY_LEFTALT, ecodes.KEY_LEFTSHIFT, ecodes.KEY_Q]
        for k in keys:
            self._emit_key(k, 1)
        for k in reversed(keys):
            self._emit_key(k, 0)
        log.info("Sent Ctrl+Alt+Shift+Q to quit Moonlight")

    def _emit_key(self, key, value):
        if self.uinput:
            try:
                self.uinput.write(ecodes.EV_KEY, key, value)
                self.uinput.syn()
            except OSError:
                log.debug("uinput write failed (device closed)")
                return

    def _reset_triggers(self):
        self.left_trigger_held = False
        self.right_trigger_held = False

    def _reset_stick_state(self):
        """Release any held stick keys and cancel repeat tasks."""
        for axis in ("x", "y"):
            key = getattr(self, f"stick_{axis}_key")
            if key is not None:
                self._emit_key(key, 0)
                setattr(self, f"stick_{axis}_key", None)
            self._cancel_stick_repeat(axis)
        self.rstick_x_dir = None
        self.rstick_y_dir = None

    def _calibrate_stick(self):
        """Read absinfo for ABS_X/ABS_Y to compute center and threshold."""
        if not self.gamepad:
            return
        caps = self.gamepad.capabilities(absinfo=True)
        abs_caps = caps.get(ecodes.EV_ABS, [])
        for code, info in abs_caps:
            center = (info.min + info.max) // 2
            half_range = (info.max - info.min) // 2
            threshold = int(half_range * STICK_DEADZONE)
            if code == ecodes.ABS_X:
                self.stick_center_x = center
                self.stick_threshold_x = threshold
            elif code == ecodes.ABS_Y:
                self.stick_center_y = center
                self.stick_threshold_y = threshold
            elif code == ecodes.ABS_RX:
                self.rstick_center_x = center
                self.rstick_threshold_x = threshold
                self.rstick_half_range_x = half_range
            elif code == ecodes.ABS_RY:
                self.rstick_center_y = center
                self.rstick_threshold_y = threshold
                self.rstick_half_range_y = half_range
        log.info("Stick calibration: X center=%d threshold=%d, Y center=%d threshold=%d",
                 self.stick_center_x, self.stick_threshold_x,
                 self.stick_center_y, self.stick_threshold_y)

    def _rebuild_button_map(self):
        """Rebuild button_map from current bindings."""
        self.button_map = {}
        for action, (btn, kb) in self._bindings.items():
            if btn in self.button_map:
                existing = [a for a, (b, _) in self._bindings.items() if b == btn and a != action]
                log.warning("Button %s (%s) assigned to multiple actions: %s and %s",
                            _button_code_to_name(btn), btn, existing, action)
            self.button_map[btn] = kb

    def _load_bindings(self):
        """Load keybinding overrides from settings.json."""
        try:
            data = json.loads(SETTINGS_PATH.read_text())
            kb = data.get("keyBindings")
            if not isinstance(kb, dict):
                return
            for action, button_name in kb.items():
                if action not in DEFAULT_BINDINGS:
                    continue
                if isinstance(button_name, (list, tuple)):
                    button_name = button_name[-1]
                code = _button_name_to_code(button_name)
                if code is None or code not in REMAPPABLE_BUTTONS:
                    continue
                _, key_code = self._bindings[action]
                self._bindings[action] = (code, key_code)
            self._rebuild_button_map()
            log.info("Loaded key bindings from %s", SETTINGS_PATH)
        except (FileNotFoundError, json.JSONDecodeError, OSError):
            pass

    def _save_bindings(self):
        """Save current keybindings to settings.json (read-modify-write, single-line)."""
        try:
            data = json.loads(SETTINGS_PATH.read_text())
        except (FileNotFoundError, json.JSONDecodeError, OSError):
            data = {}
        kb = {}
        for action, (btn_code, _) in self._bindings.items():
            kb[action] = _button_code_to_name(btn_code)
        data["keyBindings"] = kb
        SETTINGS_PATH.parent.mkdir(parents=True, exist_ok=True)
        SETTINGS_PATH.write_text(json.dumps(data, separators=(",", ":")))
        log.info("Saved key bindings to %s", SETTINGS_PATH)

    def _handle_stick_axis(self, value: int, axis: str,
                           neg_key: int, pos_key: int):
        """Handle left stick axis crossing deadzone thresholds.

        Emits a key press on threshold crossing, then starts a repeat task.
        Releases the key when the stick returns inside the deadzone.
        """
        center = getattr(self, f"stick_center_{axis}")
        threshold = getattr(self, f"stick_threshold_{axis}")
        offset = value - center
        current_key = getattr(self, f"stick_{axis}_key")

        if offset < -threshold:
            new_key = neg_key
        elif offset > threshold:
            new_key = pos_key
        else:
            new_key = None

        if new_key == current_key:
            # No change in direction — let repeat task handle it
            return

        # Release old key if any
        if current_key is not None:
            self._emit_key(current_key, 0)
            self._cancel_stick_repeat(axis)

        # Press new key if any
        setattr(self, f"stick_{axis}_key", new_key)
        if new_key is not None:
            self._emit_key(new_key, 1)
            self._start_stick_repeat(axis, new_key)
            asyncio.ensure_future(self._set_input_mode("controller"))
        asyncio.ensure_future(self._notify_held_buttons())

    def _handle_rstick_axis(self, value: int, axis: str):
        """Track right stick direction for debug overlay and mouse cursor."""
        if axis == "x":
            self.rstick_raw_x = value
        else:
            self.rstick_raw_y = value

        center = getattr(self, f"rstick_center_{axis}")
        threshold = getattr(self, f"rstick_threshold_{axis}")
        offset = value - center

        # Debug overlay direction tracking
        if axis == "x":
            new_dir = "R←" if offset < -threshold else ("R→" if offset > threshold else None)
            if new_dir != self.rstick_x_dir:
                old_dir = self.rstick_x_dir
                self.rstick_x_dir = new_dir
                if new_dir is not None and old_dir is None:
                    asyncio.ensure_future(self._set_input_mode("mouse"))
                asyncio.ensure_future(self._notify_held_buttons())
        else:
            new_dir = "R↑" if offset < -threshold else ("R↓" if offset > threshold else None)
            if new_dir != self.rstick_y_dir:
                old_dir = self.rstick_y_dir
                self.rstick_y_dir = new_dir
                if new_dir is not None and old_dir is None:
                    asyncio.ensure_future(self._set_input_mode("mouse"))
                asyncio.ensure_future(self._notify_held_buttons())

        # Start mouse movement task if any deflection, stop if both centered
        if self._has_rstick_deflection():
            if self._mouse_task is None or self._mouse_task.done():
                self._mouse_task = asyncio.create_task(self._mouse_move_loop())

    def _start_stick_repeat(self, axis: str, key: int):
        """Start a repeat task: initial delay, then periodic repeats."""
        self._cancel_stick_repeat(axis)
        task = asyncio.create_task(self._stick_repeat_loop(axis, key))
        setattr(self, f"stick_{axis}_repeat", task)

    def _cancel_stick_repeat(self, axis: str):
        """Cancel any running repeat task for the given axis."""
        task = getattr(self, f"stick_{axis}_repeat")
        if task is not None:
            task.cancel()
            setattr(self, f"stick_{axis}_repeat", None)

    async def _stick_repeat_loop(self, axis: str, key: int):
        """After initial delay, emit key-up/key-down at repeat interval."""
        try:
            await asyncio.sleep(STICK_INITIAL_DELAY)
            while self.running:
                # Emit release + press to simulate repeated key taps
                self._emit_key(key, 0)
                self._emit_key(key, 1)
                await asyncio.sleep(STICK_REPEAT_INTERVAL)
        except (asyncio.CancelledError, OSError):
            pass
        finally:
            setattr(self, f"stick_{axis}_repeat", None)

    def _has_rstick_deflection(self) -> bool:
        return self.rstick_x_dir is not None or self.rstick_y_dir is not None

    async def _mouse_move_loop(self):
        """Emit relative mouse movement at ~60Hz while right stick is deflected."""
        try:
            while self._has_rstick_deflection():
                dx = self._compute_mouse_velocity(
                    self.rstick_raw_x, self.rstick_center_x,
                    self.rstick_threshold_x, self.rstick_half_range_x,
                )
                dy = self._compute_mouse_velocity(
                    self.rstick_raw_y, self.rstick_center_y,
                    self.rstick_threshold_y, self.rstick_half_range_y,
                )
                if self.mouse_uinput:
                    if dx:
                        self.mouse_uinput.write(ecodes.EV_REL, ecodes.REL_X, dx)
                    if dy:
                        self.mouse_uinput.write(ecodes.EV_REL, ecodes.REL_Y, dy)
                    if dx or dy:
                        self.mouse_uinput.syn()
                await asyncio.sleep(MOUSE_POLL_MS / 1000)
        except (asyncio.CancelledError, OSError):
            pass

    def _compute_mouse_velocity(self, raw: int, center: int,
                                threshold: int, half_range: int) -> int:
        offset = raw - center
        if abs(offset) < threshold:
            return 0
        mag = min((abs(offset) - threshold) / max(half_range - threshold, 1), 1.0)
        speed = MOUSE_SPEED_MIN + (MOUSE_SPEED_MAX - MOUSE_SPEED_MIN) * (mag ** 2)
        return int(speed) * (1 if offset > 0 else -1)

    def _start_home_hold(self):
        if self._home_hold_task:
            self._home_hold_task.cancel()
        self._home_hold_task = asyncio.create_task(self._home_hold_timer())

    async def _home_hold_timer(self):
        try:
            await asyncio.sleep(HOME_HOLD_SECS)
            log.info("Home hold detected")
            await self._notify_subscribers("combo:home-hold")
        except asyncio.CancelledError:
            pass

    def _check_combo_start(self):
        if COMBO_KEYS.issubset(self.held_keys) and self.combo_task is None:
            self.combo_task = asyncio.create_task(self._combo_timer())

    def _cancel_combo(self):
        if self.combo_task and not COMBO_KEYS.issubset(self.held_keys):
            self.combo_task.cancel()
            self.combo_task = None

    def _cancel_combo_unconditional(self):
        """Cancel combo task regardless of held key state (for mode transitions)."""
        if self.combo_task:
            self.combo_task.cancel()
            self.combo_task = None

    def _check_quit_combo(self):
        if QUIT_COMBO_KEYS.issubset(self.held_keys):
            log.info("Force-quit combo detected (Back+Home+LB+RB)")
            if not self.grabbed:
                self._send_moonlight_quit()
            asyncio.ensure_future(self._notify_subscribers("combo:force-quit"))

    async def _combo_timer(self):
        try:
            await asyncio.sleep(COMBO_HOLD_SECS)
            if COMBO_KEYS.issubset(self.held_keys):
                log.info("End-session combo detected")
                await self._notify_subscribers("combo:end-session")
        except asyncio.CancelledError:
            pass
        finally:
            self.combo_task = None

    async def _grab(self):
        if self.gamepad and not self.grabbed:
            try:
                self.gamepad.grab()
                self.grabbed = True
                self._cancel_combo_unconditional()
                self.held_keys.clear()
                self._reset_triggers()
                asyncio.ensure_future(self._set_input_mode("controller"))
                log.info("Grabbed gamepad exclusively")
            except OSError as e:
                log.error("Failed to grab gamepad: %s", e)

    async def _ungrab(self):
        if self.gamepad and self.grabbed:
            try:
                self.gamepad.ungrab()
                self.grabbed = False
                self._cancel_combo_unconditional()
                self.held_keys.clear()
                self._reset_triggers()
                log.info("Released gamepad grab")
            except OSError as e:
                log.error("Failed to ungrab gamepad: %s", e)

    async def _notify_held_buttons(self):
        if not self.subscribers:
            return
        names = []
        for code in sorted(self.held_keys):
            if code in BUTTON_NAMES:
                names.append(BUTTON_NAMES[code])
            elif code in DPAD_NAMES:
                names.append(DPAD_NAMES[code])
            else:
                ecode_name = ecodes.bytype.get(ecodes.EV_KEY, {}).get(code)
                if ecode_name:
                    if isinstance(ecode_name, list):
                        ecode_name = ecode_name[0]
                    names.append(ecode_name.replace("BTN_", "").replace("KEY_", "").title())
                else:
                    names.append(f"0x{code:x}")
        if self.left_trigger_held:
            names.append("LT")
        if self.right_trigger_held:
            names.append("RT")
        stick_dirs = {ecodes.KEY_UP: "L↑", ecodes.KEY_DOWN: "L↓",
                      ecodes.KEY_LEFT: "L←", ecodes.KEY_RIGHT: "L→"}
        if self.stick_x_key is not None:
            names.append(stick_dirs.get(self.stick_x_key, "L?"))
        if self.stick_y_key is not None:
            names.append(stick_dirs.get(self.stick_y_key, "L?"))
        if self.rstick_x_dir is not None:
            names.append(self.rstick_x_dir)
        if self.rstick_y_dir is not None:
            names.append(self.rstick_y_dir)
        if names:
            await self._notify_subscribers("buttons:" + " + ".join(names))
        else:
            await self._notify_subscribers("buttons:")

    async def _notify_subscribers(self, message: str):
        dead = []
        for writer in self.subscribers:
            try:
                writer.write((message + "\n").encode())
                await writer.drain()
            except (ConnectionError, BrokenPipeError):
                dead.append(writer)
        for w in dead:
            self.subscribers.remove(w)

    async def _serve_socket(self):
        sock_path = Path(SOCK_PATH)
        sock_path.unlink(missing_ok=True)

        server = await asyncio.start_unix_server(self._handle_client, path=str(sock_path))
        os.chmod(str(sock_path), 0o600)
        log.info("Listening on %s", sock_path)

        async with server:
            await server.serve_forever()

    async def _handle_client(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter):
        try:
            while True:
                data = await reader.readline()
                if not data:
                    break
                cmd = data.decode().strip()

                if cmd == "grab":
                    await self._grab()
                    writer.write(b"ok\n")
                elif cmd == "release":
                    self._reset_stick_state()
                    await self._ungrab()
                    writer.write(b"ok\n")
                elif cmd == "status":
                    status = "connected" if self.gamepad else "disconnected"
                    grabbed = "grabbed" if self.grabbed else "released"
                    writer.write(f"{status}:{grabbed}\n".encode())
                elif cmd == "get-bindings":
                    result = {}
                    for action, (btn_code, _) in self._bindings.items():
                        result[action] = _button_code_to_name(btn_code)
                    writer.write((json.dumps(result, separators=(",", ":")) + "\n").encode())
                elif cmd.startswith("set-binding "):
                    parts = cmd.split(None, 2)
                    if len(parts) != 3:
                        writer.write(b"error:usage: set-binding <action> <button_name>\n")
                    else:
                        _, action, button_name = parts
                        if action not in DEFAULT_BINDINGS:
                            writer.write(f"error:unknown action '{action}'\n".encode())
                        else:
                            code = _button_name_to_code(button_name)
                            if code is None or code not in REMAPPABLE_BUTTONS:
                                writer.write(f"error:invalid button '{button_name}'\n".encode())
                            else:
                                _, key_code = self._bindings[action]
                                self._bindings[action] = (code, key_code)
                                self._rebuild_button_map()
                                self._save_bindings()
                                writer.write(b"ok\n")
                elif cmd == "capture-next":
                    if self._capture_future and not self._capture_future.done():
                        self._capture_future.cancel()
                    loop = asyncio.get_running_loop()
                    self._capture_future = loop.create_future()
                    try:
                        code = await asyncio.wait_for(self._capture_future, timeout=10.0)
                        name = _button_code_to_name(code)
                        writer.write(f"captured:{name}\n".encode())
                    except asyncio.TimeoutError:
                        writer.write(b"timeout\n")
                    except asyncio.CancelledError:
                        writer.write(b"cancelled\n")
                    finally:
                        self._capture_future = None
                elif cmd == "capture-cancel":
                    if self._capture_future and not self._capture_future.done():
                        self._capture_future.cancel()
                    self._capture_future = None
                    writer.write(b"ok\n")
                elif cmd == "subscribe":
                    self.subscribers.append(writer)
                    writer.write(b"subscribed\n")
                    await writer.drain()
                    # Keep connection open for events
                    await reader.read()
                    return
                else:
                    writer.write(b"unknown\n")

                await writer.drain()
        except (ConnectionError, BrokenPipeError):
            pass
        finally:
            if writer in self.subscribers:
                self.subscribers.remove(writer)
            writer.close()


async def main():
    logging.basicConfig(
        level=logging.INFO,
        format="%(name)s: %(message)s",
        stream=sys.stdout,
    )

    daemon = InputDaemon()

    loop = asyncio.get_running_loop()
    for sig in (signal.SIGTERM, signal.SIGINT):
        loop.add_signal_handler(sig, lambda: setattr(daemon, "running", False))

    await daemon.start()

    while daemon.running:
        await asyncio.sleep(1)

    daemon._reset_stick_state()
    if daemon.mouse_uinput:
        daemon.mouse_uinput.close()
    if daemon.uinput:
        daemon.uinput.close()
    log.info("Shutting down")


if __name__ == "__main__":
    asyncio.run(main())
