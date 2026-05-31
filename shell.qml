import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import QtQuick
import QtQuick.Layouts
import "components" as Components

ShellRoot {
    id: root

    property string state: "idle"
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

    Component.onCompleted: {
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

        // --- Control-surface intents (de-overloaded "home") ---
        // intent:home is the GLOBAL escape (keyboard Super+Escape / automation):
        // always leave the running app, regardless of focus. Fires instantly.
        onIntentHome: root.returnToShell()

        // intent:home-tap is the gamepad Home neutral (short press). The shell
        // owns the focus, so it decides: over a running app -> toggle the app
        // overlay drawer (the full return-to-shell is Home-hold / intent:home);
        // on the home screen -> toggle the nav drawer (the `menu` action).
        onIntentHomeTap: {
            if (root.state === "appRunning") {
                root.overlayDrawerOpen = !root.overlayDrawerOpen;
            } else if (root.state === "idle" && root._layout) {
                avController.wake();
                root._layout.toggleMenu();
            }
        }

        // intent:home-hold = reset. Fired by the gamepad Home long-press AND by
        // keyboard Super+Backspace.
        onIntentHomeHold: root.resetToHome()

        // intent:menu toggles the nav drawer. Fired by bare Super (keyboard),
        // the gamepad Home-tap on the home screen, and the on-screen menu
        // button. Home-screen only: a bare Super press also precedes a
        // Super+<key> chord, so over a running app `menu` is a deliberate no-op
        // (the chord's intent:home/home-hold does the real work, no overlay flash).
        onIntentMenu: {
            if (root.state === "idle" && root._layout)
                root._layout.toggleMenu();
        }

        // intent:settings / intent:power open their panels from the home screen.
        onIntentSettings: {
            if (root.state === "idle" && root._layout) {
                root._layout.settingsPanel.visible = true;
                root._layout.settingsPanel.forceActiveFocus();
            }
        }
        onIntentPower: {
            if (root.state === "idle" && root._layout) {
                root._layout.powerOverlay.opened = true;
                root._layout.powerOverlay.forceActiveFocus();
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
            // #99: short haptic confirmation on a successful app launch.
            if (inputManager)
                inputManager.rumblePulse(120);
        }
        onAppClosed: {
            appLifecycle.runningAppClass = "";
            root.returnToShell();
        }
        // App failed to launch (non-zero exit from the launcher process).
        // Stronger double-ish pulse so the failure is felt, not just logged.
        onAppLaunchFailed: {
            if (inputManager)
                inputManager.rumblePulse(250);
        }
    }

    Components.StreamManager {
        id: streamManager
        shellState: root.state
        onStreamStarted: {
            root.state = "streaming";
            // #99: short haptic confirmation on stream launch.
            if (inputManager)
                inputManager.rumblePulse(120);
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
            // #99: stronger pulse to signal the stream dropped / is reconnecting.
            if (inputManager)
                inputManager.rumblePulse(250);
        }
        onStreamFailed: {
            root.state = "idle";
            inputManager.grab();
            // #99: stronger pulse on terminal stream failure.
            if (inputManager)
                inputManager.rumblePulse(250);
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
            root._layout.powerOverlay.opened = false;
            root._layout.focusHome();
        }
    }

    function returnToShell() {
        root.state = "idle";
        inputManager.grab();
        if (root._layout) {
            root._layout.overlay.hide();
            root._layout.settingsPanel.visible = false;
            root._layout.powerOverlay.opened = false;
            root._layout.focusHome();
        }
    }

    function closeAndReturnToShell() {
        appLifecycle.closeApp();
        returnToShell();
    }

    // Full reset to a clean home screen. Triggered by intent:home-hold — the
    // gamepad Home-hold and the keyboard Super+Backspace. Over a running app it
    // returns to the shell; on the home screen it dismisses every
    // overlay/drawer and refocuses Home.
    function resetToHome() {
        if (root.state === "appRunning") {
            root.returnToShell();
        } else if (root.state === "idle" && root._layout) {
            root._layout.navDrawer.opened = false;
            root._layout.settingsPanel.visible = false;
            root._layout.notificationCenter.opened = false;
            root._layout.powerOverlay.opened = false;
            root._layout.focusHome();
        }
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
                    let argv = Components.StreamProviders.active.quitArgs(target);
                    if (argv.length > 0) {
                        streamQuitProc.command = argv;
                        streamQuitProc.running = true;
                    }
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
