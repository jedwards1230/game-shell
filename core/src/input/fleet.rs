//! The pad fleet: which physical pads the core holds, which slot each occupies,
//! and what a fresh enumeration means for that set.
//!
//! # Polled, not event-driven
//!
//! Membership is recomputed from a full enumeration on every poll, and a pad is
//! considered gone because it is **absent from the enumeration**, not because
//! some listener fired. V2_DESIGN §10 makes that the house rule after v1's
//! residual defect: an event listener that was attached and processed nothing,
//! visible only as a widening gap between events seen and windows seen. A poll
//! that stops has no such quiet failure mode — it either produces a list or it
//! does not.
//!
//! The runtime *also* retires a pad whose event stream errors, because a USB
//! unplug surfaces there first and there is no reason to wait a poll interval to
//! quiesce a presenter. Both paths funnel through [`Fleet::retire`], so the
//! bookkeeping has one implementation regardless of which noticed.
//!
//! # Nothing here creates or destroys a presenter
//!
//! [`Plan`] has no variant that could. That is not an oversight to be filled in
//! later: V2_DESIGN §7 requires presenters to be permanent, so making a
//! presenter lifecycle change *unrepresentable* in the type the join/leave path
//! produces is the strongest available statement of it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use super::discovery::{classify, Candidate, OwnedNodes, Pin, Refusal, Verdict};
use super::identity::{ControllerDb, SlotAllocator};
use super::presenter::AbsRange;

/// A physical pad the core currently holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedPad {
    pub path: PathBuf,
    pub wire_id: String,
    pub name: String,
    pub vendor: u16,
    pub product: u16,
    /// The player slot, and so the index of the presenter this pad drives.
    pub slot: u8,
    /// The source pad's own axis ranges, read from its `absinfo` at claim time.
    /// Passed to [`super::presenter::translate`] so its values land correctly on
    /// the canonical profile.
    pub source_axes: BTreeMap<u16, AbsRange>,
    /// Buttons currently held down, tracked so a leave can release them on the
    /// presenter that outlives this pad.
    pub held_keys: BTreeSet<u16>,
}

/// What a fresh enumeration implies, decided without mutating anything.
///
/// Deliberately a plan rather than an action: opening a device can fail, and a
/// failed open must not consume a player slot. The caller opens, then calls
/// [`Fleet::admit`] for each one that succeeded.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Candidates the gate accepted that the fleet does not already hold.
    pub claim: Vec<Candidate>,
    /// Candidates the gate refused, with the reason, for `input-state`.
    pub refuse: Vec<(Candidate, Refusal)>,
    /// Paths the fleet holds that the enumeration no longer lists.
    pub leave: Vec<PathBuf>,
}

/// A pad that has left, and what its presenter needs to be told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetiredPad {
    pub slot: u8,
    pub wire_id: String,
    /// Buttons the pad was holding when it went away. The presenter is still
    /// there, so these must be released explicitly (see
    /// [`super::presenter::quiesce`]).
    pub held_keys: BTreeSet<u16>,
}

/// The fleet was full: every player slot is occupied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FleetFull;

/// The set of claimed pads, keyed by devnode path.
///
/// **Keyed by path and not by fd.** An already-claimed pad re-enumerates at the
/// same path but a fresh file descriptor every time, so the fd identifies an
/// open, never a device. v1 keyed its map by fd and had to carry a separate
/// path set purely to dedup against it; keying by path directly removes the
/// second structure and the chance of the two disagreeing.
#[derive(Debug)]
pub struct Fleet {
    pads: BTreeMap<PathBuf, ClaimedPad>,
    slots: SlotAllocator,
}

impl Fleet {
    /// A fleet with `players` slots — the presenter count, since a pad with no
    /// presenter has nothing to be re-presented on.
    pub fn new(players: u8) -> Fleet {
        Fleet {
            pads: BTreeMap::new(),
            slots: SlotAllocator::new(players),
        }
    }

    /// Decide what an enumeration means. Pure: mutates nothing.
    ///
    /// Rules:
    ///
    /// * A candidate at a path the fleet **already holds** appears in neither
    ///   `claim` nor `refuse`. It is not a new device; it is the one we have,
    ///   seen again. Re-claiming it would open and grab the same pad a second
    ///   time and burn a second slot.
    /// * Everything else goes through [`classify`] and lands in `claim` or
    ///   `refuse` with its reason.
    /// * A held path absent from the enumeration is a `leave`.
    pub fn plan(
        &self,
        candidates: &[Candidate],
        db: &ControllerDb,
        owned: &OwnedNodes,
        pin: Pin,
    ) -> Plan {
        let mut plan = Plan::default();
        for candidate in candidates {
            if self.pads.contains_key(&candidate.path) {
                continue;
            }
            match classify(candidate, db, owned, pin) {
                Verdict::Claim => plan.claim.push(candidate.clone()),
                Verdict::Refuse(reason) => plan.refuse.push((candidate.clone(), reason)),
            }
        }
        let seen: BTreeSet<&PathBuf> = candidates.iter().map(|c| &c.path).collect();
        plan.leave = self
            .pads
            .keys()
            .filter(|p| !seen.contains(*p))
            .cloned()
            .collect();
        plan
    }

