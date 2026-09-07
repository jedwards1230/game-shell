//! The per-player uinput presenter: its capability profile, and the pure
//! translation of a physical pad's events onto it.
//!
//! # Why the profile is FIXED, and what that costs
//!
//! V2_DESIGN §7 requires the presenters to be **permanent**: "one clean virtual
//! pad per player, always present … never appear and vanish (that is a hotplug
//! to every game)". A presenter that is created when a pad joins and destroyed
//! when it leaves is exactly the create/destroy churn every game and Moonlight
//! forward to the streaming host as a controller reconnect
//! (jedwards1230/tv-shell#402).
//!
//! v1 built its virtual pad *from the source device* — copying its `input_id`,
//! its key set and its `absinfo` (`build_virtual_pad` in
//! `daemon/src/input/fleet.rs`). That is only possible once a physical pad is in
//! hand, so permanence and source-derived capabilities are mutually exclusive.
//! §7 picks permanence, so the profile below is a fixed, canonical one, decided
//! before any pad is seen:
//!
//! * **Identity**: the Xbox 360 wired pad (`045e:028e`), the layout SDL, Steam,
//!   Proton and Moonlight all map without configuration.
//! * **Consequence, stated plainly**: a physical pad with buttons outside this
//!   set has those events **dropped**, and one with different axis ranges has
//!   them **rescaled** ([`rescale`]). Dropped codes are counted rather than
//!   silently discarded, and the count is reported by `input-state`, so a pad
//!   losing a button is visible rather than mysterious.
//! * The alternative — derive the profile from the first pad to occupy a slot
//!   and keep it for the session — was rejected: the profile would then depend
//!   on boot ordering, and a different pad plugged into that slot later would
//!   mismatch it anyway, with no signal that it had.
//!
//! # Everything here is pure
//!
//! No uinput, no evdev, no syscall: [`PadProfile`] describes the device the
//! backend builds, and [`translate`] / [`rescale`] / [`quiesce`] decide what
//! crosses onto it. That is what makes the passthrough rules testable in CI on a
//! host with no `/dev/uinput`.

use std::collections::BTreeSet;

use serde::Serialize;

/// Kernel event types (`input-event-codes.h`). Named here rather than pulled
/// from `evdev` so this module compiles and tests anywhere.
pub mod ev {
    pub const SYN: u16 = 0x00;
    pub const KEY: u16 = 0x01;
    pub const ABS: u16 = 0x03;
}

/// `SYN_REPORT` — the end of one coherent packet of events.
pub const SYN_REPORT: u16 = 0;
/// `SYN_DROPPED` — the kernel's buffer overran and the client's view of device
/// state is now unreliable.
pub const SYN_DROPPED: u16 = 3;

/// Button codes, in the order the profile advertises them.
pub mod btn {
    pub const SOUTH: u16 = 0x130;
    pub const EAST: u16 = 0x131;
    pub const NORTH: u16 = 0x133;
    pub const WEST: u16 = 0x134;
    pub const TL: u16 = 0x136;
    pub const TR: u16 = 0x137;
    pub const SELECT: u16 = 0x13a;
    pub const START: u16 = 0x13b;
    pub const MODE: u16 = 0x13c;
    pub const THUMBL: u16 = 0x13d;
    pub const THUMBR: u16 = 0x13e;
}

/// Absolute axis codes.
pub mod abs {
    pub const X: u16 = 0x00;
    pub const Y: u16 = 0x01;
    pub const Z: u16 = 0x02;
    pub const RX: u16 = 0x03;
    pub const RY: u16 = 0x04;
    pub const RZ: u16 = 0x05;
    pub const HAT0X: u16 = 0x10;
    pub const HAT0Y: u16 = 0x11;
}

/// One axis's value range, as evdev's `absinfo` reports it.
///
/// `fuzz` and `flat` are carried because the presenter must declare them (a
/// consumer reads them to size its own deadzone), but they take no part in
/// [`rescale`] — filtering is the consumer's job, and a core that pre-filtered
/// would be imposing a deadzone every game already has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AbsRange {
    pub min: i32,
    pub max: i32,
    pub fuzz: i32,
    pub flat: i32,
}

