//! Base-layer policy: `show(app_id)` and `home()`.
//!
//! # One write, then verify
//!
//! V2_DESIGN §5: a switch is ONE `GAMESCOPECTRL_BASELAYER_APPID` write followed
//! by a read of `GAMESCOPE_FOCUSED_WINDOW`'s app id within a bounded window
//! (measured 14–19 ms, 20 of 20). **A mismatch is a typed error, a metric and a
//! log line, never `ok`.**
//!
//! Of those three, this PR ships **the typed error** ([`SwitchError`]) and **the
//! log line** (the `tracing::error!` in [`crate::compositor`]). The metric
//! arrives with the observability surface — `/metrics`, MQTT and the rest of §4's
//! carried-over transports — in a later PR; there is no metrics registry in this
//! crate to emit into yet.
//!
//! That last clause is the whole point of this module. v1 returned `ok` for a
//! dropped launch (#376), an escape that could not leave fullscreen (#436), an
//! unparked window (#448), a stopped heartbeat (#402) and a compositor wedged
//! for nine days (#383). Every one of those was a success report that was not
//! true. Here, the only way to get `Ok` out of [`show`] or [`home`] is for the
//! compositor to have published the intended app id as the focused window's id.
//!
//! # Two waits, not one, and they must not be confused
//!
//! §5's 14–19 ms is the time to switch between windows that are **already
//! mapped**. A freshly launched app takes *seconds* to map its first window, and
//! for all of that time the base-layer list is correct and the compositor is
//! behaving perfectly — there is simply nothing yet to put on screen.
//!
//! Verifying both against the same 250 ms bound made `show <id>` immediately
//! after `launch <id>` fail every single time, on a launch that was working. That
//! is worse than useless: it trains a caller to ignore `base layer did not take`,
//! which is the one error in this crate that must never be ignored.
//!
//! So the verify has two bounds, and the failures are two different variants:
//!
//! | State | Bound | Failure |
//! |---|---|---|
//! | The target has a mapped window, but another app is on screen | `switch` (250 ms) | [`SwitchError::NotObserved`] — a real failed switch, loud and fast |
//! | The target has no mapped window at all | `map` (seconds) | [`SwitchError::NeverMapped`] — the app never came up |
//!
//! The switch clock starts when the target's first window appears, not when the
//! write went out, so a launch that takes ten seconds to map still gets the full
//! 250 ms of switch headroom afterwards and a genuinely stuck switch is still
//! caught 250 ms after it could first have succeeded. Both remain errors — a
//! `show` that did not put the app on screen never replies `ok` — but a caller
//! can tell "your app did not start" from "the compositor ignored me".
//!
//! # NOT IMPLEMENTED HERE: §5's transient-unmap hysteresis
//!
//! §5 also requires the core to pin `GAMESCOPECTRL_BASELAYER_WINDOW` across
//! known transitions (Moonlight's main window handing off to its stream window,
//! a browser navigation) and to apply a short hysteresis before treating a
//! fallback to the shell as an app exit. **None of that exists in this crate.**
//! The atom is read into [`ScreenState::base_layer_windows`] and never written,
//! and nothing watches for an exit. Said plainly here rather than left implied,
//! because a reader of the two waits above could reasonably assume the third
//! case is handled too. It is not, and until it is, a transient unmap will drop
//! the base layer to the shell.
//!
//! # Steam wins this atom, and that is designed for, not fought
//!
//! §9, measured 2026-09-05: while the Steam client runs it owns
//! `GAMESCOPECTRL_BASELAYER_APPID` outright — it rewrote the list on every
//! stream start and stop and dropped our id from `GAMESCOPE_FOCUSABLE_APPS`
//! entirely. Steam is an active adversary on this atom, not a source of drift.
//!
//! So this module **never busy-loops trying to hold the atom.** It writes once,
//! per expressed intent, and verifies once. [`reconcile`] is the read side: it
//! reports the list back as the core's last intent (which is also how a
//! restarted core recovers its state without keeping anything on disk), and it
//! deliberately does not re-assert. Re-assertion happens only when the core has
//! an intent of its own to express — i.e. when something calls [`show`] or
//! [`home`].

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::atoms::{AppId, AtomConn, AtomError};
use crate::screen::{self, ScreenState};