    /// Take a successfully-opened pad into the fleet, assigning it a slot.
    ///
    /// Fails with [`FleetFull`] rather than allocating past the presenter count:
    /// a slot with no presenter behind it would index nothing.
    pub fn admit(
        &mut self,
        candidate: &Candidate,
        source_axes: BTreeMap<u16, AbsRange>,
    ) -> Result<u8, FleetFull> {
        let slot = self.slots.alloc().ok_or(FleetFull)?;
        self.pads.insert(
            candidate.path.clone(),
            ClaimedPad {
                path: candidate.path.clone(),
                wire_id: candidate.wire_id(),
                name: candidate.name.clone(),
                vendor: candidate.vendor,
                product: candidate.product,
                slot,
                source_axes,
                held_keys: BTreeSet::new(),
            },
        );
        Ok(slot)
    }

    /// Drop a pad and free its slot. `None` if it was not held.
    pub fn retire(&mut self, path: &std::path::Path) -> Option<RetiredPad> {
        let pad = self.pads.remove(path)?;
        self.slots.free(pad.slot);
        Some(RetiredPad {
            slot: pad.slot,
            wire_id: pad.wire_id,
            held_keys: pad.held_keys,
        })
    }

    pub fn get(&self, path: &std::path::Path) -> Option<&ClaimedPad> {
        self.pads.get(path)
    }

    /// Record a button transition so a later leave can release what is held.
    ///
    /// Any non-zero value is a press (a value of 2 is autorepeat, which pads do
    /// not emit but the type permits); zero is a release.
    pub fn note_key(&mut self, path: &std::path::Path, code: u16, value: i32) {
        let Some(pad) = self.pads.get_mut(path) else {
            return;
        };
        if value == 0 {
            pad.held_keys.remove(&code);
        } else {
            pad.held_keys.insert(code);
        }
    }

    /// Pads in slot order.
    pub fn pads(&self) -> impl Iterator<Item = &ClaimedPad> {
        let mut v: Vec<&ClaimedPad> = self.pads.values().collect();
        v.sort_by_key(|p| p.slot);
        v.into_iter()
    }

    pub fn len(&self) -> usize {
        self.pads.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pads.is_empty()
    }

