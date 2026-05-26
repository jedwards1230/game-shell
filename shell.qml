import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import QtQuick
import QtQuick.Layouts
import "components" as Components

ShellRoot {
    id: root

    property string state: "idle"
    property var currentTarget: null
    property int crashCount: 0
    property var targets: []
    property bool overlayDrawerOpen: false
    property var _applications: []

    Process {
        id: loadTargets
        command: ["cat", "/opt/game-shell/targets.json"]
        stdout: SplitParser {
            onRead: (line) => {
                try { root.targets = JSON.parse(line) }
                catch(e) { console.log("Failed to parse targets:", e) }
            }
        }
    }

    Component.onCompleted: { loadTargets.running = true; inputManager.startListening() }

    Components.InputManager {
        id: inputManager
        onForceQuitRequested: root.forceQuit()
        onEndSessionRequested: inputManager.endSession()
        onControllerWake: {
            if (root.state === "idle") avController.wake()
        }
        onHomePressed: {
            if (root.state === "appRunning")
                root.overlayDrawerOpen = !root.overlayDrawerOpen
        }
        onHomeHeld: {
            if (root.state === "appRunning")
                root.closeAndReturnToShell()
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
        onAppLaunched: { root.state = "appRunning" }
        onAppClosed: root.returnToShell()
    }

    Process {
        id: moonlight
        onExited: (exitCode, exitStatus) => {
            if (exitCode === 0) {
                layout.overlay.hide()
                root.state = "idle"
                inputManager.grab()
            } else {
                root.crashCount++
                if (root.crashCount < 5) {
                    root.state = "reconnecting"
                    layout.overlay.show("Reconnecting... (" + root.crashCount + "/5)")
                    reconnectTimer.start()
                } else {
                    layout.overlay.show("Stream failed after 5 attempts")
                    errorDismissTimer.start()
                    root.state = "idle"
                    inputManager.grab()
                }
            }
        }
    }

    Timer {
        id: reconnectTimer
        interval: 2000
        onTriggered: launchMoonlight()
    }

    Timer {
        id: errorDismissTimer
        interval: 5000
        onTriggered: { layout.overlay.hide() }
    }

    Timer {
        id: crashResetTimer
        interval: 300000
        running: root.state === "streaming"
        onTriggered: { root.crashCount = 0 }
    }

    Process {
        id: forceKill
        command: ["bash", "-c", "pkill -f moonlight; pkill -f steam; true"]
    }

    function forceQuit() {
        moonlight.running = false
        forceKill.running = true
        if (root.state === "appRunning") closeAndReturnToShell()
        root.overlayDrawerOpen = false
        root.state = "idle"
        inputManager.grab()
        layout.navDrawer.opened = false
        layout.settingsPanel.visible = false
        layout.focusHome()
    }

    function returnToShell() {
        appLifecycle.runningAppClass = ""
        root.overlayDrawerOpen = false
        root.state = "idle"
        inputManager.grab()
        layout.settingsPanel.visible = false
        layout.focusHome()
    }

    function closeAndReturnToShell() {
        appLifecycle.closeApp()
        returnToShell()
    }

    function launchStream(target) {
        root.currentTarget = target
        root.state = "launching"
        root.crashCount = 0
        layout.overlay.show("Launching " + (target.app || target.name) + "...")
        avController.forceWake()
        launchMoonlight()
    }

    function launchMoonlight() {
        let args = ["moonlight", "stream", currentTarget.host, currentTarget.app]
        if (currentTarget.resolution === "3840x2160") args.push("--4k")
        if (currentTarget.fps) { args.push("--fps"); args.push(String(currentTarget.fps)) }
        if (currentTarget.hdr) args.push("--hdr")
        if (currentTarget.codec) { args.push("--video-codec"); args.push(currentTarget.codec) }
        args.push("--display-mode", "fullscreen")
        args.push("--no-quit-after")
        args.push("--no-frame-pacing")
        moonlight.command = args
        inputManager.release()
        root.state = "streaming"
        moonlight.running = true
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

            Components.ShellLayout {
                id: layout
                anchors.fill: parent
                shellState: root.state
                targets: root.targets
                runningWindows: appLifecycle.runningWindows
                runningAppClass: appLifecycle.runningAppClass
                overlayDrawerOpen: root.overlayDrawerOpen
                avSystemOn: avController.systemOn
                avWaking: avController.waking
                onApplicationsChanged: root._applications = applications
                onStreamRequested: (target) => root.launchStream(target)
                onAppLaunchRequested: (app) => appLifecycle.checkAndLaunchApp(app)
                onAppFocusRequested: (windowClass) => appLifecycle.focusApp(windowClass)
                onHomeKeyPressed: {
                    if (root.state === "idle") avController.wake()
                }
                onReturnToShellRequested: root.returnToShell()
                onOverlayDrawerClosed: { root.overlayDrawerOpen = false }
            }
        }
    }
}
