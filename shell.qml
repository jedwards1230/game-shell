import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import QtQuick
import QtQuick.Layouts
import "components" as Components

ShellRoot {
    id: root

    property string state: "idle"
    property var targets: []
    property bool overlayDrawerOpen: false
    property var _applications: []
    property var _layout: null

    // Declarative state machine — ShellRoot doesn't inherit Item,
    // so we host states/transitions on an internal Item wrapper.
    Item {
        id: stateMachine
        state: root.state

        states: [
            State {
                name: "idle"
                PropertyChanges {
                    target: root
                    overlayDrawerOpen: false
                }
            },
            State {
                name: "launching"
            },
            State {
                name: "streaming"
            },
            State {
                name: "reconnecting"
            },
            State {
                name: "appRunning"
            }
        ]

        transitions: [
            Transition {
                from: "*"
                to: "idle"
            }
        ]
    }

    Process {
        id: streamQuitProc
        onExited: {
            Components.NotificationManager.info("stream", "Stream Session Ended");
        }
    }

    Process {
        id: loadTargets
        command: ["cat", "/opt/game-shell/targets.json"]
        stdout: SplitParser {
            onRead: line => {
                try {
                    root.targets = JSON.parse(line);
                } catch (e) {
                    console.log("Failed to parse targets:", e);
                }
            }
        }
    }

    Component.onCompleted: {
        loadTargets.running = true;
        inputManager.grab();
        inputManager.startListening();
    }

    Components.InputManager {
        id: inputManager
        onForceQuitRequested: root.forceQuit()
        onEndSessionRequested: inputManager.endSession()
        onSuspendStreamRequested: {
            if (root.state === "streaming" || root.state === "reconnecting")
                streamManager.suspend();
        }
        onControllerWake: {
            if (root.state === "idle")
                avController.wake();
        }
        onHomePressed: {
            if (root.state === "appRunning") {
                root.overlayDrawerOpen = !root.overlayDrawerOpen;
            } else if (root.state === "idle" && root._layout) {
                avController.wake();
                root._layout.handleHomeTap();
            }
        }
        onHomeHeld: {
            if (root.state === "appRunning") {
                root.returnToShell();
            } else if (root.state === "idle" && root._layout) {
                root._layout.navDrawer.opened = false;
                root._layout.settingsPanel.visible = false;
                root._layout.notificationCenter.opened = false;
                root._layout.focusHome();
            }
        }
    }

    Components.AVController {
        id: avController
        shellState: root.state
    }

    Components.AppLifecycleManager {
        id: appLifecycle
        shellState: root.state
        applications: root._applications
        onAppLaunched: {
            root.state = "appRunning";
        }
        onAppClosed: {
            appLifecycle.runningAppClass = "";
            root.returnToShell();
        }
    }

    Components.StreamManager {
        id: streamManager
        shellState: root.state
        onStreamStarted: {
            root.state = "streaming";
        }
        onStreamEnded: {
            Components.NotificationManager.info("stream", "Stream Suspended");
            root.returnToShell();
        }
        onStreamSuspended: {
            Components.NotificationManager.info("stream", "Stream Suspended");
            root.returnToShell();
        }
        onStreamCrashed: attempts => {
            root.state = "reconnecting";
        }
        onStreamFailed: {
            root.state = "idle";
            inputManager.grab();
        }
        onRequestOverlayShow: msg => {
            if (root._layout)
                root._layout.overlay.show(msg);
        }
        onRequestOverlayHide: {
            if (root._layout)
                root._layout.overlay.hide();
        }
        onRequestInputRelease: inputManager.release()
        onRequestInputGrab: inputManager.grab()
        onSessionConflictDetected: (runningApp, hostName) => {
            if (root._layout) {
                root._layout.sessionDialog.runningApp = runningApp;
                root._layout.sessionDialog.hostName = hostName;
                root._layout.sessionDialog.opened = true;
            }
        }
        onSessionCheckCancelled: {
            root.state = "idle";
            inputManager.grab();
            if (root._layout)
                root._layout.focusHome();
        }
    }

    function forceQuit() {
        streamManager.forceKill();
        if (root.state === "appRunning")
            closeAndReturnToShell();
        appLifecycle.runningAppClass = "";
        root.state = "idle";
        inputManager.grab();
        if (root._layout) {
            root._layout.overlay.hide();
            root._layout.sessionDialog.opened = false;
            root._layout.navDrawer.opened = false;
            root._layout.settingsPanel.visible = false;
            root._layout.notificationCenter.opened = false;
            root._layout.focusHome();
        }
    }

    function returnToShell() {
        root.state = "idle";
        inputManager.grab();
        if (root._layout) {
            root._layout.overlay.hide();
            root._layout.settingsPanel.visible = false;
            root._layout.focusHome();
        }
    }

    function closeAndReturnToShell() {
        appLifecycle.closeApp();
        returnToShell();
    }

    Variants {
        model: Quickshell.screens

        PanelWindow {
            required property var modelData
            screen: modelData
            visible: (root.state !== "appRunning" && root.state !== "streaming" && root.state !== "reconnecting" && root.state !== "launching") || root.overlayDrawerOpen

            anchors {
                top: true
                bottom: true
                left: true
                right: true
            }

            color: root.state === "appRunning" ? "transparent" : Components.Theme.background
            // Exclusive keyboard focus so non-Hyprland-bound keys (arrows,
            // Enter, Esc, etc.) reach focused QML widgets. Without this,
            // the compositor gives keyboard input to whatever non-layer
            // window happens to have focus.
            WlrLayershell.keyboardFocus: WlrKeyboardFocus.Exclusive

            Binding {
                target: Components.NotificationManager
                property: "shellVisible"
                value: root.state === "idle"
            }

            Components.ShellLayout {
                id: layout
                anchors.fill: parent
                Component.onCompleted: {
                    root._layout = layout;
                    root._layout.focusHome();
                }
                shellState: root.state
                targets: root.targets
                runningWindows: appLifecycle.runningWindows
                runningAppClass: appLifecycle.runningAppClass
                overlayDrawerOpen: root.overlayDrawerOpen
                avSystemOn: avController.systemOn
                avWaking: avController.waking
                onApplicationsChanged: root._applications = applications
                onStreamRequested: target => {
                    root.state = "launching";
                    avController.forceWake();
                    streamManager.launch(target);
                }
                onStreamQuitRequested: target => {
                    streamQuitProc.command = ["moonlight", "quit", target.host];
                    streamQuitProc.running = true;
                }
                onAppLaunchRequested: app => appLifecycle.checkAndLaunchApp(app)
                onAppFocusRequested: windowClass => appLifecycle.focusApp(windowClass)
                onAppCloseRequested: windowClass => appLifecycle.closeAppByClass(windowClass)
                onReturnToShellRequested: root.returnToShell()
                onOverlayDrawerClosed: {
                    root.overlayDrawerOpen = false;
                }

                Connections {
                    target: layout.sessionDialog
                    function onResumeRequested() {
                        layout.sessionDialog.opened = false;
                        streamManager.resumeSession();
                    }
                    function onQuitRequested() {
                        layout.sessionDialog.opened = false;
                        streamManager.quitAndRelaunch();
                    }
                    function onCancelled() {
                        layout.sessionDialog.opened = false;
                        streamManager.cancelSessionCheck();
                    }
                }
            }
        }
    }
}
