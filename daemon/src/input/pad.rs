//! Pad hardware: the physical [`PadDevice`], its input processing, and the
//! battery / LED / rumble sysfs glue.
//!
//! Split out of the former monolithic `input.rs` (behavior-preserving). Shared
//! types (`Shared`, `Fleet`, `Internal`, `Presenter`, `Axis`) and the crate
//! imports come from the parent module via `use super::*`.

use super::*;

/// One physical pad in the fleet: its grabbed event stream, stable player slot,
/// and all per-pad input state. The fd (the event stream's raw fd) is the
/// in-process key; `wire_id` is the stable cross-reconnect id used in `pad:*`
/// payloads.
pub(crate) struct PadDevice {
    pub(crate) fd: RawFd,
    pub(crate) event_stream: EventStream,
    pub(crate) wire_id: String,
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) player_slot: u8,
    pub(crate) grabbed: bool,

    pub(crate) held_keys: HashSet<u16>,
    /// Digital buttons that were physically held at the moment this pad entered
    /// the Game presenter (the shell→app flip). Populated from `held_keys` in
    /// [`PadDevice::enter_game`] on the transition that builds the virtual pad;
    /// cleared on [`PadDevice::enter_shell`] and whenever the virtual pad is
    /// (re)built. In the Game-presenter forwarding path a masked code is
    /// **swallowed** (never forwarded to the virtual pad) until it is released
    /// and pressed fresh — so the single physical press that launched an app
    /// from the shell does not also leak into the newly-focused app (the
    /// Steam-BPM A-leak, #295 follow-up). Kept SEPARATE from `held_keys`, which
    /// the safety combos read directly and must remain untouched.
    pub(crate) masked_keys: HashSet<u16>,
    /// Latest raw value of every ABS axis this pad has reported, keyed by evdev
    /// ABS code. Updated on **every** ABS event in [`PadDevice::handle_event`]
    /// regardless of presenter (exactly as `held_keys` is maintained), so that
    /// [`PadDevice::enter_game`] can tell which axes are physically deflected at
    /// the shell→app flip and seed [`Self::masked_axes`] from them.
    pub(crate) axis_values: HashMap<u16, i32>,
    /// Continuous axes (d-pad hat + both analog sticks) that were deflected past
    /// the neutral deadzone at the moment this pad entered the Game presenter —
    /// the axis analogue of [`Self::masked_keys`]. Populated from
    /// [`Self::axis_values`] in [`PadDevice::enter_game`]; cleared on
    /// [`PadDevice::enter_shell`] and whenever the virtual pad is (re)built. In
    /// the Game-presenter ABS forwarding path a masked axis is **swallowed**
    /// (never forwarded, so the vpad rests at its neutral default) until it
    /// returns to neutral and is deflected fresh — so the direction the user was
    /// holding to navigate to the launched card (a d-pad hat or stick) does not
    /// leak into the newly-focused game and latch it into a runaway scroll.
    /// Triggers (`ABS_Z`/`ABS_RZ`) are never masked (see
    /// [`PadDevice::axis_is_neutral`]).
    pub(crate) masked_axes: HashSet<u16>,
    pub(crate) left_trigger_held: bool,
    pub(crate) right_trigger_held: bool,

    // Left stick
    pub(crate) stick_x_key: Option<u16>,
    pub(crate) stick_y_key: Option<u16>,
    pub(crate) stick_x_repeat: Option<JoinHandle<()>>,
    pub(crate) stick_y_repeat: Option<JoinHandle<()>>,
    pub(crate) stick_x_gen: u64,
    pub(crate) stick_y_gen: u64,
    pub(crate) stick_center_x: i32,
    pub(crate) stick_threshold_x: i32,
    pub(crate) stick_center_y: i32,
    pub(crate) stick_threshold_y: i32,

    // Right stick
    pub(crate) rstick_center_x: i32,
    pub(crate) rstick_threshold_x: i32,
    pub(crate) rstick_half_range_x: i32,
    pub(crate) rstick_center_y: i32,
    pub(crate) rstick_threshold_y: i32,
    pub(crate) rstick_half_range_y: i32,
    pub(crate) rstick_raw_x: i32,
    pub(crate) rstick_raw_y: i32,
    pub(crate) rstick_x_dir: Option<&'static str>,
    pub(crate) rstick_y_dir: Option<&'static str>,
    pub(crate) mouse_task: Option<JoinHandle<()>>,

    // Hold/combo timers (per-pad: each pad detects its own complete combo).
    pub(crate) home_hold_task: Option<JoinHandle<()>>,
    pub(crate) home_hold_gen: u64,
    pub(crate) combo_task: Option<JoinHandle<()>>,
    pub(crate) combo_gen: u64,

    /// Combo-safety buffer (#escape-contract). In an APP presenter
    /// (Keyboard/Game) a combo-participant press is buffered here instead of
    /// forwarded, so a *partial* safety combo (e.g. the first two of
    /// Back+Home+LB+RB) never leaks into the focused app as a stray media key.
    /// The buffer is replayed to the app (in order) if the sequence is proven not
    /// to be a combo, or discarded if a combo completes. Holds `(code, value)`
    /// pairs in arrival order. Empty + disarmed in the Shell/Handoff presenters
    /// and reset on every presenter/session/overlay transition so a partial
    /// sequence can never strand across a context change.
    pub(crate) combo_buffer: Vec<(u16, i32)>,
    /// Whether [`Self::combo_buffer`] is actively buffering (armed). Armed by the
    /// first (Keyboard) / second (Game) held participant; cleared on
    /// swallow/replay/guard-timeout and every transition.
    pub(crate) combo_armed: bool,
    /// The combo settle-window timer (`combo_guard_ms`), armed when buffering
    /// starts. On fire it replays the buffer to the app (the "no combo arrived in
    /// time" disqualifier). `None` when not buffering.
    pub(crate) combo_guard_task: Option<JoinHandle<()>>,
    /// Generation token for [`Self::combo_guard_task`], so a stale fire (the guard
    /// disarmed/rearmed before its message was drained) is ignored.
    pub(crate) combo_guard_gen: u64,

    // --- Fleet outputs (ride-along, Phase 5.5) ---
    /// One clean virtual gamepad per player in game-presenter mode (Phase 5).
    /// `None` in shell mode. Registered in `Shared.reg` at creation, dropped +
    /// unregistered on leave.
    pub(crate) virtual_pad: Option<VirtualDevice>,
    /// Latest battery snapshot (#100). `None` until first read or for a wired
    /// pad with no reported battery. Updated by the sysfs battery poll; a change
    /// emits `pad:battery:{id,level,charging}`.
    pub(crate) battery: Option<BatteryState>,
    /// Player LED index lit at slot allocation (#101), if the pad is
    /// `EV_LED`-capable. `None` for pads without a controllable LED.
    pub(crate) led_index: Option<u8>,
    /// The uploaded rumble (FF_RUMBLE) effect (#99), if the pad supports force
    /// feedback. Uploaded lazily on the first `rumble` and kept alive here (its
    /// `Drop` erases the kernel effect), re-uploaded when the requested duration
    /// changes. `None` for pads without `EV_FF`/`FF_RUMBLE`.
    pub(crate) ff_effect: Option<FFEffect>,
    /// The replay length (ms) of the currently-uploaded `ff_effect`, so a repeat
    /// rumble at the same duration replays the cached effect instead of
    /// re-uploading it.
    pub(crate) ff_length_ms: u16,
}

/// Battery snapshot for a pad (#100). `None` on a pad means "no reported
/// battery" (a wired pad). Compared across polls to emit `pad:battery:*` only on
/// change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BatteryState {
    /// Charge percentage 0..=100.
    pub(crate) level: u8,
    pub(crate) charging: bool,
}

