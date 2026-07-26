//! CEC display-ownership cache with change timestamps, published on
//! `GET /status`.
//!
//! The CEC actor already tracks display ownership passively, folding received
//! `<Active Source>` / `<Inactive Source>` traffic into a cell that gates its
//! own transmits (`cec.rs`). That cell was a bare `AtomicI32` — a single current
//! value with no timestamp — and was exposed nowhere. This module is that cell
//! plus the two facts an automation consumer needs, **when did it last change**
//! and **have we ever heard a claim at all**, in a cross-platform form the HTTP
//! bridge can read.
//!
//! **The fail-safe direction is INVERTED here — read this before consuming it.**
//! For the transmit gate, "we don't own the display" is the safe answer: it
//! declines to touch the bus. For the opposite use case — suspend this box when
//! nobody is looking at it — treating "unknown" as "not focused" would suspend a
//! machine someone is actively watching. So this module reports a **tri-state**
//! ([`Ownership`]) and only [`Ownership::OwnedByOther`] means *another device
//! positively claimed the display*. [`Ownership::Unknown`] means we have no
//! evidence either way and **must not** be treated as "unfocused".
//!
//! **Not a staleness signal.** Unlike [`crate::shell_state`], there is no
//! heartbeat and therefore no `stale` verdict: CEC ownership is edge-driven, so
//! a claim observed six hours ago is still the current truth. The published age
//! is *how long the current owner has held the display*, not how old/untrusted
//! the reading is — hence `held_seconds`, not `age_seconds`.
//!
//! **History lives in the log, not here.** One current value plus its change
//! timestamp is all this cell carries: it is written from libcec's own callback
//! thread, which may not block or re-enter libcec, so it is lock-free atomics
//! (the same constraint that shaped the original `AtomicI32`). `cec.rs` logs
//! every observed transition, so the journal is the replayable history.
//!
//! Cross-platform on purpose: plain atomics + serde, no libcec and no
//! Linux-only imports, so `http.rs` can read it in every build. In a build
//! without `--features cec` nothing ever writes it and `GET /status` honestly
//! reports `unknown` / never-observed.

use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};

/// Owner value meaning "no ownership claim is currently held": nothing observed
/// yet, the owner sent `<Inactive Source>`, or the connection was reopened and
/// any claim made while it was down was missed. Mirrors `cec::OWNER_UNSEEN`.
pub const OWNER_UNSEEN: i32 = -1;

/// The wire's "no logical address" slot (`Unregistered`, 0xF). It is an answer,
/// not a device — mirrors `cec::is_addressable`, which treats it as nobody.
pub const UNREGISTERED_ADDRESS: i32 = 15;

/// Whether `addr` names a real device that could own a display: a CEC logical
/// address in `0..=14`. [`OWNER_UNSEEN`] and [`UNREGISTERED_ADDRESS`] are both
/// "nobody". Pure — this is the cross-platform twin of `cec::is_addressable`,
/// and `cec.rs` has a test asserting the two agree over all 16 addresses.
pub fn is_addressable(addr: i32) -> bool {
    (0..UNREGISTERED_ADDRESS).contains(&addr)
}

/// Shared handle to the ownership cache, mirroring
/// [`crate::shell_state::SharedShellState`] in role: the CEC worker writes it,
/// the HTTP bridge reads it. It is `Arc<DisplayOwner>` rather than
/// `Arc<RwLock<_>>` because the writer is libcec's callback thread — see the
/// module docs.
pub type SharedDisplayOwner = std::sync::Arc<DisplayOwner>;

/// Build an empty (never-observed) cache.
pub fn shared() -> SharedDisplayOwner {
    std::sync::Arc::new(DisplayOwner::new())
}

/// A recorded ownership change, returned so the caller can log it. `previous`
/// and `current` are logical addresses or [`OWNER_UNSEEN`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub previous: i32,
    pub current: i32,
}

