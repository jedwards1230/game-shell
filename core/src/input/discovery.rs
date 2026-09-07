//! The discovery gate: which enumerated input devices the core is allowed to
//! claim, and why it refused the rest.
//!
//! This is the whole of v1's `find_gamepads` selection logic with the evdev
//! enumeration lifted out, so the decision is a pure function of a description.
//! The backend produces [`Candidate`]s; this module decides. Nothing here opens
//! a device, so every rule below is tested in CI on a host with no seat.
//!
//! # Why refusal is a typed reason and not a `bool`
//!
//! Every device the core declines is a device an operator may be waiting for. A
//! bare `false` produces the support question "why won't it see my pad", and v1
//! answered it with a `debug!` line an operator had to know to enable. The
//! reason is part of the answer, so it is part of the type — and it is reported
//! by `input-state`, which is the read-only verb this all exists to feed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::identity::{derive_wire_id, guid_to_string, sdl_guid, ControllerDb};

/// One enumerated input device, described well enough to judge.
///
/// Built by the backend from an evdev `Device`; constructed directly in tests.
/// It carries only what the gate reads — deliberately not the open device, so
/// that a rejected candidate was never something the core held open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// `/dev/input/eventN`. The stable enumeration key: an already-claimed pad
    /// re-enumerates at the same path but a fresh fd, so the path — not the fd —
    /// is what dedup and ownership compare.
    pub path: PathBuf,
    pub name: String,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
    pub bus: u16,
    /// evdev `uniq`, for the wire id.
    pub uniq: Option<String>,
    /// evdev `phys`, for the wire id.
    pub phys: Option<String>,
    /// Whether the device advertises `BTN_SOUTH` — the cheap pre-filter that
    /// separates a controller-like device from a keyboard, mouse or lid switch.
    pub has_btn_south: bool,
}

impl Candidate {
    /// The SDL GUID string for this device, for logs and `input-state`.
    pub fn guid(&self) -> String {
        guid_to_string(&sdl_guid(self.bus, self.vendor, self.product, self.version))
    }

    /// The stable wire id for this device.
    pub fn wire_id(&self) -> String {
        derive_wire_id(
            self.uniq.as_deref(),
            self.phys.as_deref(),
            self.vendor,
            self.product,
            &self.path.to_string_lossy(),
        )
    }
}

/// Why a candidate was not claimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Refusal {
    /// Not controller-like: no `BTN_SOUTH`.
    NotAGamepad,
    /// One of our own uinput presenters, recognised by devnode.
    OurOwnPresenter,
    /// An operator pin is in force and this is not the pinned model.
    NotThePinnedModel,
    /// Advertises `BTN_SOUTH` but is in no controller database and is not
    /// pinned. This is the gate that structurally rejects foreign software
    /// injectors.
    NotInTheControllerDb,
}

impl Refusal {
    /// A one-line explanation, for `input-state` and logs.
    pub fn explain(self) -> &'static str {
        match self {
            Refusal::NotAGamepad => "does not advertise BTN_SOUTH",
            Refusal::OurOwnPresenter => "this is one of our own uinput presenters",
            Refusal::NotThePinnedModel => {
                "an operator pin is in force and this is not the pinned vendor/product"
            }
            Refusal::NotInTheControllerDb => {
                "not in the controller database and not pinned; add it to \
                 [input].controller_db or set [input].pin_vendor/pin_product"
            }
        }
    }
}

/// The gate's answer for one candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Claim,
    Refuse(Refusal),
}

/// An operator's `(vendor, product)` pin, if configured.
///
/// A pin is an explicit operator decision, so it **bypasses the database** —
/// that is the escape hatch for a controller the bundled baseline has never
/// heard of. It is all-or-nothing on purpose: half a pin (a vendor with no
/// product) would silently claim every device from that vendor, so
/// [`super::config::InputConfig::validate`] rejects it before it can.
pub type Pin = Option<(u16, u16)>;