impl PadDevice {
    pub(crate) fn new(
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
            held_keys: HashSet::new(),
            masked_keys: HashSet::new(),
            axis_values: HashMap::new(),
            masked_axes: HashSet::new(),
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
            combo_buffer: Vec::new(),
            combo_armed: false,
            combo_guard_task: None,
            combo_guard_gen: 0,
            virtual_pad: None,
            battery: None,
            led_index: None,
            ff_effect: None,
            ff_length_ms: 0,
        }
    }

    // --- held-buttons notification --------------------------------------

    pub(crate) fn notify_held_buttons(&self, sh: &Shared) {
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

    pub(crate) fn grab(&mut self, sh: &mut Shared) {
        if self.grabbed {
            return;
        }
        // Don't take the physical grab while our session is backgrounded — the
        // foreground DE owns the controller then. Re-grabbed on reactivation via
        // `Control::SetSessionActive(true)`.
        if !sh.session_active {
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

    pub(crate) fn ungrab(&mut self, sh: &mut Shared) {
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

    pub(crate) fn calibrate(&mut self) {
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

    pub(crate) fn reset_triggers(&mut self) {
        self.left_trigger_held = false;
        self.right_trigger_held = false;
    }

    pub(crate) fn reset_stick_state(&mut self, sh: &mut Shared) {
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
    pub(crate) fn abort_all_tasks(&mut self) {
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
        // The combo-buffer guard timer + any buffered participants. No generation
        // bump here (unlike `disarm_combo_guard`): this runs on pad-leave/shutdown
        // where the pad is dropped immediately after, so a stale queued fire is
        // reaped by the fd-miss guard in the `ComboGuardFired` handler.
        if let Some(t) = self.combo_guard_task.take() {
            t.abort();
        }
        self.combo_buffer.clear();
        self.combo_armed = false;
    }

    // --- event handling --------------------------------------------------

    pub(crate) fn handle_event(&mut self, sh: &mut Shared, ev: InputEvent) {
        // Deepest debug level: every raw evdev event from the physical pad.
        // `RUST_LOG=tv_shell_input::input=trace` shows the full input stream
        // (slot + type/code/value) for diagnosing a misbehaving button/axis.
        trace!(
            slot = self.player_slot,
            ev_type = ev.event_type().0,
            code = ev.code(),
            value = ev.value(),
            "pad event"
        );
        // Track the latest value of every ABS axis, presenter-independently, so
        // `enter_game` knows which axes are physically deflected at a shell→app
        // flip (the axis flip-mask, mirroring how `held_keys` seeds the digital
        // flip-mask). Kept out of the per-presenter handlers on purpose: the flip
        // can be entered from any of them, so the cache must never miss an event.
        if ev.event_type() == EventType::ABSOLUTE {
            self.axis_values.insert(ev.code(), ev.value());
        }
        // Route by the *effective* presenter, not the physical grab: the pad
        // stays grabbed in both Shell/Game modes (Phase 5), so `grabbed` no
        // longer discriminates. `route_presenter` folds in overlay-focus (#262)
        // — a modal shell overlay open over a running app forces the Shell
        // handler over any base presenter so the pad drives the overlay, not the
        // app; the base presenter stays remembered in `sh.presenter`.
        match route_presenter(sh.shell_focus, sh.overlay_focus, sh.presenter) {
            // Keyboard shares the shell key-map handler — the difference between
            // "shell home focused" and "keyboard-contract app focused" is which
            // surface holds Wayland focus (where the emitted keys land), not how
            // the pad is translated. Neither holds a virtual pad.
            Presenter::Shell | Presenter::Keyboard => self.handle_shell(sh, ev),
            Presenter::Game => self.handle_game(sh, ev),
            Presenter::Handoff => self.handle_handoff(sh, ev),
        }
    }

    /// Shell presenter: map the pad to keyboard nav + mouse on the shared virtual
    /// devices, and turn gamepad Home into `intent:home-tap`/`intent:home-hold`.
    pub(crate) fn handle_shell(&mut self, sh: &mut Shared, ev: InputEvent) {
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

            // Meta (BTN_MODE / Guide): tap-vs-hold split, buffered delivery. Never
            // forwarded live while discriminating (no partial-press leak); the tap
            // is delivered per presenter and the hold is the reserved escape. See
            // `handle_meta`. Handled the same in the Shell home and a Keyboard-
            // contract app (both routed here) — only the tap *destination* differs
            // by presenter, which `handle_meta` resolves from `routed`.
            let routed = route_presenter(sh.shell_focus, sh.overlay_focus, sh.presenter);
            if code == cfg::BTN_MODE {
                self.handle_meta(sh, value, routed);
                return;
            }

            // A Keyboard-contract app is a focused external app (Plex): route its
            // combo participants through the combo buffer so a partial safety
            // combo never leaks in as a media key (the bug this fixes). The Shell
            // home has no app to leak into, so it forwards immediately (unchanged).
            if routed == Presenter::Keyboard {
                self.gate_and_forward_shell_key(sh, code, value);
            } else {
                self.shell_emit_key_event(sh, code, value);
            }
        } else if et == EventType::ABSOLUTE {
            self.handle_abs(sh, code, value);
        }
    }

    /// Emit the shell key-map side effects for one KEY event: the View/Select
    /// `overlay:session` deep-link (#218), LB/RB → mouse clicks, and the layered
    /// button→keyboard map (#104). This is "forward a button to the app" in the
    /// Shell/Keyboard presenter — used both for immediate forwarding (Shell) and
    /// for replaying a buffered participant (Keyboard). Split out so the combo
    /// buffer's replay reproduces exactly the same side effects as a live press.
    ///
    /// View/Select is deliberately NOT mirrored in `handle_game`, so it never
    /// interferes with the in-game force-quit combo (Back+Home+LB+RB, which also
    /// uses BTN_SELECT) or game input. BTN_SELECT has no default key binding, so
    /// nothing else consumes its press.
    pub(crate) fn shell_emit_key_event(&mut self, sh: &mut Shared, code: u16, value: i32) {
        if code == cfg::BTN_SELECT && value == 1 {
            sh.publish(Event::Intent("overlay:session".into()));
        }
        if code == cfg::BTN_TL {
            sh.emit_mouse_button(cfg::BTN_LEFT, value);
        } else if code == cfg::BTN_TR {
            sh.emit_mouse_button(cfg::BTN_RIGHT, value);
        }
        if let Some(mapped) = sh.resolved_key(self.player_slot, code) {
            sh.emit_key(mapped, value);
        }
    }

    /// Game presenter (Phase 5): re-present this physical pad as the clean
    /// per-player virtual gamepad, forwarding its events verbatim **except Home**.
    ///
    /// The physical pad stays grabbed, so nothing leaks to the compositor; the
    /// game reads `tv-shell-virtual-pad-<slot>` instead. Home (`BTN_MODE`) is
    /// always intercepted into `intent:home-tap`/`intent:home-hold` (never
    /// forwarded) so the shell overlay can come up over a running game. The
    /// gamepad-only safety combos (force-quit / suspend / end-session) still run
    /// off `held_keys`, exactly as before.
    pub(crate) fn handle_game(&mut self, sh: &mut Shared, ev: InputEvent) {
        let et = ev.event_type();
        let code = ev.code();
        let value = ev.value();

        if et == EventType::KEY {
            // Track held keys for the gamepad-only safety combos (force-quit /
            // suspend / end-session). These watch raw button state — including
            // `BTN_MODE` — and must keep working over a running game. Ordering
            // mirrors `handle_shell` so a combo arms identically regardless of
            // which button (Home or the other) is pressed last.
            if value == 1 {
                self.held_keys.insert(code);
                self.check_combo_start(sh);
                self.check_quit_combo(sh);
                self.check_suspend_combo(sh);
            } else if value == 0 {
                self.held_keys.remove(&code);
                self.cancel_combo(sh);
            }

            // Meta (BTN_MODE / Guide): tap-vs-hold split. Never forwarded live;
            // a TAP replays a real Guide press+release to the vpad (so the game/
            // Steam sees a Guide tap), a HOLD is the reserved shell escape (never
            // reaches the game). See `handle_meta`.
            if code == cfg::BTN_MODE {
                self.handle_meta(sh, value, Presenter::Game);
                return; // never forward the live Home to the game
            }

            // Route the remaining buttons through the combo buffer (so a partial
            // safety combo doesn't leak into the game) then, on forward/replay,
            // through the flip-mask onto the clean virtual pad.
            self.gate_and_forward_game_key(sh, code, value);
        } else if et == EventType::ABSOLUTE {
            // Route sticks/d-pad through the axis flip-mask (#295 follow-up): a
            // direction deflected at the shell→app flip — the d-pad hat or stick
            // the user was holding to reach the launched card — is swallowed until
            // it returns to neutral, so it never latches the fresh vpad into a
            // runaway Steam Big Picture scroll. Triggers and any unmasked axis
            // forward verbatim (the game wants raw axes) — see `axis_is_neutral`.
            if self.axis_forward_decision(code, value) {
                self.forward_to_virtual_pad(ev);
            }
        }
    }

    /// Handoff presenter (#221): the physical pad is UNGRABBED, so SDL/Moonlight
    /// reads the real evdev node directly (true handoff — no virtual twin). The
    /// daemon still receives every event because we keep the read half of the fd,
    /// but it only watches the gamepad safety combos off `held_keys`. There is no
    /// virtual pad, no key mapping, and **no Home interception** — Home flows
    /// straight through to the game so remote Steam sees the Guide button.
    pub(crate) fn handle_handoff(&mut self, sh: &mut Shared, ev: InputEvent) {
        let et = ev.event_type();
        let code = ev.code();
        let value = ev.value();

        if et == EventType::KEY {
            // Watch only the gamepad-only safety combos (force-quit / suspend /
            // end-session). No virtual-pad forwarding, no key map, no Home
            // intercept — the ungrabbed node is what the game reads.
            if value == 1 {
                self.held_keys.insert(code);
                self.check_combo_start(sh);
                self.check_quit_combo(sh);
                self.check_suspend_combo(sh);
            } else if value == 0 {
                self.held_keys.remove(&code);
                self.cancel_combo(sh);
            }

            // Unpinned (contract-driven) Handoff: best-effort Meta HOLD escape.
            // The node is UNGRABBED, so full swallowing is impossible — the app
            // reads the raw Meta press directly (it may see Guide for up to the
            // hold threshold). But the daemon still receives events, so a HOLD past
            // the threshold publishes the escape intent (`home-tap` → the shell's
            // controllable overlay). A tap-length press is left entirely to the app
            // (no intent). A PINNED Handoff (the Moonlight `handoff` IPC, #221) is
            // untouched: Home flows straight through so remote Steam sees Guide.
            if code == cfg::BTN_MODE && !sh.handoff_pinned {
                if value == 1 {
                    self.start_home_hold(sh);
                } else if value == 0 {
                    if let Some(t) = self.home_hold_task.take() {
                        t.abort();
                        // Release before the hold fired: no escape, and no tap
                        // intent (the ungrabbed app already saw the raw press —
                        // publishing a tap would double-act). Invalidate any queued
                        // HomeHoldFired so a near-miss can't still fire the escape.
                        self.home_hold_gen = sh.next_generation();
                    }
                }
            }
        }
        // ABS (sticks/triggers/d-pad) are intentionally ignored: the game reads
        // them off the ungrabbed physical node directly.
    }

    /// Emit one event onto this pad's clean virtual gamepad, if it has one.
    /// A no-op when `virtual_pad` is `None` (e.g. a transient race during a
    /// presenter flip); the event is simply dropped rather than leaked.
    pub(crate) fn forward_to_virtual_pad(&mut self, ev: InputEvent) {
        if let Some(vpad) = self.virtual_pad.as_mut() {
            let _ = vpad.emit(&[ev]);
        }
    }

    /// Forward one KEY event to the game's virtual pad, honoring the flip-mask
    /// (#295 follow-up): a button held at the shell→app flip is swallowed until
    /// released and pressed fresh. "Forward a button to the app" in the Game
    /// presenter — used both for immediate forwarding and for replaying a buffered
    /// combo participant. ABS axes are never masked and never routed here.
    pub(crate) fn game_forward_key(&mut self, code: u16, value: i32) {
        if mask_forward_decision(&mut self.masked_keys, code, value) {
            self.forward_to_virtual_pad(InputEvent::new(EV_KEY, code, value));
        }
    }

    /// Whether an ABS `value` on `code` sits within its neutral deadzone — the
    /// axis analogue of "a button is released". The d-pad hat is discrete
    /// (neutral only at exactly `0`); the analog sticks use their per-axis
    /// calibrated center + deadzone. Triggers (`ABS_Z`/`ABS_RZ`) and any other
    /// axis report neutral unconditionally, so they are never added to (and never
    /// gate through) [`Self::masked_axes`] — analog trigger use is left untouched.
    pub(crate) fn axis_is_neutral(&self, code: u16, value: i32) -> bool {
        match code {
            cfg::ABS_HAT0X | cfg::ABS_HAT0Y => abs_in_neutral_zone(value, 0, 0),
            cfg::ABS_X => abs_in_neutral_zone(value, self.stick_center_x, self.stick_threshold_x),
            cfg::ABS_Y => abs_in_neutral_zone(value, self.stick_center_y, self.stick_threshold_y),
            cfg::ABS_RX => {
                abs_in_neutral_zone(value, self.rstick_center_x, self.rstick_threshold_x)
            }
            cfg::ABS_RY => {
                abs_in_neutral_zone(value, self.rstick_center_y, self.rstick_threshold_y)
            }
            _ => true,
        }
    }

    /// Decide whether an ABS event should reach the Game presenter's virtual pad,
    /// honoring the axis flip-mask. A masked axis is swallowed until it returns to
    /// neutral (which lifts the mask); thereafter it forwards normally. Mirrors
    /// [`Self::game_forward_key`] for continuous axes.
    pub(crate) fn axis_forward_decision(&mut self, code: u16, value: i32) -> bool {
        let neutral = self.axis_is_neutral(code, value);
        mask_axis_forward_decision(&mut self.masked_axes, code, neutral)
    }

    /// Meta (BTN_MODE / Guide) tap-vs-hold split with buffered delivery.
    ///
    /// Net semantic change vs the pre-escape-contract daemon: the Meta press is
    /// NEVER forwarded live (no partial-press leak while we discriminate). On
    /// press we arm the hold timer; the button is "buffered" (withheld). On
    /// release we know it was a TAP (timer still pending) or, if the timer already
    /// fired, a HOLD whose escape was already published — so the release is
    /// swallowed. Per presenter (`routed`):
    ///
    /// * **TAP** (release before the threshold):
    ///   - Shell → publish `intent:home-tap` (the drawer; no app to forward to).
    ///   - Game → deliver the buffered Guide to the app as a real tap: forward
    ///     BTN_MODE press then release to the vpad (the game/Steam sees a Guide
    ///     tap).
    ///   - Keyboard → deliver NOTHING: a keyboard-contract app (Plex) has no Guide
    ///     concept and a synthetic keyboard chord would be arbitrary — the user's
    ///     escape here is the HOLD.
    /// * **HOLD** (timer fired, [`Internal::HomeHoldFired`]): the escape intent is
    ///   published by that handler (fleet-deduped). In an APP presenter that intent
    ///   is `intent:home-tap` (the controllable overlay drawer over the app — a
    ///   non-destructive everyday escape); only the Shell presenter's hold publishes
    ///   `intent:home-hold` (the idle reset-to-clean-home). Either way Meta is fully
    ///   swallowed — the app never sees it (except the Game-TAP replay above).
    ///
    /// This is the shell's reserved escape in every grabbed presenter; the app is
    /// never shown the button while we discriminate, so there is no leak window.
    pub(crate) fn handle_meta(&mut self, sh: &mut Shared, value: i32, routed: Presenter) {
        if value == 1 {
            // Press: arm the hold timer, buffer the Guide (do NOT forward).
            self.start_home_hold(sh);
        } else if value == 0 {
            // Release: if the hold timer is still pending this is a TAP; if it
            // already fired the HOLD path published the escape and we swallow.
            if let Some(t) = self.home_hold_task.take() {
                t.abort();
                // Invalidate any queued HomeHoldFired so a tap never also fires a
                // hold (race: released the instant the timer's message is in flight).
                self.home_hold_gen = sh.next_generation();
                match meta_tap_action(routed) {
                    MetaTapAction::HomeTap => sh.publish(Event::Intent("home-tap".into())),
                    MetaTapAction::ReplayToPad => {
                        self.forward_to_virtual_pad(InputEvent::new(EV_KEY, cfg::BTN_MODE, 1));
                        self.forward_to_virtual_pad(InputEvent::new(EV_KEY, cfg::BTN_MODE, 0));
                    }
                    MetaTapAction::Swallow => { /* keyboard-contract app: nothing */ }
                }
            }
            // else: the hold already fired — swallow the release (escape published).
        }
    }

    // --- combo buffer (per-presenter partial-combo suppression) -----------

    /// Gate one KEY event in the **Keyboard** presenter through the combo buffer
    /// (`arm_threshold = 1`: buffer from the first held participant — a media app
    /// tolerates the latency and this fully prevents media-key leaks). Forwards
    /// via [`Self::shell_emit_key_event`].
    pub(crate) fn gate_and_forward_shell_key(&mut self, sh: &mut Shared, code: u16, value: i32) {
        let action = state::combo_buffer_action(
            self.combo_armed,
            config::is_combo_participant(code),
            value,
            state::participant_held_count(&self.held_keys),
            1,
            state::any_combo_matched(&self.held_keys),
        );
        self.apply_combo_action(sh, Presenter::Keyboard, action, code, value);
    }

    /// Gate one KEY event in the **Game** presenter through the combo buffer
    /// (`arm_threshold = 2`: buffer only once a second participant is held, so
    /// single-button gameplay stays latency-free; the first participant of a pair
    /// may forward before arming, which a game tolerates). Forwards via
    /// [`Self::game_forward_key`].
    pub(crate) fn gate_and_forward_game_key(&mut self, sh: &mut Shared, code: u16, value: i32) {
        let action = state::combo_buffer_action(
            self.combo_armed,
            config::is_combo_participant(code),
            value,
            state::participant_held_count(&self.held_keys),
            2,
            state::any_combo_matched(&self.held_keys),
        );
        self.apply_combo_action(sh, Presenter::Game, action, code, value);
    }

    /// Carry out a [`state::ComboBufferAction`] for the given routed app presenter.
    /// The pure decision lives in `state`; this applies its side effects — the
    /// buffered-event replay into uinput/the vpad (the only non-pure part).
    pub(crate) fn apply_combo_action(
        &mut self,
        sh: &mut Shared,
        routed: Presenter,
        action: state::ComboBufferAction,
        code: u16,
        value: i32,
    ) {
        use state::ComboBufferAction as A;
        match action {
            A::Forward => self.forward_app_key(sh, routed, code, value),
            A::Buffer => {
                let was_armed = self.combo_armed;
                self.combo_buffer.push((code, value));
                self.combo_armed = true;
                // Arm the settle window only on the disarmed→armed edge — a fixed
                // window from the first buffered press bounds the latency.
                if !was_armed {
                    self.start_combo_guard(sh);
                }
            }
            A::Swallow => self.reset_combo_buffer(sh),
            A::ReplayThenForward => {
                // Disqualified by a non-participant: flush the buffer, then forward
                // the current (non-participant) event after it.
                self.replay_combo_buffer(sh, routed);
                self.disarm_combo_guard(sh);
                self.forward_app_key(sh, routed, code, value);
            }
            A::ReplayIncludingEvent => {
                // Disqualified by a participant release: the current event is part
                // of the (non-combo) sequence, so replay it with the rest.
                self.combo_buffer.push((code, value));
                self.replay_combo_buffer(sh, routed);
                self.disarm_combo_guard(sh);
            }
        }
    }

    /// Forward one buffered/live KEY event to the app via the routed presenter's
    /// path. Shell/Handoff are never armed, so they never reach here for a replay;
    /// guarded defensively.
    pub(crate) fn forward_app_key(
        &mut self,
        sh: &mut Shared,
        routed: Presenter,
        code: u16,
        value: i32,
    ) {
        match routed {
            Presenter::Keyboard => self.shell_emit_key_event(sh, code, value),
            Presenter::Game => self.game_forward_key(code, value),
            Presenter::Shell | Presenter::Handoff => {}
        }
    }

    /// Replay the buffered participants to the app (in arrival order) via the
    /// routed presenter's forward path, draining the buffer. Leaves `combo_armed`
    /// / the guard for the caller to clear (they always [`Self::disarm_combo_guard`]
    /// right after).
    pub(crate) fn replay_combo_buffer(&mut self, sh: &mut Shared, routed: Presenter) {
        let buffered = std::mem::take(&mut self.combo_buffer);
        for (code, value) in buffered {
            self.forward_app_key(sh, routed, code, value);
        }
    }

    /// Abort + invalidate the combo guard timer and mark the buffer disarmed. Does
    /// NOT touch buffer contents (callers either drained via replay or clear via
    /// [`Self::reset_combo_buffer`]).
    pub(crate) fn disarm_combo_guard(&mut self, sh: &mut Shared) {
        self.combo_armed = false;
        if let Some(t) = self.combo_guard_task.take() {
            t.abort();
        }
        // Bump the generation so any already-queued ComboGuardFired is ignored.
        self.combo_guard_gen = sh.next_generation();
    }

    /// Fully reset combo-buffer state: drop any buffered participants, disarm, and
    /// invalidate the guard timer. Called on a combo swallow and on every
    /// presenter/session/overlay transition so a partial sequence can never strand
    /// across a context change.
    pub(crate) fn reset_combo_buffer(&mut self, sh: &mut Shared) {
        self.combo_buffer.clear();
        self.disarm_combo_guard(sh);
    }

    pub(crate) fn handle_abs(&mut self, sh: &mut Shared, code: u16, value: i32) {
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

    pub(crate) fn handle_stick_axis(
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

    pub(crate) fn handle_rstick_axis(&mut self, sh: &mut Shared, value: i32, axis: Axis) {
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

    pub(crate) fn has_rstick_deflection(&self) -> bool {
        self.rstick_x_dir.is_some() || self.rstick_y_dir.is_some()
    }

    /// Compute and emit one mouse-poll tick. Returns false if the deflection has
    /// ended (the caller aborts the poll task).
    pub(crate) fn mouse_tick(&mut self, sh: &mut Shared) -> bool {
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

    pub(crate) fn start_stick_repeat(&mut self, sh: &mut Shared, axis: Axis, key: u16) {
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

    pub(crate) fn cancel_stick_repeat(&mut self, sh: &mut Shared, axis: Axis) {
        match axis {
            Axis::X => self.cancel_stick_repeat_x(sh),
            Axis::Y => self.cancel_stick_repeat_y(sh),
        }
    }

    pub(crate) fn cancel_stick_repeat_x(&mut self, sh: &mut Shared) {
        if let Some(t) = self.stick_x_repeat.take() {
            t.abort();
            self.stick_x_gen = sh.next_generation();
        }
    }

    pub(crate) fn cancel_stick_repeat_y(&mut self, sh: &mut Shared) {
        if let Some(t) = self.stick_y_repeat.take() {
            t.abort();
            self.stick_y_gen = sh.next_generation();
        }
    }

    // --- combos (per-pad-complete; see module docs [critic P1]) ----------

    pub(crate) fn check_combo_start(&mut self, sh: &mut Shared) {
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

    pub(crate) fn cancel_combo(&mut self, sh: &mut Shared) {
        if self.combo_task.is_some() && !state::subset_held(&config::COMBO_KEYS, &self.held_keys) {
            if let Some(t) = self.combo_task.take() {
                t.abort();
                self.combo_gen = sh.next_generation();
            }
        }
    }

    pub(crate) fn cancel_combo_unconditional(&mut self, sh: &mut Shared) {
        if let Some(t) = self.combo_task.take() {
            t.abort();
            self.combo_gen = sh.next_generation();
        }
    }

    pub(crate) fn check_quit_combo(&mut self, sh: &mut Shared) {
        if state::subset_held(&config::QUIT_COMBO_KEYS, &self.held_keys) {
            info!("Force-quit combo detected (Back+Home+LB+RB)");
            // Whenever a focused app owns the screen — a stream (Game/Handoff) or
            // a keyboard-contract app like Plex (Keyboard) — also send the quit
            // chord on the shared virtual keyboard, so a couch user who can't
            // reach a keyboard still has a controller escape from an app that
            // captured input. Only the Shell home screen has no app to quit.
            // (Pre-Phase-5 this keyed off the grab; the pad now stays grabbed in
            // every non-Handoff presenter, so it keys off the presenter instead.)
            // The chord is best-effort per app; the authoritative escape is the
            // `ComboForceQuit` event below, which the shell turns into a
            // force-quit/return-to-shell regardless of app.
            if presenter_owns_app(sh.presenter) {
                sh.send_moonlight_quit();
            }
            sh.publish(Event::ComboForceQuit);
        }
    }

    pub(crate) fn check_suspend_combo(&mut self, sh: &mut Shared) {
        if state::subset_held(&config::SUSPEND_COMBO_KEYS, &self.held_keys)
            && !state::subset_held(&config::QUIT_COMBO_KEYS, &self.held_keys)
        {
            info!("Suspend combo detected (LB+RB+Start)");
            sh.publish(Event::ComboSuspendStream);
        }
    }

    /// Arm the Meta (BTN_MODE / Guide) hold timer: sleep `meta_hold` then post
    /// [`Internal::HomeHoldFired`]. This is the tap-vs-hold discriminator for the
    /// Meta gesture in every grabbed presenter (and best-effort in an unpinned
    /// Handoff). The threshold is the `[input].meta_hold_ms` knob, not the old
    /// 2 s constant.
    pub(crate) fn start_home_hold(&mut self, sh: &mut Shared) {
        if let Some(t) = self.home_hold_task.take() {
            t.abort();
        }
        let generation = sh.next_generation();
        self.home_hold_gen = generation;
        let tx = sh.internal_tx.clone();
        let fd = self.fd;
        let hold = sh.meta_hold;
        self.home_hold_task = Some(tokio::spawn(async move {
            tokio::time::sleep(hold).await;
            let _ = tx.send(Internal::HomeHoldFired { fd, generation }).await;
        }));
    }

    /// Arm the combo settle-window timer (`combo_guard`): sleep then post
    /// [`Internal::ComboGuardFired`]. Started when the combo buffer arms; on fire
    /// the still-buffered participants (no combo completed) are replayed to the
    /// app. A fixed window from the arm point (not reset per buffered event), so
    /// the buffered-input latency is bounded by `combo_guard_ms`.
    pub(crate) fn start_combo_guard(&mut self, sh: &mut Shared) {
        if let Some(t) = self.combo_guard_task.take() {
            t.abort();
        }
        let generation = sh.next_generation();
        self.combo_guard_gen = generation;
        let tx = sh.internal_tx.clone();
        let fd = self.fd;
        let guard = sh.combo_guard;
        self.combo_guard_task = Some(tokio::spawn(async move {
            tokio::time::sleep(guard).await;
            let _ = tx.send(Internal::ComboGuardFired { fd, generation }).await;
        }));
    }

    // --- presenter entry/exit (Phase 5) ----------------------------------

    /// Enter the game presenter: create this pad's clean virtual gamepad (if it
    /// doesn't already have one) and register its fd so discovery never grabs it.
    /// The physical pad stays grabbed; subsequent events route through
    /// [`PadDevice::handle_game`] onto the virtual pad. Idempotent.
    pub(crate) fn enter_game(&mut self, sh: &mut Shared) {
        // A presenter transition: drop any partial combo sequence buffered under
        // the previous presenter so it can't strand or replay into the wrong app.
        self.reset_combo_buffer(sh);
        if self.virtual_pad.is_some() {
            return;
        }
        match build_virtual_pad(self.event_stream.device(), self.player_slot) {
            Ok(mut vpad) => {
                register_vpad_devnodes(&mut sh.reg, &mut vpad);
                // Capture the digital buttons held at this flip so they're
                // swallowed until released and pressed fresh (#295 follow-up).
                // Done ONLY on the transition that actually creates the vpad —
                // the early-return above guards the idempotent case, so a
                // redundant `enter_game` can't re-snapshot or clobber this. A
                // fresh (re)build starts from a clean mask before recapturing.
                self.masked_keys.clear();
                self.masked_keys.extend(self.held_keys.iter().copied());
                // Same for continuous axes: swallow any d-pad hat / stick that is
                // deflected past its neutral deadzone at this flip until it returns
                // to neutral (the axis analogue of the held-button mask above).
                // Collect first to end the shared borrow of `axis_values` before
                // mutating `masked_axes`. An idle centered stick has no deflected
                // entry, so it is not masked and keeps working immediately.
                self.masked_axes.clear();
                let deflected: Vec<u16> = self
                    .axis_values
                    .iter()
                    .filter(|(&code, &value)| !self.axis_is_neutral(code, value))
                    .map(|(&code, _)| code)
                    .collect();
                self.masked_axes.extend(deflected);
                // Invariant: the flip-mask is only ever populated FROM held_keys,
                // so every masked code must be currently held. Holds by
                // construction here (clear + extend(held_keys)); the debug_assert
                // guards a future edit that seeds masked_keys from another source.
                // debug-only, matching the grab-invariant discipline: panics in
                // dev/test, compiled out in release (no error!/metric for this one).
                debug_assert!(
                    self.masked_keys.iter().all(|c| self.held_keys.contains(c)),
                    "flip-mask must be a subset of held_keys (masked={:?}, held={:?})",
                    self.masked_keys,
                    self.held_keys,
                );
                info!(
                    "Created virtual pad tv-shell-virtual-pad-{} for slot {} ({})",
                    self.player_slot, self.player_slot, self.wire_id
                );
                self.virtual_pad = Some(vpad);
            }
            Err(e) => error!(
                "Failed to create virtual pad for slot {} ({}): {e}",
                self.player_slot, self.wire_id
            ),
        }
    }

    /// Leave the game presenter: drop this pad's virtual gamepad and forget its
    /// fd. The physical pad stays grabbed; events route back through the shell
    /// presenter ([`PadDevice::handle_shell`]). Idempotent.
    pub(crate) fn enter_shell(&mut self, sh: &mut Shared) {
        // Clear the flip-mask unconditionally so no masked state can leak across
        // a Game→Shell transition (#295 follow-up); the next `enter_game`
        // recaptures from `held_keys` at that flip.
        self.masked_keys.clear();
        self.masked_axes.clear();
        // A presenter transition: drop any partial combo sequence buffered under
        // the previous presenter (Keyboard/Game) so it can't strand.
        self.reset_combo_buffer(sh);
        if let Some(mut vpad) = self.virtual_pad.take() {
            unregister_vpad_devnodes(&mut sh.reg, &mut vpad);
            info!(
                "Dropped virtual pad for slot {} ({})",
                self.player_slot, self.wire_id
            );
        }
    }

    // --- fleet outputs: LED / rumble / battery (ride-along, Phase 5.5) -----

    /// Light this pad's player-indicator LED to match its slot (#101 LED).
    ///
    /// Two backends, tried in order:
    ///   1. **EV_LED** — when the pad advertises `EV_LED` with an LED code equal
    ///      to the player slot (`LED_0..` semantics — LED code == player index).
    ///   2. **sysfs fallback** — xpad (Xbox 360) pads expose their player ring
    ///      via `/sys/class/leds/xpad*`, **not** `EV_LED`, so the EV_LED write is
    ///      a silent no-op on them. When EV_LED is unsupported we walk the pad's
    ///      `/sys/class/input/eventN/device` tree up to a `leds/` dir and write
    ///      the xpad brightness convention to `<ledsnode>/brightness`.
    ///
    /// Most other wired pads expose neither, so this still silently does nothing
    /// for them. On either success records `led_index` and publishes
    /// `pad:index:{id,index}`. Best-effort: a failed write never fails the pad.
    /// Idempotent for the same slot.
    pub(crate) fn set_player_led(&mut self, sh: &mut Shared) {
        if self.led_index == Some(self.player_slot) {
            return; // already lit for this slot
        }
        // Cap-gate: the pad must support EV_LED with an LED code == player slot.
        let ev_led_supported = self
            .event_stream
            .device()
            .supported_leds()
            .is_some_and(|leds| leds.iter().any(|l| l.0 == self.player_slot as u16));
        if ev_led_supported {
            // Light the LED whose code == the player slot (LED_0.. convention).
            let ev = InputEvent::new(EV_LED, self.player_slot as u16, 1);
            match self.event_stream.device_mut().send_events(&[ev]) {
                Ok(()) => {
                    self.led_index = Some(self.player_slot);
                    info!(
                        "Lit player LED {} for slot {} ({})",
                        self.player_slot, self.player_slot, self.wire_id
                    );
                    sh.publish(Event::PadIndex(crate::protocol::pad_index_json(
                        &self.wire_id,
                        self.player_slot,
                    )));
                }
                Err(e) => warn!("Failed to set player LED on {}: {e}", self.wire_id),
            }
            return;
        }

        // EV_LED unsupported: try the sysfs xpad fallback (Xbox 360 ring).
        match set_player_led_sysfs(&self.path, self.player_slot) {
            Ok(Some(node)) => {
                self.led_index = Some(self.player_slot);
                info!(
                    "player LED set via sysfs {} for slot {} ({})",
                    node.display(),
                    self.player_slot,
                    self.wire_id
                );
                sh.publish(Event::PadIndex(crate::protocol::pad_index_json(
                    &self.wire_id,
                    self.player_slot,
                )));
            }
            // No leds node found for this pad: a true no-op (most pads). Quiet at
            // debug so the common case doesn't spam logs.
            Ok(None) => debug!(
                "no sysfs leds node for pad slot {} ({}); player LED unset",
                self.player_slot, self.wire_id
            ),
            Err(e) => warn!(
                "Failed to set player LED via sysfs on {}: {e}",
                self.wire_id
            ),
        }
    }

    /// Fire a rumble (FF_RUMBLE) effect for `ms` milliseconds (#99).
    ///
    /// Cap-gated: a no-op unless the pad advertises `EV_FF` with `FF_RUMBLE`.
    /// The effect is uploaded lazily and cached; a repeat at the same duration
    /// replays the cached effect, a different duration re-uploads. A zero `ms`
    /// is treated as a no-op (nothing to play). Failures are logged and
    /// swallowed — rumble is best-effort and must never wedge input handling.
    pub(crate) fn rumble(&mut self, ms: u16) {
        if ms == 0 {
            return;
        }
        // Cap-gate: require EV_FF advertising FF_RUMBLE.
        let supports_rumble = self
            .event_stream
            .device()
            .supported_ff()
            .is_some_and(|ff| ff.contains(FFEffectCode::FF_RUMBLE));
        if !supports_rumble {
            return; // no force feedback on this pad -> no-op
        }
        // (Re)upload the effect if we don't have one or the duration changed.
        if self.ff_effect.is_none() || self.ff_length_ms != ms {
            let data = rumble_effect_data(ms);
            match self.event_stream.device_mut().upload_ff_effect(data) {
                Ok(effect) => {
                    self.ff_effect = Some(effect);
                    self.ff_length_ms = ms;
                }
                Err(e) => {
                    warn!("Failed to upload rumble effect on {}: {e}", self.wire_id);
                    return;
                }
            }
        }
        if let Some(effect) = self.ff_effect.as_mut() {
            if let Err(e) = effect.play(1) {
                warn!("Failed to play rumble on {}: {e}", self.wire_id);
            }
        }
    }

    /// Re-read this pad's battery from sysfs and, if it changed, emit
    /// `pad:battery:{id,level,charging}` (#100). A no-op for wired pads (no
    /// matching `power_supply`). Used on the (synchronous) pad-join path; the
    /// periodic poll instead offloads the sysfs read and calls [`apply_battery`].
    pub(crate) fn poll_battery(&mut self, sh: &mut Shared) {
        self.apply_battery(sh, read_pad_battery(&self.name));
    }

    /// Apply an already-read battery state: if it changed, emit
    /// `pad:battery:{id,level,charging}` (#100). `None` (wired pad / no supply)
    /// is left as the current state and emits nothing. Pure of I/O so the sysfs
    /// read can be offloaded to the blocking pool by the periodic poll arm.
    pub(crate) fn apply_battery(&mut self, sh: &mut Shared, state: Option<BatteryState>) {
        let Some(state) = state else {
            return; // no battery reported (wired pad) -> nothing to emit
        };
        if self.battery == Some(state) {
            return; // unchanged
        }
        self.battery = Some(state);
        sh.publish(Event::PadBattery(crate::protocol::pad_battery_json(
            &self.wire_id,
            state.level,
            state.charging,
        )));
    }
}

/// Build the `FFEffectData` for a tasteful one-shot rumble of `ms` milliseconds.
/// A medium dual-motor rumble (both the heavy and light motors) that auto-stops
/// after `length`; the effect plays once via `FFEffect::play(1)`.
pub(crate) fn rumble_effect_data(ms: u16) -> FFEffectData {
    FFEffectData {
        direction: 0,
        trigger: FFTrigger::default(),
        replay: FFReplay {
            length: ms,
            delay: 0,
        },
        kind: FFEffectKind::Rumble {
            strong_magnitude: 0x8000,
            weak_magnitude: 0x8000,
        },
    }
}

/// Best-effort sysfs battery read for a gamepad whose device name is `pad_name`.
///
/// Scans `/sys/class/power_supply/*` for a `Battery`-type supply that looks like
/// a game controller (its `model_name` matches the pad name, or its supply name
/// carries a controller marker). Returns `(level, charging)` or `None` when no
/// matching battery is found — the normal case for a wired pad. Conservative on
/// purpose: a non-match degrades to "no battery", never a wrong reading.
pub(crate) fn read_pad_battery(pad_name: &str) -> Option<BatteryState> {
    let dir = std::fs::read_dir("/sys/class/power_supply").ok()?;
    let pad_lower = pad_name.to_lowercase();
    for entry in dir.flatten() {
        let base = entry.path();
        // Only battery-type supplies.
        let kind = read_sysfs_trimmed(&base.join("type")).unwrap_or_default();
        if !kind.eq_ignore_ascii_case("Battery") {
            continue;
        }
        let supply_name = entry.file_name().to_string_lossy().to_lowercase();
        let model = read_sysfs_trimmed(&base.join("model_name"))
            .unwrap_or_default()
            .to_lowercase();
        // Match on the pad's model name, or a controller marker in the supply
        // name (joydev/xpad/sony/nintendo power supplies use such names).
        let looks_like_pad = (!model.is_empty()
            && (model.contains(&pad_lower) || pad_lower.contains(&model)))
            || [
                "controller",
                "gamepad",
                "xpad",
                "sony",
                "nintendo",
                "joypad",
            ]
            .iter()
            .any(|m| supply_name.contains(m));
        if !looks_like_pad {
            continue;
        }
        let Some(capacity) =
            read_sysfs_trimmed(&base.join("capacity")).and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let status = read_sysfs_trimmed(&base.join("status")).unwrap_or_default();
        let charging =
            status.eq_ignore_ascii_case("Charging") || status.eq_ignore_ascii_case("Full");
        return Some(BatteryState {
            level: capacity.min(100) as u8,
            charging,
        });
    }
    None
}

/// Read a sysfs attribute file and return its trimmed contents, or `None` on any
/// I/O error (an absent attribute is common and not worth logging).
pub(crate) fn read_sysfs_trimmed(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// LED driver convention inferred from a sysfs leds node name.
///
/// Classified by the NAME of the directory under `leds/` (the node itself, not
/// its parent), using substring matching so both the simple `xpad0` form and the
/// BDADDR-prefixed `0005:054C:0CE6.000D:white:player-1` form are handled
/// correctly across kernel versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LedConvention {
    /// Xbox / xpad ring-LED brightness: write `6 + slot` to `brightness`.
    Xpad,
    /// Sony DualSense/DualShock4 `*:white:player-N` per-slot player indicators.
    /// Write `1` to the matching `player-<slot+1>` sibling, `0` to the others.
    SonyPlayer,
    /// Sony DualSense RGB lightbar (`*:rgb:indicator`).
    /// Write a per-slot solid colour via `multi_intensity` (R G B).
    SonyRgb,
}

/// Classify a leds node by its directory name.
///
/// This is a pure function with no I/O so it can be unit-tested without real
/// sysfs. Matching is by substring so it is robust across kernel naming variants
/// (short `xpad0`, BDADDR-prefixed `0005:054C:...:white:player-1`, etc.).
pub(crate) fn classify_led_node(node_name: &str) -> LedConvention {
    if node_name.contains(":white:player-") {
        LedConvention::SonyPlayer
    } else if node_name.contains(":rgb:indicator") {
        LedConvention::SonyRgb
    } else {
        // Covers bare `xpad0`, `xpad1`, and any plain-brightness ring node.
        LedConvention::Xpad
    }
}

/// Per-slot RGB colour table for the Sony RGB lightbar.
///
/// P1 blue, P2 red, P3 green, P4 magenta — matches the DualSense player
/// colour conventions used by hid-playstation when the OS does not override
/// them. Written as `R G B` (space-separated, decimal) to `multi_intensity`.
pub(crate) const SONY_RGB_COLORS: [(u8, u8, u8); 4] = [
    (0, 0, 255),   // P1 blue
    (255, 0, 0),   // P2 red
    (0, 255, 0),   // P3 green
    (255, 0, 255), // P4 magenta
];

/// Choose the best-correlated leds node for a specific pad from a set of
/// candidates found under a shared `leds/` dir.
///
/// Selection criterion: the candidate whose canonicalized path shares the
/// **longest common prefix** with `pad_device_path` is the physically nearest
/// node for that pad. Ties are broken by sorted node name (lexicographic
/// ascending), so `xpad0` beats `xpad1` when prefixes are equal, and the
/// choice is always deterministic regardless of `read_dir` order.
///
/// Returns `None` when `candidates` is empty.
///
/// This is a pure function with no I/O so it can be unit-tested without real
/// sysfs. All path resolution must be done by the caller before passing in.
pub(crate) fn pick_leds_node(
    pad_device_path: &std::path::Path,
    candidates: &[PathBuf],
) -> Option<PathBuf> {
    if candidates.is_empty() {
        return None;
    }

    // Count the number of path components shared between `a` and `b`.
    fn shared_prefix_len(a: &std::path::Path, b: &std::path::Path) -> usize {
        a.components()
            .zip(b.components())
            .take_while(|(x, y)| x == y)
            .count()
    }

    candidates
        .iter()
        .max_by(|a, b| {
            let la = shared_prefix_len(pad_device_path, a);
            let lb = shared_prefix_len(pad_device_path, b);
            la.cmp(&lb)
                // Tie-break: sorted name — choose the *smaller* name (xpad0 < xpad1)
                // by reversing the secondary comparison (max_by picks the larger).
                .then_with(|| {
                    let na = a.file_name().unwrap_or_default();
                    let nb = b.file_name().unwrap_or_default();
                    nb.cmp(na) // reversed so max_by picks the lexicographically smallest
                })
        })
        .cloned()
}

/// sysfs player-LED fallback for pads that expose their player ring via
/// `/sys/class/leds` instead of `EV_LED`.
///
/// Two driver families are handled:
///
/// - **xpad (Xbox 360/One)** — a single `xpad<N>` node under `leds/` with a
///   plain `brightness` attribute. Write `6 + slot.min(3)` for SOLID P1..P4.
/// - **Sony DualSense/DualShock4** — either `*:white:player-N` per-slot nodes
///   (write `1` to the matching player node, `0` to siblings) or an
///   `*:rgb:indicator` lightbar node (write a per-slot colour to
///   `multi_intensity`).
///
/// Multi-pad correlation: when multiple candidates exist under `leds/`, the
/// one with the longest shared canonical-path prefix with the pad's own sysfs
/// device path is chosen (ties broken by sorted name), so two identical xpads
/// each light their own ring instead of racing on the first entry.
///
/// Returns `Ok(Some(node))` on a successful primary write, `Ok(None)` when no
/// usable leds node/convention is found, or `Err` on an unexpected FS error
/// (the caller logs and swallows it).
pub(crate) fn set_player_led_sysfs(
    devnode: &std::path::Path,
    slot: u8,
) -> std::io::Result<Option<PathBuf>> {
    // /dev/input/eventN -> "eventN"
    let Some(event_name) = devnode.file_name().and_then(|n| n.to_str()) else {
        return Ok(None);
    };
    if !event_name.starts_with("event") {
        return Ok(None);
    }
    // /sys/class/input/eventN/device is a symlink into the device tree; resolve
    // it so we can walk real parent dirs.
    let device_link = PathBuf::from("/sys/class/input")
        .join(event_name)
        .join("device");
    let Ok(pad_device_path) = std::fs::canonicalize(&device_link) else {
        return Ok(None);
    };
    let mut dir = pad_device_path.clone();
    // Walk up to a parent containing a `leds/` subdir. Bound the climb so a
    // surprising tree can't loop forever.
    let (leds_dir, chosen_node) = loop {
        let leds = dir.join("leds");
        if leds.is_dir() {
            // Collect ALL entries, sort by name for determinism, then delegate
            // correlation to the pure helper.
            let mut candidates: Vec<PathBuf> = std::fs::read_dir(&leds)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .collect();
            candidates.sort_by(|a, b| {
                a.file_name()
                    .unwrap_or_default()
                    .cmp(b.file_name().unwrap_or_default())
            });
            match pick_leds_node(&pad_device_path, &candidates) {
                Some(node) => break (leds, node),
                None => return Ok(None), // empty leds dir
            }
        }
        match dir.parent() {
            // Stop at the sysfs root or filesystem root.
            Some(p) if p != std::path::Path::new("/sys") && p != std::path::Path::new("/") => {
                dir = p.to_path_buf();
            }
            _ => return Ok(None),
        }
    };

    let node_name = chosen_node
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    match classify_led_node(node_name) {
        LedConvention::Xpad => {
            // 6..=9 = SOLID P1..P4; cap the slot at P4.
            let brightness = 6u8 + slot.min(3);
            std::fs::write(chosen_node.join("brightness"), brightness.to_string())?;
            Ok(Some(chosen_node))
        }
        LedConvention::SonyPlayer => {
            // Write `1` to the player-<slot+1> sibling and `0` to the others.
            // Build the target player index (1-based, capped at 4).
            let target_player = slot.min(3) + 1;
            // Scan all `*:white:player-N` siblings in the same leds/ dir.
            let siblings: Vec<PathBuf> = std::fs::read_dir(&leds_dir)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.contains(":white:player-"))
                        .unwrap_or(false)
                })
                .collect();
            if siblings.is_empty() {
                return Ok(None);
            }
            let mut primary_node: Option<PathBuf> = None;
            for sibling in &siblings {
                let sib_name = sibling.file_name().and_then(|n| n.to_str()).unwrap_or("");
                // Extract the player number from the suffix `player-N`.
                let player_num: Option<u8> = sib_name
                    .rfind(":white:player-")
                    .and_then(|pos| sib_name[pos + ":white:player-".len()..].parse().ok());
                let value = if player_num == Some(target_player) {
                    primary_node = Some(sibling.clone());
                    "1"
                } else {
                    "0"
                };
                // Best-effort: ignore individual write errors.
                let _ = std::fs::write(sibling.join("brightness"), value);
            }
            Ok(primary_node)
        }
        LedConvention::SonyRgb => {
            let (r, g, b) = SONY_RGB_COLORS[slot.min(3) as usize];
            // Try `multi_intensity` first (the standard RGB LED class
            // attribute); fall back gracefully for older kernels.
            let mi_path = chosen_node.join("multi_intensity");
            if mi_path.exists() {
                let _ = std::fs::write(&mi_path, format!("{r} {g} {b}"));
                Ok(Some(chosen_node))
            } else {
                // Unrecognized interface: leave the lightbar unset.
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod led_tests {
    use super::*;
    use std::path::PathBuf;

    // ---- classify_led_node -------------------------------------------------

    #[test]
    fn classify_xpad_bare() {
        assert_eq!(classify_led_node("xpad0"), LedConvention::Xpad);
        assert_eq!(classify_led_node("xpad1"), LedConvention::Xpad);
        // Plain brightness node with no recognized suffix -> Xpad default.
        assert_eq!(classify_led_node("input5::green:led0"), LedConvention::Xpad);
    }

    #[test]
    fn classify_sony_player() {
        // Short form (some kernel versions).
        assert_eq!(
            classify_led_node("input5::white:player-1"),
            LedConvention::SonyPlayer
        );
        // BDADDR-prefixed form (hid-playstation / kernel >= 6.3).
        assert_eq!(
            classify_led_node("0005:054C:0CE6.000D:white:player-1"),
            LedConvention::SonyPlayer
        );
        assert_eq!(
            classify_led_node("0005:054C:0CE6.000D:white:player-4"),
            LedConvention::SonyPlayer
        );
    }

    #[test]
    fn classify_sony_rgb() {
        assert_eq!(
            classify_led_node("input5::rgb:indicator"),
            LedConvention::SonyRgb
        );
        assert_eq!(
            classify_led_node("0005:054C:0CE6.000D:rgb:indicator"),
            LedConvention::SonyRgb
        );
    }

    // ---- pick_leds_node ----------------------------------------------------

    #[test]
    fn pick_empty_candidates_returns_none() {
        let pad = PathBuf::from("/sys/devices/pci0000:00/usb1/1-1/input/input5");
        assert_eq!(pick_leds_node(&pad, &[]), None);
    }

    #[test]
    fn pick_single_candidate_is_returned() {
        let pad = PathBuf::from("/sys/devices/pci0000:00/usb1/1-1/input/input5");
        let candidates = vec![PathBuf::from("/sys/devices/pci0000:00/usb1/1-1/leds/xpad0")];
        assert_eq!(
            pick_leds_node(&pad, &candidates),
            Some(PathBuf::from("/sys/devices/pci0000:00/usb1/1-1/leds/xpad0",))
        );
    }

    /// Two identical xpads on different USB ports: pad1 at usb1/1-1, pad2 at
    /// usb1/1-2. The leds nodes live under each pad's own USB subtree.
    #[test]
    fn pick_correlates_to_correct_pad() {
        let pad = PathBuf::from("/sys/devices/pci0000:00/usb1/1-1/input/input5");
        let candidates = vec![
            // This node is under a DIFFERENT USB port (1-2).
            PathBuf::from("/sys/devices/pci0000:00/usb1/1-2/leds/xpad1"),
            // This node is under the SAME USB port (1-1).
            PathBuf::from("/sys/devices/pci0000:00/usb1/1-1/leds/xpad0"),
        ];
        assert_eq!(
            pick_leds_node(&pad, &candidates),
            Some(PathBuf::from("/sys/devices/pci0000:00/usb1/1-1/leds/xpad0",))
        );
    }

    /// When both candidates share an equal-length prefix with the pad path
    /// (e.g. they live under a shared ancestor), the lexicographically first
    /// name wins (xpad0 before xpad1).
    #[test]
    fn pick_tiebreak_by_sorted_name() {
        let pad = PathBuf::from("/sys/devices/pci0000:00/usb1/1-1/input/input5");
        let candidates = vec![
            PathBuf::from("/sys/devices/pci0000:00/usb1/leds/xpad1"),
            PathBuf::from("/sys/devices/pci0000:00/usb1/leds/xpad0"),
        ];
        assert_eq!(
            pick_leds_node(&pad, &candidates),
            Some(PathBuf::from("/sys/devices/pci0000:00/usb1/leds/xpad0",))
        );
    }

    /// The single-xpad common case (PR #103 regression guard): one candidate,
    /// one pad -- must still be selected.
    #[test]
    fn pick_single_xpad_regression() {
        let pad = PathBuf::from("/sys/devices/pci0000:00/usb1/1-3/input/input2");
        let candidates = vec![PathBuf::from("/sys/devices/pci0000:00/usb1/1-3/leds/xpad0")];
        let result = pick_leds_node(&pad, &candidates);
        assert!(
            result.is_some(),
            "single-xpad path must be selected, got None"
        );
        assert_eq!(
            result.unwrap().file_name().and_then(|n| n.to_str()),
            Some("xpad0")
        );
    }
}
