import Quickshell.Io
import QtQuick

// Queries the Hyprland client list via the input daemon's `hypr-clients` IPC
// command (see docs/IPC_PROTOCOL.md), replacing the former `hyprctl clients -j`
// shell-out. The daemon owns the Hyprland IPC connection (rust/src/hyprland.rs)
// and returns a compact single-line JSON array of {class,title,address,workspace}
// — the same fields the QML consumers read. One-shot `hyprctl dispatch` actions
// (exec/closewindow/focuswindow/fullscreen) stay shell-outs in the callers.
Item {
    id: root

    property bool running: false

    signal clientsReceived(var clients)
    signal errorOccurred(string message)

    // One-shot daemon IPC over a native Quickshell socket (SocketClient, #97).
    // The `hypr-clients` reply is a single JSON line; the python3 socket shim
    // was retired in Phase 8.
    SocketClient {
        id: sock
        onResponseReceived: line => {
            root.running = false;
            try {
                let clients = JSON.parse(line);
                root.clientsReceived(clients);
            } catch (e) {
                root.errorOccurred(e.toString());
            }
        }
        onRequestFailed: {
            root.running = false;
            root.errorOccurred("hypr-clients socket request failed");
        }
    }

    // Fire the request whenever `running` flips true (preserves the old
    // `running: root.running` Process binding semantics).
    onRunningChanged: {
        if (root.running)
            sock.request("hypr-clients");
    }
}
