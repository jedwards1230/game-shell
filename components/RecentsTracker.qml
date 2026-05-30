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
        loadRecents.running = true;
    }

    function recordLaunch(app) {
        var body = JSON.stringify({
            "name": app.name || "",
            "exec": app.exec || "",
            "comment": app.comment || ""
        });
        writer.command = ["python3", "-c", tracker._ipcArg("record-launch"), body];
        writer.running = true;
        reloadTimer.start();
    }

    // One-shot Unix-socket request to the input daemon (respects GAME_SHELL_SOCK,
    // falls back to the default per-UID path). The daemon keeps the connection
    // open after replying, so read until the first newline (the response
    // terminator) rather than until EOF, which would block until the timeout.
    function _ipc(cmd) {
        return "import socket,os;s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);s.settimeout(20);s.connect(os.environ.get('GAME_SHELL_SOCK','/run/user/'+str(os.getuid())+'/game-shell-input.sock'));s.sendall(b'" + cmd + "\\n');buf=b'';exec(\"while b'\\\\n' not in buf:\\n c=s.recv(65536)\\n if not c: break\\n buf+=c\");s.close();print(buf.split(b'\\n',1)[0].decode())";
    }

    // Like _ipc, but the request body is sys.argv[1] (a JSON string), keeping
    // arbitrary JSON out of the python source literal.
    function _ipcArg(cmd) {
        return "import socket,os,sys;s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);s.settimeout(20);s.connect(os.environ.get('GAME_SHELL_SOCK','/run/user/'+str(os.getuid())+'/game-shell-input.sock'));s.sendall(('" + cmd + " '+sys.argv[1]+'\\n').encode());buf=b'';exec(\"while b'\\\\n' not in buf:\\n c=s.recv(65536)\\n if not c: break\\n buf+=c\");s.close();print(buf.split(b'\\n',1)[0].decode())";
    }

    Process {
        id: loadRecents
        command: ["python3", "-c", tracker._ipc("get-recents")]
        stdout: SplitParser {
            onRead: line => {
                try {
                    tracker.recentApps = JSON.parse(line);
                } catch (e) {
                    tracker.recentApps = [];
                }
            }
        }
    }

    Process {
        id: writer
        // command set dynamically in recordLaunch(); body passed via argv.
        command: ["true"]
    }

    Timer {
        id: reloadTimer
        interval: 500
        onTriggered: loadRecents.running = true
    }

    Component.onCompleted: load()
}
