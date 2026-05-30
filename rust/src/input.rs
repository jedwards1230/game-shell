//! Linux input runtime: the single owner of all mutable daemon state.
//!
//! Runs on its own OS thread (a current-thread tokio runtime, set up in
//! `main.rs`) so real-time input timing — 60 Hz mouse, stick auto-repeat,
//! hold/combo timers — stays off the IPC server's scheduler. The IPC server
//! messages this runtime over the `Control` channel; timers are spawned tasks
//! that post `Internal` messages back to the one select loop, which keeps state
//! mutation single-owner (no `Arc<Mutex>` across `.await`).
//!
//! This is a faithful port of `input/gamepad-input.py`. The wire-facing strings
//! and pure decision logic live in `protocol`/`config`/`state` (and are tested
//! on any host); this module is the evdev/uinput glue, exercised by CI on Linux
//! and on the target device.

use crate::config as cfg;
use crate::config::{self, Binding};
use crate::device::{self, ControllerDb, GamepadHandle};
use crate::protocol::{
    resp_cancelled, resp_captured, resp_invalid_button, resp_ok, resp_status, resp_timeout,
    resp_unknown_action, Event, InputMode,
};
use crate::state::{self, Control, Reply};
use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventStream, EventType, InputEvent, KeyCode, RelativeAxisCode};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

const EV_KEY: u16 = config::EV_KEY;
const EV_REL: u16 = config::EV_REL;

/// Messages posted back into the select loop by timer/reader tasks.
///
/// Each timer message carries a `gen` (generation) token. A timer task may
/// already have sent its message into the channel by the time it is aborted
/// (e.g. a hold fires the same instant the button is released); the handler
/// ignores any message whose `gen` no longer matches the live generation for
/// that timer slot, so a stale tick can't double-fire or mis-attribute to a
/// later press.
#[derive(Debug)]
enum Internal {
    /// The given left-stick arrow key is repeating: emit up+down.
    StickRepeat {
        axis: Axis,
        key: u16,
        generation: u64,
    },
    /// 60 Hz mouse poll tick.
    MouseTick,
    /// Home button held past the threshold.
    HomeHoldFired(u64),
    /// Home + B held past the threshold.
    ComboEndSessionFired(u64),
    /// A routed keyboard key (e.g. Meta) held past the threshold.
    RoutedHoldFired(&'static str, u64),
    /// A pending `capture-next` timed out.
    CaptureTimeout(u64),
    /// A keyboard key event from a snooped (un-grabbed) keyboard.
    Kbd {
        code: u16,
        value: i32,
        raw_name: Option<String>,
    },
}

struct Daemon {
    events: broadcast::Sender<Event>,
    internal_tx: mpsc::Sender<Internal>,
    kb: VirtualDevice,
    mouse: VirtualDevice,
    db: ControllerDb,

    grabbed: bool,
    connected: bool,

    bindings: Vec<Binding>,
    button_map: HashMap<u16, u16>,

    held_keys: HashSet<u16>,
    left_trigger_held: bool,
    right_trigger_held: bool,
    input_mode: InputMode,

    // Monotonic generation allocator for timer messages (see `Internal`).
    gen_seq: u64,

    // Left stick
    stick_x_key: Option<u16>,
    stick_y_key: Option<u16>,
    stick_x_repeat: Option<JoinHandle<()>>,
    stick_y_repeat: Option<JoinHandle<()>>,
    stick_x_gen: u64,
    stick_y_gen: u64,
    stick_center_x: i32,
    stick_threshold_x: i32,
    stick_center_y: i32,
    stick_threshold_y: i32,

    // Right stick
    rstick_center_x: i32,
    rstick_threshold_x: i32,
    rstick_half_range_x: i32,
    rstick_center_y: i32,
    rstick_threshold_y: i32,
    rstick_half_range_y: i32,
    rstick_raw_x: i32,
    rstick_raw_y: i32,
    rstick_x_dir: Option<&'static str>,
    rstick_y_dir: Option<&'static str>,
    mouse_task: Option<JoinHandle<()>>,

    // Hold/combo timers
    home_hold_task: Option<JoinHandle<()>>,
    home_hold_gen: u64,
    combo_task: Option<JoinHandle<()>>,
    combo_gen: u64,
    routed_hold_tasks: HashMap<&'static str, JoinHandle<()>>,
    routed_hold_fired: HashMap<&'static str, bool>,
    routed_chord_seen: HashMap<&'static str, bool>,
    routed_hold_gen: HashMap<&'static str, u64>,

    // Keyboard snoop
    kbd_held_keys: BTreeSet<u16>,
    kbd_log_enabled: bool,

