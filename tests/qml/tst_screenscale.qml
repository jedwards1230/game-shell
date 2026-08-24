import QtQuick
import QtTest
import "../../shell/components/screenScale.js" as ScreenScale

// Headless tests for the screen-geometry filter behind Units.screenHeight
// (shell/components/screenScale.js), imported by its real source path like the
// other pure `.pragma library` helpers — zero drift, no stubs.
//
// Units.qml itself cannot be tested here (it imports Quickshell), which is exactly
// why the decision was extracted: this is the single highest-risk piece of the
// log-noise change — every size in the shell is derived from it — and it would
// otherwise rest on reasoning alone.
//
// What these pin:
//  - a degenerate reading (empty screen list -> 0, or a ~0-height ShellScreen
//    mid-DPMS/mode-set) NEVER displaces the last good height, and returns the
//    SAME state object so a QML `var` property holding it emits no change at all
//    (that identity is what stops the whole UI rescaling and re-requesting every
//    icon);
//  - `ready` is false until a real height arrives and is one-way thereafter, so a
//    later transient cannot drop the shell back into "unknown scale";
//  - the pre-ready height is a usable number (layout must not divide by zero) but
//    is explicitly NOT marked ready, which is what keeps the icon provider from
//    being handed a guess.
TestCase {
    id: testCase
    name: "ScreenScale"

    function test_initial_is_not_ready_but_usable() {
        var s = ScreenScale.initial();
        compare(s.ready, false, "nothing has been observed yet");
        verify(s.height > 0, "layout still needs a usable number");
        compare(s.height, ScreenScale.FALLBACK_HEIGHT);
    }

    function test_valid_height_is_adopted_and_marks_ready() {
        var s = ScreenScale.adopt(ScreenScale.initial(), 1080);
        compare(s.height, 1080);
        compare(s.ready, true);
    }

    // The empty-screen-list case: `Quickshell.screens` is transiently empty during
    // startup, DPMS off/on, mode sets and CEC/TV power transitions, which the QML
    // side reports as 0.
    function test_zero_is_rejected_and_holds_the_last_good_value() {
        var good = ScreenScale.adopt(ScreenScale.initial(), 1080);
        var after = ScreenScale.adopt(good, 0);
        compare(after.height, 1080);
        compare(after.ready, true);
        verify(after === good, "same object identity — no change signal, no rescale");
    }

    // The other degenerate state: a ShellScreen that is PRESENT but reports ~0.
    function test_subfloor_height_is_rejected() {
        var good = ScreenScale.adopt(ScreenScale.initial(), 2160);
        var after = ScreenScale.adopt(good, 1);
        compare(after.height, 2160);
        verify(after === good);
        compare(ScreenScale.adopt(good, ScreenScale.MIN_VALID_HEIGHT - 1).height, 2160);
        compare(ScreenScale.adopt(good, ScreenScale.MIN_VALID_HEIGHT).height, ScreenScale.MIN_VALID_HEIGHT, "the floor itself is valid");
    }

    // A degenerate reading BEFORE anything valid must not fake readiness.
    function test_degenerate_before_first_valid_stays_not_ready() {
        var s = ScreenScale.adopt(ScreenScale.initial(), 0);
        compare(s.ready, false, "a guess must never read as ready");
        compare(s.height, ScreenScale.FALLBACK_HEIGHT);
    }

    // `ready` is one-way: once the real scale is known, a later transient keeps it.
    // If it could flip back, every AppIcon would blank and then re-request on each
    // DPMS cycle — the exact churn this change removes.
    function test_ready_is_one_way() {
        var s = ScreenScale.adopt(ScreenScale.initial(), 1080);
        s = ScreenScale.adopt(s, 0);
        compare(s.ready, true);
        s = ScreenScale.adopt(s, 2);
        compare(s.ready, true);
        compare(s.height, 1080);
    }

    // An unchanged valid reading must not churn identity either — the poll/report
    // can repeat the same height indefinitely.
    function test_identical_valid_height_is_idempotent() {
        var s = ScreenScale.adopt(ScreenScale.initial(), 1080);
        var again = ScreenScale.adopt(s, 1080);
        verify(again === s, "no change emitted for an identical reading");
    }

    // A real mode change still gets through.
    function test_real_mode_change_is_adopted() {
        var s = ScreenScale.adopt(ScreenScale.initial(), 1080);
        var changed = ScreenScale.adopt(s, 2160);
        compare(changed.height, 2160);
        compare(changed.ready, true);
        verify(changed !== s);
    }

    function test_non_numeric_readings_are_rejected() {
        var good = ScreenScale.adopt(ScreenScale.initial(), 1080);
        compare(ScreenScale.adopt(good, undefined).height, 1080);
        compare(ScreenScale.adopt(good, null).height, 1080);
        compare(ScreenScale.adopt(good, NaN).height, 1080);
        compare(ScreenScale.adopt(good, Infinity).height, 1080);
        compare(ScreenScale.adopt(good, "1080").height, 1080, "a string is not a reading");
    }

    // The QML side calls adopt() before Component.onCompleted has necessarily run.
    function test_missing_state_falls_back_to_initial() {
        var s = ScreenScale.adopt(null, 1080);
        compare(s.height, 1080);
        compare(s.ready, true);
        compare(ScreenScale.adopt(undefined, 0).ready, false);
    }
}
