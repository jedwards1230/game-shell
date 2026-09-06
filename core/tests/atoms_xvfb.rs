//! Atom round-trips against a real X server.
//!
//! These exercise the half of `atoms.rs` / `screen.rs` that unit tests cannot:
//! that the properties this crate writes are the ones an X server actually
//! stores, and that [`screen::read`] reassembles them into the state the code
//! above it believes. Everything else about those modules is pure and covered by
//! the in-crate unit tests.
//!
//! # The gate
//!
//! Every test here is `#[ignore]`d and keyed on `TV_SHELL_TEST_XVFB`, the same
//! shape `host/tests/mqtt_broker.rs` uses for `TV_SHELL_TEST_BROKER`:
//!
//! ```text
//! Xvfb :99 -screen 0 1920x1080x24 &
//! TV_SHELL_TEST_XVFB=:99 cargo test -p tv-shell-core --test atoms_xvfb -- --ignored
//! ```
//!
//! **Run with `--ignored` but no `TV_SHELL_TEST_XVFB` and these PANIC** rather
//! than quietly passing. A skipped check is indistinguishable from a passing one,
//! and deliberate opt-in is the only way these ever execute — so `cargo test`
//! stays offline and hermetic by default.
//!
//! There is deliberately **no `cfg(target_os)` gate**: `#[ignore]` already keeps
//! these out of the default run, and the file must stay compiled and
//! clippy-clean wherever the crate builds.
//!
//! # What an Xvfb server can and cannot show
//!
//! Xvfb is not gamescope: it publishes none of these atoms itself. So these
//! tests write the values and read them back, which is exactly the contract this
//! crate owns — the encode/decode and the assembly. What they cannot check is
//! whether gamescope *writes* the shapes assumed here; that is the headless
//! gamescope job in V2_DESIGN §10, and until it exists the live bench is the
//! authority. The fixtures below are therefore real measured values from
//! `dev/gamescope/lib.sh`, not invented ones (the #448 lesson).

use tv_shell_core::atoms::{names, AppId, AtomConn, FocusableWindow};
use tv_shell_core::screen::{self, AppIdSource};

/// The live reading recorded in `dev/gamescope/lib.sh` for a process launched
/// into `app-steam-app9003-2970.scope`: `GAMESCOPE_FOCUSABLE_WINDOWS` held
/// `8388625, 9003, 2998`.
const MEASURED_WINDOW: u32 = 8_388_625;
const MEASURED_APP: AppId = AppId(9003);
const MEASURED_PID: u32 = 2998;
const SHELL_APP: AppId = AppId(9001);

/// Resolve the X display from `TV_SHELL_TEST_XVFB`, or **panic**.
///
/// Deliberately not a skip: `--ignored` is already the opt-in, and a test that
/// quietly returns when its dependency is missing is indistinguishable from a
/// test that passed.
fn display() -> String {
    const VAR: &str = "TV_SHELL_TEST_XVFB";
    let raw = std::env::var(VAR).unwrap_or_default().trim().to_string();
    assert!(
        !raw.is_empty(),
        "{VAR} is unset or empty, so this test has NOTHING to talk to.\n\
         This is a PANIC and not a skip on purpose: a skipped check is \
         indistinguishable from a passing one.\n\
         Bring an X server up and opt in explicitly:\n  \
           Xvfb :99 -screen 0 1920x1080x24 &\n  \
           {VAR}=:99 cargo test -p tv-shell-core --test atoms_xvfb -- --ignored"
    );
    raw
}

fn connect() -> AtomConn {
    let d = display();
    AtomConn::connect(Some(&d)).unwrap_or_else(|e| panic!("connecting to {d}: {e}"))
}

#[test]
#[ignore = "needs a live X server: set TV_SHELL_TEST_XVFB and run with --ignored"]
fn interning_every_name_succeeds() {
    let conn = connect();
    // Iterating `names::ALL` and calling `get` proves nothing on its own — `ALL`
    // is exactly what `intern` populated, so the lookup cannot miss. The real
    // question is what the SERVER gave back, so assert on the ids: `None` (0) is
    // X's "no such atom", and two distinct names must never share an id.
    let mut seen = std::collections::HashMap::new();
    for name in names::ALL {
        let id = conn.atoms().get(name);
        assert_ne!(id, 0, "{name} interned to the None atom");
        if let Some(other) = seen.insert(id, *name) {
            panic!("{name} and {other} both interned to atom {id}");
        }
    }
    assert_eq!(seen.len(), names::ALL.len());
}

#[test]
#[ignore = "needs a live X server: set TV_SHELL_TEST_XVFB and run with --ignored"]
fn a_property_that_was_never_set_reads_as_absent_not_an_error() {
    let conn = connect();
    let win = conn.create_probe_window().unwrap();
    // Rule 1 of the atom layer, against a real server: absence is a value.
    assert_eq!(conn.read_cardinal(win, names::STEAM_GAME).unwrap(), None);
    assert!(conn
        .read_cardinals(win, names::STEAM_OVERLAY)
        .unwrap()
        .is_empty());
    assert_eq!(conn.read_flag(win, names::VRR_FEEDBACK).unwrap(), None);
}

#[test]
#[ignore = "needs a live X server: set TV_SHELL_TEST_XVFB and run with --ignored"]
fn cardinal_arrays_round_trip_through_a_real_server() {
    let conn = connect();
    let root = conn.root();
    let list = [MEASURED_APP, SHELL_APP];
    conn.set_base_layer(&list).unwrap();
    assert_eq!(conn.base_layer().unwrap(), list);

    // A replace really replaces, rather than appending.
    conn.set_base_layer(&[SHELL_APP]).unwrap();
    assert_eq!(conn.base_layer().unwrap(), vec![SHELL_APP]);

    // And a delete returns it to absent, not to an empty-but-present value we
    // would have to distinguish.
    conn.delete(root, names::BASELAYER_APPID).unwrap();
    assert!(conn.base_layer().unwrap().is_empty());
}

