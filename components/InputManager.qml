import Quickshell.Io
import QtQuick

// IPC protocol: see docs/IPC_PROTOCOL.md
// Commands used: grab, release, subscribe
// Events handled: combo:force-quit, combo:end-session, combo:suspend-stream,
//   input-mode:*, controller-wake, controller-disconnected, and intent:* (the
//   control-surface stream — the SOLE shell-intent vocabulary; the legacy
//   gamepad home-press / combo:home-hold bridge was deleted in Phase 5).
//
// Transport: SocketClient (native Quickshell.Io socket, #97) — the python3
// socket shims were retired in Phase 8.
Item {
    id: root

    signal forceQuitRequested
    signal endSessionRequested
    signal suspendStreamRequested
    signal inputModeChanged(string mode)
    signal controllerWake
    signal controllerDisconnected

    // --- Control-surface intents (Channel B: global, focus-independent) ---
    // Mapped from the daemon `intent:*` broadcast stream. QML owns the focus,
    // so it decides what each gamepad-neutral (home-tap/home-hold) means.
    signal intentHome          // global return-to-shell escape (always leaves app)
    signal intentHomeTap        // gamepad Home neutral — short press
    signal intentHomeHold       // gamepad Home neutral — long press (reset)
    signal intentMenu           // toggle nav drawer
    signal intentNav(string direction)  // up | down | left | right
    signal intentSelect
    signal intentBack
    signal intentSettings
    signal intentPower

    function grab() {
        inputGrab.request("grab");
    }
    function release() {
        inputRelease.request("release");
    }
    function startListening() {
        comboListener.start();
    }
    function endSession() {
        endSessionProc.running = true;
    }

    SocketClient {
        id: inputGrab
    }

    SocketClient {
        id: inputRelease
    }

    Process {
        id: endSessionProc
        command: ["/usr/local/bin/end-game-session"]
    }

    SocketClient {
        id: comboListener
        subscribe: true
        onLineReceived: line => {
            if (line === "combo:force-quit")
                root.forceQuitRequested();
            else if (line === "combo:end-session")
                root.endSessionRequested();
            else if (line === "combo:suspend-stream")
                root.suspendStreamRequested();
            else if (line === "input-mode:mouse") {
                // Daemon right-stick->cursor hint. Post-#45 this is just ONE
                // source for mouse-mode among QML's own pointer/key events;
                // route it through the same helper so it can't fight them.
                Theme.enterMouseMode();
                root.inputModeChanged("mouse");
            } else if (line === "input-mode:controller") {
                Theme.exitMouseMode();
                root.inputModeChanged("controller");
            } else if (line === "controller-wake") {
                root.controllerWake();
                NotificationManager.info("controller", "Controller Connected");
            } else if (line === "controller-disconnected") {
                root.controllerDisconnected();
                NotificationManager.warn("controller", "Controller Disconnected");
            } else if (line.startsWith("intent:")) {
                root._handleIntent(line.substring(7));
            }
        }
    }

    // Map a closed-vocabulary intent name to its QML signal. `home` is the
    // global return-to-shell escape (keyboard Super+Escape / automation); the
    // rest fan out 1:1. `menu` (bare Super) is the nav drawer; `home-hold`
    // (Super+Backspace / gamepad Home-hold) is the reset.
    function _handleIntent(name) {
        switch (name) {
        case "home":
            root.intentHome();
            break;
        case "home-tap":
            root.intentHomeTap();
            break;
        case "home-hold":
            root.intentHomeHold();
            break;
        case "menu":
            root.intentMenu();
            break;
        case "nav-up":
            root.intentNav("up");
            break;
        case "nav-down":
            root.intentNav("down");
            break;
        case "nav-left":
            root.intentNav("left");
            break;
        case "nav-right":
            root.intentNav("right");
            break;
        case "select":
            root.intentSelect();
            break;
        case "back":
            root.intentBack();
            break;
        case "settings":
            root.intentSettings();
            break;
        case "power":
            root.intentPower();
            break;
        }
    }
}
