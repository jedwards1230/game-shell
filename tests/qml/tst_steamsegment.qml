import QtQuick
import QtTest
import components
import components.lib
import widgets.steamlib

// Pins for the two handler-writes-a-derived-input defects in the REAL
// SteamLibraryView — the same class jedwards1230/tv-shell#441 removed from
// AppsWidget, and the rule docs/OBSERVABILITY.md states as: never write a
// dependency of a derived property from a handler on another of its dependencies.
//
// (a) `_segment` was writable and coerced inline at the end of
//     `steamMon.onUpdated`, in the same handler that adopts `recentItems` /
//     `allItems` via `_adoptRails` — and all three feed `_activeItems`. It is now
//     DERIVED from `_requestedSegment` + the two content flags, with the same
//     Qt.callLater park-latch AppsWidget uses.
//
// (b) `_endFastPoll()` was called inline from that same handler. `_fastPolling`
//     feeds exactly one binding, `steamMon.dataIntervalMs`, which IS this
//     monitor's poll `Timer.interval` — so the handler mutated the interval of
//     the very timer whose reply it was handling. It is now deferred with
//     Qt.callLater.
//
// HONEST SCOPE NOTE, both halves:
//  - Driving the pre-fix file through the REAL `steamMon.onUpdated` did NOT emit a
//    `Binding loop detected` line. Unlike AppsWidget nothing downstream of the
//    poster row reaches back up into the rails, and `onUpdated` is emitted from a
//    socket reply rather than from inside `_activeItems`' own cascade. This site
//    was a LATENT stale-value hazard, not an observed loop. What it demonstrably
//    got wrong is REACHABILITY: the coercion ran only when the poll handler fired,
//    so any other write of the rails left the chips and the row disagreeing — and
//    it could not be tested at all, since the stub SocketClient never polls.
//    Verified: against a reconstructed pre-fix file the four (a) tests that write
//    the rails directly all fail, while test_real_poll_handler_drives_the_deriva-
//    tion drives the old in-handler coercion and passes either way (an end-to-end
//    pin, not a regression witness).
//  - For (b), the predicted "the interval change re-fires `triggeredOnStart` and
//    issues a spurious extra poll from inside the reply handler" could NOT be
//    reproduced on Qt 6.11: QQmlTimer only honours `triggeredOnStart` on a
//    running-edge (its `firstTick` flag), so changing `interval` on an
//    already-running timer restarts the countdown but does not re-trigger.
//    test_no_poll_is_issued_from_inside_the_reply_handler therefore passes both
//    before and after the fix — it is a GUARD on that invariant (via the
//    SocketProbe request tally), not a reproduced regression. The deferral itself
//    IS directly observable and is pinned by
//    test_fast_poll_end_is_deferred_out_of_the_reply_handler, which does fail
//    pre-fix.
//
// The run-wide binding-loop gate lives in tests/qml/run.sh (it greps the whole run
// log). QML TestCase has no `failOnWarning` — that is a C++-only QTest API.
TestCase {
    id: testCase
    name: "SteamSegment"
    when: windowShown
    visible: true
    width: 900
    height: 600

    Item {
        id: holder
        anchors.fill: parent
    }

    Component {
        id: viewComp
        SteamLibraryView {}
    }

    function _game(appid, name) {
        return {
            "appid": appid,
            "name": name,
            "art": "",
            "localArt": "",
            "headerArt": ""
        };
    }

    function _mk() {
        var v = viewComp.createObject(holder, {});
        verify(v, "the REAL SteamLibraryView instantiates headless");
        return v;
    }

    // ServiceMonitor's `steamMon` is a private child id, so reach it by
    // duck-typing the view's children. Used by the tests that drive the
    // production `onUpdated` handler verbatim.
    function _mon(v) {
        for (var i = 0; i < v.children.length; i++) {
            var c = v.children[i];
            if (c && c.healthKey !== undefined && c.healthKey === "steam")
                return c;
        }
        return null;
    }

    function _okReply(recent, all) {
        return {
            "status": "ok",
            "recentlyPlayed": recent,
            "allGames": all
        };
    }

    // 1. Default segment when both sides have content: Recently Played wins.
    function test_default_segment_is_recent_when_both_have_content() {
        var v = _mk();
        v.recentItems = [_game(1, "Alpha")];
        v.allItems = [_game(1, "Alpha"), _game(2, "Beta")];
        wait(30);
        compare(v._segment, "recent", "Recently Played is the default segment");
        compare(v._requestedSegment, "recent", "nothing parked, so nothing latched");
        compare(v._activeItems.length, 1);
        v.destroy();
    }

    // 2. Startup ordering: the NON-default side lands first. Mirrors
    //    tst_appssegment's test_startup_order_apps_first.
    function test_startup_order_library_first() {
        var v = _mk();
        v.allItems = [_game(2, "Beta")];
        wait(30);
        compare(v._segment, "all", "an empty Recently Played parks on Library");
        compare(v._requestedSegment, "all", "the park is latched as the new request");

        v.recentItems = [_game(1, "Alpha")];
        wait(30);
        verify(v._hasRecent);
        verify(v._hasAll);
        compare(v._segment, "all", "a newly-filled Recently Played must not yank the row");
        compare(v._activeItems[0].name, "Beta");
        v.destroy();
    }

    // 3. THE REGRESSION: the currently-shown segment empties, so the row flips
    //    away — and the flip must not be permanent. See the equivalent note in
    //    tst_plexsegment.qml: the LATCH deliberately makes a refill non-yanking,
    //    so "comes back" means the derivation keeps tracking content rather than
    //    freezing at whatever the last poll handler happened to leave behind.
    function test_shown_segment_empties_then_refills() {
        var v = _mk();
        v.recentItems = [_game(1, "Alpha")];
        v.allItems = [_game(1, "Alpha"), _game(2, "Beta")];
        wait(30);
        compare(v._segment, "recent");

        // Recently Played drains -> park on Library + latch.
        v.recentItems = [];
        wait(30);
        compare(v._segment, "all", "an emptied Recently Played parks on Library");
        compare(v._requestedSegment, "all", "and the park is latched");

        // It refills: the row stays put (no mid-browse yank)...
        v.recentItems = [_game(1, "Alpha")];
        wait(30);
        compare(v._segment, "all", "the refill does not yank the row back");

        // ...but the derivation is live, so an emptied Library returns the row to
        // the refilled Recently Played instead of wedging on a stale value.
        v.allItems = [];
        wait(30);
        compare(v._segment, "recent", "the refilled segment comes back");
        compare(v._requestedSegment, "recent", "and re-latches");
        compare(v._activeItems[0].name, "Alpha");
        v.destroy();
    }

    // 4. A user chip commit is honoured and survives a data refresh.
    function test_user_commit_survives_a_data_refresh() {
        var v = _mk();
        v.recentItems = [_game(1, "Alpha")];
        v.allItems = [_game(1, "Alpha"), _game(2, "Beta")];
        wait(30);

        // What SegmentedHeader.onSegmentChanged now writes: the INTENT.
        v._requestedSegment = "all";
        wait(30);
        compare(v._segment, "all", "the user's pick has content, so it stands");

        // A refresh through the PRODUCTION adoption path, with new content.
        v._adoptRails(_okReply([_game(1, "Alpha")], [_game(1, "Alpha"), _game(2, "Beta"), _game(3, "Gamma")]));
        wait(30);
        compare(v._segment, "all", "a refresh must not undo the user's pick");
        compare(v._requestedSegment, "all");
        compare(v._activeItems.length, 3);
        v.destroy();
    }

    // End-to-end through the PRODUCTION `steamMon.onUpdated` handler — the path
    // that used to carry the coercion. It must now write DATA ONLY.
    function test_real_poll_handler_drives_the_derivation() {
        var v = _mk();
        var m = _mon(v);
        verify(m, "the view's steam ServiceMonitor is reachable");

        m.status = "ok";
        m.data = _okReply([], [_game(2, "Beta")]);
        m.updated();
        wait(30);
        compare(v._segment, "all", "an ok reply with no recents parks on Library");
        compare(v._activeItems.length, 1);

        // The `disabled` branch clears both rails; with nothing to show the
        // derivation falls back to the request rather than wedging.
        m.data = {
            "status": "disabled"
        };
        m.updated();
        wait(30);
        compare(v._activeItems.length, 0);
        compare(v._segment, "all", "no content -> the latched request stands");
        v.destroy();
    }

    // 5a. The fast-poll defect, the half that IS observable: `_fastPolling` feeds
    //     steamMon.dataIntervalMs (the poll Timer's interval), so the reply
    //     handler must not clear it inline. Pre-fix `_fastPolling` was already
    //     false — and the interval already back to 10s — the instant `updated()`
    //     returned; now the write lands a turn later, outside the reply cascade.
    function test_fast_poll_end_is_deferred_out_of_the_reply_handler() {
        var v = _mk();
        var m = _mon(v);
        verify(m, "the view's steam ServiceMonitor is reachable");

        v._fastPolling = true;
        compare(m.dataIntervalMs, v._fastPollMs, "fast polling drives the 3s cadence");

        m.status = "ok";
        m.data = _okReply([_game(1, "Alpha")], [_game(1, "Alpha")]);
        m.updated();
        // Synchronously after the handler: untouched.
        verify(v._fastPolling, "the reply handler must not clear _fastPolling inline");
        compare(m.dataIntervalMs, v._fastPollMs, "nor mutate the running poll timer's interval");

        // A turn later the deferred _endFastPoll() has run.
        wait(30);
        verify(!v._fastPolling, "the deferred _endFastPoll clears it");
        compare(m.dataIntervalMs, v._normalPollMs, "and the cadence reverts to 10s");
        v.destroy();
    }

    // 5b. No poll may be issued from inside the reply handler. Observable via the
    //     SocketProbe tally the stub SocketClient feeds (ServiceMonitor's dataReq
    //     is a private child, so a per-instance counter is not reachable).
    //
    //     READ THE HONEST SCOPE NOTE at the top: this passes pre-fix too, because
    //     QQmlTimer only honours `triggeredOnStart` on a running-edge, so the
    //     inline interval change never actually re-triggered. It is a guard on the
    //     invariant, not a reproduced regression.
    function test_no_poll_is_issued_from_inside_the_reply_handler() {
        var v = _mk();
        var m = _mon(v);
        verify(m, "the view's steam ServiceMonitor is reachable");

        v._fastPolling = true;
        wait(30);
        SocketProbe.reset();

        m.status = "ok";
        m.data = _okReply([_game(1, "Alpha")], [_game(1, "Alpha")]);
        m.updated();
        compare(SocketProbe.countOf("steam-library"), 0, "the reply handler must not issue a poll");
        v.destroy();
    }

    // The probe itself must actually see requests, or 5b would be vacuous.
    function test_socket_probe_observes_a_real_request() {
        var v = _mk();
        SocketProbe.reset();
        v.refresh();
        compare(SocketProbe.countOf("steam-library"), 1, "refresh() issues exactly one steam-library poll");
        v.destroy();
    }
}