/// Passively-tracked display ownership: who last claimed the display, when that
/// last changed, and whether the bus has ever told us anything.
///
/// Every field is atomic and written with `Relaxed` ordering, for the same
/// reason the original cell was: these are standalone values that order no other
/// memory, and a reader racing an in-flight bus event is inherent to CEC.
#[derive(Debug)]
pub struct DisplayOwner {
    /// Logical address of the device that currently holds the display, or
    /// [`OWNER_UNSEEN`].
    owner: AtomicI32,
    /// Our OWN CEC logical address, as libcec reports it, or [`OWNER_UNSEEN`]
    /// while unknown. Without it "is the owner us?" is unanswerable, which is
    /// why it is published rather than folded away.
    ours: AtomicI32,
    /// Unix seconds when `owner` last changed. `0` = never changed (same
    /// "never" sentinel as `shell_state::ShellState::last_push_unix`).
    changed_unix: AtomicU64,
    /// Whether an ownership claim has ever been RECEIVED from the bus since
    /// daemon start. Deliberately not set by our own claims ([`Self::record_local`]):
    /// its whole job is to answer "does this bus actually broadcast
    /// `<Active Source>`?", and self-claims would make that always-true.
    ever_observed: AtomicBool,
    /// Whether a receive callback is actually attached to a live CEC
    /// connection. Without it, `ever_observed == false` would conflate three
    /// very different situations: no `--features cec` build, CEC lifecycle
    /// disabled or the adapter unavailable, and a bus that simply never
    /// broadcasts `<Active Source>`. Only the last is a finding.
    tracking_active: AtomicBool,
}

impl Default for DisplayOwner {
    fn default() -> Self {
        Self::new()
    }
}

impl DisplayOwner {
    /// A cache that has seen nothing. Written out rather than derived, because a
    /// derived `Default` would zero `owner`/`ours` — and `0` is the TV's logical
    /// address, i.e. "the TV owns the display", not "unknown".
    pub fn new() -> Self {
        Self {
            owner: AtomicI32::new(OWNER_UNSEEN),
            ours: AtomicI32::new(OWNER_UNSEEN),
            changed_unix: AtomicU64::new(0),
            ever_observed: AtomicBool::new(false),
            tracking_active: AtomicBool::new(false),
        }
    }

    /// Record an ownership claim **received from the bus**. Sets the
    /// ever-observed flag (this is the only path that does). Returns the
    /// transition when the owner actually changed, so the caller logs edges
    /// rather than every repeat broadcast.
    pub fn observe(&self, owner: i32, now_unix: u64) -> Option<Transition> {
        self.ever_observed.store(true, Ordering::Relaxed);
        self.store_owner(owner, now_unix)
    }

    /// Record an ownership value we know locally rather than from the bus: our
    /// own successful `<Active Source>` claim (which never comes back through
    /// the receive callback), or the reset to [`OWNER_UNSEEN`] on a connection
    /// reopen. Does NOT set the ever-observed flag.
    pub fn record_local(&self, owner: i32, now_unix: u64) -> Option<Transition> {
        self.store_owner(owner, now_unix)
    }

    /// Record our own CEC logical address (or [`OWNER_UNSEEN`] when libcec
    /// cannot tell us). Not an ownership change — it never touches
    /// `changed_unix`.
    pub fn set_ours(&self, ours: i32) {
        self.ours.store(ours, Ordering::Relaxed);
    }

    /// Declare whether a receive callback is attached to a live connection.
    /// Called by the CEC worker once the adapter is open; never true in a build
    /// without `--features cec`.
    pub fn set_tracking_active(&self, active: bool) {
        self.tracking_active.store(active, Ordering::Relaxed);
    }

    /// Current owner, or [`OWNER_UNSEEN`].
    pub fn owner(&self) -> i32 {
        self.owner.load(Ordering::Relaxed)
    }

