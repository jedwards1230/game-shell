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
//! pads pressing Home simultaneously produce one `intent:home-hold`, never a
//! double-fire, and two pads each holding *half* a combo never trigger it. For a
//! single connected pad this is byte-identical to the pre-fleet behavior.
//!
//! This was ported from the former `input/gamepad-input.py` (since deleted —
//! this daemon is now the sole backend). The wire-facing strings
//! and pure decision logic live in `protocol`/`config`/`state` (and are tested
//! on any host); this module is the evdev/uinput glue, exercised by CI on Linux
//! and on the target device.

pub(crate) use crate::config as cfg;
pub(crate) use crate::config::{self, Binding};
pub(crate) use crate::daemon_config::{InputContract, InputContracts};
pub(crate) use crate::device::{self, ControllerDb, SlotAllocator, VirtualRegistry};
pub(crate) use crate::protocol::{
    is_known_intent, pad_connected_json, resp_cancelled, resp_captured, resp_invalid_button,
    resp_ok, resp_pad_battery_not_present, resp_pad_battery_present, resp_pad_not_found,
    resp_pad_rumble_status, resp_pads, resp_status, resp_timeout, resp_unknown_action,
    resp_unknown_intent, resp_unknown_key, Event, InputMode,
};
pub(crate) use crate::state::{self, Control, Reply};
pub(crate) use evdev::uinput::VirtualDevice;
pub(crate) use evdev::{
    AbsoluteAxisCode, AttributeSet, Device, EventStream, EventType, FFEffect, FFEffectCode,
    FFEffectData, FFEffectKind, FFReplay, FFTrigger, InputEvent, KeyCode, RelativeAxisCode,
    UinputAbsSetup,
};
pub(crate) use futures::future::FutureExt;
pub(crate) use std::collections::{HashMap, HashSet};
pub(crate) use std::os::fd::{AsRawFd, RawFd};
pub(crate) use std::panic::AssertUnwindSafe;
pub(crate) use std::path::PathBuf;
pub(crate) use std::time::Duration;
pub(crate) use tokio::sync::{broadcast, mpsc, watch};
pub(crate) use tokio::task::JoinHandle;
pub(crate) use tracing::{debug, error, info, trace, warn};

mod capture;
mod enumerate;
mod fleet;
mod grab;
mod pad;

pub(crate) use capture::*;
pub(crate) use enumerate::*;
pub(crate) use fleet::*;
pub(crate) use grab::*;
pub(crate) use pad::*;

const EV_KEY: u16 = config::EV_KEY;
const EV_REL: u16 = config::EV_REL;
/// `EV_LED` event type (kernel `input-event-codes.h`). Used to drive a
/// controller's player-indicator LED (#101). Cap-gated by the pad advertising
/// `EventType::LED`; absent on pads without controllable LEDs (most wired pads).
const EV_LED: u16 = 0x11;

/// Duration (ms) of the short haptic pulse fired when a pad connects (#99).
/// Tasteful and brief — a single confirmation buzz, not a sustained rumble.
const CONNECT_RUMBLE_MS: u16 = 150;

/// Follow-focus settle debounce (ms). Hyprland churns the focused window during
/// an app launch/close (the focus flaps empty↔toplevel, and between the shell
/// layer and the app, several times over a fraction of a second). Waiting this
/// long for focus to *settle* before applying the presenter transition collapses
/// that churn into a single net transition — and, the point, avoids tearing down
/// and rebuilding the per-player virtual pad on every flap, which Steam
/// re-enumerates as a controller reconnect (audible/visible, and worse than the
/// original bug). 300ms sits mid-range in the 200–500ms window: long enough to
/// swallow a launch flap, short enough that a deliberate focus change still feels
/// immediate. Only follow-focus is debounced; explicit `grab`/`release`/`handoff`
/// / overlay-focus IPC still applies instantly.
const FOCUS_SETTLE_MS: u64 = 300;

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
pub(crate) enum Internal {
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
    /// The combo settle window elapsed on pad `fd` with no combo matched: the
    /// buffered combo-participant presses were not a combo, so replay them to the
    /// focused app (see [`PadDevice::combo_buffer`]).
    ComboGuardFired { fd: RawFd, generation: u64 },
    /// A pending `capture-next` timed out (fleet-level).
    CaptureTimeout(u64),
    /// The follow-focus settle debounce elapsed (fleet-level): apply the latest
    /// pending focused-window class if this tick is still the live one. Carries a
    /// `generation` so a superseded settle timer (focus moved again before it
    /// fired) is ignored — only the newest pending focus is applied.
    FocusSettle { generation: u64 },
}

