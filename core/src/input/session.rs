//! The input session: presenters created once, pads claimed and released as
//! they come and go, and events forwarded 1:1 in between.
//!
//! Synchronous by design. Every rule lives here, and the async loop that feeds
//! it lives in [`super::runtime`], so the rules are testable without a reactor,
//! a seat or a device.
//!
//! # The lifecycle, and the one thing that must never happen
//!
//! ```text
//! start()   -> create presenter 0..players   <- ONCE, for the life of the session
//!              register their devnodes as ours
//! poll()    -> enumerate -> plan -> claim (open + EVIOCGRAB) -> admit to a slot
//!                                -> leave  (quiesce the presenter, then release)
//! forward() -> translate one physical event onto the pad's presenter
//! ```
//!
//! `poll` and `forward` never create or destroy a presenter. V2_DESIGN §7:
//! create/destroy is a hotplug event every game and Moonlight forward to the
//! streaming host (jedwards1230/tv-shell#402), so a pad's unplug must be
//! invisible to whatever is reading the presenter. What the game sees instead is
//! a controller that stops moving — which is why a leave *quiesces* rather than
//! simply going quiet.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::backend::{InputBackend, InputError};
use super::config::ResolvedInput;
use super::discovery::{OwnedNodes, Pin, Refusal};
use super::fleet::{Fleet, FleetFull};
use super::identity::ControllerDb;
use super::presenter::{ev, quiesce, translate, DropReason, Forward, PadProfile};

/// A pad newly taken into the fleet.
///
/// Informational: the backend already opened, grabbed and began reading it. The
/// stream is deliberately NOT handed out — the file descriptor is the grab, so
/// splitting the two would make `release` a request rather than a fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Joined {
    pub path: PathBuf,
    pub slot: u8,
}

/// One presenter, as reported.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PresenterReport {
    pub slot: u8,
    pub name: String,
    pub devnodes: Vec<String>,
}

/// One claimed pad, as reported.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PadReport {
    pub slot: u8,
    pub wire_id: String,
    pub name: String,
    pub path: String,
    pub vendor: String,
    pub product: String,
    /// Always `true` for a pad in the fleet: the core does not hold a pad it did
    /// not grab. Reported anyway because "is my controller grabbed" is the
    /// question this verb exists to answer, and answering it by omission is how
    /// a reader ends up guessing.
    pub grabbed: bool,
}

/// One device the gate refused, and why.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RefusedReport {
    pub path: String,
    pub name: String,
    pub vendor: String,
    pub product: String,
    pub guid: String,
    pub reason: Refusal,
    pub explanation: String,
}

/// The `input-state` payload.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InputReport {
    /// False when `[input].enabled` is off, in which case every list below is
    /// empty because nothing was ever opened.
    pub enabled: bool,
    pub players: u8,
    pub presenters: Vec<PresenterReport>,
    pub pads: Vec<PadReport>,
    pub refused: Vec<RefusedReport>,
    /// Events that did not cross onto a presenter, by reason. Present so a pad
    /// losing a button is a number an operator can read rather than a mystery.
    pub drops: BTreeMap<DropReason, u64>,
    /// When the last discovery pass completed, in Unix milliseconds.
    ///
    /// **This is how a stopped input loop becomes visible.** `input-state`
    /// answers from a snapshot so it cannot hang when the loop is wedged, and
    /// the price of that is a report which looks plausible whether the loop is
    /// running or dead. A timestamp that stops advancing distinguishes them.
    /// `None` before the first poll, and while disabled.
    pub last_poll_unix_ms: Option<u64>,
    /// How many discovery passes have COMPLETED.
    ///
    /// Beside the timestamp rather than instead of it, for two different
    /// readers. A person wants the wall clock ("it last looked a minute ago");
    /// a test — and a metric — needs something that changes on every pass, and
    /// a millisecond stamp does not: two polls in the same tick carry the same
    /// value, so an assertion built on the timestamp alone cannot tell "it did
    /// not run" from "it ran again quickly". This can.
    pub polls_completed: u64,
}

