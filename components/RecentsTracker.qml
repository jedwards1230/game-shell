pragma Singleton
import Quickshell.Io
import QtQuick

// Tracks recently launched apps in ~/.local/share/game-shell/recents.json.
//
// IMPORTANT: root MUST be Item (not QtObject). Quickshell 0.3.0 cannot host
// Process/Timer children inside a QtObject singleton.
//
// recents.json is written single-line (SplitParser reads line-by-line, so a
// pretty-printed file would fail to parse on the next read). App fields are
// passed to the writer via argv rather than string-interpolated into the
// python source, which avoids quoting bugs for names containing quotes.
Item {
    id: tracker

    // Recently launched apps, newest first: [{name, exec, comment, time}]
    property var recentApps: []
    readonly property int maxEntries: 20

    readonly property string _recentsFile: "~/.local/share/game-shell/recents.json"

    function load() {
        loadRecents.running = true;
    }

    function recordLaunch(app) {
        writer.command = ["python3", "-c", writer._script, (app.name || ""), (app.exec || ""), (app.comment || "")];
        writer.running = true;
        reloadTimer.start();
    }

    Process {
        id: loadRecents
        command: ["python3", "-c", `
import json, os
path = os.path.expanduser('~/.local/share/game-shell/recents.json')
try:
    with open(path) as f:
        data = json.load(f)
    print(json.dumps(data[:15]))
except:
    print('[]')
`]
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
        // app name/exec/comment arrive as argv[1..3]; output is single-line JSON.
        readonly property string _script: `
import json, os, sys, time
p = os.path.expanduser('~/.local/share/game-shell/recents.json')
os.makedirs(os.path.dirname(p), exist_ok=True)
name, exe, comment = sys.argv[1], sys.argv[2], sys.argv[3]
d = []
try:
    with open(p) as f:
        d = json.load(f)
except Exception:
    d = []
entry = {'name': name, 'exec': exe, 'comment': comment, 'time': time.time()}
d = [e for e in d if e.get('name') != name]
d.insert(0, entry)
d = d[:20]
open(p, 'w').write(json.dumps(d, separators=(',', ':')))
`
        command: ["true"]
    }

    Timer {
        id: reloadTimer
        interval: 500
        onTriggered: loadRecents.running = true
    }

    Component.onCompleted: load()
}
