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
        inputManager.startListening();
    }

    Components.InputManager {
        id: inputManager
        onForceQuitRequested: root.forceQuit()
        onEndSessionRequested: inputManager.endSession()
        onControllerWake: {
            if (root.state === "idle")
                avController.wake();
        }
        onHomePressed: {
            if (root.state === "appRunning")
                root.overlayDrawerOpen = !root.overlayDrawerOpen;
        }
        onHomeHeld: {
            if (root.state === "appRunning")
                root.closeAndReturnToShell();
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
            Components.NotificationManager.notify("Stream Ended", "", {
                icon: "📡",
                source: "stream"
            });
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
    }

    function forceQuit() {
        streamManager.forceKill();
        if (root.state === "appRunning")
            closeAndReturnToShell();
        appLifecycle.runningAppClass = "";
        root.overlayDrawerOpen = false;
        root.state = "idle";
        inputManager.grab();
        if (root._layout) {
            root._layout.overlay.hide();
            root._layout.navDrawer.opened = false;
            root._layout.settingsPanel.visible = false;
            root._layout.focusHome();
        }
    }

    function returnToShell() {
        root.overlayDrawerOpen = false;
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
            visible: root.state !== "appRunning" || root.overlayDrawerOpen

            anchors {
                top: true
                bottom: true
                left: true
                right: true
            }

            color: root.state === "appRunning" ? "transparent" : Components.Theme.background
            focusable: true

            Binding {
                target: Components.NotificationManager
                property: "shellVisible"
                value: root.state === "idle"
            }

            Components.ShellLayout {
                id: layout
                anchors.fill: parent
                Component.onCompleted: root._layout = layout
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
                onAppLaunchRequested: app => appLifecycle.checkAndLaunchApp(app)
                onAppFocusRequested: windowClass => appLifecycle.focusApp(windowClass)
                onHomeKeyPressed: {
                    if (root.state === "idle")
                        avController.wake();
                }
                onReturnToShellRequested: root.returnToShell()
                onOverlayDrawerClosed: {
                    root.overlayDrawerOpen = false;
                }
            }
        }
    }
}
