//! Shell-reported UI state, cached for the HTTP bridge's `GET /status`.
//!
//! The daemon **cannot observe** what the QML shell is doing (see the `UiState`
//! note in `bridge_core.rs`: the shell is a wlr-layer-shell surface, so the
//! compositor's answer is always about some other toplevel). The honest source
//! is the shell itself, so it *pushes* its state over IPC (`shell-state`) on the
//! same ~3s heartbeat that already re-asserts `shell-focus`.
//!
//! **The daemon reports; the consumer decides.** There is deliberately no
//! `busy` boolean here. `GET /status` publishes the raw facts — the last state
//! string verbatim, whether media is playing, how old the reading is, and
//! whether quickshell is running at all — and Home Assistant (or anything else)
//! owns the suspend policy. That keeps the rule editable without a daemon
//! release.
//!
//! Unlike `shell-focus` (which becomes a `Control::ShellFocus` consumed by the
//! Linux-only input runtime and is unreachable from `http.rs`), this state is
//! kept in an IPC-layer `Arc<RwLock<_>>` cache — the same idiom as
//! [`crate::ipc::SharedControllerDbState`] — and is threaded into BOTH
//! `ipc::serve` (the writer) and `http::serve` (the reader).
//!
//! Cross-platform: pure data + `tokio::sync`, no Linux-only imports.

use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};

/// The shell's heartbeat period, as implemented in `shell/shell.qml` (a 3000 ms
/// `Timer` that re-sends `shell-focus` and `shell-state`).
pub const HEARTBEAT_SECS: u64 = 3;

/// How old a push may get before the reading is considered stale. Three missed
/// heartbeats — long enough that a single dropped tick or a slow reply doesn't
/// flap the flag, short enough that a wedged/killed shell is visible quickly.
pub const STALE_AFTER_SECS: u64 = 3 * HEARTBEAT_SECS;

/// Shared handle to the shell-state cache, mirroring
/// [`crate::ipc::SharedControllerDbState`]: multiple IPC connections and the
/// HTTP bridge read it concurrently; a `shell-state` push takes the write lock
/// for the (in-memory, non-blocking) swap.
pub type SharedShellState = std::sync::Arc<tokio::sync::RwLock<ShellState>>;

/// Build an empty (never-pushed) cache. `last_push_unix == 0` is the "never"
/// sentinel, matching `controllerdb`'s `last_downloaded`.
pub fn shared() -> SharedShellState {
    std::sync::Arc::new(tokio::sync::RwLock::new(ShellState::default()))
}

/// Current Unix timestamp in seconds (mirrors `controllerdb::now_unix`).
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The last state the shell pushed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShellState {
    /// The shell's state machine value, **verbatim** as the shell sent it
    /// (`idle` / `launching` / `streaming` / `reconnecting` / `appRunning`).
    /// The daemon never maps, normalises, or interprets it — it is an opaque
    /// enum string on this side of the wire. `None` until the first push.
    pub state: Option<String>,
    /// Whether the shell reports media playing.
    pub media_playing: bool,
    /// Unix seconds of the last push. **`0` means "never"** — the same sentinel
    /// `controllerdb::read_last_downloaded` uses.
    pub last_push_unix: u64,
}

impl ShellState {
    /// Record one `shell-state` push. `now_unix` is injected so the write path
    /// stays testable.
    pub fn record(&mut self, state: String, media_playing: bool, now_unix: u64) {
        self.state = Some(state);
        self.media_playing = media_playing;
        self.last_push_unix = now_unix;
    }
}

/// How trustworthy the cached reading is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Freshness {
    /// `true` when the reading must not be trusted — either the shell has never
    /// pushed, or the last push is at/past the staleness threshold.
    pub stale: bool,
    /// Seconds since the last push. `None` when the shell has never pushed
    /// (there is no age to report, and `0` would read as "just now").
    pub age_seconds: Option<u64>,
}

/// Pure staleness decision for a cached push.
///
/// `now_unix` is a parameter — never read from the clock in here — so the truth
/// table below can be exercised directly (same discipline as `wol::pick_mac`
/// taking its lookups as closures).
///
/// Semantics:
/// - `last_push_unix == 0` (never pushed) ⇒ stale, no age.
/// - `age >= stale_after_secs` ⇒ stale. **At** the threshold counts as stale:
///   erring toward "don't trust it" is the safe direction, since a false
///   "fresh `idle`" is what would let a consumer suspend a box that is actually
///   streaming behind a wedged shell.
/// - `now_unix < last_push_unix` (clock skew / NTP step) ⇒ age clamps to `0`
///   rather than underflowing this `u64`.
pub fn evaluate_freshness(last_push_unix: u64, now_unix: u64, stale_after_secs: u64) -> Freshness {
    if last_push_unix == 0 {
        return Freshness {
            stale: true,
            age_seconds: None,
        };
    }
    let age = now_unix.saturating_sub(last_push_unix);
    Freshness {
        stale: age >= stale_after_secs,
        age_seconds: Some(age),
    }
}

