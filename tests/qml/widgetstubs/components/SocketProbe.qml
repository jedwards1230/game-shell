pragma Singleton
import QtQuick

// Test-only request counter for the inert stub SocketClient.
//
// The stub's `request()` is a no-op, so a component that issues a daemon request
// leaves no trace headless — which makes "did this handler issue a poll?" (the
// question tests/qml/tst_steamsegment.qml asks about SteamLibraryView's fast-poll
// path) unobservable. The request is issued by `ServiceMonitor`'s PRIVATE
// `dataReq` child, so a per-instance counter on the stub is not reachable from a
// test either; a singleton tally keyed by command is.
//
// Purely additive: nothing in the shell knows this exists, and the stub still does
// no I/O. See tests/qml/README.md.
QtObject {
    id: probe

    // Total requests since the last reset(), across every stub SocketClient.
    property int total: 0
    // Per-command counts: { "steam-library": 2, ... }
    property var counts: ({})
    // The command string of the most recent request ("" after a reset).
    property string last: ""

    function record(cmd) {
        let c = cmd === undefined ? "" : String(cmd);
        let m = probe.counts;
        m[c] = (m[c] || 0) + 1;
        probe.counts = m;
        probe.last = c;
        probe.total = probe.total + 1;
    }

    function countOf(cmd) {
        return probe.counts[cmd] || 0;
    }

    function reset() {
        probe.counts = ({});
        probe.last = "";
        probe.total = 0;
    }
}
