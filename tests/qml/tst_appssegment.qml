import QtQuick
import QtTest
import components
import components.lib
import widgets.apps

// Regression pins for the two edits that removed `Binding loop detected for
// property "_activeModel"` (~8×/minute on the device) from the real AppsWidget.
//
// CYCLE 1 — widget-internal, and the one these tests reproduce directly.
// `_segment` was a writable property that an imperative `_coerceSegment()` wrote
// from `onModelChanged` / `onApplicationsChanged`. Both `model` and `_segment` are
// dependencies of `_activeModel`, so a handler on one dependency wrote another,
// re-entering `_activeModel` mid-update. Qt abandons a re-entered update, so the
// property was left STALE and the rail rendered the wrong segment. Discovering
// apps BEFORE the recent model settles — the normal startup order — is enough to
// trigger it (test_startup_order_apps_first below fires the warning against the
// pre-fix file). `_segment` is now DERIVED from `_requestedSegment` + the two
// content flags, so nothing writes it.
//
// CYCLE 2 — the AppsWidget <-> HomeScreen hint-bar hop:
//
//   _activeModel -> appsRow.model (ListView model reset)
//                -> listView.currentIndex -> AppsWidget.currentIndex
//                -> HomeScreen HintBar.text, which ALSO read HomeScreen._recentModel
//                -> Binding{ property: "model" } writes AppsWidget.model
//                -> onModelChanged -> _coerceSegment() writes _segment
//                -> _activeModel, re-entered mid-update
//
// The hint bar now asks the widget for `currentEntryRunning` — derived from the
// rail's own settled model, i.e. strictly downstream — so no edge runs back into
// the model that feeds it. That also fixes a latent bug: the old lookup indexed
// the RECENT model even while the "All Apps" segment was showing.
//
// NOTE ON WHAT ACTUALLY GATES THIS: the settled-value assertions below describe
// the intended behaviour, but they are NOT the loop detector — Qt abandons a
// re-entered binding update and leaves a stale value that often still looks
// right. The real gate is in `tests/qml/run.sh`, which fails the whole run if any
// "Binding loop detected" line appears in the output. QML TestCase has no
// `failOnWarning` (that is a C++-only QTest API), and a per-file property would
// only cover one file anyway.
TestCase {
    id: testCase
    name: "AppsSegment"
    when: windowShown
    visible: true
    width: 900
    height: 500

    Item {
        id: holder
        anchors.fill: parent
    }

    Component {
        id: appsComp
        AppsWidget {}
    }

    function _app(name) {
        return {
            "name": name,
            "exec": name.toLowerCase(),
            "icon": "",
            "comment": "",
            "wmClass": ""
        };
    }

    function _entry(name, running) {
        return {
            "name": name,
            "exec": name.toLowerCase(),
            "icon": "",
            "comment": "",
            "wmClass": "",
            "running": running
        };
    }

    function init() {
        AppDiscoveryManager.applications = [];
    }

    function cleanup() {
        AppDiscoveryManager.applications = [];
    }

    function test_running_entry_reports_running() {
        var w = appsComp.createObject(holder, {
            "widgetEnabled": true
        });
        w.model = [_entry("Steam", true), _entry("Plex", false)];
        wait(30);
        verify(w.focusFirstChild(), "the rail takes focus with content");
        compare(w.currentIndex, 0);
        verify(w.currentEntryRunning, "index 0 is the running window");
        w.destroy();
    }

    function test_non_running_entry_reports_not_running() {
        var w = appsComp.createObject(holder, {
            "widgetEnabled": true
        });
        w.model = [_entry("Plex", false)];
        wait(30);
        verify(w.focusFirstChild());
        verify(!w.currentEntryRunning);
        w.destroy();
    }

    // The old hint looked the index up in the RECENT model regardless of segment,
    // so in "All Apps" it could report a stale running state. All-apps entries are
    // always running:false, so the answer must be false there.
    function test_all_apps_segment_reads_the_active_model() {
        AppDiscoveryManager.applications = [
            {
                "name": "Firefox",
                "exec": "firefox",
                "icon": "",
                "comment": "",
                "wmClass": ""
            }
        ];
        var w = appsComp.createObject(holder, {
            "widgetEnabled": true
        });
        // A running recent entry at index 0 — exactly the state that made the old
        // lookup answer "Resume" while the All Apps segment was showing.
        w.model = [_entry("Steam", true)];
        wait(30);
        verify(w.focusFirstChild());
        verify(w.currentEntryRunning, "recent segment: index 0 is running");

        w._requestedSegment = "all";
        wait(30);
        compare(w._segment, "all");
        compare(w._activeModel.length, 1);
        verify(!w.currentEntryRunning, "all-apps segment entries are never running");
        w.destroy();
    }

    // An empty rail has no current entry; the getter must answer false rather
    // than index into nothing (it runs in a binding on the host's hint bar).
    // === Segment derivation (cycle 1) ==================================

    // The startup order that produced the loop: `list-apps` resolves before the
    // running/recents model does, so the widget is constructed with an empty
    // `model` and a populated AppDiscoveryManager, then `model` arrives.
    //
    // Pre-fix this logged `Binding loop detected for property "_activeModel"` and
    // left `_segment` STALE (Qt abandons a re-entered update). The run.sh grep is
    // what actually catches a regression of that class (QML TestCase has no
    // `failOnWarning` — see the header note); the value assertions below describe
    // the intended behaviour.
    function test_startup_order_apps_first() {
        AppDiscoveryManager.applications = [_app("Firefox")];
        var w = appsComp.createObject(holder, {
            "widgetEnabled": true
        });
        wait(30);
        // Recent is empty, so the derivation parks on All Apps and LATCHES it as
        // the request (see _latchParkedSegment).
        compare(w._segment, "all", "an empty Recent parks on All Apps");
        compare(w._requestedSegment, "all", "the park is latched as the new request");

        // Recent now fills — the user is mid-browse in All Apps, so the rail must
        // NOT yank itself back. This is the behaviour the latch exists for.
        w.model = [_entry("Steam", true)];
        wait(30);
        verify(w._hasRecent);
        verify(w._hasAll);
        compare(w._segment, "all", "a newly-filled Recent must not yank the rail");
        compare(w._activeModel.length, 1);
        compare(w._activeModel[0].name, "Firefox");
        w.destroy();
    }

    // Coercion still applies: a request whose segment is empty falls through to
    // the one that has content (and then latches, per the test above).
    function test_empty_requested_segment_coerces() {
        AppDiscoveryManager.applications = [_app("Firefox")];
        var w = appsComp.createObject(holder, {
            "widgetEnabled": true
        });
        w.model = [];
        wait(30);
        compare(w._segment, "all", "an empty Recent falls through to All Apps");
        w.destroy();
    }

    // An EXPLICIT user pick is honoured and is not re-parked while it has content.
    function test_explicit_pick_is_honoured() {
        AppDiscoveryManager.applications = [_app("Firefox")];
        var w = appsComp.createObject(holder, {
            "widgetEnabled": true
        });
        w.model = [_entry("Steam", true)];
        wait(30);
        w._requestedSegment = "recent";
        wait(30);
        compare(w._segment, "recent", "the user's pick has content, so it stands");
        compare(w._requestedSegment, "recent", "and the latch leaves it alone");
        w.destroy();
    }

    // The user's pick empties out: park on the other segment and latch, so a
    // later refill does not yank the rail back mid-browse.
    function test_park_latches_when_the_picked_segment_empties() {
        AppDiscoveryManager.applications = [_app("Firefox")];
        var w = appsComp.createObject(holder, {
            "widgetEnabled": true
        });
        w.model = [_entry("Steam", true)];
        wait(30);
        w._requestedSegment = "recent";
        wait(30);
        compare(w._segment, "recent");

        w.model = [];
        wait(30);
        compare(w._segment, "all", "an emptied Recent parks on All Apps");
        compare(w._requestedSegment, "all", "and the park is latched");

        w.model = [_entry("Steam", true)];
        wait(30);
        compare(w._segment, "all", "the refill does not yank the rail back");
        w.destroy();
    }

    function test_empty_model_is_not_running() {
        var w = appsComp.createObject(holder, {
            "widgetEnabled": true
        });
        w.model = [];
        wait(30);
        verify(!w.currentEntryRunning);
        w.destroy();
    }
}