/// The two operations a base-layer switch is made of, as a seam.
///
/// Exists so [`write_and_verify`] — the function that decides whether a switch
/// took — is testable **without an X server**. Before this trait the whole
/// "a failed switch is never `ok`" rule was defended only by an IPC test whose
/// fake hardcoded the error string, so the rule could be inverted in this file
/// and every test still passed. A seam is what makes the rule falsifiable.
pub trait BaseLayer {
    /// Replace the ordered base-layer app-id list.
    fn set_base_layer(&self, ids: &[AppId]) -> Result<(), AtomError>;
    /// Read the compositor's published state back.
    fn screen(&self) -> Result<ScreenState, AtomError>;
}

impl BaseLayer for AtomConn {
    fn set_base_layer(&self, ids: &[AppId]) -> Result<(), AtomError> {
        AtomConn::set_base_layer(self, ids)
    }
    fn screen(&self) -> Result<ScreenState, AtomError> {
        screen::read(self)
    }
}

/// Why a base-layer switch did not take.
#[derive(Debug, thiserror::Error)]
pub enum SwitchError {
    #[error("writing the base layer: {0}")]
    Write(#[source] AtomError),
    #[error("reading back the base layer: {0}")]
    Read(#[source] AtomError),
    /// The target's window was mapped and the compositor still did not put it on
    /// screen. This is the case that must never be reported as `ok`, and the one
    /// that means the compositor boundary is misbehaving.
    #[error(
        "base layer did not take: asked for app {want}, {got} after {waited_ms} ms \
         (bound {bound_ms} ms)"
    )]
    NotObserved {
        want: AppId,
        /// A human-readable description of what was on screen instead.
        got: String,
        waited_ms: u64,
        bound_ms: u64,
    },
    /// The base-layer list was written and accepted, but the app never mapped a
    /// window for the compositor to show. Distinct from [`Self::NotObserved`] on
    /// purpose: the switch is fine, the app is not.
    #[error(
        "app {want} never mapped a window within {waited_ms} ms (bound {bound_ms} ms); \
         the base layer was set, so this is the app failing to start, not the switch \
         failing to take ({got})"
    )]
    NeverMapped {
        want: AppId,
        got: String,
        waited_ms: u64,
        bound_ms: u64,
    },
}

/// A switch that took, with the numbers that say how well.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Switched {
    pub app_id: AppId,
    /// Milliseconds from the write to the confirming read. The §6 bench measured
    /// 14–19 ms; a number drifting toward the bound is the early warning that
    /// this path is degrading, which is why it is returned rather than dropped.
    pub took_ms: u64,
    /// Of [`Self::took_ms`], how long was spent waiting for the app's first
    /// window to map. Zero for an ordinary switch between mapped windows;
    /// seconds for a switch that raced a launch. Reported separately so the
    /// switch number stays comparable with the bench.
    pub waited_for_map_ms: u64,
}

/// How long to wait for the compositor to publish the switch.
///
/// The measurement is 14–19 ms over 20 switches. 250 ms is an order of magnitude
/// of headroom — enough that a slow frame, a hotplug settle or a loaded box
/// cannot produce a false failure, and short enough that a real failure surfaces
/// inside one user interaction rather than looking like a hang. Configurable via
/// `[session].switch_timeout_ms` because the margin is a guess about hardware we
/// have measured exactly one of.
///
/// This is the SINGLE source of that number: [`crate::config::SessionConfig`]'s
/// default derives `switch_timeout_ms` from it rather than repeating the
/// literal, so the bound cannot be changed here and left stale there.
pub const DEFAULT_SWITCH_TIMEOUT: Duration = Duration::from_millis(250);

/// How long to wait for a not-yet-mapped app to map its first window.
///
/// Unmeasured, and deliberately generous: this is a cold app start on a 4K
/// desktop — Steam, a browser, Moonlight negotiating a stream — and the cost of
/// being too short is reporting a working launch as a failure. The cost of being
/// too long is only that a genuinely dead launch takes this long to say so, and
/// [`SwitchError::NeverMapped`] names the app when it does.
pub const DEFAULT_MAP_TIMEOUT: Duration = Duration::from_millis(30_000);