    pub fn capacity(&self) -> u8 {
        self.slots.capacity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::identity::bundled_db;
    use crate::input::presenter::{abs, btn};

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

    fn unknown(path: &str) -> Candidate {
        Candidate {
            vendor: 0x0bad,
            product: 0x0bad,
            ..pad(path, "usb-bad")
        }
    }

    fn axes() -> BTreeMap<u16, AbsRange> {
        BTreeMap::from([
            (abs::X, AbsRange::new(-32768, 32767, 16, 128)),
            (abs::Z, AbsRange::new(0, 255, 0, 0)),
        ])
    }

    fn plan_of(fleet: &Fleet, candidates: &[Candidate]) -> Plan {
        fleet.plan(candidates, &bundled_db(), &OwnedNodes::new(), None)
    }

    #[test]
    fn a_fresh_fleet_claims_every_known_pad() {
        let fleet = Fleet::new(4);
        let plan = plan_of(
            &fleet,
            &[pad("/dev/input/event3", "a"), unknown("/dev/input/event4")],
        );
        assert_eq!(plan.claim.len(), 1);
        assert_eq!(plan.claim[0].path, PathBuf::from("/dev/input/event3"));
        assert_eq!(plan.refuse.len(), 1);
        assert_eq!(plan.refuse[0].1, Refusal::NotInTheControllerDb);
        assert!(plan.leave.is_empty());
    }

    /// **Rule: a pad already held is not re-claimed when it re-enumerates.**
    ///
    /// Every poll lists the pads we already hold. Without the dedup each poll
    /// would open and grab the same pad again and consume another slot, so a
    /// four-slot fleet would be full within four seconds of one pad being
    /// plugged in.
    #[test]
    fn an_already_held_pad_is_not_claimed_again() {
        let mut fleet = Fleet::new(4);
        let p = pad("/dev/input/event3", "a");
        fleet.admit(&p, axes()).unwrap();

        let plan = plan_of(&fleet, std::slice::from_ref(&p));
        assert!(
            plan.claim.is_empty(),
            "the pad we already hold is not a new claim"
        );
        assert!(
            plan.refuse.is_empty(),
            "nor is it a refusal — it is simply ours"
        );
        assert!(plan.leave.is_empty());
        assert_eq!(fleet.len(), 1);
    }

    /// **Rule: a held path missing from the enumeration is a leave.**
    #[test]
    fn a_pad_absent_from_the_enumeration_leaves() {
        let mut fleet = Fleet::new(4);
        let a = pad("/dev/input/event3", "a");
        let b = pad("/dev/input/event4", "b");
        fleet.admit(&a, axes()).unwrap();
        fleet.admit(&b, axes()).unwrap();

        let plan = plan_of(&fleet, std::slice::from_ref(&a));
        assert_eq!(plan.leave, vec![PathBuf::from("/dev/input/event4")]);
        assert!(plan.claim.is_empty());
    }

    #[test]
    fn plan_mutates_nothing() {
        let mut fleet = Fleet::new(4);
        let a = pad("/dev/input/event3", "a");
        fleet.admit(&a, axes()).unwrap();
        let before = fleet.len();
        let _ = plan_of(&fleet, &[]);
        let _ = plan_of(&fleet, &[a.clone(), pad("/dev/input/event4", "b")]);
        assert_eq!(
            fleet.len(),
            before,
            "planning is a decision, not a transition"
        );
    }

    /// **Rule: slots are stable across another player's reconnect.**
    ///
    /// The behaviour the slot allocator exists for, asserted at the fleet level
    /// where it is actually observed: P1 keeps slot 0 — and therefore keeps
    /// driving presenter 0 — while P2 unplugs and comes back.
    #[test]
    fn p1_keeps_its_slot_across_a_p2_replug() {
        let mut fleet = Fleet::new(4);
        let p1 = pad("/dev/input/event3", "port-a");
        let p2 = pad("/dev/input/event4", "port-b");
        assert_eq!(fleet.admit(&p1, axes()).unwrap(), 0);
        assert_eq!(fleet.admit(&p2, axes()).unwrap(), 1);

        let retired = fleet.retire(&p2.path).unwrap();
        assert_eq!(retired.slot, 1);
        assert_eq!(fleet.get(&p1.path).unwrap().slot, 0, "P1 must not move");

        // The pad comes back on a different devnode, as a replug commonly does.
        let p2_again = pad("/dev/input/event9", "port-b");
        assert_eq!(fleet.admit(&p2_again, axes()).unwrap(), 1);
        assert_eq!(fleet.get(&p1.path).unwrap().slot, 0);
    }

    /// **Rule: a full fleet refuses rather than allocating a slot with no
    /// presenter behind it.**
    #[test]
    fn admitting_past_capacity_fails() {
        let mut fleet = Fleet::new(2);
        assert_eq!(fleet.admit(&pad("/dev/input/event3", "a"), axes()), Ok(0));
        assert_eq!(fleet.admit(&pad("/dev/input/event4", "b"), axes()), Ok(1));
        assert_eq!(
            fleet.admit(&pad("/dev/input/event5", "c"), axes()),
            Err(FleetFull)
        );
        assert_eq!(fleet.len(), 2);
    }

    #[test]
    fn retiring_a_pad_we_do_not_hold_is_none() {
        let mut fleet = Fleet::new(4);
        assert_eq!(
            fleet.retire(std::path::Path::new("/dev/input/event9")),
            None
        );
    }

    /// **Rule: held buttons are tracked, and a leave carries them out.**
    ///
    /// This is the input to [`super::presenter::quiesce`]. If the fleet forgot
    /// what was held, the presenter — which outlives the pad — would be left
    /// holding it with nothing able to notice.
    #[test]
    fn a_leave_reports_the_buttons_the_pad_was_holding() {
        let mut fleet = Fleet::new(4);
        let p = pad("/dev/input/event3", "a");
        fleet.admit(&p, axes()).unwrap();

        fleet.note_key(&p.path, btn::SOUTH, 1);
        fleet.note_key(&p.path, btn::TL, 1);
        fleet.note_key(&p.path, btn::EAST, 1);
        fleet.note_key(&p.path, btn::EAST, 0); // pressed and released: not held

        let retired = fleet.retire(&p.path).unwrap();
        assert_eq!(retired.held_keys, BTreeSet::from([btn::SOUTH, btn::TL]));
    }

    #[test]
    fn noting_a_key_for_a_pad_we_do_not_hold_is_a_no_op() {
        let mut fleet = Fleet::new(4);
        fleet.note_key(std::path::Path::new("/dev/input/event9"), btn::SOUTH, 1);
        assert!(fleet.is_empty());
    }

    #[test]
    fn pads_are_listed_in_slot_order() {
        let mut fleet = Fleet::new(4);
        // Admit in an order that does not match the path ordering of the map.
        fleet.admit(&pad("/dev/input/event9", "a"), axes()).unwrap();
        fleet.admit(&pad("/dev/input/event3", "b"), axes()).unwrap();
        let slots: Vec<u8> = fleet.pads().map(|p| p.slot).collect();
        assert_eq!(slots, vec![0, 1]);
    }

    #[test]
    fn a_claimed_pad_keeps_its_source_axis_ranges() {
        let mut fleet = Fleet::new(4);
        let p = pad("/dev/input/event3", "a");
        fleet.admit(&p, axes()).unwrap();
        let held = fleet.get(&p.path).unwrap();
        assert_eq!(
            held.source_axes.get(&abs::Z),
            Some(&AbsRange::new(0, 255, 0, 0))
        );
        assert_eq!(held.wire_id, "phys:a");
    }
}
