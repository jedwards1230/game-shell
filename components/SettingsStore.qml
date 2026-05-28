pragma Singleton
import Quickshell.Io
import QtQuick

// Centralized settings I/O for game-shell — the single source of truth for
// ~/.config/game-shell/settings.json from the QML side.
//
// IMPORTANT: root MUST be Item (not QtObject). Quickshell 0.3.0 cannot host
// Process/Timer children inside a QtObject singleton, and this store needs
// Process children to do file/socket I/O.
//
// Ownership split (both writers do read-modify-write of disjoint keys, which is
// acceptable for a single-user kiosk):
//   - QML side (this store) owns: themeMode, streamingViewMode, controllerDebug
//   - Python daemon owns persistence of: keyBindings
// This store is the SOLE QML-side writer of settings.json. `keyBindings` here is
// a read-through mirror kept in sync with the daemon over IPC.
//
// Binding IPC lives here (rather than in InputManager) because keyBindings is a
// setting and this keeps InputManager focused on the core input-grab/combo path.
// IPC protocol: see docs/IPC_PROTOCOL.md
// Binding commands used: get-bindings, set-binding, capture-next, capture-cancel
Item {
    id: store

    // === Persisted settings (QML-owned) ===
    property string themeMode: "dark"             // "auto" | "light" | "dark"
    property string streamingViewMode: "servers"  // "servers" | "apps"
    property bool controllerDebug: false

    // === Daemon-owned mirror (authoritative copy lives in the daemon) ===
    property var keyBindings: ({})

    // === Change notification (foundation for #53 file-watching) ===
    signal settingsChanged(string key, var value)

    // === Binding IPC signals ===
    signal bindingsReceived(var bindings)
    signal bindingCaptured(string button)
    signal captureCancelled

    readonly property string _settingsFile: "~/.config/game-shell/settings.json"

    // --- Load: read settings.json into properties (no write) ---
    Process {
        id: loadProc
        command: ["bash", "-c", "cat " + store._settingsFile + " 2>/dev/null || true"]
        stdout: SplitParser {
            onRead: line => {
                try {
                    var obj = JSON.parse(line);
                    if (obj.themeMode === "auto" || obj.themeMode === "light" || obj.themeMode === "dark")
                        store.themeMode = obj.themeMode;
                    // Migration: prefer streamingViewMode, fall back to the
                    // legacy moonlightViewMode key for existing settings files.
                    var viewMode = obj.streamingViewMode !== undefined ? obj.streamingViewMode : obj.moonlightViewMode;
                    if (viewMode === "servers" || viewMode === "apps")
                        store.streamingViewMode = viewMode;
                    if (typeof obj.controllerDebug === "boolean")
                        store.controllerDebug = obj.controllerDebug;
                    if (obj.keyBindings && typeof obj.keyBindings === "object")
                        store.keyBindings = obj.keyBindings;
                } catch (e) {
                    console.log("SettingsStore: failed to parse settings:", e);
                }
            }
        }
    }

    // --- Save: read-modify-write; preserves daemon-owned keyBindings ---
    Process {
        id: saveProc
        command: ["python3", "-c", "import json,os,pathlib;" + "p=pathlib.Path(os.path.expanduser('" + store._settingsFile + "'));" + "p.parent.mkdir(parents=True,exist_ok=True);" + "d=json.loads(p.read_text()) if p.exists() else {};" + "d['themeMode']='" + store.themeMode + "';" + "d['streamingViewMode']='" + store.streamingViewMode + "';" + "d.pop('moonlightViewMode',None);" + "d['controllerDebug']=" + (store.controllerDebug ? "True" : "False") + ";" + "p.write_text(json.dumps(d,separators=(',',':')))"]
    }

    function load() {
        loadProc.running = true;
    }
    function save() {
        saveProc.running = true;
    }

    function setThemeMode(mode) {
        if (mode === "auto" || mode === "light" || mode === "dark") {
            themeMode = mode;
            save();
            settingsChanged("themeMode", mode);
        }
    }

    function setStreamingViewMode(mode) {
        if (mode === "servers" || mode === "apps") {
            streamingViewMode = mode;
            save();
            settingsChanged("streamingViewMode", mode);
        }
    }

    function setControllerDebug(enabled) {
        controllerDebug = enabled;
        save();
        settingsChanged("controllerDebug", enabled);
    }

    // === Binding IPC (respects GAME_SHELL_SOCK; no hardcoded socket path) ===
    function getBindings() {
        getBindingsProc.running = true;
    }

    function setBinding(action, button) {
        setBindingProc.command = ["python3", "-c", store._ipc("set-binding " + action + " " + button)];
        setBindingProc.running = true;
    }

    function captureNext() {
        captureProc.running = true;
    }

    function cancelCapture() {
        cancelCaptureProc.running = true;
    }

    // Build a one-shot Unix-socket request/response python one-liner.
    function _ipc(cmd) {
        return "import socket,os;s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);s.settimeout(20);s.connect(os.environ.get('GAME_SHELL_SOCK','/run/user/'+str(os.getuid())+'/game-shell-input.sock'));s.sendall(b'" + cmd + "\\n');print(s.recv(1024).decode().strip());s.close()";
    }

    Process {
        id: getBindingsProc
        command: ["python3", "-c", store._ipc("get-bindings")]
        stdout: SplitParser {
            onRead: line => {
                try {
                    var b = JSON.parse(line);
                    store.keyBindings = b;
                    store.bindingsReceived(b);
                } catch (e) {
                    console.log("SettingsStore: failed to parse bindings:", e);
                }
            }
        }
    }

    Process {
        id: setBindingProc
        // command set dynamically in setBinding(); re-fetch so the mirror stays current
        onExited: store.getBindings()
    }

    Process {
        id: captureProc
        command: ["python3", "-c", store._ipc("capture-next")]
        stdout: SplitParser {
            onRead: line => {
                if (line.startsWith("captured:"))
                    store.bindingCaptured(line.substring(9));
                else if (line === "timeout" || line === "cancelled")
                    store.captureCancelled();
            }
        }
    }

    Process {
        id: cancelCaptureProc
        command: ["python3", "-c", store._ipc("capture-cancel")]
    }

    Component.onCompleted: load()
}