impl AbsRange {
    pub const fn new(min: i32, max: i32, fuzz: i32, flat: i32) -> AbsRange {
        AbsRange {
            min,
            max,
            fuzz,
            flat,
        }
    }

    /// The resting value for this axis.
    ///
    /// Zero where the range spans it (sticks, hats) and the minimum where it
    /// does not (triggers, which rest released at `0..255`'s floor). Expressed
    /// as a clamp rather than a per-axis table so it stays correct for any range
    /// a pad reports.
    pub fn neutral(&self) -> i32 {
        0.clamp(self.min, self.max)
    }

    /// True when the range carries no information (a device reporting
    /// `min == max`), which [`rescale`] must not divide by.
    pub fn is_degenerate(&self) -> bool {
        self.min >= self.max
    }
}

/// The capability profile of a presenter: what the virtual pad advertises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PadProfile {
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
    pub bus: u16,
    keys: Vec<u16>,
    axes: Vec<(u16, AbsRange)>,
}

/// The stick range an Xbox 360 pad reports.
const STICK: AbsRange = AbsRange::new(-32768, 32767, 16, 128);
/// The analog-trigger range an Xbox 360 pad reports.
const TRIGGER: AbsRange = AbsRange::new(0, 255, 0, 0);
/// The d-pad hat range.
const HAT: AbsRange = AbsRange::new(-1, 1, 0, 0);

impl PadProfile {
    /// The canonical profile every presenter is built to (see the module docs).
    pub fn canonical() -> PadProfile {
        PadProfile {
            // The Xbox 360 wired pad. Deliberately a model every consumer maps
            // without configuration — and deliberately one the controller
            // database knows, which is why discovery must reject our presenters
            // by devnode ownership and cannot rely on the database gate.
            vendor: 0x045e,
            product: 0x028e,
            version: 0x0110,
            // BUS_USB.
            bus: 0x03,
            keys: vec![
                btn::SOUTH,
                btn::EAST,
                btn::NORTH,
                btn::WEST,
                btn::TL,
                btn::TR,
                btn::SELECT,
                btn::START,
                btn::MODE,
                btn::THUMBL,
                btn::THUMBR,
            ],
            axes: vec![
                (abs::X, STICK),
                (abs::Y, STICK),
                (abs::Z, TRIGGER),
                (abs::RX, STICK),
                (abs::RY, STICK),
                (abs::RZ, TRIGGER),
                (abs::HAT0X, HAT),
                (abs::HAT0Y, HAT),
            ],
        }
    }

    /// The device name for a presenter in `slot`.
    ///
    /// The slot is in the name so an operator reading `/proc/bus/input/devices`
    /// can tell P1's presenter from P2's. It is NOT how the core identifies its
    /// own devices — that is the devnode, registered in
    /// [`super::discovery::OwnedNodes`] — because a name match is exactly the
    /// brittle `is_synthetic` check v1 replaced.
    pub fn device_name(slot: u8) -> String {
        format!("tv-shell-player-{slot}")
    }

    pub fn keys(&self) -> &[u16] {
        &self.keys
    }

    pub fn axes(&self) -> &[(u16, AbsRange)] {
        &self.axes
    }

    pub fn advertises_key(&self, code: u16) -> bool {
        self.keys.contains(&code)
    }

    pub fn axis(&self, code: u16) -> Option<AbsRange> {
        self.axes.iter().find(|(c, _)| *c == code).map(|(_, r)| *r)
    }
}

