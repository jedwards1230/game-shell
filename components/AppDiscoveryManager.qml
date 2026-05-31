pragma Singleton
import Quickshell.Io
import QtQuick

// Discovers locally installed applications by asking the input daemon to scan
// XDG .desktop entries (the `list-apps` IPC command — see docs/IPC_PROTOCOL.md).
//
// The daemon owns discovery (via the freedesktop-desktop-entry crate) and
// returns a compact single-line JSON array of {name, exec, icon, comment,
// wmClass}, already filtered (NoDisplay/Hidden/Type), de-duplicated by name,
// and sorted. This replaced an inline `python3 -c` configparser scanner.
//
// IMPORTANT: root MUST be Item (not QtObject). Quickshell 0.3.0 cannot host
// Process children inside a QtObject singleton, and this manager needs a
// Process to talk to the daemon socket.
//
// Single source of truth for the `applications` model consumed by HomeScreen's
// Applications row and by AppLifecycleManager (for window icon/name matching,
// via ShellLayout).
Item {
    id: manager

    // Sorted list of installed apps: [{name, exec, icon, comment, wmClass}]
    property var applications: []
    property bool loading: false

    function refresh() {
        loading = true;
        loadApps.request("list-apps");
    }

    // One-shot daemon IPC over a native Quickshell socket (SocketClient, #97) —
    // the `list-apps` reply is a single (possibly large) JSON line. The python3
    // socket shim was retired in Phase 8.
    SocketClient {
        id: loadApps
        onResponseReceived: line => {
            try {
                manager.applications = JSON.parse(line);
            } catch (e) {
                console.log("AppDiscoveryManager: failed to parse apps:", e);
            }
            manager.loading = false;
        }
        onRequestFailed: {
            manager.loading = false;
        }
    }

    Component.onCompleted: refresh()
}
