pragma Singleton
import Quickshell.Io
import QtQuick

// Tracks recently launched apps in ~/.local/share/game-shell/recents.json.
//
// The input daemon owns the recents file: this component asks it to read
// (`get-recents`) and to append a launch (`record-launch`) over the IPC socket
// (see docs/IPC_PROTOCOL.md). This replaced two inline `python3 -c` processes
// that read/wrote recents.json directly.
//
// IMPORTANT: root MUST be Item (not QtObject). Quickshell 0.3.0 cannot host
// Process/Timer children inside a QtObject singleton.
//
// The record-launch body (a {name,exec,comment} JSON object) is passed to the
// socket helper as a separate argv element, so app names containing quotes or
// spaces can never break the command string.
Item {
    id: tracker

    // Recently launched apps, newest first: [{name, exec, comment, time}]
    property var recentApps: []
    readonly property int maxEntries: 20

    function load() {
        loadRecents.request("get-recents");
    }

    function recordLaunch(app) {
        var body = JSON.stringify({
            "name": app.name || "",
            "exec": app.exec || "",
            "comment": app.comment || ""
        });
        // The JSON body is passed verbatim as the command argument (SocketClient
        // appends it after a space, like the old `_ipcArg` argv form), so app
        // names with quotes/spaces can't break the wire command.
        writer.request("record-launch", body);
        reloadTimer.start();
    }

    // Daemon IPC over a native Quickshell socket (SocketClient, #97) — the
    // python3 socket shims were retired in Phase 8.

    SocketClient {
        id: loadRecents
        onResponseReceived: line => {
            try {
                tracker.recentApps = JSON.parse(line);
            } catch (e) {
                tracker.recentApps = [];
            }
        }
    }

    SocketClient {
        id: writer
        // command issued dynamically in recordLaunch(); body passed via request().
    }

    Timer {
        id: reloadTimer
        interval: 500
        onTriggered: loadRecents.request("get-recents")
    }

    Component.onCompleted: load()
}