    /// Point-in-time copy for the status assembler.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            owner: self.owner.load(Ordering::Relaxed),
            ours: self.ours.load(Ordering::Relaxed),
            changed_unix: self.changed_unix.load(Ordering::Relaxed),
            ever_observed: self.ever_observed.load(Ordering::Relaxed),
            tracking_active: self.tracking_active.load(Ordering::Relaxed),
        }
    }

    /// Swap in `owner`, stamping `changed_unix` only on a real change.
    ///
    /// The two stores are not atomic together, so a reader can momentarily see a
    /// new owner with the old timestamp. That is a sub-microsecond window on a
    /// signal whose meaningful resolution is seconds, and the alternative — a
    /// lock — is exactly what the libcec callback thread may not take.
    fn store_owner(&self, owner: i32, now_unix: u64) -> Option<Transition> {
        let previous = self.owner.swap(owner, Ordering::Relaxed);
        if previous == owner {
            return None;
        }
        self.changed_unix.store(now_unix, Ordering::Relaxed);
        Some(Transition {
            previous,
            current: owner,
        })
    }
}

/// A consistent-enough read of [`DisplayOwner`] for the pure status assembler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    pub owner: i32,
    pub ours: i32,
    pub changed_unix: u64,
    pub ever_observed: bool,
    pub tracking_active: bool,
}

impl Default for Snapshot {
    fn default() -> Self {
        DisplayOwner::new().snapshot()
    }
}

/// Who holds the display, as a tri-state. **Only [`Ownership::OwnedByOther`] is
/// positive evidence that someone switched away from us**; see the module docs
/// on the inverted fail-safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Ownership {
    /// We are the active source (equivalent to `cec::owns_display`).
    OwnedByUs,
    /// A different real device claimed the display. The ONLY suspend-eligible
    /// value.
    OwnedByOther,
    /// No claim observed, the owner went inactive, the connection was reopened,
    /// or our own address is undeterminable. **Never suspend on this.**
    Unknown,
}

/// Pure ownership decision.
///
/// Mirrors the asymmetry of `cec::owns_display` deliberately: proof is required
/// in BOTH directions, so an unknown owner *or* an unknown self-address yields
/// [`Ownership::Unknown`] rather than guessing. `now` is not an input — CEC
/// ownership does not expire.
pub fn classify(owner: i32, ours: i32) -> Ownership {
    if !is_addressable(owner) || !is_addressable(ours) {
        return Ownership::Unknown;
    }
    if owner == ours {
        Ownership::OwnedByUs
    } else {
        Ownership::OwnedByOther
    }
}

/// The CEC-ownership half of the `GET /status` body, flattened alongside the
/// shell-state fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisplayOwnerStatus {
    /// Tri-state verdict. `owned_by_other` is the only value that positively
    /// means "someone is watching a different input".
    pub cec_display_ownership: Ownership,
    /// The owning device's CEC logical address, verbatim (`0` = TV, `5` = AVR,
    /// `4`/`8`/`11` = playback devices). `null` when nobody currently holds a
    /// claim.
    pub cec_display_owner: Option<i32>,
    /// Our own CEC logical address, or `null` when libcec cannot tell us. When
    /// this is `null` the verdict can never be `owned_by_us`/`owned_by_other` —
    /// published so that case is diagnosable rather than mysterious.
    pub cec_local_address: Option<i32>,
    /// Unix seconds of the last ownership change; `null` if it has never
    /// changed.
    pub cec_display_owner_changed_unix: Option<u64>,
    /// How long the current owner has held the display, in seconds. **Not a
    /// staleness measure** — a large value means "unchanged for a long time",
    /// which is normal. `null` if it has never changed.
    pub cec_display_owner_held_seconds: Option<u64>,
    /// Whether an `<Active Source>`/`<Inactive Source>` has EVER been received
    /// from the bus since daemon start. Read it together with
    /// `cec_display_owner_tracking`: `tracking: true, ever_observed: false` is
    /// the only combination that means "we are listening and this bus has never
    /// announced an ownership change".
    pub cec_display_owner_ever_observed: bool,
    /// Whether the daemon is actually listening: a CEC receive callback is
    /// attached to an open adapter. `false` when the daemon was built without
    /// `--features cec`, when the CEC lifecycle is disabled, or when the adapter
    /// never opened — in which case every other field here is a default, not an
    /// observation.
    pub cec_display_owner_tracking: bool,
}

