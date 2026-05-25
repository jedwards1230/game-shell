import QtQuick
import QtQuick.Layouts
import Quickshell.Io

Item {
    id: root

    property var controllers: []
    property bool grabActive: false
    property bool loading: true

    // --- Processes ---

    // List connected game controllers
    Process {
        id: listControllers
        command: ["python3", "-c", `
import json, os, re
devs = []
try:
    import evdev
    for p in evdev.list_devices():
        d = evdev.InputDevice(p)
        name = d.name.lower()
        if any(k in name for k in ['gamepad', 'controller', 'xbox', 'joystick', 'pad']):
            devs.append({
                'name': d.name,
                'path': d.path,
                'vendor': format(d.info.vendor, '04x'),
                'product': format(d.info.product, '04x'),
                'connected': True
            })
except ImportError:
    # Fallback: parse /proc/bus/input/devices
    try:
        with open('/proc/bus/input/devices') as f:
            blocks = f.read().split('\\n\\n')
        for block in blocks:
            name_m = re.search(r'N: Name="(.+)"', block)
            if not name_m:
                continue
            name = name_m.group(1)
            if not any(k in name.lower() for k in ['gamepad', 'controller', 'xbox', 'joystick', 'pad']):
                continue
            vendor = product = '0000'
            bus_m = re.search(r'Vendor=([0-9a-f]+) Product=([0-9a-f]+)', block)
            if bus_m:
                vendor = bus_m.group(1)
                product = bus_m.group(2)
            handler_m = re.search(r'H: Handlers=.*(event\\d+)', block)
            path = '/dev/input/' + handler_m.group(1) if handler_m else ''
            devs.append({
                'name': name,
                'path': path,
                'vendor': vendor,
                'product': product,
                'connected': True
            })
    except:
        pass
print(json.dumps(devs))
`]
        stdout: SplitParser {
            onRead: (line) => {
                try {
                    root.controllers = JSON.parse(line)
                } catch(e) {
                    root.controllers = []
                }
                root.loading = false
            }
        }
        onExited: (exitCode, exitStatus) => {
            if (exitCode !== 0) {
                root.controllers = []
                root.loading = false
            }
        }
    }

    // Query grab status from input daemon
    Process {
        id: queryGrab
        command: ["python3", "-c",
            "import socket,os; s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); " +
            "s.settimeout(2); " +
            "try:\n s.connect(os.environ.get('GAME_SHELL_SOCK','/run/user/'+str(os.getuid())+'/game-shell-input.sock'))\nexcept:\n print('disconnected'); s.close(); exit()\n" +
            "s.sendall(b'status\\n'); " +
            "resp = s.recv(256).decode().strip(); print(resp); s.close()"]
        stdout: SplitParser {
            onRead: (line) => {
                let trimmed = line.trim()
                if (trimmed === "disconnected") {
                    root.grabActive = false
                } else {
                    // Daemon responds "grabbed" or "released" or similar
                    root.grabActive = (trimmed.indexOf("grab") >= 0)
                }
            }
        }
    }

    // Send grab command
    Process {
        id: sendGrab
        command: ["python3", "-c",
            "import socket,os; s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); " +
            "s.connect(os.environ.get('GAME_SHELL_SOCK','/run/user/'+str(os.getuid())+'/game-shell-input.sock')); " +
            "s.sendall(b'grab\\n'); print(s.recv(64).decode().strip()); s.close()"]
        onExited: { queryGrab.running = true }
    }

    // Send release command
    Process {
        id: sendRelease
        command: ["python3", "-c",
            "import socket,os; s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); " +
            "s.connect(os.environ.get('GAME_SHELL_SOCK','/run/user/'+str(os.getuid())+'/game-shell-input.sock')); " +
            "s.sendall(b'release\\n'); print(s.recv(64).decode().strip()); s.close()"]
        onExited: { queryGrab.running = true }
    }

    Component.onCompleted: {
        listControllers.running = true
        queryGrab.running = true
    }

    onVisibleChanged: {
        if (visible) {
            root.loading = true
            listControllers.running = true
            queryGrab.running = true
            grabToggleScope.forceActiveFocus()
        }
    }

    // Refresh controller list periodically
    Timer {
        interval: 10000
        running: root.visible
        repeat: true
        onTriggered: { listControllers.running = true; queryGrab.running = true }
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: Theme.padding
        spacing: 32

        // Grab control
        Text {
            text: "Input Grab"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
        }

        RowLayout {
            spacing: 24

            Rectangle {
                width: 16; height: 16; radius: 8
                color: root.grabActive ? Theme.online : Theme.offline
            }

            Text {
                text: root.grabActive ? "Gamepad grabbed (keyboard emulation active)" : "Gamepad released (raw input)"
                font.pixelSize: Theme.fontSmall
                color: Theme.textSecondary
                Layout.fillWidth: true
            }
        }

        FocusScope {
            id: grabToggleScope
            width: grabToggleBtn.width
            height: grabToggleBtn.height
            focus: true

            KeyNavigation.down: controllerList

            SettingsButton {
                id: grabToggleBtn
                text: root.grabActive ? "Release Gamepad" : "Grab Gamepad"
                focus: parent.activeFocus

                MouseArea {
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        grabToggleScope.forceActiveFocus()
                        if (root.grabActive) sendRelease.running = true
                        else sendGrab.running = true
                    }
                }
            }

            Keys.onReturnPressed: {
                if (root.grabActive) sendRelease.running = true
                else sendGrab.running = true
            }
        }

        // Controller list
        Text {
            text: "Connected Controllers"
            font.pixelSize: Theme.fontBody
            font.bold: true
            color: Theme.textPrimary
        }

        Text {
            visible: root.loading
            text: "Scanning..."
            font.pixelSize: Theme.fontSmall
            color: Theme.textMuted
        }

        Text {
            visible: !root.loading && root.controllers.length === 0
            text: "No game controllers detected"
            font.pixelSize: Theme.fontSmall
            color: Theme.textSecondary
        }

        ListView {
            id: controllerList
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: 16
            clip: true
            model: root.controllers
            visible: root.controllers.length > 0

            KeyNavigation.up: grabToggleScope

            delegate: Rectangle {
                required property int index
                required property var modelData
                width: controllerList.width
                height: 160
                radius: Theme.cardRadius
                color: controllerList.currentIndex === index && controllerList.activeFocus
                       ? Theme.surfaceHover : Theme.surface
                border.width: 2
                border.color: Theme.surfaceBorder

                Behavior on color { ColorAnimation { duration: 150 } }

                ColumnLayout {
                    anchors.fill: parent
                    anchors.leftMargin: 32
                    anchors.rightMargin: 32
                    anchors.topMargin: 20
                    anchors.bottomMargin: 20
                    spacing: 8

                    RowLayout {
                        spacing: 16

                        Rectangle {
                            width: 16; height: 16; radius: 8
                            color: modelData.connected ? Theme.online : Theme.offline
                        }

                        Text {
                            text: modelData.name || "Unknown Controller"
                            font.pixelSize: Theme.fontBody
                            font.bold: true
                            color: Theme.textPrimary
                            elide: Text.ElideRight
                            Layout.fillWidth: true
                        }
                    }

                    RowLayout {
                        spacing: 24

                        Text {
                            text: modelData.vendor + ":" + modelData.product
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textSecondary
                        }

                        Text {
                            text: modelData.path || ""
                            font.pixelSize: Theme.fontSmall
                            color: Theme.textMuted
                            visible: text !== ""
                        }
                    }
                }
            }
        }
    }
}
