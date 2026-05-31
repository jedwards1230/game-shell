import Quickshell
import Quickshell.Io
import QtQuick

// Reads the local Moonlight config (`Moonlight.conf`, a Qt INI file) and
// resolves a Sunshine "currentApp" game id to its friendly app name.
//
// This logic used to live inside the `python3 -c` socket+configparser
// one-liner in StreamCard/StreamManager; Phase 8 (#97) moved the Unix-socket
// half onto SocketClient, so the config lookup moved here (read via `cat`,
// mirroring how targets.json is read elsewhere) to keep the prior name
// resolution behavior without a python subprocess.
//
// The conf has flattened keys like:
//   hosts\1\id=12345
//   hosts\1\name=My Gaming PC
// We build an id -> name map from matching sibling keys.
Item {
    id: conf

    // Map of game-id string -> friendly name (built from the conf on load).
    property var _idToName: ({})
    property bool _loaded: false

    readonly property string _path: {
        let home = Quickshell.env("HOME") || "";
        return home + "/.config/Moonlight Game Streaming Project/Moonlight.conf";
    }

    // Resolve a game id to a friendly name. Returns "" if unknown/unloaded.
    function nameFor(gameId) {
        let id = String(gameId || "");
        if (id === "")
            return "";
        return conf._idToName[id] || "";
    }

    function load() {
        conf._lines = [];
        readProc.running = true;
    }

    property var _lines: []

    Process {
        id: readProc
        command: ["cat", conf._path]
        stdout: SplitParser {
            onRead: line => {
                conf._lines.push(line);
            }
        }
        onExited: {
            // Build id->name map. Match keys ending in `\id`; for each, look up
            // the sibling `\name` key (same `hosts\<n>\` prefix).
            let idKeys = {};   // prefix -> id value
            let nameKeys = {}; // prefix -> name value
            for (let i = 0; i < conf._lines.length; i++) {
                let eq = conf._lines[i].indexOf("=");
                if (eq < 0)
                    continue;
                let key = conf._lines[i].substring(0, eq).trim();
                let val = conf._lines[i].substring(eq + 1).trim();
                if (key.endsWith("\\id")) {
                    idKeys[key.substring(0, key.length - 3)] = val;
                } else if (key.endsWith("\\name")) {
                    nameKeys[key.substring(0, key.length - 5)] = val;
                }
            }
            let map = {};
            for (let prefix in idKeys) {
                let idVal = idKeys[prefix];
                if (nameKeys[prefix] !== undefined)
                    map[idVal] = nameKeys[prefix];
            }
            conf._idToName = map;
            conf._loaded = true;
            conf._lines = [];
            conf.loaded();
        }
    }

    // Emitted once the conf has been (re)read and the id->name map rebuilt.
    signal loaded
}