/// Pure assembly of the ownership half of `GET /status`.
///
/// `now_unix` is injected — never sampled in here — so the truth table can be
/// exercised directly, matching `shell_state::status`. A clock that has stepped
/// backwards clamps `held_seconds` to `0` instead of underflowing the `u64`.
pub fn status(snapshot: &Snapshot, now_unix: u64) -> DisplayOwnerStatus {
    let changed = (snapshot.changed_unix != 0).then_some(snapshot.changed_unix);
    DisplayOwnerStatus {
        cec_display_ownership: classify(snapshot.owner, snapshot.ours),
        cec_display_owner: is_addressable(snapshot.owner).then_some(snapshot.owner),
        cec_local_address: is_addressable(snapshot.ours).then_some(snapshot.ours),
        cec_display_owner_changed_unix: changed,
        cec_display_owner_held_seconds: changed.map(|c| now_unix.saturating_sub(c)),
        cec_display_owner_ever_observed: snapshot.ever_observed,
        cec_display_owner_tracking: snapshot.tracking_active,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The address matrix used by the classification tables: the two "nobody"
    /// values plus a spread of real devices.
    const TV: i32 = 0;
    const AVR: i32 = 5;
    const PLAYBACK1: i32 = 4;

    #[test]
    fn is_addressable_excludes_unseen_and_unregistered() {
        // Table-driven so the arguments aren't compile-time constants (a literal
        // `assert!(is_addressable(0))` trips clippy's assertions_on_constants).
        let cases: [(i32, bool); 8] = [
            (OWNER_UNSEEN, false),
            (-2, false),
            (TV, true),
            (PLAYBACK1, true),
            (AVR, true),
            (14, true), // Freeuse — a real slot
            (UNREGISTERED_ADDRESS, false),
            (16, false), // off the wire entirely
        ];
        for (addr, want) in cases {
            assert_eq!(is_addressable(addr), want, "is_addressable({addr})");
        }
    }

    #[test]
    fn classify_requires_proof_in_both_directions() {
        // (owner, ours, want)
        let cases: [(i32, i32, Ownership); 10] = [
            // Positive proof it's us.
            (PLAYBACK1, PLAYBACK1, Ownership::OwnedByUs),
            (TV, TV, Ownership::OwnedByUs),
            // Positive proof it's someone else — the ONLY suspend-eligible case.
            (TV, PLAYBACK1, Ownership::OwnedByOther),
            (AVR, PLAYBACK1, Ownership::OwnedByOther),
            (PLAYBACK1, AVR, Ownership::OwnedByOther),
            // Never observed / went inactive / reopened: unknown, NOT
            // "unfocused". This is the case that must never suspend a box
            // someone is watching.
            (OWNER_UNSEEN, PLAYBACK1, Ownership::Unknown),
            // Unregistered is the wire's "no address", not a device.
            (UNREGISTERED_ADDRESS, PLAYBACK1, Ownership::Unknown),
            // We don't know our own address ⇒ we cannot tell "us" from "other".
            (PLAYBACK1, OWNER_UNSEEN, Ownership::Unknown),
            (PLAYBACK1, UNREGISTERED_ADDRESS, Ownership::Unknown),
            // Nothing known at all.
            (OWNER_UNSEEN, OWNER_UNSEEN, Ownership::Unknown),
        ];
        for (owner, ours, want) in cases {
            assert_eq!(classify(owner, ours), want, "classify({owner}, {ours})");
        }
    }

    #[test]
    fn observe_stamps_only_real_changes() {
        let cell = DisplayOwner::new();
        assert_eq!(cell.owner(), OWNER_UNSEEN);

        // First claim: a change, stamped.
        assert_eq!(
            cell.observe(PLAYBACK1, 1_000),
            Some(Transition {
                previous: OWNER_UNSEEN,
                current: PLAYBACK1
            })
        );
        assert_eq!(cell.snapshot().changed_unix, 1_000);

        // A repeat broadcast of the SAME owner is not a transition and must not
        // move the timestamp — otherwise "held for" would reset on every
        // periodic re-announce.
        assert_eq!(cell.observe(PLAYBACK1, 1_050), None);
        assert_eq!(cell.snapshot().changed_unix, 1_000);

        // A different device taking over is a change.
        assert_eq!(
            cell.observe(TV, 1_060),
            Some(Transition {
                previous: PLAYBACK1,
                current: TV
            })
        );
        assert_eq!(cell.snapshot().changed_unix, 1_060);
    }

    #[test]
    fn ever_observed_tracks_bus_traffic_not_our_own_claims() {
        // A self-claim must NOT look like evidence the bus broadcasts
        // <Active Source> — that flag is how we tell "correctly reports unknown"
        // from "the receive callback never fires".
        let local_only = DisplayOwner::new();
        local_only.record_local(PLAYBACK1, 1_000);
        let snap = local_only.snapshot();
        assert!(!snap.ever_observed);
        assert_eq!(snap.owner, PLAYBACK1);
        assert_eq!(snap.changed_unix, 1_000);

        // A received claim sets it, and it stays set across a reopen reset —
        // the bus demonstrably talks, even if we currently know nothing.
        let from_bus = DisplayOwner::new();
        from_bus.observe(TV, 1_000);
        assert!(from_bus.snapshot().ever_observed);
        from_bus.record_local(OWNER_UNSEEN, 1_200);
        let after_reset = from_bus.snapshot();
        assert!(after_reset.ever_observed);
        assert_eq!(after_reset.owner, OWNER_UNSEEN);
        assert_eq!(after_reset.changed_unix, 1_200);
        assert_eq!(
            classify(after_reset.owner, after_reset.ours),
            Ownership::Unknown
        );
    }

    #[test]
    fn set_ours_is_not_an_ownership_change() {
        let cell = DisplayOwner::new();
        cell.observe(PLAYBACK1, 1_000);
        cell.set_ours(PLAYBACK1);
        let snap = cell.snapshot();
        assert_eq!(snap.changed_unix, 1_000, "set_ours must not restamp");
        assert_eq!(classify(snap.owner, snap.ours), Ownership::OwnedByUs);
    }

    #[test]
    fn status_truth_table() {
        // (snapshot, now, want)
        let cases: [(Snapshot, u64, DisplayOwnerStatus); 5] = [
            // Never observed anything AND not listening — the "this daemon has
            // no CEC tracking at all" reading, which must not be mistaken for
            // "the bus is silent".
            (
                Snapshot::default(),
                1_000,
                DisplayOwnerStatus {
                    cec_display_ownership: Ownership::Unknown,
                    cec_display_owner: None,
                    cec_local_address: None,
                    cec_display_owner_changed_unix: None,
                    cec_display_owner_held_seconds: None,
                    cec_display_owner_ever_observed: false,
                    cec_display_owner_tracking: false,
                },
            ),
            // Listening, our own address known, but the bus has never announced
            // ownership. THIS is the reading that says "0x82 was never seen".
            (
                Snapshot {
                    owner: OWNER_UNSEEN,
                    ours: PLAYBACK1,
                    changed_unix: 0,
                    ever_observed: false,
                    tracking_active: true,
                },
                1_000,
                DisplayOwnerStatus {
                    cec_display_ownership: Ownership::Unknown,
                    cec_display_owner: None,
                    cec_local_address: Some(PLAYBACK1),
                    cec_display_owner_changed_unix: None,
                    cec_display_owner_held_seconds: None,
                    cec_display_owner_ever_observed: false,
                    cec_display_owner_tracking: true,
                },
            ),
            // Another device holds it — the suspend-eligible reading.
            (
                Snapshot {
                    owner: TV,
                    ours: PLAYBACK1,
                    changed_unix: 1_000,
                    ever_observed: true,
                    tracking_active: true,
                },
                1_042,
                DisplayOwnerStatus {
                    cec_display_ownership: Ownership::OwnedByOther,
                    cec_display_owner: Some(TV),
                    cec_local_address: Some(PLAYBACK1),
                    cec_display_owner_changed_unix: Some(1_000),
                    cec_display_owner_held_seconds: Some(42),
                    cec_display_owner_ever_observed: true,
                    cec_display_owner_tracking: true,
                },
            ),
            // We hold it: held_seconds grows without ever becoming "stale".
            (
                Snapshot {
                    owner: PLAYBACK1,
                    ours: PLAYBACK1,
                    changed_unix: 1_000,
                    ever_observed: true,
                    tracking_active: true,
                },
                90_000,
                DisplayOwnerStatus {
                    cec_display_ownership: Ownership::OwnedByUs,
                    cec_display_owner: Some(PLAYBACK1),
                    cec_local_address: Some(PLAYBACK1),
                    cec_display_owner_changed_unix: Some(1_000),
                    cec_display_owner_held_seconds: Some(89_000),
                    cec_display_owner_ever_observed: true,
                    cec_display_owner_tracking: true,
                },
            ),
            // Clock stepped backwards: clamp to 0 rather than underflowing to
            // ~u64::MAX (which would read as an absurd hold time).
            (
                Snapshot {
                    owner: PLAYBACK1,
                    ours: PLAYBACK1,
                    changed_unix: 1_000,
                    ever_observed: true,
                    tracking_active: true,
                },
                900,
                DisplayOwnerStatus {
                    cec_display_ownership: Ownership::OwnedByUs,
                    cec_display_owner: Some(PLAYBACK1),
                    cec_local_address: Some(PLAYBACK1),
                    cec_display_owner_changed_unix: Some(1_000),
                    cec_display_owner_held_seconds: Some(0),
                    cec_display_owner_ever_observed: true,
                    cec_display_owner_tracking: true,
                },
            ),
        ];
        for (snapshot, now, want) in cases {
            assert_eq!(status(&snapshot, now), want, "status({snapshot:?}, {now})");
        }
    }

    #[test]
    fn status_serialises_with_json_nulls_when_nothing_observed() {
        let json = serde_json::to_string(&status(&Snapshot::default(), 42)).unwrap();
        assert!(
            json.contains(r#""cec_display_ownership":"unknown""#),
            "got: {json}"
        );
        assert!(json.contains(r#""cec_display_owner":null"#), "got: {json}");
        assert!(json.contains(r#""cec_local_address":null"#), "got: {json}");
        assert!(
            json.contains(r#""cec_display_owner_changed_unix":null"#),
            "got: {json}"
        );
        assert!(
            json.contains(r#""cec_display_owner_held_seconds":null"#),
            "got: {json}"
        );
        assert!(
            json.contains(r#""cec_display_owner_ever_observed":false"#),
            "got: {json}"
        );
        assert!(
            json.contains(r#""cec_display_owner_tracking":false"#),
            "got: {json}"
        );
    }

    #[test]
    fn tracking_flag_is_independent_of_observations() {
        // A daemon that is listening but has heard nothing must be
        // distinguishable from one that is not listening at all — otherwise
        // "correctly reports unknown" and "the callback never fires" look
        // identical from outside.
        let cell = DisplayOwner::new();
        assert!(!cell.snapshot().tracking_active);
        cell.set_tracking_active(true);
        let listening = cell.snapshot();
        assert!(listening.tracking_active);
        assert!(!listening.ever_observed);
        assert_eq!(
            status(&listening, 5).cec_display_ownership,
            Ownership::Unknown
        );
    }

    #[test]
    fn ownership_serialises_as_snake_case_words() {
        let cases: [(Ownership, &str); 3] = [
            (Ownership::OwnedByUs, r#""owned_by_us""#),
            (Ownership::OwnedByOther, r#""owned_by_other""#),
            (Ownership::Unknown, r#""unknown""#),
        ];
        for (value, want) in cases {
            assert_eq!(serde_json::to_string(&value).unwrap(), want);
        }
    }
}
