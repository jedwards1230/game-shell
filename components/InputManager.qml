import Quickshell.Io
import QtQuick

// IPC protocol: see docs/IPC_PROTOCOL.md
// Commands used: grab, release, subscribe
// Events handled: combo:force-quit, combo:end-session, combo:suspend-stream,
//   input-mode:*, controller-wake, controller-disconnected, and intent:* (the
//   control-surface stream — the SOLE shell-intent vocabulary; the legacy
//   gamepad home-press / combo:home-hold bridge was deleted in Phase 5).
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

    // Multi-stroke keyboard reset: three intent:home within the window below
    // invokes the SAME reset action the gamepad Home-hold triggers. Counted
    // here (in QML), NOT in the daemon (per resolved OQ1).
    signal resetRequested

    readonly property int homeTapWindowMs: 1500
    property int _homeTapCount: 0

    function grab() {
        inputGrab.running = true;
    }
    function release() {
        inputRelease.running = true;
    }
    function startListening() {
        comboListener.running = true;
    }
    function endSession() {
        endSessionProc.running = true;
    }

    // intent:home arrives (keyboard Super / automation). The first tap arms a
    // window; a single tap that survives the window is a plain global escape,
    // three taps inside the window is the reset multi-stroke.
    function _onIntentHome() {
        _homeTapCount += 1;
        if (_homeTapCount >= 3) {
            homeTapTimer.stop();
            _homeTapCount = 0;
            root.resetRequested();
            return;
        }
        homeTapTimer.restart();
    }

    Timer {
        id: homeTapTimer
        interval: root.homeTapWindowMs
        onTriggered: {
            // The window closed with fewer than three taps. A lone tap is the
            // global escape; we deliberately do NOT replay buffered taps as
            // multiple escapes — a quick double-tap collapses to one escape.
            if (root._homeTapCount > 0)
                root.intentHome();
            root._homeTapCount = 0;
        }
    }

    Process {
        id: inputGrab
        command: ["python3", "-c", "import socket,os; s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); s.connect(os.environ.get('GAME_SHELL_SOCK','/run/user/'+str(os.getuid())+'/game-shell-input.sock')); s.sendall(b'grab\\n'); print(s.recv(64).decode().strip()); s.close()"]
    }

    Process {
        id: inputRelease
        command: ["python3", "-c", "import socket,os; s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); s.connect(os.environ.get('GAME_SHELL_SOCK','/run/user/'+str(os.getuid())+'/game-shell-input.sock')); s.sendall(b'release\\n'); print(s.recv(64).decode().strip()); s.close()"]
    }

    Process {
        id: endSessionProc
        command: ["/usr/local/bin/end-game-session"]
    }

    Process {
        id: comboListener
        command: ["python3", "-c", "import socket,os;s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);s.connect(os.environ.get('GAME_SHELL_SOCK','/run/user/'+str(os.getuid())+'/game-shell-input.sock'));s.sendall(b'subscribe\\n');[print(l,flush=True) for d in iter(lambda:s.recv(1024),b'') for l in d.decode().splitlines()]"]
        stdout: SplitParser {
            onRead: line => {
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
        onExited: {
            comboReconnect.start();
        }
    }

    // Map a closed-vocabulary intent name to its QML signal. `home` is the
    // multi-tap-aware global escape; the rest fan out 1:1.
    function _handleIntent(name) {
        switch (name) {
        case "home":
            _onIntentHome();
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

    Timer {
        id: comboReconnect
        interval: 2000
        onTriggered: {
            comboListener.running = true;
        }
    }
}