    // Capture (keybinding reassignment)
    pending_capture: Option<Reply>,
    capture_timeout_task: Option<JoinHandle<()>>,
    capture_gen: u64,
}

/// Entry point: build the daemon and run the event loop until `Shutdown`.
pub async fn run(mut control_rx: mpsc::Receiver<Control>, events: broadcast::Sender<Event>) {
    let (internal_tx, mut internal_rx) = mpsc::channel::<Internal>(256);

    let bindings = config::load_bindings(&config::settings_path());
    let mut button_map = HashMap::new();
    for b in &bindings {
        button_map.insert(b.button, b.key);
    }

    let (kb, mouse) = match build_uinput(&button_map) {
        Ok(v) => v,
        Err(e) => {
            error!("uinput init failed (need /dev/uinput access): {e}");
            return;
        }
    };

    let db = device::load_db();
    if db.is_empty() {
        warn!("controller DB is empty; relying on the BTN_SOUTH fallback for discovery");
    } else {
        info!("controller DB loaded: {} known models", db.len());
    }

    let mut d = Daemon {
        events,
        internal_tx: internal_tx.clone(),
        kb,
        mouse,
        db,
        grabbed: false,
        connected: false,
        bindings,
        button_map,
        held_keys: HashSet::new(),
        left_trigger_held: false,
        right_trigger_held: false,
        input_mode: InputMode::Controller,
        gen_seq: 0,
        stick_x_key: None,
        stick_y_key: None,
        stick_x_repeat: None,
        stick_y_repeat: None,
        stick_x_gen: 0,
        stick_y_gen: 0,
        stick_center_x: 0,
        stick_threshold_x: 0,
        stick_center_y: 0,
        stick_threshold_y: 0,
        rstick_center_x: 0,
        rstick_threshold_x: 0,
        rstick_half_range_x: 1,
        rstick_center_y: 0,
        rstick_threshold_y: 0,
        rstick_half_range_y: 1,
        rstick_raw_x: 0,
        rstick_raw_y: 0,
        rstick_x_dir: None,
        rstick_y_dir: None,
        mouse_task: None,
        home_hold_task: None,
        home_hold_gen: 0,
        combo_task: None,
        combo_gen: 0,
        routed_hold_tasks: HashMap::new(),
        routed_hold_fired: HashMap::new(),
        routed_chord_seen: HashMap::new(),
        routed_hold_gen: HashMap::new(),
        kbd_held_keys: BTreeSet::new(),
        kbd_log_enabled: false,
        pending_capture: None,
        capture_timeout_task: None,
        capture_gen: 0,
    };

    // Read-only keyboard snoop for the debug overlay.
    tokio::spawn(keyboard_supervisor(internal_tx.clone()));

    let mut gamepad: Option<EventStream> = None;

    loop {
        tokio::select! {
            // Control commands from the IPC server.
            ctrl = control_rx.recv() => {
                match ctrl {
                    Some(c) => if !d.handle_control(c, &mut gamepad) { break },
                    None => break,
                }
            }
            // Timer/reader callbacks.
            Some(internal) = internal_rx.recv() => {
                d.handle_internal(internal);
            }
            // Live gamepad events (only when connected).
            res = next_event(&mut gamepad), if gamepad.is_some() => {
                match res {
                    Ok(ev) => d.handle_gamepad(ev),
                    Err(_) => d.on_disconnect(&mut gamepad),
                }
            }
            // Reconnect poll while no gamepad.
            _ = tokio::time::sleep(Duration::from_secs(2)), if gamepad.is_none() => {
                d.try_connect(&mut gamepad);
            }
        }
    }

    d.reset_stick_state();
    info!("Shutting down");
}

/// Await the next event from the (optional) gamepad stream. Pends forever when
/// no gamepad is present (the select arm is guarded on `is_some()` anyway).
async fn next_event(g: &mut Option<EventStream>) -> std::io::Result<InputEvent> {
    match g {
        Some(stream) => stream.next_event().await,
        None => std::future::pending().await,
    }
}

fn build_uinput(button_map: &HashMap<u16, u16>) -> std::io::Result<(VirtualDevice, VirtualDevice)> {
    // Keyboard: all mapped keys (deduped) + the arrows, modifiers, and Q used
    // for d-pad/left-stick and the Moonlight force-quit chord.
    let mut mapped: Vec<u16> = button_map.values().copied().collect();
    mapped.sort_unstable();
    mapped.dedup();
    let extra = [
        cfg::KEY_UP,
        cfg::KEY_DOWN,
        cfg::KEY_LEFT,
        cfg::KEY_RIGHT,
        cfg::KEY_LEFTCTRL,
        cfg::KEY_LEFTALT,
        cfg::KEY_LEFTSHIFT,
        cfg::KEY_Q,
    ];
    let keys: AttributeSet<KeyCode> = mapped
        .iter()
        .chain(extra.iter())
        .map(|&k| KeyCode::new(k))
        .collect();
    let kb = VirtualDevice::builder()?
        .name("game-shell-virtual-kb")
        .with_keys(&keys)?
        .build()?;
    info!("uinput keyboard device created");

    let mkeys: AttributeSet<KeyCode> = [cfg::BTN_LEFT, cfg::BTN_RIGHT, cfg::BTN_MIDDLE]
        .into_iter()
        .map(KeyCode::new)
        .collect();
    // RelativeAxisCode is a tuple struct `RelativeAxisCode(pub u16)` (no `new`).
    let axes: AttributeSet<RelativeAxisCode> =
        [cfg::REL_X, cfg::REL_Y, cfg::REL_WHEEL, cfg::REL_HWHEEL]
            .into_iter()
            .map(RelativeAxisCode)
            .collect();
    let mouse = VirtualDevice::builder()?
        .name("game-shell-virtual-mouse")
        .with_keys(&mkeys)?
        .with_relative_axes(&axes)?
        .build()?;
    info!("uinput mouse device created");

    Ok((kb, mouse))
}

impl Daemon {
    // --- emit helpers -----------------------------------------------------

