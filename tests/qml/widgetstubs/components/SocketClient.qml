import QtQuick

// Test stub for components/SocketClient.qml (the real one wraps Quickshell.Io.Socket
// to talk to the input daemon). Headless there is no daemon, so this is an inert
// no-op: request()/start()/stop() do nothing and no signals fire. Consumers like
// ServiceMonitor therefore stay at status "unknown" and the widgets render empty —
// exactly the load-time state the widget-contract test wants. See tests/qml/README.md.
Item {
    id: client

    property bool subscribe: false
    property string subscribeCommand: "subscribe"
    property int reconnectMs: 2000

    signal responseReceived(string response)
    signal requestFailed
    signal lineReceived(string line)

    // Still no I/O — the call is only TALLIED so a test can ask whether a given
    // handler issued a poll (see SocketProbe.qml). `body` is deliberately ignored.
    function request(cmd, body) {
        SocketProbe.record(cmd);
    }
    function start() {
    }
    function stop() {
    }
}
