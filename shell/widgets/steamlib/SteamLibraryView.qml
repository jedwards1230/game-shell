import QtQuick
import QtQuick.Layouts
import "../lib"
import "../../components"
import "../../components/lib/artMemo.js" as ArtMemo
import "../../components/lib"

// Steam-library poster view — shared between the Moonlight and Steam home
// widgets (`shell.widgets.steamlib` module; MoonlightWidget hosts this for its
// medium/large sizes, SteamWidget hosts it exclusively). It is NOT an
// independently-enableable widget itself. ONE poster row with a segmented
// header that flips between "Recently Played" and "Library", fed by the
// daemon's `steam-library` IPC, plus an optional right-justified, status-only
// session indicator opposite the tabs (see `showSessionIndicator`).
//
// Selecting a card navigates Big Picture to that game on the host
// (`steam-launch <appid>` now NAVIGATES BPM, it does not launch the game) and
// then — only if no stream is already live — starts the single-target Moonlight
// stream. There is exactly one session; cards do not own a stream. The
// stream/session choreography stays in QML (each host widget's owning
// HomeScreen logic decides); this view only emits `gameSelected(appid)`.
//
// The status-only session indicator in the header reads the `streaming` flag
// from the `steam-library` reply (no separate `sunshine-status` probe). It is
// meaningless for a widget that never streams (the local-Steam widget), which
// sets `showSessionIndicator: false` to suppress it entirely.
//
// `posterScale` is set by the host so the same view renders at two sizes:
//   medium = 0.62 (smaller posters)
//   large  = 0.82 (full posters)
//
// Health-aware: a `ServiceMonitor` keyed on "steam" collapses the view when
// unconfigured/empty and shows a graceful `ServiceStatusNotice` when the host is
// down. The reply's `runningAppid` badges the card whose appid matches as
// "Playing" — source of truth is the gaming host, not which card was tapped.
//
// Focus contract (host uses these): `firstRow`/`lastRow` resolve to the first/
// last *visible* internal region (segment chips, poster row). The session
// indicator is non-focusable (status only).
ColumnLayout {
    id: root

    property Item previousRow: null
    property Item nextRow: null

    // Whether this is the *active* view of the parent MoonlightWidget (i.e. the
    // widget is enabled and its size is medium/large). The parent gates WHICH
    // view renders; this view owns its own DATA-driven visibility. The two are
    // ANDed here — the parent must NOT override `visible` directly (doing so
    // clobbers the persist-last-good binding and renders an empty zero-height
    // column when the data is good). When `viewActive` is false the view is the
    // hidden sibling; when true, `visible` reduces to the data-driven binding.
    property bool viewActive: true

    // Poster scale set by the host (medium vs large). A reflow, not a crop.
    property real posterScale: 0.62

    // The streaming-target host (IP/hostname) this view can wake. Threaded in
    // from the parent widget (which reads it off the configured targets) as a
    // FALLBACK only: the daemon's `steam-library` reply carries the ACTIVE
    // sidecar's host (`reportedHost` below), which wins when present — the view
    // then checks/wakes the server actually selected via `steam-set-host`, not
    // whatever targets.json happens to list first. When the effective host is
    // non-empty and the library is unreachable, the poster row is replaced by a
    // single "Wake <host>" card. Never hardcoded.
    property string host: ""

    // The active sidecar's host from the latest `steam-library` reply (its
    // `host` field, present on both populated and down/empty replies). "" until
    // the first reply — or when the daemon predates the field.
    property string reportedHost: ""

    // What the Wake card shows and `wol` targets: the daemon-reported active
    // host, else the caller-threaded fallback.
    readonly property string effectiveHost: reportedHost !== "" ? reportedHost : host

    // Mirror of the daemon's `streaming` flag from the latest `steam-library`
    // reply (a Moonlight stream is currently live on the host). Surfaced for the
    // parent / session indicator; the library data itself is independent of it.
    property bool streaming: false

    // Whether to render the header's session indicator (a Moonlight stream is
    // live). Default true keeps MoonlightWidget's rendering byte-for-byte
    // unchanged. The local-Steam widget (SteamWidget) sets this false — the
    // Sunshine `streaming` flag is meaningless there since it never streams.
    property bool showSessionIndicator: true

    signal escaped
    // Emitted when a card is activated; HomeScreen launches + streams the appid.
    signal gameSelected(int appid)
    // Emitted on the X face over the RUNNING game's card (only that card emits —
    // see SteamCard's guard). HomeScreen opens a Resume/Quit popover for `appid`.
    signal gameContextRequested(int appid)
    // Emitted when the trailing "Open Steam" action chip fires; HomeScreen resets
    // the host to Big Picture HOME then streams it (no game pre-selected).
    signal openBigPictureRequested
    signal ensureVisibleRequested(var item)

    spacing: Units.spacingMD

    // === Data (populated from `steam-library`) ===
    property var recentItems: []
    property var allItems: []
    // Content signatures of the last ADOPTED payload for each rail. The poll
    // returns freshly built lists every 10s (3s while fast-polling), so a blind
    // reassignment changed the property IDENTITY even when the payload was
    // byte-identical — which reset `_activeItems`, reset the ListView model,
    // destroyed and recreated EVERY poster delegate, and made every art chain
    // re-walk from candidate 0. Same guard, same reasoning as
    // AppLifecycleManager's `_runningWindowsSig` (components/AppLifecycleManager.qml
    // ~:858-868). Cheap string signature, compared before assigning.
    property string _recentSig: ""
    property string _allSig: ""

    // Stable, cheap signature of a library payload. Covers exactly the fields the
    // cards render off (appid identity + title + the three art URLs), so a change
    // that would alter a poster still gets through while an identical repoll does
    // not. JSON.stringify rather than a join on a delimiter: a game title
    // containing the delimiter could otherwise make two different payloads
    // signature-identical and silently freeze the rail.
    function _librarySig(items) {
        if (!items || items.length === undefined)
            return "";
        let parts = [];
        for (let i = 0; i < items.length; i++) {
            let it = items[i];
            parts.push([it.appid, it.name, it.art, it.localArt, it.headerArt]);
        }
        return JSON.stringify(parts);
    }

    // Adopt the two rails from an `ok` payload, but ONLY when the content actually
    // changed. Extracted from `onUpdated` so the guard is directly exercisable
    // headless (tests/qml/tst_steamlibrary.qml) — the stub SocketClient never
    // drives a real poll.
    function _adoptRails(d) {
        // Guard partial replies: only overwrite a rail when the reply actually
        // carries it, so a malformed/partial "ok" can never blank a good row.
        if (d.recentlyPlayed !== undefined) {
            let rItems = d.recentlyPlayed || [];
            let rSig = root._librarySig(rItems);
            if (rSig !== root._recentSig) {
                root._recentSig = rSig;
                root.recentItems = rItems;
            }
        }
        if (d.allGames !== undefined) {
            let aItems = d.allGames || [];
            let aSig = root._librarySig(aItems);
            if (aSig !== root._allSig) {
                root._allSig = aSig;
                root.allItems = aItems;
            }
        }
    }
    // The host's currently-running Steam appid (or -1 when nothing is running).
    // Drives the per-card "Playing" badge.
    property int runningAppid: -1

    readonly property bool _hasRecent: recentItems.length > 0
    readonly property bool _hasAll: allItems.length > 0

    // === Segment (Recently Played vs Library) ===
    // The segment the USER last committed (or the default). It is never coerced —
    // it records intent; `_segment` below derives the rendered segment from it.
    property string _requestedSegment: "recent"
    readonly property var _segmentOptions: {
        let o = [];
        if (_hasRecent)
            o.push({
                "label": "Recently Played",
                "value": "recent"
            });
        if (_hasAll)
            o.push({
                "label": "Library",
                "value": "all"
            });
        return o;
    }

    // The segment actually RENDERED: the request, coerced onto a segment that has
    // content. Derived, never assigned — the same treatment AppsWidget's `_segment`
    // got (shell/widgets/apps/AppsWidget.qml), and the same segment roles.
    //
    // It used to be a writable property that `steamMon.onUpdated` coerced inline,
    // in the same handler that adopts `recentItems`/`allItems` via `_adoptRails`.
    // That is the anti-pattern recorded in docs/OBSERVABILITY.md: a handler writing
    // one dependency of a derived property (`_segment`) alongside another
    // (`recentItems`/`allItems`). In AppsWidget that shape produced `Binding loop
    // detected for property "_activeModel"` on device — Qt abandons a re-entered
    // update and leaves the property STALE, so the row can keep rendering the wrong
    // list.
    //
    // Two concrete wins beyond removing that hazard: the coercion now applies to
    // ANY change of the two rails rather than only when the poll handler happens to
    // fire (previously a rail written from anywhere else left the chip highlight and
    // the rendered row disagreeing), and it is directly exercisable headless — the
    // stub SocketClient never drives a real poll, so the old in-handler coercion was
    // unreachable from a test at all (tests/qml/tst_steamsegment.qml).
    readonly property string _segment: {
        if (root._requestedSegment === "all")
            return root._hasAll ? "all" : (root._hasRecent ? "recent" : "all");
        return root._hasRecent ? "recent" : (root._hasAll ? "all" : "recent");
    }

    // Auto-parking LATCHES: when the derivation has to move off the request because
    // the requested segment is empty, adopt the parked segment as the new request,
    // so a later refill does not yank the row out from under a user who is
    // mid-browse. Deferred via Qt.callLater because `_requestedSegment` is a
    // dependency of `_segment` — writing it straight from `_segment`'s own change
    // handler would reintroduce exactly the synchronous re-entrancy this change
    // removes. It is a no-op whenever the two already agree.
    function _latchParkedSegment() {
        if (root._segment !== root._requestedSegment)
            root._requestedSegment = root._segment;
    }
    on_SegmentChanged: Qt.callLater(root._latchParkedSegment)
    Component.onCompleted: Qt.callLater(root._latchParkedSegment)

    readonly property var _activeItems: _segment === "recent" ? recentItems : allItems

    // Trailing "Open Steam" ACTION chip sentinel — the SegmentedHeader renders it
    // as an ember action chip that opens the host's Big Picture HOME over the stream
    // (no game). Ignored by the segment handler (mirrors PlexWidget's "Open Plex").
    readonly property string _openValue: "__open_bigpicture__"

    readonly property bool rowFocused: posterRow.activeFocus || segmentChips.activeFocus

    // Whether there's something to show or say — the data half of visibility,
    // exposed as its OWN property so the parent (MoonlightWidget) can gate the
    // whole widget on it WITHOUT reading this view's `visible`. Reading a Layout
    // child's `visible` from a sibling binding clobbered that child's `visible`
    // binding (it evaluated once at init and never tracked data changes — the
    // "widget vanishes" bug); routing the parent through this data property
    // instead leaves `visible` as a clean binding only the Layout reads, exactly
    // like PlexWidget's inner rows. True when:
    //   • last-good items loaded (persisted across a transient poll failure — a
    //     stream close churns a non-ok `steam-library` poll, which must NOT make
    //     the games row vanish); OR
    //   • degraded (`unreachable`/`error`) so the ServiceStatusNotice can render.
    // False only when truly unconfigured (`disabled`) with no data, or after a
    // successful poll returned an empty library.
    readonly property bool hasContent: root._hasRecent || root._hasAll || steamMon.degraded

    // Host unavailable: the `steam-library` monitor is degraded
    // (`unreachable`/`error`) AND a host is known to wake. In this state the
    // poster row is hidden and a single "Wake <host>" card takes its place. When
    // no host is known (no reply-carried host, no target) we fall back to the
    // plain ServiceStatusNotice.
    readonly property bool _hostDown: steamMon.degraded && root.effectiveHost !== ""
    // Show the wake card in place of posters whenever the host is down. Once the
    // monitor recovers to "ok" this flips false and the poster row returns
    // automatically (the existing reconnect refetch repopulates the rails).
    readonly property bool _showWake: _hostDown

    // Host reachability as a positive edge, so the art memo can be cleared the
    // moment the sidecar comes back.
    //
    // `Image.status` cannot distinguish "this URL 404s" from "the machine serving
    // it is asleep", and the FIRST art candidate for every card is a URL on this
    // host. While it is down each visible card errors on its local capsule and the
    // memo records it — so without this edge, one sleep/wake cycle would
    // permanently demote the whole library to CDN art. That matters because the
    // CDN portrait can return a ~1.6KB stub that loads as *Ready*, which the error
    // walk cannot skip, leaving those cards blank until the shell restarts (the
    // exact bug that put the LAN capsule first — see SteamCard.qml).
    //
    // A blanket reset rather than a host-scoped one: the daemon builds the capsule
    // URLs, so the view does not know their base, and re-trying a handful of real
    // 404s once per host recovery costs nothing next to the failure it prevents.
    // Deliberately hung off the reachability transition rather than off
    // `_endFastPoll()`, which only runs when WE initiated the wake — a host that
    // comes back on its own (reboot, someone else's WoL) never calls it.
    readonly property bool hostReachable: !root._hostDown
    onHostReachableChanged: {
        if (root.hostReachable)
            ArtMemo.reset();
    }

    // ANDed with `viewActive` (the parent decides WHICH view renders). Nothing
    // outside this view reads `visible` — only the parent Layout — so the binding
    // tracks `hasContent` cleanly.
    visible: root.viewActive && root.hasContent

    // === Home-tile focus contract ===
    // When the host is down the wake card is the only focusable region; otherwise
    // the segment chips → poster row chain applies.
    readonly property var firstRow: root._showWake ? wakeCard : segmentChips
    readonly property var lastRow: root._showWake ? wakeCard : posterRow
    readonly property bool canFocus: visible && (root._showWake || root._hasRecent || root._hasAll)
    readonly property bool regionFocused: root._showWake ? wakeCard.activeFocus : rowFocused

    // The currently-running game's poster card, for the context-popover anchor.
    // During the running-game lockdown the only focusable card is the running one
    // and focus snaps to it, so `posterRow.currentItem` IS the running card; null
    // when nothing runs or the wake card has taken the row's place.
    readonly property Item runningCard: (!root._showWake && root.runningAppid > 0) ? posterRow.currentItem : null

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

    // === Poster geometry (reflow by posterScale) ===
    readonly property int posterW: Math.round(Theme.cardWidth * posterScale)
    readonly property int posterH: Math.round(posterW * 1.5)
    readonly property int _captionBand: Math.round(Theme.fontSmall * 1.4 + Units.spacingSM * 2)
    readonly property int steamRowHeight: posterH + _captionBand

    function refresh() {
        steamMon.refresh();
    }

    // === Wake-on-LAN adaptive polling ===
    // Normal availability poll cadence (10s — see steamMon below). After a wake
    // the poll switches to `_fastPollMs` for `_fastPollWindowMs` so the shell
    // detects the host coming back quickly, then reverts.
    readonly property int _normalPollMs: 10000
    readonly property int _fastPollMs: 3000
    readonly property int _fastPollWindowMs: 120000
    // True while fast-polling. Drives steamMon.dataIntervalMs (3s vs 10s).
    property bool _fastPolling: false

    // Send `wol <host>` over the socket, then switch to the fast poll for 2 min
    // (or until the host returns "ok", whichever is first). One-shot SocketClient
    // request, same mechanism ServiceMonitor uses for `dataCommand`.
    function wakeHost() {
        if (root.effectiveHost === "")
            return;
        wakeReq.request("wol", root.effectiveHost);
        root._fastPolling = true;
        fastPollWindow.restart();
        // Kick an immediate poll so the 3s cadence starts from "now".
        steamMon.refresh();
    }

    // Revert from the fast poll back to the normal cadence and clear the
    // wake-card "Waking…" state.
    function _endFastPoll() {
        root._fastPolling = false;
        fastPollWindow.stop();
    }

    // One-shot WoL command sender (fire-and-forget; the reply JSON is logged but
    // the UI reacts to the availability poll, not the reply).
    SocketClient {
        id: wakeReq
        onResponseReceived: response => console.log("[SteamLibraryView] wol reply:", response)
        onRequestFailed: console.warn("[SteamLibraryView] wol request failed")
    }

    // 2-minute fast-poll window. When it elapses we back off to the normal poll
    // even if the host hasn't returned (it may be powered off / WoL disabled).
    Timer {
        id: fastPollWindow
        interval: root._fastPollWindowMs
        repeat: false
        onTriggered: root._endFastPoll()
    }

    ServiceMonitor {
        id: steamMon
        healthKey: "steam"
        dataCommand: "steam-library"
        // 10s so the session indicator + running badge track the host reasonably
        // promptly (closing a stream reflects within ~10s, not ~30s). The poll is
        // cheap: host /status (a loopback serverinfo) + /library (a few appmanifest
        // reads). Independent of the daemon's 30s service_health broadcast.
        // After a wake the cadence drops to ~3s for 2 min (`_fastPolling`) so the
        // host coming back is detected promptly, then reverts.
        dataIntervalMs: root._fastPolling ? root._fastPollMs : root._normalPollMs
        // ALWAYS poll — the library is available independent of any Moonlight
        // stream/session (the daemon serves `steam-library` whether or not a
        // stream is live). Nothing gates this on shellState/streaming, so a
        // stream start/stop never pauses the poll.
        active: true
        onUpdated: {
            // Adopt the library off the PAYLOAD, not the broadcast `ok` flag. A
            // `steam-library` reply carrying the library arrays is authoritative
            // even when a concurrent stream start/stop has churned the broadcast
            // `status` to a transient non-ok — that disagreement was the reason
            // a good "ok+21 games" reply could fail to render. Health events
            // (which carry no payload) and the `disabled` clear are handled
            // separately below.
            // Adopt off the reply's OWN `status` field (carried in the payload),
            // NOT the broadcast `ok` flag (a concurrent stream start/stop can churn
            // the broadcast to a transient non-ok while the reply still carries the
            // full library). Do NOT use `Array.isArray` on the payload arrays — QML
            // exposes JSON arrays as `QVariantList`, for which `Array.isArray()`
            // returns false, which silently rejected good 21-game replies.
            let d = steamMon.data;
            // Adopt the reply-carried active host on EVERY data poll, ok or not —
            // the down/empty reply carries it too, and that is exactly when the
            // Wake card needs it. Absent field (older daemon) leaves the
            // caller-threaded fallback in charge.
            if (d && d.host !== undefined)
                root.reportedHost = d.host || "";
            if (d && d.status === "ok") {
                // Adopt only on a CONTENT change (see _adoptRails / _librarySig
                // above) — an identical repoll must not churn the property
                // identity and tear down every delegate. `runningAppid` /
                // `streaming` always refresh on ok: they are the per-game and
                // session signals.
                root._adoptRails(d);
                let ra = d.runningAppid;
                root.runningAppid = (typeof ra === "number" && ra > 0) ? ra : -1;
                root.streaming = d.streaming === true;
                // Host is back — drop out of the fast wake-poll (don't wait out
                // the 2-min window). The poster row returns via the normal data
                // binding (_showWake flips false on recovery).
                //
                // DEFERRED on purpose. `_fastPolling` feeds exactly one binding,
                // `steamMon.dataIntervalMs` (below), which IS this monitor's poll
                // `Timer.interval`. Clearing it inline mutates the interval of the
                // very timer whose reply is being handled, from inside that reply
                // handler — a handler on one input of a derived value writing
                // another, the shape docs/OBSERVABILITY.md forbids. Qt.callLater
                // puts the write a turn later, outside the reply cascade, where the
                // timer restart is an ordinary state change. `_endFastPoll()` is
                // idempotent, so a redundant schedule costs nothing.
                if (root._fastPolling)
                    Qt.callLater(root._endFastPoll);
            } else if (d && d.status === "disabled") {
                // Unconfigured (TV_SHELL_STEAM_URL unset): collapse for real.
                // Clear the signatures too, so a later good poll is seen as a
                // change and repopulates the rails.
                root._recentSig = "";
                root._allSig = "";
                root.recentItems = [];
                root.allItems = [];
                root.runningAppid = -1;
                root.streaming = false;
            }
            // Else — a payload-less health event or a transient non-ok poll with
            // no data (`unreachable`/`error`/`unknown`, which a stream-close
            // state churn produces): KEEP the last-good lists, running-game
            // indicator, and streaming flag so the games row doesn't vanish. The
            // next good poll (or recovery refetch) refreshes them.

            // No segment coercion here on purpose — `_segment` derives it (above),
            // so this handler only writes DATA. Writing `_segment` from the same
            // handler that writes its sibling dependencies is the loop shape #441
            // removed from AppsWidget.
        }
    }

    ServiceStatusNotice {
        Layout.fillWidth: true
        serviceName: "Steam"
        status: steamMon.status
    }

    // === Wake card (host unavailable) ===
    // Replaces the poster row + header when the host is down and a target is
    // configured. Activating it fires `wol <host>` and kicks the fast availability
    // poll; the poster row returns automatically once the host reconnects.
    WakeCard {
        id: wakeCard
        visible: root._showWake
        Layout.topMargin: Units.spacingMD
        cardWidth: root.posterW
        cardHeight: root.steamRowHeight
        host: root.effectiveHost
        waking: root._fastPolling
        previousRow: root.previousRow
        nextRow: root.nextRow
        onActivated: {
            root.wakeHost();
            Qt.callLater(() => root.ensureVisibleRequested(wakeCard));
        }
        onEscaped: root.escaped()
        onActiveFocusChanged: if (activeFocus)
            Qt.callLater(() => root.ensureVisibleRequested(wakeCard))
    }

    // === Header: segment chips + right-justified session indicator ===
    SegmentedHeader {
        id: segmentChips
        Layout.fillWidth: true
        visible: !root._showWake && (root._hasRecent || root._hasAll)
        segments: root._segmentOptions
        currentValue: root._segment
        actions: [
            {
                "label": "Open Steam",
                "value": root._openValue
            }
        ]
        previousRow: root.previousRow
        nextRow: posterRow
        // A chip commit records the user's INTENT; `_segment` derives from it.
        onSegmentChanged: value => root._requestedSegment = value
        onActionTriggered: value => root.openBigPictureRequested()
        onEscaped: root.escaped()
        onEnsureVisibleRequested: item => root.ensureVisibleRequested(item)

        // Session indicator — status only (NOT focusable, NOT a pill, NOT
        // actionable). Tells the user at a glance whether a Moonlight stream is
        // live on the host, driven by the `streaming` flag from the same
        // `steam-library` reply that feeds the poster row (no separate
        // `sunshine-status` probe). Gated on `showSessionIndicator` — the local
        // Steam widget hides it entirely (see the property doc above).
        trailing: root.showSessionIndicator ? sessionIndicatorComp : null
    }

    Component {
        id: sessionIndicatorComp
        SessionIndicator {
            inSession: root.streaming
        }
    }

    // === The one poster row (shows the active segment) ===
    NavigableRow {
        id: posterRow
        visible: !root._showWake && root._activeItems.length > 0
        Layout.fillWidth: true
        // Extra breathing room between the chip strip and the posters (on top of
        // the ColumnLayout spacing) so the pills don't crowd the row below.
        Layout.topMargin: Units.spacingMD
        Layout.preferredHeight: root.steamRowHeight
        keyNavigationWraps: true
        previousRow: segmentChips
        nextRow: root.nextRow
        model: root._activeItems

        // Running-game lockdown: while a game is actively running on the host
        // (`runningAppid > 0`), only the matching card is focusable/clickable —
        // every other card is skipped in left/right scroll/focus (and dimmed +
        // disabled in SteamCard). With nothing running (`runningAppid <= 0`) the
        // predicate is null, so navigation is byte-for-byte the unlocked default.
        focusableIndex: root.runningAppid <= 0 ? null : (function (i) {
                return root._activeItems[i] && root._activeItems[i].appid === root.runningAppid;
            })
        onActiveFocusChanged: if (activeFocus) {
            // When locked, snap the highlight onto the running card if it landed
            // on a now-disabled one (e.g. entered via the up/down chain, which
            // forceActiveFocus()es without resetting currentIndex).
            if (root.runningAppid > 0 && !posterRow._indexFocusable(posterRow.currentIndex)) {
                var first = posterRow._firstFocusableIndex();
                if (first >= 0)
                    posterRow.currentIndex = first;
            }
            Qt.callLater(() => root.ensureVisibleRequested(posterRow));
        }
        onActivated: {
            let it = root._activeItems[posterRow.currentIndex];
            if (it)
                root.gameSelected(it.appid);
        }
        onEscaped: root.escaped()

        delegate: SteamCard {
            required property int index
            required property var modelData
            posterWidth: root.posterW
            posterHeight: root.posterH
            showCaption: true
            title: modelData.name || ""
            art: modelData.art || ""
            localArt: modelData.localArt || ""
            headerArt: modelData.headerArt || ""
            playing: root.runningAppid > 0 && modelData.appid === root.runningAppid
            // Locked = a lockdown is active (a game runs) and this is NOT it:
            // dimmed + non-interactive so focus/click can only land on the
            // running card.
            locked: root.runningAppid > 0 && modelData.appid !== root.runningAppid
            focus: index === posterRow.currentIndex
            onActivated: root.gameSelected(modelData.appid)
            // Only the running card's SteamCard emits this (its own guard); forward
            // it up with the card's appid so HomeScreen opens the Resume/Quit menu.
            onContextRequested: root.gameContextRequested(modelData.appid)
        }
    }
}
