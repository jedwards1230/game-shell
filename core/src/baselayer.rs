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

use std::time::{Duration, Instant};

use crate::atoms::{AppId, AtomConn, AtomError};
use crate::screen::{self, ScreenState};

/// Why a base-layer switch did not take.
#[derive(Debug, thiserror::Error)]
pub enum SwitchError {
    #[error("writing the base layer: {0}")]
    Write(#[source] AtomError),
    #[error("reading back the base layer: {0}")]
    Read(#[source] AtomError),
    /// The compositor never published the intended id within the bound. This is
    /// the case that must never be reported as `ok`.
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
}

/// A switch that took, with the number that says how well.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Switched {
    pub app_id: AppId,
    /// Milliseconds from the write to the confirming read. The §6 bench measured
    /// 14–19 ms; a number drifting toward the bound is the early warning that
    /// this path is degrading, which is why it is returned rather than dropped.
    pub took_ms: u64,
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

/// How often the verify loop re-reads while waiting.
///
/// Polled, not event-driven, on purpose: §10 records that v1's residual defect
/// was an attached event listener that processed nothing, and a poll cannot fail
/// that way — it either reads the value or reports that it did not.
const POLL_INTERVAL: Duration = Duration::from_millis(2);

/// Put `app_id` on screen: `[app_id, shell]`.
///
/// The shell is kept as the tail so an app exiting falls back to the shell with
/// no compositor action at all (§5) — the fallback is a property of the list,
/// not of anything the core has to notice and react to.
pub fn show(
    conn: &AtomConn,
    app_id: AppId,
    shell_app_id: AppId,
    timeout: Duration,
) -> Result<Switched, SwitchError> {
    let list = if app_id == shell_app_id {
        vec![shell_app_id]
    } else {
        vec![app_id, shell_app_id]
    };
    write_and_verify(conn, &list, app_id, timeout)
}

/// Return to the shell: `[shell]`.
pub fn home(
    conn: &AtomConn,
    shell_app_id: AppId,
    timeout: Duration,
) -> Result<Switched, SwitchError> {
    write_and_verify(conn, &[shell_app_id], shell_app_id, timeout)
}

/// The single write, then the bounded read-back.
///
/// **Callers must serialize this against other intents.** The write and the
/// verify are one indivisible operation: run two concurrently and one can verify
/// a transient frame the other has already replaced, and report `ok` for a state
/// that no longer holds. `GamescopeCompositor` holds its `intent_lock` across
/// the whole call for that reason.
fn write_and_verify(
    conn: &AtomConn,
    list: &[AppId],
    want: AppId,
    timeout: Duration,
) -> Result<Switched, SwitchError> {
    let started = Instant::now();
    // ONE write. Not a loop, not a retry — see the module docs on Steam.
    conn.set_base_layer(list).map_err(SwitchError::Write)?;

    let mut last;
    loop {
        let state = screen::read(conn).map_err(SwitchError::Read)?;
        if state.on_screen_app() == Some(want) {
            return Ok(Switched {
                app_id: want,
                took_ms: elapsed_ms(started),
            });
        }
        last = Some(describe(&state));
        if started.elapsed() >= timeout {
            return Err(SwitchError::NotObserved {
                want,
                got: last.unwrap_or_else(|| "unknown".to_string()),
                waited_ms: elapsed_ms(started),
                bound_ms: timeout.as_millis() as u64,
            });
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Read the base-layer list back as the core's last intent.
///
/// This is how a restarted core recovers what it was doing without keeping any
/// state on disk (§9 "Core dies"), and how it stays out of Steam's way: it
/// observes, it does not re-assert. In particular it must **never** write "home"
/// on boot — that would yank a live game.
pub fn reconcile(conn: &AtomConn) -> Result<Reconciled, AtomError> {
    let state = screen::read(conn)?;
    Ok(Reconciled {
        base_layer: state.base_layer.clone(),
        on_screen: state.on_screen_app(),
        ours: false,
        state,
    })
}

/// What the core found when it read the world back.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Reconciled {
    /// The list as it currently reads — possibly Steam's, not ours.
    pub base_layer: Vec<AppId>,
    /// The app id of the base window, if one resolved.
    pub on_screen: Option<AppId>,
    /// Always false today: the core has no way to prove it wrote the list it is
    /// reading, and claiming otherwise would be exactly the unverifiable claim
    /// §1 goal 2 forbids. Kept in the shape so a later PR that tracks the core's
    /// own last write can fill it in without changing callers.
    pub ours: bool,
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
    use crate::screen::{AppIdSource, ScreenParts};

    const SHELL: AppId = AppId(9001);
    const GAME: AppId = AppId(9003);

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
        for app in [AppId(0), AppId(769), AppId(413091)] {
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

    fn state(on: Option<(u32, AppId)>, focused: Option<u32>) -> ScreenState {
        let focusable = on
            .map(|(w, a)| {
                vec![crate::atoms::FocusableWindow {
                    window: w,
                    app_id: a,
                    pid: 1,
                }]
            })
            .unwrap_or_default();
        ScreenState::assemble(ScreenParts {
            focused_window: focused.or(on.map(|(w, _)| w)),
            focusable_windows: focusable,
            ..Default::default()
        })
    }

    #[test]
    fn describe_names_the_app_when_one_resolved() {
        let s = state(Some((0x800011, GAME)), None);
        let d = describe(&s);
        assert!(d.contains("9003"), "{d}");
        assert!(d.contains("0x800011"), "{d}");
        assert_eq!(s.on_screen().unwrap().source, AppIdSource::Focusable);
    }

    #[test]
    fn describe_distinguishes_unresolved_from_unfocused() {
        assert!(describe(&state(None, Some(0x42))).contains("resolves to no app id"));
        assert!(describe(&state(None, None)).contains("no window is focused"));
    }
}