    fn emit_key(&mut self, key: u16, value: i32) {
        let _ = self.kb.emit(&[InputEvent::new(EV_KEY, key, value)]);
    }

    fn emit_mouse_button(&mut self, button: u16, value: i32) {
        let _ = self.mouse.emit(&[InputEvent::new(EV_KEY, button, value)]);
    }

    fn emit_mouse_move(&mut self, dx: i32, dy: i32) {
        let mut evs: Vec<InputEvent> = Vec::with_capacity(2);
        if dx != 0 {
            evs.push(InputEvent::new(EV_REL, cfg::REL_X, dx));
        }
        if dy != 0 {
            evs.push(InputEvent::new(EV_REL, cfg::REL_Y, dy));
        }
        if !evs.is_empty() {
            let _ = self.mouse.emit(&evs);
        }
    }

    fn send_moonlight_quit(&mut self) {
        let keys = [
            cfg::KEY_LEFTCTRL,
            cfg::KEY_LEFTALT,
            cfg::KEY_LEFTSHIFT,
            cfg::KEY_Q,
        ];
        for &k in &keys {
            self.emit_key(k, 1);
        }
        for &k in keys.iter().rev() {
            self.emit_key(k, 0);
        }
        info!("Sent Ctrl+Alt+Shift+Q to quit Moonlight");
    }

    // --- event publishing -------------------------------------------------

    fn publish(&self, ev: Event) {
        let _ = self.events.send(ev);
    }

    fn set_input_mode(&mut self, mode: InputMode) {
        if mode == self.input_mode {
            return;
        }
        self.input_mode = mode;
        self.publish(Event::InputMode(mode));
    }

    fn notify_held_buttons(&self) {
        if self.events.receiver_count() == 0 {
            return;
        }
        let mut held: Vec<u16> = self.held_keys.iter().copied().collect();
        held.sort_unstable();
        let payload = state::build_buttons_payload(
            &held,
            self.left_trigger_held,
            self.right_trigger_held,
            self.stick_x_key,
            self.stick_y_key,
            self.rstick_x_dir,
            self.rstick_y_dir,
            |code| Some(format!("{:?}", KeyCode::new(code))),
        );
        self.publish(Event::Buttons(payload));
    }

    fn notify_held_keys(&self) {
        if self.events.receiver_count() == 0 {
            return;
        }
        let names: Vec<String> = self
            .kbd_held_keys
            .iter()
            .map(|&code| {
                let raw = format!("{:?}", KeyCode::new(code));
                config::kbd_display_name(code, Some(&raw))
            })
            .collect();
        self.publish(Event::Keys(names.join(" + ")));
    }

    // --- control handling -------------------------------------------------

    /// Returns false to stop the loop (shutdown).
    fn handle_control(&mut self, ctrl: Control, gamepad: &mut Option<EventStream>) -> bool {
        match ctrl {
            Control::Grab(r) => {
                self.do_grab(gamepad);
                let _ = r.send(resp_ok());
            }
            Control::Release(r) => {
                self.reset_stick_state();
                self.do_ungrab(gamepad);
                let _ = r.send(resp_ok());
            }
            Control::Status(r) => {
                let _ = r.send(resp_status(self.connected, self.grabbed));
            }
            Control::GetBindings(r) => {
                let ordered: Vec<(String, String)> = self
                    .bindings
                    .iter()
                    .map(|b| (b.action.to_string(), config::button_code_to_name(b.button)))
                    .collect();
                let _ = r.send(crate::protocol::resp_bindings(&ordered));
            }
            Control::SetBinding {
                action,
                button,
                reply,
            } => {
                let resp = self.do_set_binding(&action, &button);
                let _ = reply.send(resp);
            }
            Control::CaptureNext(r) => self.arm_capture(r),
            Control::CaptureCancel(r) => {
                self.cancel_capture();
                let _ = r.send(resp_ok());
            }
            Control::KbdLog(on, r) => {
                self.kbd_log_enabled = on;
                info!(
                    "keyboard logging {}",
                    if on { "enabled" } else { "disabled" }
                );
                let _ = r.send(resp_ok());
            }
            Control::Shutdown => return false,
        }
        true
    }

    fn do_set_binding(&mut self, action: &str, button: &str) -> String {
        if !config::is_default_action(action) {
            return resp_unknown_action(action);
        }
        let Some(code) = config::button_name_to_code(button) else {
            return resp_invalid_button(button);
        };
        if !config::is_remappable(code) {
            return resp_invalid_button(button);
        }
        for b in self.bindings.iter_mut() {
            if b.action == action {
                b.button = code;
            }
        }
        self.rebuild_button_map();
        if let Err(e) = config::save_bindings(&config::settings_path(), &self.bindings) {
            warn!("failed to save bindings: {e}");
        }
        resp_ok()
    }

