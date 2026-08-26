import QtQuick
import QtTest
import "../../shell/components/resumeFocus.js" as ResumeFocus

// Headless tests for the resume-focus decision logic (#347). resumeFocus.js is a
// pure `.pragma library` imported by its real source path (zero drift) — no
// Quickshell, no stubs.
//
// WHAT THESE PIN, and why it matters more than usual: the bug being fixed was
// INVISIBLE at runtime. `hyprctl dispatch` exits 0 even when its selector
// matched no window, and the miss branch used to `return` with no log at all, so
// neither an exit code nor a journal could tell a working resume from a dead
// one. On-device observation could not distinguish these branches; assertions
// can. Every branch below is one the device could not show us.
TestCase {
    id: testCase
    name: "ResumeFocus"

    function _win(address, windowClass) {
        return {
            address: address,
            windowClass: windowClass
        };
    }

    // --- resolve(): picking a selector -------------------------------------

    function test_known_address_takes_the_precise_path() {
        var d = ResumeFocus.resolve("0xaaa", "", [_win("0xaaa", "tv.plex.Plex")]);
        compare(d.mode, ResumeFocus.MODE_ADDRESS);
        compare(d.address, "0xaaa");
        compare(d.windowClass, "tv.plex.Plex", "class comes from the snapshot, which is authoritative");
        compare(d.reason, "");
    }

    // The snapshot wins over a caller-supplied class: the poller read it from the
    // compositor, the caller's copy may be a lowercased StartupWMClass.
    function test_snapshot_class_overrides_the_caller_hint() {
        var d = ResumeFocus.resolve("0xaaa", "plex", [_win("0xaaa", "tv.plex.Plex")]);
        compare(d.mode, ResumeFocus.MODE_ADDRESS);
        compare(d.windowClass, "tv.plex.Plex");
    }

    function test_correct_window_is_chosen_among_several() {
        var d = ResumeFocus.resolve("0xbbb", "", [_win("0xaaa", "steam"), _win("0xbbb", "tv.plex.Plex"), _win("0xccc", "other")]);
        compare(d.mode, ResumeFocus.MODE_ADDRESS);
        compare(d.windowClass, "tv.plex.Plex");
    }

    // THE REGRESSION TEST FOR #347. This exact input used to hit `if (!found)
    // return;` — no focus, no launch, no log.
    function test_unknown_address_with_class_falls_back_instead_of_vanishing() {
        var d = ResumeFocus.resolve("0xstale", "tv.plex.Plex", [_win("0xaaa", "steam")]);
        compare(d.mode, ResumeFocus.MODE_CLASS, "a stale snapshot must degrade to class focus, not to silence");
        compare(d.windowClass, "tv.plex.Plex");
        compare(d.reason, ResumeFocus.REASON_UNKNOWN_ADDRESS, "the reason is what makes the fallback greppable");
    }

    function test_unknown_address_without_class_is_reported_not_silent() {
        var d = ResumeFocus.resolve("0xstale", "", [_win("0xaaa", "steam")]);
        compare(d.mode, ResumeFocus.MODE_NONE);
        compare(d.reason, ResumeFocus.REASON_UNKNOWN_ADDRESS, "MODE_NONE still carries a reason so the caller can log WHY");
    }

    function test_empty_address_with_class_still_focuses() {
        var d = ResumeFocus.resolve("", "tv.plex.Plex", [_win("0xaaa", "steam")]);
        compare(d.mode, ResumeFocus.MODE_CLASS);
        compare(d.reason, ResumeFocus.REASON_NO_ADDRESS);
    }

    function test_no_address_and_no_class_is_the_only_true_noop() {
        var d = ResumeFocus.resolve("", "", []);
        compare(d.mode, ResumeFocus.MODE_NONE);
        compare(d.reason, ResumeFocus.REASON_NO_ADDRESS);
    }

    function test_empty_snapshot_does_not_match_an_empty_address() {
        // A window with no address must never be matched by an empty address —
        // that would resume an arbitrary window.
        var d = ResumeFocus.resolve("", "", [_win("", "steam")]);
        compare(d.mode, ResumeFocus.MODE_NONE, "an empty address must not match an addressless window");
    }

    function test_null_and_undefined_inputs_do_not_throw() {
        var d = ResumeFocus.resolve(null, undefined, null);
        compare(d.mode, ResumeFocus.MODE_NONE);
        var d2 = ResumeFocus.resolve("0xaaa", null, [null, _win("0xaaa", "steam")]);
        compare(d2.mode, ResumeFocus.MODE_ADDRESS, "a null entry in the snapshot must not abort the scan");
    }

    // --- verifyFocus(): did the dispatch land? -----------------------------

    function test_address_focus_that_landed_verifies_ok() {
        var d = ResumeFocus.resolve("0xaaa", "", [_win("0xaaa", "tv.plex.Plex")]);
        var r = ResumeFocus.verifyFocus(d, {
            "class": "tv.plex.Plex",
            "address": "0xaaa",
            "fullscreen": true
        });
        verify(r.ok);
        compare(r.reason, "");
    }

    // The measured #347 state: focus dispatched at Plex, Steam still active.
    // Exit code was 0. Only this comparison can tell.
    function test_address_focus_that_hit_nothing_is_detected() {
        var d = ResumeFocus.resolve("0xaaa", "", [_win("0xaaa", "tv.plex.Plex")]);
        var r = ResumeFocus.verifyFocus(d, {
            "class": "steam",
            "address": "0xbbb",
            "fullscreen": true
        });
        verify(!r.ok, "a dispatch that left a DIFFERENT window active must not read as success");
        compare(r.reason, ResumeFocus.REASON_ADDRESS_MISMATCH);
    }

    function test_class_focus_verifies_case_insensitively() {
        var d = ResumeFocus.resolve("0xstale", "tv.plex.plex", []);
        var r = ResumeFocus.verifyFocus(d, {
            "class": "tv.plex.Plex",
            "address": "0xaaa"
        });
        verify(r.ok, "Hyprland reports the window's own casing; a case difference is not a miss");
    }

    function test_class_focus_that_hit_nothing_is_detected() {
        var d = ResumeFocus.resolve("0xstale", "tv.plex.Plex", []);
        var r = ResumeFocus.verifyFocus(d, {
            "class": "steam",
            "address": "0xbbb"
        });
        verify(!r.ok);
        compare(r.reason, ResumeFocus.REASON_CLASS_MISMATCH);
    }

    // `hypr-active` answers `{}` when nothing is focused, and on IPC failure.
    function test_empty_active_window_is_a_miss_not_a_pass() {
        var d = ResumeFocus.resolve("0xaaa", "", [_win("0xaaa", "tv.plex.Plex")]);
        var r = ResumeFocus.verifyFocus(d, {});
        verify(!r.ok, "an empty hypr-active reply must never read as a successful resume");
        compare(r.reason, ResumeFocus.REASON_NO_ACTIVE_WINDOW);
    }

    function test_verify_handles_null_active() {
        var d = ResumeFocus.resolve("0xaaa", "", [_win("0xaaa", "tv.plex.Plex")]);
        var r = ResumeFocus.verifyFocus(d, null);
        verify(!r.ok);
    }

    // Nothing was dispatched, so nothing can have landed — verifying a MODE_NONE
    // decision must never report success.
    function test_none_decision_never_verifies_ok() {
        var d = ResumeFocus.resolve("", "", []);
        var r = ResumeFocus.verifyFocus(d, {
            "class": "steam",
            "address": "0xbbb"
        });
        verify(!r.ok);
        compare(r.reason, ResumeFocus.REASON_NO_TARGET);
    }

    function test_verify_handles_missing_decision() {
        var r = ResumeFocus.verifyFocus(null, {
            "class": "steam"
        });
        verify(!r.ok);
        compare(r.reason, ResumeFocus.REASON_NO_TARGET);
    }

    // --- shouldAssertFullscreen(): the ORDERING guard ----------------------
    //
    // `hyprctl dispatch fullscreen 0 set` takes no window selector — it acts on
    // whatever is active when it runs. These pin that we only ever issue it once
    // the compositor has confirmed OUR window is the active one. They are the
    // headless stand-in for a race that cannot be reproduced on demand on-device.

    function test_fullscreen_asserted_once_the_intended_window_is_active() {
        var d = ResumeFocus.resolve("0xaaa", "", [_win("0xaaa", "tv.plex.Plex")]);
        var r = ResumeFocus.shouldAssertFullscreen(d, {
            "class": "tv.plex.Plex",
            "address": "0xaaa",
            "fullscreen": false
        });
        verify(r.assert, "a resumed window confirmed active and still tiled is exactly the case QML exists to fix");
        compare(r.reason, "");
    }

    // THE REGRESSION TEST FOR THE ORDERING DEFECT. Resume tiled Plex while
    // fullscreen Steam is still active: asserting here would fullscreen STEAM,
    // reproducing #347. Idempotence is no defence — `set` is idempotent in which
    // STATE it applies, not which WINDOW.
    function test_fullscreen_not_asserted_while_the_previous_window_is_still_active() {
        var d = ResumeFocus.resolve("0xplex", "", [_win("0xplex", "tv.plex.Plex")]);
        var r = ResumeFocus.shouldAssertFullscreen(d, {
            "class": "steam",
            "address": "0xsteam",
            "fullscreen": true
        });
        verify(!r.assert, "asserting fullscreen while the PREVIOUS window is active re-fullscreens that window — the #347 bug");
        compare(r.reason, ResumeFocus.REASON_ADDRESS_MISMATCH);
    }

    function test_fullscreen_not_asserted_on_a_class_miss() {
        var d = ResumeFocus.resolve("0xstale", "tv.plex.Plex", []);
        var r = ResumeFocus.shouldAssertFullscreen(d, {
            "class": "steam",
            "address": "0xsteam",
            "fullscreen": true
        });
        verify(!r.assert);
        compare(r.reason, ResumeFocus.REASON_CLASS_MISMATCH);
    }

    // Nothing active at all: `fullscreen 0 set` prints "Window not found" and
    // exits 0, so a blind dispatch here is silently wasted rather than caught.
    function test_fullscreen_not_asserted_when_nothing_is_active() {
        var d = ResumeFocus.resolve("0xaaa", "", [_win("0xaaa", "tv.plex.Plex")]);
        var r = ResumeFocus.shouldAssertFullscreen(d, {});
        verify(!r.assert);
        compare(r.reason, ResumeFocus.REASON_NO_ACTIVE_WINDOW);
    }

    function test_fullscreen_not_asserted_for_a_none_decision() {
        var d = ResumeFocus.resolve("", "", []);
        var r = ResumeFocus.shouldAssertFullscreen(d, {
            "class": "steam",
            "address": "0xsteam"
        });
        verify(!r.assert, "nothing was dispatched, so no window was ever aimed at");
        compare(r.reason, ResumeFocus.REASON_NO_TARGET);
    }

    // Mirrors the daemon's needs_fullscreen skip: already-fullscreen is a no-op.
    function test_already_fullscreen_window_is_left_alone() {
        var d = ResumeFocus.resolve("0xaaa", "", [_win("0xaaa", "tv.plex.Plex")]);
        var r = ResumeFocus.shouldAssertFullscreen(d, {
            "class": "tv.plex.Plex",
            "address": "0xaaa",
            "fullscreen": true
        });
        verify(!r.assert, "the declarative swap already fired — re-dispatching is pointless churn");
        compare(r.reason, ResumeFocus.REASON_ALREADY_FULLSCREEN);
    }

    // Fail-safe direction: an absent/unknown `fullscreen` field must still
    // assert. A redundant idempotent `set` is harmless; a skipped needed one
    // leaves the resumed window focused-but-invisible, which is the #347 symptom.
    function test_unknown_fullscreen_field_still_asserts() {
        var d = ResumeFocus.resolve("0xaaa", "", [_win("0xaaa", "tv.plex.Plex")]);
        var r = ResumeFocus.shouldAssertFullscreen(d, {
            "class": "tv.plex.Plex",
            "address": "0xaaa"
        });
        verify(r.assert, "a missing fullscreen field must not suppress the assertion");
        var r2 = ResumeFocus.shouldAssertFullscreen(d, {
            "class": "tv.plex.Plex",
            "address": "0xaaa",
            "fullscreen": 0
        });
        verify(r2.assert, "fullscreen:0 means NOT fullscreen — assert");
    }

    function test_should_assert_handles_null_inputs() {
        var r = ResumeFocus.shouldAssertFullscreen(null, null);
        verify(!r.assert);
    }

    // --- resolveWorkspaceMove(): workspace consolidation --------------------
    //
    // The device could not show us these branches either: a resume that focused a
    // window on an off-screen workspace changed nothing visible, and `hyprctl
    // dispatch` reported success for it. These assertions are the only place the
    // "did we decide to consolidate, and onto what" question is answerable.

    function _wsWin(address, windowClass, workspace) {
        return {
            address: address,
            windowClass: windowClass,
            workspace: workspace
        };
    }

    function _monitors(activeWorkspace) {
        return [
            {
                name: "HDMI-A-1",
                activeWorkspace: activeWorkspace
            }
        ];
    }

    // The reported bug, reduced: Steam on workspace 4, TV displaying workspace 2.
    function test_target_on_another_workspace_is_pulled_onto_the_displayed_one() {
        var windows = [_wsWin("0xsteam", "steam", "4")];
        var d = ResumeFocus.resolve("0xsteam", "", windows);
        var plan = ResumeFocus.resolveWorkspaceMove(d, _monitors("2"), windows);
        verify(plan.move, "a target off the displayed workspace must be consolidated");
        compare(plan.workspace, "2", "destination is the DISPLAYED workspace, not the window's");
        compare(plan.address, "0xsteam");
    }

    function test_target_already_on_the_displayed_workspace_is_left_alone() {
        var windows = [_wsWin("0xaaa", "tv.plex.Plex", "1")];
        var d = ResumeFocus.resolve("0xaaa", "", windows);
        var plan = ResumeFocus.resolveWorkspaceMove(d, _monitors("1"), windows);
        verify(!plan.move);
        compare(plan.reason, ResumeFocus.REASON_ALREADY_ON_ACTIVE);
    }

    // The CLASS path exists because our snapshot goes stale; consolidation must
    // still work there, and must still move by ADDRESS (moving by class would
    // pick whichever window Hyprland matched first).
    function test_class_fallback_consolidates_by_address() {
        var windows = [_wsWin("0xbbb", "steam", "4")];
        var d = ResumeFocus.resolve("0xstale", "steam", windows);
        compare(d.mode, ResumeFocus.MODE_CLASS);
        var plan = ResumeFocus.resolveWorkspaceMove(d, _monitors("2"), windows);
        verify(plan.move);
        compare(plan.address, "0xbbb", "must resolve the class back to a concrete address");
    }

    // Every "we could not establish this" branch must degrade to the
    // pre-change behaviour (focus alone), never to a speculative move.
    function test_unknown_inputs_never_move() {
        var windows = [_wsWin("0xaaa", "steam", "4")];
        var d = ResumeFocus.resolve("0xaaa", "", windows);

        compare(ResumeFocus.resolveWorkspaceMove(d, [], windows).reason, ResumeFocus.REASON_NO_MONITORS);
        verify(!ResumeFocus.resolveWorkspaceMove(d, [], windows).move);

        compare(ResumeFocus.resolveWorkspaceMove(d, _monitors(""), windows).reason, ResumeFocus.REASON_NO_ACTIVE_WORKSPACE);

        var noWs = [_wsWin("0xaaa", "steam", "")];
        compare(ResumeFocus.resolveWorkspaceMove(ResumeFocus.resolve("0xaaa", "", noWs), _monitors("2"), noWs).reason, ResumeFocus.REASON_UNKNOWN_TARGET_WORKSPACE);

        var none = ResumeFocus.resolve("", "", []);
        compare(none.mode, ResumeFocus.MODE_NONE);
        compare(ResumeFocus.resolveWorkspaceMove(none, _monitors("2"), []).reason, ResumeFocus.REASON_NO_TARGET);
    }

    function test_resolve_workspace_move_handles_null_inputs() {
        verify(!ResumeFocus.resolveWorkspaceMove(null, null, null).move);
    }

    // A multi-head box cannot say which output is focused (hypr-monitors carries
    // no `focused` field), so the first monitor is taken as the display — the
    // stated single-output kiosk assumption.
    function test_active_workspace_reads_the_first_monitor() {
        compare(ResumeFocus.activeWorkspaceOf(_monitors("7")), "7");
        compare(ResumeFocus.activeWorkspaceOf([]), "");
        compare(ResumeFocus.activeWorkspaceOf(null), "");
    }

    // --- workspaceSelector(): id vs named workspaces ------------------------

    function test_numeric_workspace_is_a_bare_id_selector() {
        compare(ResumeFocus.workspaceSelector("2"), "2");
    }

    // A named workspace passed bare would be parsed as an id, match nothing, and
    // still exit 0 — failing exactly as silently as the bug being fixed.
    function test_named_workspace_is_qualified() {
        compare(ResumeFocus.workspaceSelector("games"), "name:games");
        compare(ResumeFocus.workspaceSelector(""), "");
    }

    // --- stamp()/isStale(): superseded-resume suppression -------------------
    //
    // A resume is a chain of async hops and every one is a place a second resume
    // can start. This is the field failure, reduced: two resumes in quick
    // succession, and the FIRST one's verification running against state the
    // SECOND one produced. Before generations that read as a focus miss, and
    // (once a miss started recovering) bounced the user to the shell mid-resume.

    function test_a_stamped_decision_is_current_at_its_own_generation() {
        var d = ResumeFocus.stamp(ResumeFocus.resolve("0xaaa", "", [_win("0xaaa", "steam")]), 7);
        compare(d.generation, 7);
        verify(!ResumeFocus.isStale(d, 7));
    }

    function test_a_superseded_decision_is_stale() {
        var first = ResumeFocus.stamp(ResumeFocus.resolve("0xaaa", "", [_win("0xaaa", "steam")]), 1);
        // A second resume claims the next generation.
        verify(ResumeFocus.isStale(first, 2), "the earlier resume must be suppressed once a newer one starts");
    }

    // The exact shape of the observed journal lines: Steam resumed, then Plex
    // resumed, then Steam's verification arrives to find Plex active. It must be
    // dropped rather than reported as a miss.
    function test_the_observed_crossed_verification_is_suppressed() {
        var windows = [_wsWin("0xsteam", "steam", "4"), _wsWin("0xplex", "tv.plex.Plex", "1")];
        var steam = ResumeFocus.stamp(ResumeFocus.resolve("0xsteam", "", windows), 1);
        ResumeFocus.stamp(ResumeFocus.resolve("0xplex", "", windows), 2);

        // Steam's verification would otherwise judge itself against Plex.
        var wouldMiss = ResumeFocus.verifyFocus(steam, {
            "class": "tv.plex.Plex",
            "address": "0xplex"
        });
        verify(!wouldMiss.ok, "sanity: without generations this reads as a miss");
        compare(wouldMiss.reason, ResumeFocus.REASON_ADDRESS_MISMATCH);

        // With generations it never gets that far.
        verify(ResumeFocus.isStale(steam, 2), "the crossed verification must be dropped before it judges anything");
    }

    // An unstamped decision must still be acted on — dropping work for a caller
    // that never opted into generations would be worse than the race.
    function test_an_unstamped_decision_is_never_stale() {
        var d = ResumeFocus.resolve("0xaaa", "", [_win("0xaaa", "steam")]);
        compare(d.generation, undefined);
        verify(!ResumeFocus.isStale(d, 99));
    }

    function test_stamp_and_isStale_handle_null_inputs() {
        compare(ResumeFocus.stamp(null, 3), null);
        verify(!ResumeFocus.isStale(null, 3));
    }
}