/// Which presenter the fleet is currently driving (Phase 5). Mode is toggled by
/// the `grab`/`release` IPC; the daemon **keeps the physical EVIOCGRAB in both
/// modes** so no controller input ever leaks to the compositor.
///
/// * [`Presenter::Shell`] — the shell home screen owns input. Each pad's
///   buttons/d-pad/left-stick map to keyboard nav keys on the shared virtual
///   keyboard; the right stick drives the shared virtual mouse; gamepad Home
///   becomes `intent:home-tap`/`intent:home-hold`. This is the menu-navigation
///   presenter (#7: all pads share one focus).
/// * [`Presenter::Keyboard`] — a focused external app with a **keyboard input
///   contract** (e.g. `tv.plex.Plex`) owns input. Mechanically identical to
///   [`Presenter::Shell`] — the same shell key-map is emitted onto the shared
///   virtual keyboard/mouse — but the events land on the *focused app* (which
///   holds Wayland focus), not the shell, so a key-driven HTPC UI is d-pad
///   drivable. The distinction from `Shell` is which surface has focus, not the
///   handler; kept a separate variant so follow-focus, `status`, and the logs can
///   tell "shell home focused" from "keyboard-contract app focused". Critically,
///   like `Shell` it holds **no virtual pad**, so Steam has nothing to
///   exclusive-grab (the fix for the always-alive-vpad bug).
/// * [`Presenter::Game`] — a streamed/launched app owns input. Each pad is
///   re-presented as one clean per-player virtual gamepad (`PadDevice.virtual_pad`,
///   #6) carrying the physical pad's events verbatim **except Home**, which is
///   always intercepted into `intent:home-*` so the shell overlay can come up
///   over a running game (substrate for #75). The gamepad-only safety combos
///   (force-quit / suspend / end-session) still run.
/// * [`Presenter::Handoff`] — the Moonlight stream presenter (#221). The physical
///   pad is **UNGRABBED** (EVIOCGRAB released) so SDL/Moonlight reads the real
///   evdev node directly — a true handoff, no virtual twin. The daemon still
///   receives events (the session stays active), but watches **only** the gamepad
///   safety combos (force-quit / suspend / end-session); Home is **not**
///   intercepted, so remote Steam sees the Guide button. Contrast with
///   [`Presenter::Game`], which keeps the grab + a virtual twin (the old
///   `release` path that left SDL seeing a live virtual pad *and* a silently
///   grabbed physical node).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Presenter {
    Shell,
    Keyboard,
    Game,
    Handoff,
}

/// Resources shared across every pad in the fleet: the virtual output devices,
/// the broadcast bus, identity tables, the remap map, and the fleet-level
/// capture/input-mode/generation state. Per-pad input state lives in
/// [`PadDevice`]; methods there borrow this for emission and publishing.
pub(crate) struct Shared {
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
    /// fires; `intent:home-hold` is published only on the 0->1 edge, so two pads
    /// holding Home simultaneously emit the single `intent:home-hold` once
    /// (followup: "simultaneous Home-hold fires once"). Cleared when no pad
    /// holds `BTN_MODE` anymore. Single-pad: the latch toggles exactly with the
    /// one pad, so behavior is identical to the pre-fleet single fire.
    home_hold_active: bool,

    /// The active (base) presenter (Phase 5). Starts in [`Presenter::Shell`]
    /// (the shell boots focused). `release`/`grab`/`handoff` flip it; per-pad
    /// event routing (`handle_event`) branches on it — unless `overlay_focus`
    /// is set, in which case the SHELL handler runs regardless (see below).
    presenter: Presenter,

    /// Whether a modal shell overlay is open over a running app (`overlay-focus
    /// on|off`, #262). While `true`, [`Self::presenter`] is left untouched (it
    /// remembers the base presenter) but pad events route to the SHELL key-map
    /// via [`route_presenter`] and every pad is force-grabbed via
    /// [`should_grab`], so the app stops seeing raw events — critical for
    /// [`Presenter::Handoff`], where the pad is normally ungrabbed. Turning it
    /// off restores the base presenter's routing + grab exactly. Defaults
    /// `false`; in-memory only.
    overlay_focus: bool,

    /// Whether the SHELL currently owns (or shares) the screen (`shell-focus
    /// on|off`). The shell's own authoritative declaration, mirroring its
    /// `visible:` binding — the daemon cannot derive it, because the shell is a
    /// wlr-layer-shell surface that never appears in Hyprland's `activewindow`,
    /// so compositor focus keeps naming a backgrounded toplevel while the shell
    /// is on screen.
    ///
    /// While `true`, [`Self::presenter`] is left untouched (it remembers the base
    /// routing) but pad events route to the SHELL key-map via [`route_presenter`],
    /// every pad is force-grabbed via [`should_grab`], and follow-focus is
    /// suppressed entirely ([`focus_presenter_target`]) so a stale `activewindow`
    /// class cannot hand the pad to a backgrounded app.
    ///
    /// Defaults `true` — the shell boots owning the screen, matching
    /// [`Presenter::Shell`]. This default is also what makes a daemon restart
    /// mid-session safe: it comes up shell-owned, and the shell's heartbeat
    /// re-asserts the truth within one tick. In-memory only.
    shell_focus: bool,

    /// Publishes [`Self::shell_focus`] to the Hyprland actor, whose kiosk
    /// fullscreen backstop must not force-focus a stale active window while the
    /// shell owns the screen (see `hyprland::kiosk_may_enforce`).
    shell_focus_tx: watch::Sender<bool>,

