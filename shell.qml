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

    Component.onCompleted: { loadTargets.running = true }

    Timer {
        id: crashResetTimer
        interval: 300000
        running: root.state === "streaming"
        onTriggered: { root.crashCount = 0 }
    }

    Process {
        id: avWake
        command: ["/usr/local/bin/living-room-cec", "on"]
        onExited: (exitCode, exitStatus) => {
            if (root.state === "launching") launchMoonlight()
        }
    }

    Process {
        id: moonlight
        onExited: (exitCode, exitStatus) => {
            if (exitCode === 0) {
                root.state = "idle"
                overlay.hide()
                grabInput()
            } else {
                root.crashCount++
                if (root.crashCount < 5) {
                    root.state = "reconnecting"
                    overlay.show("Reconnecting...")
                    overlay.attemptCount = root.crashCount
                    reconnectTimer.start()
                } else {
                    root.state = "idle"
                    overlay.show("Stream failed after 5 attempts")
                    errorDismissTimer.start()
                    grabInput()
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
        onTriggered: { overlay.hide() }
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
        id: endSession
        command: ["/usr/local/bin/end-game-session"]
    }

    function launchStream(target) {
        root.currentTarget = target
        root.state = "launching"
        root.crashCount = 0
        overlay.show("Launching " + target.name + "...")
        avWake.running = true
    }

    function launchMoonlight() {
        let args = ["moonlight", "stream", currentTarget.host, currentTarget.app]
        if (currentTarget.resolution === "3840x2160") args.push("--4k")
        if (currentTarget.fps) { args.push("--fps"); args.push(String(currentTarget.fps)) }
        if (currentTarget.hdr) args.push("--hdr")
        if (currentTarget.codec) { args.push("--video-codec"); args.push(currentTarget.codec) }
        args.push("--display-mode", "borderless")
        args.push("--no-quit-after")
        args.push("--no-frame-pacing")

        moonlight.command = args
        releaseInput()
        root.state = "streaming"
        overlay.hide()
        moonlight.running = true
    }

    function grabInput() { inputGrab.running = true }
    function releaseInput() { inputRelease.running = true }

    Variants {
        model: Quickshell.screens

        PanelWindow {
            required property var modelData
            screen: modelData

            anchors {
                top: true
                bottom: true
                left: true
                right: true
            }

            color: Components.Theme.background
            focusable: true

            Item {
                anchors.fill: parent

                Components.HomeScreen {
                    id: homeScreen
                    anchors.fill: parent
                    visible: root.state === "idle"
                    targets: root.targets
                    shellState: root.state
                    focus: root.state === "idle" && !settingsPanel.visible

                    onStreamRequested: (target) => root.launchStream(target)
                    onSettingsRequested: {
                        settingsPanel.visible = true
                        settingsPanel.forceActiveFocus()
                    }
                }

                Components.SettingsPanel {
                    id: settingsPanel
                    anchors.fill: parent
                    onClosed: {
                        settingsPanel.visible = false
                        homeScreen.forceActiveFocus()
                    }
                }

                Components.StreamOverlay {
                    id: overlay
                    anchors.fill: parent
                }

                // --- Debug Input Overlay ---
                // Visible when controllerDebug is enabled in settings.
                // Subscribes to the input daemon socket for real-time events.
                // NOTE: The daemon currently only broadcasts combo events
                // (e.g. "combo:end-session"). Per-button/axis event streaming
                // requires a daemon-side enhancement to call _notify_subscribers
                // for each input event.
                Item {
                    id: debugOverlay
                    anchors.fill: parent
                    visible: Components.Theme.controllerDebug
                    z: 100

                    property var eventLog: []
                    property int maxEvents: 4

                    // Subscribe to daemon events
                    Process {
                        id: debugSubscribe
                        command: ["python3", "-c", "import socket,os,sys; s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); s.connect(os.environ.get('GAME_SHELL_SOCK','/run/user/'+str(os.getuid())+'/game-shell-input.sock')); s.sendall(b'subscribe\\n')\nwhile True:\n    data=s.recv(1024)\n    if not data: break\n    for line in data.decode().splitlines():\n        print(line,flush=True)"]
                        running: debugOverlay.visible
                        stdout: SplitParser {
                            onRead: (line) => {
                                if (line === "subscribed") return
                                let events = debugOverlay.eventLog.slice()
                                events.unshift({ text: line, time: Date.now() })
                                if (events.length > debugOverlay.maxEvents)
                                    events = events.slice(0, debugOverlay.maxEvents)
                                debugOverlay.eventLog = events
                            }
                        }
                        onExited: {
                            // Reconnect if overlay is still visible
                            if (debugOverlay.visible)
                                reconnectDebug.start()
                        }
                    }

                    Timer {
                        id: reconnectDebug
                        interval: 2000
                        onTriggered: {
                            if (debugOverlay.visible)
                                debugSubscribe.running = true
                        }
                    }

                    // Fade out old events
                    Timer {
                        interval: 500
                        running: debugOverlay.visible
                        repeat: true
                        onTriggered: {
                            let now = Date.now()
                            let filtered = debugOverlay.eventLog.filter(e => now - e.time < 1500)
                            if (filtered.length !== debugOverlay.eventLog.length)
                                debugOverlay.eventLog = filtered
                        }
                    }

                    onVisibleChanged: {
                        if (!visible) {
                            debugSubscribe.running = false
                            eventLog = []
                        } else {
                            debugSubscribe.running = true
                        }
                    }

                    // Toast container — bottom-right
                    Column {
                        anchors.right: parent.right
                        anchors.bottom: parent.bottom
                        anchors.rightMargin: 48
                        anchors.bottomMargin: 48
                        spacing: 8

                        // "Debug mode active" indicator
                        Rectangle {
                            width: debugLabel.implicitWidth + 48
                            height: debugLabel.implicitHeight + 24
                            radius: 12
                            color: Qt.rgba(0, 0, 0, 0.7)
                            border.width: 2
                            border.color: Components.Theme.ember

                            Text {
                                id: debugLabel
                                anchors.centerIn: parent
                                text: "Debug Input Active"
                                font.pixelSize: Components.Theme.fontSmall
                                color: Components.Theme.ember
                            }
                        }

                        // Event toasts
                        Repeater {
                            model: debugOverlay.eventLog

                            Rectangle {
                                required property var modelData
                                required property int index
                                width: eventText.implicitWidth + 48
                                height: eventText.implicitHeight + 24
                                radius: 12
                                color: Qt.rgba(0, 0, 0, 0.75)

                                opacity: {
                                    let age = Date.now() - modelData.time
                                    if (age > 1200) return Math.max(0, 1.0 - (age - 1200) / 300)
                                    return 1.0
                                }

                                Behavior on opacity { NumberAnimation { duration: 200 } }

                                Text {
                                    id: eventText
                                    anchors.centerIn: parent
                                    text: modelData.text
                                    font.pixelSize: Components.Theme.fontSmall
                                    color: Components.Theme.textOnDark
                                }
                            }
                        }
                    }
                }

            }
        }
    }
}
