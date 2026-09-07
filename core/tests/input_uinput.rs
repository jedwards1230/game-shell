//! The evdev/uinput backend against a real kernel.
//!
//! These exercise the half of `input/` that no unit test can: that `/dev/uinput`
//! accepts the canonical profile, that the kernel publishes a devnode for the
//! device we created, and that discovery then recognises that devnode as ours.
//! Everything else about the input layer is a pure decision and is covered by
//! the in-crate unit tests.
//!
//! # The gate
//!
//! Every test here is `#[ignore]`d and keyed on `TV_SHELL_TEST_UINPUT`, the same
//! shape `atoms_xvfb.rs` uses for `TV_SHELL_TEST_XVFB` and the host crate uses
//! for `TV_SHELL_TEST_BROKER`:
//!
//! ```text
//! sudo modprobe uinput
//! sudo chmod 0660 /dev/uinput && sudo chgrp input /dev/uinput   # or run as root
//! TV_SHELL_TEST_UINPUT=1 cargo test -p tv-shell-core --test input_uinput -- --ignored
//! ```
//!
//! **Run with `--ignored` but no `TV_SHELL_TEST_UINPUT` and these PANIC** rather
//! than quietly passing. A skipped check is indistinguishable from a passing one.
//!
//! There is deliberately **no `cfg(target_os)` gate**: `#[ignore]` already keeps
//! these out of the default run, and the file must stay compiled and
//! clippy-clean wherever the crate builds. The evdev calls inside are behind
//! `cfg(target_os = "linux")` because the types do not exist elsewhere.
//!
//! # What these do NOT cover, and why it matters
//!
//! Nothing here grabs a *physical* pad, because a test cannot plug one in. So
//! the claim "`EVIOCGRAB` stops the game reading the pad directly" is still
//! verified only on hardware, by a person. That is stated rather than papered
//! over: a test that grabbed a device it had itself created would be asserting
//! against its own fixture, which is the failure this crate's test strategy is
//! written against.
//!
//! # Not wired into CI yet, on purpose
//!
//! `/dev/uinput` is a kernel device, not an apt package. A GitHub-hosted runner
//! is a full VM with passwordless sudo, so `modprobe uinput` is *likely* to
//! work — but "likely" is how `atoms_xvfb.rs` spent months compiled everywhere
//! and run nowhere. Wiring the CI leg is a follow-up that has to be **verified
//! by watching it actually execute**, not assumed from the runner's shape.

#![cfg(target_os = "linux")]

use std::path::PathBuf;

use tv_shell_core::input::backend::InputBackend;
use tv_shell_core::input::discovery::{classify, Candidate, OwnedNodes, Refusal, Verdict};
use tv_shell_core::input::identity::bundled_db;
use tv_shell_core::input::presenter::{btn, PadProfile};

/// Require the opt-in and **both** permissions these tests need, or panic with
/// the remedy.
///
/// Deliberately not a skip: `--ignored` is already the opt-in, and a test that
/// quietly returns when its dependency is missing is indistinguishable from a
/// test that passed.
///
/// # Two permissions, not one — measured 2026-09-06
///
/// Writing `/dev/uinput` is enough to CREATE a presenter, and not enough to read
/// one back. On a desktop session `logind` grants the seat user an ACL on
/// `/dev/uinput`, but the `/dev/input/eventN` the kernel then publishes for that
/// device is plain `root:input 0660` with no ACL — so `create_presenter`
/// succeeds and every readback fails `EACCES`, and `evdev::enumerate()` silently
/// omits the device because it skips what it cannot open.
///
/// That surfaced as "the presenter must appear in an enumeration" — a confusing
/// assertion failure a long way from the cause. The preflight below turns it
/// into one line naming the group, in the same spirit as `atoms_xvfb.rs` failing
/// by name when Xvfb never comes up rather than letting a dead server present as
/// a client-side error inside a test.
fn require_opt_in() {
    const VAR: &str = "TV_SHELL_TEST_UINPUT";
    let raw = std::env::var(VAR).unwrap_or_default();
    assert!(
        !raw.trim().is_empty(),
        "{VAR} is unset, so this test has NO kernel to talk to.\n\
         This is a PANIC and not a skip on purpose: a skipped check is \
         indistinguishable from a passing one.\n\
         Run:  sudo modprobe uinput && TV_SHELL_TEST_UINPUT=1 cargo test \
         -p tv-shell-core --test input_uinput -- --ignored"
    );
    assert!(
        PathBuf::from("/dev/uinput").exists(),
        "/dev/uinput does not exist. Load the module: sudo modprobe uinput"
    );

    // Preflight: create a throwaway presenter and read its devnode back. Both
    // halves are required by every test below, and they fail for different
    // reasons with different fixes.
    let mut backend = tv_shell_core::input::evdev_backend::EvdevBackend::new();
    let nodes = backend
        .create_presenter(0, &PadProfile::canonical())
        .unwrap_or_else(|e| {
            panic!(
                "cannot create a uinput device: {e}\n\
                 Need WRITE access to /dev/uinput — join the `input` group, or run as root."
            )
        });
    let node = nodes
        .first()
        .expect("a created presenter has a devnode")
        .clone();
    if let Err(e) = evdev::Device::open(&node) {
        panic!(
            "created the presenter at {} but cannot READ it back: {e}\n\
             Writing /dev/uinput is enough to create a device and NOT enough to open the\n\
             /dev/input/eventN the kernel publishes for it (root:input 0660, no ACL).\n\
             Fix:  sudo usermod -aG input \"$USER\"   (then log out and back in)\n\
             Or run the suite as root.",
            node.display()
        );
    }
}