/// Linearly map `value` from one axis range onto another.
///
/// Rules, each defended by a test below:
///
/// * **Identical ranges are the identity.** A pad that already reports the
///   canonical ranges is passed through byte-for-byte, with no arithmetic to be
///   wrong. This is the common case on the target hardware.
/// * **Out-of-range input is clamped**, not wrapped or extrapolated. A pad that
///   overshoots its own declared range must not push the presenter past the
///   range it declared, which a consumer trusts.
/// * **A degenerate source range yields the target's neutral**, because a device
///   reporting `min == max` carries no position to map.
/// * The division rounds to nearest rather than truncating, so a centred stick
///   maps to centre instead of drifting one unit toward zero.
pub fn rescale(value: i32, from: AbsRange, to: AbsRange) -> i32 {
    if from.min == to.min && from.max == to.max {
        return value.clamp(to.min, to.max);
    }
    if from.is_degenerate() {
        return to.neutral();
    }
    let value = value.clamp(from.min, from.max);
    // i64 throughout: a full-scale i32 span times another overflows i32.
    let num = (value as i64 - from.min as i64) * (to.max as i64 - to.min as i64);
    let den = from.max as i64 - from.min as i64;
    // `num` and `den` are both non-negative here (value was clamped into
    // `from`, and `to.max > to.min` unless `to` is degenerate too), so
    // half-up rounding is just `+ den/2`.
    let scaled = to.min as i64 + (num + den / 2) / den;
    scaled.clamp(to.min as i64, to.max as i64) as i32
}

/// One event's fate on the way to a presenter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forward {
    /// Emit this button event unchanged.
    Key { code: u16, value: i32 },
    /// Emit this axis event, rescaled onto the presenter's range.
    Abs { code: u16, value: i32 },
    /// End of packet: flush.
    Sync,
    /// Do not emit. The reason is counted and reported.
    Drop(DropReason),
}

/// Why an event did not reach the presenter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DropReason {
    /// An event type the presenter does not carry (force feedback, LEDs,
    /// misc scancodes). Not a loss: these are outputs or metadata, not input.
    UnsupportedType,
    /// A button the canonical profile does not advertise — a pad-specific extra
    /// (a touchpad click, a paddle, a capture button).
    UnadvertisedKey,
    /// An axis the canonical profile does not advertise (a gyro, an
    /// accelerometer, a second hat).
    UnadvertisedAxis,
    /// The kernel's event buffer overran. State is unreliable until the next
    /// full packet; forwarding the partial packet would leave the presenter
    /// holding whatever was in flight.
    SyncDropped,
}

/// Decide what one physical event becomes on the presenter.
///
/// `source_axis` is the *source pad's* range for an `EV_ABS` code, which the
/// backend read from its `absinfo` at claim time. It is passed in rather than
/// looked up because only the backend has ever seen the physical device — which
/// is what keeps this function pure.
///
/// A `SYN_DROPPED` is **not** forwarded as a sync: the kernel is telling us the
/// packet in flight is incomplete, and flushing it would commit a half-applied
/// state to the presenter.
pub fn translate(
    event_type: u16,
    code: u16,
    value: i32,
    source_axis: Option<AbsRange>,
    profile: &PadProfile,
) -> Forward {
    match event_type {
        ev::KEY => {
            if profile.advertises_key(code) {
                Forward::Key { code, value }
            } else {
                Forward::Drop(DropReason::UnadvertisedKey)
            }
        }
        ev::ABS => match profile.axis(code) {
            Some(target) => {
                // With no source range known, the value is already in the
                // presenter's terms as far as anything here can tell; clamp it
                // rather than invent a mapping.
                let from = source_axis.unwrap_or(target);
                Forward::Abs {
                    code,
                    value: rescale(value, from, target),
                }
            }
            None => Forward::Drop(DropReason::UnadvertisedAxis),
        },
        ev::SYN => match code {
            SYN_REPORT => Forward::Sync,
            SYN_DROPPED => Forward::Drop(DropReason::SyncDropped),
            _ => Forward::Drop(DropReason::UnsupportedType),
        },
        _ => Forward::Drop(DropReason::UnsupportedType),
    }
}