    fn rebuild_button_map(&mut self) {
        self.button_map.clear();
        for b in &self.bindings {
            self.button_map.insert(b.button, b.key);
        }
    }

    // --- generation tokens ------------------------------------------------

    /// Allocate the next monotonic generation token for a timer.
    fn next_generation(&mut self) -> u64 {
        self.gen_seq += 1;
        self.gen_seq
    }

    // --- capture ----------------------------------------------------------

    fn arm_capture(&mut self, reply: Reply) {
        // Replacing a pending capture cancels the previous one.
        if let Some(old) = self.pending_capture.take() {
            let _ = old.send(resp_cancelled());
        }
        if let Some(t) = self.capture_timeout_task.take() {
            t.abort();
        }
        let generation = self.next_generation();
        self.capture_gen = generation;
        self.pending_capture = Some(reply);
        let tx = self.internal_tx.clone();
        self.capture_timeout_task = Some(tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(config::CAPTURE_TIMEOUT_SECS)).await;
            let _ = tx.send(Internal::CaptureTimeout(generation)).await;
        }));
    }

    fn resolve_capture(&mut self, code: u16) {
        if let Some(r) = self.pending_capture.take() {
            let _ = r.send(resp_captured(&config::button_code_to_name(code)));
        }
        if let Some(t) = self.capture_timeout_task.take() {
            t.abort();
        }
        // Invalidate any already-queued timeout from the resolved capture.
        self.capture_gen = self.next_generation();
    }

    fn cancel_capture(&mut self) {
        if let Some(r) = self.pending_capture.take() {
            let _ = r.send(resp_cancelled());
        }
        if let Some(t) = self.capture_timeout_task.take() {
            t.abort();
        }
        self.capture_gen = self.next_generation();
    }

    // --- grab lifecycle ---------------------------------------------------

    fn do_grab(&mut self, gamepad: &mut Option<EventStream>) {
        if self.grabbed {
            return;
        }
        let Some(stream) = gamepad.as_mut() else {
            return;
        };
        match stream.device_mut().grab() {
            Ok(()) => {
                self.grabbed = true;
                self.cancel_combo_unconditional();
                self.held_keys.clear();
                self.reset_triggers();
                self.set_input_mode(InputMode::Controller);
                info!("Grabbed gamepad exclusively");
            }
            Err(e) => error!("Failed to grab gamepad: {e}"),
        }
    }

    fn do_ungrab(&mut self, gamepad: &mut Option<EventStream>) {
        if !self.grabbed {
            return;
        }
        let Some(stream) = gamepad.as_mut() else {
            return;
        };
        match stream.device_mut().ungrab() {
            Ok(()) => {
                self.grabbed = false;
                self.cancel_combo_unconditional();
                self.held_keys.clear();
                self.reset_triggers();
                info!("Released gamepad grab");
            }
            Err(e) => error!("Failed to ungrab gamepad: {e}"),
        }
    }

    fn try_connect(&mut self, gamepad: &mut Option<EventStream>) {
        let Some(handle) = device::find_gamepad(&self.db) else {
            return;
        };
        info!(
            "Found gamepad: {} at {}",
            handle.name,
            handle.path.display()
        );
        self.calibrate(&handle.device);
        match handle.device.into_event_stream() {
            Ok(stream) => {
                *gamepad = Some(stream);
                self.connected = true;
                // Auto-grab on connect (matches the Python device loop).
                self.do_grab(gamepad);
                self.publish(Event::ControllerWake);
            }
            Err(e) => error!("Failed to open event stream: {e}"),
        }
    }

    fn on_disconnect(&mut self, gamepad: &mut Option<EventStream>) {
        warn!("Gamepad disconnected, will reconnect...");
        self.publish(Event::ControllerDisconnected);
        self.reset_stick_state();
        *gamepad = None;
        self.connected = false;
        self.grabbed = false;
    }

    fn calibrate(&mut self, device: &evdev::Device) {
        let Ok(absinfo) = device.get_absinfo() else {
            return;
        };
        for (axis, info) in absinfo {
            let center = (info.minimum() + info.maximum()) / 2;
            let half_range = (info.maximum() - info.minimum()) / 2;
            let threshold = (half_range as f64 * config::STICK_DEADZONE) as i32;
            match axis.0 {
                cfg::ABS_X => {
                    self.stick_center_x = center;
                    self.stick_threshold_x = threshold;
                }
                cfg::ABS_Y => {
                    self.stick_center_y = center;
                    self.stick_threshold_y = threshold;
                }
                cfg::ABS_RX => {
                    self.rstick_center_x = center;
                    self.rstick_threshold_x = threshold;
                    self.rstick_half_range_x = half_range;
                }
                cfg::ABS_RY => {
                    self.rstick_center_y = center;
                    self.rstick_threshold_y = threshold;
                    self.rstick_half_range_y = half_range;
                }
                _ => {}
            }
        }
        info!(
            "Stick calibration: X center={} threshold={}, Y center={} threshold={}",
            self.stick_center_x,
            self.stick_threshold_x,
            self.stick_center_y,
            self.stick_threshold_y
        );
    }

    fn reset_triggers(&mut self) {
        self.left_trigger_held = false;
        self.right_trigger_held = false;
    }

    fn reset_stick_state(&mut self) {
        if let Some(key) = self.stick_x_key.take() {
            self.emit_key(key, 0);
        }
        self.cancel_stick_repeat_x();
        if let Some(key) = self.stick_y_key.take() {
            self.emit_key(key, 0);
        }
        self.cancel_stick_repeat_y();
        self.rstick_x_dir = None;
        self.rstick_y_dir = None;
        if let Some(t) = self.mouse_task.take() {
            t.abort();
        }
    }

    // --- internal (timer) handling ---------------------------------------

    fn handle_internal(&mut self, internal: Internal) {
        match internal {
            Internal::StickRepeat {
                axis,
                key,
                generation,
            } => {
                // Ignore stale ticks: a tick whose generation no longer matches
                // the axis's live repeat (e.g. the stick re-deflected the same
                // direction) would otherwise repeat without the initial delay.
                let (live_gen, live_key) = match axis {
                    Axis::X => (self.stick_x_gen, self.stick_x_key),
                    Axis::Y => (self.stick_y_gen, self.stick_y_key),
                };
                if generation == live_gen && live_key == Some(key) {
                    self.emit_key(key, 0);
                    self.emit_key(key, 1);
                }
            }
            Internal::MouseTick => {
                if !self.has_rstick_deflection() {
                    if let Some(t) = self.mouse_task.take() {
                        t.abort();
                    }
                    return;
                }
                let dx = state::compute_mouse_velocity(
                    self.rstick_raw_x,
                    self.rstick_center_x,
                    self.rstick_threshold_x,
                    self.rstick_half_range_x,
                );
                let dy = state::compute_mouse_velocity(
                    self.rstick_raw_y,
                    self.rstick_center_y,
                    self.rstick_threshold_y,
                    self.rstick_half_range_y,
                );
                self.emit_mouse_move(dx, dy);
            }
            Internal::HomeHoldFired(generation) => {
                if generation != self.home_hold_gen {
                    return; // stale: the button was already released/re-pressed
                }
                self.home_hold_task = None;
                info!("Home hold detected");
                self.publish(Event::ComboHomeHold);
            }
            Internal::ComboEndSessionFired(generation) => {
                if generation != self.combo_gen {
                    return; // stale: a prior combo timer that was cancelled
                }
                self.combo_task = None;
                if state::subset_held(&config::COMBO_KEYS, &self.held_keys) {
                    info!("End-session combo detected");
                    self.publish(Event::ComboEndSession);
                }
            }
            Internal::RoutedHoldFired(name, generation) => {
                if self.routed_hold_gen.get(name) != Some(&generation) {
                    return; // stale: a prior routed-hold that was released/replaced
                }
                self.routed_hold_fired.insert(name, true);
                info!("Routed {name} hold detected");
                self.publish(Event::ComboHomeHold);
            }
            Internal::CaptureTimeout(generation) => {
                if generation != self.capture_gen {
                    return; // stale: the capture was already resolved/cancelled
                }
                self.capture_timeout_task = None;
                if let Some(r) = self.pending_capture.take() {
                    let _ = r.send(resp_timeout());
                }
            }
            Internal::Kbd {
                code,
                value,
                raw_name,
            } => self.handle_kbd(code, value, raw_name),
        }
    }

    // --- gamepad event handling ------------------------------------------

    fn handle_gamepad(&mut self, ev: InputEvent) {
        if self.grabbed {
            self.handle_event(ev);
        } else {
            self.handle_event_ungrabbed(ev);
        }
    }

    fn handle_event(&mut self, ev: InputEvent) {
        let et = ev.event_type();
        let code = ev.code();
        let value = ev.value();

        if et == EventType::KEY {
            // Capture mode resolves on keydown of a remappable button.
            if self.pending_capture.is_some() && value == 1 {
                if config::is_remappable(code) {
                    self.resolve_capture(code);
                }
                return;
            }

            if value == 1 {
                self.held_keys.insert(code);
                self.check_combo_start();
                self.check_quit_combo();
                self.check_suspend_combo();
                self.notify_held_buttons();
                if code == cfg::BTN_TL || code == cfg::BTN_TR {
                    self.set_input_mode(InputMode::Mouse);
                } else if config::is_remappable(code) || code == cfg::BTN_SELECT {
                    self.set_input_mode(InputMode::Controller);
                }
            } else if value == 0 {
                self.held_keys.remove(&code);
                self.cancel_combo();
                self.notify_held_buttons();
            }

            // Home button hold detection.
            if code == cfg::BTN_MODE {
                if value == 1 {
                    self.start_home_hold();
                } else if value == 0 {
                    if let Some(t) = self.home_hold_task.take() {
                        t.abort();
                        // Invalidate a possibly-queued HomeHoldFired so the tap
                        // doesn't also produce combo:home-hold.
                        self.home_hold_gen = self.next_generation();
                        self.publish(Event::HomePress);
                    }
                }
            }

            // LB/RB -> mouse left/right click.
            if code == cfg::BTN_TL {
                self.emit_mouse_button(cfg::BTN_LEFT, value);
            } else if code == cfg::BTN_TR {
                self.emit_mouse_button(cfg::BTN_RIGHT, value);
            }

            // Map to keyboard.
            if let Some(&mapped) = self.button_map.get(&code) {
                self.emit_key(mapped, value);
            }
        } else if et == EventType::ABSOLUTE {
            self.handle_abs(code, value);
        }
    }

    fn handle_event_ungrabbed(&mut self, ev: InputEvent) {
        let et = ev.event_type();
        let code = ev.code();
        let value = ev.value();

        if et == EventType::ABSOLUTE {
            // Right-stick mouse cursor stays active when ungrabbed.
            if code == cfg::ABS_RX {
                self.handle_rstick_axis(value, Axis::X);
            } else if code == cfg::ABS_RY {
                self.handle_rstick_axis(value, Axis::Y);
            }
            return;
        }
        if et != EventType::KEY {
            return;
        }

        // LB/RB clicks stay active when ungrabbed.
        if code == cfg::BTN_TL {
            self.emit_mouse_button(cfg::BTN_LEFT, value);
        } else if code == cfg::BTN_TR {
            self.emit_mouse_button(cfg::BTN_RIGHT, value);
        }

        if value == 1 {
            self.held_keys.insert(code);
            self.check_combo_start();
            self.check_quit_combo();
            self.check_suspend_combo();
            self.notify_held_buttons();
            if code == cfg::BTN_TL || code == cfg::BTN_TR {
                self.set_input_mode(InputMode::Mouse);
            } else if config::is_remappable(code) || code == cfg::BTN_SELECT {
                self.set_input_mode(InputMode::Controller);
            }
        } else if value == 0 {
            self.held_keys.remove(&code);
            self.cancel_combo();
            self.notify_held_buttons();
        }
    }

    fn handle_abs(&mut self, code: u16, value: i32) {
        match code {
            cfg::ABS_HAT0X => {
                if value == -1 {
                    self.emit_key(cfg::KEY_LEFT, 1);
                    self.held_keys.insert(cfg::KEY_LEFT);
                } else if value == 1 {
                    self.emit_key(cfg::KEY_RIGHT, 1);
                    self.held_keys.insert(cfg::KEY_RIGHT);
                } else {
                    self.emit_key(cfg::KEY_LEFT, 0);
                    self.emit_key(cfg::KEY_RIGHT, 0);
                    self.held_keys.remove(&cfg::KEY_LEFT);
                    self.held_keys.remove(&cfg::KEY_RIGHT);
                }
                if value != 0 {
                    self.set_input_mode(InputMode::Controller);
                }
                self.notify_held_buttons();
            }
            cfg::ABS_HAT0Y => {
                if value == -1 {
                    self.emit_key(cfg::KEY_UP, 1);
                    self.held_keys.insert(cfg::KEY_UP);
                } else if value == 1 {
                    self.emit_key(cfg::KEY_DOWN, 1);
                    self.held_keys.insert(cfg::KEY_DOWN);
                } else {
                    self.emit_key(cfg::KEY_UP, 0);
                    self.emit_key(cfg::KEY_DOWN, 0);
                    self.held_keys.remove(&cfg::KEY_UP);
                    self.held_keys.remove(&cfg::KEY_DOWN);
                }
                if value != 0 {
                    self.set_input_mode(InputMode::Controller);
                }
                self.notify_held_buttons();
            }
            cfg::ABS_X => self.handle_stick_axis(value, Axis::X, cfg::KEY_LEFT, cfg::KEY_RIGHT),
            cfg::ABS_Y => self.handle_stick_axis(value, Axis::Y, cfg::KEY_UP, cfg::KEY_DOWN),
            cfg::ABS_RX => self.handle_rstick_axis(value, Axis::X),
            cfg::ABS_RY => self.handle_rstick_axis(value, Axis::Y),
            cfg::ABS_Z => {
                let was = self.left_trigger_held;
                self.left_trigger_held = value > 100;
                if was != self.left_trigger_held {
                    self.notify_held_buttons();
                }
            }
            cfg::ABS_RZ => {
                let was = self.right_trigger_held;
                self.right_trigger_held = value > 100;
                if was != self.right_trigger_held {
                    self.notify_held_buttons();
                }
            }
            _ => {}
        }
    }

    fn handle_stick_axis(&mut self, value: i32, axis: Axis, neg_key: u16, pos_key: u16) {
        let (center, threshold) = match axis {
            Axis::X => (self.stick_center_x, self.stick_threshold_x),
            Axis::Y => (self.stick_center_y, self.stick_threshold_y),
        };
        let new_key = state::left_stick_target(value, center, threshold, neg_key, pos_key);
        let current = match axis {
            Axis::X => self.stick_x_key,
            Axis::Y => self.stick_y_key,
        };
        if new_key == current {
            return;
        }
        if let Some(k) = current {
            self.emit_key(k, 0);
            self.cancel_stick_repeat(axis);
        }
        match axis {
            Axis::X => self.stick_x_key = new_key,
            Axis::Y => self.stick_y_key = new_key,
        }
        if let Some(k) = new_key {
            self.emit_key(k, 1);
            self.start_stick_repeat(axis, k);
            self.set_input_mode(InputMode::Controller);
        }
        self.notify_held_buttons();
    }

    fn handle_rstick_axis(&mut self, value: i32, axis: Axis) {
        match axis {
            Axis::X => {
                self.rstick_raw_x = value;
                let new_dir =
                    state::rstick_x_dir(value, self.rstick_center_x, self.rstick_threshold_x);
                if new_dir != self.rstick_x_dir {
                    let old = self.rstick_x_dir;
                    self.rstick_x_dir = new_dir;
                    if new_dir.is_some() && old.is_none() {
                        self.set_input_mode(InputMode::Mouse);
                    }
                    self.notify_held_buttons();
                }
            }
            Axis::Y => {
                self.rstick_raw_y = value;
                let new_dir =
                    state::rstick_y_dir(value, self.rstick_center_y, self.rstick_threshold_y);
                if new_dir != self.rstick_y_dir {
                    let old = self.rstick_y_dir;
                    self.rstick_y_dir = new_dir;
                    if new_dir.is_some() && old.is_none() {
                        self.set_input_mode(InputMode::Mouse);
                    }
                    self.notify_held_buttons();
                }
            }
        }

        if self.has_rstick_deflection() {
            let running = self.mouse_task.as_ref().is_some_and(|t| !t.is_finished());
            if !running {
                let tx = self.internal_tx.clone();
                self.mouse_task = Some(tokio::spawn(async move {
                    loop {
                        if tx.send(Internal::MouseTick).await.is_err() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(config::MOUSE_POLL_MS)).await;
                    }
                }));
            }
        }
    }

    fn has_rstick_deflection(&self) -> bool {
        self.rstick_x_dir.is_some() || self.rstick_y_dir.is_some()
    }

    fn start_stick_repeat(&mut self, axis: Axis, key: u16) {
        self.cancel_stick_repeat(axis);
        let generation = self.next_generation();
        match axis {
            Axis::X => self.stick_x_gen = generation,
            Axis::Y => self.stick_y_gen = generation,
        }
        let tx = self.internal_tx.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(config::STICK_INITIAL_DELAY_MS)).await;
            loop {
                if tx
                    .send(Internal::StickRepeat {
                        axis,
                        key,
                        generation,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(config::STICK_REPEAT_INTERVAL_MS)).await;
            }
        });
        match axis {
            Axis::X => self.stick_x_repeat = Some(handle),
            Axis::Y => self.stick_y_repeat = Some(handle),
        }
    }

    fn cancel_stick_repeat(&mut self, axis: Axis) {
        match axis {
            Axis::X => self.cancel_stick_repeat_x(),
            Axis::Y => self.cancel_stick_repeat_y(),
        }
    }

    fn cancel_stick_repeat_x(&mut self) {
        if let Some(t) = self.stick_x_repeat.take() {
            t.abort();
            self.stick_x_gen = self.next_generation();
        }
    }

    fn cancel_stick_repeat_y(&mut self) {
        if let Some(t) = self.stick_y_repeat.take() {
            t.abort();
            self.stick_y_gen = self.next_generation();
        }
    }

    // --- combos -----------------------------------------------------------

    fn check_combo_start(&mut self) {
        if state::subset_held(&config::COMBO_KEYS, &self.held_keys) && self.combo_task.is_none() {
            let generation = self.next_generation();
            self.combo_gen = generation;
            let tx = self.internal_tx.clone();
            self.combo_task = Some(tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs_f64(config::COMBO_HOLD_SECS)).await;
                let _ = tx.send(Internal::ComboEndSessionFired(generation)).await;
            }));
        }
    }

    fn cancel_combo(&mut self) {
        if self.combo_task.is_some() && !state::subset_held(&config::COMBO_KEYS, &self.held_keys) {
            if let Some(t) = self.combo_task.take() {
                t.abort();
                self.combo_gen = self.next_generation();
            }
        }
    }

    fn cancel_combo_unconditional(&mut self) {
        if let Some(t) = self.combo_task.take() {
            t.abort();
            self.combo_gen = self.next_generation();
        }
    }

    fn check_quit_combo(&mut self) {
        if state::subset_held(&config::QUIT_COMBO_KEYS, &self.held_keys) {
            info!("Force-quit combo detected (Back+Home+LB+RB)");
            if !self.grabbed {
                self.send_moonlight_quit();
            }
            self.publish(Event::ComboForceQuit);
        }
    }

    fn check_suspend_combo(&mut self) {
        if state::subset_held(&config::SUSPEND_COMBO_KEYS, &self.held_keys)
            && !state::subset_held(&config::QUIT_COMBO_KEYS, &self.held_keys)
        {
            info!("Suspend combo detected (LB+RB+Start)");
            self.publish(Event::ComboSuspendStream);
        }
    }

    fn start_home_hold(&mut self) {
        if let Some(t) = self.home_hold_task.take() {
            t.abort();
        }
        let generation = self.next_generation();
        self.home_hold_gen = generation;
        let tx = self.internal_tx.clone();
        self.home_hold_task = Some(tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs_f64(config::HOME_HOLD_SECS)).await;
            let _ = tx.send(Internal::HomeHoldFired(generation)).await;
        }));
    }

    // --- keyboard snoop ---------------------------------------------------

    fn handle_kbd(&mut self, code: u16, value: i32, raw_name: Option<String>) {
        if self.kbd_log_enabled && value == 1 {
            let (raw, disp, source) = config::kbd_key_info(code, raw_name.as_deref());
            // The local is `disp`, not `display`: a `display` identifier collides
            // with `tracing::field::display` inside the `info!` macro expansion.
            info!(
                "kbd-key code={} raw={} display={:?} source={}",
                code,
                raw,
                disp,
                source.as_str()
            );
        }

        // Route Meta (Super) through the same tap-vs-hold flow as BTN_MODE.
        if code == cfg::KEY_LEFTMETA || code == cfg::KEY_RIGHTMETA {
            if value == 1 {
                self.start_routed_hold("meta");
            } else if value == 0 {
                self.resolve_routed_release("meta");
            }
        } else if value == 1 && !self.routed_hold_tasks.is_empty() {
            let names: Vec<&'static str> = self.routed_hold_tasks.keys().copied().collect();
            for name in names {
                self.routed_chord_seen.insert(name, true);
            }
        }

        let mut changed = false;
        if value == 1 && !self.kbd_held_keys.contains(&code) {
            self.kbd_held_keys.insert(code);
            changed = true;
        } else if value == 0 && self.kbd_held_keys.contains(&code) {
            self.kbd_held_keys.remove(&code);
            changed = true;
        }
        if changed {
            self.notify_held_keys();
        }
    }

    fn start_routed_hold(&mut self, name: &'static str) {
        if let Some(prev) = self.routed_hold_tasks.remove(name) {
            prev.abort();
        }
        self.routed_hold_fired.insert(name, false);
        self.routed_chord_seen.insert(name, false);
        // ROUTED_HOME_KEYS is currently just {"meta"}.
        if name == "meta" {
            let generation = self.next_generation();
            self.routed_hold_gen.insert(name, generation);
            let tx = self.internal_tx.clone();
            let handle = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs_f64(config::HOME_HOLD_SECS)).await;
                let _ = tx.send(Internal::RoutedHoldFired(name, generation)).await;
            });
            self.routed_hold_tasks.insert(name, handle);
        }
    }

    fn resolve_routed_release(&mut self, name: &'static str) {
        if let Some(task) = self.routed_hold_tasks.remove(name) {
            task.abort();
        }
        // Invalidate a possibly-queued RoutedHoldFired for this key.
        let generation = self.next_generation();
        self.routed_hold_gen.insert(name, generation);
        let fired = self.routed_hold_fired.remove(name).unwrap_or(false);
        let chord = self.routed_chord_seen.remove(name).unwrap_or(false);
        if name == "meta" && !fired && !chord {
            info!("Routed {name} tap detected");
            self.publish(Event::HomePress);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    X,
    Y,
}

/// Discover keyboards and forward their key events to the input loop, without
/// grabbing (so focused apps still receive every keystroke). Mirrors Python's
/// `_keyboard_loop`: re-discover every 2s; when any reader errors, drop them
/// all and rediscover.
async fn keyboard_supervisor(tx: mpsc::Sender<Internal>) {
    loop {
        let keyboards: Vec<GamepadHandle> = device::find_keyboards();
        if keyboards.is_empty() {
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        info!("Watching {} keyboard device(s)", keyboards.len());

        let mut readers = Vec::new();
        for handle in keyboards {
            match handle.device.into_event_stream() {
                Ok(stream) => readers.push(Box::pin(read_keyboard(stream, tx.clone()))),
                Err(e) => warn!("could not open keyboard stream: {e}"),
            }
        }
        if readers.is_empty() {
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        // Resolve when the first reader ends (error/disconnect); drop the rest
        // and rediscover after a short delay.
        let _ = futures::future::select_all(readers).await;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn read_keyboard(mut stream: EventStream, tx: mpsc::Sender<Internal>) {
    loop {
        match stream.next_event().await {
            Ok(ev) => {
                if ev.event_type() == EventType::KEY {
                    let code = ev.code();
                    let value = ev.value();
                    let raw_name = Some(format!("{:?}", KeyCode::new(code)));
                    if tx
                        .send(Internal::Kbd {
                            code,
                            value,
                            raw_name,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
            Err(_) => return,
        }
    }
}
