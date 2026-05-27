import Quickshell.Io
import QtQuick

// IPC protocol: see docs/IPC_PROTOCOL.md
// Commands used: grab, release, subscribe
// Events handled: combo:force-quit, combo:end-session, combo:suspend-stream, input-mode:*, controller-wake, controller-disconnected, home-press, combo:home-hold
Item {
    id: root

    signal forceQuitRequested
    signal endSessionRequested
    signal suspendStreamRequested
    signal inputModeChanged(string mode)
    signal controllerWake
    signal controllerDisconnected
    signal homePressed
    signal homeHeld

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
                    Theme.mouseMode = true;
                    root.inputModeChanged("mouse");
                } else if (line === "input-mode:controller") {
                    Theme.mouseMode = false;
                    root.inputModeChanged("controller");
                } else if (line === "controller-wake") {
                    root.controllerWake();
                    NotificationManager.info("controller", "Controller Connected");
                } else if (line === "controller-disconnected") {
                    root.controllerDisconnected();
                    NotificationManager.warn("controller", "Controller Disconnected");
                } else if (line === "home-press")
                    root.homePressed();
                else if (line === "combo:home-hold")
                    root.homeHeld();
            }
        }
        onExited: {
            comboReconnect.start();
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
