pragma Singleton
import Quickshell.Io
import QtQuick

// Centralized settings I/O for game-shell — the QML-side façade over
// ~/.config/game-shell/settings.json.
//
// IMPORTANT: root MUST be Item (not QtObject). Quickshell 0.3.0 cannot host
// Process/Timer children inside a QtObject singleton, and this store needs
// Process children to do socket I/O.
//
// Ownership: the input daemon is the SOLE writer of settings.json. This store
// holds the QML-relevant keys (themeMode, streamingViewMode, controllerDebug)
// and hands them to the daemon via `set-config`; the daemon does the
// read-modify-write, preserving foreign keys it owns (notably keyBindings).
// `keyBindings` here is a read-through mirror kept in sync over IPC.
//
// Binding IPC lives here (rather than in InputManager) because keyBindings is a
// setting and this keeps InputManager focused on the core input-grab/combo path.
// IPC protocol: see docs/IPC_PROTOCOL.md
// Commands used: get-config, set-config, get-bindings, set-binding,
//                capture-next, capture-cancel
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

    // --- Load: read settings via the daemon's `get-config` IPC (no write) ---
    // The daemon is the sole settings.json writer; this asks it for the current
    // document as a compact single-line JSON object.
    Process {
        id: loadProc
        command: ["python3", "-c", store._ipc("get-config")]
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

    // --- Save: hand the QML-owned keys to the daemon via `set-config`. ---
    // The daemon does the read-modify-write, preserving foreign keys (notably
    // daemon-owned keyBindings). The JSON body is built with JSON.stringify and
    // passed as argv to python (not interpolated into the source), so app/enum
    // values can never break the command string. `moonlightViewMode:null` drops
    // the legacy key, matching the previous behavior.
    Process {
        id: saveProc
        command: ["true"]
    }

    function load() {
        loadProc.running = true;
    }
    function save() {
        var body = JSON.stringify({
            "themeMode": store.themeMode,
            "streamingViewMode": store.streamingViewMode,
            "controllerDebug": store.controllerDebug,
            "moonlightViewMode": null
        });
        saveProc.command = ["python3", "-c", store._ipcArg("set-config"), body];
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

    // Build a one-shot Unix-socket request/response python one-liner. The daemon
    // keeps the connection open after replying (it loops waiting for the next
    // command), so we read until the first newline — the response terminator —
    // rather than until EOF, which would block until the socket timeout.
    function _ipc(cmd) {
        return "import socket,os;s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);s.settimeout(20);s.connect(os.environ.get('GAME_SHELL_SOCK','/run/user/'+str(os.getuid())+'/game-shell-input.sock'));s.sendall(b'" + cmd + "\\n');buf=b'';exec(\"while b'\\\\n' not in buf:\\n c=s.recv(65536)\\n if not c: break\\n buf+=c\");s.close();print(buf.split(b'\\n',1)[0].decode())";
    }

    // Like _ipc, but the request body is sys.argv[1] (a JSON string passed as a
    // separate argv element). This keeps arbitrary JSON out of the python source
    // literal — no quoting/escaping bugs — and is used for `set-config`.
    function _ipcArg(cmd) {
        return "import socket,os,sys;s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);s.settimeout(20);s.connect(os.environ.get('GAME_SHELL_SOCK','/run/user/'+str(os.getuid())+'/game-shell-input.sock'));s.sendall(('" + cmd + " '+sys.argv[1]+'\\n').encode());buf=b'';exec(\"while b'\\\\n' not in buf:\\n c=s.recv(65536)\\n if not c: break\\n buf+=c\");s.close();print(buf.split(b'\\n',1)[0].decode())";
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