impl InputReport {
    /// The report for a core running with input disabled.
    ///
    /// Structurally empty, because with the flag off there is no session: no
    /// enumeration has happened, no device has been opened and no presenter
    /// exists. This is the value the IPC verb returns in that case, and it is
    /// built without touching hardware.
    pub fn disabled() -> InputReport {
        InputReport {
            enabled: false,
            players: 0,
            presenters: Vec::new(),
            pads: Vec::new(),
            refused: Vec::new(),
            drops: BTreeMap::new(),
            last_poll_unix_ms: None,
            polls_completed: 0,
        }
    }
}

/// Milliseconds since the Unix epoch, saturating rather than panicking on a
/// clock set before 1970.
fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The running input layer.
pub struct InputSession<B: InputBackend> {
    backend: B,
    profile: PadProfile,
    fleet: Fleet,
    owned: OwnedNodes,
    db: ControllerDb,
    pin: Pin,
    players: u8,
    presenters: Vec<PresenterReport>,
    drops: BTreeMap<DropReason, u64>,
    /// The most recent poll's refusals, so `input-state` explains what the core
    /// is currently declining rather than everything it ever declined.
    refused: Vec<RefusedReport>,
    last_poll_unix_ms: Option<u64>,
    polls_completed: u64,
}

impl<B: InputBackend> InputSession<B> {
    /// Create every presenter, then return a session ready to poll.
    ///
    /// **Presenter creation is here and nowhere else.** It happens once, before
    /// any pad is looked at, and a failure is fatal to the session rather than
    /// degraded: a core that came up with three of four presenters would hand
    /// player four's input nowhere, and the `players` count it reports would be
    /// a lie.
    pub fn start(mut backend: B, config: &ResolvedInput) -> Result<InputSession<B>, InputError> {
        let profile = PadProfile::canonical();
        let mut owned = OwnedNodes::new();
        let mut presenters = Vec::new();

        for slot in 0..config.players {
            let devnodes = backend.create_presenter(slot, &profile)?;
            if devnodes.is_empty() {
                return Err(InputError::Presenter {
                    slot,
                    detail: "the backend reported no devnode, so discovery could not \
                             recognise this presenter as ours and would grab it"
                        .into(),
                });
            }
            for node in &devnodes {
                owned.register(node.clone());
            }
            presenters.push(PresenterReport {
                slot,
                name: PadProfile::device_name(slot),
                devnodes: devnodes.iter().map(|p| p.display().to_string()).collect(),
            });
        }

        tracing::info!(
            players = config.players,
            "input presenters created; they persist for the life of this session"
        );

        Ok(InputSession {
            backend,
            profile,
            fleet: Fleet::new(config.players),
            owned,
            db: config.db.clone(),
            pin: config.pin,
            players: config.players,
            presenters,
            drops: BTreeMap::new(),
            refused: Vec::new(),
            last_poll_unix_ms: None,
            polls_completed: 0,
        })
    }