    /// Whether our logind session is the foreground (active) one. Maintained by
    /// `Control::SetSessionActive` (the `session` actor). While `false` the
    /// physical `EVIOCGRAB` is dropped on every pad and their events are ignored,
    /// so a VT-switched-to session (Plasma/Bigscreen) owns the controller.
    /// Orthogonal to `presenter`. Defaults `true`.
    session_active: bool,

    /// Per-app input-contract resolver (built-in defaults + `[input.contracts]`
    /// overrides from config.toml). Cloned once at startup from
    /// [`crate::daemon_config`]; follow-focus consults it to pick the presenter
    /// for a focused window class (gamepad→Game, keyboard→Keyboard,
    /// handoff→Handoff). Read-only after startup — contracts change on restart.
    contracts: InputContracts,

    /// True only while [`Presenter::Handoff`] was entered by the explicit
    /// `handoff` IPC (the Moonlight stream path, #221). A pinned Handoff is
    /// **never** overridden by follow-focus — the streamed window holds
    /// compositor focus for the whole stream, and only the shell's explicit
    /// `grab` ends it. A Handoff reached instead via a `handoff` *contract*
    /// (follow-focus) is NOT pinned, so focus moving to another app/the shell
    /// arbitrates normally (no stranding). Set by `handoff_all`; cleared by every
    /// other presenter transition.
    handoff_pinned: bool,

    /// Latest focused-window class awaiting the follow-focus settle debounce
    /// ([`FOCUS_SETTLE_MS`]). A focus change stores the class here and (re)arms
    /// the settle timer; the transition is applied only when
    /// [`Internal::FocusSettle`] fires for the live [`Self::focus_settle_gen`].
    /// `None` when no focus change is pending.
    pending_focus_class: Option<String>,
    /// Generation of the in-flight follow-focus settle timer. Bumped on every
    /// focus change so a superseded timer's `FocusSettle` is ignored — only the
    /// newest pending focus is applied.
    focus_settle_gen: u64,
    /// Handle to the in-flight settle timer task, so a re-arm aborts the prior
    /// one. Bounds the live settle tasks to exactly one during a focus-churn
    /// burst instead of accumulating sleeping tasks (each superseded by the
    /// `focus_settle_gen` check anyway; this just frees them eagerly). `None`
    /// when no settle is pending.
    focus_settle_task: Option<JoinHandle<()>>,

    /// Cached `rumbleEnabled` setting (#108) — refreshed on `set-config` via
    /// `Control::ConfigChanged` instead of re-reading settings.json on every
    /// rumble.
    rumble_enabled: bool,

    /// Per-player binding overrides (#104). Keyed by player slot (0..=3).
    /// Refreshed on `Control::ConfigChanged`.
    per_player_bindings: HashMap<u8, HashMap<&'static str, u16>>,
    /// Per-game binding overrides (#104). Keyed by game id string.
    /// Refreshed on `Control::ConfigChanged`.
    per_game_bindings: HashMap<String, HashMap<&'static str, u16>>,
    /// The currently active game id for per-game binding lookup (#104).
    /// Set via `set-active-game <id>` IPC; in-memory only (not persisted).
    active_game: Option<String>,

    /// Shared observability counters. Incremented on the relevant events
    /// (intents, transitions, pad join/leave) and read by the metrics exporter.
    metrics: std::sync::Arc<crate::metrics::Metrics>,

    /// Meta (BTN_MODE / Guide) tap-vs-hold threshold. Read once at startup from
    /// `daemon_config::global().input.meta_hold_ms` (like `contracts`); the Meta
    /// hold timer sleeps this long before firing the shell escape. Replaces the
    /// former hard-coded `config::HOME_HOLD_SECS`.
    meta_hold: Duration,
    /// Combo settle window. Read once at startup from
    /// `daemon_config::global().input.combo_guard_ms`; the combo buffer replays a
    /// buffered participant to the app if no combo completes within this window.
    combo_guard: Duration,
}

impl Shared {
    fn publish(&self, ev: Event) {
        // One chokepoint for every broadcast event — `RUST_LOG=…input=debug`
        // turns this into a full event tracer (intents, combos, pad:*,
        // input-mode, controller-wake, status pushes).
        debug!(event = %ev, "publish");
        // Observability: this is also the single chokepoint for the
        // intent/pad-join/pad-leave counters — every intent (IPC, HTTP, MCP,
        // gamepad Home-tap/hold) and every pad connect/disconnect funnels here.
        match &ev {
            Event::Intent(_) => self.metrics.inc_intents(),
            Event::PadConnected(_) => self.metrics.inc_pad_joins(),
            Event::PadDisconnected(_) => self.metrics.inc_pad_leaves(),
            _ => {}
        }
        let _ = self.events.send(ev);
    }

    fn emit_key(&mut self, key: u16, value: i32) {
        // value: 1=press, 0=release, 2=autorepeat. The nav keys/intents the
        // shell sees originate here; trace-level so a stick auto-repeat burst
        // doesn't drown the debug stream.
        trace!(key, value, "emit_key");
        let _ = self.kb.emit(&[InputEvent::new(EV_KEY, key, value)]);
    }

