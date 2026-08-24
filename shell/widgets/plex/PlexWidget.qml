import QtQuick
import QtQuick.Layouts
import "../lib"
import "../../components"
import "../../components/lib"

// Home-screen Plex widget (#249) — ONE poster row with a segmented header that
// flips between "Up Next" (continue-watching / On Deck) and "Recently Added"
// (new arrivals), fed by the daemon's `plex-hubs` IPC. Apple-TV-style: a single
// prominent row, not two stacked rows. The segment control appears only when
// BOTH segments have content (otherwise the lone segment's name is just a
// header). `size` reformats the row (not a scale):
//   small  = poster-only rail (caption band removed) — glanceable
//   medium = posters + title/subtitle captions + resume bars (default)
// (A "large" featured-backdrop hero is a planned follow-up — it needs 16:9
// backdrop art the daemon doesn't return yet.)
//
// Health-aware: a `ServiceMonitor` keyed on "plex" collapses the widget when
// unconfigured/empty and shows a graceful `ServiceStatusNotice` when the server
// is down.
//
// Extends Widget (the home-screen widget base): a FocusScope hosting the existing
// ColumnLayout. Focus contract (host uses these): `firstRow`/`lastRow` resolve to
// the first/last *visible* internal region (segment chips, poster row); the
// internal chain lets NavigableRow/FilterChips skip a hidden region.
Widget {
    id: root

    // The base defaults size to ""; Plex defaults to the captioned poster row.
    size: "medium"

    signal openPlexRequested
    // ensureVisibleRequested is inherited from the Widget base (auto-wired to the
    // host by WidgetHost); emitted below from the chips / poster-row focus.

    // === Data (populated from `plex-hubs`) ===
    property var onDeckItems: []
    property var recentItems: []

    readonly property bool _hasOnDeck: onDeckItems.length > 0
    readonly property bool _hasRecent: recentItems.length > 0

    // === Segment (Up Next vs Recently Added) ===
    // The segment the USER last committed (or the default). It is never coerced —
    // it records intent; `_segment` below derives the rendered segment from it.
    property string _requestedSegment: "ondeck"
    readonly property var _segmentOptions: {
        let o = [];
        if (_hasOnDeck)
            o.push({
                "label": "Up Next",
                "value": "ondeck"
            });
        if (_hasRecent)
            o.push({
                "label": "Recently Added",
                "value": "recent"
            });
        return o;
    }

    // The segment actually RENDERED: the request, coerced onto a segment that has
    // content. Derived, never assigned — same treatment as AppsWidget's `_segment`
    // (shell/widgets/apps/AppsWidget.qml), with the roles swapped ("ondeck" is the
    // default here).
    //
    // It used to be a writable property that `plexMon.onUpdated` coerced inline,
    // right after writing `onDeckItems`/`recentItems` in the same handler. That is
    // the anti-pattern recorded in docs/OBSERVABILITY.md: a handler writing one
    // dependency of a derived property (`_segment`) from a handler that also writes
    // another (`onDeckItems`/`recentItems`). In AppsWidget the equivalent shape
    // produced `Binding loop detected for property "_activeModel"` on device — Qt
    // abandons a re-entered update and leaves the property STALE, so the row can
    // keep rendering the wrong list.
    //
    // Two concrete wins beyond removing that hazard: the coercion now applies to
    // ANY change of the two lists (previously it only ran when the poll handler
    // happened to fire, so a list written from anywhere else left the chip
    // highlight and the rendered row disagreeing), and it is directly exercisable
    // headless — the stub SocketClient never drives a real poll, so the old
    // in-handler coercion was unreachable from a test at all
    // (tests/qml/tst_plexsegment.qml).
    readonly property string _segment: {
        if (root._requestedSegment === "recent")
            return root._hasRecent ? "recent" : (root._hasOnDeck ? "ondeck" : "recent");
        return root._hasOnDeck ? "ondeck" : (root._hasRecent ? "recent" : "ondeck");
    }

    // Auto-parking LATCHES: when the derivation has to move off the request
    // because the requested segment is empty, adopt the parked segment as the new
    // request, so a later refill does not yank the row out from under a user who
    // is mid-browse. Deferred via Qt.callLater because `_requestedSegment` is a
    // dependency of `_segment` — writing it straight from `_segment`'s own change
    // handler would reintroduce exactly the synchronous re-entrancy this change
    // removes. It is a no-op whenever the two already agree.
    function _latchParkedSegment() {
        if (root._segment !== root._requestedSegment)
            root._requestedSegment = root._segment;
    }
    on_SegmentChanged: Qt.callLater(root._latchParkedSegment)
    Component.onCompleted: Qt.callLater(root._latchParkedSegment)

    readonly property string _segmentName: _segment === "ondeck" ? "Up Next" : "Recently Added"
    readonly property var _activeItems: _segment === "ondeck" ? onDeckItems : recentItems

    // Trailing "Open Plex" ACTION chip sentinel — the SegmentedHeader renders it as
    // an ember action chip; the action handler launches the app directly. Ignored
    // by the segment handler (mirrors Steam's "Open Steam" / Apps' "Open Library").
    readonly property string _openValue: "__open_plex__"

    readonly property bool rowFocused: posterRow.activeFocus || segmentChips.activeFocus

    wantVisible: root.widgetEnabled && (plexMon.degraded || (plexMon.ok && (root._hasOnDeck || root._hasRecent)))

    implicitWidth: col.implicitWidth
    implicitHeight: root.wantVisible ? col.implicitHeight : 0

    // === Home-tile focus contract ===
    firstRow: segmentChips
    lastRow: posterRow
    canFocus: visible && (root._hasOnDeck || root._hasRecent)

    function focusFirstChild() {
        if (!root.canFocus)
            return false;
        let r = root.firstRow;
        if (r && r.visible) {
            if (r.focusFirstChild)
                r.focusFirstChild();
            else
                r.forceActiveFocus();
            return true;
        }
        return false;
    }

    // === Poster geometry (reflow by size) ===
    readonly property real _posterScale: root.size === "small" ? 0.50 : 0.62
    readonly property bool _showCaption: root.size !== "small"
    readonly property int posterW: Math.round(Theme.cardWidth * _posterScale)
    readonly property int posterH: Math.round(posterW * 1.5)
    readonly property int _captionBand: Math.round(Theme.fontSmall * 1.4 + Theme.fontCaption * 1.4 + Units.spacingSM * 2)
    readonly property int plexRowHeight: posterH + (_showCaption ? _captionBand : 0)

    function refresh() {
        plexMon.refresh();
    }

    ServiceMonitor {
        id: plexMon
        healthKey: "plex"
        dataCommand: "plex-hubs"
        dataIntervalMs: 30000  // matches daemon service_health POLL_INTERVAL
        onUpdated: {
            if (plexMon.ok && plexMon.data) {
                root.onDeckItems = plexMon.data.onDeck || [];
                root.recentItems = plexMon.data.recentlyAdded || [];
            } else {
                root.onDeckItems = [];
                root.recentItems = [];
            }
            // No segment coercion here on purpose — `_segment` derives it (above),
            // so this handler only writes DATA. Writing `_segment` from the same
            // handler that writes its sibling dependencies is the loop shape #441
            // removed from AppsWidget.
        }
    }

    ColumnLayout {
        id: col
        width: root.width
        spacing: Units.spacingMD

        ServiceStatusNotice {
            Layout.fillWidth: true
            serviceName: "Plex"
            status: plexMon.status
        }

        // === Header: segment chips + trailing "Open Plex" action chip ===
        SegmentedHeader {
            id: segmentChips
            Layout.fillWidth: true
            visible: root._hasOnDeck || root._hasRecent
            segments: root._segmentOptions
            currentValue: root._segment
            actions: [
                {
                    "label": "Open Plex",
                    "value": root._openValue
                }
            ]
            previousRow: root.previousRow
            nextRow: posterRow
            // A chip commit records the user's INTENT; `_segment` derives from it.
            onSegmentChanged: value => root._requestedSegment = value
            onActionTriggered: value => root.openPlexRequested()
            onEscaped: root.escaped()
            onEnsureVisibleRequested: item => root.ensureVisibleRequested(item)
        }

        // === The one poster row (shows the active segment) ===
        NavigableRow {
            id: posterRow
            visible: root._activeItems.length > 0
            Layout.fillWidth: true
            // Extra breathing room between the chip strip and the posters (on top of
            // the ColumnLayout spacing) so the pills don't crowd the row below.
            Layout.topMargin: Units.spacingMD
            Layout.preferredHeight: root.plexRowHeight
            keyNavigationWraps: true
            previousRow: segmentChips
            nextRow: root.nextRow
            model: root._activeItems
            onActiveFocusChanged: if (activeFocus)
                Qt.callLater(() => root.ensureVisibleRequested(posterRow))
            onActivated: root.openPlexRequested()
            onEscaped: root.escaped()

            delegate: PlexCard {
                required property int index
                required property var modelData
                posterWidth: root.posterW
                posterHeight: root.posterH
                showCaption: root._showCaption
                title: modelData.title || ""
                subtitle: modelData.subtitle || ""
                art: modelData.art || ""
                progress: modelData.progress || 0
                focus: index === posterRow.currentIndex
                onActivated: root.openPlexRequested()
            }
        }
    }
}
