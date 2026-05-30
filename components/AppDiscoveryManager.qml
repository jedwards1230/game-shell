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
        loadApps.running = true;
    }

    // One-shot Unix-socket request to the input daemon (respects GAME_SHELL_SOCK,
    // falls back to the default per-UID path). The `list-apps` reply can be large,
    // so accumulate chunks until the first newline (the response terminator). The
    // daemon keeps the connection open after replying, so reading until EOF would
    // block until the socket timeout instead.
    readonly property string _listAppsCmd: "import socket,os;s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);s.settimeout(20);s.connect(os.environ.get('GAME_SHELL_SOCK','/run/user/'+str(os.getuid())+'/game-shell-input.sock'));s.sendall(b'list-apps\\n');buf=b'';exec(\"while b'\\\\n' not in buf:\\n c=s.recv(65536)\\n if not c: break\\n buf+=c\");s.close();print(buf.split(b'\\n',1)[0].decode())"

    Process {
        id: loadApps
        command: ["python3", "-c", manager._listAppsCmd]
        stdout: SplitParser {
            onRead: line => {
                try {
                    manager.applications = JSON.parse(line);
                } catch (e) {
                    console.log("AppDiscoveryManager: failed to parse apps:", e);
                }
                manager.loading = false;
            }
        }
    }

    Component.onCompleted: refresh()
}