    /// One discovery pass: claim what is new, retire what is gone.
    ///
    /// Returns the pads that joined. An enumeration failure is reported and changes nothing —
    /// notably it does **not** retire the whole fleet, because "we could not
    /// read the device list" is not evidence that every pad was unplugged.
    pub fn poll(&mut self) -> Vec<Joined> {
        let candidates = match self.backend.enumerate() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("input discovery: {e}");
                return Vec::new();
            }
        };
        let plan = self
            .fleet
            .plan(&candidates, &self.db, &self.owned, self.pin);

        self.refused = plan
            .refuse
            .iter()
            .map(|(c, reason)| RefusedReport {
                path: c.path.display().to_string(),
                name: c.name.clone(),
                vendor: format!("{:04x}", c.vendor),
                product: format!("{:04x}", c.product),
                guid: c.guid(),
                reason: *reason,
                explanation: reason.explain().to_string(),
            })
            .collect();

        for path in plan.leave {
            self.retire(&path);
        }

        let mut joined = Vec::new();
        for candidate in plan.claim {
            let source_axes = match self.backend.claim(&candidate.path) {
                Ok(a) => a,
                Err(e) => {
                    // A pad we cannot open is a pad we do not hold. Left out of
                    // the fleet, it is re-tried on the next poll — which is the
                    // right behaviour for a device still settling after a plug.
                    tracing::warn!("{e}");
                    continue;
                }
            };
            match self.fleet.admit(&candidate, source_axes) {
                Ok(slot) => {
                    tracing::info!(
                        slot,
                        pad = %candidate.name,
                        path = %candidate.path.display(),
                        "pad claimed"
                    );
                    joined.push(Joined {
                        path: candidate.path.clone(),
                        slot,
                    });
                }
                Err(FleetFull) => {
                    // We grabbed it and then found no slot: give it straight
                    // back rather than holding a pad we will never present.
                    // Otherwise the pad is exclusively ours and dead to
                    // everything, which is worse than not claiming it.
                    tracing::warn!(
                        players = self.players,
                        pad = %candidate.name,
                        "no free player slot; releasing the pad rather than holding it unpresented"
                    );
                    self.backend.release(&candidate.path);
                }
            }
        }
        self.last_poll_unix_ms = Some(unix_millis());
        self.polls_completed += 1;
        joined
    }

    /// Forward one physical event onto its pad's presenter.
    ///
    /// The 1:1 passthrough. An event from a pad the fleet does not hold is
    /// ignored — that is a stream draining after a retire, not something to
    /// route at a slot which may already belong to another player.
    pub fn forward(&mut self, path: &Path, event_type: u16, code: u16, value: i32) {
        let Some(pad) = self.fleet.get(path) else {
            return;
        };
        let slot = pad.slot;
        let source_axis = if event_type == ev::ABS {
            pad.source_axes.get(&code).copied()
        } else {
            None
        };

        let forward = translate(event_type, code, value, source_axis, &self.profile);
        if let Forward::Drop(reason) = forward {
            *self.drops.entry(reason).or_insert(0) += 1;
            return;
        }
        // Track the button BEFORE emitting: if the emit fails we still know what
        // the pad is holding, and quiesce stays correct.
        if let Forward::Key { code, value } = forward {
            self.fleet.note_key(path, code, value);
        }
        self.emit(slot, forward);
    }

    /// A pad's event stream failed — a USB unplug, usually. Retire it now rather
    /// than waiting for the next poll to notice its absence.
    pub fn on_stream_error(&mut self, path: &Path) {
        self.retire(path);
    }

    /// Drop a pad: return its presenter to rest, then release the grab.
    ///
    /// **Order matters.** Releasing first widens the window in which the pad is
    /// free while the presenter is still holding a button — exactly the state a
    /// game reads as stuck input.
    fn retire(&mut self, path: &Path) {
        let Some(retired) = self.fleet.retire(path) else {
            return;
        };
        tracing::info!(
            slot = retired.slot,
            pad = %retired.wire_id,
            held = retired.held_keys.len(),
            "pad left; returning its presenter to rest (the presenter itself stays)"
        );
        for forward in quiesce(&retired.held_keys, &self.profile) {
            self.emit(retired.slot, forward);
        }
        self.backend.release(path);
    }

    fn emit(&mut self, slot: u8, forward: Forward) {
        if let Err(e) = self.backend.emit(slot, forward) {
            tracing::warn!("{e}");
        }
    }

    /// The `input-state` payload.
    pub fn report(&self) -> InputReport {
        InputReport {
            enabled: true,
            players: self.players,
            presenters: self.presenters.clone(),
            pads: self
                .fleet
                .pads()
                .map(|p| PadReport {
                    slot: p.slot,
                    wire_id: p.wire_id.clone(),
                    name: p.name.clone(),
                    path: p.path.display().to_string(),
                    vendor: format!("{:04x}", p.vendor),
                    product: format!("{:04x}", p.product),
                    grabbed: true,
                })
                .collect(),
            refused: self.refused.clone(),
            drops: self.drops.clone(),
            last_poll_unix_ms: self.last_poll_unix_ms,
            polls_completed: self.polls_completed,
        }
    }

    /// The backend, for the concrete async runtime that must await its streams.
    ///
    /// A deliberate, narrow leak: multiplexing event streams needs the concrete
    /// backend type, and putting an `async fn` on [`InputBackend`] would drag a
    /// reactor into every rule this module states.
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Release every pad. The presenters go with the process (see
    /// [`super::runtime`] on unclean exits).
    pub fn shutdown(&mut self) {
        let paths: Vec<PathBuf> = self.fleet.pads().map(|p| p.path.clone()).collect();
        for path in paths {
            self.retire(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::discovery::Candidate;
    use crate::input::identity::bundled_db;
    use crate::input::presenter::{abs, btn, AbsRange, SYN_REPORT};
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::rc::Rc;

    /// What the double recorded. Shared so a test can read it while the session
    /// still owns the backend.
    #[derive(Debug, Default)]
    struct Log {
        /// Every `create_presenter(slot)` call, in order. The count is the
        /// jedwards1230/tv-shell#402 assertion.
        created: Vec<u8>,
        claimed: Vec<PathBuf>,
        released: Vec<PathBuf>,
        /// For each release, how many events had been emitted when it happened.
        /// This is what makes the quiesce-BEFORE-release ordering assertable:
        /// two independent lists record that both occurred but never in which
        /// order, and the order is the rule.
        released_at_emit_count: Vec<usize>,
        /// `(slot, forward)` for everything emitted.
        emitted: Vec<(u8, Forward)>,
    }

    /// A recording backend.
    ///
    /// **It fakes no device behaviour.** It does not pretend to grab, to exclude
    /// another reader, or to deliver input; it records which calls this crate
    /// made, in what order. Every assertion below is therefore about our own
    /// sequencing — never about a behaviour the double invented.
    struct Recorder {
        log: Rc<RefCell<Log>>,
        /// What `enumerate` returns next.
        devices: Rc<RefCell<Vec<Candidate>>>,
        /// Paths whose `claim` must fail, to exercise the open-failure path.
        claim_fails: BTreeSet<PathBuf>,
        /// When set, `create_presenter` returns no devnodes.
        presenter_without_devnode: bool,
        /// When set, `enumerate` fails.
        enumerate_fails: bool,
    }

    impl Recorder {
        fn new(log: &Rc<RefCell<Log>>, devices: &Rc<RefCell<Vec<Candidate>>>) -> Recorder {
            Recorder {
                log: Rc::clone(log),
                devices: Rc::clone(devices),
                claim_fails: BTreeSet::new(),
                presenter_without_devnode: false,
                enumerate_fails: false,
            }
        }
    }

    impl InputBackend for Recorder {
        fn enumerate(&mut self) -> Result<Vec<Candidate>, InputError> {
            if self.enumerate_fails {
                return Err(InputError::Enumerate("permission denied".into()));
            }
            Ok(self.devices.borrow().clone())
        }

        fn create_presenter(
            &mut self,
            slot: u8,
            _profile: &PadProfile,
        ) -> Result<Vec<PathBuf>, InputError> {
            self.log.borrow_mut().created.push(slot);
            if self.presenter_without_devnode {
                return Ok(Vec::new());
            }
            Ok(vec![PathBuf::from(format!("/dev/input/event2{slot}"))])
        }

        fn claim(&mut self, path: &Path) -> Result<BTreeMap<u16, AbsRange>, InputError> {
            if self.claim_fails.contains(path) {
                return Err(InputError::Claim {
                    path: path.to_path_buf(),
                    detail: "device busy".into(),
                });
            }
            self.log.borrow_mut().claimed.push(path.to_path_buf());
            Ok(BTreeMap::from([
                (abs::X, AbsRange::new(-32768, 32767, 16, 128)),
                (abs::Z, AbsRange::new(0, 255, 0, 0)),
            ]))
        }

        fn release(&mut self, path: &Path) {
            let mut log = self.log.borrow_mut();
            let emitted = log.emitted.len();
            log.released.push(path.to_path_buf());
            log.released_at_emit_count.push(emitted);
        }

        fn emit(&mut self, slot: u8, forward: Forward) -> Result<(), InputError> {
            self.log.borrow_mut().emitted.push((slot, forward));
            Ok(())
        }
    }

    struct Harness {
        session: InputSession<Recorder>,
        log: Rc<RefCell<Log>>,
        devices: Rc<RefCell<Vec<Candidate>>>,
    }

    fn config(players: u8) -> ResolvedInput {
        ResolvedInput {
            players,
            db: bundled_db(),
            pin: None,
            poll_interval: std::time::Duration::from_secs(2),
        }
    }

    fn harness(players: u8) -> Harness {
        let log = Rc::new(RefCell::new(Log::default()));
        let devices = Rc::new(RefCell::new(Vec::new()));
        let session = InputSession::start(Recorder::new(&log, &devices), &config(players)).unwrap();
        Harness {
            session,
            log,
            devices,
        }
    }

    fn pad(path: &str, phys: &str) -> Candidate {
        Candidate {
            path: PathBuf::from(path),
            name: "Microsoft X-Box 360 pad".into(),
            vendor: 0x045e,
            product: 0x028e,
            version: 0x0110,
            bus: 3,
            uniq: None,
            phys: Some(phys.into()),
            has_btn_south: true,
        }
    }

    #[test]
    fn start_creates_one_presenter_per_player_and_registers_their_devnodes() {
        let h = harness(4);
        assert_eq!(h.log.borrow().created, vec![0, 1, 2, 3]);
        let report = h.session.report();
        assert!(report.enabled);
        assert_eq!(report.players, 4);
        assert_eq!(report.presenters.len(), 4);
        assert_eq!(report.presenters[2].name, "tv-shell-player-2");
        assert_eq!(report.presenters[2].devnodes, vec!["/dev/input/event22"]);
    }

    /// **Rule (§7 / jedwards1230/tv-shell#402): a physical unplug and replug does
    /// NOT destroy or recreate a presenter.**
    ///
    /// The acceptance test for this whole PR, at the level where the decision is
    /// made. A create or destroy on the pad's lifecycle is a hotplug event every
    /// game and Moonlight forwards to the streaming host. The presenter is
    /// created exactly `players` times, all before any pad is looked at, and a
    /// pad cycling in and out does not add one.
    #[test]
    fn a_pad_unplug_and_replug_never_touches_a_presenter() {
        let mut h = harness(2);
        let creations_after_start = h.log.borrow().created.len();
        assert_eq!(creations_after_start, 2);

        let p = pad("/dev/input/event3", "port-a");
        *h.devices.borrow_mut() = vec![p.clone()];
        assert_eq!(h.session.poll().len(), 1);

        // Unplug: the enumeration no longer lists it.
        h.devices.borrow_mut().clear();
        assert!(h.session.poll().is_empty());
        assert!(h.session.report().pads.is_empty());

        // Replug, on a different devnode as a real replug commonly is.
        *h.devices.borrow_mut() = vec![pad("/dev/input/event7", "port-a")];
        assert_eq!(h.session.poll().len(), 1);

        assert_eq!(
            h.log.borrow().created.len(),
            creations_after_start,
            "a pad cycling must not create a presenter"
        );
        assert_eq!(
            h.session.report().presenters.len(),
            2,
            "and must not destroy one either"
        );
    }

    /// **Rule: a leave returns the presenter to rest BEFORE releasing the pad.**
    ///
    /// The presenter outlives the pad, so a button held at unplug would stay
    /// held forever with nothing able to notice.
    #[test]
    fn a_leave_quiesces_the_presenter_then_releases_the_pad() {
        let mut h = harness(2);
        let p = pad("/dev/input/event3", "port-a");
        *h.devices.borrow_mut() = vec![p.clone()];
        h.session.poll();

        // Press and hold A, then yank the pad.
        h.session.forward(&p.path, ev::KEY, btn::SOUTH, 1);
        h.log.borrow_mut().emitted.clear();
        h.devices.borrow_mut().clear();
        h.session.poll();

        let log = h.log.borrow();
        assert!(
            log.emitted.contains(&(
                0,
                Forward::Key {
                    code: btn::SOUTH,
                    value: 0
                }
            )),
            "the held button must be released on the presenter: {:?}",
            log.emitted
        );
        assert_eq!(
            log.emitted.last(),
            Some(&(0, Forward::Sync)),
            "and the reset must be flushed"
        );
        assert_eq!(log.released, vec![p.path.clone()]);

        // ORDER, not merely occurrence: the release must come after the whole
        // quiesce. Two separate lists record that both happened but never in
        // which sequence, and the sequence is the rule — releasing first widens
        // the window in which the pad is free and the presenter still holds a
        // button.
        assert_eq!(
            log.released_at_emit_count,
            vec![log.emitted.len()],
            "the pad was released before the quiesce finished"
        );
    }

    /// **Rule: an advertised event crosses to the right slot's presenter,
    /// unchanged.**
    #[test]
    fn events_are_forwarded_one_to_one_to_the_pads_own_slot() {
        let mut h = harness(2);
        let p1 = pad("/dev/input/event3", "a");
        let p2 = pad("/dev/input/event4", "b");
        *h.devices.borrow_mut() = vec![p1.clone(), p2.clone()];
        h.session.poll();
        h.log.borrow_mut().emitted.clear();

        h.session.forward(&p2.path, ev::KEY, btn::START, 1);
        h.session.forward(&p2.path, ev::SYN, SYN_REPORT, 0);
        h.session.forward(&p1.path, ev::ABS, abs::X, -32768);

        assert_eq!(
            h.log.borrow().emitted,
            vec![
                (
                    1,
                    Forward::Key {
                        code: btn::START,
                        value: 1
                    }
                ),
                (1, Forward::Sync),
                (
                    0,
                    Forward::Abs {
                        code: abs::X,
                        value: -32768
                    }
                ),
            ]
        );
    }

    /// **Rule: an event from a pad the fleet no longer holds emits nothing.**
    ///
    /// A stream drains after a retire. Routing those late events by the slot
    /// they used to occupy would inject one player's input into another's
    /// presenter the moment the slot is reused.
    #[test]
    fn events_from_a_retired_pad_are_dropped_not_routed_to_its_old_slot() {
        let mut h = harness(2);
        let p = pad("/dev/input/event3", "a");
        *h.devices.borrow_mut() = vec![p.clone()];
        h.session.poll();
        h.devices.borrow_mut().clear();
        h.session.poll();
        h.log.borrow_mut().emitted.clear();

        h.session.forward(&p.path, ev::KEY, btn::SOUTH, 1);
        assert!(h.log.borrow().emitted.is_empty());
    }

    /// **Rule: dropped events are counted, per reason.**
    #[test]
    fn drops_are_counted_and_reported() {
        let mut h = harness(2);
        let p = pad("/dev/input/event3", "a");
        *h.devices.borrow_mut() = vec![p.clone()];
        h.session.poll();

        // BTN_TOUCH twice, and one unadvertised axis.
        h.session.forward(&p.path, ev::KEY, 0x14a, 1);
        h.session.forward(&p.path, ev::KEY, 0x14a, 0);
        h.session.forward(&p.path, ev::ABS, 0x12, 1);

        let drops = h.session.report().drops;
        assert_eq!(drops.get(&DropReason::UnadvertisedKey), Some(&2));
        assert_eq!(drops.get(&DropReason::UnadvertisedAxis), Some(&1));
    }

    /// **Rule: a refused device is reported with its reason.**
    #[test]
    fn refusals_are_reported_with_a_reason_and_an_explanation() {
        let mut h = harness(2);
        *h.devices.borrow_mut() = vec![Candidate {
            vendor: 0,
            product: 0,
            name: "ydotoold virtual device".into(),
            ..pad("/dev/input/event9", "")
        }];
        h.session.poll();

        let report = h.session.report();
        assert!(
            report.pads.is_empty(),
            "an unknown injector must not be claimed"
        );
        assert_eq!(report.refused.len(), 1);
        assert_eq!(report.refused[0].reason, Refusal::NotInTheControllerDb);
        assert!(!report.refused[0].explanation.is_empty());
        assert_eq!(h.log.borrow().claimed.len(), 0, "and never even opened");
    }

    /// **Rule: the session never claims its own presenters.**
    ///
    /// The presenter carries a database-known id on purpose, so only devnode
    /// ownership stops the session grabbing the device it just created and
    /// feeding its own output back into itself. Here the enumeration lists the
    /// presenters exactly as `/dev/input` would.
    #[test]
    fn the_session_never_claims_its_own_presenters() {
        let mut h = harness(2);
        *h.devices.borrow_mut() = vec![
            pad("/dev/input/event20", "virtual-0"),
            pad("/dev/input/event21", "virtual-1"),
        ];
        h.session.poll();

        let report = h.session.report();
        assert!(report.pads.is_empty());
        assert_eq!(h.log.borrow().claimed.len(), 0);
        assert_eq!(report.refused.len(), 2);
        assert!(report
            .refused
            .iter()
            .all(|r| r.reason == Refusal::OurOwnPresenter));
    }

    /// **Rule: a pad we grabbed but cannot seat is given straight back.**
    ///
    /// Holding an exclusive grab on a pad we will never present is strictly
    /// worse than not claiming it: the pad is then dead to the game too.
    #[test]
    fn a_pad_beyond_capacity_is_released_not_held() {
        let mut h = harness(1);
        *h.devices.borrow_mut() =
            vec![pad("/dev/input/event3", "a"), pad("/dev/input/event4", "b")];
        let joined = h.session.poll();

        assert_eq!(joined.len(), 1, "only one slot exists");
        assert_eq!(h.session.report().pads.len(), 1);
        assert_eq!(
            h.log.borrow().released,
            vec![PathBuf::from("/dev/input/event4")],
            "the unseatable pad must be released, not held"
        );
    }

    /// A pad that fails to open is simply not in the fleet, and stays a
    /// candidate — the right behaviour for a device still settling.
    #[test]
    fn a_pad_that_fails_to_open_is_retried_rather_than_seated() {
        let log = Rc::new(RefCell::new(Log::default()));
        let devices = Rc::new(RefCell::new(vec![pad("/dev/input/event3", "a")]));
        let mut backend = Recorder::new(&log, &devices);
        backend.claim_fails = BTreeSet::from([PathBuf::from("/dev/input/event3")]);
        let mut session = InputSession::start(backend, &config(2)).unwrap();

        assert!(session.poll().is_empty());
        assert!(session.report().pads.is_empty());
        // Still a claim candidate next time round.
        assert!(session.poll().is_empty());
        assert!(session.report().pads.is_empty());
    }

    /// **Rule: a presenter with no devnode is a fatal start, not a warning.**
    ///
    /// Without a devnode the session cannot register it as ours, so discovery
    /// would grab it on the very next poll.
    #[test]
    fn a_presenter_whose_devnode_never_appeared_fails_the_start() {
        let log = Rc::new(RefCell::new(Log::default()));
        let devices = Rc::new(RefCell::new(Vec::new()));
        let mut backend = Recorder::new(&log, &devices);
        backend.presenter_without_devnode = true;
        // `unwrap_err` would need `InputSession: Debug`, and so `Debug` on every
        // backend. Match instead.
        let err = match InputSession::start(backend, &config(2)) {
            Ok(_) => panic!("a presenter with no devnode must not yield a session"),
            Err(e) => e,
        };
        assert!(
            matches!(err, InputError::Presenter { slot: 0, .. }),
            "{err}"
        );
    }

    /// A stream error retires the pad immediately, without waiting for a poll.
    #[test]
    fn a_stream_error_retires_the_pad() {
        let mut h = harness(2);
        let p = pad("/dev/input/event3", "a");
        *h.devices.borrow_mut() = vec![p.clone()];
        h.session.poll();

        h.session.on_stream_error(&p.path);
        assert!(h.session.report().pads.is_empty());
        assert_eq!(h.log.borrow().released, vec![p.path.clone()]);
    }

    /// **Rule: a failed enumeration changes nothing.**
    ///
    /// "We could not read the device list" is not evidence that every pad was
    /// unplugged. Treating it as one would quiesce and release a fleet that is
    /// still physically present — mid-game.
    #[test]
    fn a_failed_enumeration_does_not_retire_the_fleet() {
        let log = Rc::new(RefCell::new(Log::default()));
        let devices = Rc::new(RefCell::new(vec![pad("/dev/input/event3", "a")]));
        let mut session = InputSession::start(Recorder::new(&log, &devices), &config(2)).unwrap();
        assert_eq!(session.poll().len(), 1);

        session.backend.enumerate_fails = true;
        session.poll();

        assert_eq!(session.report().pads.len(), 1, "the fleet must survive");
        assert!(log.borrow().released.is_empty(), "nothing may be released");
    }

    /// Shutting down releases every pad and leaves its presenter at rest.
    #[test]
    fn shutdown_releases_every_pad() {
        let mut h = harness(2);
        let p1 = pad("/dev/input/event3", "a");
        let p2 = pad("/dev/input/event4", "b");
        *h.devices.borrow_mut() = vec![p1.clone(), p2.clone()];
        h.session.poll();

        h.session.shutdown();
        assert!(h.session.report().pads.is_empty());
        let released = h.log.borrow().released.clone();
        assert!(released.contains(&p1.path) && released.contains(&p2.path));
    }

    #[test]
    fn the_disabled_report_is_structurally_empty() {
        let r = InputReport::disabled();
        assert!(!r.enabled);
        assert_eq!(r.players, 0);
        assert!(r.presenters.is_empty() && r.pads.is_empty() && r.refused.is_empty());
        assert!(r.drops.is_empty());
        assert_eq!(r.last_poll_unix_ms, None);
    }

    /// **Rule: every completed poll stamps the report.**
    ///
    /// A snapshot-based `input-state` cannot hang, but it also cannot show that
    /// the loop behind it has stopped. The timestamp is what makes a dead loop
    /// visible instead of merely stale-looking.
    #[test]
    fn a_completed_poll_stamps_the_report() {
        let mut h = harness(2);
        let before = h.session.report();
        assert_eq!(before.last_poll_unix_ms, None, "before any poll");
        assert_eq!(before.polls_completed, 0);

        h.session.poll();
        let after = h.session.report();
        assert!(after.last_poll_unix_ms.is_some_and(|ms| ms > 0));
        assert_eq!(after.polls_completed, 1);

        // A poll whose enumeration FAILED must not count: the whole point is to
        // distinguish "we ran" from "we did not".
        //
        // Asserted on the COUNTER, not the timestamp. Two polls in the same
        // millisecond carry the same stamp, so a timestamp assertion here holds
        // whether or not the failing poll stamped — which is exactly how the
        // first version of this test passed against a `poll` mutated to stamp
        // unconditionally at its top.
        h.session.backend.enumerate_fails = true;
        h.session.poll();
        assert_eq!(
            h.session.report().polls_completed,
            1,
            "a failed enumeration is not a completed poll"
        );

        h.session.backend.enumerate_fails = false;
        h.session.poll();
        assert_eq!(h.session.report().polls_completed, 2);
    }
}