/// How often the verify loop re-reads while waiting.
///
/// Polled, not event-driven, on purpose: §10 records that v1's residual defect
/// was an attached event listener that processed nothing, and a poll cannot fail
/// that way — it either reads the value or reports that it did not.
const POLL_INTERVAL: Duration = Duration::from_millis(2);

/// The two bounds a switch is verified against. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadlines {
    /// How long the compositor gets to put an ALREADY-MAPPED window on screen.
    pub switch: Duration,
    /// How long the app gets to map its first window at all.
    pub map: Duration,
}

impl Default for Deadlines {
    fn default() -> Self {
        Self {
            switch: DEFAULT_SWITCH_TIMEOUT,
            map: DEFAULT_MAP_TIMEOUT,
        }
    }
}

/// Serializes one whole intent — a write AND its verify — against other intents.
///
/// **"One write, then verify" is only a real primitive if it is atomic.** The
/// IPC server runs one task per connection with nothing shared between them, so
/// a `show A` and a concurrent `home` would otherwise interleave: A's write,
/// home's write, then A's verify catching a transient frame in which A was still
/// on screen — and returning `ok` for a state another intent had already
/// replaced. That is a success report that is not true, which is the exact class
/// this module exists to eliminate; a verify that can observe someone else's
/// window is not a verify.
///
/// A `std::sync::Mutex`, not a tokio one, on purpose: every compositor call is
/// blocking X I/O dispatched through `spawn_blocking`, so this lock is never
/// held across an `.await` and a blocking mutex is the correct primitive.
///
/// It lives here rather than as a bare field on `GamescopeCompositor` so it can
/// be tested: constructing a `GamescopeCompositor` needs an X server, and a lock
/// nothing can exercise is a lock nobody knows is held.
#[derive(Debug, Default)]
pub struct IntentGate {
    lock: Mutex<()>,
}

impl IntentGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run `f` with no other intent in flight.
    ///
    /// A poisoned lock is recovered from rather than propagated. The data it
    /// guards is `()` — there is no invariant to have been corrupted — so
    /// refusing every subsequent switch would turn one panic into a permanently
    /// unswitchable screen. Same recovery idiom as `daemon/src/daemon_config.rs`.
    pub fn run<T>(&self, f: impl FnOnce() -> T) -> T {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f()
    }
}

/// Put `app_id` on screen: `[app_id, shell]`.
///
/// The shell is kept as the tail so an app exiting falls back to the shell with
/// no compositor action at all (§5) — the fallback is a property of the list,
/// not of anything the core has to notice and react to.
pub fn show(
    conn: &impl BaseLayer,
    app_id: AppId,
    shell_app_id: AppId,
    deadlines: Deadlines,
) -> Result<Switched, SwitchError> {
    write_and_verify(conn, &list_for(app_id, shell_app_id), app_id, deadlines)
}

/// Return to the shell: `[shell]`.
pub fn home(
    conn: &impl BaseLayer,
    shell_app_id: AppId,
    deadlines: Deadlines,
) -> Result<Switched, SwitchError> {
    write_and_verify(conn, &[shell_app_id], shell_app_id, deadlines)
}

/// The single write, then the bounded read-back.
///
/// **Callers must serialize this against other intents** — see [`IntentGate`],
/// which is what `GamescopeCompositor` wraps every call in.
pub fn write_and_verify(
    conn: &impl BaseLayer,
    list: &[AppId],
    want: AppId,
    deadlines: Deadlines,
) -> Result<Switched, SwitchError> {
    write_and_verify_with(conn, list, want, deadlines, || {
        std::thread::sleep(POLL_INTERVAL)
    })
}

