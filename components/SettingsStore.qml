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
// holds the QML-relevant keys (themeMode, streamingViewMode, controllerDebug,
// rumbleEnabled) and hands them to the daemon via `set-config`; the daemon does
// the
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
    property bool rumbleEnabled: true             // gates daemon-fired rumble (#99)

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
    SocketClient {
        id: loadProc
        onResponseReceived: line => {
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
                if (typeof obj.rumbleEnabled === "boolean")
                    store.rumbleEnabled = obj.rumbleEnabled;
                if (obj.keyBindings && typeof obj.keyBindings === "object")
                    store.keyBindings = obj.keyBindings;
            } catch (e) {
                console.log("SettingsStore: failed to parse settings:", e);
            }
        }
    }

    // --- Save: hand the QML-owned keys to the daemon via `set-config`. ---
    // The daemon does the read-modify-write, preserving foreign keys (notably
    // daemon-owned keyBindings). The JSON body is built with JSON.stringify and
    // passed as argv to python (not interpolated into the source), so app/enum
    // values can never break the command string. `moonlightViewMode:null` drops
    // the legacy key, matching the previous behavior.
    SocketClient {
        id: saveProc
    }

    function load() {
        loadProc.request("get-config");
    }
    function save() {
        var body = JSON.stringify({
            "themeMode": store.themeMode,
            "streamingViewMode": store.streamingViewMode,
            "controllerDebug": store.controllerDebug,
            "rumbleEnabled": store.rumbleEnabled,
            "moonlightViewMode": null
        });
        saveProc.request("set-config", body);
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

    function setRumbleEnabled(enabled) {
        rumbleEnabled = enabled;
        save();
        settingsChanged("rumbleEnabled", enabled);
    }

    // === Binding IPC (respects GAME_SHELL_SOCK; no hardcoded socket path) ===
    function getBindings() {
        getBindingsProc.request("get-bindings");
    }

    function setBinding(action, button) {
        setBindingProc.request("set-binding " + action + " " + button);
    }

    function captureNext() {
        captureProc.request("capture-next");
    }

    function cancelCapture() {
        cancelCaptureProc.request("capture-cancel");
    }

    // All daemon IPC goes over native Quickshell sockets (SocketClient, #97).
    // The set-config body (a JSON string) is passed as the request body so
    // arbitrary JSON never needs shell/argv quoting. (Phase 8 retired the
    // python3 socket shims.)

    SocketClient {
        id: getBindingsProc
        onResponseReceived: line => {
            try {
                var b = JSON.parse(line);
                store.keyBindings = b;
                store.bindingsReceived(b);
            } catch (e) {
                console.log("SettingsStore: failed to parse bindings:", e);
            }
        }
    }

    SocketClient {
        id: setBindingProc
        // command issued dynamically in setBinding(); re-fetch so the mirror
        // stays current once the daemon acknowledges.
        onResponseReceived: response => store.getBindings()
        onRequestFailed: store.getBindings()
    }

    SocketClient {
        id: captureProc
        onResponseReceived: line => {
            if (line.startsWith("captured:"))
                store.bindingCaptured(line.substring(9));
            else if (line === "timeout" || line === "cancelled")
                store.captureCancelled();
        }
    }

    SocketClient {
        id: cancelCaptureProc
    }

    Component.onCompleted: load()
}
