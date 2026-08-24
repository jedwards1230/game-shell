import QtQuick
import QtQuick.Layouts
import "../lib"
import "../../components"

// Home-screen Apps widget (#249) — the evolution of the old Recent widget. A
// SegmentedHeader flips between two segments fed into ONE horizontal rail:
//   "recent" → the merged running+recents model (HomeScreen-owned, passed in via
//              `model`), exactly what the Recent widget rendered.
//   "all"    → every installed app, read straight off the AppDiscoveryManager
//              singleton (the same source the Library surface uses).
// Both segments render in the SAME single-scroll horizontal rail (the "All Apps"
// pill stays bound to the rail, it does not expand inline) — the full vertical
// browse GRID lives in the fullscreen Library surface, one step away behind the
// "Open Library" action chip (and the standalone All Apps entry below the widget).
//
// The widget id stays "recent" (config namespace + registry key) so this is NOT a
// settings migration — only the DISPLAY name became "Apps". Both segments emit the
// same outward signals the Recent widget had (entryActivated / entryContextRequested
// / ensureVisibleRequested / escaped) so HomeScreen's launch/focus/PopoverMenu
// wiring is unchanged; the new openLibraryRequested is the one addition.
//
// Extends Widget (the home-screen base): a FocusScope hosting the header + grid,
// satisfying the duck-typed focus contract by delegating to them.
Widget {
    id: root

    // Merged recent/running model (HomeScreen-owned), and the small-size reflow
    // flag (small = icon-only square tiles; medium = full icon + label cards).
    property var model: []
    property bool recentSmall: false

    // Bubbled up so HomeScreen keeps the launch/focus + PopoverMenu logic. An
    // all-apps entry is just {name, exec, icon, comment, running:false}, which
    // HomeScreen's _recentActivate/_recentContext already treat as a launch.
    signal entryActivated(var entry)
    signal entryContextRequested(var entry, var card)
    // ensureVisibleRequested is inherited from the Widget base (auto-wired to the
    // host by WidgetHost); emitted below from the header / rail focus.
    // Trailing "Open Library" action chip → HomeScreen opens the Library surface.
    signal openLibraryRequested

    // === Segments ===
    // The segment the USER last committed (or the default). It is never coerced —
    // it records intent, so a segment that empties and refills comes back.
    property string _requestedSegment: "recent"

    // All installed apps, alphabetised, shaped exactly like AppCard expects with an
    // explicit running:false (so the shared _recentActivate path launches them).
    readonly property var _allApps: {
        var apps = (AppDiscoveryManager.applications || []).slice();
        apps.sort(function (a, b) {
            var an = (a.name || "").toLowerCase();
            var bn = (b.name || "").toLowerCase();
            return an < bn ? -1 : (an > bn ? 1 : 0);
        });
        var out = [];
        for (var i = 0; i < apps.length; i++) {
            var a = apps[i];
            out.push({
                "name": a.name || "",
                "exec": a.exec || "",
                "icon": a.icon || "",
                "comment": a.comment || "",
                "wmClass": a.wmClass || "",
                "running": false
            });
        }
        return out;
    }

    readonly property bool _hasRecent: root.model.length > 0
    readonly property bool _hasAll: root._allApps.length > 0

    // Present segments: Recent only when it has content; All Apps only when apps
    // exist (essentially always). Mirrors Plex/Steam's dynamic segment list.
    readonly property var _segmentOptions: {
        var o = [];
        if (_hasRecent)
            o.push({
                "label": "Recent",
                "value": "recent"
            });
        if (_hasAll)
            o.push({
                "label": "All Apps",
                "value": "all"
            });
        return o;
    }

    // The segment actually RENDERED: the request, coerced onto a segment that has
    // content. This is derived, not assigned.
    //
    // It used to be a writable property that an imperative `_coerceSegment()`
    // updated from `onModelChanged` / `onApplicationsChanged`. That is what
    // produced `Binding loop detected for property "_activeModel"` on device: a
    // signal handler on one of `_activeModel`'s dependencies (`model`) wrote
    // another of its dependencies (`_segment`), re-entering `_activeModel`'s
    // update while it was still in flight. Qt then abandons the update and leaves
    // the property STALE — the rail could keep rendering the wrong list.
    // (Reproduced headlessly against the pre-fix file: apps discovered before the
    // recent model settles — the normal startup order — is enough. See
    // tests/qml/tst_appssegment.qml.)
    //
    // Expressing the coercion as a binding removes the writer entirely, so nothing
    // mutates `_activeModel`'s dependency set from inside its own cascade. The
    // behaviour is also strictly better: previously a segment that emptied flipped
    // away permanently, now it is restored when its content returns.
    readonly property string _segment: {
        if (root._requestedSegment === "all")
            return root._hasAll ? "all" : (root._hasRecent ? "recent" : "all");
        return root._hasRecent ? "recent" : (root._hasAll ? "all" : "recent");
    }

    // Auto-parking LATCHES: when the derivation has to move off the request
    // because the requested segment is empty, adopt the parked segment as the new
    // request. Without this the rail yanks itself out from under the user — cold
    // boot with Recent empty parks on All Apps, the user starts browsing, then the
    // first launch fills Recent and (since the request was still the default
    // "recent") the content and chip highlight jump mid-browse. The old imperative
    // coercion never did that, because it only ever moved when the CURRENT segment
    // emptied.
    //
    // Deferred via Qt.callLater on purpose. `_requestedSegment` is a dependency of
    // `_segment`, so writing it directly from `_segment`'s own change handler is
    // precisely the synchronous re-entrancy this whole change removed — it would
    // just relocate the loop from `_activeModel` to `_segment`. Running a turn
    // later puts the write outside every binding cascade, and it is a no-op
    // whenever the two already agree. (tests/qml/run.sh fails the run on any
    // "Binding loop detected" line, so a regression here breaks the suite rather
    // than printing a warning nobody reads.)
    function _latchParkedSegment() {
        if (root._segment !== root._requestedSegment)
            root._requestedSegment = root._segment;
    }
    on_SegmentChanged: Qt.callLater(root._latchParkedSegment)
    Component.onCompleted: Qt.callLater(root._latchParkedSegment)

    readonly property var _activeModel: root._segment === "all" ? root._allApps : root.model

    // Trailing "Open Library" ACTION chip sentinel (ignored by the segment handler).
    readonly property string _openValue: "__open_library__"

    // Current rail selection. Nothing outside the tests reads it any more — the
    // hint bar was its only consumer and now uses currentEntryRunning below — but
    // it is kept as part of the widget's outward surface for a host that wants the
    // cursor position.
    readonly property int currentIndex: appsRow.currentIndex

    // Is the rail's CURRENT entry a running app? Exposed so the host's hint bar
    // can label A as "Resume" vs "Launch" from a value DOWNSTREAM of the rail.
    //
    // This exists to break a real binding loop (`Binding loop detected for
    // property "_activeModel"`, ~8×/minute on device). HomeScreen's hint bar used
    // to answer the same question by reading `recentWidget.currentIndex` AND
    // `HomeScreen._recentModel`, which closed this cycle:
    //
    //   _activeModel  -> appsRow.model (ListView model reset)
    //                 -> listView.currentIndex
    //                 -> AppsWidget.currentIndex
    //                 -> HintBar.text  (pulls the now-dirty _recentModel)
    //                 -> HomeScreen._recentModel
    //                 -> Binding{ property: "model" } writes AppsWidget.model
    //                 -> onModelChanged -> _coerceSegment() writes _segment
    //                 -> _activeModel   (re-entered mid-update -> loop warning)
    //
    // The load-bearing hop is the hint bar reaching back UPSTREAM for the recent
    // model; answering from a downstream value removes that edge entirely. It also
    // fixes a latent correctness bug: the old lookup indexed `_recentModel` even
    // while the "All Apps" segment was showing, so the hint could report the wrong
    // entry's running state.
    //
    // Note this reads the RAIL's settled model (`appsRow.model`), NOT
    // `_activeModel`. `currentIndex` is itself downstream of `_activeModel` — a
    // model reset moves the ListView cursor — so a getter reading both would
    // evaluate `_activeModel` from inside its own cascade and re-enter it: the
    // same loop, just relocated. `appsRow.model` is already written by the time
    // the cursor moves, which keeps every dependency on one side of the flow.
    readonly property bool currentEntryRunning: {
        let m = appsRow.model;
        let i = appsRow.currentIndex;
        return !!m && i >= 0 && i < m.length && m[i].running === true;
    }

    // Apps essentially always exist, so this widget basically always shows — that's
    // intended (it is the home screen's app launcher).
    wantVisible: root.widgetEnabled && (root._hasRecent || root._hasAll)

    implicitWidth: col.implicitWidth
    implicitHeight: root.wantVisible ? col.implicitHeight : 0

    // === Home-tile focus contract ===
    firstRow: segmentHeader
    lastRow: appsRow
    canFocus: visible && (root._hasRecent || root._hasAll)

    function focusFirstChild() {
        if (!root.canFocus)
            return false;
        // Prefer the rail when the active segment has content; otherwise focus the
        // header (e.g. the active segment is empty but the other still has apps, so
        // the user can flip segments). Mirrors PlexWidget's firstRow-or-fallback.
        if (appsRow.canFocus)
            return appsRow.focusFirstChild();
        if (segmentHeader.visible)
            return segmentHeader.focusFirstChild();
        return false;
    }

    ColumnLayout {
        id: col
        width: root.width
        spacing: Units.spacingMD

        // === Header: Recent / All Apps segments + "Open Library" action chip ===
        SegmentedHeader {
            id: segmentHeader
            Layout.fillWidth: true
            visible: root._hasRecent || root._hasAll
            segments: root._segmentOptions
            currentValue: root._segment
            actions: [
                {
                    "label": "Open Library",
                    "value": root._openValue
                }
            ]
            previousRow: root.previousRow
            nextRow: appsRow
            // A chip commit records the user's INTENT; `_segment` derives from it.
            onSegmentChanged: value => root._requestedSegment = value
            onActionTriggered: value => root.openLibraryRequested()
            onEscaped: root.escaped()
            onEnsureVisibleRequested: item => root.ensureVisibleRequested(item)
        }

        // === The one horizontal rail (shows the active segment) ===
        // Both the Recent and All Apps segments render here, in this single
        // horizontal single-scroll rail — exactly like the old Recent widget's row.
        // The vertical browse grid of every app lives in the Library surface, not
        // here; this stays a glance rail.
        NavigableRow {
            id: appsRow
            visible: root._activeModel.length > 0
            Layout.fillWidth: true
            // Extra breathing room between the chip strip and the rail (on top of
            // the ColumnLayout spacing) so the pills don't crowd the row below.
            Layout.topMargin: Units.spacingMD
            Layout.preferredHeight: Theme.rowHeight
            keyNavigationWraps: true
            previousRow: segmentHeader
            nextRow: root.nextRow
            model: root._activeModel
            onActiveFocusChanged: if (activeFocus)
                root.ensureVisibleRequested(appsRow)

            delegate: AppCard {
                required property int index
                required property var modelData
                iconOnly: root.recentSmall
                width: root.recentSmall ? Theme.cardHeight : Theme.cardWidth
                height: Theme.cardHeight
                app: modelData
                running: modelData.running === true
                focus: index === appsRow.currentIndex
                onActivated: {
                    // Sync the cursor to a clicked card (mouse mode) so a later
                    // controller move resumes from here, then bubble the launch up.
                    appsRow.currentIndex = index;
                    root.entryActivated(modelData);
                }
            }

            onContextRequested: {
                if (currentItem && currentIndex >= 0 && currentIndex < root._activeModel.length)
                    root.entryContextRequested(root._activeModel[currentIndex], currentItem);
            }
            onEscaped: root.escaped()
        }
    }
}