/// [`write_and_verify`] with the inter-poll wait injected.
///
/// Only so tests do not have to sleep for real; production passes the sleep.
fn write_and_verify_with(
    conn: &impl BaseLayer,
    list: &[AppId],
    want: AppId,
    deadlines: Deadlines,
    mut wait: impl FnMut(),
) -> Result<Switched, SwitchError> {
    let started = Instant::now();
    // ONE write. Not a loop, not a retry — see the module docs on Steam.
    conn.set_base_layer(list).map_err(SwitchError::Write)?;

    // When the target's first window appeared. `None` until it does, which is
    // what selects the map bound over the switch bound.
    let mut mapped_at: Option<Instant> = None;
    loop {
        let state = conn.screen().map_err(SwitchError::Read)?;
        if state.on_screen_app() == Some(want) {
            return Ok(Switched {
                app_id: want,
                took_ms: elapsed_ms(started),
                waited_for_map_ms: mapped_at
                    .map(|t| (t - started).as_millis() as u64)
                    .unwrap_or(0),
            });
        }
        if mapped_at.is_none() && state.is_mapped(want) {
            mapped_at = Some(Instant::now());
        }
        let got = describe(&state);

        match mapped_at {
            // Mapped, and still not on screen: a real failed switch.
            Some(t) if t.elapsed() >= deadlines.switch => {
                return Err(SwitchError::NotObserved {
                    want,
                    got,
                    waited_ms: elapsed_ms(started),
                    bound_ms: deadlines.switch.as_millis() as u64,
                })
            }
            // Never mapped: the app did not come up. Different failure.
            None if started.elapsed() >= deadlines.map => {
                return Err(SwitchError::NeverMapped {
                    want,
                    got,
                    waited_ms: elapsed_ms(started),
                    bound_ms: deadlines.map.as_millis() as u64,
                })
            }
            _ => {}
        }
        wait();
    }
}

/// Read the base-layer list back as the core's last intent.
///
/// This is how a restarted core recovers what it was doing without keeping any
/// state on disk (§9 "Core dies"), and how it stays out of Steam's way: it
/// observes, it does not re-assert. In particular it must **never** write "home"
/// on boot — that would yank a live game.
pub fn reconcile(conn: &impl BaseLayer) -> Result<Reconciled, AtomError> {
    let state = conn.screen()?;
    Ok(Reconciled {
        base_layer: state.base_layer.clone(),
        on_screen: state.on_screen_app(),
        state,
    })
}

/// What the core found when it read the world back.
///
/// There is deliberately **no `ours` flag**. It shipped as a serialized field
/// hardcoded to `false` with a comment promising a later PR would fill it in —
/// which is precisely the "a type nothing constructs is dead code dressed as a
/// contract" rule `protocol.rs` states and this crate otherwise keeps. The core
/// cannot prove it wrote the list it is reading (Steam rewrites the atom
/// underneath it, §9), so the honest shape is not to have the field. It comes
/// back if and when the core tracks its own last write.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Reconciled {
    /// The list as it currently reads — possibly Steam's, not ours.
    pub base_layer: Vec<AppId>,
    /// The app id of the base window, if one resolved.
    pub on_screen: Option<AppId>,
    /// The full snapshot the reconcile was derived from.
    pub state: ScreenState,
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

/// Describe what is on screen, for a failure message an operator can act on.
fn describe(state: &ScreenState) -> String {
    match state.on_screen() {
        Some(on) => format!(
            "app {} is on screen (window {:#x}, via {:?})",
            on.app_id, on.window, on.source
        ),
        None => match state.focused_window {
            Some(w) => format!("window {w:#x} is focused but resolves to no app id"),
            None => "no window is focused".to_string(),
        },
    }
}