/// The `GET /status` body: raw facts only, no policy.
///
/// `shell_state` is the **last-known** value, not necessarily a live one — a
/// consumer must gate on `stale` before acting on it. A stale `"idle"` means
/// "the shell stopped reporting", NOT "the box is idle".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShellStatus {
    /// The shell's last-pushed state string, verbatim. `null` until the shell
    /// has pushed at least once.
    pub shell_state: Option<String>,
    /// The shell's last-pushed media-playing flag (`false` until first push).
    pub media_playing: bool,
    /// `true` when `shell_state`/`media_playing` must not be trusted.
    pub stale: bool,
    /// Seconds since the last push; `null` when the shell has never pushed.
    pub age_seconds: Option<u64>,
    /// The staleness threshold in effect, published so a consumer can see the
    /// contract instead of hard-coding it.
    pub stale_after_seconds: u64,
    /// Whether `pgrep -x quickshell` finds a running shell. Independent of the
    /// push cache: it distinguishes "shell is gone" from "shell is alive but
    /// silent".
    pub shell_running: bool,
}

/// Pure assembly of the `GET /status` body from a cache snapshot.
///
/// `now_unix` and `shell_running` are injected (the caller samples the clock and
/// runs `pgrep`), so this stays a total, side-effect-free function.
pub fn status(snapshot: &ShellState, now_unix: u64, shell_running: bool) -> ShellStatus {
    let freshness = evaluate_freshness(snapshot.last_push_unix, now_unix, STALE_AFTER_SECS);
    ShellStatus {
        shell_state: snapshot.state.clone(),
        media_playing: snapshot.media_playing,
        stale: freshness.stale,
        age_seconds: freshness.age_seconds,
        stale_after_seconds: STALE_AFTER_SECS,
        shell_running,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_after_is_three_heartbeats() {
        assert_eq!(STALE_AFTER_SECS, 3 * HEARTBEAT_SECS);
        assert_eq!(STALE_AFTER_SECS, 9);
    }

    #[test]
    fn evaluate_freshness_truth_table() {
        // Table-driven so the inputs aren't compile-time constants (a literal
        // `assert!(evaluate_freshness(0, 0, 9).stale)` trips clippy's
        // assertions_on_constants).
        // (last_push, now, stale_after, want_stale, want_age)
        let cases: [(u64, u64, u64, bool, Option<u64>); 9] = [
            // Never pushed: the 0 sentinel is stale and has no age at all —
            // NOT age 0, which would read as "pushed just now".
            (0, 1_000, 9, true, None),
            // Never pushed, and the clock is also 0 (degenerate now_unix).
            (0, 0, 9, true, None),
            // Fresh: well inside the window.
            (1_000, 1_000, 9, false, Some(0)),
            (1_000, 1_003, 9, false, Some(3)),
            (1_000, 1_008, 9, false, Some(8)),
            // Exactly at the threshold counts as STALE (safe direction).
            (1_000, 1_009, 9, true, Some(9)),
            // Past the threshold.
            (1_000, 1_060, 9, true, Some(60)),
            // Clock skew: now < last_push clamps to age 0 instead of
            // underflowing the u64 (a naive subtraction panics in debug).
            (1_000, 990, 9, false, Some(0)),
            // Extreme skew, same clamp — must not panic or wrap to ~u64::MAX
            // (which would read as wildly stale).
            (u64::MAX, 1, 9, false, Some(0)),
        ];
        for (last_push, now, stale_after, want_stale, want_age) in cases {
            let got = evaluate_freshness(last_push, now, stale_after);
            assert_eq!(
                got,
                Freshness {
                    stale: want_stale,
                    age_seconds: want_age
                },
                "evaluate_freshness({last_push}, {now}, {stale_after})"
            );
        }
    }

    #[test]
    fn record_then_status_reports_verbatim_state() {
        let mut cache = ShellState::default();
        // Never pushed: null state, stale, no age.
        let empty = status(&cache, 1_000, false);
        assert_eq!(empty.shell_state, None);
        assert!(empty.stale);
        assert_eq!(empty.age_seconds, None);
        assert!(!empty.media_playing);

        // An unrecognised state string must survive untouched — the daemon does
        // not validate the shell's enum vocabulary.
        cache.record("appRunning".to_string(), true, 1_000);
        let fresh = status(&cache, 1_002, true);
        assert_eq!(fresh.shell_state.as_deref(), Some("appRunning"));
        assert!(fresh.media_playing);
        assert!(!fresh.stale);
        assert_eq!(fresh.age_seconds, Some(2));
        assert!(fresh.shell_running);
        assert_eq!(fresh.stale_after_seconds, STALE_AFTER_SECS);

        // Same cache, later clock: the state string is unchanged but the reading
        // is now flagged stale. This is the case that must not fool a consumer.
        let wedged = status(&cache, 1_100, true);
        assert_eq!(wedged.shell_state.as_deref(), Some("appRunning"));
        assert!(wedged.stale);
        assert_eq!(wedged.age_seconds, Some(100));
    }

    #[test]
    fn status_serialises_with_json_nulls_for_never_pushed() {
        let json = serde_json::to_string(&status(&ShellState::default(), 42, false)).unwrap();
        assert!(json.contains(r#""shell_state":null"#), "got: {json}");
        assert!(json.contains(r#""age_seconds":null"#), "got: {json}");
        assert!(json.contains(r#""stale":true"#), "got: {json}");
        assert!(json.contains(r#""shell_running":false"#), "got: {json}");
    }
}