/// Decide whether the core may claim one device.
///
/// The gate, in order:
///
/// 1. **Not controller-like → refuse.** No `BTN_SOUTH`, not a pad.
/// 2. **Our own presenter → refuse.** Recognised by devnode path, not by name.
///    This gate is load-bearing rather than defensive: a presenter deliberately
///    advertises a *canonical, database-known* `input_id` (see
///    [`super::presenter`]), so without it every presenter would pass step 4 and
///    be grabbed as a bogus pad on the next discovery poll — feeding its own
///    output back into itself.
/// 3. **A pin is in force → match it exactly, or refuse.**
/// 4. **Otherwise require a database match.**
///
/// There is deliberately **no bare-`BTN_SOUTH` fallback**. A software injector
/// such as `ydotoold` advertises `BTN_SOUTH` and is in no controller database,
/// so "claim the first `BTN_SOUTH` device" would grab it and feed synthetic
/// input back into the fleet. Requiring a database match rejects foreign
/// injectors *structurally*, which is what v1's brittle `is_synthetic` name
/// match was replaced with.
pub fn classify(candidate: &Candidate, db: &ControllerDb, owned: &OwnedNodes, pin: Pin) -> Verdict {
    if !candidate.has_btn_south {
        return Verdict::Refuse(Refusal::NotAGamepad);
    }
    if owned.contains(&candidate.path) {
        return Verdict::Refuse(Refusal::OurOwnPresenter);
    }
    if let Some((v, p)) = pin {
        return if candidate.vendor == v && candidate.product == p {
            Verdict::Claim
        } else {
            Verdict::Refuse(Refusal::NotThePinnedModel)
        };
    }
    if db.is_known(candidate.vendor, candidate.product) {
        Verdict::Claim
    } else {
        Verdict::Refuse(Refusal::NotInTheControllerDb)
    }
}

/// The devnodes of every uinput device this core created.
///
/// Ownership is by **devnode path** and not by fd or name. The path is stable
/// across the fresh `evdev::enumerate` reopen that discovery performs — a raw fd
/// is a new number every time — so the skip in [`classify`] actually fires.
#[derive(Debug, Default, Clone)]
pub struct OwnedNodes {
    paths: HashSet<PathBuf>,
}

impl OwnedNodes {
    pub fn new() -> OwnedNodes {
        OwnedNodes::default()
    }