/// The events that return a presenter to rest.
///
/// **This is what permanence costs and why it is cheap.** A presenter outlives
/// the pad that was driving it (§7), so when that pad leaves mid-input the
/// presenter is left holding whatever the pad last sent — a button down forever,
/// a stick deflected — and a game reads that as a player leaning on the stick.
/// Nothing else in the system will correct it: there is no disconnect for a
/// consumer to observe, precisely because the device did not go away.
///
/// So a leave emits an explicit release for every key the pad held and a return
/// to neutral for every axis, followed by one sync. Keys are released in code
/// order so the sequence is deterministic and testable.
pub fn quiesce(held_keys: &BTreeSet<u16>, profile: &PadProfile) -> Vec<Forward> {
    let mut out: Vec<Forward> = held_keys
        .iter()
        .filter(|c| profile.advertises_key(**c))
        .map(|&code| Forward::Key { code, value: 0 })
        .collect();
    for &(code, range) in profile.axes() {
        out.push(Forward::Abs {
            code,
            value: range.neutral(),
        });
    }
    out.push(Forward::Sync);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> PadProfile {
        PadProfile::canonical()
    }

    #[test]
    fn the_canonical_profile_is_an_xbox_360_pad() {
        let p = profile();
        assert_eq!((p.vendor, p.product), (0x045e, 0x028e));
        assert_eq!(p.keys().len(), 11);
        assert_eq!(p.axes().len(), 8);
        assert_eq!(p.axis(abs::X), Some(STICK));
        assert_eq!(p.axis(abs::Z), Some(TRIGGER));
        assert_eq!(p.axis(abs::HAT0X), Some(HAT));
    }

    #[test]
    fn presenter_names_carry_the_slot() {
        assert_eq!(PadProfile::device_name(0), "tv-shell-player-0");
        assert_eq!(PadProfile::device_name(3), "tv-shell-player-3");
    }

    /// **Rule: neutral is zero where the range spans it, the minimum where it
    /// does not.**
    ///
    /// A trigger resting at its range's centre is a half-pulled trigger.
    #[test]
    fn neutral_is_rest_for_every_axis_shape() {
        assert_eq!(STICK.neutral(), 0);
        assert_eq!(HAT.neutral(), 0);
        assert_eq!(TRIGGER.neutral(), 0, "a trigger rests released, at its min");
        // A range entirely above zero rests at its floor.
        assert_eq!(AbsRange::new(10, 20, 0, 0).neutral(), 10);
        // A range entirely below zero rests at its ceiling.
        assert_eq!(AbsRange::new(-20, -10, 0, 0).neutral(), -10);
    }

    /// **Rule: identical ranges are the identity.**
    ///
    /// The common case on this hardware. Any arithmetic here is a chance to be
    /// wrong for no gain.
    #[test]
    fn rescale_between_identical_ranges_changes_nothing() {
        for v in [-32768, -1, 0, 1, 32767, 12345] {
            assert_eq!(rescale(v, STICK, STICK), v);
        }
        for v in [0, 1, 127, 128, 255] {
            assert_eq!(rescale(v, TRIGGER, TRIGGER), v);
        }
    }

    /// **Rule: the endpoints map to the endpoints and centre maps to centre.**
    #[test]
    fn rescale_maps_endpoints_and_centre() {
        // A DS4-style 0..255 stick onto the canonical -32768..32767.
        let ds4 = AbsRange::new(0, 255, 0, 0);
        assert_eq!(rescale(0, ds4, STICK), STICK.min);
        assert_eq!(rescale(255, ds4, STICK), STICK.max);
        let centre = rescale(128, ds4, STICK);
        assert!(
            centre.abs() < 300,
            "a centred stick must map near centre, got {centre}"
        );
    }

    /// **Rule: rounding is to nearest, not truncating toward zero.**
    ///
    /// Truncation biases every negative value one unit toward zero and every
    /// positive one likewise, which shows up as a stick that will not quite
    /// reach its corners.
    #[test]
    fn rescale_rounds_to_nearest() {
        // 0..2 onto 0..10: the midpoint 1 is exactly 5, and 0..3 onto 0..10
        // puts 1 at 3.33 (→3) and 2 at 6.67 (→7), which truncation would make
        // 3 and 6.
        let from = AbsRange::new(0, 3, 0, 0);
        let to = AbsRange::new(0, 10, 0, 0);
        assert_eq!(rescale(1, from, to), 3);
        assert_eq!(rescale(2, from, to), 7, "truncation would give 6");
    }

    /// **Rule: out-of-range input is clamped, never extrapolated.**
    ///
    /// A presenter that reports past the range it declared breaks the consumer
    /// that trusted the declaration.
    #[test]
    fn rescale_clamps_out_of_range_input() {
        let from = AbsRange::new(0, 255, 0, 0);
        assert_eq!(rescale(-50, from, STICK), STICK.min);
        assert_eq!(rescale(9999, from, STICK), STICK.max);
        // And within an identical-range pass-through too.
        assert_eq!(rescale(99999, STICK, STICK), STICK.max);
    }

    /// **Rule: the input clamp is load-bearing, not belt-and-braces.**
    ///
    /// The final clamp on the *result* already keeps the output in range, so
    /// every ordinary out-of-range case looks the same with or without the
    /// input clamp — which is why removing it survived a first mutation pass.
    /// What it actually prevents is the intermediate multiply overflowing:
    /// a value at one end of `i32` against a narrow source range at the other,
    /// scaled onto a wide target, is `~4.3e9 * ~4.3e9` — about 1.8e19, past
    /// `i64::MAX`. Clamping the input first makes that term zero.
    #[test]
    fn rescale_does_not_overflow_on_a_far_out_of_range_value() {
        // A one-unit-wide source range at the very top of i32.
        let narrow_high = AbsRange::new(i32::MAX - 1, i32::MAX, 0, 0);
        let wide = AbsRange::new(i32::MIN, i32::MAX, 0, 0);
        // Unclamped this is (i32::MIN - (i32::MAX - 1)) * (i32::MAX - i32::MIN),
        // which overflows i64 and panics in a debug build.
        assert_eq!(rescale(i32::MIN, narrow_high, wide), wide.min);

        // The mirror case, at the bottom of i32.
        let narrow_low = AbsRange::new(i32::MIN, i32::MIN + 1, 0, 0);
        assert_eq!(rescale(i32::MAX, narrow_low, wide), wide.max);
    }

    /// **Rule: a degenerate source range yields neutral, not a division by zero.**
    #[test]
    fn a_degenerate_source_range_is_neutral() {
        let flat = AbsRange::new(7, 7, 0, 0);
        assert_eq!(rescale(7, flat, STICK), 0);
        assert_eq!(rescale(7, flat, TRIGGER), TRIGGER.min);
    }

    /// Full-scale i32 ranges must not overflow the intermediate multiply.
    #[test]
    fn rescale_does_not_overflow_on_full_scale_ranges() {
        let wide = AbsRange::new(i32::MIN, i32::MAX, 0, 0);
        assert_eq!(rescale(i32::MIN, wide, STICK), STICK.min);
        assert_eq!(rescale(i32::MAX, wide, STICK), STICK.max);

        // Widening does not overflow either. Note the result is 32768 and not
        // 0: an evdev stick range is ASYMMETRIC (-32768..32767), so its true
        // midpoint is -0.5 and a reported 0 already sits a half-step above
        // centre. Scaling to a 2^32-wide range multiplies that half-step by
        // ~65536. This is arithmetic, not drift — and it is exactly why
        // `rescale` short-circuits identical ranges instead of round-tripping
        // through the same formula.
        let widened = rescale(0, STICK, wide);
        assert!(
            (widened as i64 - 32_768).abs() <= 1,
            "expected the half-step above centre, got {widened}"
        );
    }

    /// **Rule: an advertised button crosses unchanged.**
    #[test]
    fn an_advertised_button_is_forwarded_verbatim() {
        let p = profile();
        assert_eq!(
            translate(ev::KEY, btn::SOUTH, 1, None, &p),
            Forward::Key {
                code: btn::SOUTH,
                value: 1
            }
        );
        assert_eq!(
            translate(ev::KEY, btn::MODE, 0, None, &p),
            Forward::Key {
                code: btn::MODE,
                value: 0
            }
        );
    }

    /// **Rule: a button outside the profile is dropped, and says so.**
    ///
    /// A uinput device silently discards events for codes it never declared, so
    /// the choice is between a drop we count and a drop we do not know about.
    #[test]
    fn an_unadvertised_button_is_dropped_with_a_reason() {
        // BTN_TOUCH — a DualShock touchpad click. Real, and outside the profile.
        assert_eq!(
            translate(ev::KEY, 0x14a, 1, None, &profile()),
            Forward::Drop(DropReason::UnadvertisedKey)
        );
    }

    #[test]
    fn an_advertised_axis_is_rescaled_onto_the_profile() {
        let ds4_stick = AbsRange::new(0, 255, 0, 0);
        assert_eq!(
            translate(ev::ABS, abs::X, 255, Some(ds4_stick), &profile()),
            Forward::Abs {
                code: abs::X,
                value: STICK.max
            }
        );
    }

    #[test]
    fn an_unadvertised_axis_is_dropped() {
        // ABS_HAT1X — a second hat some pads expose.
        assert_eq!(
            translate(ev::ABS, 0x12, 1, None, &profile()),
            Forward::Drop(DropReason::UnadvertisedAxis)
        );
    }

    /// **Rule: `SYN_REPORT` flushes; `SYN_DROPPED` does NOT.**
    ///
    /// `SYN_DROPPED` says the kernel's buffer overran and the packet in flight
    /// is incomplete. Treating it as a flush commits a half-applied state — a
    /// button pressed with its release lost, which is the stuck-input shape.
    #[test]
    fn syn_report_flushes_and_syn_dropped_does_not() {
        let p = profile();
        assert_eq!(translate(ev::SYN, SYN_REPORT, 0, None, &p), Forward::Sync);
        assert_eq!(
            translate(ev::SYN, SYN_DROPPED, 0, None, &p),
            Forward::Drop(DropReason::SyncDropped)
        );
        assert_ne!(translate(ev::SYN, SYN_DROPPED, 0, None, &p), Forward::Sync);
    }

    /// Event types the presenter does not carry — force feedback (0x15), LEDs
    /// (0x11), misc scancodes (0x04), relative axes (0x02) — are dropped.
    #[test]
    fn unsupported_event_types_are_dropped() {
        let p = profile();
        for t in [0x02u16, 0x04, 0x11, 0x15, 0x17] {
            assert_eq!(
                translate(t, 0, 1, None, &p),
                Forward::Drop(DropReason::UnsupportedType),
                "event type {t:#x}"
            );
        }
    }

    /// **Rule: a leaving pad's presenter is returned to rest.**
    ///
    /// The presenter survives the pad (§7), so nothing else will ever correct a
    /// button the pad was holding when it was unplugged. Without this the
    /// player's `A` stays down for the rest of the session and no consumer can
    /// tell, because from its side no device disconnected.
    #[test]
    fn quiesce_releases_every_held_key_and_centres_every_axis() {
        let held = BTreeSet::from([btn::SOUTH, btn::TL, btn::MODE]);
        let out = quiesce(&held, &profile());

        let releases: Vec<_> = out
            .iter()
            .filter_map(|f| match f {
                Forward::Key { code, value } => Some((*code, *value)),
                _ => None,
            })
            .collect();
        assert_eq!(
            releases,
            vec![(btn::SOUTH, 0), (btn::TL, 0), (btn::MODE, 0)],
            "every held key released, in code order"
        );

        // Every axis the profile advertises is driven to rest.
        for &(code, range) in profile().axes() {
            assert!(
                out.contains(&Forward::Abs {
                    code,
                    value: range.neutral()
                }),
                "axis {code:#x} must be returned to neutral"
            );
        }

        assert_eq!(
            out.last(),
            Some(&Forward::Sync),
            "the reset must be flushed, or the presenter never applies it"
        );
    }

    #[test]
    fn quiesce_with_nothing_held_still_centres_and_flushes() {
        let out = quiesce(&BTreeSet::new(), &profile());
        assert!(!out.iter().any(|f| matches!(f, Forward::Key { .. })));
        assert_eq!(out.len(), profile().axes().len() + 1);
        assert_eq!(out.last(), Some(&Forward::Sync));
    }

    /// A key the pad held that the profile never advertised was never forwarded,
    /// so releasing it would be an event the presenter cannot carry.
    #[test]
    fn quiesce_does_not_release_a_key_the_presenter_never_carried() {
        let out = quiesce(&BTreeSet::from([0x14a]), &profile());
        assert!(!out.iter().any(|f| matches!(f, Forward::Key { .. })));
    }
}
