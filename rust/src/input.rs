//! Linux input runtime: the single owner of all mutable daemon state.
//!
//! Runs on its own OS thread (a current-thread tokio runtime, set up in
//! `main.rs`) so real-time input timing — 60 Hz mouse, stick auto-repeat,
//! hold/combo timers — stays off the IPC server's scheduler. The IPC server
//! messages this runtime over the `Control` channel; timers are spawned tasks
//! that post `Internal` messages back to the one select loop, which keeps state
//! mutation single-owner (no `Arc<Mutex>` across `.await`).
//!
//! ## Fleet model (Phase 4)
//!
//! The runtime owns a [`Fleet`] of physical pads keyed by raw fd. Each
//! [`PadDevice`] carries its own input state — held buttons, stick calibration,
//! per-pad timers, a stable player slot (#101) — so multiple pads run
//! independently. Shared resources (the virtual uinput keyboard/mouse, the
//! broadcast bus, the controller DB, the remap table, the capture state) live in
//! [`Shared`], borrowed by every per-pad method.
//!
//! Combo / Home-hold detection is **per-pad-complete** (`[critic P1]`): a combo
//! fires only when a *single* pad holds the full key set. The fleet-level timers
//! check, at fire time, whether *any* pad still holds the complete combo, so two
//! pads pressing Home simultaneously produce one `combo:home-hold`, never a
//! double-fire, and two pads each holding *half* a combo never trigger it. For a
//! single connected pad this is byte-identical to the pre-fleet behavior.
//!
//! This is a faithful port of `input/gamepad-input.py`. The wire-facing strings
//! and pure decision logic live in `protocol`/`config`/`state` (and are tested
//! on any host); this module is the evdev/uinput glue, exercised by CI on Linux
//! and on the target device.

use crate::config as cfg;
use crate::config::{self, Binding};
use crate::device::{self, ControllerDb, SlotAllocator, VirtualRegistry};
use crate::protocol::{
    is_known_intent, pad_connected_json, resp_cancelled, resp_captured, resp_invalid_button,
    resp_ok, resp_pads, resp_status, resp_timeout, resp_unknown_action, resp_unknown_intent, Event,
    InputMode,
};
use crate::state::{self, Control, Reply};
use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventStream, EventType, InputEvent, KeyCode, RelativeAxisCode};
use std::collections::{HashMap, HashSet};
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

const EV_KEY: u16 = config::EV_KEY;
const EV_REL: u16 = config::EV_REL;

/// Messages posted back into the select loop by timer/reader tasks.
///
/// Each timer message carries a `gen` (generation) token, and the pad-scoped
/// ones also carry the originating pad's `fd`. A timer task may already have sent
/// its message into the channel by the time it is aborted (e.g. a hold fires the
/// same instant the button is released); the handler ignores any message whose
/// `gen` no longer matches the live generation for that timer slot — or whose
/// `fd` is no longer in the fleet — so a stale tick can't double-fire or
/// mis-attribute to a later press or a different pad.
#[derive(Debug)]
enum Internal {
    /// The given left-stick arrow key is repeating on pad `fd`: emit up+down.
    StickRepeat {
        fd: RawFd,
        axis: Axis,
        key: u16,
        generation: u64,
    },
    /// 60 Hz mouse poll tick for pad `fd`.
    MouseTick { fd: RawFd },
    /// Home button held past the threshold on pad `fd`.
    HomeHoldFired { fd: RawFd, generation: u64 },
    /// Home + B held past the threshold on pad `fd`.
    ComboEndSessionFired { fd: RawFd, generation: u64 },
    /// A pending `capture-next` timed out (fleet-level).
    CaptureTimeout(u64),
}

/// Resources shared across every pad in the fleet: the virtual output devices,
/// the broadcast bus, identity tables, the remap map, and the fleet-level
/// capture/input-mode/generation state. Per-pad input state lives in
/// [`PadDevice`]; methods there borrow this for emission and publishing.
struct Shared {
    events: broadcast::Sender<Event>,
    internal_tx: mpsc::Sender<Internal>,
    kb: VirtualDevice,
    mouse: VirtualDevice,
    db: ControllerDb,
    /// fds of the uinput devices we own, so discovery never re-grabs them.
    reg: VirtualRegistry,

    bindings: Vec<Binding>,
    button_map: HashMap<u16, u16>,

    /// Fleet-aggregate input mode (any pad's most recent mode change). One
    /// source among several once #45 lands; here it's the gamepad source.
    input_mode: InputMode,

    // Monotonic generation allocator for timer messages (see `Internal`).
    gen_seq: u64,

    // Capture (keybinding reassignment) — fleet-level: the next remappable
    // press on ANY grabbed pad resolves it.
    pending_capture: Option<Reply>,
    capture_timeout_task: Option<JoinHandle<()>>,
    capture_gen: u64,