/// Read one enumerated device back out of `/dev/input` as discovery would see
/// it, by devnode path.
fn enumerate_by_path(backend: &mut impl InputBackend, path: &std::path::Path) -> Option<Candidate> {
    backend
        .enumerate()
        .expect("enumerating /dev/input")
        .into_iter()
        .find(|c| c.path == path)
}

/// **The kernel really does publish a devnode for the presenter we create, and
/// discovery really does refuse it.**
///
/// This is the one hardware claim the unit tests cannot make. `session.rs`
/// asserts that a presenter reporting no devnode fails the start, and that a
/// devnode registered as ours is refused — but both use a double, so neither
/// says the kernel hands us a devnode at all, nor that the device it publishes
/// carries the database-known `input_id` that makes the ownership check
/// load-bearing in the first place.
///
/// If this fails, the session eats itself: the very next discovery poll sees a
/// database-known pad it does not recognise as its own and grabs it, feeding the
/// presenter's output back into the presenter.
#[test]
#[ignore = "needs /dev/uinput: set TV_SHELL_TEST_UINPUT and run with --ignored"]
fn a_created_presenter_gets_a_devnode_that_discovery_refuses_as_ours() {
    require_opt_in();

    let mut backend = tv_shell_core::input::evdev_backend::EvdevBackend::new();
    let profile = PadProfile::canonical();

    let devnodes = backend
        .create_presenter(0, &profile)
        .expect("the kernel must accept the canonical profile on /dev/uinput");
    assert!(
        !devnodes.is_empty(),
        "the kernel published no devnode for the presenter"
    );

    let node = &devnodes[0];
    let seen = enumerate_by_path(&mut backend, node)
        .expect("the presenter must appear in an enumeration of /dev/input");

    // It carries the canonical identity — which is exactly why the database gate
    // cannot save us here and devnode ownership has to.
    assert_eq!(
        (seen.vendor, seen.product),
        (profile.vendor, profile.product)
    );
    assert!(
        seen.has_btn_south,
        "it must look like a pad, or it is not one"
    );
    assert!(
        bundled_db().is_known(seen.vendor, seen.product),
        "precondition: the presenter's id IS database-known, so the db gate would claim it"
    );

    // Unowned, the gate would claim our own device.
    assert_eq!(
        classify(&seen, &bundled_db(), &OwnedNodes::new(), None),
        Verdict::Claim,
        "without ownership the gate claims the presenter — this is the hazard"
    );

    // Owned, it is refused. This is the check that keeps the loop closed.
    let mut owned = OwnedNodes::new();
    for n in &devnodes {
        owned.register(n.clone());
    }
    assert_eq!(
        classify(&seen, &bundled_db(), &owned, None),
        Verdict::Refuse(Refusal::OurOwnPresenter),
    );
}