    fn emit_mouse_button(&mut self, button: u16, value: i32) {
        trace!(button, value, "emit_mouse_button");
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

    /// Resolve the keyboard key for a remappable button press from the layered
    /// bindings (#104). Resolution order: game override → player override →
    /// global. Returns `None` when `button` is not an assigned action button.
    ///
    /// Only call this for remappable buttons on a keydown event — the hot path
    /// is allocation-light (≤4 actions via `config::resolve_button_key`).
    fn resolved_key(&self, slot: u8, button: u16) -> Option<u16> {
        let player = self.per_player_bindings.get(&slot);
        let game = self
            .active_game
            .as_deref()
            .and_then(|id| self.per_game_bindings.get(id));
        config::resolve_button_key(&self.bindings, player, game, button)
    }
}

/// Supervise the input runtime: run the event loop, and on a panic respawn a
/// fresh runtime a bounded number of times before staying **dead but detectable**.
///
/// A panic in [`run`]'s select loop used to silently kill the input OS thread and
/// drop the moved `control_rx`, after which every input-runtime IPC command
/// degraded with no recovery short of a whole-daemon restart — the "stuck
/// controller, only a daemon restart fixes it" class. The supervisor breaks that:
///
/// * `control_rx` (the single-owner control channel) plus the shared handles
///   (`events`, `config_changed`, `metrics`, `active_window_rx`) are RETAINED here
///   and re-passed on each attempt, so a panic never drops the control channel.
/// * Each attempt builds a fresh [`Fleet`] INSIDE [`run`]. A panic that unwinds out
///   of the loop drops that `Fleet` (and every [`PadDevice`]); dropping a pad closes
///   its evdev fd, and the kernel `EVIOCGRAB` is tied to the fd — so an fd close is
///   an implicit ungrab. That is what releases the grabs on panic; the respawned
///   attempt re-discovers and re-grabs cleanly. (There is no `panic="abort"` in
///   Cargo.toml, so unwinding — hence [`std::panic::catch_unwind`] — works.)
///
/// Bounded retries with a short growing backoff; after they are exhausted the
/// supervisor sets the up-gauge to 0 and returns rather than looping forever or
/// aborting the process — the daemon stays alive so `/metrics` and the IPC wire
/// (`error:input-runtime-down`) keep reporting the death. A clean (non-panic)
/// return from [`run`] is a normal shutdown (`Control::Shutdown` or a closed
/// control channel) and exits the supervisor WITHOUT counting a restart.
pub async fn run_supervised(
    mut control_rx: mpsc::Receiver<Control>,
    events: broadcast::Sender<Event>,
    config_changed: std::sync::Arc<tokio::sync::Notify>,
    metrics: std::sync::Arc<crate::metrics::Metrics>,
    active_window_rx: watch::Receiver<String>,
    shell_focus_tx: watch::Sender<bool>,
) {
    // Bounded respawns: enough to ride out a transient fault, few enough that a
    // hard-looping panic gives up quickly and stays visibly down.
    const MAX_RESTARTS: u32 = 3;
    let mut restarts: u32 = 0;
    loop {
        metrics.set_runtime_up(true);
        // `AssertUnwindSafe`: `run` borrows `&mut control_rx` and clones the shared
        // handles, none of which observe a broken invariant across the unwind — the
        // Fleet is created fresh inside `run` and dropped by the unwind, and the
        // control channel is just a queue. A clean return = normal shutdown.
        let outcome = AssertUnwindSafe(run(
            &mut control_rx,
            events.clone(),
            std::sync::Arc::clone(&config_changed),
            std::sync::Arc::clone(&metrics),
            active_window_rx.clone(),
            shell_focus_tx.clone(),
        ))
        .catch_unwind()
        .await;

        match outcome {
            Ok(()) => {
                // Normal shutdown (Control::Shutdown / control channel closed).
                metrics.set_runtime_up(false);
                return;
            }
            Err(payload) => {
                // The loop body panicked and unwound: the Fleet was dropped, so
                // every pad's fd is closed and its EVIOCGRAB released. The up-gauge
                // drops to 0 on EVERY caught panic (correct in both cases below).
                let msg = panic_payload_str(payload.as_ref());
                metrics.set_runtime_up(false);
                restarts += 1;
                if restarts > MAX_RESTARTS {
                    error!(
                        panic = %msg,
                        restarts,
                        "input runtime panicked and exhausted restarts; staying down (up-gauge 0, IPC replies error:input-runtime-down)"
                    );
                    // Terminal panic: no respawn happens here, so do NOT count it
                    // as a restart — the counter means actual respawns, not panics.
                    // Stay dead but detectable — do NOT abort the process.
                    return;
                }
                // A respawn WILL occur (attempts remain): count it now, so
                // `tv_shell_input_runtime_restarts_total` advances exactly once
                // per actual re-invocation of `run` (never on the terminal panic).
                metrics.inc_runtime_restarts();
                // Growing backoff: 200ms, 400ms, 800ms.
                let backoff = Duration::from_millis(200u64 << (restarts - 1));
                error!(
                    panic = %msg,
                    restart = restarts,
                    backoff_ms = backoff.as_millis() as u64,
                    "input runtime panicked; respawning after backoff"
                );
                tokio::time::sleep(backoff).await;
            }
        }
    }
}

/// Extract a human-readable message from a caught panic payload (the `Err` of
/// [`std::panic::catch_unwind`]). Panics usually carry a `&str` or `String`; a
/// non-string payload degrades to a placeholder rather than being lost.
fn panic_payload_str(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Entry point: build the daemon and run the event loop until `Shutdown`.
///
/// Called via [`run_supervised`], which owns `control_rx` and re-invokes this on a
/// panic; hence the `&mut` borrow of the receiver (it must survive a respawn) and
/// the by-value shared handles (cloned fresh per attempt by the supervisor). A
/// clean return (`Control::Shutdown` or a closed `control_rx`) means normal
/// shutdown; a panic unwinds out to the supervisor.
///
/// Receives external `config:changed` signals from the file-watch actor via a
/// dedicated [`tokio::sync::Notify`] (not the global broadcast bus) so that
/// the input runtime does not hold a permanent broadcast receiver — which
/// would prevent `receiver_count()==0` fast-paths like `notify_held_buttons()`.
/// Both paths (IPC `set-config` → `Control::ConfigChanged` and file-watch →
/// `config_changed` notification) converge on the same `apply_config_changed`
/// helper, so cache state is always consistent (#163).
///
/// Compositor focus changes arrive over `active_window_rx` (a coalescing
/// [`tokio::sync::watch`]) rather than the control channel — focus is state, so
/// latest-wins is correct and a busy loop can never drop or back up a change.
pub async fn run(
    control_rx: &mut mpsc::Receiver<Control>,
    events: broadcast::Sender<Event>,
    config_changed: std::sync::Arc<tokio::sync::Notify>,
    metrics: std::sync::Arc<crate::metrics::Metrics>,
    mut active_window_rx: watch::Receiver<String>,
    shell_focus_tx: watch::Sender<bool>,
) {
    let (internal_tx, mut internal_rx) = mpsc::channel::<Internal>(256);

    let bindings = config::load_bindings(&config::settings_path());
    let mut button_map = HashMap::new();
    for b in &bindings {
        button_map.insert(b.button, b.key);
    }
    // Parse per-player/per-game binding overrides (#104) and rumbleEnabled from a
    // single settings.json read, offloaded to the blocking pool so the input
    // runtime's startup isn't stalled on disk I/O (consistent with
    // apply_config_changed's later reads).
    let startup_doc = read_settings_doc().await;
    let per_player_bindings = config::parse_per_player_bindings(&startup_doc);
    let per_game_bindings = config::parse_per_game_bindings(&startup_doc);
    let rumble_enabled = config::rumble_enabled_from(&startup_doc);

    let (kb, mouse) = match build_uinput(&button_map) {
        Ok(v) => v,
        Err(e) => {
            error!("uinput init failed (need /dev/uinput access): {e}");
            return;
        }
    };

    // Device-ownership registry (replaces the old `is_synthetic` name match).
    // Only per-player virtual pads need registering — they advertise BTN_SOUTH
    // and copy the physical pad's input_id, so discovery would otherwise grab
    // them. The virtual keyboard/mouse don't advertise BTN_SOUTH, so discovery's
    // has_btn_south filter already excludes them; no need to track them here.
    let reg = VirtualRegistry::new();

    let db = device::load_db();
    if db.is_empty() {
        warn!("controller DB is empty; non-pinned discovery will reject all pads — set TV_SHELL_GAMECONTROLLERDB or pin GAMEPAD_VENDOR/GAMEPAD_PRODUCT");
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
        per_player_bindings,
        per_game_bindings,
        active_game: None,
        input_mode: InputMode::Controller,
        gen_seq: 0,
        pending_capture: None,
        capture_timeout_task: None,
        capture_gen: 0,
        home_hold_active: false,
        presenter: Presenter::Shell,
        overlay_focus: false,
        shell_focus: true,
        shell_focus_tx,
        session_active: true,
        // Per-app contracts + the Meta/combo timing knobs are read once at
        // startup (like every other config.toml section); `global()` is populated
        // in main before this thread spawns, and re-read cleanly on a supervisor
        // respawn.
        contracts: crate::daemon_config::global().input_contracts(),
        meta_hold: Duration::from_millis(crate::daemon_config::global().input.meta_hold_ms),
        combo_guard: Duration::from_millis(crate::daemon_config::global().input.combo_guard_ms),
        handoff_pinned: false,
        pending_focus_class: None,
        focus_settle_gen: 0,
        focus_settle_task: None,
        // rumble_enabled is derived from the single offloaded startup settings
        // read (M3), superseding origin/main's inline config::rumble_enabled call.
        rumble_enabled,
        metrics,
    };
    let mut fleet = Fleet::new();

    // Guards the `active_window_rx.changed()` arm: once every watch sender is
    // dropped (shutdown) `changed()` resolves `Err` immediately on every poll, so
    // we disable the arm to avoid a busy-spin. In normal operation `Shutdown`
    // arrives over `control_rx` first and breaks the loop.
    let mut focus_watch_live = true;

    loop {
        tokio::select! {
            // Control commands from the IPC server.
            ctrl = control_rx.recv() => {
                match ctrl {
                    Some(c) => if !handle_control(&mut sh, &mut fleet, c).await { break },
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
                        // Observability: count every raw evdev event we read from
                        // the fleet (the hot path), whether or not it is acted on.
                        sh.metrics.inc_input_events();
                        // While our session is backgrounded the pad is ungrabbed
                        // and the foreground compositor also receives this event;
                        // read-and-drop so nothing is injected via uinput.
                        if sh.session_active {
                            if let Some(pad) = fleet.pads.get_mut(&fd) {
                                pad.handle_event(&mut sh, ev);
                            }
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
            // Reconnect / hot-join poll: discover any new pad not already owned,
            // and refresh each pad's battery (#100) — a change emits
            // `pad:battery:*`. A no-op for wired pads.
            _ = tokio::time::sleep(Duration::from_secs(2)) => {
                try_join(&mut sh, &mut fleet);
                // Offload the per-pad sysfs battery scan to the blocking pool so the
                // input loop stays responsive while walking /sys/class/power_supply.
                let names: Vec<(RawFd, String)> = fleet
                    .pads
                    .iter()
                    .map(|(fd, pad)| (*fd, pad.name.clone()))
                    .collect();
                if !names.is_empty() {
                    let results = tokio::task::spawn_blocking(move || {
                        names
                            .into_iter()
                            .map(|(fd, name)| (fd, read_pad_battery(&name)))
                            .collect::<Vec<_>>()
                    })
                    .await
                    .unwrap_or_default();
                    for (fd, state) in results {
                        if let Some(pad) = fleet.pads.get_mut(&fd) {
                            pad.apply_battery(&mut sh, state);
                        }
                    }
                }
            }
            // External config:changed signal (#163): the file-watch actor
            // detected an external edit to settings.json and notified via the
            // dedicated Notify channel. Refresh in-memory caches exactly as
            // the IPC set-config path does via Control::ConfigChanged, so
            // rumbleEnabled and per-player/per-game bindings stay current
            // without a daemon restart. Using a Notify instead of subscribing
            // to the global broadcast bus avoids a permanent receiver that
            // would defeat receiver_count()==0 fast-paths.
            _ = config_changed.notified() => {
                tracing::debug!("input: config_changed notified, refreshing caches");
                apply_config_changed(&mut sh).await;
            }
            // Coalesced compositor focus (latest-wins) from the Hyprland actor.
            // `borrow_and_update` marks the value seen so `changed()` re-fires only
            // on a genuinely newer focus. Instead of applying immediately, arm the
            // settle debounce (schedule_focus_change) so a burst of launch/close
            // focus flaps collapses to one net transition and never thrashes the
            // virtual pad (which Steam re-enumerates on each create/destroy). The
            // startup "" never fires (watch signals only post-construction values),
            // and even a spurious "" just settles to a no-op. See #221/#294.
            res = active_window_rx.changed(), if focus_watch_live => {
                if res.is_ok() {
                    let class = active_window_rx.borrow_and_update().to_string();
                    schedule_focus_change(&mut sh, &class);
                } else {
                    // All watch senders dropped (shutdown in progress); stop
                    // polling this arm so it can't busy-spin.
                    focus_watch_live = false;
                }
            }
        }
    }

    // On shutdown release each pad's kernel grab (kept in both presenters during
    // normal operation), reset stick state, and abort tasks. Releasing the grab
    // lets the next daemon/compositor own the pad cleanly.
    let fds: Vec<RawFd> = fleet.pads.keys().copied().collect();
    for fd in fds {
        if let Some(mut pad) = fleet.pads.remove(&fd) {
            pad.ungrab(&mut sh); // also resets stick state
            pad.abort_all_tasks();
        }
    }
    info!("Shutting down");
}

// --- control handling ----------------------------------------------------

/// Read and parse `settings.json` off the input runtime's event loop.
///
/// The actual disk read + JSON parse runs on Tokio's blocking pool via
/// `spawn_blocking`, so the single input task stays responsive to gamepad
/// events and timers while waiting. A missing/garbage file degrades to an empty
/// object (matching the prior inline `unwrap_or` behavior). If the blocking task
/// somehow fails to join (runtime shutting down), it also degrades to empty.
async fn read_settings_doc() -> serde_json::Value {
    tokio::task::spawn_blocking(|| {
        std::fs::read_to_string(config::settings_path())
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()))
    })
    .await
    .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()))
}

/// Apply an already-read settings document to the input runtime's in-memory
/// caches (#163, #108). Pure (no I/O) so the disk read can be offloaded by the
/// caller; both refresh paths converge here so they apply identical logic.
fn apply_config_doc(sh: &mut Shared, doc: &serde_json::Value) {
    sh.rumble_enabled = config::rumble_enabled_from(doc);
    sh.per_player_bindings = config::parse_per_player_bindings(doc);
    sh.per_game_bindings = config::parse_per_game_bindings(doc);
}

/// Refresh the input runtime's caches from disk, reading `settings.json` off the
/// event loop. Called by both the `Control::ConfigChanged` arm (IPC `set-config`
/// path) and the `config_changed.notified()` select arm (file-watch path).
async fn apply_config_changed(sh: &mut Shared) {
    let doc = read_settings_doc().await;
    apply_config_doc(sh, &doc);
}

/// Returns false to stop the loop (shutdown).
async fn handle_control(sh: &mut Shared, fleet: &mut Fleet, ctrl: Control) -> bool {
    match ctrl {
        Control::Grab(r) => {
            grab_all(sh, fleet);
            let _ = r.send(resp_ok());
        }
        Control::Release(r) => {
            release_all(sh, fleet);
            let _ = r.send(resp_ok());
        }
        Control::Handoff(r) => {
            // Replies `ok` unconditionally, mirroring Grab/Release above:
            // ungrab()/enter_shell() are best-effort and log internally on the
            // rare failure (the ungrab ioctl can only fail on an already-dead
            // fd, which on_pad_leave reaps). A stuck grab is self-correcting —
            // the shell re-emits `grab` on every stream-exit path (see the
            // handoff section in docs/IPC_PROTOCOL.md).
            handoff_all(sh, fleet);
            let _ = r.send(resp_ok());
        }
        Control::Status(r) => {
            let _ = r.send(fleet.status_string(sh.presenter));
        }
        Control::GetPads(r) => {
            let _ = r.send(fleet.pads_json());
        }
        Control::ListInputDevices(r) => {
            // #97 diagnostics enumerator. evdev::enumerate() is synchronous and can be
            // slow on a host with many input devices, so run it off the input runtime
            // (#108) — the fleet's grabbed paths are the only runtime state it needs.
            let grabbed_paths: HashSet<PathBuf> = fleet
                .pads
                .values()
                .filter(|p| p.grabbed)
                .map(|p| p.path.clone())
                .collect();
            tokio::task::spawn_blocking(move || {
                let _ = r.send(list_input_devices_with(grabbed_paths));
            });
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
        Control::Rumble { id, ms, reply } => {
            // Fire a rumble on the pad with this wire id (#99). Gated by the
            // persisted `rumbleEnabled` setting and the pad's FF capability;
            // a missing pad / disabled setting / no-FF pad is a clean no-op that
            // still replies `ok` (the shell shouldn't treat "no rumble hardware"
            // as an error). `ms` is clamped to u16 (kernel FF replay length).
            if sh.rumble_enabled {
                if let Some(pad) = fleet.find_by_wire_id_mut(&id) {
                    pad.rumble(ms.min(u16::MAX as u32) as u16);
                }
            }
            let _ = reply.send(resp_ok());
        }
        Control::Key { name, reply } => {
            // Synthesize a keystroke (press+release) on the shared virtual
            // keyboard — the headless counterpart to a gamepad d-pad/A/B tap or
            // a `wtype -k`, reaching whatever surface holds Wayland focus. Unlike
            // `intent`, this deliberately touches the device. Unknown names are
            // rejected without emitting.
            match config::key_for_action(&name) {
                Some(code) => {
                    sh.emit_key(code, 1);
                    sh.emit_key(code, 0);
                    let _ = reply.send(resp_ok());
                }
                None => {
                    let _ = reply.send(resp_unknown_key(&name));
                }
            }
        }
        Control::ConfigChanged => {
            // set-config or file-watch: refresh cached settings (#108, #104, #163).
            apply_config_changed(sh).await;
        }
        Control::SetActiveGame { id, reply } => {
            // Set or clear the active game for per-game binding lookup (#104).
            // In-memory only — not persisted to settings.json.
            sh.active_game = id;
            let _ = reply.send(resp_ok());
        }
        Control::ShellFocus { on, reply } => {
            // The shell declared whether it owns (or shares) the screen. Flip
            // routing to the shell key-map + force-grab (on) / restore the base
            // presenter's grab (off) without touching `sh.presenter`, and gate
            // follow-focus so a stale `activewindow` class cannot hand the pad to
            // a backgrounded app while the shell is on screen.
            set_shell_focus(sh, fleet, on);
            let _ = reply.send(resp_ok());
        }
        Control::OverlayFocus { on, reply } => {
            // #262: a modal shell overlay opened/closed over a running app.
            // Flip routing to the shell key-map + force-grab (on) / restore the
            // base presenter's grab (off) without touching `sh.presenter`.
            set_overlay_focus(sh, fleet, on);
            let _ = reply.send(resp_ok());
        }
        Control::PadBatteryQuery { id, reply } => {
            // #160: reply with battery state for the pad identified by wire id.
            // A wired pad (no battery sysfs entry) replies `present:false`;
            // an unknown id replies `error:pad not found '<id>'`.
            match fleet.find_by_wire_id_mut(&id) {
                None => {
                    let _ = reply.send(resp_pad_not_found(&id));
                }
                Some(pad) => {
                    let resp = match pad.battery {
                        None => resp_pad_battery_not_present(&id),
                        Some(bs) => resp_pad_battery_present(&id, bs.level, bs.charging),
                    };
                    let _ = reply.send(resp);
                }
            }
        }
        Control::PadRumbleStatus { id, reply } => {
            // #160: reply with rumble capability and whether it is currently
            // enabled (the `rumbleEnabled` setting). `supported=true` iff the
            // pad has an uploaded `ff_effect`.
            match fleet.find_by_wire_id_mut(&id) {
                None => {
                    let _ = reply.send(resp_pad_not_found(&id));
                }
                Some(pad) => {
                    let supported = pad.ff_effect.is_some();
                    let resp = resp_pad_rumble_status(&id, supported, sh.rumble_enabled);
                    let _ = reply.send(resp);
                }
            }
        }
        Control::ControllerDbRefreshed { reply } => {
            // #159: the IPC layer fetched a fresh upstream DB and updated the
            // on-disk cache; re-read the merged DB (bundled baseline + cache +
            // env override) and merge it into the live runtime so new
            // controllers are identified without a daemon restart. The runtime's
            // initial DB comes from `device::load_db()` (baseline + env); the
            // cached upstream entries are folded in here, on refresh.
            let (fresh, _source) = crate::controllerdb::load_merged_db();
            sh.db.merge(&fresh);
            let _ = reply.send(resp_ok());
        }
        Control::SetSessionActive(active) => {
            if active != sh.session_active {
                sh.session_active = active;
                if active {
                    info!(
                        pads = fleet.pads.len(),
                        "session active -> re-grabbing pads"
                    );
                    for pad in fleet.pads.values_mut() {
                        pad.grab(sh);
                    }
                } else {
                    info!(
                        pads = fleet.pads.len(),
                        "session inactive -> releasing pads"
                    );
                    for pad in fleet.pads.values_mut() {
                        pad.ungrab(sh);
                        // Drop any in-flight combo buffer: a backgrounded session's
                        // events are ignored, so a partial sequence must not linger
                        // and replay when the session reactivates.
                        pad.reset_combo_buffer(sh);
                    }
                }
            }
        }

        Control::Shutdown => return false,
    }
    true
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
            // Fleet-level dedup: publish the escape intent only on the 0->1 edge
            // of the latch, so two pads holding Home at once fire it once (a
            // double home-tap would toggle the drawer open then shut). The latch
            // clears (loop event arm) when no pad holds Home. Single-pad: the latch
            // toggles with the one pad, identical to the old behavior.
            //
            // Which intent depends on the presenter (see `hold_fire_intent`): the
            // Shell home publishes `home-hold` (idle reset), an APP presenter
            // publishes `home-tap` (the controllable overlay drawer over the app).
            if !sh.home_hold_active {
                sh.home_hold_active = true;
                let routed = route_presenter(sh.shell_focus, sh.overlay_focus, sh.presenter);
                sh.publish(Event::Intent(hold_fire_intent(routed).into()));
            }
        }
        Internal::ComboGuardFired { fd, generation } => {
            let Some(pad) = fleet.pads.get_mut(&fd) else {
                return; // pad left
            };
            // Stale (disarmed/rearmed since) or no longer buffering: ignore.
            if generation != pad.combo_guard_gen || !pad.combo_armed {
                return;
            }
            pad.combo_guard_task = None;
            // A combo can complete via a button that never routes through the
            // combo gate: the end-session chord is Home+B, and Home (`BTN_MODE`)
            // goes through `handle_meta`, not the buffer — so `combo_buffer_action`
            // never sees such a combo as matched. Re-check the LIVE held set at
            // fire time: if a combo is now held, SWALLOW the buffer (clear, don't
            // replay — replaying the buffered B would leak it into the app after
            // the chord already fired). Otherwise the settle window genuinely
            // elapsed with no combo.
            if state::any_combo_matched(&pad.held_keys) {
                pad.reset_combo_buffer(sh);
                return;
            }
            // No combo: the buffered participants were not a chord. Replay them to
            // the app (in order) via the current routed presenter and disarm. If
            // the presenter changed since arming, a transition already reset the
            // buffer, so this is a no-op.
            let routed = route_presenter(sh.shell_focus, sh.overlay_focus, sh.presenter);
            pad.replay_combo_buffer(sh, routed);
            pad.disarm_combo_guard(sh);
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
        Internal::FocusSettle { generation } => {
            // The follow-focus settle debounce elapsed. Ignore a superseded tick
            // (focus moved again before this timer fired) — only the newest
            // pending focus is applied, so a launch/close flap burst yields a
            // single net presenter transition.
            if generation != sh.focus_settle_gen {
                return;
            }
            // This is the live timer firing (not a superseded one): it has
            // completed by sending this message, so drop its now-finished handle.
            // A superseded timer never reaches here (gen mismatch above), so this
            // can't clear the handle of a newer armed timer.
            sh.focus_settle_task = None;
            if let Some(class) = sh.pending_focus_class.take() {
                apply_focus_change(sh, fleet, &class);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Axis {
    X,
    Y,
}