    /// Fleet-level Home-hold dedup latch. Set when any pad's Home-hold timer
    /// fires; `ComboHomeHold` is published only on the 0->1 edge, so two pads
    /// holding Home simultaneously emit the single `combo:home-hold` once
    /// (followup: "simultaneous Home-hold fires once"). Cleared when no pad
    /// holds `BTN_MODE` anymore. Single-pad: the latch toggles exactly with the
    /// one pad, so behavior is identical to the pre-fleet single fire.
    home_hold_active: bool,
}

impl Shared {
    fn publish(&self, ev: Event) {
        let _ = self.events.send(ev);
    }

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

    fn set_input_mode(&mut self, mode: InputMode) {
        if mode == self.input_mode {
            return;
        }
        self.input_mode = mode;
        self.publish(Event::InputMode(mode));
    }

    /// Allocate the next monotonic generation token for a timer.
    fn next_generation(&mut self) -> u64 {
        self.gen_seq += 1;
        self.gen_seq
    }

    fn rebuild_button_map(&mut self) {
        self.button_map.clear();
        for b in &self.bindings {
            self.button_map.insert(b.button, b.key);
        }
    }
}

/// One physical pad in the fleet: its grabbed event stream, stable player slot,
/// and all per-pad input state. The fd (the event stream's raw fd) is the
/// in-process key; `wire_id` is the stable cross-reconnect id used in `pad:*`
/// payloads.
struct PadDevice {
    fd: RawFd,
    event_stream: EventStream,
    wire_id: String,
    name: String,
    path: PathBuf,
    player_slot: u8,
    grabbed: bool,
    connected: bool,

    held_keys: HashSet<u16>,
    left_trigger_held: bool,
    right_trigger_held: bool,

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

    // Hold/combo timers (per-pad: each pad detects its own complete combo).
    home_hold_task: Option<JoinHandle<()>>,
    home_hold_gen: u64,
    combo_task: Option<JoinHandle<()>>,
    combo_gen: u64,

    // --- Fleet outputs (ride-along, Phase 5.5) ---
    /// One clean virtual gamepad per player in game-presenter mode (Phase 5).
    /// `None` in shell mode. Registered in `Shared.reg` at creation, dropped +
    /// unregistered on leave.
    virtual_pad: Option<VirtualDevice>,
    /// Latest battery snapshot (#100), if the pad exposes one. Populated by the
    /// Phase 5.5 UPower/sysfs ride-along; the slot is reserved here so the fleet
    /// struct is final for Phase 4.
    #[allow(dead_code)] // wired in Phase 5.5 (#100); field reserved in Phase 4
    battery: Option<BatteryState>,
    /// Player LED index assigned at slot allocation (#101), if `EV_LED`-capable.
    /// Set by the Phase 5.5 LED ride-along; reserved here.
    #[allow(dead_code)] // wired in Phase 5.5 (#101 LED); field reserved in Phase 4
    led_index: Option<u8>,
}

/// Battery snapshot for a pad (#100). Populated by the Phase 5.5 UPower/sysfs
/// ride-along; `None` battery on a pad means "no reported battery". The fields
/// are read when the ride-along emits `pad:battery:{level,charging}`.
#[allow(dead_code)] // fields read by the Phase 5.5 battery ride-along (#100)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BatteryState {
    /// Charge percentage 0..=100.
    level: u8,
    charging: bool,
}

impl PadDevice {
    fn new(
        fd: RawFd,
        stream: EventStream,
        wire_id: String,
        name: String,
        path: PathBuf,
        slot: u8,
    ) -> PadDevice {
        PadDevice {
            fd,
            event_stream: stream,
            wire_id,
            name,
            path,
            player_slot: slot,
            grabbed: false,
            connected: true,
            held_keys: HashSet::new(),
            left_trigger_held: false,
            right_trigger_held: false,
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
            virtual_pad: None,
            battery: None,
            led_index: None,
        }
    }

    // --- held-buttons notification --------------------------------------