#[test]
#[ignore = "needs a live X server: set TV_SHELL_TEST_XVFB and run with --ignored"]
fn deleting_an_absent_property_is_a_no_op() {
    let conn = connect();
    let win = conn.create_probe_window().unwrap();
    conn.delete(win, names::STEAM_GAME).unwrap();
    conn.delete(win, names::STEAM_GAME).unwrap();
}

#[test]
#[ignore = "needs a live X server: set TV_SHELL_TEST_XVFB and run with --ignored"]
fn a_window_tag_round_trips() {
    let conn = connect();
    let win = conn.create_probe_window().unwrap();
    assert_eq!(conn.window_app_id(win).unwrap(), None);
    conn.tag_window(win, MEASURED_APP).unwrap();
    assert_eq!(conn.window_app_id(win).unwrap(), Some(MEASURED_APP));
}

#[test]
#[ignore = "needs a live X server: set TV_SHELL_TEST_XVFB and run with --ignored"]
fn focusable_window_triplets_round_trip() {
    let conn = connect();
    let root = conn.root();
    conn.write_cardinals(
        root,
        names::FOCUSABLE_WINDOWS,
        &[MEASURED_WINDOW, MEASURED_APP.0, MEASURED_PID],
    )
    .unwrap();
    assert_eq!(
        conn.read_focusable_windows().unwrap(),
        vec![FocusableWindow {
            window: MEASURED_WINDOW,
            app_id: MEASURED_APP,
            pid: MEASURED_PID
        }]
    );
    conn.delete(root, names::FOCUSABLE_WINDOWS).unwrap();
}

#[test]
#[ignore = "needs a live X server: set TV_SHELL_TEST_XVFB and run with --ignored"]
fn a_ragged_triplet_array_from_the_server_is_an_error() {
    let conn = connect();
    let root = conn.root();
    // Write a shape the code does not expect, and confirm it is refused rather
    // than truncated into a plausible-looking wrong answer.
    conn.write_cardinals(root, names::FOCUSABLE_WINDOWS, &[1, 2, 3, 4])
        .unwrap();
    assert!(conn.read_focusable_windows().is_err());
    conn.delete(root, names::FOCUSABLE_WINDOWS).unwrap();
}

#[test]
#[ignore = "needs a live X server: set TV_SHELL_TEST_XVFB and run with --ignored"]
fn screen_state_assembles_from_real_server_bytes() {
    let conn = connect();
    let root = conn.root();

    conn.write_cardinals(root, names::FOCUSED_WINDOW, &[MEASURED_WINDOW])
        .unwrap();
    conn.write_cardinals(
        root,
        names::FOCUSABLE_WINDOWS,
        &[MEASURED_WINDOW, MEASURED_APP.0, MEASURED_PID],
    )
    .unwrap();
    conn.write_app_ids(root, names::FOCUSABLE_APPS, &[MEASURED_APP, SHELL_APP])
        .unwrap();
    conn.set_base_layer(&[MEASURED_APP, SHELL_APP]).unwrap();
    conn.write_cardinals(root, names::HDR_OUTPUT_FEEDBACK, &[1])
        .unwrap();
    conn.write_cardinals(root, names::VRR_FEEDBACK, &[0])
        .unwrap();
    // GAMESCOPE_FOCUSED_APP is left UNSET on purpose: that is the measured
    // overlay case, and the state must still name what is on screen.
    conn.delete(root, names::FOCUSED_APP).unwrap();

    let state = screen::read(&conn).unwrap();

    let on = state.on_screen().expect("the focused window must resolve");
    assert_eq!(on.app_id, MEASURED_APP);
    assert_eq!(on.window, MEASURED_WINDOW);
    assert_eq!(on.source, AppIdSource::Focusable);
    assert_eq!(state.focused_app_atom_diagnostic(), None);
    assert_eq!(state.base_layer, vec![MEASURED_APP, SHELL_APP]);
    assert_eq!(state.focusable_apps, vec![MEASURED_APP, SHELL_APP]);
    assert_eq!(state.display.hdr_output, Some(true));
    // Published-false must not read as absent.
    assert_eq!(state.display.vrr, Some(false));
    assert_eq!(state.display.supports_hdr, None);

    for name in [
        names::FOCUSED_WINDOW,
        names::FOCUSABLE_WINDOWS,
        names::FOCUSABLE_APPS,
        names::BASELAYER_APPID,
        names::HDR_OUTPUT_FEEDBACK,
        names::VRR_FEEDBACK,
    ] {
        conn.delete(root, name).unwrap();
    }
}

#[test]
#[ignore = "needs a live X server: set TV_SHELL_TEST_XVFB and run with --ignored"]
fn an_empty_server_yields_an_empty_but_valid_screen_state() {
    let conn = connect();
    let root = conn.root();
    for name in names::ALL {
        conn.delete(root, name).unwrap();
    }
    // Boot state: gamescope has published nothing yet. That must be a readable
    // snapshot, not an error — the core has to serve `screen-state` before the
    // first window exists.
    let state = screen::read(&conn).unwrap();
    assert_eq!(state.focused_window, None);
    assert_eq!(state.on_screen(), None);
    assert!(state.base_layer.is_empty());
    assert!(state.focusable_windows.is_empty());
    assert_eq!(state.display.hdr_output, None);
    assert!(!state.focused_app_atom_disagrees());
}
