import Quickshell.Io
import QtQuick

Item {
    id: root

    property bool running: false

    signal clientsReceived(var clients)
    signal errorOccurred(string message)

    Process {
        id: proc
        running: root.running
        command: ["hyprctl", "clients", "-j"]
        stdout: SplitParser {
            property string buffer: ""
            onRead: (line) => { buffer += line }
        }
        onExited: {
            root.running = false
            try {
                let clients = JSON.parse(proc.stdout.buffer)
                root.clientsReceived(clients)
            } catch(e) {
                root.errorOccurred(e.toString())
            }
            proc.stdout.buffer = ""
        }
    }
}