    fn notify_held_buttons(&self, sh: &Shared) {
        if sh.events.receiver_count() == 0 {
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
        sh.publish(Event::Buttons(payload));
    }

    // --- grab lifecycle --------------------------------------------------

    fn grab(&mut self, sh: &mut Shared) {
        if self.grabbed {
            return;
        }
        match self.event_stream.device_mut().grab() {
            Ok(()) => {
                self.grabbed = true;
                self.cancel_combo_unconditional(sh);
                self.held_keys.clear();
                self.reset_triggers();
                sh.set_input_mode(InputMode::Controller);
                info!("Grabbed pad slot {} ({})", self.player_slot, self.wire_id);
            }
            Err(e) => error!("Failed to grab pad {}: {e}", self.wire_id),
        }
    }

    fn ungrab(&mut self, sh: &mut Shared) {
        self.reset_stick_state(sh);
        if !self.grabbed {
            return;
        }
        match self.event_stream.device_mut().ungrab() {
            Ok(()) => {
                self.grabbed = false;
                self.cancel_combo_unconditional(sh);
                self.held_keys.clear();
                self.reset_triggers();
                info!("Released pad slot {} ({})", self.player_slot, self.wire_id);
            }
            Err(e) => error!("Failed to ungrab pad {}: {e}", self.wire_id),
        }
    }

    fn calibrate(&mut self) {
        let Ok(absinfo) = self.event_stream.device().get_absinfo() else {
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
            "Pad slot {} calibration: X center={} threshold={}, Y center={} threshold={}",
            self.player_slot,
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

    fn reset_stick_state(&mut self, sh: &mut Shared) {
        if let Some(key) = self.stick_x_key.take() {
            sh.emit_key(key, 0);
        }
        self.cancel_stick_repeat_x(sh);
        if let Some(key) = self.stick_y_key.take() {
            sh.emit_key(key, 0);
        }
        self.cancel_stick_repeat_y(sh);
        self.rstick_x_dir = None;
        self.rstick_y_dir = None;
        if let Some(t) = self.mouse_task.take() {
            t.abort();
        }
    }

    /// Abort every spawned task this pad owns (stick repeats, mouse poll,
    /// hold/combo timers). Called on leave so a dropped pad leaves no orphan
    /// tasks posting `Internal` messages for a fd that's gone.
    fn abort_all_tasks(&mut self) {
        if let Some(t) = self.stick_x_repeat.take() {
            t.abort();
        }
        if let Some(t) = self.stick_y_repeat.take() {
            t.abort();
        }
        if let Some(t) = self.mouse_task.take() {
            t.abort();
        }
        if let Some(t) = self.home_hold_task.take() {
            t.abort();
        }
        if let Some(t) = self.combo_task.take() {
            t.abort();
        }
    }

    // --- event handling --------------------------------------------------

    fn handle_event(&mut self, sh: &mut Shared, ev: InputEvent) {
        if self.grabbed {
            self.handle_grabbed(sh, ev);
        } else {
            self.handle_ungrabbed(sh, ev);
        }
    }

    fn handle_grabbed(&mut self, sh: &mut Shared, ev: InputEvent) {
        let et = ev.event_type();
        let code = ev.code();
        let value = ev.value();

        if et == EventType::KEY {
            // Capture mode resolves on keydown of a remappable button (any pad).
            if sh.pending_capture.is_some() && value == 1 {
                if config::is_remappable(code) {
                    resolve_capture(sh, code);
                }
                return;
            }

            if value == 1 {
                self.held_keys.insert(code);
                self.check_combo_start(sh);
                self.check_quit_combo(sh);
                self.check_suspend_combo(sh);
                self.notify_held_buttons(sh);
                if code == cfg::BTN_TL || code == cfg::BTN_TR {
                    sh.set_input_mode(InputMode::Mouse);
                } else if config::is_remappable(code) || code == cfg::BTN_SELECT {
                    sh.set_input_mode(InputMode::Controller);
                }
            } else if value == 0 {
                self.held_keys.remove(&code);
                self.cancel_combo(sh);
                self.notify_held_buttons(sh);
            }

            // Home button hold detection.
            if code == cfg::BTN_MODE {
                if value == 1 {
                    self.start_home_hold(sh);
                } else if value == 0 {
                    if let Some(t) = self.home_hold_task.take() {
                        t.abort();
                        // Invalidate a possibly-queued HomeHoldFired so the tap
                        // doesn't also produce combo:home-hold.
                        self.home_hold_gen = sh.next_generation();
                        sh.publish(Event::HomePress);
                    }
                }
            }

            // LB/RB -> mouse left/right click.
            if code == cfg::BTN_TL {
                sh.emit_mouse_button(cfg::BTN_LEFT, value);
            } else if code == cfg::BTN_TR {
                sh.emit_mouse_button(cfg::BTN_RIGHT, value);
            }

            // Map to keyboard.
            if let Some(&mapped) = sh.button_map.get(&code) {
                sh.emit_key(mapped, value);
            }
        } else if et == EventType::ABSOLUTE {
            self.handle_abs(sh, code, value);
        }
    }

    fn handle_ungrabbed(&mut self, sh: &mut Shared, ev: InputEvent) {
        let et = ev.event_type();
        let code = ev.code();
        let value = ev.value();

        if et == EventType::ABSOLUTE {
            // Right-stick mouse cursor stays active when ungrabbed.
            if code == cfg::ABS_RX {
                self.handle_rstick_axis(sh, value, Axis::X);
            } else if code == cfg::ABS_RY {
                self.handle_rstick_axis(sh, value, Axis::Y);
            }
            return;
        }
        if et != EventType::KEY {
            return;
        }

        // LB/RB clicks stay active when ungrabbed.
        if code == cfg::BTN_TL {
            sh.emit_mouse_button(cfg::BTN_LEFT, value);
        } else if code == cfg::BTN_TR {
            sh.emit_mouse_button(cfg::BTN_RIGHT, value);
        }

        if value == 1 {
            self.held_keys.insert(code);
            self.check_combo_start(sh);
            self.check_quit_combo(sh);
            self.check_suspend_combo(sh);
            self.notify_held_buttons(sh);
            if code == cfg::BTN_TL || code == cfg::BTN_TR {
                sh.set_input_mode(InputMode::Mouse);
            } else if config::is_remappable(code) || code == cfg::BTN_SELECT {
                sh.set_input_mode(InputMode::Controller);
            }
        } else if value == 0 {
            self.held_keys.remove(&code);
            self.cancel_combo(sh);
            self.notify_held_buttons(sh);
        }
    }

    fn handle_abs(&mut self, sh: &mut Shared, code: u16, value: i32) {
        match code {
            cfg::ABS_HAT0X => {
                if value == -1 {
                    sh.emit_key(cfg::KEY_LEFT, 1);
                    self.held_keys.insert(cfg::KEY_LEFT);
                } else if value == 1 {
                    sh.emit_key(cfg::KEY_RIGHT, 1);
                    self.held_keys.insert(cfg::KEY_RIGHT);
                } else {
                    sh.emit_key(cfg::KEY_LEFT, 0);
                    sh.emit_key(cfg::KEY_RIGHT, 0);
                    self.held_keys.remove(&cfg::KEY_LEFT);
                    self.held_keys.remove(&cfg::KEY_RIGHT);
                }
                if value != 0 {
                    sh.set_input_mode(InputMode::Controller);
                }
                self.notify_held_buttons(sh);
            }
            cfg::ABS_HAT0Y => {
                if value == -1 {
                    sh.emit_key(cfg::KEY_UP, 1);
                    self.held_keys.insert(cfg::KEY_UP);
                } else if value == 1 {
                    sh.emit_key(cfg::KEY_DOWN, 1);
                    self.held_keys.insert(cfg::KEY_DOWN);
                } else {
                    sh.emit_key(cfg::KEY_UP, 0);
                    sh.emit_key(cfg::KEY_DOWN, 0);
                    self.held_keys.remove(&cfg::KEY_UP);
                    self.held_keys.remove(&cfg::KEY_DOWN);
                }
                if value != 0 {
                    sh.set_input_mode(InputMode::Controller);
                }
                self.notify_held_buttons(sh);
            }
            cfg::ABS_X => self.handle_stick_axis(sh, value, Axis::X, cfg::KEY_LEFT, cfg::KEY_RIGHT),
            cfg::ABS_Y => self.handle_stick_axis(sh, value, Axis::Y, cfg::KEY_UP, cfg::KEY_DOWN),
            cfg::ABS_RX => self.handle_rstick_axis(sh, value, Axis::X),
            cfg::ABS_RY => self.handle_rstick_axis(sh, value, Axis::Y),
            cfg::ABS_Z => {
                let was = self.left_trigger_held;
                self.left_trigger_held = value > 100;
                if was != self.left_trigger_held {
                    self.notify_held_buttons(sh);
                }
            }
            cfg::ABS_RZ => {
                let was = self.right_trigger_held;
                self.right_trigger_held = value > 100;
                if was != self.right_trigger_held {
                    self.notify_held_buttons(sh);
                }
            }
            _ => {}
        }
    }

    fn handle_stick_axis(
        &mut self,
        sh: &mut Shared,
        value: i32,
        axis: Axis,
        neg_key: u16,
        pos_key: u16,
    ) {
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
            sh.emit_key(k, 0);
            self.cancel_stick_repeat(sh, axis);
        }
        match axis {
            Axis::X => self.stick_x_key = new_key,
            Axis::Y => self.stick_y_key = new_key,
        }
        if let Some(k) = new_key {
            sh.emit_key(k, 1);
            self.start_stick_repeat(sh, axis, k);
            sh.set_input_mode(InputMode::Controller);
        }
        self.notify_held_buttons(sh);
    }

    fn handle_rstick_axis(&mut self, sh: &mut Shared, value: i32, axis: Axis) {
        match axis {
            Axis::X => {
                self.rstick_raw_x = value;
                let new_dir =
                    state::rstick_x_dir(value, self.rstick_center_x, self.rstick_threshold_x);
                if new_dir != self.rstick_x_dir {
                    let old = self.rstick_x_dir;
                    self.rstick_x_dir = new_dir;
                    if new_dir.is_some() && old.is_none() {
                        sh.set_input_mode(InputMode::Mouse);
                    }
                    self.notify_held_buttons(sh);
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
                        sh.set_input_mode(InputMode::Mouse);
                    }
                    self.notify_held_buttons(sh);
                }
            }
        }

        if self.has_rstick_deflection() {
            let running = self.mouse_task.as_ref().is_some_and(|t| !t.is_finished());
            if !running {
                let tx = sh.internal_tx.clone();
                let fd = self.fd;
                self.mouse_task = Some(tokio::spawn(async move {
                    loop {
                        if tx.send(Internal::MouseTick { fd }).await.is_err() {
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

    /// Compute and emit one mouse-poll tick. Returns false if the deflection has
    /// ended (the caller aborts the poll task).
    fn mouse_tick(&mut self, sh: &mut Shared) -> bool {
        if !self.has_rstick_deflection() {
            if let Some(t) = self.mouse_task.take() {
                t.abort();
            }
            return false;
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
        sh.emit_mouse_move(dx, dy);
        true
    }

    fn start_stick_repeat(&mut self, sh: &mut Shared, axis: Axis, key: u16) {
        self.cancel_stick_repeat(sh, axis);
        let generation = sh.next_generation();
        match axis {
            Axis::X => self.stick_x_gen = generation,
            Axis::Y => self.stick_y_gen = generation,
        }
        let tx = sh.internal_tx.clone();
        let fd = self.fd;
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(config::STICK_INITIAL_DELAY_MS)).await;
            loop {
                if tx
                    .send(Internal::StickRepeat {
                        fd,
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

    fn cancel_stick_repeat(&mut self, sh: &mut Shared, axis: Axis) {
        match axis {
            Axis::X => self.cancel_stick_repeat_x(sh),
            Axis::Y => self.cancel_stick_repeat_y(sh),
        }
    }

    fn cancel_stick_repeat_x(&mut self, sh: &mut Shared) {
        if let Some(t) = self.stick_x_repeat.take() {
            t.abort();
            self.stick_x_gen = sh.next_generation();
        }
    }

    fn cancel_stick_repeat_y(&mut self, sh: &mut Shared) {
        if let Some(t) = self.stick_y_repeat.take() {
            t.abort();
            self.stick_y_gen = sh.next_generation();
        }
    }

    // --- combos (per-pad-complete; see module docs [critic P1]) ----------

    fn check_combo_start(&mut self, sh: &mut Shared) {
        if state::subset_held(&config::COMBO_KEYS, &self.held_keys) && self.combo_task.is_none() {
            let generation = sh.next_generation();
            self.combo_gen = generation;
            let tx = sh.internal_tx.clone();
            let fd = self.fd;
            self.combo_task = Some(tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs_f64(config::COMBO_HOLD_SECS)).await;
                let _ = tx
                    .send(Internal::ComboEndSessionFired { fd, generation })
                    .await;
            }));
        }
    }

    fn cancel_combo(&mut self, sh: &mut Shared) {
        if self.combo_task.is_some() && !state::subset_held(&config::COMBO_KEYS, &self.held_keys) {
            if let Some(t) = self.combo_task.take() {
                t.abort();
                self.combo_gen = sh.next_generation();
            }
        }
    }

    fn cancel_combo_unconditional(&mut self, sh: &mut Shared) {
        if let Some(t) = self.combo_task.take() {
            t.abort();
            self.combo_gen = sh.next_generation();
        }
    }

    fn check_quit_combo(&mut self, sh: &mut Shared) {
        if state::subset_held(&config::QUIT_COMBO_KEYS, &self.held_keys) {
            info!("Force-quit combo detected (Back+Home+LB+RB)");
            if !self.grabbed {
                sh.send_moonlight_quit();
            }
            sh.publish(Event::ComboForceQuit);
        }
    }

    fn check_suspend_combo(&mut self, sh: &mut Shared) {
        if state::subset_held(&config::SUSPEND_COMBO_KEYS, &self.held_keys)
            && !state::subset_held(&config::QUIT_COMBO_KEYS, &self.held_keys)
        {
            info!("Suspend combo detected (LB+RB+Start)");
            sh.publish(Event::ComboSuspendStream);
        }
    }

    fn start_home_hold(&mut self, sh: &mut Shared) {
        if let Some(t) = self.home_hold_task.take() {
            t.abort();
        }
        let generation = sh.next_generation();
        self.home_hold_gen = generation;
        let tx = sh.internal_tx.clone();
        let fd = self.fd;
        self.home_hold_task = Some(tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs_f64(config::HOME_HOLD_SECS)).await;
            let _ = tx.send(Internal::HomeHoldFired { fd, generation }).await;
        }));
    }
}

/// The gamepad fleet: physical pads keyed by raw fd, plus the stable player-slot
/// allocator (#101).
struct Fleet {
    pads: HashMap<RawFd, PadDevice>,
    slots: SlotAllocator,
}

impl Fleet {
    fn new() -> Fleet {
        Fleet {
            pads: HashMap::new(),
            slots: SlotAllocator::new(),
        }
    }

    /// Fleet aggregate for the `status` reply: connected if any pad is present,
    /// grabbed if any pad is grabbed. For a single pad this is byte-identical to
    /// the pre-fleet `resp_status(connected, grabbed)`.
    fn status_string(&self) -> String {
        let connected = !self.pads.is_empty();
        let grabbed = self.pads.values().any(|p| p.grabbed);
        resp_status(connected, grabbed)
    }

    /// True if any pad currently holds the Home (`BTN_MODE`) button. Used to
    /// clear the fleet-level Home-hold latch.
    fn any_holds_home(&self) -> bool {
        self.pads
            .values()
            .any(|p| p.held_keys.contains(&cfg::BTN_MODE))
    }

    /// The `get-pads` reply: one JSON object per pad in ascending player-slot
    /// order.
    fn pads_json(&self) -> String {
        let mut pads: Vec<&PadDevice> = self.pads.values().collect();
        pads.sort_by_key(|p| p.player_slot);
        let rows: Vec<(String, u8, String, bool)> = pads
            .iter()
            .map(|p| (p.wire_id.clone(), p.player_slot, p.name.clone(), p.grabbed))
            .collect();
        resp_pads(&rows)
    }
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

    // Track our own uinput devices by fd so discovery never re-grabs them
    // (replaces the old `is_synthetic` name match). Per-player virtual pads
    // created later (Phase 5) register here at creation, too.
    let mut reg = VirtualRegistry::new();
    reg.register(kb.as_raw_fd());
    reg.register(mouse.as_raw_fd());

    let db = device::load_db();
    if db.is_empty() {
        warn!("controller DB is empty; non-pinned discovery will reject all pads — set GAME_SHELL_GAMECONTROLLERDB or pin GAMEPAD_VENDOR/GAMEPAD_PRODUCT");
    } else {
        info!("controller DB loaded: {} known models", db.len());
    }

    let mut sh = Shared {
        events,
        internal_tx,
        kb,
        mouse,
        db,
        reg,
        bindings,
        button_map,
        input_mode: InputMode::Controller,
        gen_seq: 0,
        pending_capture: None,
        capture_timeout_task: None,
        capture_gen: 0,
        home_hold_active: false,
    };
    let mut fleet = Fleet::new();

    loop {
        tokio::select! {
            // Control commands from the IPC server.
            ctrl = control_rx.recv() => {
                match ctrl {
                    Some(c) => if !handle_control(&mut sh, &mut fleet, c) { break },
                    None => break,
                }
            }
            // Timer/reader callbacks.
            Some(internal) = internal_rx.recv() => {
                handle_internal(&mut sh, &mut fleet, internal);
            }
            // Live gamepad events from any pad in the fleet.
            Some((fd, res)) = next_fleet_event(&mut fleet), if !fleet.pads.is_empty() => {
                match res {
                    Ok(ev) => {
                        if let Some(pad) = fleet.pads.get_mut(&fd) {
                            pad.handle_event(&mut sh, ev);
                        }
                    }
                    Err(_) => on_pad_leave(&mut sh, &mut fleet, fd),
                }
                // Clear the fleet-level Home-hold latch once no pad holds Home,
                // re-arming the 0->1 edge for the next coordinated hold.
                if !fleet.any_holds_home() {
                    sh.home_hold_active = false;
                }
            }
            // Reconnect / hot-join poll: discover any new pad not already owned.
            _ = tokio::time::sleep(Duration::from_secs(2)) => {
                try_join(&mut sh, &mut fleet);
            }
        }
    }

    // Reset every pad's stick state and abort tasks on shutdown.
    let fds: Vec<RawFd> = fleet.pads.keys().copied().collect();
    for fd in fds {
        if let Some(mut pad) = fleet.pads.remove(&fd) {
            pad.reset_stick_state(&mut sh);
            pad.abort_all_tasks();
        }
    }
    info!("Shutting down");
}

/// Await the next event from any pad's stream, tagged with its fd. Pends forever
/// when the fleet is empty (the select arm is guarded on `!pads.is_empty()`).
async fn next_fleet_event(fleet: &mut Fleet) -> Option<(RawFd, std::io::Result<InputEvent>)> {
    use futures::stream::{FuturesUnordered, StreamExt};
    if fleet.pads.is_empty() {
        return std::future::pending().await;
    }
    // Race every pad's next_event; the first to resolve wins this tick. Each
    // future borrows one pad mutably and yields its fd alongside the result.
    let mut futs = FuturesUnordered::new();
    for (&fd, pad) in fleet.pads.iter_mut() {
        futs.push(async move { (fd, pad.event_stream.next_event().await) });
    }
    futs.next().await
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

// --- control handling ----------------------------------------------------

/// Returns false to stop the loop (shutdown).
fn handle_control(sh: &mut Shared, fleet: &mut Fleet, ctrl: Control) -> bool {
    match ctrl {
        Control::Grab(r) => {
            grab_all(sh, fleet);
            let _ = r.send(resp_ok());
        }
        Control::Release(r) => {
            release_all(sh, fleet);
            let _ = r.send(resp_ok());
        }
        Control::Status(r) => {
            let _ = r.send(fleet.status_string());
        }
        Control::GetPads(r) => {
            let _ = r.send(fleet.pads_json());
        }
        Control::GetBindings(r) => {
            let ordered: Vec<(String, String)> = sh
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
            let resp = do_set_binding(sh, &action, &button);
            let _ = reply.send(resp);
        }
        Control::CaptureNext(r) => arm_capture(sh, r),
        Control::CaptureCancel(r) => {
            cancel_capture(sh);
            let _ = r.send(resp_ok());
        }
        Control::Intent { name, reply } => {
            // Pure broadcast: validate against the closed vocabulary and, if
            // valid, re-emit `intent:<name>` to all subscribers. Touches no
            // device — the control surface for keyboard global-escape and
            // automation.
            if is_known_intent(&name) {
                sh.publish(Event::Intent(name));
                let _ = reply.send(resp_ok());
            } else {
                let _ = reply.send(resp_unknown_intent(&name));
            }
        }
        Control::Shutdown => return false,
    }
    true
}

/// Grab every connected pad (shell-presenter entry point). Per-fleet mode
/// toggle: the existing `grab` IPC now applies to the whole fleet.
fn grab_all(sh: &mut Shared, fleet: &mut Fleet) {
    for pad in fleet.pads.values_mut() {
        pad.grab(sh);
    }
}

/// Release every connected pad's grab (game-presenter entry point, Phase 5).
fn release_all(sh: &mut Shared, fleet: &mut Fleet) {
    for pad in fleet.pads.values_mut() {
        pad.ungrab(sh);
    }
}

fn do_set_binding(sh: &mut Shared, action: &str, button: &str) -> String {
    if !config::is_default_action(action) {
        return resp_unknown_action(action);
    }
    let Some(code) = config::button_name_to_code(button) else {
        return resp_invalid_button(button);
    };
    if !config::is_remappable(code) {
        return resp_invalid_button(button);
    }
    for b in sh.bindings.iter_mut() {
        if b.action == action {
            b.button = code;
        }
    }
    sh.rebuild_button_map();
    if let Err(e) = config::save_bindings(&config::settings_path(), &sh.bindings) {
        warn!("failed to save bindings: {e}");
    }
    resp_ok()
}

// --- capture (fleet-level) -----------------------------------------------

fn arm_capture(sh: &mut Shared, reply: Reply) {
    // Replacing a pending capture cancels the previous one.
    if let Some(old) = sh.pending_capture.take() {
        let _ = old.send(resp_cancelled());
    }
    if let Some(t) = sh.capture_timeout_task.take() {
        t.abort();
    }
    let generation = sh.next_generation();
    sh.capture_gen = generation;
    sh.pending_capture = Some(reply);
    let tx = sh.internal_tx.clone();
    sh.capture_timeout_task = Some(tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(config::CAPTURE_TIMEOUT_SECS)).await;
        let _ = tx.send(Internal::CaptureTimeout(generation)).await;
    }));
}

fn resolve_capture(sh: &mut Shared, code: u16) {
    if let Some(r) = sh.pending_capture.take() {
        let _ = r.send(resp_captured(&config::button_code_to_name(code)));
    }
    if let Some(t) = sh.capture_timeout_task.take() {
        t.abort();
    }
    // Invalidate any already-queued timeout from the resolved capture.
    sh.capture_gen = sh.next_generation();
}

fn cancel_capture(sh: &mut Shared) {
    if let Some(r) = sh.pending_capture.take() {
        let _ = r.send(resp_cancelled());
    }
    if let Some(t) = sh.capture_timeout_task.take() {
        t.abort();
    }
    sh.capture_gen = sh.next_generation();
}

// --- hot-join / leave ----------------------------------------------------

/// Discover any newly-connected pads and add them to the fleet. Skips pads
/// already in the fleet (by **device path** — an already-grabbed pad
/// re-enumerates at the same path but a fresh fd) and our own virtual devices
/// (by fd, inside `find_gamepads`). Each joining pad is grabbed (shell
/// presenter), calibrated, assigned the lowest free slot, and announced via
/// `controller-wake` + `pad:connected:{id,index,name}`.
fn try_join(sh: &mut Shared, fleet: &mut Fleet) {
    // Paths already represented in the fleet, so a re-enumeration of a
    // connected pad doesn't open + grab it a second time.
    let known_paths: HashSet<PathBuf> = fleet.pads.values().map(|p| p.path.clone()).collect();

    for handle in device::find_gamepads(&sh.db, &sh.reg) {
        // Destructure before consuming `device` (into_event_stream moves it).
        let device::GamepadHandle {
            device,
            name,
            path,
            wire_id,
        } = handle;
        if known_paths.contains(&path) {
            continue; // already in the fleet
        }
        let stream = match device.into_event_stream() {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to open event stream for {}: {e}", path.display());
                continue;
            }
        };
        let fd = stream.as_raw_fd();
        // A freshly-opened physical pad's fd can't already be in the fleet;
        // guard anyway so a duplicate enumeration never double-inserts.
        if fleet.pads.contains_key(&fd) {
            continue;
        }
        let slot = fleet.slots.alloc();
        info!(
            "Pad joined: {} at {} (id={}, slot={})",
            name,
            path.display(),
            wire_id,
            slot,
        );
        let mut pad = PadDevice::new(fd, stream, wire_id.clone(), name.clone(), path, slot);
        pad.calibrate();
        // Auto-grab on connect (shell presenter; matches the Python device loop).
        pad.grab(sh);
        fleet.pads.insert(fd, pad);

        // Legacy single-pad signal (any pad still drives `controller-wake` so the
        // existing QML wake path is unchanged), plus the fleet-aware
        // `pad:connected:{id,index,name}`.
        sh.publish(Event::ControllerWake);
        sh.publish(Event::PadConnected(pad_connected_json(
            &wire_id, slot, &name,
        )));
    }
}

/// A pad's stream errored (USB disconnect): drop it from the fleet, free its
/// slot for reuse, abort its tasks, and announce the leave.
fn on_pad_leave(sh: &mut Shared, fleet: &mut Fleet, fd: RawFd) {
    let Some(mut pad) = fleet.pads.remove(&fd) else {
        return;
    };
    warn!(
        "Pad left: slot {} ({}), freeing slot",
        pad.player_slot, pad.wire_id
    );
    pad.reset_stick_state(sh);
    pad.abort_all_tasks();
    // Drop any per-player virtual pad and forget its fd (Phase 5 game presenter).
    if let Some(vpad) = pad.virtual_pad.take() {
        sh.reg.unregister(vpad.as_raw_fd());
    }
    let slot = pad.player_slot;
    let wire_id = pad.wire_id.clone();
    fleet.slots.free(slot);
    drop(pad);

    // Legacy single-pad signal + fleet-aware `pad:disconnected:{id}`.
    sh.publish(Event::ControllerDisconnected);
    sh.publish(Event::PadDisconnected(wire_id));
}

// --- internal (timer) handling -------------------------------------------

fn handle_internal(sh: &mut Shared, fleet: &mut Fleet, internal: Internal) {
    match internal {
        Internal::StickRepeat {
            fd,
            axis,
            key,
            generation,
        } => {
            let Some(pad) = fleet.pads.get_mut(&fd) else {
                return; // pad left
            };
            // Ignore stale ticks: a tick whose generation no longer matches the
            // axis's live repeat (e.g. the stick re-deflected the same
            // direction) would otherwise repeat without the initial delay.
            let (live_gen, live_key) = match axis {
                Axis::X => (pad.stick_x_gen, pad.stick_x_key),
                Axis::Y => (pad.stick_y_gen, pad.stick_y_key),
            };
            if generation == live_gen && live_key == Some(key) {
                sh.emit_key(key, 0);
                sh.emit_key(key, 1);
            }
        }
        Internal::MouseTick { fd } => {
            if let Some(pad) = fleet.pads.get_mut(&fd) {
                pad.mouse_tick(sh);
            }
        }
        Internal::HomeHoldFired { fd, generation } => {
            let Some(pad) = fleet.pads.get_mut(&fd) else {
                return; // pad left
            };
            if generation != pad.home_hold_gen {
                return; // stale: the button was already released/re-pressed
            }
            pad.home_hold_task = None;
            info!("Home hold detected (slot {})", pad.player_slot);
            // Fleet-level dedup: publish `combo:home-hold` only on the 0->1 edge
            // of the latch, so two pads holding Home at once fire it once. The
            // latch clears (loop event arm) when no pad holds Home. Single-pad:
            // the latch toggles with the one pad, identical to the old behavior.
            if !sh.home_hold_active {
                sh.home_hold_active = true;
                sh.publish(Event::ComboHomeHold);
            }
        }
        Internal::ComboEndSessionFired { fd, generation } => {
            let Some(pad) = fleet.pads.get_mut(&fd) else {
                return; // pad left
            };
            if generation != pad.combo_gen {
                return; // stale: a prior combo timer that was cancelled
            }
            pad.combo_task = None;
            // Per-pad-complete: this pad must still hold the whole combo.
            if state::subset_held(&config::COMBO_KEYS, &pad.held_keys) {
                info!("End-session combo detected (slot {})", pad.player_slot);
                sh.publish(Event::ComboEndSession);
            }
        }
        Internal::CaptureTimeout(generation) => {
            if generation != sh.capture_gen {
                return; // stale: the capture was already resolved/cancelled
            }
            sh.capture_timeout_task = None;
            if let Some(r) = sh.pending_capture.take() {
                let _ = r.send(resp_timeout());
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    X,
    Y,
}