    pub fn register(&mut self, path: PathBuf) {
        self.paths.insert(path);
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.paths.contains(path)
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> ControllerDb {
        super::super::identity::bundled_db()
    }

    /// A candidate for a real, database-known pad (Xbox 360 wired).
    fn known_pad(path: &str) -> Candidate {
        Candidate {
            path: PathBuf::from(path),
            name: "Microsoft X-Box 360 pad".into(),
            vendor: 0x045e,
            product: 0x028e,
            version: 0x0110,
            bus: 3,
            uniq: None,
            phys: Some("usb-0000:00:14.0-1/input0".into()),
            has_btn_south: true,
        }
    }

    /// `ydotoold`'s virtual device: advertises `BTN_SOUTH`, in no database.
    /// This is the exact shape the gate exists to refuse.
    fn injector(path: &str) -> Candidate {
        Candidate {
            path: PathBuf::from(path),
            name: "ydotoold virtual device".into(),
            vendor: 0,
            product: 0,
            version: 0,
            bus: 6,
            uniq: None,
            phys: None,
            has_btn_south: true,
        }
    }

    fn keyboard(path: &str) -> Candidate {
        Candidate {
            has_btn_south: false,
            name: "AT Translated Set 2 keyboard".into(),
            ..known_pad(path)
        }
    }

    #[test]
    fn a_database_known_pad_is_claimed() {
        let v = classify(
            &known_pad("/dev/input/event3"),
            &db(),
            &OwnedNodes::new(),
            None,
        );
        assert_eq!(v, Verdict::Claim);
    }

    /// **Rule: no bare-`BTN_SOUTH` fallback — an unknown injector is refused.**
    ///
    /// If the gate ever degrades to "claim anything with BTN_SOUTH", `ydotoold`
    /// gets grabbed as a pad and the core feeds synthetic input to itself.
    #[test]
    fn a_btn_south_device_in_no_database_is_refused() {
        let v = classify(
            &injector("/dev/input/event9"),
            &db(),
            &OwnedNodes::new(),
            None,
        );
        assert_eq!(v, Verdict::Refuse(Refusal::NotInTheControllerDb));
    }

    #[test]
    fn a_device_without_btn_south_is_not_a_gamepad() {
        let v = classify(
            &keyboard("/dev/input/event0"),
            &db(),
            &OwnedNodes::new(),
            None,
        );
        assert_eq!(v, Verdict::Refuse(Refusal::NotAGamepad));
    }

    /// **Rule: our own presenters are never claimed.**
    ///
    /// A presenter advertises a canonical, *database-known* `input_id`, so it
    /// passes the database gate on its own merits. Only the devnode-ownership
    /// check keeps the core from grabbing the device it just created. Drop that
    /// check and this candidate is claimed — which is the feedback loop.
    #[test]
    fn our_own_presenter_is_refused_even_though_the_db_knows_its_id() {
        let presenter_path = "/dev/input/event20";
        // Same identity as a real Xbox pad: that is the point of a canonical
        // profile, and why the db gate alone cannot save us here.
        let candidate = known_pad(presenter_path);
        assert!(
            db().is_known(candidate.vendor, candidate.product),
            "precondition: the presenter's id is database-known"
        );
        let mut owned = OwnedNodes::new();
        owned.register(PathBuf::from(presenter_path));
        assert_eq!(
            classify(&candidate, &db(), &owned, None),
            Verdict::Refuse(Refusal::OurOwnPresenter)
        );
    }

    /// Ownership is compared by path, so a *different* device is unaffected by
    /// one presenter being owned.
    #[test]
    fn ownership_is_per_path_not_global() {
        let mut owned = OwnedNodes::new();
        owned.register(PathBuf::from("/dev/input/event20"));
        assert_eq!(
            classify(&known_pad("/dev/input/event3"), &db(), &owned, None),
            Verdict::Claim
        );
    }

    /// **Rule: a pin bypasses the database.**
    ///
    /// This is the operator escape hatch for a pad no database knows.
    #[test]
    fn a_pin_claims_an_otherwise_unknown_model() {
        let unknown = Candidate {
            vendor: 0x1234,
            product: 0x5678,
            ..known_pad("/dev/input/event7")
        };
        assert_eq!(
            classify(&unknown, &db(), &OwnedNodes::new(), None),
            Verdict::Refuse(Refusal::NotInTheControllerDb),
        );
        assert_eq!(
            classify(&unknown, &db(), &OwnedNodes::new(), Some((0x1234, 0x5678))),
            Verdict::Claim,
        );
    }

    /// **Rule: a pin also EXCLUDES — it is a whitelist of one, not a hint.**
    ///
    /// An operator who pins a model is saying "this pad and no other". A pin
    /// that only added devices would silently keep claiming everything the
    /// database knows, which is not what the setting says.
    #[test]
    fn a_pin_refuses_a_database_known_pad_that_is_not_the_pinned_one() {
        assert_eq!(
            classify(
                &known_pad("/dev/input/event3"),
                &db(),
                &OwnedNodes::new(),
                Some((0x054c, 0x09cc)),
            ),
            Verdict::Refuse(Refusal::NotThePinnedModel),
        );
    }

    /// **Rule: ownership outranks the pin.**
    ///
    /// A presenter carries a canonical id; if an operator happened to pin that
    /// same model, a pin checked first would claim our own presenter.
    #[test]
    fn ownership_is_checked_before_the_pin() {
        let path = "/dev/input/event20";
        let mut owned = OwnedNodes::new();
        owned.register(PathBuf::from(path));
        assert_eq!(
            classify(&known_pad(path), &db(), &owned, Some((0x045e, 0x028e))),
            Verdict::Refuse(Refusal::OurOwnPresenter),
        );
    }

    /// A device that is not a pad is refused as such even when pinned — the
    /// pin selects among pads, it does not turn a keyboard into one.
    #[test]
    fn the_btn_south_filter_is_checked_before_the_pin() {
        assert_eq!(
            classify(
                &keyboard("/dev/input/event0"),
                &db(),
                &OwnedNodes::new(),
                Some((0x045e, 0x028e)),
            ),
            Verdict::Refuse(Refusal::NotAGamepad),
        );
    }

    #[test]
    fn every_refusal_explains_itself() {
        for r in [
            Refusal::NotAGamepad,
            Refusal::OurOwnPresenter,
            Refusal::NotThePinnedModel,
            Refusal::NotInTheControllerDb,
        ] {
            assert!(!r.explain().is_empty());
            assert!(
                !r.explain().contains('\n'),
                "reasons ride an IPC reply line"
            );
        }
    }

    #[test]
    fn candidate_derives_its_guid_and_wire_id() {
        let c = known_pad("/dev/input/event3");
        assert_eq!(c.guid(), "030000005e0400008e02000010010000");
        assert_eq!(c.wire_id(), "phys:usb-0000:00:14.0-1/input0");
    }
}
