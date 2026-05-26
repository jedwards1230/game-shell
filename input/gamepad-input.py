#!/usr/bin/env python3
"""Game Shell input daemon.

Grabs a gamepad exclusively via EVIOCGRAB and emits keyboard events via uinput.
Listens on a unix socket for grab/release/subscribe commands from the shell.
"""

import asyncio
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

BUTTON_MAP = {
    ecodes.BTN_SOUTH: ecodes.KEY_ENTER,   # A → Enter
    ecodes.BTN_EAST: ecodes.KEY_ESC,      # B → Escape
    ecodes.BTN_NORTH: ecodes.KEY_TAB,     # Y → Tab
    ecodes.BTN_START: ecodes.KEY_ENTER,   # Start → Enter
    ecodes.BTN_MODE: ecodes.KEY_HOMEPAGE, # Guide → Homepage (navigation drawer)
}

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


# Left analog stick configuration
STICK_DEADZONE = 0.30  # 30% of half-range from center before triggering

# Repeat timing (seconds)
STICK_INITIAL_DELAY = 0.300  # 300ms before repeat starts
STICK_REPEAT_INTERVAL = 0.150  # 150ms between repeats


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

        # Left stick state: track which direction is currently "pressed"
        # and repeat tasks for each axis
        self.stick_x_key: int | None = None  # Currently emitted key for X axis
        self.stick_y_key: int | None = None  # Currently emitted key for Y axis
        self.stick_x_repeat: asyncio.Task | None = None
        self.stick_y_repeat: asyncio.Task | None = None

        # Trigger state
        self.left_trigger_held = False
        self.right_trigger_held = False

        # Stick calibration — computed from device absinfo on connect
        self.stick_center_x: int = 0
        self.stick_center_y: int = 0
        self.stick_threshold_x: int = 0
        self.stick_threshold_y: int = 0

    async def start(self):
        # Deduplicate mapped keys (e.g., BTN_SOUTH and BTN_START both map to KEY_ENTER)
        # sorted() ensures deterministic uinput capability registration order
        mapped_keys = sorted(set(BUTTON_MAP.values()))
        self.uinput = UInput(
            {ecodes.EV_KEY: mapped_keys + [
                ecodes.KEY_UP, ecodes.KEY_DOWN, ecodes.KEY_LEFT, ecodes.KEY_RIGHT,
                ecodes.KEY_LEFTCTRL, ecodes.KEY_LEFTALT, ecodes.KEY_LEFTSHIFT,
                ecodes.KEY_Q,
            ]},
            name="game-shell-virtual-kb",
        )
        log.info("uinput device created: %s", self.uinput.device.path)

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

            try:
                async for event in self.gamepad.async_read_loop():
                    if not self.grabbed:
                        await self._handle_event_ungrabbed(event)
                        continue
                    await self._handle_event(event)
            except OSError:
                log.warning("Gamepad disconnected, will reconnect...")
                self._reset_stick_state()
                self.gamepad = None
                self.grabbed = False
                await asyncio.sleep(1)

    async def _handle_event(self, event):
        if event.type == ecodes.EV_KEY:
            key_event = categorize(event)

            # Track held state for combo detection
            if key_event.keystate == 1:  # down
                self.held_keys.add(event.code)
                self._check_combo_start()
                self._check_quit_combo()
                asyncio.ensure_future(self._notify_held_buttons())
            elif key_event.keystate == 0:  # up
                self.held_keys.discard(event.code)
                self._cancel_combo()
                asyncio.ensure_future(self._notify_held_buttons())

            # Map to keyboard
            mapped = BUTTON_MAP.get(event.code)
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
        if event.type != ecodes.EV_KEY:
            return

        key_event = categorize(event)
        if key_event.keystate == 1:  # down
            self.held_keys.add(event.code)
            self._check_combo_start()
            self._check_quit_combo()
            asyncio.ensure_future(self._notify_held_buttons())
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
            self.uinput.write(ecodes.EV_KEY, k, 1)
        self.uinput.syn()
        for k in reversed(keys):
            self.uinput.write(ecodes.EV_KEY, k, 0)
        self.uinput.syn()
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
        log.info("Stick calibration: X center=%d threshold=%d, Y center=%d threshold=%d",
                 self.stick_center_x, self.stick_threshold_x,
                 self.stick_center_y, self.stick_threshold_y)

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

    def _check_combo_start(self):
        if COMBO_KEYS.issubset(self.held_keys) and self.combo_task is None:
            self.combo_task = asyncio.create_task(self._combo_timer())

    def _cancel_combo(self):
        if self.combo_task and not COMBO_KEYS.issubset(self.held_keys):
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
                self.held_keys.clear()
                log.info("Grabbed gamepad exclusively")
            except OSError as e:
                log.error("Failed to grab gamepad: %s", e)

    async def _ungrab(self):
        if self.gamepad and self.grabbed:
            try:
                self.gamepad.ungrab()
                self.grabbed = False
                self.held_keys.clear()
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
    if daemon.uinput:
        daemon.uinput.close()
    log.info("Shutting down")


if __name__ == "__main__":
    asyncio.run(main())