/// Build the base-layer list for an intent.
///
/// Split out from [`show`] so the ordering rule — the target first, the shell
/// always last as the no-action exit fallback — is asserted without an X server.
pub fn list_for(app_id: AppId, shell_app_id: AppId) -> Vec<AppId> {
    if app_id == shell_app_id {
        vec![shell_app_id]
    } else {
        vec![app_id, shell_app_id]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atoms::FocusableWindow;
    use crate::screen::{AppIdSource, ScreenParts};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const SHELL: AppId = AppId::new(9001);
    const GAME: AppId = AppId::new(9003);
    const OTHER: AppId = AppId::new(769);
    const GAME_WIN: u32 = 8_388_625;

    // -- the fake compositor -------------------------------------------------

    /// A scripted [`BaseLayer`]: each `screen()` call pops the next state.
    ///
    /// This is the seam that makes the crate's central rule falsifiable. Invert
    /// the `Err(NotObserved)` in `write_and_verify` to an `Ok` and
    /// `a_switch_that_never_takes_is_never_ok` goes red — which is the whole
    /// reason the trait exists.
    struct Scripted {
        states: Mutex<Vec<ScreenState>>,
        writes: Mutex<Vec<Vec<AppId>>>,
        write_error: bool,
    }

    impl Scripted {
        fn new(states: Vec<ScreenState>) -> Self {
            let mut s = states;
            s.reverse();
            Self {
                states: Mutex::new(s),
                writes: Mutex::new(Vec::new()),
                write_error: false,
            }
        }
        /// A compositor stuck in one state forever.
        fn stuck(state: ScreenState) -> Self {
            Self {
                states: Mutex::new(vec![state]),
                writes: Mutex::new(Vec::new()),
                write_error: false,
            }
        }
        fn writes(&self) -> Vec<Vec<AppId>> {
            self.writes.lock().unwrap().clone()
        }
    }

    impl BaseLayer for Scripted {
        fn set_base_layer(&self, ids: &[AppId]) -> Result<(), AtomError> {
            if self.write_error {
                return Err(AtomError::Connect("scripted write failure".into()));
            }
            self.writes.lock().unwrap().push(ids.to_vec());
            Ok(())
        }
        fn screen(&self) -> Result<ScreenState, AtomError> {
            let mut s = self.states.lock().unwrap();
            // The last scripted state repeats, so "stuck forever" is expressible.
            if s.len() > 1 {
                Ok(s.pop().unwrap())
            } else {
                Ok(s[0].clone())
            }
        }
    }

    /// A state in which `on` is the base window, and every app in `mapped` has a
    /// candidate window.
    fn state(on: Option<(u32, AppId)>, mapped: &[AppId]) -> ScreenState {
        let mut focusable: Vec<FocusableWindow> = mapped
            .iter()
            .enumerate()
            .map(|(i, a)| FocusableWindow {
                window: 0x900_000 + i as u32,
                app_id: *a,
                pid: 100 + i as u32,
            })
            .collect();
        if let Some((w, a)) = on {
            focusable.push(FocusableWindow {
                window: w,
                app_id: a,
                pid: 1,
            });
        }
        ScreenState::assemble(ScreenParts {
            focused_window: on.map(|(w, _)| w),
            focusable_windows: focusable,
            ..Default::default()
        })
    }

    fn fast() -> Deadlines {
        // Real durations, tiny ones: the wait closure is a no-op in tests, so
        // the loop spins and these expire in microseconds.
        Deadlines {
            switch: Duration::from_millis(5),
            map: Duration::from_millis(50),
        }
    }

    fn verify(conn: &Scripted, want: AppId, d: Deadlines) -> Result<Switched, SwitchError> {
        write_and_verify_with(conn, &list_for(want, SHELL), want, d, || {})
    }

    // -- the rule this module exists for -------------------------------------

    #[test]
    fn a_switch_that_never_takes_is_never_ok() {
        // The target IS mapped and another app holds the screen: the compositor
        // boundary is misbehaving, and that must surface as NotObserved.
        let c = Scripted::stuck(state(Some((0x800011, OTHER)), &[GAME, OTHER]));
        let err = verify(&c, GAME, fast()).unwrap_err();
        assert!(
            matches!(err, SwitchError::NotObserved { want, .. } if want == GAME),
            "{err}"
        );
        let msg = err.to_string();
        assert!(msg.contains("did not take"), "{msg}");
        assert!(!msg.starts_with("ok"), "{msg}");
    }

    #[test]
    fn a_switch_that_takes_is_ok_and_reports_the_write_that_caused_it() {
        let c = Scripted::new(vec![
            state(None, &[GAME]),
            state(Some((GAME_WIN, GAME)), &[GAME]),
        ]);
        let s = verify(&c, GAME, fast()).unwrap();
        assert_eq!(s.app_id, GAME);
        // Exactly ONE write: not a retry loop (see the module docs on Steam).
        assert_eq!(c.writes(), vec![vec![GAME, SHELL]]);
    }

    #[test]
    fn the_verify_reads_until_the_compositor_catches_up() {
        // Three reads before the switch lands, all inside the bound.
        let c = Scripted::new(vec![
            state(Some((0x1, OTHER)), &[GAME, OTHER]),
            state(Some((0x1, OTHER)), &[GAME, OTHER]),
            state(Some((GAME_WIN, GAME)), &[GAME]),
        ]);
        assert_eq!(verify(&c, GAME, fast()).unwrap().app_id, GAME);
    }

    #[test]
    fn a_write_failure_is_reported_as_a_write_failure_and_nothing_is_verified() {
        let c = Scripted {
            states: Mutex::new(vec![state(Some((GAME_WIN, GAME)), &[GAME])]),
            writes: Mutex::new(Vec::new()),
            write_error: true,
        };
        // Even though the world already reads as "GAME is on screen", a failed
        // write must not be laundered into a success by the verify.
        assert!(matches!(
            verify(&c, GAME, fast()).unwrap_err(),
            SwitchError::Write(_)
        ));
    }

    #[test]
    fn home_writes_the_shell_alone() {
        let c = Scripted::stuck(state(Some((0x1, SHELL)), &[SHELL]));
        home(&c, SHELL, fast()).unwrap();
        assert_eq!(c.writes(), vec![vec![SHELL]]);
    }

    // -- the two waits (M1) --------------------------------------------------

    #[test]
    fn an_app_that_has_not_mapped_yet_is_not_a_failed_switch() {
        // The launch case: the list is written, nothing is on screen, and the
        // target has no window YET. Under one shared bound this was an error on
        // every working launch.
        let mut states = vec![state(None, &[SHELL]); 40];
        states.push(state(Some((GAME_WIN, GAME)), &[GAME]));
        let c = Scripted::new(states);
        let d = Deadlines {
            // A switch bound far shorter than the wait that follows: if the two
            // were the same clock this would fail.
            switch: Duration::from_millis(1),
            map: Duration::from_secs(5),
        };
        let s = verify(&c, GAME, d).unwrap();
        assert_eq!(s.app_id, GAME);
    }

    #[test]
    fn an_app_that_never_maps_is_never_mapped_not_did_not_take() {
        let c = Scripted::stuck(state(None, &[SHELL]));
        let err = verify(&c, GAME, fast()).unwrap_err();
        assert!(
            matches!(err, SwitchError::NeverMapped { want, .. } if want == GAME),
            "{err}"
        );
        let msg = err.to_string();
        assert!(msg.contains("never mapped"), "{msg}");
        assert!(msg.contains("9003"), "{msg}");
        // It must not read as the compositor's fault.
        assert!(!msg.contains("did not take"), "{msg}");
    }

    #[test]
    fn neither_wait_can_report_ok() {
        for c in [
            Scripted::stuck(state(None, &[SHELL])), // never maps
            Scripted::stuck(state(Some((0x1, OTHER)), &[GAME, OTHER])), // never takes
        ] {
            assert!(verify(&c, GAME, fast()).is_err());
        }
    }

    #[test]
    fn the_switch_clock_starts_when_the_window_maps_not_when_the_write_went_out() {
        // Mapped from read 3 onward, but never on screen. The failure must be
        // NotObserved (the switch bound applied from the map), NOT NeverMapped.
        let mut states = vec![state(None, &[SHELL]); 3];
        states.push(state(Some((0x1, OTHER)), &[GAME, OTHER]));
        let c = Scripted::new(states);
        let err = verify(&c, GAME, fast()).unwrap_err();
        assert!(matches!(err, SwitchError::NotObserved { .. }), "{err}");
    }

    #[test]
    fn a_map_wait_is_reported_separately_from_the_switch_time() {
        let mut states = vec![state(None, &[SHELL]); 5];
        states.push(state(Some((GAME_WIN, GAME)), &[GAME]));
        let c = Scripted::new(states);
        let s = verify(
            &c,
            GAME,
            Deadlines {
                switch: Duration::from_millis(5),
                map: Duration::from_secs(5),
            },
        )
        .unwrap();
        // The app never mapped before it was on screen, so there is no separate
        // map wait to report — but the field exists and is never larger than the
        // total, which is the invariant a reader depends on.
        assert!(s.waited_for_map_ms <= s.took_ms);
    }

    // -- the intent gate (H2) ------------------------------------------------

    #[test]
    fn the_intent_gate_admits_one_intent_at_a_time() {
        let gate = Arc::new(IntentGate::new());
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (gate, in_flight, max_seen) =
                    (gate.clone(), in_flight.clone(), max_seen.clone());
                std::thread::spawn(move || {
                    for _ in 0..200 {
                        gate.run(|| {
                            let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                            max_seen.fetch_max(now, Ordering::SeqCst);
                            std::thread::yield_now();
                            in_flight.fetch_sub(1, Ordering::SeqCst);
                        });
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(
            max_seen.load(Ordering::SeqCst),
            1,
            "two intents were in flight at once; a verify that can observe \
             another intent's window is not a verify"
        );
    }

    #[test]
    fn one_panicking_intent_does_not_wedge_the_screen_forever() {
        let gate = Arc::new(IntentGate::new());
        let g = gate.clone();
        let panicked = std::thread::spawn(move || g.run(|| panic!("mid-switch")));
        assert!(panicked.join().is_err());
        // The lock is now poisoned. Refusing every later switch would turn one
        // panic into a permanently unswitchable screen.
        assert_eq!(gate.run(|| 42), 42);
    }

    // -- shape and message ---------------------------------------------------

    #[test]
    fn show_puts_the_app_first_and_the_shell_last() {
        assert_eq!(list_for(GAME, SHELL), vec![GAME, SHELL]);
    }

    #[test]
    fn showing_the_shell_is_home_not_a_duplicated_entry() {
        assert_eq!(list_for(SHELL, SHELL), vec![SHELL]);
    }

    #[test]
    fn the_shell_is_always_the_exit_fallback() {
        // §5: an app exiting falls back to the shell with no compositor action,
        // which is only true if the shell is in the list.
        for app in [AppId::new(0), AppId::new(769), AppId::new(413091)] {
            let list = list_for(app, SHELL);
            assert_eq!(*list.last().unwrap(), SHELL, "list {list:?}");
        }
    }

    #[test]
    fn the_default_bound_has_an_order_of_magnitude_over_the_measurement() {
        // Measured 14-19 ms; anything at or below ~20 ms would flake on a busy
        // box and anything huge would read as a hang.
        assert!(DEFAULT_SWITCH_TIMEOUT >= Duration::from_millis(100));
        assert!(DEFAULT_SWITCH_TIMEOUT <= Duration::from_millis(1000));
        // A cold app start is orders of magnitude slower than a switch, and the
        // whole point of the second bound is that it is not the first one.
        assert!(DEFAULT_MAP_TIMEOUT > DEFAULT_SWITCH_TIMEOUT * 10);
    }

    #[test]
    fn a_mismatch_message_names_both_sides() {
        let err = SwitchError::NotObserved {
            want: GAME,
            got: "app 769 is on screen (window 0x800011, via Focusable)".to_string(),
            waited_ms: 250,
            bound_ms: 250,
        };
        let msg = err.to_string();
        assert!(msg.contains("9003"), "{msg}");
        assert!(msg.contains("769"), "{msg}");
        assert!(msg.contains("250"), "{msg}");
        // It must not read like a success.
        assert!(!msg.starts_with("ok"), "{msg}");
    }

    #[test]
    fn describe_names_the_app_when_one_resolved() {
        let s = state(Some((0x800011, GAME)), &[]);
        let d = describe(&s);
        assert!(d.contains("9003"), "{d}");
        assert!(d.contains("0x800011"), "{d}");
        assert_eq!(s.on_screen().unwrap().source, AppIdSource::Focusable);
    }

    #[test]
    fn describe_distinguishes_unresolved_from_unfocused() {
        let focused_but_unresolved = ScreenState::assemble(ScreenParts {
            focused_window: Some(0x42),
            ..Default::default()
        });
        assert!(describe(&focused_but_unresolved).contains("resolves to no app id"));
        assert!(describe(&state(None, &[])).contains("no window is focused"));
    }

    #[test]
    fn reconcile_reports_the_world_without_writing_to_it() {
        let c = Scripted::stuck(state(Some((GAME_WIN, GAME)), &[GAME]));
        let r = reconcile(&c).unwrap();
        assert_eq!(r.on_screen, Some(GAME));
        // §9: the core NEVER writes on boot — that would yank a live game.
        assert!(c.writes().is_empty(), "reconcile must not assert an intent");
    }
}
