import QtQuick
import QtTest
import "../../shell/components/resumeFocus.js" as ResumeFocus

// Headless tests for the resume decision logic. resumeFocus.js is a pure
// `.pragma library` imported by its real source path (zero drift) — no
// Quickshell, no stubs.
//
// WHAT THESE PIN, and why it matters more than usual: the bugs on this path were
// INVISIBLE at runtime. `hyprctl dispatch` exits 0 even when its selector matched
// no window, so neither an exit code nor a journal could tell a working resume
// from a dead one. On-device observation could not distinguish these branches;
// assertions can.
//
// The headline case, from the field (2026-08-26): a live Steam Remote Play
// `streaming_client` window and Steam Big Picture are two separate windows the
// user must be able to switch between. Under the old focus-based model the
// stream reported `acceptsInput: false`, `dispatch focuswindow` returned ok and
// did nothing, and the stream was unreachable. Resolution is now
// window -> workspace, and dispatch is a workspace switch no window can refuse.
TestCase {
    id: testCase
    name: "ResumeFocus"

    function _win(address, windowClass, workspace) {
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

    // The real shape observed on-device: Big Picture and the live stream, each on
    // its own workspace, plus an unrelated app.
    readonly property var fleet: [_win("0xplex", "tv.plex.Plex", "2"), _win("0xsteam", "steam", "3"), _win("0xgame", "streaming_client", "4")]

    // --- resolveTarget(): finding the window and its workspace ---------------

    function test_address_resolves_to_that_windows_workspace() {
        var t = ResumeFocus.resolveTarget("0xgame", "", testCase.fleet);
        compare(t.workspace, "4");
        compare(t.windowClass, "streaming_client");
        compare(t.reason, "");
        verify(ResumeFocus.canDispatch(t));
    }

    // THE REGRESSION GUARD. Steam and the game stream must resolve to DIFFERENT
    // workspaces — they are two destinations, and collapsing them is what left
    // the user with no route to the running game.
    function test_steam_and_its_stream_are_separate_destinations() {
        var steam = ResumeFocus.resolveTarget("0xsteam", "", testCase.fleet);
        var game = ResumeFocus.resolveTarget("0xgame", "", testCase.fleet);
        verify(ResumeFocus.canDispatch(steam));
        verify(ResumeFocus.canDispatch(game));
        verify(steam.workspace !== game.workspace, "resuming the stream must not land on Steam's workspace");
    }

    function test_class_is_the_fallback_when_no_address_is_known() {
        // The "resume a recent app" path holds a desktop entry, not a window.
        var t = ResumeFocus.resolveTarget("", "steam", testCase.fleet);
        compare(t.workspace, "3");
        compare(t.address, "0xsteam", "the address is recovered from the snapshot");
    }

    function test_address_wins_over_class() {
        // Both given, and they disagree: the address is the specific window the
        // user pressed, so it must win.
        var t = ResumeFocus.resolveTarget("0xgame", "steam", testCase.fleet);
        compare(t.workspace, "4");
        compare(t.windowClass, "streaming_client");
    }

    function test_unknown_address_falls_back_to_class() {
        // Our snapshot is a poll up to a few seconds old, so an address we can't
        // find usually means WE are stale, not that the app is gone.
        var t = ResumeFocus.resolveTarget("0xstale", "steam", testCase.fleet);
        compare(t.workspace, "3");
        compare(t.reason, "");
    }

    function test_no_match_at_all_is_a_reported_failure_not_a_silent_no_op() {
        var t = ResumeFocus.resolveTarget("0xstale", "nosuchapp", testCase.fleet);
        compare(t.reason, ResumeFocus.REASON_NO_TARGET);
        compare(t.workspace, "");
        verify(!ResumeFocus.canDispatch(t));
    }

    // A window present but with no workspace means the client list was published
    // before the daemon parked it. Guessing would be worse than failing:
    // dispatching an empty selector is a syntax error, and defaulting to home
    // would hide the app the user just asked for.
    function test_window_without_a_workspace_refuses_to_dispatch() {
        var t = ResumeFocus.resolveTarget("0xnew", "", [_win("0xnew", "steam", "")]);
        compare(t.reason, ResumeFocus.REASON_NO_WORKSPACE);
        verify(!ResumeFocus.canDispatch(t));
    }

    function test_null_and_empty_inputs_are_safe() {
        verify(!ResumeFocus.canDispatch(ResumeFocus.resolveTarget("", "", [])));
        verify(!ResumeFocus.canDispatch(ResumeFocus.resolveTarget(null, null, null)));
        verify(!ResumeFocus.canDispatch(null));
    }

    // --- workspaceSelector() -------------------------------------------------

    function test_numeric_workspaces_pass_through_bare() {
        compare(ResumeFocus.workspaceSelector("4"), "4");
        compare(ResumeFocus.workspaceSelector(4), "4");
    }

    function test_named_workspaces_get_the_name_prefix() {
        compare(ResumeFocus.workspaceSelector("special:games"), "name:special:games");
        compare(ResumeFocus.workspaceSelector("scratch"), "name:scratch");
    }

    function test_empty_workspace_yields_no_selector() {
        compare(ResumeFocus.workspaceSelector(""), "");
        compare(ResumeFocus.workspaceSelector(null), "");
    }

    // --- verifyLanding(): one integer ---------------------------------------

    function test_landing_confirmed_when_the_workspace_is_displayed() {
        var t = ResumeFocus.resolveTarget("0xgame", "", testCase.fleet);
        var res = ResumeFocus.verifyLanding(t, _monitors("4"));
        verify(res.landed);
        compare(res.reason, "");
        verify(!ResumeFocus.isRealMiss(res));
    }

    function test_landing_missed_when_a_different_workspace_is_displayed() {
        var t = ResumeFocus.resolveTarget("0xgame", "", testCase.fleet);
        var res = ResumeFocus.verifyLanding(t, _monitors("3"));
        verify(!res.landed);
        compare(res.reason, ResumeFocus.REASON_WORKSPACE_MISMATCH);
        // Only THIS reason may bounce the user back to the shell.
        verify(ResumeFocus.isRealMiss(res));
    }

    // An unreadable probe is not evidence the resume failed. Treating it as one
    // would yank the user out of a working app over a socket hiccup — which is
    // why the recovery is gated on isRealMiss rather than on `!landed`.
    function test_unreadable_probe_is_not_a_real_miss() {
        var t = ResumeFocus.resolveTarget("0xgame", "", testCase.fleet);

        var noMonitors = ResumeFocus.verifyLanding(t, []);
        verify(!noMonitors.landed);
        compare(noMonitors.reason, ResumeFocus.REASON_NO_MONITORS);
        verify(!ResumeFocus.isRealMiss(noMonitors));

        var noActive = ResumeFocus.verifyLanding(t, _monitors(""));
        verify(!noActive.landed);
        compare(noActive.reason, ResumeFocus.REASON_NO_ACTIVE_WORKSPACE);
        verify(!ResumeFocus.isRealMiss(noActive));

        var malformed = ResumeFocus.verifyLanding(t, null);
        verify(!malformed.landed);
        verify(!ResumeFocus.isRealMiss(malformed));
    }

    function test_verify_without_a_target_is_not_a_real_miss() {
        var res = ResumeFocus.verifyLanding(null, _monitors("4"));
        verify(!res.landed);
        compare(res.reason, ResumeFocus.REASON_NO_TARGET);
        verify(!ResumeFocus.isRealMiss(res));
    }

    // --- activeWorkspaceOf(): reply shapes ----------------------------------

    function test_active_workspace_accepts_a_bare_value_or_an_object() {
        compare(ResumeFocus.activeWorkspaceOf(_monitors("4")), "4");
        compare(ResumeFocus.activeWorkspaceOf([
            {
                activeWorkspace: {
                    id: 4,
                    name: "4"
                }
            }
        ]), "4");
        compare(ResumeFocus.activeWorkspaceOf([
            {
                activeWorkspace: {
                    id: 4
                }
            }
        ]), "4");
    }

    function test_active_workspace_of_empty_is_blank_not_a_throw() {
        compare(ResumeFocus.activeWorkspaceOf([]), "");
        compare(ResumeFocus.activeWorkspaceOf(null), "");
        compare(ResumeFocus.activeWorkspaceOf([
            {}
        ]), "");
    }

    // --- generations: interleaved resumes -----------------------------------

    function test_stamp_and_staleness() {
        var t = ResumeFocus.resolveTarget("0xgame", "", testCase.fleet);
        ResumeFocus.stamp(t, 7);
        compare(t.generation, 7);
        verify(!ResumeFocus.isStale(t, 7));
        verify(ResumeFocus.isStale(t, 8));
    }

    // An unstamped decision counts as current, so a caller that never opted into
    // generations keeps working.
    function test_unstamped_decision_is_never_stale() {
        verify(!ResumeFocus.isStale(ResumeFocus.resolveTarget("0xgame", "", testCase.fleet), 3));
        verify(!ResumeFocus.isStale(null, 3));
        verify(!ResumeFocus.isStale({}, 3));
    }

    // The exact interleaving observed on-device: resume Steam, then resume Plex
    // before Steam's verification lands. Steam's stale reply must NOT conclude —
    // it would judge itself against Plex's workspace, call a correct resume a
    // miss, and bounce the user out of the app they just switched to.
    function test_a_superseded_resume_cannot_conclude() {
        var gen = 1;
        var steam = ResumeFocus.resolveTarget("0xsteam", "", testCase.fleet);
        ResumeFocus.stamp(steam, gen);

        gen += 1;
        var plex = ResumeFocus.resolveTarget("0xplex", "", testCase.fleet);
        ResumeFocus.stamp(plex, gen);

        // Plex landed; the compositor now displays workspace 2.
        var displayed = _monitors("2");

        verify(ResumeFocus.isStale(steam, gen), "Steam's chain must be suppressed");
        // ...and had it NOT been suppressed, it would have reported a false miss.
        verify(ResumeFocus.isRealMiss(ResumeFocus.verifyLanding(steam, displayed)), "which is exactly why the stale guard exists");

        verify(!ResumeFocus.isStale(plex, gen));
        verify(ResumeFocus.verifyLanding(plex, displayed).landed);
    }
}
