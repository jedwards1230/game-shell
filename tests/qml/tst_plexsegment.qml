import QtQuick
import QtTest
import components
import components.lib
import widgets.plex

// Segment-derivation pins for the REAL PlexWidget — the second live instance of
// the anti-pattern jedwards1230/tv-shell#441 removed from AppsWidget, and the one
// docs/OBSERVABILITY.md states as: never write a dependency of a derived property
// from a handler on another of its dependencies.
//
// PRE-FIX SHAPE. `_segment` was a writable property coerced inline at the end of
// `plexMon.onUpdated`, in the same handler that had just written `onDeckItems` and
// `recentItems` — and both of those, plus `_segment`, are dependencies of
// `_activeItems`. In AppsWidget that shape produced `Binding loop detected for
// property "_activeModel"` on device; Qt abandons a re-entered update and leaves
// the property STALE, so the row keeps rendering the wrong list.
//
// HONEST SCOPE NOTE: driving the pre-fix file through the REAL `plexMon.onUpdated`
// (data set on the monitor, `updated()` emitted) did NOT reproduce a
// `Binding loop detected` line — unlike AppsWidget, nothing downstream of the
// poster row reaches back up into the lists, and `onUpdated` is emitted from a
// socket reply rather than from inside one of `_activeItems`' own binding
// cascades. So this site was a LATENT stale-value hazard, not an observed loop.
// What the pre-fix code demonstrably did get wrong is REACHABILITY: the coercion
// only ran when the poll handler happened to fire, so any other write of the two
// lists left the chip highlight and the rendered row disagreeing — and it could
// not be tested at all, since the stub SocketClient never drives a poll. Verified:
// against a reconstructed pre-fix file 4 of the 5 tests below fail (the fifth,
// test_real_poll_handler_drives_the_derivation, drives the old in-handler coercion
// and so passes either way — it is an end-to-end pin, not a regression witness).
//
// The run-wide binding-loop gate is in tests/qml/run.sh (it greps the whole run
// log for "Binding loop detected"). QML TestCase has no `failOnWarning` — that is
// a C++-only QTest API.
TestCase {
    id: testCase
    name: "PlexSegment"
    when: windowShown
    visible: true
    width: 900
    height: 600

    Item {
        id: holder
        anchors.fill: parent
    }

    Component {
        id: plexComp
        PlexWidget {}
    }

    function _item(title) {
        return {
            "title": title,
            "subtitle": "",
            "art": "",
            "progress": 0
        };
    }

    function _mk() {
        var w = plexComp.createObject(holder, {
            "widgetEnabled": true
        });
        verify(w, "the REAL PlexWidget instantiates headless");
        return w;
    }

    // ServiceMonitor's `plexMon` is a private child id, so reach it by duck-typing
    // the widget's children. Used only by the end-to-end test below, which drives
    // the production `onUpdated` handler verbatim.
    function _mon(w) {
        for (var i = 0; i < w.children.length; i++) {
            var c = w.children[i];
            if (c && c.healthKey !== undefined && c.healthKey === "plex")
                return c;
        }
        return null;
    }

    // 1. Default segment when both sides have content: Up Next wins.
    function test_default_segment_is_ondeck_when_both_have_content() {
        var w = _mk();
        w.onDeckItems = [_item("Up Next A")];
        w.recentItems = [_item("New B")];
        wait(30);
        compare(w._segment, "ondeck", "Up Next is the default segment");
        compare(w._requestedSegment, "ondeck", "nothing parked, so nothing latched");
        compare(w._activeItems.length, 1);
        compare(w._activeItems[0].title, "Up Next A");
        w.destroy();
    }

    // 2. Startup ordering: the NON-default side lands first. Mirrors
    //    tst_appssegment's test_startup_order_apps_first — the widget shows the
    //    only segment that has content, latches that park, and a later arrival on
    //    the default side must NOT yank the row out from under the user.
    function test_startup_order_recently_added_first() {
        var w = _mk();
        w.recentItems = [_item("New B")];
        wait(30);
        compare(w._segment, "recent", "an empty Up Next parks on Recently Added");
        compare(w._requestedSegment, "recent", "the park is latched as the new request");

        w.onDeckItems = [_item("Up Next A")];
        wait(30);
        verify(w._hasOnDeck);
        verify(w._hasRecent);
        compare(w._segment, "recent", "a newly-filled Up Next must not yank the row");
        compare(w._activeItems[0].title, "New B");
        w.destroy();
    }

    // 3. THE REGRESSION: the currently-shown segment empties, so the row flips
    //    away — and the flip must not be permanent. Pre-fix `_segment` was frozen
    //    at its last in-handler value, so a row could keep rendering a list that
    //    no longer matched the chips.
    //
    //    Note the LATCH deliberately makes the refill non-yanking (same contract
    //    as tst_appssegment's test_park_latches_when_the_picked_segment_empties):
    //    "comes back" means the derivation keeps tracking content, so the moment
    //    the parked-to segment is the empty one the row returns — NOT that a
    //    refill snatches the row back mid-browse.
    function test_shown_segment_empties_then_refills() {
        var w = _mk();
        w.onDeckItems = [_item("Up Next A")];
        w.recentItems = [_item("New B")];
        wait(30);
        compare(w._segment, "ondeck");

        // Up Next drains (everything watched) -> park on Recently Added + latch.
        w.onDeckItems = [];
        wait(30);
        compare(w._segment, "recent", "an emptied Up Next parks on Recently Added");
        compare(w._requestedSegment, "recent", "and the park is latched");
        compare(w._activeItems[0].title, "New B");

        // Up Next refills. The latch means the row stays put (no mid-browse yank),
        // but the derivation is live, not frozen...
        w.onDeckItems = [_item("Up Next A")];
        wait(30);
        compare(w._segment, "recent", "the refill does not yank the row back");

        // ...so as soon as the parked-to segment is the empty one, the refilled
        // segment is shown again. Pre-fix this only ever happened if a poll
        // handler ran; now it is a property of the binding.
        w.recentItems = [];
        wait(30);
        compare(w._segment, "ondeck", "the refilled segment comes back");
        compare(w._requestedSegment, "ondeck", "and re-latches");
        compare(w._activeItems[0].title, "Up Next A");
        w.destroy();
    }

    // 4. A user chip commit is honoured and survives a data refresh that leaves
    //    the picked segment populated.
    function test_user_commit_survives_a_data_refresh() {
        var w = _mk();
        w.onDeckItems = [_item("Up Next A")];
        w.recentItems = [_item("New B")];
        wait(30);
        compare(w._segment, "ondeck");

        // What SegmentedHeader.onSegmentChanged now writes: the INTENT.
        w._requestedSegment = "recent";
        wait(30);
        compare(w._segment, "recent", "the user's pick has content, so it stands");

        // A refresh replaces both lists with new (still populated) arrays.
        w.onDeckItems = [_item("Up Next A"), _item("Up Next C")];
        w.recentItems = [_item("New B"), _item("New D")];
        wait(30);
        compare(w._segment, "recent", "a refresh must not undo the user's pick");
        compare(w._requestedSegment, "recent");
        compare(w._activeItems.length, 2);
        compare(w._activeItems[0].title, "New B");
        w.destroy();
    }

    // End-to-end through the PRODUCTION `plexMon.onUpdated` handler (the stub
    // SocketClient never polls, so the reply is injected on the monitor and
    // `updated()` emitted by hand). This is the path that used to carry the
    // coercion; it must now write DATA ONLY and let the derivation follow.
    function test_real_poll_handler_drives_the_derivation() {
        var w = _mk();
        var m = _mon(w);
        verify(m, "the widget's plex ServiceMonitor is reachable");

        m.status = "ok";
        m.data = {
            "onDeck": [],
            "recentlyAdded": [_item("New B")]
        };
        m.updated();
        wait(30);
        compare(w._segment, "recent", "an ok reply with no On Deck parks on Recently Added");
        compare(w._activeItems.length, 1);

        // A non-ok reply clears both lists; with nothing to show the derivation
        // falls back to the request rather than wedging on a stale value.
        m.status = "unreachable";
        m.data = null;
        m.updated();
        wait(30);
        compare(w._activeItems.length, 0);
        compare(w._segment, "recent", "no content -> the request stands");
        w.destroy();
    }
}