/// **The presenter advertises the profile we asked for, and rests at neutral.**
///
/// A uinput device silently drops events for codes it never declared, so a
/// profile the kernel did not actually accept would show up as input that simply
/// vanishes. Resting position matters too: a presenter created before any pad
/// connects must read as a controller at rest, not one with its triggers held.
#[test]
#[ignore = "needs /dev/uinput: set TV_SHELL_TEST_UINPUT and run with --ignored"]
fn a_created_presenter_advertises_the_canonical_profile_at_rest() {
    require_opt_in();

    let mut backend = tv_shell_core::input::evdev_backend::EvdevBackend::new();
    let profile = PadProfile::canonical();
    let devnodes = backend.create_presenter(0, &profile).expect("create");

    let device = evdev::Device::open(&devnodes[0]).expect("opening our own presenter");

    let keys = device.supported_keys().expect("it must advertise keys");
    for &code in profile.keys() {
        assert!(
            keys.contains(evdev::KeyCode::new(code)),
            "the presenter dropped button {code:#x} from the profile"
        );
    }
    assert!(
        keys.contains(evdev::KeyCode::new(btn::MODE)),
        "Home is present"
    );

    let absinfo: std::collections::BTreeMap<u16, evdev::AbsInfo> = device
        .get_absinfo()
        .expect("it must advertise axes")
        .map(|(code, info)| (code.0, info))
        .collect();

    for &(code, range) in profile.axes() {
        let info = absinfo
            .get(&code)
            .unwrap_or_else(|| panic!("the presenter dropped axis {code:#x}"));
        assert_eq!(info.minimum(), range.min, "axis {code:#x} minimum");
        assert_eq!(info.maximum(), range.max, "axis {code:#x} maximum");
        assert_eq!(
            info.value(),
            range.neutral(),
            "axis {code:#x} must rest at neutral, not mid-range"
        );
    }
}

/// **Creating presenters out of slot order is refused, in a release build.**
///
/// `emit` indexes `presenters` by `slot as usize`, so a presenter pushed at the
/// wrong index routes one player's input to another player's device — silently,
/// and with no way to notice from either end. This was a `debug_assert_eq!`,
/// which compiles to nothing in exactly the release build that runs on the
/// couch: the check would have been absent precisely where the corruption
/// matters. Flagged in review on jedwards1230/tv-shell#466.
///
/// The test lives here rather than beside a double because the ordering
/// invariant belongs to the backend that holds the `Vec`, and a double would be
/// asserting against its own bookkeeping.
#[test]
#[ignore = "needs /dev/uinput: set TV_SHELL_TEST_UINPUT and run with --ignored"]
fn creating_presenters_out_of_order_is_refused() {
    require_opt_in();

    let mut backend = tv_shell_core::input::evdev_backend::EvdevBackend::new();
    let profile = PadProfile::canonical();

    // Slot 1 with no slot 0 yet would land at index 0 and answer to slot 1.
    let out_of_order = backend.create_presenter(1, &profile);
    assert!(
        out_of_order.is_err(),
        "creating slot 1 before slot 0 must fail, not silently misalign the table"
    );

    // In order, it works — so the check is an ordering rule and not a refusal
    // of everything.
    backend.create_presenter(0, &profile).expect("slot 0");
    backend.create_presenter(1, &profile).expect("slot 1");

    // And a repeat of a slot already created is equally refused: it would push
    // a second device at an index that is now past its own slot.
    assert!(
        backend.create_presenter(1, &profile).is_err(),
        "re-creating an existing slot must fail"
    );
}

/// **The presenters are independent devices, one per player.**
///
/// They share a profile, so a bug that returned the same device twice would be
/// invisible to anything checking capabilities. The devnodes must differ, or two
/// players drive one virtual pad.
#[test]
#[ignore = "needs /dev/uinput: set TV_SHELL_TEST_UINPUT and run with --ignored"]
fn each_player_gets_its_own_presenter_device() {
    require_opt_in();

    let mut backend = tv_shell_core::input::evdev_backend::EvdevBackend::new();
    let profile = PadProfile::canonical();

    let p0 = backend.create_presenter(0, &profile).expect("player 0");
    let p1 = backend.create_presenter(1, &profile).expect("player 1");

    assert_ne!(p0, p1, "two players must not share one presenter devnode");

    // And they are named per slot, so an operator reading
    // /proc/bus/input/devices can tell them apart.
    //
    // Asserted WITHOUT calling `device_name`. Comparing each device against
    // `device_name(n)` compares the naming function to itself: a mutation that
    // dropped the slot from every name moved both sides together and this test
    // passed. Measured 2026-09-06 — it survived exactly that mutation. So the
    // properties checked here are the ones an operator actually relies on, each
    // independent of how the name is built.
    let name0 = evdev::Device::open(&p0[0])
        .expect("open")
        .name()
        .unwrap_or_default()
        .to_string();
    let name1 = evdev::Device::open(&p1[0])
        .expect("open")
        .name()
        .unwrap_or_default()
        .to_string();

    assert_ne!(
        name0, name1,
        "two presenters must not share a name, or /proc/bus/input/devices \
         cannot tell P1 from P2"
    );
    assert!(
        name0.contains('0') && name1.contains('1'),
        "each presenter's name must carry its own slot: {name0:?} / {name1:?}"
    );
    for name in [&name0, &name1] {
        assert!(
            name.starts_with("tv-shell-"),
            "a presenter must be identifiable as ours by name for a human \
             reading the device list: {name:?}"
        );
    }
}
